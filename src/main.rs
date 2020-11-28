use anyhow::Result;
use async_std::io::BufReader;
use async_std::net::{TcpListener, TcpStream};
use async_std::prelude::*;
use async_std::sync::{channel, Receiver, Sender};
use async_std::task;
use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4};

use rand::distributions::Alphanumeric;
use rand::Rng;

#[derive(Debug, Clone)]
struct Client {
    nick: String,
    stream: TcpStream,
}

#[derive(Debug)]
enum Command {
    MsgToAll(SocketAddrV4, String),
    MsgToOne(SocketAddrV4, String),
    Quit(SocketAddrV4),
    New(SocketAddrV4, Client),
    ChangeNick(SocketAddrV4, String),
    Users(SocketAddrV4),
    Me(SocketAddrV4),
}

struct Server {
    rx: Receiver<Command>,
    clients: HashMap<SocketAddrV4, Client>,
}

impl Server {
    fn new(rx: Receiver<Command>) -> Self {
        Server {
            rx,
            clients: HashMap::new(),
        }
    }

    fn random_nick(&self) -> Result<String> {
        let mut limit = 10;
        loop {
            if limit == 0 {
                anyhow::bail!("unable to generate nick");
            }
            let nick = rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(6)
                .collect::<String>();
            if self.clients.iter().all(|(_, client)| client.nick != nick) {
                return Ok(nick);
            }
            limit -= 1;
        }
    }

    async fn broadcast(&mut self, addr: &SocketAddrV4, is_admin: bool, s: &str) {
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        let msg = s.trim();
        if let Some(client) = self.clients.get(addr) {
            eprintln!("{} says '{}'", client.nick, msg);
            let line = if is_admin {
                format!("{} <{}>\n", time, msg)
            } else {
                format!("{} [{}]: {}\n", time, client.nick, msg)
            };
            for (_, client) in self.clients.iter_mut().filter(|(a, _)| a.ne(&addr)) {
                if let Err(e) = client.stream.write_all(line.as_bytes()).await {
                    eprintln!("broadcast error: {}", e);
                }
            }
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

    async fn change_nick(&mut self, addr: &SocketAddrV4, nick: &str) -> Result<String> {
        if self
            .clients
            .iter()
            .find(|(_, client)| client.nick == nick)
            .is_some()
        {
            anyhow::bail!("Nick '{}' already present!", nick);
        }
        if let Some(client) = self.clients.get_mut(&addr) {
            let old_nick = client.nick.clone();
            client.nick = nick.to_string();
            return Ok(old_nick);
        } else {
            anyhow::bail!("Cannot change nick.");
        }
    }

    async fn new_client(&mut self, addr: SocketAddrV4, mut client: Client) -> Result<String> {
        let nick = self.random_nick().unwrap_or("<anonymous>".to_string());
        if !self.clients.contains_key(&addr) {
            client.nick = nick.clone();
            self.clients.insert(addr, client);
            Ok(nick)
        } else {
            anyhow::bail!("Client '{}' already present!", addr);
        }
    }

    async fn run(&mut self) -> Result<()> {
        loop {
            match self.rx.recv().await? {
                Command::New(addr, client) => {
                    eprintln!("new client: {}", addr);
                    match self.new_client(addr, client).await {
                        Ok(nick) => {
                            self.broadcast(&addr, true, &format!("{} joined", &nick))
                                .await;
                            self.send_message(&addr, &format!("Hello '{}'!", &nick))
                                .await;
                        }
                        Err(e) => {
                            self.send_message(&addr, &e.to_string()).await;
                        }
                    }
                }
                Command::ChangeNick(addr, nick) => match self.change_nick(&addr, &nick).await {
                    Ok(old_nick) => {
                        self.broadcast(&addr, true, &format!("{} is now {}", &old_nick, &nick))
                            .await;
                        self.send_message(&addr, &format!("You are now '{}'!", &nick))
                            .await;
                    }
                    Err(e) => {
                        self.send_message(&addr, &e.to_string()).await;
                    }
                },
                Command::Quit(addr) => {
                    let nick = if let Some(client) = self.clients.get(&addr) {
                        client.nick.clone()
                    } else {
                        "<anonymous>".to_string()
                    };
                    self.broadcast(&addr, true, &format!("{} left", nick)).await;
                    self.clients.remove(&addr);
                }
                Command::MsgToAll(addr, s) => {
                    eprintln!("broadcasting: {}", &s);
                    self.broadcast(&addr, false, &s).await;
                }
                Command::MsgToOne(addr, s) => {
                    eprintln!("private: {}", &s);
                    self.send_message(&addr, &s).await;
                }
                Command::Users(addr) => {
                    let mut users = vec![];
                    self.clients.iter().for_each(|(_, c)| {
                        users.push(c.nick.clone());
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
                Command::Me(addr) => {
                    let nick = if let Some(client) = self.clients.get(&addr) {
                        client.nick.clone()
                    } else {
                        "<anonymous>".to_string()
                    };
                    self.send_message(
                        &addr,
                        &format!("You are '{}' connected from '{}'", &nick, addr),
                    )
                    .await;
                }
            }
        }
    }
}

async fn handle_client(stream: TcpStream, addr: SocketAddrV4, tx: Sender<Command>) -> Result<()> {
    eprintln!("connection from: {} ", &addr);

    // TODO: implement Client::new(stream)
    let client = Client {
        nick: "".to_string(),
        stream: stream.clone(),
    };

    tx.send(Command::New(addr, client)).await;

    let mut reader = BufReader::new(stream);
    let mut buf = String::from("");

    loop {
        match reader.read_line(&mut buf).await {
            Ok(0) => {
                tx.send(Command::Quit(addr)).await;
                break;
            }
            Ok(_) => {
                if let Some(cmd) = buf.split_ascii_whitespace().next() {
                    match cmd {
                        "/quit" => {
                            tx.send(Command::Quit(addr)).await;
                            break;
                        }
                        "/nick" => {
                            if let Some(nick) = buf.split_ascii_whitespace().nth(1) {
                                tx.send(Command::ChangeNick(addr, nick.to_string())).await;
                            } else {
                                tx.send(Command::Me(addr)).await;
                            }
                        }
                        "/users" => {
                            tx.send(Command::Users(addr)).await;
                        }
                        "/me" => {
                            tx.send(Command::Me(addr)).await;
                        }
                        _ => {
                            if cmd.starts_with('/') {
                                tx.send(Command::MsgToOne(
                                    addr,
                                    format!("no such command '{}'", cmd),
                                ))
                                .await;
                            } else {
                                let line = buf.trim().to_string();
                                tx.send(Command::MsgToAll(addr, line)).await;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                anyhow::bail!(e);
            }
        }

        buf.clear();
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
