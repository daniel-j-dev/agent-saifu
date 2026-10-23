//! Relay transport: the `Relay` trait the rest of the code uses, plus the real WebSocket client.
//!
//! The WebSocket lives on its own thread so requests from dapps are received while the MCP
//! server is idle. That thread only moves already-encrypted messages; keys, session state and
//! everything about wallets stay on the main thread.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

use super::auth;
use super::now_secs;
use crate::crypto::{hex, random};
use crate::json::{self, Value};

/// How long a subscribe / publish waits for the relay to confirm.
pub const RELAY_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
const INBOUND_QUEUE: usize = 512;
const MAX_PENDING: usize = 256;
const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub topic: String,
    pub message: String,
    pub tag: u64,
}

pub trait Relay {
    fn subscribe(&self, topic: &str) -> Result<(), String>;
    fn unsubscribe(&self, topic: &str);
    fn publish(
        &self,
        topic: &str,
        message: &str,
        ttl: u64,
        tag: u64,
        prompt: bool,
    ) -> Result<(), String>;
    /// Waits up to `timeout` for the next message.
    fn recv(&self, timeout: Duration) -> Option<Inbound>;
}

/// Stands in for the relay when WalletConnect is not set up; every use explains why.
pub struct UnavailableRelay(pub String);

impl Relay for UnavailableRelay {
    fn subscribe(&self, _: &str) -> Result<(), String> {
        Err(self.0.clone())
    }
    fn unsubscribe(&self, _: &str) {}
    fn publish(&self, _: &str, _: &str, _: u64, _: u64, _: bool) -> Result<(), String> {
        Err(self.0.clone())
    }
    fn recv(&self, _: Duration) -> Option<Inbound> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// `wss://host[:port]`; plain `ws://` is accepted for localhost only.
    pub url: String,
    pub project_id: String,
}

/// Project ids are hex strings from the WalletConnect dashboard.
pub fn validate_project_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 64 {
        return Err("the WalletConnect project ID must be 1-64 characters".to_string());
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(
            "the WalletConnect project ID may only contain letters, digits, - and _".to_string(),
        );
    }
    Ok(())
}

struct Endpoint {
    tls: bool,
    host: String,
    port: u16,
}

fn parse_endpoint(url: &str) -> Result<Endpoint, String> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err("relay URL must start with wss://".to_string());
    };
    let authority = rest.trim_end_matches('/');
    if authority.is_empty() || authority.contains(['/', '?', '#', '@', ' ']) {
        return Err("relay URL must be just a host and optional port".to_string());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| "invalid relay port".to_string())?,
        ),
        None => (authority, if tls { 443 } else { 80 }),
    };
    if !tls && host != "localhost" && host != "127.0.0.1" {
        return Err("plain ws:// is only allowed for localhost".to_string());
    }
    Ok(Endpoint {
        tls,
        host: host.to_string(),
        port,
    })
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => &s.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

type Socket = WebSocket<Stream>;

fn tls_config() -> Result<Arc<ClientConfig>, String> {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("TLS setup failed: {e}"))?
            .with_root_certificates(roots)
            .with_no_client_auth();
    Ok(Arc::new(config))
}

fn open_stream(endpoint: &Endpoint) -> Result<Stream, String> {
    let addr = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve the relay: {e}"))?
        .next()
        .ok_or("the relay host has no address")?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("cannot reach the relay: {e}"))?;
    tcp.set_read_timeout(Some(CONNECT_TIMEOUT)).ok();
    tcp.set_write_timeout(Some(CONNECT_TIMEOUT)).ok();
    tcp.set_nodelay(true).ok();
    if !endpoint.tls {
        return Ok(Stream::Plain(tcp));
    }
    let name = ServerName::try_from(endpoint.host.clone())
        .map_err(|_| "invalid relay host name".to_string())?;
    let conn =
        ClientConnection::new(tls_config()?, name).map_err(|e| format!("TLS setup failed: {e}"))?;
    Ok(Stream::Tls(Box::new(StreamOwned::new(conn, tcp))))
}

fn connect(config: &RelayConfig) -> Result<Socket, String> {
    validate_project_id(&config.project_id)?;
    let endpoint = parse_endpoint(&config.url)?;
    let seed = random::random_array::<32>()?;
    let nonce = hex::encode(&random::random_array::<32>()?);
    let jwt = auth::build_jwt(&seed, &nonce, config.url.trim_end_matches('/'), now_secs());
    let url = format!(
        "{}/?auth={jwt}&projectId={}&ua=agent-saifu",
        config.url.trim_end_matches('/'),
        config.project_id
    );
    let request = url
        .into_client_request()
        .map_err(|e| format!("invalid relay URL: {e}"))?;
    let stream = open_stream(&endpoint)?;
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let (socket, _response) =
        tungstenite::client::client_with_config(request, stream, Some(ws_config))
            .map_err(|e| format!("relay handshake failed: {e}"))?;
    socket
        .get_ref()
        .tcp()
        .set_read_timeout(Some(POLL_INTERVAL))
        .ok();
    Ok(socket)
}

/// Builds the relay for the given project ID, or one that explains what is missing.
pub fn build_relay(project_id: &str, url: &str) -> std::rc::Rc<dyn Relay> {
    let project_id = project_id.trim();
    if project_id.is_empty() {
        return std::rc::Rc::new(UnavailableRelay(
            "WalletConnect needs a project ID: create a free one at https://dashboard.walletconnect.com/ and set the WALLETCONNECT_PROJECT_ID environment variable (or WALLETCONNECT_PROJECT_ID in core/src/walletconnect_config.rs)"
                .to_string(),
        ));
    }
    if let Err(e) = validate_project_id(project_id).and_then(|_| parse_endpoint(url).map(|_| ())) {
        return std::rc::Rc::new(UnavailableRelay(format!(
            "WalletConnect is misconfigured: {e}"
        )));
    }
    std::rc::Rc::new(WsRelay::new(RelayConfig {
        url: url.to_string(),
        project_id: project_id.to_string(),
    }))
}

enum Command {
    Subscribe {
        topic: String,
        reply: Sender<Result<(), String>>,
    },
    Unsubscribe {
        topic: String,
    },
    Publish {
        topic: String,
        message: String,
        ttl: u64,
        tag: u64,
        prompt: bool,
        reply: Sender<Result<(), String>>,
    },
}

struct Pending {
    reply: Option<Sender<Result<(), String>>>,
    created: Instant,
}

struct Worker {
    config: RelayConfig,
    commands: Receiver<Command>,
    inbound: SyncSender<Inbound>,
    socket: Option<Socket>,
    topics: HashSet<String>,
    subscription_ids: HashMap<String, String>,
    pending: HashMap<u64, (Pending, Option<String>)>,
    next_id: u64,
    retry_at: Instant,
}

fn request_json(id: u64, method: &str, params: Value) -> String {
    Value::object(vec![
        ("id", Value::Number(id.to_string())),
        ("jsonrpc", Value::str("2.0")),
        ("method", Value::str(method)),
        ("params", params),
    ])
    .to_json()
}

impl Worker {
    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        now_secs() * 1_000_000_000 + self.next_id % 1_000_000_000
    }

    fn drop_socket(&mut self, reason: &str) {
        self.socket = None;
        self.subscription_ids.clear();
        self.retry_at = Instant::now() + RECONNECT_DELAY;
        for (_, (pending, _)) in self.pending.drain() {
            if let Some(reply) = pending.reply {
                let _ = reply.send(Err(format!("relay connection lost: {reason}")));
            }
        }
    }

    fn ensure_connected(&mut self) -> Result<(), String> {
        if self.socket.is_some() {
            return Ok(());
        }
        let socket = connect(&self.config)?;
        self.socket = Some(socket);
        let topics: Vec<String> = self.topics.iter().cloned().collect();
        for topic in topics {
            self.send_subscribe(&topic, None)?;
        }
        Ok(())
    }

    fn send_text(&mut self, text: String) -> Result<(), String> {
        let socket = self.socket.as_mut().ok_or("not connected to the relay")?;
        match socket.send(Message::text(text)) {
            Ok(()) => Ok(()),
            Err(e) => {
                let reason = e.to_string();
                self.drop_socket(&reason);
                Err(format!("relay connection lost: {reason}"))
            }
        }
    }

    fn send_subscribe(
        &mut self,
        topic: &str,
        reply: Option<Sender<Result<(), String>>>,
    ) -> Result<(), String> {
        let id = self.new_id();
        self.track(id, reply, Some(topic.to_string()));
        let params = Value::object(vec![("topic", Value::str(topic))]);
        self.send_text(request_json(id, "irn_subscribe", params))
    }

    fn track(&mut self, id: u64, reply: Option<Sender<Result<(), String>>>, topic: Option<String>) {
        if self.pending.len() >= MAX_PENDING {
            let oldest = self
                .pending
                .iter()
                .min_by_key(|(_, (p, _))| p.created)
                .map(|(id, _)| *id);
            if let Some(oldest) = oldest {
                self.pending.remove(&oldest);
            }
        }
        self.pending.insert(
            id,
            (
                Pending {
                    reply,
                    created: Instant::now(),
                },
                topic,
            ),
        );
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::Subscribe { topic, reply } => {
                if let Err(e) = self.ensure_connected() {
                    let _ = reply.send(Err(e));
                    return;
                }
                self.topics.insert(topic.clone());
                if let Err(e) = self.send_subscribe(&topic, Some(reply.clone())) {
                    let _ = reply.send(Err(e));
                }
            }
            Command::Unsubscribe { topic } => {
                self.topics.remove(&topic);
                if let (Some(sub_id), true) =
                    (self.subscription_ids.remove(&topic), self.socket.is_some())
                {
                    let id = self.new_id();
                    let params = Value::object(vec![
                        ("topic", Value::String(topic)),
                        ("id", Value::String(sub_id)),
                    ]);
                    self.track(id, None, None);
                    let _ = self.send_text(request_json(id, "irn_unsubscribe", params));
                }
            }
            Command::Publish {
                topic,
                message,
                ttl,
                tag,
                prompt,
                reply,
            } => {
                if let Err(e) = self.ensure_connected() {
                    let _ = reply.send(Err(e));
                    return;
                }
                let id = self.new_id();
                self.track(id, Some(reply.clone()), None);
                let params = Value::object(vec![
                    ("topic", Value::String(topic)),
                    ("message", Value::String(message)),
                    ("ttl", Value::Number(ttl.to_string())),
                    ("tag", Value::Number(tag.to_string())),
                    ("prompt", Value::Bool(prompt)),
                ]);
                if let Err(e) = self.send_text(request_json(id, "irn_publish", params)) {
                    let _ = reply.send(Err(e));
                }
            }
        }
    }

    fn handle_text(&mut self, text: &str) {
        let Ok(doc) = json::parse(text) else { return };
        if let Some(method) = doc.get("method").and_then(Value::as_str) {
            if method.ends_with("_subscription") {
                self.handle_subscription(&doc);
            }
            return;
        }
        let Some(id) = doc.get("id").and_then(Value::as_u64) else {
            return;
        };
        let Some((pending, topic)) = self.pending.remove(&id) else {
            return;
        };
        let outcome = match doc.get("error") {
            Some(error) => Err(format!(
                "relay error: {}",
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(|m| m
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(200)
                        .collect::<String>())
                    .unwrap_or_else(|| "unknown".to_string())
            )),
            None => Ok(()),
        };
        if let (Ok(()), Some(topic), Some(sub_id)) =
            (&outcome, topic, doc.get("result").and_then(Value::as_str))
        {
            self.subscription_ids.insert(topic, sub_id.to_string());
        }
        if let Some(reply) = pending.reply {
            let _ = reply.send(outcome);
        }
    }

    fn handle_subscription(&mut self, doc: &Value) {
        let Some(id) = doc.get("id") else { return };
        let ack = Value::object(vec![
            ("id", id.clone()),
            ("jsonrpc", Value::str("2.0")),
            ("result", Value::Bool(true)),
        ]);
        let _ = self.send_text(ack.to_json());
        let Some(data) = doc.get("params").and_then(|p| p.get("data")) else {
            return;
        };
        let (Some(topic), Some(message)) = (
            data.get("topic").and_then(Value::as_str),
            data.get("message").and_then(Value::as_str),
        ) else {
            return;
        };
        if !self.topics.contains(topic) {
            return;
        }
        let tag = data.get("tag").and_then(Value::as_u64).unwrap_or(0);
        // A full queue means nobody is listening; dropping beats blocking the socket.
        let _ = self.inbound.try_send(Inbound {
            topic: topic.to_string(),
            message: message.to_string(),
            tag,
        });
    }

    fn read_once(&mut self) {
        let Some(socket) = self.socket.as_mut() else {
            return;
        };
        match socket.read() {
            Ok(Message::Text(text)) => self.handle_text(text.as_str()),
            Ok(Message::Close(_)) => self.drop_socket("closed by the relay"),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => {
                let reason = e.to_string();
                self.drop_socket(&reason);
            }
        }
    }

    fn run(mut self) {
        loop {
            let command = if self.socket.is_none() && self.topics.is_empty() {
                match self.commands.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return,
                }
            } else {
                match self.commands.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            };
            if let Some(command) = command {
                self.handle_command(command);
                continue;
            }
            if self.socket.is_none() {
                if Instant::now() >= self.retry_at && self.ensure_connected().is_err() {
                    self.retry_at = Instant::now() + RECONNECT_DELAY;
                }
                thread::sleep(POLL_INTERVAL);
                continue;
            }
            self.read_once();
        }
    }
}

/// The real relay client. The connection is opened on first use.
pub struct WsRelay {
    commands: Sender<Command>,
    inbound: Receiver<Inbound>,
}

impl WsRelay {
    pub fn new(config: RelayConfig) -> WsRelay {
        let (commands, command_rx) = mpsc::channel();
        let (inbound_tx, inbound) = mpsc::sync_channel(INBOUND_QUEUE);
        let worker = Worker {
            config,
            commands: command_rx,
            inbound: inbound_tx,
            socket: None,
            topics: HashSet::new(),
            subscription_ids: HashMap::new(),
            pending: HashMap::new(),
            next_id: 0,
            retry_at: Instant::now(),
        };
        thread::Builder::new()
            .name("walletconnect-relay".to_string())
            .spawn(move || worker.run())
            .expect("spawning the relay thread");
        WsRelay { commands, inbound }
    }

    fn call(
        &self,
        command: impl FnOnce(Sender<Result<(), String>>) -> Command,
    ) -> Result<(), String> {
        let (reply, answer) = mpsc::channel();
        self.commands
            .send(command(reply))
            .map_err(|_| "the relay thread has stopped".to_string())?;
        match answer.recv_timeout(RELAY_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err("the relay did not respond in time".to_string()),
            Err(RecvTimeoutError::Disconnected) => Err("the relay thread has stopped".to_string()),
        }
    }
}

impl Relay for WsRelay {
    fn subscribe(&self, topic: &str) -> Result<(), String> {
        self.call(|reply| Command::Subscribe {
            topic: topic.to_string(),
            reply,
        })
    }

    fn unsubscribe(&self, topic: &str) {
        let _ = self.commands.send(Command::Unsubscribe {
            topic: topic.to_string(),
        });
    }

    fn publish(
        &self,
        topic: &str,
        message: &str,
        ttl: u64,
        tag: u64,
        prompt: bool,
    ) -> Result<(), String> {
        self.call(|reply| Command::Publish {
            topic: topic.to_string(),
            message: message.to_string(),
            ttl,
            tag,
            prompt,
            reply,
        })
    }

    fn recv(&self, timeout: Duration) -> Option<Inbound> {
        self.inbound.recv_timeout(timeout).ok()
    }
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Published {
        pub topic: String,
        pub message: String,
        pub ttl: u64,
        pub tag: u64,
        pub prompt: bool,
    }

    /// In-memory relay: tests queue inbound messages and inspect what was published.
    #[derive(Default)]
    pub struct MockRelay {
        pub inbound: RefCell<VecDeque<Inbound>>,
        pub published: RefCell<Vec<Published>>,
        pub subscribed: RefCell<Vec<String>>,
        pub unsubscribed: RefCell<Vec<String>>,
        pub fail_publish: Cell<bool>,
    }

    impl MockRelay {
        pub fn push(&self, topic: &str, message: String, tag: u64) {
            self.inbound.borrow_mut().push_back(Inbound {
                topic: topic.to_string(),
                message,
                tag,
            });
        }

        pub fn take_published(&self) -> Vec<Published> {
            std::mem::take(&mut *self.published.borrow_mut())
        }
    }

    impl Relay for MockRelay {
        fn subscribe(&self, topic: &str) -> Result<(), String> {
            self.subscribed.borrow_mut().push(topic.to_string());
            Ok(())
        }
        fn unsubscribe(&self, topic: &str) {
            self.unsubscribed.borrow_mut().push(topic.to_string());
        }
        fn publish(
            &self,
            topic: &str,
            message: &str,
            ttl: u64,
            tag: u64,
            prompt: bool,
        ) -> Result<(), String> {
            if self.fail_publish.get() {
                return Err("relay down".to_string());
            }
            self.published.borrow_mut().push(Published {
                topic: topic.to_string(),
                message: message.to_string(),
                ttl,
                tag,
                prompt,
            });
            Ok(())
        }
        fn recv(&self, _timeout: Duration) -> Option<Inbound> {
            self.inbound.borrow_mut().pop_front()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn project_ids_are_validated() {
        assert!(validate_project_id("0123456789abcdef0123456789abcdef").is_ok());
        assert!(validate_project_id("my-project_1").is_ok());
        assert!(validate_project_id("").is_err());
        assert!(validate_project_id("a&b=c").is_err());
        assert!(validate_project_id("a b").is_err());
        assert!(validate_project_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn endpoints_are_parsed_strictly() {
        let e = parse_endpoint("wss://relay.walletconnect.org").unwrap();
        assert!(e.tls && e.host == "relay.walletconnect.org" && e.port == 443);
        let e = parse_endpoint("wss://relay.example.org:8443/").unwrap();
        assert_eq!((e.host.as_str(), e.port), ("relay.example.org", 8443));
        let e = parse_endpoint("ws://127.0.0.1:9000").unwrap();
        assert!(!e.tls && e.port == 9000);
        for bad in [
            "https://relay.example.org",
            "ws://relay.example.org",
            "wss://",
            "wss://user@relay.example.org",
            "wss://relay.example.org/path",
            "wss://relay.example.org:notaport",
            "relay.example.org",
        ] {
            assert!(parse_endpoint(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn build_relay_explains_missing_or_bad_settings() {
        let none = build_relay("  ", "wss://relay.example.org");
        assert!(none.subscribe("t").unwrap_err().contains("project ID"));
        let bad_id = build_relay("a&b", "wss://relay.example.org");
        assert!(bad_id.subscribe("t").unwrap_err().contains("misconfigured"));
        let bad_url = build_relay("proj", "https://relay.example.org");
        assert!(
            bad_url
                .subscribe("t")
                .unwrap_err()
                .contains("misconfigured")
        );
    }

    #[test]
    fn unavailable_relay_explains_itself() {
        let relay = UnavailableRelay("set a project ID".to_string());
        assert_eq!(relay.subscribe("t").unwrap_err(), "set a project ID");
        assert_eq!(
            relay.publish("t", "m", 1, 1, false).unwrap_err(),
            "set a project ID"
        );
        assert!(relay.recv(Duration::from_millis(1)).is_none());
    }

    fn ws_url(listener: &TcpListener) -> String {
        format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port())
    }

    /// A tiny relay: accepts one client, answers subscribe/publish and pushes one message.
    fn serve_once(listener: TcpListener, saw: Arc<std::sync::Mutex<Vec<String>>>) {
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            #[allow(clippy::result_large_err)]
            let mut ws = tungstenite::accept_hdr(
                stream,
                |req: &tungstenite::handshake::server::Request,
                 resp: tungstenite::handshake::server::Response| {
                    saw.lock().unwrap().push(req.uri().to_string());
                    Ok(resp)
                },
            )
            .unwrap();
            loop {
                let Ok(Message::Text(text)) = ws.read() else {
                    return;
                };
                let doc = json::parse(text.as_str()).unwrap();
                saw.lock().unwrap().push(text.as_str().to_string());
                let id = doc.get("id").unwrap().clone();
                let method = doc.get("method").and_then(Value::as_str).unwrap_or("");
                if method == "irn_subscribe" {
                    let reply = Value::object(vec![
                        ("id", id),
                        ("jsonrpc", Value::str("2.0")),
                        ("result", Value::str("sub-1")),
                    ]);
                    ws.send(Message::text(reply.to_json())).unwrap();
                    let topic = doc.get("params").unwrap().get("topic").unwrap().clone();
                    let push = Value::object(vec![
                        ("id", Value::int(77)),
                        ("jsonrpc", Value::str("2.0")),
                        ("method", Value::str("irn_subscription")),
                        (
                            "params",
                            Value::object(vec![
                                ("id", Value::str("x")),
                                (
                                    "data",
                                    Value::object(vec![
                                        ("topic", topic),
                                        ("message", Value::str("hello")),
                                        ("tag", Value::int(1100)),
                                    ]),
                                ),
                            ]),
                        ),
                    ]);
                    ws.send(Message::text(push.to_json())).unwrap();
                } else if method == "irn_publish" {
                    let reply = if doc
                        .get("params")
                        .and_then(|p| p.get("message"))
                        .and_then(Value::as_str)
                        == Some("reject-me")
                    {
                        Value::object(vec![
                            ("id", id),
                            ("jsonrpc", Value::str("2.0")),
                            (
                                "error",
                                Value::object(vec![
                                    ("code", Value::int(-32000)),
                                    ("message", Value::str("not allowed")),
                                ]),
                            ),
                        ])
                    } else {
                        Value::object(vec![
                            ("id", id),
                            ("jsonrpc", Value::str("2.0")),
                            ("result", Value::Bool(true)),
                        ])
                    };
                    ws.send(Message::text(reply.to_json())).unwrap();
                }
            }
        });
    }

    #[test]
    fn websocket_relay_subscribes_publishes_and_delivers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = ws_url(&listener);
        let saw = Arc::new(std::sync::Mutex::new(vec![]));
        serve_once(listener, saw.clone());
        let relay = WsRelay::new(RelayConfig {
            url,
            project_id: "proj-123".to_string(),
        });
        relay.subscribe("topic-a").unwrap();
        let got = relay.recv(Duration::from_secs(5)).expect("pushed message");
        assert_eq!(
            got,
            Inbound {
                topic: "topic-a".into(),
                message: "hello".into(),
                tag: 1100
            }
        );
        relay
            .publish("topic-a", "payload", 300, 1101, false)
            .unwrap();
        let err = relay
            .publish("topic-a", "reject-me", 300, 1101, false)
            .unwrap_err();
        assert!(err.contains("not allowed"), "{err}");
        let log = saw.lock().unwrap().clone();
        assert!(
            log[0].contains("auth=") && log[0].contains("projectId=proj-123"),
            "{}",
            log[0]
        );
        assert!(
            log.iter()
                .any(|l| l.contains("\"ttl\":300") && l.contains("\"tag\":1101"))
        );
        assert!(
            log.iter().any(|l| l.contains("\"result\":true")),
            "subscription must be acked"
        );
    }

    #[test]
    fn unreachable_relay_reports_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = ws_url(&listener);
        drop(listener);
        let relay = WsRelay::new(RelayConfig {
            url,
            project_id: "proj".to_string(),
        });
        assert!(relay.subscribe("t").unwrap_err().contains("relay"));
        assert!(relay.publish("t", "m", 1, 1, false).is_err());
    }

    #[test]
    fn missing_project_id_is_reported() {
        let relay = WsRelay::new(RelayConfig {
            url: "wss://relay.example.org".to_string(),
            project_id: String::new(),
        });
        assert!(relay.subscribe("t").unwrap_err().contains("project ID"));
    }
}
