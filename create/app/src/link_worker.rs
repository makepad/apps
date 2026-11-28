//! The link worker owns the device socket and feeds datagrams, clock readings, and commands to
//! [`makepad_town_core::hub::Hub`]. Its slot's application channels carry calls, blobs, and sync. The UI thread
//! never waits on it: commands go in through a bounded queue with `try_send`, and what the worker
//! has to say comes back on a channel the UI drains when the worker signals it. A queued command
//! wakes a worker that sleeps in its socket read ([`Waker`]), so it leaves at once.
//!
//! Call media shares the socket and follows the current link peer, including on a LAN path.

use makepad_town_core::{connect::Kind, home::{Home, LinkPortLease}, hub::{self, Attempt, BatchEnd, Carrier, Event, Hub, Now, Output, SlotPhase, Topic}, key::{DeviceKey, PublicKey}, link::{InviteSession, Pin}, peers::PathMemory};
pub use makepad_town_core::hub::{
    BlockReason, Status, WordsRejected, BLOCKED_NETWORKS, GATE_REFUSED, SAME_NETWORK_LAN_OFF_HERE,
    SAME_NETWORK_NO_LOCAL_ADDRESS, SAME_NETWORK_PEER_LAN_OFF, VERSION_TEXT,
};
use makepad_widgets::makepad_platform::thread::{Class, TaskPool};
#[cfg(test)]
use makepad_town_core::{hub::Start, connect::{self, Connection, Failure, Phase, ProbeTags, Remembered}, link::{INVITE_LIFETIME_MS, Mode, Reconnect, Refusal}, hub::{describe, FAREWELL_MS}};
use makepad_core_util::udp::{bind_udp_on_interface, InterfaceBind};
#[cfg(test)]
use makepad_network::p2p::discover::Hint;
use makepad_network::p2p::reflect::{self, Style};
use makepad_network::p2p::words::{self, Invite};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
#[cfg(test)]
use std::net::Ipv4Addr;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub mod blobs;
mod socket;
use socket::{LinkSocket, WakeLine};
#[cfg(test)]
mod blob_tests;

/// Both invites as decoded locally. Only the worker that proved this exact pair of words
/// can carry the consent recorded when the person pressed Pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairWords {
    pub own: Invite,
    pub peer: Invite,
}

impl PairWords {
    pub fn read(own: &str, peer: &str) -> Result<Self, String> {
        Ok(Self { own: Self::decode(own)?, peer: Self::decode(peer)? })
    }

    pub fn decode(text: &str) -> Result<Invite, String> {
        Invite::from_words(text).map(|(invite, _)| invite).map_err(|e| match e {
            words::WordsError::NewerVersion { .. } => VERSION_TEXT.to_owned(),
            _ => BAD_WORDS.to_owned(),
        })
    }
}

/// What the UI asks of the worker.
pub type Request = (Option<Attempt>, ToWorker);
pub type Notice = (Option<Attempt>, FromWorker);

pub enum SendError {
    Full(Request),
    Disconnected(Request),
}

pub enum ToWorker {
    SetWanted { devices: Vec<Known>, final_sync: Vec<(Attempt, Vec<u8>)> },
    OpenWords { request: u64, same_network: bool, lookup: Receiver<io::Result<Vec<SocketAddr>>> },
    CancelWords,
    /// The words the other person read out, and the name to show them once linked.
    PeerWords { words: PairWords, name: String },
    /// One whole sync message for the peer.
    Send(Vec<u8>),
    /// Return one inbox credit to each attempt whose message the UI consumed.
    SyncRead(Vec<Attempt>),
    /// Attach the call side once the link is up. It stays attached for the link's life and owns
    /// ring and call state; the worker only carries its traffic.
    Call(MediaPort),
    /// Detach the call side before the link ends.
    EndCall,
    Stop,
}

impl ToWorker {
    fn changes_slots(&self) -> bool {
        matches!(self, Self::SetWanted { .. } | Self::OpenWords { .. } | Self::CancelWords)
    }
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

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
enum Route {
    Link,
    Call,
    /// This worker's own wake: it has already done its job by ending the read.
    Wake,
    Drop,
}

#[cfg(test)]
fn route(packet: &[u8], from: SocketAddr, call_peer: Option<SocketAddr>, waker: &Waker) -> Route {
    if waker.is_wake(packet, from) { return Route::Wake }
    match hub::route(packet, from, call_peer) {
        hub::Datagram::Link => Route::Link,
        hub::Datagram::Call => Route::Call,
        _ => Route::Drop,
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

    fn rate_cap(&self) -> Option<u64> {
        self.port.as_ref().map(|port| port.rate_cap.load(Ordering::Relaxed)).filter(|&bps| bps > 0)
    }

    fn outgoing(&mut self, hub: &mut Hub, at: Attempt, mut send: impl FnMut(SocketAddr, &[u8]), now: u64) {
        let mut moved = false;
        if let (Some(port), Carrier::Peer(peer)) = (&self.port, at.carrier) {
            for message in port.messages_out.try_iter() {
                // The call controller fragments its frames to MAX_CALL before handoff.
                if hub.send_peer(peer, Topic::Call, message).is_err() {
                    makepad_widgets::log!("Town call: invalid local signalling frame");
                }
                moved = true;
            }
            if let Some(peer) = hub.media_address(at) {
                for bytes in port.datagrams_out.try_iter() {
                    send(peer, &bytes);
                    moved = true;
                }
            }
        }
        if moved {
            self.active_until = now.saturating_add(1_000);
        }
    }

    fn deliver(&mut self, sent_bytes: u64, now: u64) -> bool {
        let mut delivered = false;
        let mut gone = false;
        if let Some(port) = &self.port {
            port.sent_bytes.store(sent_bytes, Ordering::Relaxed);
            while let Some(message) = self.pending.pop_front() {
                match port.messages_in.try_send(message) {
                    Ok(()) => {
                        delivered = true;
                        self.active_until = now.saturating_add(1_000);
                    }
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
            makepad_widgets::log!("Town call: dropped {} incoming media datagrams on a full channel", self.dropped);
            self.reported = self.dropped;
            self.next_report = now.saturating_add(1_000);
        }
        delivered
    }

    fn datagram(&mut self, from: SocketAddr, bytes: impl Into<Vec<u8>>, now: u64) {
        if let Some(port) = &self.port {
            self.active_until = now.saturating_add(1_000);
            if let Err(TrySendError::Full(_)) = port.datagrams_in.try_send((from, bytes.into())) {
                self.dropped += 1;
            }
        }
    }
}

/// What the worker tells the UI.
#[derive(Debug)]
pub enum FromWorker {
    /// The UI request and (in the notice envelope) the generation it opened.
    WordsOpened(u64),
    WordsRejected(WordsRejected),
    Idle(u64),
    BlobWorker(blobs::Endpoint),
    BlobStatus,
    /// One new sync batch may be submitted on this proved attempt.
    Writable,
    Invite { words: Vec<&'static str> },
    /// This attempt can no longer serve requests or complete a proof.
    WordsChanged,
    /// The system refuses the path; the worker keeps trying, in case the person allows it.
    Blocked(&'static str),
    /// The link moved on.
    Phase(Status),
    /// The peer proved its key.
    /// A first contact carries the two invites used in the proof. Reconnects carry none.
    Linked { peer_device: PublicKey, version: u16, peer_name: String, contact: Option<PairWords> },
    /// Where the linked device is on the path that carries the link, from what the worker saw.
    Path { peer_device: PublicKey, path: PathMemory },
    /// The link dropped. The worker goes on reaching the device, and says so with `Reconnecting`.
    Lost,
    /// The worker is reaching a known device with no words, since this wall time (unix seconds).
    Reconnecting { device: PublicKey, since: u64 },
    /// The device refused the gate; the worker keeps trying slowly. The line is plain words.
    PeerVersion(u16),
    Refused(String),
    /// The device proved the gate but not its pinned key. The worker stopped reconnecting to it.
    KeyChanged,
    /// One whole sync message from the peer.
    Message(Vec<u8>),
    /// The worker stopped: no address, refused words, a failed link. It says why.
    Failed(String),
}

#[cfg(test)]
const KEEP_MAPPING: Duration = Duration::from_secs(20);

/// The wake datagram's bytes. The worker also checks where it came from.
const WAKE: &[u8] = b"work wake\0";

/// Wakes a worker that sleeps in its socket read when the UI queues a command: one small tagged
/// datagram to the socket's own bound address, from a socket of the waker's own, so the reader
/// knows it by its bytes and its source and drops it. One wake per burst: while one is on its
/// way, later commands send none. The worker spends it as it starts to drain the queue, so a
/// command queued after that sends the next one.
#[derive(Default)]
struct Waker {
    line: Mutex<Option<Arc<WakeLine>>>,
    home: OnceLock<std::sync::Weak<Home>>,
    worker: OnceLock<std::thread::Thread>,
    stopped: AtomicBool,
    pending: AtomicBool,
}

impl Waker {
    /// The worker has bound `socket`: wakes go to it from now on. A waker that can't be made is
    /// logged, and the worker still takes its commands each time it wakes on its own.
    fn serve(&self, socket: &impl LinkSocket) {
        match socket.wake_line() {
            Ok(line) => {
                let old = self.line.lock().unwrap().replace(Arc::new(line));
                drop(old);
                self.pending.store(false, Ordering::SeqCst);
            }
            Err(e) => makepad_widgets::log!("town: the link worker can't be woken, so commands wait for its next wake-up: {e}"),
        }
    }

    /// From the UI thread, after it queued a command. One non-blocking send per burst.
    fn wake(&self) {
        if let Some(thread) = self.worker.get() { thread.unpark(); }
        let Some(line) = self.line.lock().unwrap().clone() else { return };
        if self.pending.swap(true, Ordering::SeqCst) {
            return;
        }
        if line.wake().is_err() {
            self.pending.store(false, Ordering::SeqCst);
        }
    }

    fn cancel_wait(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(home) = self.home.get().and_then(std::sync::Weak::upgrade) { home.wake_port_waiters(); }
    }

    fn is_wake(&self, packet: &[u8], from: SocketAddr) -> bool {
        packet == WAKE && self.line.lock().unwrap().as_ref().is_some_and(|line| line.from == from)
    }
}

/// The worker's end of the UI's commands: the queue, and the waker that gets it read.
struct Commands {
    signal: Box<dyn Fn() + Send>,
    queue: Receiver<Request>,
    config: Cell<u64>,
    waker: Arc<Waker>,
    phases: RefCell<SlotLogs>,
}

/// One log line per phase change, with the time the previous phase took: never per packet.
#[derive(Default)]
struct PhaseLog { last: Option<(&'static str, Instant)> }

impl PhaseLog {
    /// The line for entering `phase`, or `None` while the phase stays the same.
    fn enter(&mut self, phase: &'static str, now: Instant) -> Option<String> {
        let line = match self.last {
            Some((prev, _)) if prev == phase => return None,
            Some((prev, since)) => format!("Town link: {phase} ({:.1} s after {prev})", now.duration_since(since).as_secs_f64()),
            None => format!("Town link: {phase}"),
        };
        self.last = Some((phase, now));
        Some(line)
    }
}

#[derive(Default)]
struct SlotLogs {
    selected: Option<Carrier>,
    logs: std::collections::BTreeMap<Carrier, PhaseLog>,
}

impl SlotLogs {
    fn select(&mut self, carrier: Carrier, promoted: bool) {
        if promoted {
            if let Some(words) = self.logs.remove(&Carrier::Words) { self.logs.insert(carrier, words); }
        }
        self.selected = Some(carrier);
    }
    fn enter(&mut self, phase: &'static str, now: Instant) -> Option<String> {
        let key = self.selected.unwrap_or(Carrier::Words);
        let line = self.logs.entry(key).or_default().enter(phase, now)?;
        let name = match key {
            Carrier::Words => "words".to_string(),
            Carrier::Peer(peer) => makepad_town_core::hex(&peer[..4]),
        };
        Some(format!("{line} [{name}]"))
    }
}

/// The phase an event to the UI enters, if it is one. Traffic and progress counters are none.
fn phase_of(event: &FromWorker) -> Option<&'static str> {
    Some(match event {
        FromWorker::Phase(Status::FindingAddress) => "finding our address",
        FromWorker::Invite { .. } => "words ready",
        FromWorker::Phase(Status::WaitingForWords) => "waiting for the other words",
        FromWorker::Phase(Status::Reaching { .. }) => "dialing",
        FromWorker::Phase(Status::Reconnecting) => "reconnecting",
        FromWorker::Phase(Status::Backoff { .. }) => "waiting to retry",
        FromWorker::Phase(Status::Stopped) => "stopped",
        FromWorker::Phase(Status::Securing) => "stream up, securing",
        FromWorker::Phase(Status::Proving) => "proving keys",
        FromWorker::Phase(Status::Linked) | FromWorker::Linked { .. } => "verified and linked",
        FromWorker::Lost => "lost",
        FromWorker::WordsChanged => "words changed",
        FromWorker::Blocked(_) => "path blocked",
        FromWorker::Refused(_) => "refused by the other device",
        FromWorker::KeyChanged => "the other device's key changed",
        FromWorker::Failed(_) => "stopped",
        _ => return None,
    })
}

impl Commands {
    /// The next command, without waiting. The wake sent for it, if any, is spent here.
    fn try_recv(&self, hub: &Hub) -> Result<Request, mpsc::TryRecvError> {
        self.waker.pending.store(false, Ordering::SeqCst);
        loop {
            let (at, command) = self.queue.try_recv()?;
            if matches!(command, ToWorker::PeerWords { .. }) || at.is_none_or(|at| hub.attempt(at.carrier) == Some(at)) {
                if command.changes_slots() { self.config.set(self.config.get().wrapping_add(1)); }
                return Ok((at, command));
            }
        }
    }
}

pub struct LinkWorker {
    to: SyncSender<Request>,
    config: Cell<u64>,
    from: Receiver<Notice>,
    waker: Arc<Waker>,
    #[cfg(test)]
    observed: Cell<Option<Attempt>>,
}

impl LinkWorker {
    pub fn new(home: Arc<Home>, device: Arc<DeviceKey>, signal: impl Fn() + Send + 'static) -> io::Result<Self> {
        Self::spawn(signal, move |commands, tell| {
            let lan_allowed = makepad_network::lan::allowed();
            run(&home, device.public(), Hub::with_lan(device, lan_allowed), commands, tell, LinkClock::Real(Instant::now()), bind_link)
        })
    }

    pub fn open_words(&self, pool: TaskPool, request: u64, same_network: bool) -> Result<ToWorker, String> {
        let (send, lookup) = mpsc::channel();
        let wake = self.waker.clone();
        pool.submit_named(Class::Background, "Town address reflectors", move || {
            let _ = send.send(reflect::resolve(&reflect::DEFAULT_SERVERS));
            wake.wake();
        }).map_err(|e| e.to_string())?.detach();
        Ok(ToWorker::OpenWords { request, same_network, lookup })
    }

    /// The worker thread: `body` gets the command queue and the way to tell the UI thread.
    fn spawn(
        signal: impl Fn() + Send + 'static,
        body: impl FnOnce(&Commands, &dyn Fn(Option<Attempt>, FromWorker)) -> Result<(), String> + Send + 'static,
    ) -> io::Result<LinkWorker> {
        let (to, queue) = mpsc::sync_channel(256);
        let waker = Arc::new(Waker::default());
        let commands = Commands { config: Cell::new(0), signal: Box::new(signal), queue, waker: waker.clone(), phases: RefCell::default() };
        let (events, from) = mpsc::channel();
        std::thread::Builder::new().name("create link".into()).spawn(move || {
            let tell = |at: Option<Attempt>, event: FromWorker| {
                if let Some(line) = phase_of(&event).and_then(|phase| commands.phases.borrow_mut().enter(phase, Instant::now())) {
                    match &event {
                        FromWorker::Failed(why) | FromWorker::Refused(why) => makepad_widgets::log!("{line}: {}", why.replace('\n', " ")),
                        FromWorker::Blocked(why) => makepad_widgets::log!("{line}: {}", why.replace('\n', " ")),
                        _ => makepad_widgets::log!("{line}"),
                    }
                }
                let _ = events.send((at, event));
                (commands.signal)();
            };
            use std::panic::{catch_unwind, AssertUnwindSafe};
            let failure = match catch_unwind(AssertUnwindSafe(|| body(&commands, &tell))) {
                Ok(Ok(())) => None,
                Ok(Err(why)) => Some((why, None)),
                Err(payload) => Some(("the link worker stopped unexpectedly".into(), Some(payload))),
            };
            if let Some((why, panic)) = failure {
                // The signal or the logger itself may be what panicked. Deliver the failure first.
                let _ = events.send((None, FromWorker::Failed(why.clone())));
                let _ = catch_unwind(AssertUnwindSafe(|| (commands.signal)()));
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    if let Some(payload) = panic {
                        let msg = payload.downcast_ref::<&str>().copied()
                            .or_else(|| payload.downcast_ref::<String>().map(String::as_str)).unwrap_or("unknown panic");
                        makepad_widgets::log!("Town link: worker panicked: {msg}");
                    }
                    if let Some(line) = commands.phases.borrow_mut().enter("stopped", Instant::now()) {
                        makepad_widgets::log!("{line}: {}", why.replace('\n', " "));
                    }
                }));
            }
            // Once notices close, every send reports Disconnected, so both signs of worker exit agree.
            drop(commands);
            drop(events);
        })?;
        Ok(LinkWorker { to, from, config: Cell::new(0), waker, #[cfg(test)] observed: Cell::new(None) })
    }

    /// Queue and wake without waiting. Only a full queue can be retried on a later frame.
    pub fn send(&self, request: Request) -> Result<(), SendError> {
        let stop = matches!(request.1, ToWorker::Stop);
        let config = request.1.changes_slots();
        match self.to.try_send(request) {
            Ok(()) => {
                if config { self.config.set(self.config.get().wrapping_add(1)); }
                if stop { self.waker.cancel_wait(); }
                self.waker.wake();
                Ok(())
            }
            Err(TrySendError::Full(request)) => Err(SendError::Full(request)),
            Err(TrySendError::Disconnected(request)) => Err(SendError::Disconnected(request)),
        }
    }

    pub fn configuration(&self) -> u64 { self.config.get() }

    pub fn next_event(&self) -> Result<Notice, mpsc::TryRecvError> { self.from.try_recv() }

    #[cfg(test)]
    pub(crate) fn try_send(&self, command: ToWorker) -> Result<(), ToWorker> {
        let at = if matches!(command, ToWorker::Stop) { None } else { self.observed.get() };
        self.send((at, command)).map_err(|err| match err {
            SendError::Full((_, c)) | SendError::Disconnected((_, c)) => c,
        })
    }

    #[cfg(test)]
    pub(crate) fn try_recv(&self) -> Option<FromWorker> {
        self.next_event().ok().map(|notice| self.observe(notice))
    }

    #[cfg(test)]
    fn observe(&self, (at, event): Notice) -> FromWorker {
        if let Some(at) = at {
            self.observed.set(Some(at));
            if matches!(event, FromWorker::Message(_)) {
                self.to.send((None, ToWorker::SyncRead(vec![at]))).unwrap();
                self.waker.wake();
            }
        }
        event
    }

}

impl Drop for LinkWorker {
    fn drop(&mut self) {
        // The worker sees the queue close on its next wake, or this when there is room.
        self.waker.cancel_wait();
        let _ = self.to.try_send((None, ToWorker::Stop));
        self.waker.wake();
    }
}

/// What a worker needs to reach a device again: the pin, and where it was seen last. Plain data
/// from the device's record, so the worker shares nothing with the UI thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub pin: Pin,
    pub name: String,
    pub path: PathMemory,
}

/// A device that proved the gate but not its pinned key. Reconnecting stops.
pub const KEY_CHANGED: &str = "This isn't the device you linked with. Someone may be between you, or they set up a new device. \
    Link again with new words if it is them.";

fn wall_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The worker's fixed surroundings: the socket and clock it runs on, and how it talks.
struct Env<'a, S: LinkSocket> {
    socket: &'a S,
    clock: LinkClock,
    #[cfg(test)]
    device: Option<&'a Arc<DeviceKey>>,
    commands: &'a Commands,
    tell: &'a dyn Fn(Option<Attempt>, FromWorker),
}

enum LinkClock {
    Real(Instant),
    #[cfg(test)]
    Pair { progress: Arc<Progress>, seat: usize },
}

impl<S: LinkSocket> Env<'_, S> {
    fn now(&self) -> u64 {
        match &self.clock {
            LinkClock::Real(start) => start.elapsed().as_millis() as u64,
            #[cfg(test)]
            LinkClock::Pair { progress, .. } => progress.link_time(),
        }
    }

    fn receive(&self, buf: &mut [u8], wait: Duration) -> io::Result<(usize, SocketAddr)> {
        let packet = self.socket.recv_from_within(buf, wait);
        #[cfg(test)]
        if let LinkClock::Pair { progress, seat } = &self.clock {
            // Account for one completed I/O turn. Time spent descheduled counts for nothing.
            let elapsed = if packet.as_ref().is_err_and(|e| timed_out(e)) { wait.as_millis() as u64 } else { 1 };
            progress.advance(*seat, elapsed);
        }
        packet
    }
}

fn timed_out(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// The channel ends for one slot's current authenticated generation.
#[derive(Default)]
struct Pump {
    sync_credit: bool,
    blocked: bool,
    peer_newest: Option<u16>,
    call: CallTraffic,
}

fn worker_blobs(me: PublicKey, commands: &Commands, tell: &dyn Fn(Option<Attempt>, FromWorker)) -> blobs::BlobPump {
    let wake = commands.waker.clone();
    let (blobs, endpoint) = blobs::BlobPump::new(me, makepad_town_core::blob::CHUNK, move || wake.wake());
    tell(None, FromWorker::BlobWorker(endpoint));
    blobs
}

type Resolver = Receiver<io::Result<Vec<SocketAddr>>>;

#[cfg(test)]
fn reconnect<S: LinkSocket>(
    home: &Arc<Home>,
    device: &Arc<DeviceKey>,
    known: Known,
    commands: &Commands,
    tell: &dyn Fn(Option<Attempt>, FromWorker),
    clock: LinkClock,
    bind: impl Fn(u16) -> io::Result<S>,
) -> Result<(), String> {
    let path = connect_path(known.path);
    let mut hub = Hub::new(device.clone());
    hub.set_wanted([(known.pin, path)], Now { ms: 0, unix_secs: wall_secs() });
    run(home, device.public(), hub, commands, tell, clock, bind)
}

fn connect_path(path: PathMemory) -> makepad_town_core::connect::Remembered {
    makepad_town_core::connect::Remembered { public: path.addr, lan: path.lan }
}

fn bind_port<S: LinkSocket>(lease: &LinkPortLease, bind: &impl Fn(u16) -> io::Result<S>) -> io::Result<S> {
    let saved = lease.port()?;
    let (socket, replaced) = match bind(saved.unwrap_or(0)) {
        Err(e) if saved.is_some() && e.kind() == io::ErrorKind::AddrInUse => (bind(0)?, true),
        result => (result?, false),
    };
    let port = std::num::NonZeroU16::new(socket.local_addr()?.port())
        .ok_or_else(|| io::Error::other("the bound socket has no port"))?;
    if Some(port.get()) != saved {
        lease.remember(port)?;
    }
    if replaced {
        makepad_widgets::log!("Town link: another program holds the saved port. Now using {port}.");
    }
    Ok(socket)
}

fn run<S: LinkSocket>(
    home: &Arc<Home>,
    me: PublicKey,
    mut hub: Hub,
    commands: &Commands,
    tell: &dyn Fn(Option<Attempt>, FromWorker),
    mut clock: LinkClock,
    bind: impl Fn(u16) -> io::Result<S>,
) -> Result<(), String> {
    let mut blobs = worker_blobs(me, commands, tell);
    let mut resolver = None;
    let mut words_request = None;
    let waker = &commands.waker;
    let _ = waker.worker.set(std::thread::current());
    let _ = waker.home.set(Arc::downgrade(home));
    let mut servers = Vec::new();
    loop {
        let Some(lease) = home.lease_link_port(&waker.stopped) else { return Ok(()) };
        let mut last_error = None;
        let socket = loop {
            if waker.stopped.load(Ordering::Acquire) {
                return Ok(());
            }
            match bind_port(&lease, &bind) {
                Ok(socket) => break socket,
                Err(e) => {
                    if last_error != Some(e.kind()) {
                        makepad_widgets::log!("Town link: binding the device port: {e}. Retrying.");
                    }
                    last_error = Some(e.kind());
                    std::thread::park_timeout(Duration::from_millis(hub::RETRY_MS));
                }
            }
        };
        waker.serve(&socket);
        let env = Env {
            socket: &socket,
            clock,
            #[cfg(test)]
            device: None,
            commands,
            tell,
        };
        if let Some(at) = hub.attempt(Carrier::Words).filter(|_| hub.reflecting() && !servers.is_empty()) {
            if let Err(why) = begin_reflection(&mut hub, at, &servers, env.now()) { hub.fail_words(at, why); }
        }
        hub.rebound(env.now());
        match drive(&env, &mut hub, &mut blobs, &mut resolver, &mut servers, &mut words_request)? {
            None => return Ok(()),
            Some(error) => makepad_widgets::log!("Town link: receive failed ({error}). Rebinding the device port."),
        }
        clock = env.clock;
        *waker.line.lock().unwrap() = None;
        drop(socket);
        drop(lease);
    }
}

fn begin_reflection(hub: &mut Hub, at: Attempt, servers: &[SocketAddr], now: u64) -> Result<(), String> {
    let mut error = None;
    let query = reflect::Query::new(
        servers,
        Style::Plain,
        &mut |bytes| {
            if let Err(e) = makepad_network::tls::os_random(bytes) {
                error = Some(e);
            }
        },
        now,
    );
    if let Some(e) = error {
        return Err(e.to_string());
    }
    hub.reflect(at, query, now);
    Ok(())
}

fn transient(e: &io::Error) -> bool {
    timed_out(e)
        || local_network_off(e)
        || matches!(
            e.kind(),
            io::ErrorKind::Interrupted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionRefused
                | io::ErrorKind::HostUnreachable
                | io::ErrorKind::NetworkUnreachable
                | io::ErrorKind::NetworkDown
        )
}

fn at(env: &Env<impl LinkSocket>) -> Now {
    Now { ms: env.now(), unix_secs: wall_secs() }
}

/// Yield after a batch so commands and outgoing traffic make progress even on a busy socket.
/// An overdue stream gets to inspect queued traffic before polling its idle timer.
fn read_batch<S: LinkSocket>(
    env: &Env<S>,
    buf: &mut [u8],
    mut receive: impl FnMut(SocketAddr, &[u8]),
) -> io::Result<bool> {
    let mut count = 0;
    const BATCH: usize = 32;
    env.socket.drain_queued(buf, |bytes, from| {
        receive(from, bytes);
        count += 1;
        count < BATCH
    })?;
    Ok(count == BATCH)
}

fn drive<S: LinkSocket>(
    env: &Env<S>,
    hub: &mut Hub,
    blobs: &mut blobs::BlobPump,
    resolver: &mut Option<(Attempt, Resolver)>,
    servers: &mut Vec<SocketAddr>,
    words_request: &mut Option<u64>,
) -> Result<Option<io::Error>, String> {
    let mut buf = [0u8; 2048];
    let mut pumps = BTreeMap::<Attempt, Pump>::new();
    let mut attached = BTreeSet::<Attempt>::new();
    let mut broken = None;
    let mut stopping = false;
    let mut reported_idle = true;
    loop {
        let now = at(env);
        hub.begin_batch(now);
        loop {
            match env.commands.try_recv(hub) {
                Ok((_, ToWorker::Stop)) => {
                    hub.stop(now.ms);
                    stopping = true;
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !stopping { hub.stop(now.ms); }
                    stopping = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Ok((_, ToWorker::SyncRead(attempts))) => {
                    for at in attempts { hub.sync_read(at); }
                }
                Ok((_, ToWorker::SetWanted { devices, final_sync })) => {
                    for (at, bytes) in final_sync { hub.send_sync(at, &bytes); }
                    reported_idle = false;
                    hub.set_wanted(devices.into_iter().map(|d| (d.pin, connect_path(d.path))), now);
                }
                Ok((_, ToWorker::OpenWords { request, same_network, lookup })) => {
                    reported_idle = false;
                    *words_request = Some(request);
                    let words = hub.start_words(same_network, now);
                    (env.tell)(Some(words), FromWorker::WordsOpened(request));
                    *resolver = Some((words, lookup));
                }
                Ok((_, ToWorker::CancelWords)) => {
                    reported_idle = false;
                    *words_request = None;
                    hub.cancel_words(now.ms);
                    *resolver = None;
                }
                Ok((target, ToWorker::PeerWords { words, name })) => {
                    let result = match (target, hub.attempt(Carrier::Words)) {
                        (Some(at), Some(cur)) if at != cur => Err(WordsRejected::StaleAttempt),
                        (_, Some(cur)) => hub.words(cur, words.own, words.peer, name, now.ms),
                        (_, None) => Err(WordsRejected::StaleAttempt),
                    };
                    if let Err(reason) = result {
                        makepad_widgets::log!("Town link: peer words rejected ({reason:?}) [words]");
                        (env.tell)(target, FromWorker::WordsRejected(reason));
                    }
                }
                Ok((Some(at), ToWorker::Call(port))) if hub.phase(at.carrier) == Some(SlotPhase::Up) => {
                    pumps.entry(at).or_default().call.port = Some(port);
                    attached.insert(at);
                    hub.attach_call(at, true);
                }
                Ok((Some(at), ToWorker::EndCall)) if attached.remove(&at) => {
                    if let Some(p) = pumps.get_mut(&at) { p.call.port = None; }
                    hub.attach_call(at, false);
                }
                Ok((Some(at), ToWorker::Send(bytes))) => {
                    hub.send_sync(at, &bytes);
                    if let Some(p) = pumps.get_mut(&at) { p.sync_credit = false; }
                }
                Ok((_, ToWorker::Call(_) | ToWorker::EndCall | ToWorker::Send(_))) => {}
            }
        }
        if let Some((words, rx)) = resolver.as_ref() {
            let words = *words;
            let result = match rx.try_recv() {
                Ok(answer) => Some(answer.map_err(|e| format!("couldn't find the address reflectors: {e}"))
                    .and_then(|found| {
                        *servers = found;
                        begin_reflection(hub, words, servers, now.ms)
                    })),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err("The address lookup stopped.".into())),
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                *resolver = None;
                if let Err(why) = result { hub.fail_words(words, why); }
            }
        }
        let now = env.now();
        let cap = attached.iter().filter_map(|at| pumps.get(at)?.call.rate_cap()).min();
        hub.set_call_cap(cap, now);
        for &at in &attached {
            if let Some(pump) = pumps.get_mut(&at) {
                let lan_off = !hub.lan_allowed();
                pump.call.outgoing(hub, at, |to, bytes| {
                    if lan_off && makepad_network::lan::is_local_network(to.ip()) {
                        let _ = makepad_network::lan::check("p2p send", to);
                        return;
                    }
                    let _ = env.socket.send_to(bytes, to);
                }, now);
            }
        }
        let more = if broken.is_some() {
            false
        } else {
            match read_batch(env, &mut buf, |from, bytes| {
                if !env.commands.waker.is_wake(bytes, from) {
                    hub.handle(from, bytes, now);
                }
            }) {
                Ok(more) => more,
                Err(e) if transient(&e) => false,
                Err(e) => {
                    broken = Some(e);
                    false
                }
            }
        };
        if broken.is_some() {
            hub.socket_lost(at(env));
            if let (Some(request), Some(cur)) = (*words_request, hub.attempt(Carrier::Words)) {
                (env.tell)(Some(cur), FromWorker::WordsOpened(request));
                if let Some((res_at, _)) = resolver.as_mut() { *res_at = cur; }
            }
        }
        let events = if broken.is_some() { hub.take_events() }
            else { hub.poll(at(env), if more { BatchEnd::MoreQueued } else { BatchEnd::Drained }) };
        if hub.attempt(Carrier::Words).is_none() { *words_request = None; }
        let mut changed = false;
        for Output { attempt, event } in events {
            let lost = matches!(event, Event::Lost { .. });
            let pump = pumps.entry(attempt).or_default();
            match pump.dispatch(env, hub, blobs, attempt, event, now) {
                Ok(moved) => changed |= moved,
                Err(why) if attempt.carrier == Carrier::Words => hub.fail_words(attempt, why),
                Err(why) => return Err(why),
            }
            if lost { attached.remove(&attempt); }
        }
        pumps.retain(|at, _| {
            let up = hub.attempt(at.carrier) == Some(*at) && hub.phase(at.carrier) == Some(SlotPhase::Up);
            if !up {
                blobs.detach(*at);
                attached.remove(at);
            }
            up
        });
        if !hub.has_work() {
            if stopping { return Ok(None); }
            if !reported_idle && !hub.awaiting_sync() {
                (env.tell)(None, FromWorker::Idle(env.commands.config.get()));
                reported_idle = true;
            }
        } else { reported_idle = false; }
        if broken.is_some() { return Ok(broken); }
        if blobs.pump(now) { (env.tell)(None, FromWorker::BlobStatus); }
        for (&at, pump) in &mut pumps {
            if !pump.sync_credit && hub.writable(at, Kind::Sync) {
                pump.sync_credit = true;
                (env.tell)(Some(at), FromWorker::Writable);
            }
            if hub.writable(at, Kind::Blob) {
                if let Some(bytes) = blobs.poll_send(at) {
                    hub.send_blob(at, &bytes);
                    changed = true;
                }
            }
            if attached.contains(&at) {
                if pump.call.deliver(hub.stream_bytes(), now) { (env.commands.signal)(); }
                if pump.call.port.is_none() { hub.attach_call(at, false); attached.remove(&at); }
            }
        }
        // A command or store result can wake us between the command drain and the socket
        // batch. Even if that batch already consumed its datagram, service it before sleeping.
        if changed || more || env.commands.waker.pending.load(Ordering::SeqCst) {
            continue;
        }
        let now = env.now();
        let deadline = hub.next_wake().into_iter().chain(blobs.next_wake(now)).min().unwrap_or(u64::MAX);
        if deadline == 0 { continue; }
        let max_sleep = attached.iter().filter_map(|at| pumps.get(at).map(|p| p.call.max_sleep(now))).min()
            .unwrap_or_else(|| if !pumps.is_empty() || env.commands.waker.line.lock().unwrap().is_none() { 1_000 } else { 20_000 });
        let wait = deadline.saturating_sub(now).clamp(1, max_sleep);
        match env.receive(&mut buf, Duration::from_millis(wait)) {
            Ok((n, from)) if !env.commands.waker.is_wake(&buf[..n], from) => {
                hub.begin_batch(at(env));
                hub.handle(from, &buf[..n], env.now());
            }
            Ok(_) => {}
            Err(e) if transient(&e) => {}
            Err(e) => broken = Some(e),
        }
    }
}

impl Pump {
    fn dispatch(
        &mut self,
        env: &Env<impl LinkSocket>,
        hub: &mut Hub,
        blobs: &mut blobs::BlobPump,
        attempt: Attempt,
        event: Event,
        now: u64,
    ) -> Result<bool, String> {
        let mut changed = false;
        let tell = |event| (env.tell)(Some(attempt), event);
        env.commands
            .phases
            .borrow_mut()
            .select(attempt.carrier, matches!(event, Event::Linked { words: Some(_), .. }));
        match event {
            Event::Send(to, bytes) => {
                if !hub.lan_allowed() && makepad_network::lan::is_local_network(to.ip()) {
                    let _ = makepad_network::lan::check("p2p send", to);
                    return Ok(false);
                }
                if env.socket.send_to(&bytes, to).is_err_and(|e| local_network_off(&e)) && !self.blocked {
                    self.blocked = true;
                    tell(FromWorker::Blocked(LOCAL_NETWORK_OFF));
                }
            }
            Event::Reflect(to, bytes) => {
                makepad_network::lan::check("p2p reflect", to).map_err(|e| e.to_string())?;
                let _ = env.socket.send_to(&bytes, to);
            }
            Event::OpenInvite { server, addr, hint, include_lan } => {
                let opened = env.now();
                let session = InviteSession::open(opened).map_err(|e| format!("no invite key: {e}"))?;
                let lan = if include_lan {
                    env.socket.lan_address(addr)
                        .map_err(|why| makepad_widgets::log!("Town link: no LAN address for the words ({why})"))
                        .ok()
                } else {
                    None
                };
                hub.open_invite(attempt, session, server, addr, hint, lan, opened);
                changed = true;
            }
            Event::CheckAddress { addr, include_lan } => {
                let lan = if include_lan { env.socket.lan_address(addr).ok() } else { None };
                let mut tx = [0; 12];
                makepad_network::tls::os_random(&mut tx).map_err(|e| e.to_string())?;
                hub.check_address(attempt, lan, tx);
                changed = true;
            }
            Event::Punch => {
                let mut random = [0; 72];
                makepad_network::tls::os_random(&mut random).map_err(|e| e.to_string())?;
                hub.punch(attempt, random, at(env));
                self.blocked = false;
                changed = true;
            }
            Event::Invite(invite) => tell(FromWorker::Invite { words: invite.to_words() }),
            Event::WordsChanged => tell(FromWorker::WordsChanged),
            Event::Blocked(reason) => tell(FromWorker::Blocked(reason.text())),
            Event::Phase(status) => tell(FromWorker::Phase(status)),
            Event::Note(phase) => {
                if let Some(line) = env.commands.phases.borrow_mut().enter(phase, Instant::now()) {
                    makepad_widgets::log!("{line}");
                }
            }
            Event::Linked { peer, words } => {
                tell(FromWorker::Linked {
                    peer_device: peer.device,
                    version: peer.version,
                    peer_name: peer.name,
                    contact: words.map(|(own, peer)| PairWords { own, peer }),
                });
                blobs.attach(attempt);
            }
            Event::Path(peer_device, path) => {
                tell(FromWorker::Path { peer_device, path: PathMemory { addr: path.public, lan: path.lan } })
            }
            Event::Lost { why, .. } => {
                if self.call.deliver(hub.stream_bytes(), now) {
                    (env.commands.signal)();
                }
                if let Some(line) = env.commands.phases.borrow_mut().enter("lost", Instant::now()) {
                    makepad_widgets::log!("{line}: {why}");
                }
                blobs.detach(attempt);
                *self = Pump::default();
                tell(FromWorker::Lost);
            }
            Event::Reconnecting(device, since) => tell(FromWorker::Reconnecting { device, since }),
            Event::PeerVersion(version) => {
                self.peer_newest = Some(version);
                tell(FromWorker::PeerVersion(version));
            }
            Event::Refused(why) => tell(FromWorker::Refused(why)),
            Event::KeyChanged => tell(FromWorker::KeyChanged),
            Event::Failed(why) if attempt.carrier == Carrier::Words && why == VERSION_TEXT =>
                tell(FromWorker::Failed(pair_version_text(self.peer_newest).into())),
            Event::Failed(why) => tell(FromWorker::Failed(why)),
            Event::Sync(bytes) => tell(FromWorker::Message(bytes)),
            Event::Blob(bytes) => blobs.receive(attempt, &bytes, now),
            Event::CallReady => {
                self.call.pending.extend(hub.take_call_frames(attempt).into_iter().map(|(_, bytes)| bytes))
            }
            Event::Media(from, bytes) => self.call.datagram(from, bytes, now),
            Event::PeerUndelivered(e) => makepad_widgets::log!(
                "Town link: {} undelivered {:?} messages for {}",
                e.count,
                e.topic,
                makepad_town_core::hex(&e.peer)
            ),
        }
        Ok(changed)
    }
}

pub const PAIR_UPDATING: &str = "This copy of Makepad Town is older than the other one; it is updating. Pair again once it has restarted.";
pub const PAIR_PEER_OLDER: &str = "The other copy of Makepad Town is older; it needs to update before you can pair.";

fn pair_version_text(peer_newest: Option<u16>) -> &'static str {
    match peer_newest {
        Some(v) if v > makepad_town_core::link::VERSION => PAIR_UPDATING,
        Some(v) if v == makepad_town_core::link::VERSION => VERSION_TEXT,
        _ => PAIR_PEER_OLDER,
    }
}

pub const LOCAL_NETWORK_OFF: &str = "Allow Local Network access in System Settings.";
pub const BAD_WORDS: &str = "Those words don't add up. Check them.";
pub const WORDS_OUT_OF_DATE: &str = "These words are out of date. Try again.";
pub const NOT_REACHED_SECS: u64 = 15;
pub const START_WAIT_SECS: u64 = 5;

/// The developer switch that sends the link through one network interface (`en1`), so a machine
/// whose routes prefer another one (Ethernet first) can still link over this one (a phone's
/// hotspot). The reflectors are asked over the same socket, so the words carry that network's
/// public address. Unset, the system's routes decide.
const LINK_INTERFACE: &str = "MAKEPAD_TOWN_LINK_IF";

fn link_interface() -> Option<String> {
    std::env::var(LINK_INTERFACE).ok().map(|name| name.trim().to_owned()).filter(|name| !name.is_empty())
}

/// The link's socket on `port` (0: any free one), through the interface [`LINK_INTERFACE`] names,
/// with one log line naming it. A switch that can't be honoured is an error, never the default
/// route: that would link over the path the developer asked to avoid.
fn bind_link(port: u16) -> io::Result<UdpSocket> {
    let (socket, tied) = bind_link_quietly(port)?;
    match tied {
        Some((name, InterfaceBind::Device)) => {
            makepad_widgets::log!("town: the link goes only through network interface {name} ({LINK_INTERFACE})")
        }
        Some((name, InterfaceBind::Address(addr))) => makepad_widgets::log!(
            "town: the link uses network interface {name}'s address {addr} ({LINK_INTERFACE}); the routes still choose the way out"
        ),
        None => {}
    }
    Ok(socket)
}

fn bind_link_quietly(port: u16) -> io::Result<(UdpSocket, Option<(String, InterfaceBind)>)> {
    match link_interface() {
        None => UdpSocket::bind(("0.0.0.0", port)).map(|socket| (socket, None)),
        Some(name) => match bind_udp_on_interface(&name, port) {
            Ok((socket, how)) => Ok((socket, Some((name, how)))),
            Err(e) => Err(io::Error::new(e.kind(), format!("couldn't send the link through network interface {name} ({LINK_INTERFACE}): {e}"))),
        },
    }
}

/// This machine's LAN address on the route toward `public`, with `socket`'s port. Connecting a
/// UDP socket sends nothing; it only picks the interface (the switch's, when it is set).
fn lan_address(socket: &impl LinkSocket, public: SocketAddrV4) -> Result<SocketAddrV4, String> {
    let (probe, _) = bind_link_quietly(0).map_err(|e| e.to_string())?;
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
    pub devices: DeviceLinks,
    pub side: Option<crate::model::PairSide>,
    /// The form owns this first-contact worker. Viewing a full town starts no worker.
    pub first_contact: bool,
    pub confirm_forget: bool,
    pub devices_error: Option<String>,
    /// A first contact is running (the worker is up).
    pub open: bool,
    /// Our invite words, once the address is known, in spoken groups of four.
    pub own_words: Option<String>,
    /// From pressing Pair until the town opens, including the offer and answer.
    pub waiting_since: Option<u64>,
    pub renewed: bool,
    pub status: Option<Status>,
    /// Why the last words typed didn't decode.
    pub bad_words: Option<String>,
    /// What the system refuses, while it does.
    pub blocked: Option<&'static str>,
    /// The label from the current proof; the model keeps the frozen pin label.
    pub linked: Option<String>,
    /// Why the worker stopped.
    pub failed: Option<String>,
    /// The device selected in the connect form, once known.
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
    /// The next try is due at this wall time (unix seconds).
    Backoff { retry_at: u64 },
    /// Nothing is reaching it; it was last linked at `last_seen` (unix seconds).
    Offline { last_seen: u64 },
}

#[derive(Default)]
pub struct DeviceLinks(BTreeMap<PublicKey, DeviceAttempt>);

struct DeviceAttempt {
    generation: u64,
    // A words slot keeps its own generation when promoted over a newer known slot.
    newest: u64,
    link: Option<DeviceLink>,
}

impl ConnectView {
    pub fn edit_words(&mut self, draft: &str) {
        if self.waiting_since.is_none() {
            self.bad_words = if draft.trim().is_empty() { None } else { PairWords::decode(draft).err() };
        }
    }

    /// Update presence for every peer. Only the selected attempt also drives the single form.
    /// A false result identifies a retired attempt's status notice.
    pub fn apply_attempt(&mut self, at: Option<Attempt>, event: &FromWorker, selected: bool) -> bool {
        if let Some(Attempt { carrier: Carrier::Peer(key), generation }) = at {
            let link = match event {
                FromWorker::PeerVersion(_) => return self.devices.0.get(&key)
                    .is_none_or(|d| generation == d.generation || generation > d.newest),
                FromWorker::Reconnecting { since, .. } => Some(DeviceLink::Reaching { since: *since }),
                FromWorker::Linked { .. } | FromWorker::Phase(Status::Linked) => Some(DeviceLink::Linked),
                FromWorker::Phase(Status::Backoff { retry_at }) => Some(DeviceLink::Backoff { retry_at: *retry_at }),
                FromWorker::Lost | FromWorker::KeyChanged | FromWorker::Refused(_) | FromWorker::Failed(_)
                    | FromWorker::Phase(Status::Stopped) => None,
                FromWorker::Phase(Status::Reconnecting | Status::Reaching { .. } | Status::Securing | Status::Proving) => {
                    let since = match self.devices.0.get(&key) {
                        Some(DeviceAttempt { generation: current, link: Some(DeviceLink::Reaching { since }), .. })
                            if *current == generation => *since,
                        _ => wall_secs(),
                    };
                    Some(DeviceLink::Reaching { since })
                }
                _ => {
                    if selected { self.apply(event); }
                    return true;
                }
            };
            let device = self.devices.0.entry(key).or_insert(DeviceAttempt { generation, newest: generation, link: None });
            // On promotion the hub first stops the displaced slot. The window has matched
            // this words proof to the open request. Later events of the displaced slot stay stale.
            let promotion = selected && device.link.is_none() && matches!(event, FromWorker::Linked { contact: Some(_), .. });
            if generation != device.generation && generation <= device.newest && !promotion { return false; }
            device.generation = generation;
            device.newest = device.newest.max(generation);
            device.link = link;
        } else if at.is_none() && matches!(event, FromWorker::Failed(_) | FromWorker::Idle(_)) {
            self.devices.0.clear();
        }
        if selected { self.apply(event); }
        true
    }

    /// Takes a notice for the connect form.
    pub fn apply(&mut self, event: &FromWorker) {
        match event {
            FromWorker::Invite { words } => self.own_words = Some(words::group(words)),
            FromWorker::WordsChanged => {
                self.own_words = None;
                self.waiting_since = None;
                self.renewed = true;
                self.status = None;
                self.blocked = None;
            }
            FromWorker::WordsRejected(WordsRejected::AlreadyPairing) => {}
            FromWorker::WordsRejected(WordsRejected::StaleAttempt | WordsRejected::OlderInvite | WordsRejected::NotReady) => {
                self.waiting_since = None;
                self.blocked = None;
                self.failed = Some(WORDS_OUT_OF_DATE.into());
            }
            FromWorker::Blocked(why) => self.blocked = Some(*why),
            FromWorker::Phase(status) => {
                self.status = Some(*status);
                if matches!(status, Status::Backoff { .. } | Status::Stopped) {
                    self.reaching_since = None;
                    self.linked = None;
                }
                if matches!(status, Status::Securing | Status::Proving | Status::Linked) {
                    self.blocked = None;
                    self.failed = None;
                } else if matches!(status, Status::Reaching { .. }) {
                    self.failed = None;
                }
            }
            FromWorker::Linked { peer_device, peer_name, .. } => {
                self.blocked = None;
                self.failed = None;
                self.linked = Some(peer_name.clone());
                (self.peer, self.reaching_since, self.refused) = (Some(*peer_device), None, None);
            }
            FromWorker::PeerVersion(_) | FromWorker::WordsOpened(_) | FromWorker::Idle(_) | FromWorker::Writable | FromWorker::Message(_) | FromWorker::Path { .. } | FromWorker::BlobWorker(_) | FromWorker::BlobStatus => {}
            FromWorker::Lost => (self.linked, self.reaching_since) = (None, None),
            // Reaching a known device needs no words: the first contact's form and notes go. A
            // refusal stays until a link proves otherwise, so its line doesn't flicker between tries.
            FromWorker::Reconnecting { device, since } => {
                *self = ConnectView {
                    devices: std::mem::take(&mut self.devices),
                    side: self.side, first_contact: self.first_contact, failed: self.failed.take(),
                    confirm_forget: self.confirm_forget, devices_error: self.devices_error.take(), refused: self.refused.take(),
                    open: true, peer: Some(*device), reaching_since: Some(*since), ..ConnectView::default()
                };
            }
            FromWorker::Refused(line) => (self.refused, self.reaching_since) = (Some(line.clone()), None),
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
        self.devices.0.get(device).and_then(|d| d.link).unwrap_or(DeviceLink::Offline { last_seen })
    }

}

/// Two real loopback workers with the address discovery already done. A worker waits for
/// its model's words command before starting the production proof.
#[cfg(test)]
pub(crate) fn pair_workers(a: Arc<DeviceKey>, b: Arc<DeviceKey>, progress: Arc<Progress>) -> [(LinkWorker, PairWords); 2] {
    pair_workers_on(a, b, progress, || (UdpSocket::bind("127.0.0.1:0").expect("a loopback socket"), None))
}

#[cfg(test)]
fn pair_workers_on<S: LinkSocket + Send + 'static>(a: Arc<DeviceKey>, b: Arc<DeviceKey>, progress: Arc<Progress>,
    mut bind: impl FnMut() -> (S, Option<LinkPortLease>)) -> [(LinkWorker, PairWords); 2] {
    let mut side = |device: Arc<DeviceKey>| {
        let bound = bind();
        let SocketAddr::V4(addr) = bound.0.local_addr().unwrap() else { unreachable!("bound to IPv4") };
        let mut session = InviteSession::open(0).expect("an invite key");
        let invite_key = session.key(0).expect("a new invite serves");
        let invite = Invite { nat: Hint::Easy, addr, lan: Some(addr), commit: Invite::commitment(Hint::Easy, addr, Some(addr), &invite_key.public()) };
        (bound, device, session, invite)
    };
    let (sa, da, session_a, invite_a) = side(a);
    let (sb, db, session_b, invite_b) = side(b);
    let spawn = |seat, bound: (S, Option<LinkPortLease>), device: Arc<DeviceKey>, session, own_invite: Invite, peer_invite: Invite| {
        let words = PairWords { own: own_invite.clone(), peer: peer_invite };
        let progress = progress.clone();
        let clock = LinkClock::Pair { progress: progress.clone(), seat };
        // A worker that panics says so, as a failure, so a test waiting on it ends.
        let body = move |commands: &Commands, tell: &dyn Fn(Option<Attempt>, FromWorker)| {
            let env = Env { socket: &bound.0, clock, device: Some(&device), commands, tell };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tests::wait_for_words(&env, session, own_invite, None)))
                .unwrap_or_else(|_| Err("the worker panicked".to_owned()));
            drop(bound);
            result
        };
        (LinkWorker::spawn(move || progress.tell(), body).expect("a worker thread"), words)
    };
    [
        spawn(0, sa, da, session_a, invite_a.clone(), invite_b.clone()),
        spawn(1, sb, db, session_b, invite_b, invite_a),
    ]
}

#[cfg(test)]
fn spawn_pair([a, b]: [(LinkWorker, PairWords); 2]) -> [(LinkWorker, SocketAddrV4); 2] {
    [(a, "Ada"), (b, "Bob")].map(|((worker, words), name)| {
        let addr = words.own.addr;
        assert!(worker.try_send(ToWorker::PeerWords { words, name: name.into() }).is_ok());
        (worker, addr)
    })
}

/// Counts the events a test's link workers have sent, so the test waits on their progress and
/// never on a clock. A test that hangs is the test runner's to catch.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Progress {
    // Both workers must service the current instant before the shared clock advances.
    turns: [AtomicU64; 2],
    said: std::sync::Mutex<u64>,
    wake: std::sync::Condvar,
}

#[cfg(test)]
impl Drop for LinkClock {
    fn drop(&mut self) {
        if let Self::Pair { progress, seat } = self {
            progress.turns[*seat].store(u64::MAX, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
impl Progress {
    fn link_time(&self) -> u64 {
        self.turns[0].load(Ordering::SeqCst).min(self.turns[1].load(Ordering::SeqCst))
    }

    fn advance(&self, seat: usize, millis: u64) {
        self.turns[seat].fetch_max(self.link_time().saturating_add(millis), Ordering::SeqCst);
    }

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
        match worker.from.recv().map(|event| worker.observe(event)) {
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
    let pair = spawn_pair(pair_workers(a, b, progress.clone())).map(|(worker, _)| {
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
    use socket::mem::MemNet;
    use makepad_town_core::connect::CALL_DATAGRAM;

    pub(super) struct AddressWatch { core: hub::AddressWatch, transaction: Option<[u8; 12]> }

    impl AddressWatch {
        fn new(server: SocketAddr, addr: SocketAddrV4, lan: Option<SocketAddrV4>, include_lan: bool, opened: u64) -> Self {
            Self { core: hub::AddressWatch::new(server,addr,lan,include_lan,opened), transaction: None }
        }
        fn changed(&self, from: SocketAddr, bytes: &[u8]) -> bool {
            let mut core = self.core.clone();
            if let Some(tx) = self.transaction { core.request(tx); }
            core.observation(from, bytes) == Some(true)
        }
        fn poll(&mut self, _socket: &impl LinkSocket, now: u64) -> Result<bool,String> { Ok(self.core.expired(now)) }
    }

    pub(super) fn wait_for_words(env: &Env<impl LinkSocket>, session: InviteSession, own: Invite, watch: Option<AddressWatch>) -> Result<(),String> {
        let mut hub = Hub::new(env.device.unwrap().clone());
        let selected = hub.start_words(false, at(env));
        hub.wait_for_words(selected, session,own,watch.map(|w| w.core));
        env.commands.waker.serve(env.socket);
        let mut blobs = worker_blobs(env.device.unwrap().public(), env.commands, env.tell);
        match drive(env,&mut hub,&mut blobs,&mut None,&mut Vec::new(),&mut None)? {
            Some(e) => Err(e.to_string()), None => Ok(()),
        }
    }

    // The overdue-stream fixture lends its already authenticated connection to the production
    // turn and inspects its terminal stream state afterwards.
    struct Extras { watch: Option<AddressWatch>, tags: Option<ProbeTags>, invite: Option<Invite> }
    enum End { Failed { lost: bool } }

    fn run_connection(env: &Env<impl LinkSocket>, conn: &mut Connection, extras: &mut Extras, established: &mut Option<PublicKey>) -> End {
        assert!(extras.watch.is_none() && extras.tags.is_none() && extras.invite.is_none());
        let peer = conn.peer().unwrap();
        let mut slot = hub::Slot::new(env.device.unwrap().clone(), Start::Known {pin: Pin { device:peer.device, min_version:peer.version }, path:conn.path().unwrap()});
        let mut buf = [0; 2048];
        loop {
            let now = at(env);
            let more = read_batch(env, &mut buf, |from, bytes| {
                if !env.commands.waker.is_wake(bytes, from) { conn.handle(from, bytes, now.ms); }
            }).unwrap();
            if more && conn.idle_due(now.ms) { continue }
            let (active, events) = slot.poll_connection(conn, now);
            let mut lost = false;
            for event in events {
                match event {
                    Event::Sync(body) => (env.tell)(None, FromWorker::Message(body)),
                    Event::Linked {peer, ..} => *established = Some(peer.device),
                    Event::Lost {..} => lost = true,
                    Event::Send(to,bytes) => { env.socket.send_to(&bytes,to).unwrap(); }
                    _ => {}
                }
            }
            if !active { return End::Failed { lost } }
            if let Ok((n, from)) = env.receive(&mut buf, Duration::from_millis(1)) {
                if !env.commands.waker.is_wake(&buf[..n], from) { conn.handle(from,&buf[..n],env.now()); }
            }
        }
    }

    fn report_lost(end: &End, _conn: &Connection, tell: &dyn Fn(FromWorker)) {
        if matches!(end, End::Failed {lost:true}) { tell(FromWorker::Lost); }
    }

    #[test]
    fn a_failure_reads_in_plain_words_and_a_version_mismatch_is_never_an_attacker() {
        let refusals = [
            Refusal::Format("unexpected message"), Refusal::Version { peer_newest: None }, Refusal::Gate, Refusal::Signature, Refusal::Commitment,
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
        for refusal in [Refusal::Version { peer_newest: None }, Refusal::Format("unexpected message"), Refusal::Format("version not offered")] {
            let text = describe(&Failure::Refused(refusal.clone()));
            assert_eq!(text, "Update Makepad Town on both computers, then try again.", "{refusal:?} is a version problem");
            assert!(!text.contains("between you"), "{refusal:?} must not warn of an attacker");
        }
        for refusal in [Refusal::Gate, Refusal::Signature, Refusal::Commitment] {
            assert!(describe(&Failure::Refused(refusal.clone())).contains("someone may be between you"), "{refusal:?} keeps the warning");
        }
    }

    #[test]
    fn only_current_words_survive_expiry_or_a_new_reflected_address() {
        let addr = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 4242);
        let new_addr = SocketAddrV4::new(*addr.ip(), addr.port() + 1);
        let server: SocketAddr = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 2), 3478).into();
        let net = MemNet::default();
        let socket = net.bind(0).unwrap();
        let mut watch = AddressWatch::new(server, addr, None, false, 0);
        let tx = [1; 12];
        watch.transaction = Some(tx);
        let response = |addr: SocketAddrV4, tx| reflect::answer(&reflect::request(&tx, Style::Padded), addr.into()).unwrap();
        assert!(!watch.changed(server, &response(addr, tx)));
        assert!(!watch.changed(server, &response(new_addr, [2; 12])), "old responses can't replace words");
        assert!(!watch.changed(addr.into(), &response(new_addr, tx)), "only the chosen reflector is trusted");
        assert!(watch.changed(server, &response(new_addr, tx)));
        assert!(watch.poll(&socket, INVITE_LIFETIME_MS).unwrap(), "expiry also ends a pending proof");

        let mut view = ConnectView::default();
        view.apply(&FromWorker::Phase(Status::FindingAddress));
        assert!(view.own_words.is_none());
        let mut session = InviteSession::open(0).unwrap();
        let key = session.key(0).unwrap().public();
        let invite = Invite { nat: Hint::Easy, addr, lan: None, commit: Invite::commitment(Hint::Easy, addr, None, &key) };
        view.apply(&FromWorker::Invite { words: invite.to_words() });
        let old = view.own_words.clone().unwrap();
        view.waiting_since = Some(0);
        // A replacement reflection reaches the same socket while it is already punching.
        // Only this test's reflector and prospective peer can receive a packet.
        let reflector = net.bind(0).unwrap();
        let peer = net.bind(0).unwrap();
        let SocketAddr::V4(target) = peer.local_addr().unwrap() else { unreachable!() };
        let watch = AddressWatch::new(reflector.local_addr().unwrap(), addr, None, false, 0);
        let device = Arc::new(DeviceKey::generate().unwrap());
        let local = invite.clone();
        let worker = LinkWorker::spawn(|| {}, move |commands, tell| {
            let progress = Arc::new(Progress::default());
            progress.turns[1].store(u64::MAX, Ordering::SeqCst);
            let env = Env { socket: &socket, clock: LinkClock::Pair { progress, seat: 0 }, device: Some(&device), commands, tell };
            wait_for_words(&env, session, local, Some(watch))
        }).unwrap();
        let words = PairWords { own: invite.clone(), peer: Invite { addr: target, ..invite.clone() } };
        assert!(worker.try_send(ToWorker::PeerWords { words, name: "First".into() }).is_ok());
        wait_for(&worker, "punching", |e| matches!(e, FromWorker::Phase(Status::Reaching { .. })).then_some(()));
        let mut packet = [0; 128];
        let (len, from) = reflector.recv_from_within(&mut packet, KEEP_MAPPING).unwrap();
        let request = makepad_network::p2p::stun::parse(&packet[..len]).unwrap();
        reflector.send_to(&response(new_addr, request.transaction), from).unwrap();
        wait_for(&worker, "changed words", |e| {
            assert!(!matches!(e, FromWorker::Linked { .. }), "the retired proof cannot publish a pin");
            matches!(e, FromWorker::WordsChanged).then_some(())
        });
        view.apply(&FromWorker::WordsChanged);
        assert!(view.own_words.is_none() && view.waiting_since.is_none());
        let mut fresh = InviteSession::open(1).unwrap();
        let key = fresh.key(1).unwrap().public();
        let renewed = Invite { addr: new_addr, commit: Invite::commitment(Hint::Easy, new_addr, None, &key), ..invite.clone() };
        view.apply(&FromWorker::Invite { words: renewed.to_words() });
        assert!(view.renewed);
        assert_eq!(PairWords::decode(view.own_words.as_ref().unwrap()).unwrap(), renewed);
        assert_eq!(PairWords::decode(&old).unwrap(), invite);
        assert!(!invite.matches(&key), "the old words cannot prove the new key");
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
            let commands = Commands { config: Cell::new(0), signal: Box::new(|| {}), queue, waker: Arc::new(Waker::default()), phases: RefCell::default() };
            let local = Arc::new(DeviceKey::generate().unwrap());
            let mut hub = Hub::new(local);
            let old = hub.start_words(false, Now { ms: 0, unix_secs: 0 });
            let new = hub.start_words(false, Now { ms: 0, unix_secs: 0 });
            let waker = &commands.waker;
            waker.wake();
            assert!(!waker.pending.load(Ordering::SeqCst), "{bind}: a worker with no socket yet has nothing to wake");
            waker.serve(&socket);
            for _ in 0..3 {
                queue_in.try_send((None, ToWorker::Stop)).unwrap();
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
            while commands.try_recv(&hub).is_ok() {}
            queue_in.try_send((Some(old), ToWorker::Send(vec![1]))).unwrap();
            queue_in.try_send((Some(new), ToWorker::Send(vec![2]))).unwrap();
            assert!(matches!(commands.try_recv(&hub), Ok((Some(at), ToWorker::Send(bytes))) if at == new && bytes == [2]),
                "a command queued for the lost link never reaches its replacement");
            queue_in.try_send((None, ToWorker::Stop)).unwrap();
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
        assert!(call.deliver(10, 0), "delivering signalling wakes the UI");
        assert!(!call.deliver(10, 0), "a full control channel needs no additional wake");
        assert_eq!(messages.try_recv().unwrap(), [1], "the oldest control message is delivered first");
        call.pending.push_back(vec![3]);
        assert!(call.deliver(20, 5), "delivering signalling wakes the UI");
        assert_eq!(messages.try_recv().unwrap(), [2], "the retained message precedes newer traffic");
        assert!(call.deliver(30, 10), "delivering signalling wakes the UI");
        assert_eq!(messages.try_recv().unwrap(), [3], "the last queued message also arrives");
        assert_eq!(sent_bytes.load(Ordering::Relaxed), 30, "the attached call sees current stream traffic");
        assert_eq!(call.max_sleep(2_000), 50, "a second after the last traffic the worker slows down");
        drop(messages);
        call.pending.push_back(vec![4]);
        assert!(!call.deliver(40, 15), "a disconnected receiver needs no wake");
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
    fn overdue_worker_reads_queued_peer_traffic_and_reports_a_genuinely_idle_stream() {
        use makepad_network::p2p::stream::{CloseReason, State};
        let net = MemNet::default();
        let sockets = [(); 2].map(|_| net.bind(0).unwrap());
        let addr = sockets.each_ref().map(|s| s.local_addr().unwrap());
        let keys = [(); 2].map(|_| Arc::new(DeviceKey::generate().unwrap()));
        let mut conns = [0, 1].map(|i| {
            let peer = 1 - i;
            let SocketAddr::V4(lan) = addr[peer] else { unreachable!() };
            let mode = Mode::Reconnect(Reconnect { device: keys[i].clone(), peers: vec![Pin {
                device: keys[peer].public(), min_version: makepad_town_core::link::VERSION,
            }] });
            Connection::reconnect(mode, Remembered { public: None, lan: Some(lan) },
                &ProbeTags::new(&[4; 32], 0), [1 + i as u8; 72], 0).unwrap()
        });
        // Complete the normal proof, then queue the wake and both datagrams before the worker starts.
        let mut now = 0;
        while conns.iter().any(|c| c.phase() != &Phase::Linked) {
            now += 1;
            for i in [0, 1] {
                assert!(!matches!(conns[i].phase(), Phase::Failed(_)), "{:?}", conns[i].phase());
                for (_, bytes) in conns[i].poll(now) { conns[1 - i].handle(addr[i], &bytes, now); }
            }
        }
        let (_send, queue) = mpsc::sync_channel(1);
        let commands = Commands { config: Cell::new(0), queue, waker: Arc::default(), signal: Box::new(|| {}), phases: RefCell::default() };
        commands.waker.serve(&sockets[1]);
        commands.waker.wake();
        // Even an unrelated stream datagram ahead of the signed one must not expire the link.
        sockets[0].send_to(&[0xe1], addr[1]).unwrap();
        conns[0].send(Kind::Sync, b"after the stall");
        for (to, bytes) in conns[0].poll(now) { sockets[0].send_to(&bytes, to).unwrap(); }

        let progress = Arc::new(Progress::default());
        for _ in 0..100 { progress.advance(0, 1); }
        assert_eq!(progress.link_time(), 0, "one scheduled worker cannot age out its stalled peer");
        let jump = now + 90_000;
        progress.advance(0, jump);
        progress.advance(1, jump);
        let received = Cell::new(false);
        let tell = |_: Option<Attempt>, event| {
            if let FromWorker::Message(body) = event {
                assert_eq!(body, b"after the stall");
                received.set(true);
                // No further peer packet. Force another stall and let empty reads time out.
                progress.advance(0, 90_000);
                progress.advance(1, 90_000);
                commands.waker.wake();
                net.close();
            }
        };
        let env = Env { socket: &sockets[1], device: Some(&keys[1]), commands: &commands, tell: &tell,
            clock: LinkClock::Pair { progress: progress.clone(), seat: 1 } };
        let mut established = None;
        let mut extras = Extras { watch: None, tags: None, invite: None };
        let end = run_connection(&env, &mut conns[1], &mut extras, &mut established);
        assert!(received.get(), "the overdue worker read through its wake to the queued message");
        assert!(matches!(end, End::Failed { .. }));
        assert_eq!(conns[1].stream_state(), Some(State::Closed(CloseReason::TimedOut)));
        report_lost(&end, &conns[1], &|event| assert!(matches!(event, FromWorker::Lost)));
    }

    #[test]
    fn two_workers_link_over_loopback_and_carry_sync_and_call_traffic() {
        use crate::call::{CallFlow, Message, Now as CallNow};

        let keys = [(); 2].map(|_| Arc::new(DeviceKey::generate().unwrap()));
        let pair = pair_workers(keys[0].clone(), keys[1].clone(), Arc::default());
        let [(a, at_a), (b, at_b)] = spawn_pair(pair).map(|(worker, _)| {
            let at = worker.from.iter().find_map(|(at, event)| match event {
                FromWorker::Linked { peer_device, contact, .. } => {
                    assert!(contact.is_some(), "the proof used the first-contact words");
                    let at = at.expect("Linked names its attempt");
                    assert_eq!(at.carrier, Carrier::Peer(peer_device), "Linked names the promoted peer");
                    Some(at)
                }
                FromWorker::Failed(why) => panic!("first contact failed: {why}"),
                _ => None,
            }).expect("the worker stopped before proof");
            (worker, at)
        });
        assert_eq!((at_a.carrier, at_b.carrier), (Carrier::Peer(keys[1].public()), Carrier::Peer(keys[0].public())));
        let credit = a.from.iter().find_map(|(at, event)| matches!(event, FromWorker::Writable).then_some(at));
        assert_eq!(credit, Some(Some(at_a)), "first-contact sync is pulled on the promoted attempt");
        assert!(a.send((Some(at_a), ToWorker::Send(b"sync".to_vec()))).is_ok());
        assert_eq!(next_sync(&b), b"sync", "a sync message crosses");
        let ((port_a, ends_a), (port_b, ends_b)) = (media_port(), media_port());
        assert!(a.send((Some(at_a), ToWorker::Call(port_a))).is_ok());
        assert!(b.send((Some(at_b), ToWorker::Call(port_b))).is_ok());
        let call_id = [1; 16];
        let ring = Message::Ring { call_id, secret: [2; 32], video: false, ext: crate::call::RingExt { town: Some([9; 32]), ..Default::default() } };
        ends_a.messages_out.send(ring.encode()).unwrap();
        let mut ben = CallFlow::new(Box::new(|bytes| bytes.fill(3)));
        let now = CallNow::default();
        let mut effects = Vec::new();
        ben.set_open_town(Some(([9; 32], vec![(keys[0].public(), "Ana".into())])), &mut effects);
        ben.receive(keys[0].public(), &ends_b.messages_in.recv().expect("the link worker stopped"), now, &mut effects);
        assert!(ben.view().ringing() && ben.session() == Some(call_id), "Ben rings on the first-contact call");
        // B's port is attached once it has delivered the ring, so media from A now has somewhere to go.
        ends_a.datagrams_out.send(vec![CALL_DATAGRAM, 7]).unwrap();
        let (_, media) = ends_b.datagrams_in.recv().expect("the link worker stopped");
        assert_eq!(media, [CALL_DATAGRAM, 7], "call media crosses on the link's socket");
        ends_b.messages_out.send(b"answer".to_vec()).unwrap();
        assert_eq!(ends_a.messages_in.recv().expect("the link worker stopped"), b"answer", "and back the other way");
        assert!(b.send((Some(at_b), ToWorker::Send(b"after".to_vec()))).is_ok());
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
    fn device_link_follows_each_peers_current_attempt() {
        let (ana, ben, kit) = ([1; 65], [2; 65], [3; 65]);
        let attempt = |key, generation| Some(Attempt { carrier: Carrier::Peer(key), generation });
        let mut view = ConnectView { first_contact: true, waiting_since: Some(5), ..Default::default() };
        for (key, since) in [(ana, 10), (ben, 11), (kit, 12)] {
            view.apply_attempt(attempt(key, 1), &FromWorker::Reconnecting { device: key, since }, false);
        }
        let linked = FromWorker::Linked {
            peer_device: kit, peer_name: "Kit".into(), version: makepad_town_core::link::VERSION, contact: None,
        };
        view.apply_attempt(attempt(kit, 1), &linked, false);
        assert_eq!(view.device_link(&ana, 3), DeviceLink::Reaching { since: 10 });
        assert_eq!(view.device_link(&ben, 4), DeviceLink::Reaching { since: 11 });
        assert_eq!(view.device_link(&kit, 5), DeviceLink::Linked);

        view.apply_attempt(attempt(ben, 1), &FromWorker::Phase(Status::Backoff { retry_at: 30 }), false);
        assert_eq!(view.device_link(&ben, 4), DeviceLink::Backoff { retry_at: 30 });
        view.apply_attempt(attempt(ben, 2), &FromWorker::Reconnecting { device: ben, since: 30 }, false);
        assert!(!view.apply_attempt(attempt(ben, 1), &FromWorker::Lost, false));
        assert_eq!(view.device_link(&ben, 4), DeviceLink::Reaching { since: 30 });
        view.apply_attempt(attempt(ana, 1), &FromWorker::Phase(Status::Stopped), false);
        assert_eq!(view.device_link(&ana, 3), DeviceLink::Offline { last_seen: 3 });
        assert_eq!(view.device_link(&kit, 5), DeviceLink::Linked);
        assert_eq!(view.waiting_since, Some(5), "known carriers leave the words slot alone");
    }

    #[test]
    fn a_stopped_device_is_reached_again_with_no_words_and_a_stranger_is_not_answered() {
        let keys = [(); 3].map(|_| Arc::new(DeviceKey::generate().unwrap()));
        let net = MemNet::default();
        let progress = Arc::new(Progress::default());
        let dirs = crate::model::tests::ShareHomes::new("link-port");
        let homes = ["a","b"].map(|name| Arc::new(Home::open(&dirs.0.join(name)).unwrap()));
        let mut holders = homes.iter();
        let pair = pair_workers_on(keys[0].clone(), keys[1].clone(), progress.clone(), || {
            let home = holders.next().unwrap();
            let lease = home.lease_link_port(&AtomicBool::new(false)).unwrap();
            let socket = bind_port(&lease, &|port| net.bind(port)).unwrap();
            (socket, Some(lease))
        });
        let [(wa, addr_a), (wb, addr_b)] = spawn_pair(pair);
        for (worker, peer, reconnected) in [(&wa, keys[1].public(), false), (&wb, keys[0].public(), false)] {
            let linked = wait_for(worker, "the first link", |event| match event {
                FromWorker::Linked { peer_device, contact, .. } => Some((peer_device, contact.is_none())),
                _ => None,
            });
            assert_eq!(linked, (peer, reconnected), "a first contact links with the other device, by words");
        }
        let (path_a, path_b) = (recorded_path(&wa), recorded_path(&wb));
        assert_eq!((path_a.lan, path_b.lan), (Some(addr_b), Some(addr_a)), "each side records the peer's LAN address it saw, over the LAN path");
        assert_eq!((path_a.addr, path_b.addr), (None, None), "the LAN link records no public address");
        assert_eq!((homes[0].link_port().unwrap(), homes[1].link_port().unwrap()), (Some(addr_a.port()), Some(addr_b.port())), "each home holds its bound socket port");

        // B stops for good. A sees the close at once and goes back to reaching it.
        assert!(wb.try_send(ToWorker::Stop).is_ok());
        wait_for(&wa, "the drop", |event| matches!(event, FromWorker::Lost).then_some(()));
        let reaching = wait_for(&wa, "reaching again", |event| match event {
            FromWorker::Reconnecting { device, .. } => Some(device),
            _ => None,
        });
        assert_eq!(reaching, keys[1].public(), "a link that drops goes back to reconnecting, for the same device");
        let mut view = ConnectView::default();
        for event in [FromWorker::Invite { words: vec!["evolved"] }, FromWorker::Refused("No.".into()),
            FromWorker::Reconnecting { device: reaching, since: 1 }] {
            view.apply(&event);
        }
        assert!(view.own_words.is_none() && view.side.is_none(), "reaching a known device shows no words form");
        assert!(view.open && view.linked.is_none());
        assert_eq!((view.peer, view.reaching_since, view.refused.as_deref()), (Some(reaching), Some(1), Some("No.")),
            "it shows who is being reached, and a refusal stays between tries");

        // A device with another key gets no answer; the pair's own tag does.
        let probe = |tag: [u8; 8]| {
            let mut punch = makepad_network::p2p::punch::Punch::new(makepad_network::p2p::punch::Config::new(tag), SocketAddr::V4(addr_a), [7; 8], 0);
            punch.poll(0).0.pop().expect("a probe").1
        };
        let hour = connect::probe_hour(wall_secs());
        let outsider = net.bind(0).unwrap();
        let mut buf = [0u8; 64];
        let stranger_tag = connect::probe_tag(&keys[2].link_secret(&keys[0].public()).unwrap(), hour);
        outsider.send_to(&probe(stranger_tag), addr_a.into()).unwrap();
        net.wait_for_reader(addr_a.into());
        assert!(outsider.try_recv_from(&mut buf).is_err(), "a probe under a third device's tag gets no answer");
        let pair_tag = connect::probe_tag(&keys[0].link_secret(&keys[1].public()).unwrap(), hour);
        outsider.send_to(&probe(pair_tag), addr_a.into()).unwrap();
        net.wait_for_reader(addr_a.into());
        assert!(outsider.try_recv_from(&mut buf).is_ok_and(|(n, _)| n == 18 && buf[1] == 2), "the pair's tag is acknowledged, so the silence above means something");

        // B comes back as a reconnect worker from what it recorded, and both relink with no words.
        let known = Known { pin: Pin { device: keys[0].public(), min_version: 3 }, name: "Ada".into(), path: path_b };
        // B already sent its close. Finish its farewell and release its remembered port.
        progress.advance(0, FAREWELL_MS);
        progress.advance(1, FAREWELL_MS);
        // Release the pending read even if the command waker has already spent its burst.
        outsider.send_to(&[], addr_b.into()).unwrap();
        while wb.from.recv().is_ok() {}
        // The replacement joins A at its current time after the old clock retired this seat.
        progress.turns[1].store(progress.link_time(), Ordering::SeqCst);
        let clock = LinkClock::Pair { progress: progress.clone(), seat: 1 };
        let device = keys[1].clone();
        let bind = net.binder();
        let home = homes[1].clone();
        let wb = LinkWorker::spawn(|| {}, move |commands, tell| {
            reconnect(&home, &device, known, commands, tell, clock, bind)
        }).expect("a worker thread");
        for (worker, peer) in [(&wa, keys[1].public()), (&wb, keys[0].public())] {
            let linked = wait_for(worker, "the relink", |event| match event {
                FromWorker::Linked { peer_device, contact, .. } => Some((peer_device, contact.is_none())),
                _ => None,
            });
            assert_eq!(linked, (peer, true), "the device is reached again by its key, with no words");
        }
        assert!(wa.try_send(ToWorker::Send(b"to B".to_vec())).is_ok() && wb.try_send(ToWorker::Send(b"to A".to_vec())).is_ok());
        assert_eq!(next_sync(&wb), b"to B", "a sync message crosses to the restarted device");
        assert_eq!(next_sync(&wa), b"to A", "and back");
        assert!(wb.try_send(ToWorker::Stop).is_ok() && wa.try_send(ToWorker::Stop).is_ok());
    }

    fn memory_workers<const N: usize>(names: [&str; N]) -> (MemNet, Vec<LinkWorker>, [Known; N]) {
        let (net, _probes) = MemNet::observed(Ipv4Addr::new(192, 0, 2, 1));
        let keys = [(); N].map(|_| Arc::new(DeviceKey::generate().unwrap()));
        let sockets = [(); N].map(|_| net.bind(0).unwrap());
        let devices = std::array::from_fn(|i| Known {
            pin: Pin { device: keys[i].public(), min_version: makepad_town_core::link::VERSION },
            path: PathMemory { addr: Some(sockets[i].local_addr().unwrap()), lan: None },
            name: names[i].into(),
        });
        let workers: Vec<_> = sockets.into_iter().zip(keys).map(|(socket, key)| {
            LinkWorker::spawn(|| {}, move |commands, tell| {
                commands.waker.serve(&socket);
                let env = Env { socket: &socket, clock: LinkClock::Real(Instant::now()), device: None, commands, tell };
                let mut blobs = worker_blobs(key.public(), commands, tell);
                drive(&env, &mut Hub::new(key), &mut blobs, &mut None, &mut Vec::new(), &mut None).map(|_| ())
            }).unwrap()
        }).collect();
        (net, workers, devices)
    }

    #[test]
    fn link_worker_attaches_and_detaches_multiple_call_ports_independently() {
        let (_net, workers, devices) = memory_workers(["Hub", "Ana", "Ben", "Kit"]);
        for (worker, wanted) in workers.iter().zip([devices[1..].to_vec(), vec![devices[0].clone()],
            vec![devices[0].clone()], vec![devices[0].clone()]]) {
            assert!(worker.send((None, ToWorker::SetWanted { devices: wanted, final_sync: Vec::new() })).is_ok());
        }
        let proved = |worker: &LinkWorker| {
            worker.from.iter().find_map(|(at, event)| match event {
                FromWorker::Linked { peer_device, .. } => Some((peer_device, at.unwrap())),
                FromWorker::Failed(why) => panic!("worker failed: {why}"),
                _ => None,
            }).expect("worker stopped before link proof")
        };
        let attempts = BTreeMap::from([proved(&workers[0]), proved(&workers[0]), proved(&workers[0])]);
        let attach = |worker: &LinkWorker, at| {
            let (port, ends) = media_port();
            assert!(worker.send((Some(at), ToWorker::Call(port))).is_ok());
            ends
        };
        let ports = [1, 2, 3].map(|i| {
            let (peer, at) = proved(&workers[i]);
            assert_eq!(peer, devices[0].pin.device);
            (attach(&workers[0], attempts[&devices[i].pin.device]), attach(&workers[i], at))
        });
        let exchange = |indices: &[usize], round: u8| {
            let mut paths = Vec::new();
            for &i in indices {
                let (hub, peer) = &ports[i];
                paths.push((hub, peer, devices[0].path.addr.unwrap(), 2 * i as u8));
                paths.push((peer, hub, devices[i + 1].path.addr.unwrap(), 2 * i as u8 + 1));
            }
            let frames = |tag| [
                crate::call::Message::Mute { call_id: [tag; 16], on: round == 1 }.encode(),
                vec![1, 9, crate::call::topic::TWEAK, round, tag],
            ];
            for &(src, _, _, tag) in &paths {
                for frame in frames(tag) { src.messages_out.send(frame).unwrap(); }
            }
            // Receipt at every port also confirms every attachment before sending UDP.
            for &(_, dst, _, tag) in &paths {
                for frame in frames(tag) { assert_eq!(dst.messages_in.recv().unwrap(), frame); }
            }
            for &(src, _, _, tag) in &paths {
                src.datagrams_out.send(vec![CALL_DATAGRAM, round, tag]).unwrap();
            }
            for &(_, dst, from, tag) in &paths {
                assert_eq!(dst.datagrams_in.recv().unwrap(), (from, vec![CALL_DATAGRAM, round, tag]));
            }
        };
        exchange(&[0, 1, 2], 1);
        assert!(workers[0].send((Some(attempts[&devices[2].pin.device]), ToWorker::EndCall)).is_ok());
        assert!(ports[1].0.messages_in.recv().is_err(), "the detached signalling port closed");
        assert!(ports[1].0.datagrams_in.recv().is_err(), "the detached UDP port closed");
        exchange(&[0, 2], 2);
        for worker in &workers { assert!(worker.send((None, ToWorker::Stop)).is_ok()); }
        for worker in &workers { while worker.from.recv().is_ok() {} }
    }

    #[test]
    fn one_worker_keeps_two_proved_carriers_while_words_open_and_a_device_retires() {
        let (_net, workers, devices) = memory_workers(["Ana", "Ben", "Kit"]);
        for (worker, wanted) in workers.iter().zip([vec![1, 2], vec![0], vec![0]]) {
            let devices = wanted.into_iter().map(|i| devices[i].clone()).collect();
            assert!(worker.send((None, ToWorker::SetWanted { devices, final_sync: Vec::new() })).is_ok());
        }
        let notice = |i: usize| {
            let (at, event) = workers[i].from.recv().unwrap();
            let event = workers[i].observe((at, event));
            if let FromWorker::Failed(why) = event { panic!("worker {i}: {why}"); }
            (at, event)
        };
        let proved = |i| loop {
            if let (Some(at), FromWorker::Linked { peer_device, .. }) = notice(i) {
                assert_eq!(at.carrier, Carrier::Peer(peer_device));
                break (peer_device, at);
            }
        };
        let peers = BTreeMap::from([proved(0), proved(0)]);
        let ben = peers[&devices[1].pin.device];
        let kit = peers[&devices[2].pin.device];
        let ana_at_ben = proved(1).1;
        let ana_at_kit = proved(2).1;
        let (_resolving, lookup) = mpsc::channel();
        assert!(workers[0].send((None, ToWorker::OpenWords { request: 1, same_network: false, lookup })).is_ok());
        loop {
            if let (Some(at), FromWorker::Phase(Status::FindingAddress)) = notice(0) {
                assert_eq!(at.carrier, Carrier::Words);
                break;
            }
        }
        assert!(workers[0].send((None, ToWorker::CancelWords)).is_ok());
        assert!(workers[0].send((None, ToWorker::SetWanted {
            devices: vec![devices[2].clone()], final_sync: vec![(ben, b"last".to_vec())],
        })).is_ok());
        assert!(workers[0].send((Some(ben), ToWorker::Send(b"past the cut".to_vec()))).is_ok());
        assert!(workers[0].send((Some(kit), ToWorker::Send(b"still up".to_vec()))).is_ok());
        let mut final_messages = Vec::new();
        loop {
            match notice(1) {
                (Some(at), FromWorker::Message(bytes)) if at == ana_at_ben => final_messages.push(bytes),
                (Some(at), FromWorker::Lost) if at == ana_at_ben => break,
                _ => {}
            }
        }
        assert_eq!(final_messages, [b"last"]);
        loop {
            if let (Some(at), FromWorker::Message(bytes)) = notice(2) {
                assert_eq!((at, bytes), (ana_at_kit, b"still up".to_vec()));
                break;
            }
        }
        for worker in &workers { assert!(worker.send((None, ToWorker::Stop)).is_ok()); }
        for worker in &workers { while worker.from.recv().is_ok() {} }
    }

    // Answer one production STUN transaction with the router's shared public IP and
    // the sender's original port. MemNet::default waits for a packet on logical time.
    fn reflect_once(reflector: &socket::mem::MemSocket) {
        let mut packet = [0; 128];
        let (len, from) = reflector.recv_from_within(&mut packet, Duration::ZERO).unwrap();
        let tx = makepad_network::p2p::stun::parse(&packet[..len]).unwrap().transaction;
        let mapped = SocketAddr::from((Ipv4Addr::new(198, 51, 100, 10), from.port()));
        let reply = reflect::answer(&reflect::request(&tx, Style::Padded), mapped).unwrap();
        reflector.send_to(&reply, from).unwrap();
    }

    fn open_mem_words(worker: &LinkWorker, reflector: &socket::mem::MemSocket) -> (Attempt, Invite) {
        let (send, lookup) = mpsc::channel();
        send.send(Ok(vec![reflector.local_addr().unwrap()])).unwrap();
        assert!(worker.send((None, ToWorker::OpenWords { request: 1, same_network: true, lookup })).is_ok());
        wait_for(worker, "words attempt", |e| matches!(e, FromWorker::WordsOpened(1)).then_some(()));
        let at = worker.observed.get().unwrap();
        reflect_once(reflector);
        let invite = wait_for(worker, "reflected invite", |e| match e {
            FromWorker::Invite { words } => Some(PairWords::decode(&words.join(" ")).unwrap()),
            _ => None,
        });
        (at, invite)
    }

    #[test]
    fn two_workers_behind_same_nat_with_one_lan_off_both_explain_why_instead_of_counting() {
        use makepad_network::p2p::punch::{Config, Punch};
        let net = MemNet::default();
        let sends = net.watch_sends();
        let reflector = net.bind_addr("203.0.113.1:3478".parse().unwrap()).unwrap();
        let local = ["192.168.77.10:4100", "192.168.77.20:4200"].map(|s| s.parse().unwrap());
        let progress = Arc::new(Progress::default());
        let workers = [0, 1].map(|seat| {
            let socket = net.bind_addr(local[seat]).unwrap();
            let clock = LinkClock::Pair { progress: progress.clone(), seat };
            LinkWorker::spawn(|| {}, move |commands, tell| {
                let device = Arc::new(DeviceKey::generate().unwrap());
                let mut blobs = worker_blobs(device.public(), commands, tell);
                let mut hub = Hub::with_lan(device, seat == 0);
                commands.waker.serve(&socket);
                let env = Env { socket: &socket, clock, device: None, commands, tell };
                drive(&env, &mut hub, &mut blobs, &mut None, &mut Vec::new(), &mut None).map(|_| ())
            }).unwrap()
        });
        let [(at_a, a), (at_b, b)] = workers.each_ref().map(|w| open_mem_words(w, &reflector));
        assert_eq!((a.to_words().len(), a.lan.map(SocketAddr::V4)), (14, Some(local[0])));
        assert_eq!((b.to_words().len(), b.lan), (10, None));
        let mut views = [0, 1].map(|_| ConnectView { waiting_since: Some(0), ..Default::default() });
        let receive = |seat: usize, view: &mut ConnectView| {
            net.wait_for_reader(local[seat]);
            for (_, event) in workers[seat].from.try_iter() {
                assert!(!matches!(event, FromWorker::Failed(_) | FromWorker::WordsChanged | FromWorker::WordsRejected(_)),
                    "worker {seat} lost the pairing attempt: {event:?}");
                view.apply(&event);
            }
        };
        let advance = |step| {
            progress.advance(0, step);
            progress.advance(1, step);
            for worker in &workers { worker.waker.wake(); }
        };
        let submit = |seat: usize, at, own: &Invite, peer: &Invite, name: &str| {
            let words = PairWords { own: own.clone(), peer: peer.clone() };
            assert!(workers[seat].send((Some(at), ToWorker::PeerWords { words, name: name.into() })).is_ok());
        };

        // Ben enters Ana's words first. No public endpoint loops back to a worker yet.
        submit(1, at_b, &b, &a, "Ben");
        receive(1, &mut views[1]);
        assert_eq!(views[1].blocked, Some(SAME_NETWORK_LAN_OFF_HERE));
        while !matches!(views[1].status, Some(Status::Reaching { secs }) if secs >= NOT_REACHED_SECS) {
            assert!(progress.link_time() < KEEP_MAPPING.as_millis() as u64);
            advance(1_000);
            receive(1, &mut views[1]);
        }
        assert_eq!(views[1].wait_status(100), SAME_NETWORK_LAN_OFF_HERE);

        let spare = net.bind_addr("192.168.77.30:4300".parse().unwrap()).unwrap();
        // first_contact_tag calls the core's crate-private invite_tag on these two invites.
        let key = Arc::new(DeviceKey::generate().unwrap());
        let tag = connect::first_contact_tag(&Mode::FirstContact(makepad_town_core::link::FirstContact {
            device: key.clone(), invite_key: key, own_invite: a.clone(), peer_invite: b.clone(), name: "Ana".into(),
        })).unwrap();
        let now = progress.link_time();
        let mut punch = Punch::new(Config::new(tag), local[0], [3; 8], now);
        let (_, probe) = punch.poll(now).0.pop().unwrap();
        spare.send_to(&probe, local[0]).unwrap();
        net.wait_for_reader(local[0]);
        submit(0, at_a, &a, &b, "Ana");
        receive(0, &mut views[0]);
        assert!(matches!(views[0].status, Some(Status::Reaching { .. })), "Ana must answer the submitted words");
        assert_eq!(views[0].blocked, Some(SAME_NETWORK_PEER_LAN_OFF));
        assert_eq!(views[0].wait_status(100), SAME_NETWORK_PEER_LAN_OFF);

        // Enable hairpin translation on the same router while the original attempts keep dialing.
        net.set_hairpin(local[0], a.addr.into());
        net.set_hairpin(local[1], b.addr.into());
        while views.iter().any(|v| v.linked.is_none()) {
            advance(20);
            for (seat, view) in views.iter_mut().enumerate() { receive(seat, view); }
        }
        assert_eq!(views[0].linked.as_deref(), Some("Ben"));
        assert_eq!(views[1].linked.as_deref(), Some("Ana"));
        assert!(views.iter().all(|v| v.blocked.is_none()));
        let ben_sends: Vec<_> = sends.try_iter().filter_map(|(from, to)| (from == local[1]).then_some(to)).collect();
        assert!(ben_sends.contains(&SocketAddr::V4(a.addr)), "Ben dialed Ana's public address");
        assert!(ben_sends.iter().all(|to| !makepad_network::lan::is_local_network(to.ip())),
            "Ben sent a datagram to the local network: {ben_sends:?}");
        for worker in &workers { assert!(worker.send((None, ToWorker::Stop)).is_ok()); }
        net.close();
        for worker in &workers { for _ in worker.from.iter() {} }
    }

    #[test]
    fn peer_words_never_drop_silently_across_socket_rebind_stale_attempt_or_worker_exit() {
        let dirs = crate::model::tests::ShareHomes::new("words-rebind");
        let home = Arc::new(Home::open(&dirs.0).unwrap());
        let net = Arc::new(MemNet::default());
        let local: SocketAddr = "192.168.77.10:4100".parse().unwrap();
        let reflector = net.bind_addr("203.0.113.1:3478".parse().unwrap()).unwrap();
        home.lease_link_port(&AtomicBool::new(false)).unwrap().remember(local.port().try_into().unwrap()).unwrap();
        let worker = {
            let net = net.clone();
            LinkWorker::spawn(|| {}, move |commands, tell| {
                let device = Arc::new(DeviceKey::generate().unwrap());
                let progress = Arc::new(Progress::default());
                progress.turns[1].store(u64::MAX, Ordering::SeqCst);
                run(&home, device.public(), Hub::with_lan(device, true), commands, tell,
                    LinkClock::Pair { progress, seat: 0 }, |port| net.bind_addr(SocketAddr::new(local.ip(), port)))
            }).unwrap()
        };
        let (at1, own) = open_mem_words(&worker, &reflector);
        net.fail_receive(local, io::ErrorKind::NotConnected);
        wait_for(&worker, "rebound words attempt", |e| matches!(e, FromWorker::WordsOpened(1)).then_some(()));
        let at2 = worker.observed.get().unwrap();
        assert_ne!(at1, at2);
        reflect_once(&reflector);
        net.wait_for_reader(local);
        let peer = Invite { addr: "192.0.2.2:4200".parse().unwrap(), lan: None, ..own.clone() };
        let submit = |at, own: &Invite| {
            let words = PairWords { own: own.clone(), peer: peer.clone() };
            assert!(worker.send((Some(at), ToWorker::PeerWords { words, name: "Ana".into() })).is_ok());
        };
        let rejected = |at, why| {
            let reason = wait_for(&worker, "words rejection", |event| match event {
                FromWorker::WordsRejected(reason) => Some(reason),
                _ => None,
            });
            assert_eq!((worker.observed.get(), reason), (Some(at), why));
        };
        submit(at1, &own);
        rejected(at1, WordsRejected::StaleAttempt);
        submit(at2, &Invite { commit: own.commit ^ 1, ..own.clone() });
        rejected(at2, WordsRejected::OlderInvite);
        submit(at2, &own);
        wait_for(&worker, "current words start dialing", |e| matches!(e, FromWorker::Phase(Status::Reaching { secs: 0 })).then_some(()));
        assert_eq!(worker.observed.get(), Some(at2));
        reflect_once(&reflector);
        assert!(worker.send((Some(at2), ToWorker::CancelWords)).is_ok());
        submit(at2, &own);
        rejected(at2, WordsRejected::StaleAttempt);
        assert!(worker.send((None, ToWorker::Stop)).is_ok());
        for _ in worker.from.iter() {}

        let stopped = LinkWorker::spawn(|| {}, |_, _| panic!("test worker exit")).unwrap();
        assert!(matches!(stopped.from.recv().unwrap(), (None, FromWorker::Failed(why)) if why == "the link worker stopped unexpectedly"));
        assert!(stopped.from.recv().is_err(), "worker exited after reporting its panic");
        let words = PairWords { own, peer };
        assert!(matches!(stopped.send((Some(at2), ToWorker::PeerWords { words, name: "Ana".into() })),
            Err(SendError::Disconnected((Some(at), ToWorker::PeerWords { .. }))) if at == at2));
    }

    #[test]
    fn worker_exit_reports_failure_even_when_signal_and_phase_logging_panic() {
        let worker = LinkWorker::spawn(|| {}, |_, _| Ok(())).unwrap();
        assert!(worker.from.recv().is_err());
        assert!(matches!(worker.send((None, ToWorker::Stop)), Err(SendError::Disconnected((None, ToWorker::Stop)))));
        assert!(matches!(worker.next_event(), Err(mpsc::TryRecvError::Disconnected)));

        for (body_panics, expected) in [(false, "boom"), (true, "the link worker stopped unexpectedly")] {
            let worker = LinkWorker::spawn(|| panic!("signal panic"), move |commands, _| {
                std::mem::forget(commands.phases.borrow_mut());
                if body_panics { panic!("body panic") }
                Err("boom".into())
            }).unwrap();
            assert!(matches!(worker.from.recv().unwrap(), (None, FromWorker::Failed(why)) if why == expected));
            assert!(worker.from.recv().is_err(), "the worker must deliver exactly one failure");
        }
    }

    #[test]
    fn a_broken_socket_rebinds_the_home_port_and_punches_again() {
        let dirs = crate::model::tests::ShareHomes::new("socket-rebind");
        let ip = Ipv4Addr::new(192, 0, 2, 7);
        let (net, punches) = MemNet::observed(ip);
        let keys = [(); 2].map(|_| Arc::new(DeviceKey::generate().unwrap()));
        let ports = [4500, 4501];
        let homes = ["a", "b"].map(|name| Arc::new(Home::open(&dirs.0.join(name)).unwrap()));
        let addresses = ports.map(|port| SocketAddr::from((ip, port)));
        let binds = Arc::new(Mutex::new(Vec::new()));
        let workers = [0, 1].map(|i| {
            let home = homes[i].clone();
            home.lease_link_port(&AtomicBool::new(false)).unwrap().remember(ports[i].try_into().unwrap()).unwrap();
            let device = keys[i].clone();
            let known = Known {pin: Pin {device: keys[1-i].public(), min_version: makepad_town_core::link::VERSION},
                name: String::new(), path: PathMemory {addr:Some(addresses[1-i]), lan:None}};
            let bind = net.binder();
            let binds = binds.clone();
            LinkWorker::spawn(|| {}, move |commands,tell| {
                reconnect(&home,&device,known,commands,tell,LinkClock::Real(Instant::now()),|port| {
                    binds.lock().unwrap().push((i,port));
                    bind(port)
                })
            }).unwrap()
        });
        let proved = |worker: &LinkWorker| wait_for(worker, "authenticated carrier", |event| match event {
            FromWorker::Linked {peer_device, ..} => Some(peer_device), _ => None,
        });
        for i in [0,1] { assert_eq!(proved(&workers[i]), keys[1-i].public()); }
        net.fail_receive(addresses[0], io::ErrorKind::ConnectionReset);
        assert!(workers[0].try_send(ToWorker::Send(b"before".to_vec())).is_ok());
        assert_eq!(next_sync(&workers[1]), b"before");
        assert_eq!(binds.lock().unwrap().len(), 2, "a transient error kept the original socket");
        for _ in punches.try_iter() {}
        net.fail_receive(addresses[0], io::ErrorKind::NotConnected);
        wait_for(&workers[0], "lost carrier", |e| matches!(e,FromWorker::Lost).then_some(()));
        for i in [0,1] { assert_eq!(proved(&workers[i]), keys[1-i].public()); }
        assert!(punches.try_iter().any(|from| from == addresses[0]), "the replacement socket punched from the home port");
        assert_eq!(binds.lock().unwrap().iter().filter(|(i,_)| *i == 0).copied().collect::<Vec<_>>(), [(0,4500),(0,4500)]);
        assert!(workers[0].try_send(ToWorker::Send(b"after".to_vec())).is_ok());
        assert_eq!(next_sync(&workers[1]), b"after", "a command wakes the rebound socket");
        for w in &workers { assert!(w.try_send(ToWorker::Stop).is_ok()); }
    }

    #[test]
    fn a_phase_logs_once_on_entry_with_the_time_of_the_one_before() {
        let start = Instant::now();
        let mut log = PhaseLog::default();
        let reaching = |secs| phase_of(&FromWorker::Phase(Status::Reaching { secs }));
        assert_eq!(log.enter(reaching(0).unwrap(), start).as_deref(), Some("Town link: dialing"), "the first phase logs without a time");
        assert_eq!(reaching(1), reaching(7), "the seconds counter is one phase");
        assert!(log.enter(reaching(7).unwrap(), start + Duration::from_secs(7)).is_none(), "a phase that continues logs nothing");
        let line = log.enter(phase_of(&FromWorker::Phase(Status::Securing)).unwrap(), start + Duration::from_millis(12_340)).unwrap();
        assert_eq!(line, "Town link: stream up, securing (12.3 s after dialing)", "a change names the phase before it and its time");
        assert!(phase_of(&FromWorker::Message(vec![1])).is_none(), "traffic is not a phase");
        assert!(phase_of(&FromWorker::BlobStatus).is_none(), "blob progress is not a phase");
    }
}
