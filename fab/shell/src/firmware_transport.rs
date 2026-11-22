//! The real network behind a twin's upstream connections (`fab_elec_mcu_rt::net::upstream`): the
//! owner's `Network`, for a project whose real network the person turned on ([`crate::firmware_net`]).
//!
//! - It reaches only the servers it was built with: [`dials`] keeps the bench's endpoints whose host
//!   the person approved for the project. Every other connection plays its fake server.
//! - Each open starts one worker for the connection's life (the app's thread spawner), which
//!   connects through the platform's local network rule, speaks WebSocket or a TCP stream (TLS
//!   checks the server's certificate with the system's own stack), and answers through a channel the
//!   twin drains into its input log ([`Network::answers`]).
//! - A WebSocket open sends the endpoint's token as `Authorization: Bearer`, only when it was saved
//!   for that host, and only over TLS unless the host is this machine: a token never crosses the
//!   network in plain text (the bench refuses such a declaration, the settings refuse to hand the
//!   token out, and the transport checks again). It is read as the connection opens, off the UI
//!   thread, and every buffer that held it is wiped once sent.
//! - Each part of a message goes out as it comes (a WebSocket fragment, or stream bytes), and the
//!   bytes written come back as `Taken`, so the device's grants follow what the server takes.
//! - A connection to a service of this computer's AI hub (`hub: "talk"` in the bench) goes through
//!   the hub client's own WebSocket opener (`makepad_ai_hub::web_socket`), which keeps the hub's
//!   network rule and this computer's own identity: no token of the person's is involved. Only a
//!   hub on loopback is dialled ([`machine_hubs`]); one named elsewhere is logged and skipped. The
//!   bench's chat model rides on the route (`/talk?model=<id>`, percent-encoded). Its
//!   parts are taken as Fab holds them, and each message goes to the hub whole at its last part.
//! - Every connection is logged like the model downloads: its endpoint, its address without the
//!   query (a hub's, once it is ready), and how it ended. Never a token, never a message.
//! - A cut ([`Network::close_all`]) shuts every socket down at once (a hub's through its client's
//!   closer), a worker blocked in a write included, and drops whatever the old connections still
//!   answer.
//!
//! Cost: one worker per open connection, waking every 20 ms to read; nothing on the UI thread but
//! queueing parts and draining a channel.

use crate::firmware_net::{FirmwareNet, TokenFor};
use fab_elec_bench::net::{NetEndpoint, Real, Scheme, Server};
use fab_elec_mcu_rt::net::{code, code_text, Arrival, Network, Request, PART_BINARY, PART_FIRST, PART_LAST};
use makepad_widgets_core::makepad_platform::makepad_network::digest::{base64_encode, Sha1};
use makepad_ai_hub::web_socket::{HubWebSocket, HubWsCloser};
use makepad_widgets_core::makepad_platform::makepad_network::web_socket_parser::WebSocketMessage;
use makepad_widgets_core::makepad_platform::makepad_network::{SocketStream, WsSend};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A connection the twin may open on the real network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dial {
    pub c: u8,
    /// The endpoint's name: the key of its token, and what the log calls it (None: no token).
    pub endpoint: Option<String>,
    pub real: Real,
}

/// The connections of `endpoints` (one firmware part's) that may reach their real end for
/// `project`: a server whose host, or a hub service (`hub:<service>`), the person approved.
pub fn dials(net: &FirmwareNet, project: &str, endpoints: &[NetEndpoint]) -> Vec<Dial> {
    endpoints.iter().filter_map(|e| {
        let real = e.real.as_ref()?;
        net.allowed(project, &real.approval()).then(|| Dial { c: e.c, endpoint: e.name.clone(), real: real.clone() })
    }).collect()
}

/// This computer's hubs as base URLs, in the order to try ([`machine_hubs`] in the app; a test's
/// scripted hub).
pub type Hubs = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// The hubs serving on this computer: each running hub's entry in the hub's run folder, on
/// loopback. A stale entry refuses the connection and the next one is tried.
pub fn machine_hubs() -> Hubs {
    Arc::new(|| {
        makepad_ai_hub::machine::read_node_entries().into_iter()
            .filter(|(_, entry)| entry.port != 0)
            .map(|(_, entry)| format!("http://127.0.0.1:{}", entry.port))
            .collect()
    })
}

/// A hub base URL on this computer: plain http to a loopback address, written as one.
fn on_this_computer(base: &str) -> bool {
    let Some(rest) = base.strip_prefix("http://") else { return false };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = authority.rsplit_once(':').map_or(authority, |(host, _)| host);
    host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The token for an endpoint name, host, and whether the connection is encrypted
/// ([`settings_tokens`] in the app).
pub type Tokens = Arc<dyn Fn(&str, &str, bool) -> Result<TokenFor, String> + Send + Sync>;

/// The tokens the person saved in their own Fab settings.
pub fn settings_tokens() -> Tokens { Arc::new(crate::firmware_net::token) }

/// Connects to a server: [`checked_connect`] in the app (TLS checks the certificate with the
/// system's own stack); a test pins its own server's certificate instead.
pub type Connect = Arc<dyn Fn(&Server) -> std::io::Result<SocketStream> + Send + Sync>;

pub fn checked_connect() -> Connect {
    Arc::new(|server: &Server| SocketStream::connect(&server.host, &server.port.to_string(), server.scheme.tls(), false))
}
/// Where log lines go.
pub type Log = Arc<dyn Fn(String) + Send + Sync>;
/// Starts a connection's worker.
pub type Spawn = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

enum Out {
    Part { flags: u8, bytes: Vec<u8> },
    Close,
}

/// The socket under a connection, once connected: what a cut shuts down.
type Socket = Arc<Mutex<Option<Cut>>>;

/// How a cut reaches a connection's socket from the transport's thread.
enum Cut {
    Server(TcpStream),
    Hub(HubWsCloser),
}

impl Cut {
    fn shut(self) {
        let _ = match self {
            Cut::Server(socket) => socket.shutdown(Shutdown::Both),
            Cut::Hub(closer) => closer.close(),
        };
    }
}

struct Conn {
    attempt: (u8, u32),
    out: Sender<Out>,
    socket: Socket,
}

/// What every connection's worker shares.
struct Shared {
    tokens: Tokens,
    connect: Connect,
    hubs: Hubs,
    log: Log,
}

pub struct Transport {
    dials: Vec<Dial>,
    shared: Arc<Shared>,
    spawn: Spawn,
    tx: Sender<Arrival>,
    rx: Receiver<Arrival>,
    conns: Vec<Conn>,
}

impl Transport {
    pub fn new(dials: Vec<Dial>, tokens: Tokens, connect: Connect, hubs: Hubs, log: Log, spawn: Spawn) -> Transport {
        let (tx, rx) = channel();
        Transport { dials, shared: Arc::new(Shared { tokens, connect, hubs, log }), spawn, tx, rx, conns: Vec::new() }
    }

    fn open(&mut self, c: u8, gen: u32) {
        let Some(dial) = self.dials.iter().find(|d| d.c == c).cloned() else {
            let _ = self.tx.send(Arrival::Ended { c, gen, code: code::NOT_CONFIGURED, detail: 0 });
            return;
        };
        let (out, out_rx) = channel();
        let socket = Socket::default();
        self.conns.push(Conn { attempt: (c, gen), out, socket: socket.clone() });
        let (shared, tx) = (self.shared.clone(), self.tx.clone());
        (self.spawn)(Box::new(move || connection(dial, gen, &shared, out_rx, socket, tx)));
    }

    fn queue(&mut self, attempt: (u8, u32), out: Out) {
        let close = matches!(out, Out::Close);
        if let Some(k) = self.conns.iter().position(|conn| conn.attempt == attempt) {
            // A worker that already ended has answered for itself.
            let _ = self.conns[k].out.send(out);
            if close { self.conns.remove(k); }
        }
    }
}

impl Network for Transport {
    fn send(&mut self, request: &Request) {
        match request {
            Request::Open { c, gen } => self.open(*c, *gen),
            Request::Send { flags, bytes, .. } => self.queue(request.attempt(), Out::Part { flags: *flags, bytes: bytes.clone() }),
            Request::Close { .. } => self.queue(request.attempt(), Out::Close),
        }
    }

    fn close_all(&mut self) {
        for conn in self.conns.drain(..) {
            if let Some(cut) = conn.socket.lock().ok().and_then(|mut s| s.take()) {
                cut.shut();
            }
        }
        // What the old connections still answer goes to a channel no one reads.
        let (tx, rx) = channel();
        self.tx = tx;
        self.rx = rx;
    }

    fn answers(&mut self, out: &mut Vec<Arrival>) { out.extend(self.rx.try_iter()); }
}

impl Drop for Transport {
    fn drop(&mut self) { self.close_all(); }
}

/// Wipes bytes that held a credential.
fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Bytes no other call returns, for the WebSocket key and the frame masks.
fn nonce() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; 16];
    for half in out.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

/// The code an open's failure to connect reads as. Past the TCP connection's own failures, a TLS
/// connection's is its handshake's (a certificate the system does not trust among them).
fn connect_failure(error: &std::io::Error, tls: bool) -> u8 {
    let text = error.to_string().to_ascii_lowercase();
    match error.kind() {
        ErrorKind::ConnectionRefused => code::REFUSED,
        ErrorKind::TimedOut => code::TIMED_OUT,
        ErrorKind::NotFound => code::NAME_NOT_FOUND,
        _ if text.contains("lookup") || text.contains("resolve") || text.contains("not known") => code::NAME_NOT_FOUND,
        ErrorKind::Other | ErrorKind::InvalidData | ErrorKind::UnexpectedEof if tls => code::PROTOCOL,
        _ => code::REFUSED,
    }
}

/// One connection, from its open to its end.
fn connection(dial: Dial, gen: u32, shared: &Shared, out: Receiver<Out>, socket: Socket, tx: Sender<Arrival>) {
    let server = match &dial.real {
        Real::Server(server) => server,
        Real::Hub { service, model } => {
            let path = hub_path(service, model.as_deref());
            return hub_connection(&dial, gen, &path, shared, out, socket, tx);
        }
    };
    let (c, log) = (dial.c, &shared.log);
    // The address as a log line shows it: no query, which may carry a key.
    let name = match &dial.endpoint {
        Some(endpoint) => format!("{endpoint} (connection {c}) to {}", dial.real.shown()),
        None => format!("connection {c} to {}", dial.real.shown()),
    };
    let end = |code: u8, detail: u32, why: &str| {
        log(format!("firmware network: {name}: {} ({why})", code_text(code)));
        let _ = tx.send(Arrival::Ended { c, gen, code, detail });
    };
    log(format!("firmware network: {name}: opening"));
    let websocket = matches!(server.scheme, Scheme::Ws | Scheme::Wss);
    let token = match (&dial.endpoint, websocket) {
        (Some(endpoint), true) => match (shared.tokens)(endpoint, &server.host, server.scheme.tls()) {
            Ok(TokenFor::Token(secret)) if server.carries_tokens() => Some(secret),
            Ok(TokenFor::Token(_) | TokenFor::Plaintext) => {
                return end(code::NOT_CONFIGURED, 0, &format!("plaintext: the token saved as {endpoint} goes only over wss://, or to this machine; it is not sent"));
            }
            Ok(TokenFor::None) => None,
            Ok(TokenFor::OtherHost(host)) => {
                return end(code::NOT_CONFIGURED, 0, &format!("the token saved as {endpoint} is for {host}; it is not sent"));
            }
            Err(why) => return end(code::NOT_CONFIGURED, 0, &format!("its token could not be read: {why}")),
        },
        _ => None,
    };
    let mut stream = match (shared.connect)(server) {
        Ok(stream) => stream,
        Err(error) => return end(connect_failure(&error, server.scheme.tls()), 0, &error.to_string()),
    };
    if let (Ok(tcp), Ok(mut slot)) = (stream.try_clone_tcp(), socket.lock()) { *slot = Some(Cut::Server(tcp)); }
    // Closed or cut while connecting: nothing more goes out.
    if !matches!(out.try_recv(), Err(TryRecvError::Empty)) {
        stream.shutdown();
        return log(format!("firmware network: {name}: closed while opening"));
    }
    let _ = stream.set_write_timeout(None);
    let mut frames = Frames::default();
    if websocket {
        let answered = handshake(&mut stream, server, token.as_ref().map(|t| t.bytes()), &mut frames.buf);
        drop(token);
        if let Err((code, detail, why)) = answered {
            stream.shutdown();
            return end(code, detail, &why);
        }
    }
    let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
    log(format!("firmware network: {name}: ready"));
    let _ = tx.send(Arrival::Opened { c, gen });
    let mut chunk = vec![0u8; 16 * 1024];
    // Frames that came with the upgrade's answer go first.
    let mut input = std::mem::take(&mut frames.buf);
    loop {
        loop {
            match out.try_recv() {
                Ok(Out::Part { flags, bytes }) => {
                    let written = if websocket {
                        write_frame(&mut stream, part_opcode(flags), flags & PART_LAST != 0, &bytes)
                    } else {
                        stream.write_all(&bytes)
                    };
                    if let Err(error) = written.and_then(|()| stream.flush()) {
                        stream.shutdown();
                        return end(code::RESET, 0, &error.to_string());
                    }
                    let _ = tx.send(Arrival::Taken { c, gen, bytes: bytes.len() as u32 });
                }
                Ok(Out::Close) | Err(TryRecvError::Disconnected) => {
                    if websocket { let _ = write_frame(&mut stream, 8, true, &1000u16.to_be_bytes()); }
                    stream.shutdown();
                    return log(format!("firmware network: {name}: closed"));
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if input.is_empty() {
            match stream.read(&mut chunk) {
                Ok(0) => {
                    stream.shutdown();
                    return if websocket { end(code::RESET, 0, "the server went away without closing") } else { end(code::CLOSED, 0, "the server closed the stream") };
                }
                Ok(n) => input.extend_from_slice(&chunk[..n]),
                Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => continue,
                Err(error) => {
                    stream.shutdown();
                    return end(code::RESET, 0, &error.to_string());
                }
            }
        }
        if !websocket {
            let _ = tx.send(Arrival::Message { c, gen, binary: true, bytes: std::mem::take(&mut input) });
            continue;
        }
        frames.buf.append(&mut input);
        loop {
            match frames.next() {
                Ok(None) => break,
                Ok(Some(Frame::Message(binary, bytes))) => { let _ = tx.send(Arrival::Message { c, gen, binary, bytes }); }
                Ok(Some(Frame::Ping(payload))) => {
                    if let Err(error) = write_frame(&mut stream, 10, true, &payload).and_then(|()| stream.flush()) {
                        stream.shutdown();
                        return end(code::RESET, 0, &error.to_string());
                    }
                }
                Ok(Some(Frame::Close(status))) => {
                    let _ = write_frame(&mut stream, 8, true, &status.to_be_bytes());
                    stream.shutdown();
                    return end(code::CLOSED, u32::from(status), "the server closed it");
                }
                Err(why) => {
                    stream.shutdown();
                    return end(code::PROTOCOL, 0, &why);
                }
            }
        }
    }
}

/// The route a hub service's connection opens: `/<service>`, and `?model=<id>` (percent-encoded)
/// when the bench names the chat model.
fn hub_path(service: &str, model: Option<&str>) -> String {
    let mut path = format!("/{service}");
    if let Some(model) = model {
        path.push_str("?model=");
        for byte in model.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                path.push(char::from(byte));
            } else {
                path.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    path
}

/// One connection to a service of this computer's hub, from its open to its end: the first hub of
/// [`Hubs`] on loopback that takes `path`, through the hub client's opener. A hub elsewhere is
/// never dialled.
fn hub_connection(dial: &Dial, gen: u32, path: &str, shared: &Shared, out: Receiver<Out>, socket: Socket, tx: Sender<Arrival>) {
    let log = &shared.log;
    let c = dial.c;
    let name = format!("connection {c} to {}", dial.real.shown());
    let end = |code: u8, why: &str| {
        log(format!("firmware network: {name}: {} ({why})", code_text(code)));
        let _ = tx.send(Arrival::Ended { c, gen, code, detail: 0 });
    };
    log(format!("firmware network: {name}: opening"));
    let mut why = "no hub runs on this computer".to_string();
    let mut opened = None;
    for base in (shared.hubs)() {
        if !on_this_computer(&base) {
            log(format!("firmware network: {name}: {base} is not on this computer; it is not dialled"));
            continue;
        }
        match HubWebSocket::open(&base, path) {
            Ok(ws) => {
                opened = Some((ws, base));
                break;
            }
            Err(error) => why = error,
        }
    }
    let Some((mut ws, base)) = opened else { return end(code::REFUSED, &why) };
    if let (Ok(closer), Ok(mut slot)) = (ws.closer(), socket.lock()) { *slot = Some(Cut::Hub(closer)); }
    // Closed or cut while connecting: nothing more goes out.
    if !matches!(out.try_recv(), Err(TryRecvError::Empty)) {
        return log(format!("firmware network: {name}: closed while opening"));
    }
    ws.set_read_timeout(Duration::from_millis(20));
    log(format!("firmware network: {name}: ready at {base}"));
    let _ = tx.send(Arrival::Opened { c, gen });
    // The message the device is sending: whether it is binary, and its parts so far.
    let mut message: Option<(bool, Vec<u8>)> = None;
    let mut got = Vec::new();
    loop {
        loop {
            match out.try_recv() {
                Ok(Out::Part { flags, bytes }) => {
                    if flags & PART_FIRST != 0 || message.is_none() {
                        message = Some((flags & PART_BINARY != 0, Vec::with_capacity(bytes.len())));
                    }
                    if let Some((_, body)) = &mut message { body.extend_from_slice(&bytes); }
                    if flags & PART_LAST != 0 {
                        let Some((binary, body)) = message.take() else { continue };
                        let send = if binary {
                            WsSend::Binary(body)
                        } else {
                            match String::from_utf8(body) {
                                Ok(text) => WsSend::Text(text),
                                Err(_) => return end(code::PROTOCOL, "the device sent a text message that is not UTF-8"),
                            }
                        };
                        if let Err(error) = ws.send(send) { return end(code::RESET, &error.to_string()); }
                    }
                    let _ = tx.send(Arrival::Taken { c, gen, bytes: bytes.len() as u32 });
                }
                Ok(Out::Close) | Err(TryRecvError::Disconnected) => return log(format!("firmware network: {name}: closed")),
                Err(TryRecvError::Empty) => break,
            }
        }
        let read = ws.read(|frame| match frame {
            WebSocketMessage::Text(text) => got.push((false, text.as_bytes().to_vec())),
            WebSocketMessage::Binary(bytes) => got.push((true, bytes.to_vec())),
            _ => {}
        });
        // What came before a close goes first.
        for (binary, bytes) in got.drain(..) { let _ = tx.send(Arrival::Message { c, gen, binary, bytes }); }
        match read {
            Ok(()) => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => return end(code::CLOSED, "the hub closed it"),
            Err(error) => return end(code::RESET, &error.to_string()),
        }
    }
}

/// A part's WebSocket opcode: text or binary for a message's first part, continuation after.
fn part_opcode(flags: u8) -> u8 {
    if flags & PART_FIRST == 0 { 0 } else if flags & PART_BINARY != 0 { 2 } else { 1 }
}

/// The key's answer a server's upgrade must carry (RFC 6455 section 4.2.2).
fn accept(key: &str) -> String {
    let mut sha1 = Sha1::new();
    sha1.update(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
    base64_encode(&sha1.finalise())
}

/// The upgrade: the request (with the token, when one is given) and the server's 101 with the key's
/// accept. Bytes after the answer's head are frames already, left in `rest`.
fn handshake(stream: &mut SocketStream, server: &Server, token: Option<&[u8]>, rest: &mut Vec<u8>) -> Result<(), (u8, u32, String)> {
    let key = base64_encode(&nonce());
    let default_port = if server.scheme == Scheme::Wss { 443 } else { 80 };
    let host = if server.port == default_port { server.host.clone() } else { format!("{}:{}", server.host, server.port) };
    let head = format!("GET {} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n", server.path);
    // Sized up front, so no copy of the token is left behind by a reallocation.
    let mut request = Vec::with_capacity(head.len() + token.map_or(0, |t| t.len() + 26) + 2);
    request.extend_from_slice(head.as_bytes());
    if let Some(token) = token {
        request.extend_from_slice(b"Authorization: Bearer ");
        request.extend_from_slice(token);
        request.extend_from_slice(b"\r\n");
    }
    request.extend_from_slice(b"\r\n");
    let sent = stream.write_all(&request).and_then(|()| stream.flush());
    wipe(&mut request);
    sent.map_err(|e| (code::RESET, 0, e.to_string()))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut answer = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(k) = answer.windows(4).position(|w| w == b"\r\n\r\n") { break k + 4; }
        match stream.read(&mut chunk) {
            Ok(0) => return Err((code::RESET, 0, "the server closed before answering the upgrade".into())),
            Ok(n) => answer.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Err((code::TIMED_OUT, 0, "no answer to the upgrade".into())),
            Err(e) => return Err((code::RESET, 0, e.to_string())),
        }
    };
    rest.extend_from_slice(&answer[end..]);
    let text = String::from_utf8_lossy(&answer[..end]);
    let mut lines = text.split("\r\n");
    let status: u32 = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|s| s.parse().ok())
        .ok_or_else(|| (code::PROTOCOL, 0, "the answer to the upgrade is not HTTP".to_string()))?;
    if status != 101 {
        return Err((code::REJECTED, status, format!("the server answered HTTP {status}")));
    }
    let accepted = lines.filter_map(|l| l.split_once(':')).find(|(k, _)| k.trim().eq_ignore_ascii_case("sec-websocket-accept")).map(|(_, v)| v.trim().to_string());
    if accepted.as_deref() != Some(accept(&key).as_str()) {
        return Err((code::PROTOCOL, 0, "the server's upgrade does not answer its key".into()));
    }
    Ok(())
}

/// One client frame, masked as RFC 6455 section 5.3 asks of a client.
fn write_frame(stream: &mut SocketStream, opcode: u8, fin: bool, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(if fin { 0x80 } else { 0 } | opcode);
    match payload.len() {
        n if n < 126 => frame.push(0x80 | n as u8),
        n if n <= 0xffff => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    let mask: [u8; 4] = nonce()[..4].try_into().expect("four bytes");
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    stream.write_all(&frame)
}

enum Frame {
    Message(bool, Vec<u8>),
    Ping(Vec<u8>),
    Close(u16),
}

/// The server's frames, with a fragmented message put back together (control frames may come
/// between its fragments).
#[derive(Default)]
struct Frames {
    buf: Vec<u8>,
    message: Vec<u8>,
    binary: bool,
    /// A fragmented message is under way.
    open: bool,
}

impl Frames {
    fn next(&mut self) -> Result<Option<Frame>, String> {
        loop {
            let b = &self.buf;
            if b.len() < 2 { return Ok(None); }
            let (fin, opcode, masked) = (b[0] & 0x80 != 0, b[0] & 0x0f, b[1] & 0x80 != 0);
            let (len, at) = match b[1] & 0x7f {
                126 if b.len() >= 4 => (u64::from(u16::from_be_bytes([b[2], b[3]])), 4),
                127 if b.len() >= 10 => (u64::from_be_bytes(b[2..10].try_into().expect("eight bytes")), 10),
                126 | 127 => return Ok(None),
                n => (u64::from(n), 2),
            };
            if masked { return Err("the server masked a frame".into()); }
            let len = usize::try_from(len).map_err(|_| "a frame larger than memory".to_string())?;
            if b.len() < at + len { return Ok(None); }
            let payload = self.buf[at..at + len].to_vec();
            self.buf.drain(..at + len);
            match opcode {
                0 if self.open => {
                    self.message.extend_from_slice(&payload);
                    if fin {
                        self.open = false;
                        return Ok(Some(Frame::Message(self.binary, std::mem::take(&mut self.message))));
                    }
                }
                1 | 2 if !self.open => {
                    if fin { return Ok(Some(Frame::Message(opcode == 2, payload))); }
                    self.open = true;
                    self.binary = opcode == 2;
                    self.message = payload;
                }
                8 => return Ok(Some(Frame::Close(if payload.len() >= 2 { u16::from_be_bytes([payload[0], payload[1]]) } else { 1005 }))),
                9 => return Ok(Some(Frame::Ping(payload))),
                10 => {}
                _ => return Err(format!("an unexpected frame (opcode {opcode})")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firmware_net::Secret;
    use fab_elec_bench::net::Fake;
    use makepad_widgets_core::makepad_platform::makepad_network::tls::{self, generate_p256, TlsIdentity, TlsServer};
    use std::net::TcpListener;

    const TOKEN: &[u8] = b"t0k3n-for-the-test";

    /// A test server's side of a connection: a TCP stream, or TLS over one.
    trait Duplex: Read + Write {}
    impl<T: Read + Write> Duplex for T {}

    /// What a test server saw of one connection.
    #[derive(Debug, Default)]
    struct Seen {
        /// The upgrade request arrived (a refused TLS handshake carries none).
        requested: bool,
        path: String,
        authorization: Option<String>,
        /// The `X-Makepad-Machine-Token` header: how a hub knows this computer's own apps.
        machine_token: Option<String>,
        /// Each data frame's opcode and fin bit.
        frames: Vec<(u8, bool)>,
        /// The client closed it with a close frame (else the socket just ended).
        closed: bool,
    }

    fn read_frame<S: Read + ?Sized>(s: &mut S) -> Option<(u8, bool, Vec<u8>)> {
        let mut head = [0u8; 2];
        s.read_exact(&mut head).ok()?;
        let len = match head[1] & 0x7f {
            126 => { let mut l = [0u8; 2]; s.read_exact(&mut l).ok()?; u16::from_be_bytes(l) as usize }
            127 => { let mut l = [0u8; 8]; s.read_exact(&mut l).ok()?; u64::from_be_bytes(l) as usize }
            n => n as usize,
        };
        assert!(head[1] & 0x80 != 0, "every client frame is masked");
        let mut mask = [0u8; 4];
        s.read_exact(&mut mask).ok()?;
        let mut payload = vec![0u8; len];
        s.read_exact(&mut payload).ok()?;
        payload.iter_mut().enumerate().for_each(|(i, b)| *b ^= mask[i % 4]);
        Some((head[0] & 0x0f, head[0] & 0x80 != 0, payload))
    }

    fn server_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0x80 | opcode];
        if payload.len() < 126 { f.push(payload.len() as u8) } else { f.push(126); f.extend_from_slice(&(payload.len() as u16).to_be_bytes()) }
        f.extend_from_slice(payload);
        f
    }

    /// One WebSocket session: `/deny` answers 401; `/talk` first says `status ready`; then each
    /// whole message is echoed as one frame, and the text `bye` is answered by closing with 1000.
    fn session(s: &mut dyn Duplex) -> Seen {
        let mut seen = Seen::default();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if s.read_exact(&mut byte).is_err() { return seen; }
            head.push(byte[0]);
        }
        seen.requested = true;
        let head = String::from_utf8(head).expect("an ASCII request");
        seen.path = head.split(' ').nth(1).unwrap_or_default().to_string();
        let header = |name: &str| head.lines().filter_map(|l| l.split_once(':')).find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string());
        seen.authorization = header("authorization");
        seen.machine_token = header("x-makepad-machine-token");
        if seen.path == "/deny" {
            s.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n").unwrap();
            return seen;
        }
        let key = header("sec-websocket-key").expect("a key");
        s.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", accept(&key)).as_bytes()).unwrap();
        // A scripted hub's `/talk` speaks first, as the hub's route does.
        if seen.path.split('?').next() == Some("/talk") { s.write_all(&server_frame(1, b"status ready")).unwrap(); }
        let (mut message, mut opcode) = (Vec::new(), 0);
        while let Some((op, fin, payload)) = read_frame(s) {
            if op == 8 { seen.closed = true; break; }
            seen.frames.push((op, fin));
            if op != 0 { opcode = op; }
            message.extend_from_slice(&payload);
            if !fin { continue; }
            if opcode == 1 && message == b"bye" {
                s.write_all(&server_frame(8, &1000u16.to_be_bytes())).unwrap();
            } else {
                s.write_all(&server_frame(opcode, &message)).unwrap();
            }
            message.clear();
        }
        seen
    }

    /// A WebSocket server for `n` connections, one after the other, each through `wrap` (None: the
    /// connection's TLS handshake failed).
    fn websocket_server(listener: TcpListener, n: usize, wrap: impl Fn(TcpStream) -> Option<Box<dyn Duplex>> + Send + 'static) -> std::thread::JoinHandle<Vec<Seen>> {
        std::thread::spawn(move || (0..n).map(|_| {
            let (tcp, _) = listener.accept().expect("a connection");
            match wrap(tcp) {
                Some(mut s) => session(&mut *s),
                None => Seen::default(),
            }
        }).collect())
    }

    fn plain(tcp: TcpStream) -> Option<Box<dyn Duplex>> { Some(Box::new(tcp)) }

    fn dial(c: u8, endpoint: Option<&str>, url: &str) -> Dial {
        Dial { c, endpoint: endpoint.map(Into::into), real: Real::Server(Server::parse(url).expect("a test address")) }
    }

    fn next(rx: &Receiver<Arrival>) -> Arrival {
        rx.recv_timeout(Duration::from_secs(10)).expect("the network answers")
    }

    fn log_lines() -> (Arc<Mutex<Vec<String>>>, Log) {
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let log: Log = { let lines = lines.clone(); Arc::new(move |line| lines.lock().unwrap().push(line)) };
        (lines, log)
    }

    fn threads() -> Spawn { Arc::new(|work| { std::thread::spawn(work); }) }

    fn no_hubs() -> Hubs { Arc::new(Vec::new) }

    fn no_secret_in(lines: &[String]) {
        let secret = String::from_utf8_lossy(TOKEN);
        for line in lines {
            assert!(!line.contains(secret.as_ref()) && !line.contains("hidden") && !line.contains("bye"), "a log line holds a secret or a message: {line}");
        }
    }

    #[test]
    fn the_transport_reaches_only_its_dials_sends_tokens_to_their_host_and_logs_no_secret() {
        let ws = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = ws.local_addr().unwrap().port();
        let raw = TcpListener::bind("127.0.0.1:0").unwrap();
        let raw_port = raw.local_addr().unwrap().port();
        let dead = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let server = websocket_server(ws, 3, plain);
        let raw_server = std::thread::spawn(move || {
            let (mut s, _) = raw.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 64];
            while let Ok(n) = s.read(&mut buf) {
                if n == 0 { break; }
                got.extend_from_slice(&buf[..n]);
                s.write_all(&buf[..n]).unwrap();
            }
            got
        });
        // A stand-in for the settings that hands out every token it holds: the transport's own check
        // keeps one off a plain connection to another machine.
        let tokens: Tokens = Arc::new(|name, host, _encrypted| Ok(match (name, host) {
            ("chat", "127.0.0.1") | ("plain", _) => TokenFor::Token(Secret::new(TOKEN.to_vec())),
            ("other", _) => TokenFor::OtherHost("chat.example.org".into()),
            _ => TokenFor::None,
        }));
        let (lines, log) = log_lines();
        let mut t = Transport::new(vec![
            dial(0, Some("chat"), &format!("ws://127.0.0.1:{port}/ws?key=hidden")),
            dial(1, None, &format!("ws://127.0.0.1:{port}/deny")),
            dial(2, Some("other"), &format!("ws://127.0.0.1:{port}/ws")),
            dial(3, Some("gone"), &format!("ws://127.0.0.1:{dead}/")),
            dial(4, Some("raw"), &format!("tcp://127.0.0.1:{raw_port}")),
            dial(5, Some("plain"), "ws://chat.invalid/"),
        ], tokens, checked_connect(), no_hubs(), log, threads());
        let ended = |c, code, detail| Arrival::Ended { c, gen: 1, code, detail };

        t.send(&Request::Open { c: 0, gen: 1 });
        assert_eq!(next(&t.rx), Arrival::Opened { c: 0, gen: 1 });
        let (first, last) = (vec![1u8; 1000], vec![2u8; 500]);
        t.send(&Request::Send { c: 0, gen: 1, flags: PART_FIRST | PART_BINARY, bytes: first.clone() });
        t.send(&Request::Send { c: 0, gen: 1, flags: PART_LAST, bytes: last.clone() });
        assert_eq!(next(&t.rx), Arrival::Taken { c: 0, gen: 1, bytes: 1000 }, "a part is taken as it is written");
        assert_eq!(next(&t.rx), Arrival::Taken { c: 0, gen: 1, bytes: 500 });
        assert_eq!(next(&t.rx), Arrival::Message { c: 0, gen: 1, binary: true, bytes: [first, last].concat() },
            "the server's message arrives whole");
        t.send(&Request::Send { c: 0, gen: 1, flags: PART_FIRST | PART_LAST, bytes: b"bye".to_vec() });
        assert_eq!(next(&t.rx), Arrival::Taken { c: 0, gen: 1, bytes: 3 });
        assert_eq!(next(&t.rx), ended(0, code::CLOSED, 1000), "the server's close ends it with its status");

        t.send(&Request::Open { c: 1, gen: 1 });
        assert_eq!(next(&t.rx), ended(1, code::REJECTED, 401), "a refused upgrade is rejected with its HTTP status");
        t.send(&Request::Open { c: 2, gen: 1 });
        assert_eq!(next(&t.rx), ended(2, code::NOT_CONFIGURED, 0), "a token saved for another host stops the open");
        t.send(&Request::Open { c: 5, gen: 1 });
        assert_eq!(next(&t.rx), ended(5, code::NOT_CONFIGURED, 0), "a token never goes in plain text to another machine");
        t.send(&Request::Open { c: 3, gen: 1 });
        assert_eq!(next(&t.rx), ended(3, code::REFUSED, 0), "no server listening");
        t.send(&Request::Open { c: 9, gen: 1 });
        assert_eq!(next(&t.rx), ended(9, code::NOT_CONFIGURED, 0), "a connection it was not built with never goes out");

        t.send(&Request::Open { c: 4, gen: 1 });
        assert_eq!(next(&t.rx), Arrival::Opened { c: 4, gen: 1 });
        t.send(&Request::Send { c: 4, gen: 1, flags: PART_FIRST | PART_LAST | PART_BINARY, bytes: b"ping".to_vec() });
        assert_eq!(next(&t.rx), Arrival::Taken { c: 4, gen: 1, bytes: 4 });
        let mut echoed = Vec::new();
        while echoed.len() < 4 {
            let Arrival::Message { c: 4, bytes, .. } = next(&t.rx) else { panic!("a stream answers with its bytes") };
            echoed.extend(bytes);
        }
        assert_eq!(echoed, b"ping");
        t.send(&Request::Close { c: 4, gen: 1 });
        assert_eq!(raw_server.join().unwrap(), b"ping", "the stream closes after its bytes");

        t.send(&Request::Open { c: 0, gen: 2 });
        assert_eq!(next(&t.rx), Arrival::Opened { c: 0, gen: 2 });
        t.close_all();
        let seen = server.join().unwrap();
        assert_eq!(seen.len(), 3, "neither the token for another host nor the plain one connected: {seen:?}");
        assert_eq!((seen[0].path.as_str(), seen[0].frames.as_slice(), seen[0].closed), ("/ws?key=hidden", &[(2, false), (0, true), (1, true)][..], true),
            "parts go out as fragments, and a close answers the server's");
        let bearer = format!("Bearer {}", String::from_utf8_lossy(TOKEN));
        assert_eq!(seen[0].authorization.as_deref(), Some(bearer.as_str()), "the endpoint's token goes to its host, in plain text only on this machine");
        assert_eq!(seen[1].authorization, None, "an unnamed endpoint sends none");
        assert!(seen[2].frames.is_empty() && !seen[2].closed, "a cut ends the connection without a close");
        let mut late = Vec::new();
        t.answers(&mut late);
        assert!(late.is_empty(), "nothing a cut connection answers comes through");

        let lines = lines.lock().unwrap();
        no_secret_in(&lines);
        for want in ["chat (connection 0) to ws://127.0.0.1", "ready", "closed by the peer (the server closed it)", "connection 1 to ws://127.0.0.1",
            "rejected by the server", "is for chat.example.org; it is not sent", "plaintext: the token saved as plain", "raw (connection 4)"] {
            assert!(lines.iter().any(|l| l.contains(want)), "no log line says {want:?}: {lines:#?}");
        }
    }

    /// The platform's own TLS server (a self-signed identity) stands in for a real wss:// server.
    #[test]
    fn over_tls_a_token_reaches_only_a_server_the_client_trusts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let tls_server = TlsServer::new(TlsIdentity::in_memory(generate_p256().unwrap(), "fab firmware transport test", None).unwrap()).unwrap();
        let pin = tls_server.identity().fingerprint;
        let server = websocket_server(listener, 2, move |tcp| {
            let _ = tcp.set_read_timeout(Some(Duration::from_secs(10)));
            tls_server.accept(tcp).ok().map(|s| Box::new(s) as Box<dyn Duplex>)
        });
        let tokens: Tokens = Arc::new(|name, _host, encrypted| Ok(match name {
            "chat" if encrypted => TokenFor::Token(Secret::new(TOKEN.to_vec())),
            _ => TokenFor::None,
        }));
        let url = format!("wss://127.0.0.1:{port}/ws");
        let (lines, log) = log_lines();

        // The app's connector checks certificates with the system's stack, which does not trust the
        // test's: the open ends before any request, so before the token.
        let checked = {
            let mut t = Transport::new(vec![dial(0, Some("chat"), &url)], tokens.clone(), checked_connect(), no_hubs(), log.clone(), threads());
            t.send(&Request::Open { c: 0, gen: 1 });
            next(&t.rx)
        };
        assert_eq!(checked, Arrival::Ended { c: 0, gen: 1, code: code::PROTOCOL, detail: 0 }, "an untrusted certificate ends the open");

        // Pinned to the test server's certificate, the same transport speaks WebSocket inside TLS.
        let pinned: Connect = Arc::new(move |server: &Server| {
            let (stream, der) = SocketStream::connect_capture(&server.host, &server.port.to_string())?;
            if tls::fingerprint(&der) != pin {
                return Err(std::io::Error::new(ErrorKind::InvalidData, "not the test server's certificate"));
            }
            Ok(stream)
        });
        let mut t = Transport::new(vec![dial(0, Some("chat"), &url)], tokens, pinned, no_hubs(), log, threads());
        t.send(&Request::Open { c: 0, gen: 1 });
        assert_eq!(next(&t.rx), Arrival::Opened { c: 0, gen: 1 });
        // Larger than a TLS record (16 KiB), so both ways span several.
        let message: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        t.send(&Request::Send { c: 0, gen: 1, flags: PART_FIRST | PART_BINARY, bytes: message[..30_000].to_vec() });
        t.send(&Request::Send { c: 0, gen: 1, flags: PART_LAST, bytes: message[30_000..].to_vec() });
        assert_eq!(next(&t.rx), Arrival::Taken { c: 0, gen: 1, bytes: 30_000 });
        assert_eq!(next(&t.rx), Arrival::Taken { c: 0, gen: 1, bytes: 10_000 });
        assert_eq!(next(&t.rx), Arrival::Message { c: 0, gen: 1, binary: true, bytes: message }, "the echo arrives whole through timed TLS reads");
        t.send(&Request::Send { c: 0, gen: 1, flags: PART_FIRST | PART_LAST, bytes: b"bye".to_vec() });
        assert_eq!(next(&t.rx), Arrival::Taken { c: 0, gen: 1, bytes: 3 });
        assert_eq!(next(&t.rx), Arrival::Ended { c: 0, gen: 1, code: code::CLOSED, detail: 1000 });

        let seen = server.join().unwrap();
        assert!(!seen[0].requested && seen[0].authorization.is_none(), "the untrusted server got no request and no token: {:?}", seen[0]);
        let bearer = format!("Bearer {}", String::from_utf8_lossy(TOKEN));
        assert_eq!((seen[1].requested, seen[1].authorization.as_deref()), (true, Some(bearer.as_str())), "the token went inside TLS");
        assert_eq!((seen[1].frames.as_slice(), seen[1].closed), (&[(2, false), (0, true), (1, true)][..], true), "fragments and the close, inside TLS");
        no_secret_in(&lines.lock().unwrap());
    }

    #[test]
    fn a_hub_service_is_dialled_on_this_computer_only_through_the_hub_client_its_messages_whole_and_typed() {
        let hub = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = hub.local_addr().unwrap().port();
        let scripted = websocket_server(hub, 1, plain);
        // A hub named elsewhere comes first: it is skipped, not dialled (a dial to this address
        // would not answer for seconds).
        let hubs: Hubs = Arc::new(move || vec!["http://192.0.2.1:9".to_string(), format!("http://127.0.0.1:{port}")]);
        let no_tokens: Tokens = Arc::new(|_, _, _| Ok(TokenFor::None));
        let (lines, log) = log_lines();
        let talk = Dial { c: 1, endpoint: None, real: Real::Hub { service: "talk".into(), model: Some("voice/β+1&2".into()) } };
        assert_eq!(talk.real.approval(), "hub:talk", "the person approves the service, whichever model it asks");
        let mut t = Transport::new(vec![talk.clone()], no_tokens.clone(), checked_connect(), hubs, log.clone(), threads());

        t.send(&Request::Open { c: 1, gen: 1 });
        assert_eq!(next(&t.rx), Arrival::Opened { c: 1, gen: 1 }, "the hub on loopback takes /talk");
        assert_eq!(next(&t.rx), Arrival::Message { c: 1, gen: 1, binary: false, bytes: b"status ready".to_vec() }, "the hub's first line arrives as text");
        t.send(&Request::Send { c: 1, gen: 1, flags: PART_FIRST | PART_LAST, bytes: b"hello 16000".to_vec() });
        t.send(&Request::Send { c: 1, gen: 1, flags: PART_FIRST | PART_BINARY, bytes: b"M\x00".to_vec() });
        t.send(&Request::Send { c: 1, gen: 1, flags: PART_BINARY | PART_LAST, bytes: vec![1, 2, 3, 4] });
        let (mut taken, mut echoed) = (0, Vec::new());
        while echoed.len() < 2 {
            match next(&t.rx) {
                Arrival::Taken { bytes, .. } => taken += bytes,
                Arrival::Message { binary, bytes, .. } => echoed.push((binary, bytes)),
                other => panic!("the hub connection answered {other:?}"),
            }
        }
        assert_eq!(taken, 17, "every part is taken, each before the hub's answer to its message");
        assert_eq!(echoed, [(false, b"hello 16000".to_vec()), (true, b"M\x00\x01\x02\x03\x04".to_vec())], "the hub's answers arrive typed");
        // A cut reaches the hub's socket through its client's closer: the hub's session ends.
        t.close_all();
        let seen = scripted.join().unwrap();
        assert_eq!(seen[0].path, "/talk?model=voice%2F%CE%B2%2B1%262", "the model is one percent-encoded query value");
        assert_eq!(seen[0].frames, [(1, true), (2, true)], "the device's two-part message reached the hub whole, in one frame of its type");
        assert!(seen[0].authorization.is_none(), "no token of the person's goes to the hub");
        // Compared, never printed: the machine token is a credential.
        assert!(seen[0].machine_token.is_some() && seen[0].machine_token == makepad_ai_hub::machine::machine_token().ok(),
            "the hub client's opener carries this computer's own identity, as for the hub's own apps");

        // With no hub on this computer, the open is refused and nothing is dialled.
        let elsewhere: Hubs = Arc::new(|| vec!["http://192.0.2.1:9".to_string(), "https://127.0.0.1:9".to_string()]);
        let mut t = Transport::new(vec![talk], no_tokens, checked_connect(), elsewhere, log, threads());
        t.send(&Request::Open { c: 1, gen: 2 });
        assert_eq!(next(&t.rx), Arrival::Ended { c: 1, gen: 2, code: code::REFUSED, detail: 0 }, "no hub here: refused");
        let lines = lines.lock().unwrap();
        no_secret_in(&lines);
        assert!(lines.iter().any(|line| line.ends_with(&format!("ready at http://127.0.0.1:{port}"))), "the log names the hub's loopback address: {lines:?}");
        let skipped = lines.iter().filter(|line| line.ends_with("is not on this computer; it is not dialled")).count();
        assert_eq!(skipped, 3, "each hub elsewhere is logged as skipped: {lines:?}");
        assert!(lines.iter().all(|line| !line.contains("192.0.2.1") || line.ends_with("it is not dialled")), "nothing else names one: {lines:?}");
    }

    #[test]
    fn only_approved_hosts_dial() {
        let server = |url: &str| Some(Real::Server(Server::parse(url).unwrap()));
        let endpoint = |c, name: Option<&str>, real: Option<Real>| NetEndpoint {
            path: "u".into(), c, name: name.map(Into::into), real, fake: Fake::Echo, latency: 0.0, jitter: 0.0, rate: 0.0,
        };
        let endpoints = [
            endpoint(0, Some("chat"), server("wss://chat.example.org/ws")),
            endpoint(1, Some("other"), server("wss://other.example.org/")),
            endpoint(2, None, server("ws://chat.example.org:8080/")),
            endpoint(3, None, None),
            endpoint(4, None, Some(Real::Hub { service: "talk".into(), model: None })),
        ];
        let mut net = FirmwareNet::default();
        assert!(dials(&net, "p", &endpoints).is_empty(), "off by default: every connection plays its fake");
        net.turn_on("p", &["chat.example.org".to_string()], 1);
        let dial = |c, name: Option<&str>, url: &str| Dial { c, endpoint: name.map(Into::into), real: server(url).unwrap() };
        let approved = [dial(0, Some("chat"), "wss://chat.example.org/ws"), dial(2, None, "ws://chat.example.org:8080/")];
        assert_eq!(dials(&net, "p", &endpoints), approved, "only an approved host dials, named (with its token) or not");
        net.turn_on("p", &["chat.example.org".to_string(), "hub:talk".to_string()], 2);
        let talk = Dial { c: 4, endpoint: None, real: Real::Hub { service: "talk".into(), model: None } };
        assert_eq!(dials(&net, "p", &endpoints), [approved[0].clone(), approved[1].clone(), talk],
            "a hub service dials once it is approved by its own name, not with a host");
        assert!(dials(&net, "q", &endpoints).is_empty(), "the approval is the project's");
        net.turn_off("p");
        assert!(dials(&net, "p", &endpoints).is_empty(), "off again: nothing dials");
    }
}
