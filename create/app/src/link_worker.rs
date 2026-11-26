//! The link worker: one UDP socket on a thread of its own, for one first contact by words. It
//! learns our public address, makes the invite, waits for the peer's words, then runs the link
//! ([`makepad_work_core::connect`]) and carries sync and call traffic. The UI thread
//! never waits on it: commands go in through a bounded queue with `try_send`, and what the worker
//! has to say comes back on a channel the UI drains when the worker signals it. A queued command
//! wakes a worker that sleeps in its socket read ([`Waker`]), so it leaves at once.
//!
//! Call media shares the socket and follows the current link peer, including on a LAN path.

use makepad_work_core::connect::{self, first_contact_tag, Connection, Failure, Kind, Phase, ProbeTags, Remembered};
use makepad_work_core::key::{device_words, DeviceKey, PublicKey};
use makepad_work_core::link::{FirstContact, InviteSession, Mode, Pin, Reconnect, Refusal};
use makepad_work_core::peers::PathMemory;
use makepad_core_util::udp::{bind_udp_on_interface, InterfaceBind};
use makepad_network::p2p::discover::{self, Found, Hint};
use makepad_network::p2p::reflect::{self, Style};
use makepad_network::p2p::words::{self, Invite};
use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// What the UI asks of the worker.
pub enum ToWorker {
    /// The words the other person read out, and the name to show them once linked.
    PeerWords { words: String, name: String },
    /// One whole sync message for the peer.
    Send(Vec<u8>),
    /// Attach the call side once the link is up. It stays attached for the link's life and owns
    /// ring and call state; the worker only carries its traffic.
    Call(MediaPort),
    /// Detach the call side before the link ends.
    EndCall,
    Stop,
}

/// The link worker's ends of the call's bounded channels.
pub struct MediaPort {
    pub datagrams_in: SyncSender<(SocketAddr, Vec<u8>)>,
    pub datagrams_out: Receiver<Vec<u8>>,
    pub messages_in: SyncSender<Vec<u8>>,
    pub messages_out: Receiver<Vec<u8>>,
    /// Requested stream bits per second. Zero lifts the cap.
    pub rate_cap: Arc<AtomicU64>,
    /// Stream wire bytes since the link started.
    pub sent_bytes: Arc<AtomicU64>,
}

#[derive(Debug, PartialEq, Eq)]
enum Route {
    Link,
    Call,
    /// This worker's own wake: it has already done its job by ending the read.
    Wake,
    Drop,
}

fn route(packet: &[u8], from: SocketAddr, call_peer: Option<SocketAddr>, waker: &Waker) -> Route {
    if waker.is_wake(packet, from) {
        Route::Wake
    } else if Connection::is_ours(packet) {
        Route::Link
    } else if call_peer == Some(from) && Connection::is_call(packet) {
        Route::Call
    } else {
        Route::Drop
    }
}

#[derive(Default)]
struct CallTraffic {
    port: Option<MediaPort>,
    /// Call messages from the gap between the link coming up and the attach, or held by a full
    /// channel, in wire order.
    pending: VecDeque<Vec<u8>>,
    /// Call traffic moved until shortly before this time (ms).
    active_until: u64,
    applied_cap: u64,
    dropped: u64,
    reported: u64,
    next_report: u64,
}

impl CallTraffic {
    /// How long the worker may sleep. While call traffic moves it wakes every 5 ms, so media waits
    /// at most that. An attached call with nothing moving wakes it every 50 ms, so a ring or an
    /// answer goes out promptly without 200 wake-ups a second for the link's whole life.
    fn max_sleep(&self, now: u64) -> u64 {
        match &self.port {
            None => 1_000,
            Some(_) if now < self.active_until => 5,
            Some(_) => 50,
        }
    }

    fn outgoing(&mut self, conn: &mut Connection, socket: &UdpSocket, now: u64) {
        let cap = self.port.as_ref().map_or(0, |port| port.rate_cap.load(Ordering::Relaxed));
        if cap != self.applied_cap {
            conn.set_rate_cap((cap != 0).then_some(cap));
            self.applied_cap = cap;
        }
        let mut moved = false;
        if let Some(port) = &self.port {
            for message in port.messages_out.try_iter() {
                conn.send(Kind::Call, &message);
                moved = true;
            }
            if let Some(peer) = conn.peer_address() {
                for bytes in port.datagrams_out.try_iter() {
                    let _ = socket.send_to(&bytes, peer);
                    moved = true;
                }
            }
        }
        if moved {
            self.active_until = now.saturating_add(1_000);
        }
    }

    fn deliver(&mut self, sent_bytes: u64, now: u64) {
        let mut gone = false;
        if let Some(port) = &self.port {
            port.sent_bytes.store(sent_bytes, Ordering::Relaxed);
            while let Some(message) = self.pending.pop_front() {
                match port.messages_in.try_send(message) {
                    Ok(()) => self.active_until = now.saturating_add(1_000),
                    Err(TrySendError::Full(message)) => {
                        self.pending.push_front(message);
                        break;
                    }
                    Err(TrySendError::Disconnected(message)) => {
                        self.pending.push_front(message);
                        gone = true;
                        break;
                    }
                }
            }
        }
        // A call side that dropped its channels without EndCall has ended: detach it, so the
        // worker's sleep returns to normal. Undelivered messages stay queued in order.
        if gone {
            self.port = None;
        }
        if self.dropped != self.reported && now >= self.next_report {
            makepad_widgets::log!("Work call: dropped {} incoming media datagrams on a full channel", self.dropped);
            self.reported = self.dropped;
            self.next_report = now.saturating_add(1_000);
        }
    }

    fn datagram(&mut self, from: SocketAddr, bytes: &[u8], now: u64) {
        if let Some(port) = &self.port {
            self.active_until = now.saturating_add(1_000);
            if let Err(TrySendError::Full(_)) = port.datagrams_in.try_send((from, bytes.to_vec())) {
                self.dropped += 1;
            }
        }
    }
}

/// What the worker tells the UI.
#[derive(Debug)]
pub enum FromWorker {
    /// Our invite, to read out, and a note on the path when there is one.
    Invite { words: Vec<&'static str>, note: Option<&'static str>, lan: bool },
    /// Both invites name one router and carry a LAN address: the link tries the LAN directly.
    SameNetwork,
    /// The peer's words didn't decode; the UI keeps the field for another try.
    BadWords(String),
    /// The system refuses the path; the worker keeps trying, in case the person allows it.
    Blocked(&'static str),
    /// The link moved on.
    Phase(Status),
    /// The peer proved its key. Both read out the device words to check no one is between them.
    /// `reconnected`: a known device again, with no words.
    Linked { peer_device: PublicKey, version: u16, peer_name: String, own_words: String, peer_words: String, reconnected: bool },
    /// Where the linked device is on the path that carries the link, from what the worker saw.
    Path { peer_device: PublicKey, path: PathMemory },
    /// The link dropped. The worker goes on reaching the device, and says so with `Reconnecting`.
    Lost,
    /// The worker is reaching a known device with no words, since this wall time (unix seconds).
    Reconnecting { device: PublicKey, since: u64 },
    /// The device refused the gate; the worker keeps trying slowly. The line is plain words.
    Refused(String),
    /// The device proved the gate but not its pinned key. The worker stopped reconnecting to it.
    KeyChanged,
    /// One whole sync message from the peer.
    Message(Vec<u8>),
    /// The worker stopped: no address, refused words, a failed link. It says why.
    Failed(String),
}

/// Where a first contact is, for the Connect screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Asking the reflectors for our public address.
    FindingAddress,
    /// Our words are out; waiting for theirs.
    WaitingForWords,
    /// Trying to reach the other device, for `secs` seconds so far.
    Reaching { secs: u64 },
    /// Reaching a device linked with before, with no words.
    Reconnecting,
    /// Reached; setting up the secure channel.
    Securing,
    /// Checking the other device's key against its words.
    Proving,
    Linked,
}

/// How long the reflectors get, and how often a waiting socket refreshes its NAT mapping.
const REFLECT_WAIT: Duration = Duration::from_secs(3);
const KEEP_MAPPING: Duration = Duration::from_secs(20);
/// A proof that hasn't finished by then won't: the words or the path are wrong.
const PROVE_WAIT_MS: u64 = 20_000;

/// The wake datagram's bytes. The worker also checks where it came from.
const WAKE: &[u8] = b"work wake\0";

/// Wakes a worker that sleeps in its socket read when the UI queues a command: one small tagged
/// datagram to the socket's own bound address, from a socket of the waker's own, so the reader
/// knows it by its bytes and its source and drops it. One wake per burst: while one is on its
/// way, later commands send none. The worker spends it as it starts to drain the queue, so a
/// command queued after that sends the next one.
#[derive(Default)]
struct Waker {
    line: OnceLock<WakeLine>,
    pending: AtomicBool,
}

struct WakeLine {
    socket: UdpSocket,
    /// The waker's own address: the wake's source.
    from: SocketAddr,
    /// The worker socket's address: its own bound address, or loopback when it listens on every
    /// address (a bind to one interface's address is reached on that address).
    to: SocketAddr,
}

impl Waker {
    /// The worker has bound `socket`: wakes go to it from now on. A waker that can't be made is
    /// logged, and the worker still takes its commands each time it wakes on its own.
    fn serve(&self, socket: &UdpSocket) {
        let line = || -> io::Result<WakeLine> {
            let mut to = socket.local_addr()?;
            if to.ip().is_unspecified() {
                to.set_ip(Ipv4Addr::LOCALHOST.into());
            }
            let socket = UdpSocket::bind((to.ip(), 0))?;
            socket.set_nonblocking(true)?;
            Ok(WakeLine { from: socket.local_addr()?, socket, to })
        };
        match line() {
            Ok(line) => { let _ = self.line.set(line); }
            Err(e) => makepad_widgets::log!("work: the link worker can't be woken, so commands wait for its next wake-up: {e}"),
        }
    }

    /// From the UI thread, after it queued a command. One non-blocking send per burst.
    fn wake(&self) {
        let Some(line) = self.line.get() else { return };
        if self.pending.swap(true, Ordering::SeqCst) {
            return;
        }
        if line.socket.send_to(WAKE, line.to).is_err() {
            self.pending.store(false, Ordering::SeqCst);
        }
    }

    fn is_wake(&self, packet: &[u8], from: SocketAddr) -> bool {
        packet == WAKE && self.line.get().is_some_and(|line| line.from == from)
    }
}

/// The worker's end of the UI's commands: the queue, and the waker that gets it read.
struct Commands {
    queue: Receiver<ToWorker>,
    waker: Arc<Waker>,
}

impl Commands {
    /// The next command, without waiting. The wake sent for it, if any, is spent here.
    fn try_recv(&self) -> Result<ToWorker, mpsc::TryRecvError> {
        self.waker.pending.store(false, Ordering::SeqCst);
        self.queue.try_recv()
    }

    fn recv_timeout(&self, timeout: Duration) -> Result<ToWorker, mpsc::RecvTimeoutError> {
        self.waker.pending.store(false, Ordering::SeqCst);
        self.queue.recv_timeout(timeout)
    }
}

pub struct LinkWorker {
    to: SyncSender<ToWorker>,
    from: Receiver<FromWorker>,
    waker: Arc<Waker>,
}

impl LinkWorker {
    /// Starts a first contact for `device`. With `same_network`, the words also carry this
    /// device's LAN address, for a peer behind the same router. `signal` wakes the UI thread after
    /// each message the worker sends.
    pub fn start(device: Arc<DeviceKey>, same_network: bool, signal: impl Fn() + Send + 'static) -> io::Result<LinkWorker> {
        Self::spawn(signal, move |commands, tell| run(&device, same_network, commands, tell))
    }

    /// Starts reaching a device this one linked with before, with no words, for as long as the
    /// worker runs. The model syncs with one peer, so the app runs one such worker; a second
    /// known device waits until the model can hold two links.
    pub fn reconnect(device: Arc<DeviceKey>, known: Known, signal: impl Fn() + Send + 'static) -> io::Result<LinkWorker> {
        Self::spawn(signal, move |commands, tell| reconnect(&device, known, commands, tell))
    }

    /// The worker thread: `body` gets the command queue and the way to tell the UI thread.
    fn spawn(
        signal: impl Fn() + Send + 'static,
        body: impl FnOnce(&Commands, &dyn Fn(FromWorker)) -> Result<(), String> + Send + 'static,
    ) -> io::Result<LinkWorker> {
        let (to, queue) = mpsc::sync_channel(256);
        let waker = Arc::new(Waker::default());
        let commands = Commands { queue, waker: waker.clone() };
        let (events, from) = mpsc::channel();
        std::thread::Builder::new().name("create link".into()).spawn(move || {
            let tell = |event: FromWorker| {
                let _ = events.send(event);
                signal();
            };
            if let Err(why) = body(&commands, &tell) {
                tell(FromWorker::Failed(why));
            }
        })?;
        Ok(LinkWorker { to, from, waker })
    }

    /// Hands the worker `command` without waiting, and wakes it. `Err` gives it back when the
    /// queue is full, for another try on a later frame.
    pub fn try_send(&self, command: ToWorker) -> Result<(), ToWorker> {
        match self.to.try_send(command) {
            Ok(()) => {
                self.waker.wake();
                Ok(())
            }
            Err(TrySendError::Full(command)) => Err(command),
            // The worker has stopped; what it said is already in the channel.
            Err(TrySendError::Disconnected(_)) => Ok(()),
        }
    }

    pub fn try_recv(&self) -> Option<FromWorker> {
        self.from.try_recv().ok()
    }
}

impl Drop for LinkWorker {
    fn drop(&mut self) {
        // The worker sees the queue close on its next wake, or this when there is room.
        let _ = self.to.try_send(ToWorker::Stop);
    }
}

fn run(device: &Arc<DeviceKey>, same_network: bool, commands: &Commands, tell: &dyn Fn(FromWorker)) -> Result<(), String> {
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let socket = bind_link(0)?;

    // Our public address, from the reflectors, over the link's own socket and interface.
    tell(FromWorker::Phase(Status::FindingAddress));
    let servers = reflect::resolve(&reflect::DEFAULT_SERVERS).map_err(|e| format!("couldn't find the address reflectors: {e}"))?;
    let reflection = reflect::ask(&socket, &servers, Style::Plain, REFLECT_WAIT).map_err(|e| format!("asking for our address: {e}"))?;
    let reflected = match reflection.mapped() {
        Some(SocketAddr::V4(addr)) => Some((addr, reflection.mapping())),
        _ => None,
    };
    let choice = discover::choose(&Found { router: None, reflected });
    let Some(addr) = choice.address else {
        return Err("couldn't learn this machine's public address: is it online?".into());
    };

    // The invite: a one-hour key, committed to in the words.
    let mut session = InviteSession::open(now()).map_err(|e| format!("no invite key: {e}"))?;
    let invite_key = session.key(now()).expect("a new invite serves");
    // This device's LAN address joins the words when the person allows it. Learning it sends
    // nothing; the first LAN packet goes only once both invites name one router.
    let lan = if same_network {
        match lan_address(&socket, addr) {
            Ok(lan) => Some(lan),
            Err(why) => {
                makepad_widgets::log!("work: no local network address for the words ({why})");
                None
            }
        }
    } else { None };
    let own_invite = Invite { nat: choice.hint, addr, lan, commit: Invite::commitment(choice.hint, addr, lan, &invite_key.public()) };
    let note = if choice.hint == Hint::Symmetric { Some("this network's firewall may block a direct path") } else { None };
    tell(FromWorker::Invite { words: own_invite.to_words(), note, lan: lan.is_some() });
    tell(FromWorker::Phase(Status::WaitingForWords));

    // Their words. Meanwhile keep our NAT mapping, the one the words name, alive.
    let (peer_invite, name) = loop {
        match commands.recv_timeout(KEEP_MAPPING) {
            Ok(ToWorker::PeerWords { words, name }) => match Invite::from_words(&words) {
                Ok((invite, _)) => break (invite, name),
                Err(e) => tell(FromWorker::BadWords(e.to_string())),
            },
            Ok(ToWorker::Send(_) | ToWorker::Call(_) | ToWorker::EndCall) => {}
            Ok(ToWorker::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if session.key(now()).is_none() {
                    return Err("the words have expired: start again for new ones".into());
                }
                let mut transaction = [0u8; 12];
                let _ = makepad_network::tls::os_random(&mut transaction);
                if let Some(server) = servers.first() {
                    let _ = socket.send_to(&reflect::request(&transaction, Style::Plain), server);
                }
            }
        }
    };

    link(&socket, start, device, Contact { session, invite_key, own_invite, peer_invite, name }, commands, tell)
}

/// A first contact once both invites are known.
struct Contact {
    session: InviteSession,
    invite_key: Arc<DeviceKey>,
    own_invite: Invite,
    peer_invite: Invite,
    name: String,
}

/// What a worker needs to reach a device again: the pin, and where it was seen last. Plain data
/// from the device's record, so the worker shares nothing with the UI thread.
#[derive(Clone, Debug)]
pub struct Known {
    pub pin: Pin,
    pub name: String,
    pub path: PathMemory,
}

/// A reconnect keeps trying a refused device this often, and any other failure a little sooner.
const REFUSED_RETRY_MS: u64 = connect::RECONNECT_IDLE_INTERVAL_MS;
const RETRY_MS: u64 = 2_000;
/// How long a deliberate stop waits for its close to be acknowledged.
const FAREWELL_MS: u64 = 2_000;

/// What the worker tells the UI about a device that refused the gate. Plain, and never a Debug
/// text: the device may have been reset, or it removed this link.
pub const GATE_REFUSED: &str = "Couldn't prove this device. It may have been reset, or it removed this link.";
/// A device that proved the gate but not its pinned key. Reconnecting stops.
pub const KEY_CHANGED: &str = "This isn't the device you linked with. Someone may be between you, or they set up a new device. \
    Link again with new words if it is them.";

fn wall_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The worker's fixed surroundings: the socket and clock it runs on, and how it talks.
struct Env<'a> {
    socket: &'a UdpSocket,
    start: Instant,
    device: &'a Arc<DeviceKey>,
    commands: &'a Commands,
    tell: &'a dyn Fn(FromWorker),
}

impl Env<'_> {
    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// The device a worker is linked with, or reaches for, and where it was last seen.
struct Established {
    peer: PublicKey,
    version: u16,
    path: Remembered,
}

/// How one run of a connection ended.
enum End {
    /// The app stopped the worker (the close was sent when there was a link to close).
    Stopped,
    /// The other device closed the link, or started over. A restart's probe goes to the next
    /// connection, which is the only one that has heard the other device's new nonce.
    Dropped { probe: Option<(SocketAddr, Vec<u8>)> },
    /// The connection failed; `refusal` when the key proof refused the peer.
    Failed { why: String, refusal: Option<Refusal> },
}

/// A connection's extras: what only a first contact or only a reconnect has.
struct Extras {
    /// A first contact: the invite session, claimed when the link comes up.
    invite: Option<InviteSession>,
    /// A reconnect: the link secret the probe tags come from, and the hour they are for.
    tags: Option<([u8; 32], u64)>,
}

/// The link from known invites: punch, stream, TLS and the proof, then sync and call traffic.
/// Once linked, the worker goes on as a reconnect worker for that device: a link that drops
/// is reached again with no words. `start` is the clock the invite session was opened on.
fn link(socket: &UdpSocket, start: Instant, device: &Arc<DeviceKey>, contact: Contact, commands: &Commands, tell: &dyn Fn(FromWorker)) -> Result<(), String> {
    commands.waker.serve(socket);
    let env = Env { socket, start, device, commands, tell };
    let Contact { session, invite_key, own_invite, peer_invite, name } = contact;
    let mode = Mode::FirstContact(FirstContact { invite_key, own_invite, peer_invite: peer_invite.clone(), device: device.clone(), name });
    let tag = first_contact_tag(&mode).expect("a first contact has a tag");
    let mut random = [0u8; 72];
    makepad_network::tls::os_random(&mut random).map_err(|e| format!("no random bytes: {e}"))?;
    let mut conn = Connection::new(mode, SocketAddr::V4(peer_invite.addr), tag, random, env.now()).map_err(|refusal| match refusal {
        other => {
            makepad_widgets::log!("Work link: refused before starting: {other:?}");
            refusal_text(&other)
        }
    })?;
    let mut established = None;
    let mut extras = Extras { invite: Some(session), tags: None };
    match run_connection(&env, &mut conn, &mut extras, &mut established) {
        End::Stopped => Ok(()),
        End::Failed { why, .. } if established.is_none() => Err(why),
        // The link was up: from here the worker keeps the device reached.
        _ => match established {
            Some(known) => reconnecting(&env, known),
            None => Ok(()),
        },
    }
}

/// A worker that reaches a device it has linked with before, from what was recorded of it.
fn reconnect(device: &Arc<DeviceKey>, known: Known, commands: &Commands, tell: &dyn Fn(FromWorker)) -> Result<(), String> {
    let start = Instant::now();
    // The port this device was last reached from, so the address it remembers of us stays right.
    let socket = match known.path.port.map(bind_link) {
        Some(Ok(socket)) => socket,
        _ => bind_link(0)?,
    };
    commands.waker.serve(&socket);
    let env = Env { socket: &socket, start, device, commands, tell };
    let path = Remembered { public: known.path.addr, lan: known.path.lan };
    reconnecting(&env, Established { peer: known.pin.device, version: known.pin.min_version, path })
}

/// Reaches `target` and stays linked to it for as long as the worker runs: a connection that
/// ends goes back to punching, with the same remembered addresses and no words. It never gives
/// up. A refused gate waits longer between tries; a key that changed ends it.
fn reconnecting(env: &Env, mut target: Established) -> Result<(), String> {
    let secret = env.device.link_secret(&target.peer).map_err(|e| format!("no link secret: {e}"))?;
    let mut carried: Option<(SocketAddr, Vec<u8>)> = None;
    loop {
        (env.tell)(FromWorker::Reconnecting { device: target.peer, since: wall_secs() });
        (env.tell)(FromWorker::Phase(Status::Reconnecting));
        let hour = connect::probe_hour(wall_secs());
        let mut random = [0u8; 72];
        makepad_network::tls::os_random(&mut random).map_err(|e| format!("no random bytes: {e}"))?;
        let mode = Mode::Reconnect(Reconnect { device: env.device.clone(), peers: vec![Pin { device: target.peer, min_version: target.version }] });
        let mut conn = Connection::reconnect(mode, target.path, &ProbeTags::new(&secret, hour), random, env.now()).map_err(|r| format!("can't reach this device again: {r:?}"))?;
        if let Some((from, probe)) = carried.take() {
            conn.handle(from, &probe, env.now());
        }
        let mut extras = Extras { invite: None, tags: Some((secret, hour)) };
        let mut established = None;
        let end = run_connection(env, &mut conn, &mut extras, &mut established);
        if let Some(seen) = established {
            target.version = seen.version;
            target.path = Remembered { public: seen.path.public.or(target.path.public), lan: seen.path.lan.or(target.path.lan) };
        }
        let pause = match end {
            End::Stopped => return Ok(()),
            End::Dropped { probe } => {
                carried = probe;
                0
            }
            End::Failed { refusal: Some(Refusal::KeyChanged), .. } => {
                (env.tell)(FromWorker::KeyChanged);
                return Ok(());
            }
            End::Failed { refusal: Some(Refusal::Version), why } => {
                (env.tell)(FromWorker::Refused(why));
                REFUSED_RETRY_MS
            }
            End::Failed { refusal: Some(_), .. } => {
                (env.tell)(FromWorker::Refused(GATE_REFUSED.to_owned()));
                REFUSED_RETRY_MS
            }
            End::Failed { .. } => RETRY_MS,
        };
        if wait_or_stop(env, pause) {
            return Ok(());
        }
    }
}

/// Waits `ms`, answering nothing, and returns true when the worker was told to stop.
fn wait_or_stop(env: &Env, ms: u64) -> bool {
    let until = env.now() + ms;
    let mut buf = [0u8; 2048];
    loop {
        match env.commands.try_recv() {
            Ok(ToWorker::Stop) | Err(mpsc::TryRecvError::Disconnected) => return true,
            Ok(_) => continue,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let left = until.saturating_sub(env.now());
        if left == 0 {
            return false;
        }
        let _ = env.socket.set_read_timeout(Some(Duration::from_millis(left.clamp(1, 200))));
        // Datagrams that arrive while no connection runs are the peer's probes: it tries again.
        let _ = env.socket.recv_from(&mut buf);
    }
}

/// Sends the stream's own close and gives it a moment to be acknowledged, so the other device
/// sees the end at once and goes back to reconnecting instead of waiting for a timeout.
fn farewell(env: &Env, conn: &mut Connection) {
    conn.close();
    let until = env.now() + FAREWELL_MS;
    let mut buf = [0u8; 2048];
    while env.now() < until {
        for (to, bytes) in conn.poll(env.now()) {
            let _ = env.socket.send_to(&bytes, to);
        }
        if conn.close_done() {
            return;
        }
        let _ = env.socket.set_read_timeout(Some(Duration::from_millis(20)));
        if let Ok((n, from)) = env.socket.recv_from(&mut buf) {
            if Connection::is_ours(&buf[..n]) {
                conn.handle(from, &buf[..n], env.now());
            }
        }
    }
}

/// Runs one connection: sync and call traffic until it ends. `established` records the device it
/// linked with, and where.
fn run_connection(env: &Env, conn: &mut Connection, extras: &mut Extras, established: &mut Option<Established>) -> End {
    let Env { socket, device, commands, tell, .. } = *env;
    let now = || env.now();
    let reconnecting = extras.tags.is_some();
    let mut shown = if reconnecting { Status::Reconnecting } else { Status::Reaching { secs: 0 } };
    tell(FromWorker::Phase(shown));
    if conn.lan_probing().is_some() && !reconnecting {
        tell(FromWorker::SameNetwork);
    }
    let mut proving_since = None;
    let mut blocked = false;
    let mut call = CallTraffic::default();
    let mut told_path = None;
    let mut buf = vec![0u8; 2048];
    loop {
        // Commands, without waiting.
        loop {
            match commands.try_recv() {
                Ok(ToWorker::Send(message)) => conn.send(Kind::Sync, &message),
                Ok(ToWorker::Call(port)) => {
                    if conn.phase() == &Phase::Linked {
                        call.port = Some(port);
                    }
                }
                Ok(ToWorker::EndCall) => call.port = None,
                Ok(ToWorker::PeerWords { .. }) => {}
                Ok(ToWorker::Stop) | Err(mpsc::TryRecvError::Disconnected) => {
                    farewell(env, conn);
                    return End::Stopped;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        // The reconnect tags turn over with the hour; the neighbouring hours stay accepted.
        if let Some((secret, hour)) = &mut extras.tags {
            let current = connect::probe_hour(wall_secs());
            if current != *hour {
                conn.set_probe_tags(&ProbeTags::new(secret, current));
                *hour = current;
            }
        }
        call.outgoing(conn, socket, now());
        for (to, bytes) in conn.poll(now()) {
            if let Err(e) = socket.send_to(&bytes, to) {
                if local_network_off(&e) && !blocked {
                    blocked = true;
                    tell(FromWorker::Blocked(LOCAL_NETWORK_OFF));
                }
            }
        }
        while let Some((kind, message)) = conn.recv() {
            match kind {
                Kind::Sync => tell(FromWorker::Message(message)),
                Kind::Call => call.pending.push_back(message),
                Kind::Blob => {} // the blob lane (L3b) takes these
            }
        }
        call.deliver(conn.sent_bytes(), now());

        if conn.dropped().is_some() {
            if established.is_some() {
                tell(FromWorker::Lost);
            }
            return End::Dropped { probe: conn.take_restart_probe() };
        }
        let status = match conn.phase() {
            Phase::Punching { .. } if reconnecting => Status::Reconnecting,
            Phase::Punching { since } => Status::Reaching { secs: now().saturating_sub(*since) / 1000 },
            Phase::Securing => Status::Securing,
            Phase::Proving => Status::Proving,
            Phase::Linked => Status::Linked,
            Phase::Failed(failure) if conn.done(now()) => {
                if established.is_some() {
                    tell(FromWorker::Lost);
                }
                let refusal = match failure {
                    Failure::Refused(refusal) => Some(refusal.clone()),
                    _ => None,
                };
                // A first contact explains itself in full; a reconnect words its own refusals.
                return End::Failed { why: describe(failure), refusal };
            }
            Phase::Failed(_) => Status::Proving, // Still transmitting the version notice.
        };
        if conn.phase() == &Phase::Proving {
            let since = *proving_since.get_or_insert(now());
            if now() - since > PROVE_WAIT_MS {
                if established.is_some() {
                    tell(FromWorker::Lost);
                }
                let why = "the other device didn't prove itself: check both sets of words and try again".to_owned();
                return End::Failed { why, refusal: None };
            }
        }
        if status != shown {
            if status == Status::Linked {
                if let Some(session) = &mut extras.invite {
                    if !session.claim(now()) {
                        return End::Failed { why: "the words have expired: start again for new ones".into(), refusal: None };
                    }
                }
                let peer = conn.peer().expect("a linked connection knows its peer");
                *established = Some(Established { peer: peer.device, version: peer.version, path: conn.path().unwrap_or_default() });
                tell(FromWorker::Linked {
                    peer_device: peer.device,
                    version: peer.version,
                    peer_name: peer.name,
                    own_words: device_words(&device.public()).join(" "),
                    peer_words: device_words(&peer.device).join(" "),
                    reconnected: reconnecting,
                });
            }
            tell(FromWorker::Phase(status));
            shown = status;
        }
        // Where the peer is on the path that carries the link, as this worker sees it. The
        // address is recorded when the link comes up, and again if the path moves.
        if status == Status::Linked {
            let seen = conn.path();
            if seen != told_path {
                if let (Some(path), Some(known)) = (seen, established.as_mut()) {
                    known.path = path;
                    tell(FromWorker::Path {
                        peer_device: known.peer,
                        path: PathMemory { addr: path.public, lan: path.lan, port: socket.local_addr().ok().map(|a| a.port()) },
                    });
                }
                told_path = seen;
            }
        }

        let wake = conn.next_wake().saturating_sub(now()).clamp(1, call.max_sleep(now()));
        let _ = socket.set_read_timeout(Some(Duration::from_millis(wake)));
        match socket.recv_from(&mut buf) {
            Ok((n, from)) => match route(&buf[..n], from, call.port.as_ref().and_then(|_| conn.peer_address()), &commands.waker) {
                Route::Link => conn.handle(from, &buf[..n], now()),
                Route::Call => call.datagram(from, &buf[..n], now()),
                Route::Wake | Route::Drop => {}
            },
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            // Windows reports an ICMP port-unreachable on the next receive; the punch carries on.
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
            Err(e) if local_network_off(&e) => {}
            Err(e) => return End::Failed { why: format!("the socket failed: {e}"), refusal: None },
        }
    }
}

/// After this long without an answer, the status line suggests checking the words.
const NO_ANSWER_HINT_SECS: u64 = 20;

const LOCAL_NETWORK_OFF: &str = "Waiting for Local Network access: choose Allow in the macOS prompt, or turn it on in \
     System Settings, Privacy & Security, Local Network.";

/// The developer switch that sends the link through one network interface (`en1`), so a machine
/// whose routes prefer another one (Ethernet first) can still link over this one (a phone's
/// hotspot). The reflectors are asked over the same socket, so the words carry that network's
/// public address. Unset, the system's routes decide.
const LINK_INTERFACE: &str = "MAKEPAD_WORK_LINK_IF";

fn link_interface() -> Option<String> {
    std::env::var(LINK_INTERFACE).ok().map(|name| name.trim().to_owned()).filter(|name| !name.is_empty())
}

/// The link's socket on `port` (0: any free one), through the interface [`LINK_INTERFACE`] names,
/// with one log line naming it. A switch that can't be honoured is an error, never the default
/// route: that would link over the path the developer asked to avoid.
fn bind_link(port: u16) -> Result<UdpSocket, String> {
    let (socket, tied) = bind_link_quietly(port)?;
    match tied {
        Some((name, InterfaceBind::Device)) => {
            makepad_widgets::log!("work: the link goes only through network interface {name} ({LINK_INTERFACE})")
        }
        Some((name, InterfaceBind::Address(addr))) => makepad_widgets::log!(
            "work: the link uses network interface {name}'s address {addr} ({LINK_INTERFACE}); the routes still choose the way out"
        ),
        None => {}
    }
    Ok(socket)
}

fn bind_link_quietly(port: u16) -> Result<(UdpSocket, Option<(String, InterfaceBind)>), String> {
    match link_interface() {
        None => UdpSocket::bind(("0.0.0.0", port)).map(|socket| (socket, None)).map_err(|e| format!("no socket: {e}")),
        Some(name) => match bind_udp_on_interface(&name, port) {
            Ok((socket, how)) => Ok((socket, Some((name, how)))),
            Err(e) => Err(format!("couldn't send the link through network interface {name} ({LINK_INTERFACE}): {e}")),
        },
    }
}

/// This machine's LAN address on the route toward `public`, with `socket`'s port. Connecting a
/// UDP socket sends nothing; it only picks the interface (the switch's, when it is set).
fn lan_address(socket: &UdpSocket, public: SocketAddrV4) -> Result<SocketAddrV4, String> {
    let (probe, _) = bind_link_quietly(0)?;
    probe.connect(public).map_err(|e| format!("no route: {e}"))?;
    let SocketAddr::V4(local) = probe.local_addr().map_err(|e| e.to_string())? else {
        return Err("no IPv4 route".into())
    };
    let lan = SocketAddrV4::new(*local.ip(), socket.local_addr().map_err(|e| e.to_string())?.port());
    if words::lan_candidate_ok(lan) { Ok(lan) }
    else if !lan.ip().is_private() { Err("address outside the private ranges".into()) }
    else { Err("the words cannot carry this private address or port".into()) }
}

/// macOS refuses a send to the local network without the person's Local Network permission, and
/// says only "no route to host" (EHOSTUNREACH).
fn local_network_off(e: &io::Error) -> bool {
    cfg!(target_vendor = "apple") && (e.kind() == io::ErrorKind::HostUnreachable || e.raw_os_error() == Some(65))
}

/// What the Connect screen shows, kept by the UI from what the worker tells it.
#[derive(Default)]
pub struct ConnectView {
    pub confirm_forget: bool,
    pub devices_error: Option<String>,
    /// A first contact is running (the worker is up).
    pub open: bool,
    /// Our invite words, once the address is known, in spoken groups of four. Selected and
    /// copied as shown, they decode.
    pub own_words: Option<String>,
    /// A note on the path, when there is one.
    pub note: Option<&'static str>,
    /// Our words carry this device's LAN address too.
    pub lan_in_words: bool,
    /// The UI has handed their words to the worker, which may already be reaching them.
    pub peer_words_submitted: bool,
    /// The link is trying the LAN: both are behind one router.
    pub same_network: bool,
    pub status: Option<Status>,
    /// Why the last words typed didn't decode.
    pub bad_words: Option<String>,
    /// What the system refuses, while it does.
    pub blocked: Option<&'static str>,
    /// Who we linked with, and the device words both read out.
    pub linked: Option<Linked>,
    /// Why the worker stopped.
    pub failed: Option<String>,
    /// The device this worker is linked with or reaching, once known.
    pub peer: Option<PublicKey>,
    /// Since when (unix seconds) the worker has been reaching `peer` with no words.
    pub reaching_since: Option<u64>,
    /// Why the device refused the last try, in plain words.
    pub refused: Option<String>,
}

/// Where one known device stands, for the seat that shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceLink {
    Linked,
    /// A worker is reaching it, since this wall time (unix seconds).
    Reaching { since: u64 },
    /// Nothing is reaching it; it was last linked at `last_seen` (unix seconds).
    Offline { last_seen: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    pub peer_name: String,
    pub own_words: String,
    pub peer_words: String,
}

impl ConnectView {
    /// A changed setting starts a fresh invite only while nobody has begun using the old one
    /// here. In every other phase the preference applies at the next Start.
    pub fn restart_for_lan_change(&self, on: bool, peer_words: &str) -> Option<bool> {
        (self.open && self.status == Some(Status::WaitingForWords) && self.own_words.is_some()
            && self.peer.is_none() && self.linked.is_none() && !self.peer_words_submitted
            && peer_words.trim().is_empty()).then_some(on)
    }

    /// Takes in one thing the worker said. Sync messages are the model's, not the view's.
    pub fn apply(&mut self, event: &FromWorker) {
        match event {
            FromWorker::Invite { words, note, lan } => {
                self.own_words = Some(words::group(words));
                self.note = *note;
                self.lan_in_words = *lan;
            }
            FromWorker::SameNetwork => self.same_network = true,
            FromWorker::BadWords(why) => self.bad_words = Some(why.clone()),
            FromWorker::Blocked(why) => self.blocked = Some(*why),
            FromWorker::Phase(status) => {
                self.status = Some(*status);
                if *status != Status::WaitingForWords {
                    self.bad_words = None;
                }
                if matches!(status, Status::Securing | Status::Proving | Status::Linked) {
                    self.blocked = None;
                }
            }
            FromWorker::Linked { peer_device, peer_name, own_words, peer_words, .. } => {
                self.linked = Some(Linked { peer_name: peer_name.clone(), own_words: own_words.clone(), peer_words: peer_words.clone() });
                (self.peer, self.reaching_since, self.refused) = (Some(*peer_device), None, None);
            }
            FromWorker::Message(_) | FromWorker::Path { .. } => {}
            FromWorker::Lost => self.linked = None,
            // Reaching a known device needs no words: the first contact's form and notes go. A
            // refusal stays until a link proves otherwise, so its line doesn't flicker between tries.
            FromWorker::Reconnecting { device, since } => {
                *self = ConnectView {
                    confirm_forget: self.confirm_forget, devices_error: self.devices_error.take(), refused: self.refused.take(),
                    open: true, peer: Some(*device), reaching_since: Some(*since), ..ConnectView::default()
                };
            }
            FromWorker::Refused(line) => self.refused = Some(line.clone()),
            FromWorker::KeyChanged => {
                (self.open, self.linked, self.reaching_since, self.refused) = (false, None, None, None);
                self.failed = Some(KEY_CHANGED.to_owned());
            }
            // A link that drops is over: the screen offers a fresh start, with the reason.
            FromWorker::Failed(why) => (self.open, self.linked, self.failed) = (false, None, Some(why.clone())),
        }
    }

    /// Where `device` stands, from what the worker said. `last_seen` is its record's last link.
    pub fn device_link(&self, device: &PublicKey, last_seen: u64) -> DeviceLink {
        match (self.peer.as_ref() == Some(device), self.linked.is_some(), self.reaching_since) {
            (true, true, _) => DeviceLink::Linked,
            (true, false, Some(since)) if self.open => DeviceLink::Reaching { since },
            _ => DeviceLink::Offline { last_seen },
        }
    }

    /// What the worker is doing about a device it has no link to, and a plain line on why it
    /// can't be proven when it refused.
    pub fn reaching(&self) -> Option<(&PublicKey, Option<&str>)> {
        match (&self.peer, self.reaching_since, self.linked.is_none() && self.open) {
            (Some(peer), Some(_), true) => Some((peer, self.refused.as_deref())),
            _ => None,
        }
    }

    /// What the words tell whoever hears them.
    pub fn words_privacy(&self) -> &'static str {
        if self.lan_in_words {
            "These words hold your internet address and this device's address on your network."
        } else {
            "These words hold only your internet address."
        }
    }

    /// The line shown while the link tries the LAN, at the moment the first LAN packet goes out.
    pub const SAME_NETWORK: &'static str = if cfg!(target_vendor = "apple") {
        "You're on the same network. Work connects directly; macOS may ask for Local Network access."
    } else {
        "You're on the same network, so Work connects directly."
    };

    /// The status line, in plain words.
    pub fn status_text(&self) -> String {
        match self.status {
            None => String::new(),
            Some(Status::FindingAddress) => "Finding this machine's address…".into(),
            Some(Status::WaitingForWords) => "Read your words to the other person, and type theirs.".into(),
            // A hint, not a timeout: the punch keeps trying while the other person catches up.
            Some(Status::Reaching { secs }) if secs >= NO_ANSWER_HINT_SECS => format!(
                "Reaching the other device… {secs} s. No answer yet: check the words with the other person, and that their Work is open."
            ),
            Some(Status::Reaching { secs }) => format!("Reaching the other device… {secs} s"),
            Some(Status::Reconnecting) => "Reaching the other device…".into(),
            Some(Status::Securing) => "Reached it. Setting up a secure channel…".into(),
            Some(Status::Proving) => "Checking the other device's key against its words…".into(),
            Some(Status::Linked) => "Linked.".into(),
        }
    }
}

const VERSION_TEXT: &str = "The other device runs a different version of Work. Both of you need to update Work, then start again.";

/// What a refusal says to a person, in plain words and with no debug text. The detail is logged
/// by the caller. The attacker warning is only for the refusals that can mean someone in the
/// middle; a malformed or unexpected message is what an older build sends, so it reads as a
/// version problem.
fn refusal_text(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Version | Refusal::Format(_) => VERSION_TEXT.into(),
        Refusal::Gate | Refusal::Signature | Refusal::Commitment => {
            "The other device couldn't be proven. Check both sets of words. If they're right, someone may be between you.".into()
        }
        Refusal::KeyChanged => "The other device's key is not the one you linked with before. Link again with fresh words only if you know it changed.".into(),
        Refusal::SelfLink => "Those are this device's own words. Read the other device's words instead.".into(),
        Refusal::Candidates => "Work couldn't tell which device this is. Start again.".into(),
        Refusal::Crypto(_) => "This device couldn't make the keys for the link. Start again, and restart Work if it keeps happening.".into(),
    }
}

fn describe(failure: &Failure) -> String {
    makepad_widgets::log!("Work link: failed: {failure:?}");
    match failure {
        Failure::Refused(refusal) => refusal_text(refusal),
        Failure::Tls(_) => "The secure channel to the other device couldn't be set up. Check that both devices run the same version of Work, then start again.".into(),
        Failure::Stream(_) => "The connection to the other device dropped. Check that both devices are online, then start again.".into(),
    }
}

/// Two workers over 127.0.0.1 in one process, with no reflectors and no words, started on a first
/// contact. Each item is a worker and its own socket address. Both invites carry their loopback
/// address as the LAN candidate too: a connection takes datagrams from a local source only on
/// that path.
#[cfg(test)]
fn spawn_pair(a: Arc<DeviceKey>, b: Arc<DeviceKey>, progress: Arc<Progress>) -> [(LinkWorker, SocketAddrV4); 2] {
    let start = Instant::now();
    let side = |device: Arc<DeviceKey>| {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("a loopback socket");
        let SocketAddr::V4(addr) = socket.local_addr().unwrap() else { unreachable!("bound to IPv4") };
        let mut session = InviteSession::open(0).expect("an invite key");
        let invite_key = session.key(0).expect("a new invite serves");
        let invite = Invite { nat: Hint::Easy, addr, lan: Some(addr), commit: Invite::commitment(Hint::Easy, addr, Some(addr), &invite_key.public()) };
        (socket, device, session, invite_key, invite)
    };
    let (sa, da, session_a, key_a, invite_a) = side(a);
    let (sb, db, session_b, key_b, invite_b) = side(b);
    let spawn = |socket: UdpSocket, device: Arc<DeviceKey>, session, invite_key, own_invite: Invite, peer_invite: Invite, name: &str| {
        let addr = own_invite.addr;
        let contact = Contact { session, invite_key, own_invite, peer_invite, name: name.to_owned() };
        let progress = progress.clone();
        // A worker that panics says so, as a failure, so a test waiting on it ends.
        let body = move |commands: &Commands, tell: &dyn Fn(FromWorker)| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| link(&socket, start, &device, contact, commands, tell)))
                .unwrap_or_else(|_| Err("the worker panicked".to_owned()))
        };
        (LinkWorker::spawn(move || progress.tell(), body).expect("a worker thread"), addr)
    };
    [
        spawn(sa, da, session_a, key_a, invite_a.clone(), invite_b.clone(), "Ada"),
        spawn(sb, db, session_b, key_b, invite_b, invite_a, "Bob"),
    ]
}

/// Counts the events a test's link workers have sent, so the test waits on their progress and
/// never on a clock. A test that hangs is the test runner's to catch.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Progress {
    said: std::sync::Mutex<u64>,
    wake: std::sync::Condvar,
}

#[cfg(test)]
impl Progress {
    fn tell(&self) {
        *self.said.lock().unwrap() += 1;
        self.wake.notify_all();
    }

    /// How many events the workers have sent so far.
    pub(crate) fn said(&self) -> u64 {
        *self.said.lock().unwrap()
    }

    /// Waits until the workers have sent more than `seen` events.
    pub(crate) fn wait_past(&self, seen: u64) {
        let mut said = self.said.lock().unwrap();
        while *said <= seen {
            said = self.wake.wait(said).unwrap();
        }
    }
}

/// Waits for the first event `wanted` takes; the others are skipped. A worker that fails or
/// stops ends the wait with its reason.
#[cfg(test)]
fn wait_for<T>(worker: &LinkWorker, what: &str, mut wanted: impl FnMut(FromWorker) -> Option<T>) -> T {
    loop {
        match worker.from.recv() {
            Ok(FromWorker::Failed(why)) => panic!("the worker failed while waiting for {what}: {why}"),
            Ok(event) => {
                if let Some(found) = wanted(event) {
                    return found;
                }
            }
            Err(_) => panic!("the worker stopped while waiting for {what}"),
        }
    }
}

/// Two workers linked over 127.0.0.1, for tests of what travels over a link. Each item is a
/// worker and the device it linked with.
#[cfg(test)]
pub(crate) fn linked_pair(a: Arc<DeviceKey>, b: Arc<DeviceKey>) -> ([(LinkWorker, PublicKey); 2], Arc<Progress>) {
    let progress = Arc::new(Progress::default());
    let pair = spawn_pair(a, b, progress.clone()).map(|(worker, _)| {
        let peer = wait_for(&worker, "the loopback link", |event| match event {
            FromWorker::Linked { peer_device, .. } => Some(peer_device),
            _ => None,
        });
        (worker, peer)
    });
    (pair, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_work_core::connect::CALL_DATAGRAM;

    #[test]
    fn a_failure_reads_in_plain_words_and_a_version_mismatch_is_never_an_attacker() {
        let refusals = [
            Refusal::Format("unexpected message"), Refusal::Version, Refusal::Gate, Refusal::Signature, Refusal::Commitment,
            Refusal::KeyChanged, Refusal::SelfLink, Refusal::Candidates, Refusal::Crypto("os error 5".into()),
        ];
        let failures = refusals.iter().cloned().map(Failure::Refused).chain([Failure::Tls("record_overflow(22)".into()), Failure::Stream("Timeout".into())]);
        for failure in failures {
            let text = describe(&failure);
            for bad in ['(', '{', ')', '}', '"', ':'] {
                assert!(!text.contains(bad), "{failure:?} shows debug text ({bad:?}): {text}");
            }
            for name in ["Format", "Gate", "Signature", "Commitment", "KeyChanged", "SelfLink", "Candidates", "Crypto", "Timeout", "record_overflow", "os error"] {
                assert!(!text.contains(name), "{failure:?} leaks {name}: {text}");
            }
        }
        for refusal in [Refusal::Version, Refusal::Format("unexpected message"), Refusal::Format("version not offered")] {
            let text = describe(&Failure::Refused(refusal.clone()));
            assert_eq!(text, VERSION_TEXT, "{refusal:?} is a version problem");
            assert!(!text.contains("between you"), "{refusal:?} must not warn of an attacker");
        }
        for refusal in [Refusal::Gate, Refusal::Signature, Refusal::Commitment] {
            assert!(describe(&Failure::Refused(refusal.clone())).contains("someone may be between you"), "{refusal:?} keeps the warning");
        }
    }

    #[test]
    fn invite_words_copied_as_shown_decode_and_the_unused_invite_can_be_restarted() {
        let invite = Invite { nat: Hint::Easy, addr: "192.0.2.1:4242".parse().unwrap(), lan: None, commit: 17 };
        let mut view = ConnectView { open: true, ..ConnectView::default() };
        view.apply(&FromWorker::Invite { words: invite.to_words(), note: None, lan: false });
        let words = view.own_words.as_deref().unwrap();
        assert!(words.contains(" · "), "the displayed words keep their spoken groups");
        let (decoded, corrections) = Invite::from_words(words).unwrap();
        assert_eq!(decoded, invite, "the words, selected and copied as shown, decode");
        assert!(corrections.is_empty());
        assert_eq!(view.words_privacy(), "These words hold only your internet address.");
        assert_eq!(view.restart_for_lan_change(true, ""), None, "finding the address keeps the current start");
        view.apply(&FromWorker::Phase(Status::WaitingForWords));
        for on in [true, false] {
            assert_eq!(view.restart_for_lan_change(on, ""), Some(on), "start the next invite with the changed setting");
        }
        assert_eq!(view.restart_for_lan_change(true, "one word"), None, "even a partial draft keeps this invite");
        view.peer_words_submitted = true;
        assert_eq!(view.restart_for_lan_change(true, ""), None, "clearing the field after submitting cannot restart the worker");
        view.peer_words_submitted = false;
        view.apply(&FromWorker::Phase(Status::Reaching { secs: 0 }));
        assert_eq!(view.restart_for_lan_change(false, ""), None, "reaching keeps its worker");
        view.apply(&FromWorker::Linked { peer_device: [4; 65], peer_name: "Peer".into(), version: 1,
            own_words: "one".into(), peer_words: "two".into(), reconnected: false });
        assert_eq!(view.restart_for_lan_change(true, ""), None, "a linked device keeps its worker");
    }

    /// A queued command wakes a worker asleep in its socket read: the wake reaches the socket's
    /// own address (loopback for an every-address bind), is known by its bytes and source, and is
    /// sent once per burst. The reads have no timeout: a broken wake hangs, for the runner to catch.
    #[test]
    fn a_queued_command_wakes_the_worker_at_its_own_address_once_per_burst() {
        for bind in ["127.0.0.1:0", "0.0.0.0:0"] {
            let socket = UdpSocket::bind(bind).unwrap();
            socket.set_read_timeout(None).unwrap();
            let (queue_in, queue) = mpsc::sync_channel(4);
            let commands = Commands { queue, waker: Arc::new(Waker::default()) };
            let waker = &commands.waker;
            waker.wake();
            assert!(!waker.pending.load(Ordering::SeqCst), "{bind}: a worker with no socket yet has nothing to wake");
            waker.serve(&socket);
            for _ in 0..3 {
                queue_in.try_send(ToWorker::Stop).unwrap();
                waker.wake();
            }
            let mut buf = [0u8; 64];
            let (n, from) = socket.recv_from(&mut buf).unwrap();
            assert_eq!(route(&buf[..n], from, None, waker), Route::Wake, "{bind}: the reader knows its own wake");
            assert_ne!(route(&buf[..n], "192.0.2.1:9".parse().unwrap(), None, waker), Route::Wake,
                "{bind}: the same bytes from another source are not a wake");
            socket.set_nonblocking(true).unwrap();
            assert!(socket.recv_from(&mut buf).is_err(), "{bind}: three commands in one burst send one wake");
            socket.set_nonblocking(false).unwrap();
            // Draining the queue spends the wake, so the next command wakes the worker again.
            while commands.try_recv().is_ok() {}
            queue_in.try_send(ToWorker::Stop).unwrap();
            waker.wake();
            let (n, from) = socket.recv_from(&mut buf).unwrap();
            assert!(waker.is_wake(&buf[..n], from), "{bind}: a command after the drain sends the next wake");
        }
    }

    #[test]
    fn only_the_attached_peers_media_goes_to_the_call() {
        let peer = "192.0.2.1:4000".parse().unwrap();
        let other = "198.51.100.2:5000".parse().unwrap();
        let mut punch = [0; makepad_network::p2p::punch::LEN];
        punch[0] = makepad_network::p2p::punch::KIND;
        for (packet, from, attached, expected, why) in [
            (&punch[..], other, Some(peer), Route::Link, "the link handles punches from new sources"),
            (&[0xE1], peer, None, Route::Link, "stream datagrams go to the link"),
            (&[CALL_DATAGRAM], peer, Some(peer), Route::Call, "media from the current peer goes to the call"),
            (&[CALL_DATAGRAM], other, Some(peer), Route::Drop, "media from another address is dropped"),
            (&[CALL_DATAGRAM], peer, None, Route::Drop, "media without a call is dropped"),
            (&[0xFF], peer, Some(peer), Route::Drop, "other datagrams are dropped"),
            (&[], peer, Some(peer), Route::Drop, "empty datagrams are dropped"),
        ] {
            assert_eq!(route(packet, from, attached, &Waker::default()), expected, "{why}");
        }
    }

    #[test]
    fn full_call_channels_keep_control_in_order_and_count_lost_media() {
        let (messages_in, messages) = mpsc::sync_channel(1);
        let (datagrams_in, datagrams) = mpsc::sync_channel(1);
        let sent_bytes = Arc::new(AtomicU64::new(0));
        let mut call = CallTraffic::default();
        call.pending.extend([vec![1], vec![2]]);
        call.port = Some(MediaPort {
            datagrams_in, datagrams_out: mpsc::sync_channel(1).1,
            messages_in, messages_out: mpsc::sync_channel(1).1,
            rate_cap: Arc::new(AtomicU64::new(0)), sent_bytes: sent_bytes.clone(),
        });
        assert_eq!(call.max_sleep(0), 50, "an attached call with nothing moving wakes the worker every 50 ms");
        let peer = "192.0.2.1:4000".parse().unwrap();
        call.datagram(peer, &[CALL_DATAGRAM, 1], 0);
        call.datagram(peer, &[CALL_DATAGRAM, 2], 0);
        assert_eq!(call.max_sleep(0), 5, "moving call traffic wakes the worker every 5 ms");
        assert_eq!(datagrams.try_recv().unwrap(), (peer, vec![CALL_DATAGRAM, 1]), "inbound media preserves source and bytes");
        assert_eq!(call.dropped, 1, "the full media channel counts the loss");
        call.deliver(10, 0);
        assert_eq!(messages.try_recv().unwrap(), [1], "the oldest control message is delivered first");
        call.pending.push_back(vec![3]);
        call.deliver(20, 5);
        assert_eq!(messages.try_recv().unwrap(), [2], "the retained message precedes newer traffic");
        call.deliver(30, 10);
        assert_eq!(messages.try_recv().unwrap(), [3], "the last queued message also arrives");
        assert_eq!(sent_bytes.load(Ordering::Relaxed), 30, "the attached call sees current stream traffic");
        assert_eq!(call.max_sleep(2_000), 50, "a second after the last traffic the worker slows down");
        drop(messages);
        call.pending.push_back(vec![4]);
        call.deliver(40, 15);
        assert!(call.port.is_none(), "a call side that dropped its channels is detached");
        assert_eq!(call.pending, [vec![4]], "a message for a detached call stays queued");
        assert_eq!(call.max_sleep(2_000), 1_000, "with no call attached the worker sleeps as before");
    }

    /// The call's ends of a port: what a call side would hold.
    struct CallEnds {
        datagrams_in: Receiver<(SocketAddr, Vec<u8>)>,
        datagrams_out: SyncSender<Vec<u8>>,
        messages_in: Receiver<Vec<u8>>,
        messages_out: SyncSender<Vec<u8>>,
    }

    fn media_port() -> (MediaPort, CallEnds) {
        let (datagrams_in, datagrams_in_rx) = mpsc::sync_channel(16);
        let (datagrams_out_tx, datagrams_out) = mpsc::sync_channel(16);
        let (messages_in, messages_in_rx) = mpsc::sync_channel(16);
        let (messages_out_tx, messages_out) = mpsc::sync_channel(16);
        let port = MediaPort { datagrams_in, datagrams_out, messages_in, messages_out,
            rate_cap: Arc::new(AtomicU64::new(0)), sent_bytes: Arc::new(AtomicU64::new(0)) };
        (port, CallEnds { datagrams_in: datagrams_in_rx, datagrams_out: datagrams_out_tx, messages_in: messages_in_rx, messages_out: messages_out_tx })
    }

    fn next_sync(worker: &LinkWorker) -> Vec<u8> {
        wait_for(worker, "a sync message", |event| match event {
            FromWorker::Message(message) => Some(message),
            _ => None,
        })
    }

    #[test]
    fn two_workers_link_over_loopback_and_carry_sync_and_call_traffic() {
        let keys = [(); 2].map(|_| Arc::new(DeviceKey::generate().unwrap()));
        let ([(a, a_peer), (b, b_peer)], _) = linked_pair(keys[0].clone(), keys[1].clone());
        assert_eq!((a_peer, b_peer), (keys[1].public(), keys[0].public()), "each side links with the other's device");
        assert!(a.try_send(ToWorker::Send(b"sync".to_vec())).is_ok());
        assert_eq!(next_sync(&b), b"sync", "a sync message crosses");
        let ((port_a, ends_a), (port_b, ends_b)) = (media_port(), media_port());
        assert!(a.try_send(ToWorker::Call(port_a)).is_ok() && b.try_send(ToWorker::Call(port_b)).is_ok());
        let wait = Duration::from_secs(10);
        ends_a.messages_out.send(b"ring".to_vec()).unwrap();
        assert_eq!(ends_b.messages_in.recv_timeout(wait).unwrap(), b"ring", "a call message crosses as its own kind");
        // B's port is attached once it has delivered the ring, so media from A now has somewhere to go.
        ends_a.datagrams_out.send(vec![CALL_DATAGRAM, 7]).unwrap();
        let (_, media) = ends_b.datagrams_in.recv_timeout(wait).unwrap();
        assert_eq!(media, [CALL_DATAGRAM, 7], "call media crosses on the link's socket");
        ends_b.messages_out.send(b"answer".to_vec()).unwrap();
        assert_eq!(ends_a.messages_in.recv_timeout(wait).unwrap(), b"answer", "and back the other way");
        assert!(b.try_send(ToWorker::Send(b"after".to_vec())).is_ok());
        assert_eq!(next_sync(&a), b"after", "sync still flows beside a call, and no call message reached it");
    }
    /// The path a worker told the UI about the device it linked with.
    fn recorded_path(worker: &LinkWorker) -> PathMemory {
        wait_for(worker, "the path of the link", |event| match event {
            FromWorker::Path { path, .. } => Some(path),
            _ => None,
        })
    }

    #[test]
    fn a_stopped_device_is_reached_again_with_no_words_and_a_stranger_is_not_answered() {
        let keys = [(); 3].map(|_| Arc::new(DeviceKey::generate().unwrap()));
        let [(wa, addr_a), (wb, addr_b)] = spawn_pair(keys[0].clone(), keys[1].clone(), Arc::default());
        for (worker, peer, reconnected) in [(&wa, keys[1].public(), false), (&wb, keys[0].public(), false)] {
            let linked = wait_for(worker, "the first link", |event| match event {
                FromWorker::Linked { peer_device, reconnected, .. } => Some((peer_device, reconnected)),
                _ => None,
            });
            assert_eq!(linked, (peer, reconnected), "a first contact links with the other device, by words");
        }
        let (path_a, path_b) = (recorded_path(&wa), recorded_path(&wb));
        assert_eq!((path_a.lan, path_b.lan), (Some(addr_b), Some(addr_a)), "each side records the peer's LAN address it saw, over the LAN path");
        assert_eq!((path_a.addr, path_a.port, path_b.port), (None, Some(addr_a.port()), Some(addr_b.port())), "and its own port, and no public address");

        // B stops for good. A sees the close at once and goes back to reaching it.
        assert!(wb.try_send(ToWorker::Stop).is_ok());
        wait_for(&wa, "the drop", |event| matches!(event, FromWorker::Lost).then_some(()));
        let reaching = wait_for(&wa, "reaching again", |event| match event {
            FromWorker::Reconnecting { device, .. } => Some(device),
            _ => None,
        });
        assert_eq!(reaching, keys[1].public(), "a link that drops goes back to reconnecting, for the same device");
        let mut view = ConnectView::default();
        for event in [FromWorker::Invite { words: vec!["evolved"], note: None, lan: true }, FromWorker::Refused("No.".into()),
            FromWorker::Reconnecting { device: reaching, since: 1 }] {
            view.apply(&event);
        }
        assert!(view.own_words.is_none() && !view.lan_in_words, "reaching a known device shows no words form");
        assert_eq!((view.reaching(), view.open), (Some((&reaching, Some("No."))), true), "it shows who is being reached, and a refusal stays between tries");

        // A device with another key gets no answer; the pair's own tag does.
        let probe = |tag: [u8; 8]| {
            let mut punch = makepad_network::p2p::punch::Punch::new(makepad_network::p2p::punch::Config::new(tag), SocketAddr::V4(addr_a), [7; 8], 0);
            punch.poll(0).0.pop().expect("a probe").1
        };
        let hour = connect::probe_hour(wall_secs());
        let outsider = UdpSocket::bind("127.0.0.1:0").unwrap();
        outsider.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut buf = [0u8; 64];
        let stranger_tag = connect::probe_tag(&keys[2].link_secret(&keys[0].public()).unwrap(), hour);
        outsider.send_to(&probe(stranger_tag), addr_a).unwrap();
        assert!(outsider.recv_from(&mut buf).is_err(), "a probe under a third device's tag gets no answer");
        let pair_tag = connect::probe_tag(&keys[0].link_secret(&keys[1].public()).unwrap(), hour);
        outsider.send_to(&probe(pair_tag), addr_a).unwrap();
        assert!(outsider.recv_from(&mut buf).is_ok_and(|(n, _)| n == 18 && buf[1] == 2), "the pair's tag is acknowledged, so the silence above means something");

        // B comes back as a reconnect worker from what it recorded, and both relink with no words.
        let known = Known { pin: Pin { device: keys[0].public(), min_version: 3 }, name: "Ada".into(), path: path_b };
        let wb = LinkWorker::reconnect(keys[1].clone(), known, || {}).expect("a worker thread");
        for (worker, peer) in [(&wa, keys[1].public()), (&wb, keys[0].public())] {
            let linked = wait_for(worker, "the relink", |event| match event {
                FromWorker::Linked { peer_device, reconnected, .. } => Some((peer_device, reconnected)),
                _ => None,
            });
            assert_eq!(linked, (peer, true), "the device is reached again by its key, with no words");
        }
        assert!(wa.try_send(ToWorker::Send(b"to B".to_vec())).is_ok() && wb.try_send(ToWorker::Send(b"to A".to_vec())).is_ok());
        assert_eq!(next_sync(&wb), b"to B", "a sync message crosses to the restarted device");
        assert_eq!(next_sync(&wa), b"to A", "and back");
        assert!(wb.try_send(ToWorker::Stop).is_ok() && wa.try_send(ToWorker::Stop).is_ok());
    }
}
