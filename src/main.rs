use anyhow::Result;
use async_std::io::BufReader;
use async_std::net::{TcpListener, TcpStream};
use async_std::prelude::*;
use async_std::sync::{channel, Receiver, Sender};
use async_std::task;
use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4};

#[derive(Debug, Clone)]
struct Client {
    nick: Option<String>,
    stream: TcpStream,
}

#[derive(Debug)]
enum Message {
    BroadcastMsg(SocketAddrV4, String),
    PrivateMsg(SocketAddrV4, String),
    ClientQuit(SocketAddrV4),
    ClientNew(SocketAddrV4, Client),
    NickChange(SocketAddrV4, String),
    Users(SocketAddrV4),
}

struct Server {
    rx: Receiver<Message>,
    clients: HashMap<SocketAddrV4, Client>,
}

impl Server {
    fn new(rx: Receiver<Message>) -> Self {
        Server {
            rx,
            clients: HashMap::new(),
        }
    }

    async fn broadcast(&mut self, addr: &SocketAddrV4, s: &str) {
        let msg = s.trim();

        if let Some(nick) = self.clients.get(addr).and_then(|c| c.nick.as_ref()) {
            eprintln!("{} says '{}'", nick, msg);
            let line = format!("{}: {}\n", nick, msg);
            for (_, client) in self.clients.iter_mut().filter(|(a, _)| a.ne(&addr)) {
                if let Err(e) = client.stream.write_all(line.as_bytes()).await {
                    eprintln!("broadcast error: {}", e);
                }
            }
        } else {
            self.send_message(addr, "Cannot send messages as anonymous user. Use command '/nick' to select name for you.").await;
        }
    }

    async fn send_message(&mut self, addr: &SocketAddrV4, s: &str) {
        let msg = format!("{}\n", s.trim());
        eprintln!(
            "sending message '{}' to addr '{}' ",
            msg.as_str().trim(),
            addr
        );

        if let Some(client) = self.clients.get_mut(addr) {
            if let Err(e) = client.stream.write_all(msg.as_bytes()).await {
                eprintln!("send_message error: {}", e);
            }
        }
    }

    async fn run(&mut self) -> Result<()> {
        loop {
            match self.rx.recv().await? {
                Message::ClientNew(addr, client) => {
                    eprintln!("new client: {}", addr);
                    if !self.clients.contains_key(&addr) {
                        self.clients.insert(addr, client);
                    } else {
                        self.send_message(
                            &addr,
                            &format!("Client with id '{}' already present!", addr),
                        )
                        .await;
                    }
                }
                Message::NickChange(addr, nick) => {
                    if self
                        .clients
                        .iter()
                        .find(|(_, client)| client.nick == Some(nick.clone()))
                        .is_some()
                    {
                        self.send_message(&addr, &format!("nick '{}' already present!", &nick))
                            .await;
                    } else {
                        let msg = if let Some(client) = self.clients.get_mut(&addr) {
                            client.nick = Some(nick.clone());
                            format!("you are now '{}'", &nick)
                        } else {
                            format!("cannot change your nick!")
                        };
                        self.send_message(&addr, &msg).await;
                    };
                }
                Message::ClientQuit(addr) => {
                    self.broadcast(&addr, &format!("<left>")).await;
                    self.clients.remove(&addr);
                }
                Message::BroadcastMsg(addr, s) => {
                    eprintln!("broadcasting: {}", &s);
                    self.broadcast(&addr, &s).await;
                }
                Message::PrivateMsg(addr, s) => {
                    eprintln!("private: {}", &s);
                    self.send_message(&addr, &s).await;
                }
                Message::Users(addr) => {
                    let mut users = vec![];
                    self.clients.iter().for_each(|(_, c)| {
                        if c.nick.is_some() {
                            users.push(c.nick.clone().unwrap())
                        }
                    });
                    self.send_message(
                        &addr,
                        &format!(
                            "{} users online: [{}]\n",
                            self.clients.len(),
                            users.join(", ")
                        ),
                    )
                    .await;
                }
            }
        }
    }
}

async fn handle_client(stream: TcpStream, addr: SocketAddrV4, tx: Sender<Message>) -> Result<()> {
    eprintln!("connection from: {} ", &addr);

    let mut client = Client {
        nick: None,
        stream: stream.clone(),
    };
    client.stream.write_all(b"hello").await?;

    tx.send(Message::ClientNew(addr, client)).await;

    let mut reader = BufReader::new(stream);

    loop {
        let mut buf = String::from("");
        if let Ok(0) = reader.read_line(&mut buf).await {
            eprintln!("EOF from '{}'", addr);
            tx.send(Message::ClientQuit(addr)).await;
            break;
        }

        if let Some(cmd) = buf.split_ascii_whitespace().next() {
            match cmd {
                "/quit" => {
                    tx.send(Message::ClientQuit(addr)).await;
                    break;
                }
                "/nick" => {
                    if let Some(nick) = buf.split_ascii_whitespace().skip(1).next() {
                        tx.send(Message::NickChange(addr, nick.to_string())).await;
                    }
                }
                "/users" => {
                    tx.send(Message::Users(addr)).await;
                }
                _ => {
                    if cmd.starts_with('/') {
                        tx.send(Message::PrivateMsg(
                            addr,
                            format!("no such command '{}'", cmd),
                        ))
                        .await;
                    } else {
                        let line = buf.trim().to_string();
                        tx.send(Message::BroadcastMsg(addr, line)).await;
                    }
                }
            }
        }
    }

    Ok(())
}

#[async_std::main]
async fn main() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    let (tx, rx) = channel(100);

    task::spawn(async move {
        if let Err(e) = Server::new(rx).run().await {
            panic!("server crash: {}", e);
        }
    });

    loop {
        let (stream, addr) = listener.accept().await?;

        if let SocketAddr::V4(addr) = addr {
            let tx2 = tx.clone();
            task::spawn(async move {
                match handle_client(stream, addr, tx2).await {
                    Ok(_) => {}
                    Err(e) => eprintln!("Unable to handle client: {}", e),
                };
            });
        }
    }
}
