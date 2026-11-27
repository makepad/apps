//! One device's datagram and link state. All times, datagrams and host observations are inputs.
use crate::{
    connect::{self, Connection, Failure, Kind, Phase, ProbeKey, ProbeTags, Remembered},
    key::{DeviceKey, PublicKey},
    link::{FirstContact, InviteSession, Mode, Pin, Reconnect, Refusal, INVITE_LIFETIME_MS},
};
use makepad_network::p2p::{
    discover::{self, Found, Hint},
    reflect::{self, Query, Style},
    words::Invite,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    net::{SocketAddr, SocketAddrV4},
    sync::Arc,
};

pub const RETRY_MS: u64 = 2_000;
pub const FAREWELL_MS: u64 = 2_000;
const KEEP_MAPPING: u64 = 20_000;
const PROVE_WAIT_MS: u64 = 20_000;

#[derive(Clone, Copy)]
pub struct Now {
    pub ms: u64,
    pub unix_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    FindingAddress,
    WaitingForWords,
    Reaching { secs: u64 },
    Reconnecting,
    /// Wall time (Unix seconds) of the known device's next try.
    Backoff { retry_at: u64 },
    Stopped,
    Securing,
    Proving,
    Linked,
}

impl Status {
    fn retry_after(now: Now, delay_ms: u64) -> Self {
        Self::Backoff { retry_at: now.unix_secs.saturating_add(delay_ms.div_ceil(1000)) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotPhase {
    Idle,
    Punching,
    Proving,
    Up,
    Closing { until: u64 },
    Backoff { retry_at: u64 },
    Stopped,
}

pub enum Start {
    Words { same_network: bool },
    Known { pin: Pin, path: Remembered },
}

/// Raw Call frames include share and media-key signalling. Numbered topics use the existing
/// peer envelope within Kind::Call. Neither form carries a device identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Topic {
    Call,
    Opaque(u8),
}

#[derive(Debug, PartialEq, Eq)]
pub struct PeerUndelivered {
    pub peer: PublicKey,
    pub topic: Topic,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Datagram {
    Reflect,
    Link,
    Call,
    Drop,
}

pub fn route(bytes: &[u8], from: SocketAddr, attached: Option<SocketAddr>) -> Datagram {
    if matches!(bytes.first(), Some(0..=3)) {
        Datagram::Reflect
    } else if Connection::is_ours(bytes) {
        Datagram::Link
    } else if attached == Some(from) && Connection::is_call(bytes) {
        Datagram::Call
    } else {
        Datagram::Drop
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Carrier {
    Words,
    Peer(PublicKey),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Attempt {
    pub carrier: Carrier,
    pub generation: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BatchEnd {
    Drained,
    MoreQueued,
}

/// The attempt owns both its host requests and its notifications.
pub struct Output {
    pub attempt: Attempt,
    pub event: Event,
}

pub enum Event {
    Send(SocketAddr, Vec<u8>),
    Reflect(SocketAddr, Vec<u8>),
    OpenInvite { server: SocketAddr, addr: SocketAddrV4, hint: Hint, include_lan: bool },
    CheckAddress { addr: SocketAddrV4, include_lan: bool },
    Punch,
    Invite(Invite),
    WordsChanged,
    Phase(Status),
    Note(&'static str),
    Linked { peer: connect::Peer, words: Option<(Invite, Invite)> },
    Path(PublicKey, Remembered),
    Lost { why: String, sent_bytes: u64 },
    Reconnecting(PublicKey, u64),
    PeerVersion(u16),
    Refused(String),
    KeyChanged,
    Failed(String),
    Sync(Vec<u8>),
    CallReady,
    Blob(Vec<u8>),
    Media(SocketAddr, Vec<u8>),
    PeerUndelivered(PeerUndelivered),
}

#[derive(Clone)]
pub struct AddressWatch {
    server: SocketAddr,
    addr: SocketAddrV4,
    lan: Option<SocketAddrV4>,
    include_lan: bool,
    transaction: Option<[u8; 12]>,
    next: u64,
    expires: u64,
    confirm_until: Option<u64>,
}

impl AddressWatch {
    pub fn new(
        server: SocketAddr,
        addr: SocketAddrV4,
        lan: Option<SocketAddrV4>,
        include_lan: bool,
        opened: u64,
    ) -> Self {
        Self {
            server,
            addr,
            lan,
            include_lan,
            transaction: None,
            next: opened.saturating_add(KEEP_MAPPING),
            expires: opened.saturating_add(INVITE_LIFETIME_MS),
            confirm_until: None,
        }
    }

    pub fn expired(&self, now: u64) -> bool {
        now >= self.expires
    }

    pub fn request(&mut self, transaction: [u8; 12]) -> Vec<u8> {
        self.transaction = Some(transaction);
        reflect::request(&transaction, Style::Plain)
    }

    /// A matching reply reports whether the advertised endpoint changed.
    pub fn observation(&self, from: SocketAddr, bytes: &[u8]) -> Option<bool> {
        if from != self.server {
            return None;
        }
        self.transaction.as_ref().and_then(|tx| reflect::read_answer(bytes, tx)).map(|seen| seen != self.addr.into())
    }
}

struct Words {
    session: InviteSession,
    own: Invite,
    matched: Option<(Invite, String)>,
}

#[derive(Default)]
struct PeerTraffic {
    outgoing: VecDeque<(Topic, bool, Vec<u8>)>,
    generation: u64,
    incoming: VecDeque<(u64, PublicKey, Vec<u8>)>,
    partial: BTreeMap<(u64, PublicKey, u8), Vec<u8>>,
}

// The numbered peer envelope already carried by the app's call/signal.rs. Raw frames
// from MediaPort pass through verbatim during the one-slot transition.
const PEER: u8 = 9;
const MORE: u8 = PEER | 0x80;

fn envelope(bytes: &[u8]) -> Option<(u8, bool, &[u8])> {
    match bytes {
        [1, tag @ (PEER | MORE), topic, body @ ..] => Some((*topic, *tag == PEER, body)),
        _ => None,
    }
}

impl PeerTraffic {
    fn take_call_frames(&mut self) -> impl Iterator<Item = (PublicKey, Vec<u8>)> + '_ {
        self.incoming.drain(..).map(|(_, peer, bytes)| (peer, bytes))
    }

    fn enqueue(&mut self, topic: Topic, body: Vec<u8>) -> Result<(), Vec<u8>> {
        match topic {
            Topic::Call if body.len() > connect::MAX_CALL => return Err(body),
            Topic::Call => {
                let (topic, last) = envelope(&body).map_or((topic, true), |(n, last, _)| (Topic::Opaque(n), last));
                self.outgoing.push_back((topic, last, body));
            }
            Topic::Opaque(number) => {
                let mut chunks = body.chunks(connect::MAX_CALL - 3).peekable();
                loop {
                    let part = chunks.next().unwrap_or_default();
                    let last = chunks.peek().is_none();
                    let mut frame = Vec::with_capacity(3 + part.len());
                    frame.extend([1, if last { PEER } else { MORE }, number]);
                    frame.extend(part);
                    self.outgoing.push_back((topic, last, frame));
                    if last {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn flush(&mut self, conn: &mut Connection) {
        if conn.phase() == &Phase::Linked && conn.writable(Kind::Call) {
            if let Some((_, _, bytes)) = self.outgoing.pop_front() {
                conn.send(Kind::Call, &bytes);
            }
        }
    }

    fn retire(&mut self, peer: PublicKey, events: &mut Vec<Event>) {
        let mut counts = BTreeMap::new();
        for (topic, last, _) in self.outgoing.drain(..) {
            if last {
                *counts.entry(topic).or_insert(0) += 1;
            }
        }
        events.extend(
            counts.into_iter().map(|(topic, count)| Event::PeerUndelivered(PeerUndelivered { peer, topic, count })),
        );
        self.generation = self.generation.wrapping_add(1);
        // Complete messages already in the inbox remain readable. Retire incomplete
        // reassembly without letting a new connection supply the rest of an old body.
        if !self.partial.is_empty() {
            let complete: BTreeSet<_> = self
                .incoming
                .iter()
                .filter_map(|(epoch, peer, frame)| {
                    let (topic, last, _) = envelope(frame)?;
                    last.then_some((*epoch, *peer, topic))
                })
                .collect();
            self.partial.retain(|key, _| complete.contains(key));
        }
    }

    fn take(&mut self, topic: Topic) -> Vec<(PublicKey, Vec<u8>)> {
        let mut ready = Vec::new();
        let mut other = VecDeque::new();
        for (generation, peer, frame) in self.incoming.drain(..) {
            match (topic, envelope(&frame)) {
                (Topic::Call, None) => ready.push((peer, frame)),
                (Topic::Opaque(want), Some((number, last, part))) if want == number => {
                    let key = (generation, peer, number);
                    let body = self.partial.entry(key).or_default();
                    body.extend_from_slice(part);
                    if last {
                        if let Some(body) = self.partial.remove(&key) {
                            ready.push((peer, body));
                        }
                    }
                }
                _ => other.push_back((generation, peer, frame)),
            }
        }
        self.incoming = other;
        self.partial
            .retain(|(generation, _, number), _| *generation == self.generation || topic != Topic::Opaque(*number));
        ready
    }
}

/// One authenticated stream turn, shared with callers driving a connection on a simulated wire.
fn receive_messages(conn: &mut Connection, peer: PublicKey, traffic: &mut PeerTraffic, events: &mut Vec<Event>) {
    let mut call_ready = false;
    while let Some((kind, body)) = conn.recv() {
        match kind {
            Kind::Sync => events.push(Event::Sync(body)),
            Kind::Blob => events.push(Event::Blob(body)),
            Kind::Call => {
                traffic.incoming.push_back((traffic.generation, peer, body));
                if !call_ready {
                    events.push(Event::CallReady);
                    call_ready = true;
                }
            }
        }
    }
}

/// A client can open a stream as soon as its own probe is acknowledged. After an
/// ID clash a server must first hear its *replacement* nonce echoed, or a fast ACK
/// could let that client open the discarded ID. The client side answers normally,
/// including when both ends detected a clash.
#[derive(Default)]
enum Repunch {
    #[default]
    None,
    NewNonce,
    Announcing {
        nonce: [u8; 8],
        acks: BTreeMap<SocketAddr, Vec<u8>>,
    },
}

impl Repunch {
    fn begin(&mut self, mode: &Mode, random: &[u8; 72]) {
        if matches!(self, Self::NewNonce) {
            *self = if crate::link::role(mode) == Ok(crate::link::Role::Server) {
                Self::Announcing { nonce: random[..8].try_into().unwrap(), acks: BTreeMap::new() }
            } else {
                Self::None
            };
        }
    }
    fn transmit(&mut self, datagrams: Vec<(SocketAddr, Vec<u8>)>, events: &mut Vec<Event>) {
        for (to, bytes) in datagrams {
            if let Self::Announcing { acks, .. } = self {
                if bytes.starts_with(&[makepad_network::p2p::punch::KIND, 2]) {
                    // A peer's latest nonce supersedes its earlier one at this address.
                    acks.insert(to, bytes);
                    continue;
                }
            }
            events.push(Event::Send(to, bytes));
        }
    }
    fn heard(&mut self, conn: &Connection, from: SocketAddr, bytes: &[u8], events: &mut Vec<Event>) {
        if let Self::Announcing { nonce, .. } = self {
            if conn.peer_address() == Some(from)
                && bytes.starts_with(&[makepad_network::p2p::punch::KIND, 2])
                && bytes.get(10..) == Some(nonce.as_slice())
            {
                if let Self::Announcing { acks, .. } = std::mem::take(self) {
                    events.extend(acks.into_iter().map(|(to, bytes)| Event::Send(to, bytes)));
                }
            }
        }
    }
}

/// The connection state advanced by one carrier turn.
pub struct Slot {
    device: Arc<DeviceKey>,
    generation: u64,
    wanted: bool,
    wake: Option<u64>,
    phase: SlotPhase,
    conn: Option<Connection>,
    words: Option<Words>,
    known: Option<(Pin, Remembered)>,
    tags: Option<(ProbeKey, ProbeTags)>,
    first_tag: Option<[u8; 8]>,
    peer: Option<PublicKey>,
    told_path: Option<Remembered>,
    watch: Option<AddressWatch>,
    shown: Option<Status>,
    prove_until: Option<u64>,
    restart: Option<(SocketAddr, Vec<u8>)>,
    repunch: Repunch,
    traffic: PeerTraffic,
    same_network: bool,
    events: Vec<Event>,
}

impl Slot {
    pub fn new(device: Arc<DeviceKey>, start: Start) -> Self {
        let (phase, known, same_network) = match start {
            Start::Words { same_network } => (SlotPhase::Idle, None, same_network),
            Start::Known { pin, path } => {
                let phase =
                    if path == Remembered::default() { SlotPhase::Idle } else { SlotPhase::Backoff { retry_at: 0 } };
                (phase, Some((pin, path)), false)
            }
        };
        Self {
            device,
            phase,
            known,
            same_network,
            generation: 0,
            wanted: true,
            wake: Some(0),
            conn: None,
            words: None,
            tags: None,
            first_tag: None,
            peer: None,
            told_path: None,
            watch: None,
            shown: None,
            prove_until: None,
            restart: None,
            repunch: Repunch::None,
            traffic: Default::default(),
            events: Vec::new(),
        }
    }

    fn check_address(&mut self, lan: Option<SocketAddrV4>, transaction: [u8; 12]) {
        let Some(w) = &mut self.watch else { return };
        if w.include_lan && lan != w.lan {
            self.words_changed();
            return;
        }
        self.events.push(Event::Reflect(w.server, w.request(transaction)));
    }

    fn words(&mut self, own: Invite, peer: Invite, name: String, now: u64) {
        if self.phase != SlotPhase::Idle {
            return;
        }
        let Some(words) = &mut self.words else { return };
        if words.own != own {
            self.events.push(Event::Note("words for an older invite of ours, ignored"));
            return;
        }
        if discover::assess(own.nat, peer.nat) == discover::Assessment::NotPossible
            && connect::lan_candidate(&own, &peer).is_none()
        {
            self.fail("Couldn't reach it: both networks block direct links.".into());
            return;
        }
        self.first_tag = Some(connect::invite_tag(&own, &peer));
        words.matched = Some((peer, name));
        self.events.push(Event::Note("words matched"));
        if let Some(watch) = &mut self.watch {
            watch.next = now;
        }
        self.phase = SlotPhase::Punching;
        if !self.watch.as_ref().is_some_and(|w| w.confirm_until.is_some()) {
            self.events.push(Event::Punch);
        }
    }

    fn punch(&mut self, random: [u8; 72], now: Now) {
        if self.phase != SlotPhase::Punching || self.conn.is_some() {
            return;
        }
        let result = if let Some((pin, path)) = self.known {
            let Some((_, tags)) = &self.tags else { return };
            self.first_tag = None;
            let mode = Mode::Reconnect(Reconnect { device: self.device.clone(), peers: vec![pin] });
            self.repunch.begin(&mode, &random);
            Connection::reconnect(mode, path, tags, random, now.ms)
        } else {
            let Some(words) = &mut self.words else { return };
            let Some((peer, name)) = &words.matched else { return };
            let Some(invite_key) = words.session.key(now.ms) else {
                self.words_changed();
                return;
            };
            let mode = Mode::FirstContact(FirstContact {
                device: self.device.clone(),
                invite_key,
                own_invite: words.own.clone(),
                peer_invite: peer.clone(),
                name: name.clone(),
            });
            let Some(tag) = self.first_tag else {
                self.fail("Missing first-contact tag.".into());
                return;
            };
            self.repunch.begin(&mode, &random);
            Connection::new(mode, peer.addr.into(), tag, random, now.ms)
        };
        match result {
            Ok(mut conn) => {
                if let Some((from, bytes)) = self.restart.take() {
                    conn.handle(from, &bytes, now.ms);
                }
                self.conn = Some(conn);
                if self.tags.is_none() {
                    self.show(Status::Reaching { secs: 0 });
                }
            }
            Err(r) => self.fail(refusal_text(&r)),
        }
    }

    fn handle_link(&mut self, from: SocketAddr, bytes: &[u8], now: u64) {
        if let Some(conn) = &mut self.conn {
            conn.handle(from, bytes, now);
            self.repunch.heard(conn, from, bytes, &mut self.events);
            if self.peer.is_none() && self.wanted && conn.phase() == &Phase::Linked {
                let (peer, path) = (conn.peer(), conn.path());
                self.establish(peer, path, now);
            }
        } else if self.wanted
            && self.starting_carrier(now)
            && bytes.get(1) == Some(&1)
            && Connection::punch_tag(bytes).is_some()
        {
            // The host can still be supplying entropy. Keep the most recent probe (and
            // thus the remote nonce), just as Connection does before its stream opens.
            self.restart = Some((from, bytes.to_vec()));
        }
    }

    pub fn take_call_frames(&mut self) -> impl Iterator<Item = (PublicKey, Vec<u8>)> + '_ {
        self.traffic.take_call_frames()
    }

    fn send(&mut self, kind: Kind, bytes: &[u8]) {
        if let Some(conn) = &mut self.conn {
            conn.send(kind, bytes);
        }
    }
    fn writable(&self, kind: Kind) -> bool {
        self.conn.as_ref().is_some_and(|c| c.writable(kind))
    }
    fn media_address(&self) -> Option<SocketAddr> {
        self.conn.as_ref().and_then(Connection::peer_address)
    }
    fn sent_bytes(&self) -> u64 {
        self.conn.as_ref().map_or(0, Connection::sent_bytes)
    }
    /// Handle the queued start before reading a peer's first probe from the new socket.
    fn starting_carrier(&self, now: u64) -> bool {
        match self.phase {
            SlotPhase::Backoff { retry_at } => now >= retry_at,
            SlotPhase::Punching => {
                self.conn.is_none() && !self.watch.as_ref().is_some_and(|w| w.confirm_until.is_some())
            }
            _ => false,
        }
    }

    fn idle_due(&self, now: u64) -> bool {
        self.conn.as_ref().is_some_and(|c| c.idle_due(now))
    }

    fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    fn show(&mut self, status: Status) {
        if self.shown != Some(status) {
            self.shown = Some(status);
            self.events.push(Event::Phase(status));
        }
    }

    fn finish(&mut self) {
        if let Some(peer) = self.peer.take() {
            self.traffic.retire(peer, &mut self.events);
        }
        self.phase = SlotPhase::Stopped;
        if self.known.is_some() {
            self.show(Status::Stopped);
        }
        self.words = None;
        self.prove_until = None;
        self.conn = None;
        self.watch = None;
    }
    fn words_changed(&mut self) {
        self.finish();
        self.events.push(Event::WordsChanged);
    }
    fn fail(&mut self, why: String) {
        self.finish();
        self.events.push(Event::Failed(why));
    }

    fn stop(&mut self, now: u64, drain_sync: bool) {
        if self.phase == SlotPhase::Up {
            if let Some(conn) = &mut self.conn {
                if drain_sync { conn.close_after_sync(); } else { conn.close(); }
            }
            self.phase = SlotPhase::Closing { until: now.saturating_add(FAREWELL_MS) };
            if self.known.is_some() {
                self.show(Status::Stopped);
            }
        } else {
            self.finish();
        }
    }

    fn retire_peer(&mut self, why: String, sent_bytes: u64) {
        if let Some(peer) = self.peer.take() {
            if let Some(conn) = &mut self.conn {
                receive_messages(conn, peer, &mut self.traffic, &mut self.events);
            }
            self.traffic.retire(peer, &mut self.events);
            self.events.push(Event::Lost { why, sent_bytes });
        }
    }

    fn lost(&mut self, why: String, sent_bytes: u64) {
        self.retire_peer(why, sent_bytes);
        self.repunch = Repunch::None;
        self.told_path = None;
        self.prove_until = None;
        self.shown = None;
        self.conn = None;
    }

    /// The old socket and all its in-flight host observations have retired.
    fn socket_lost(&mut self, now: Now) {
        if matches!(self.phase, SlotPhase::Stopped | SlotPhase::Closing { .. }) {
            self.finish();
            return;
        }
        self.lost("the socket was lost".into(), self.sent_bytes());
        if let Some((_, path)) = self.known {
            if path == Remembered::default() { self.phase = SlotPhase::Idle; }
            else { self.backoff(now, 0); }
        } else if self.words.is_some() {
            if self.words.as_ref().is_some_and(|w| w.matched.is_some()) {
                self.phase = SlotPhase::Punching;
            }
            if let Some(w) = &mut self.watch {
                w.next = now.ms;
            }
        }
    }

    fn rebound(&mut self, now: u64) {
        if let Some(w) = &mut self.watch {
            w.transaction = None;
            w.next = now;
            w.confirm_until = Some(now.saturating_add(3_000));
            return;
        }
        if self.phase == SlotPhase::Punching && self.conn.is_none() {
            self.events.push(Event::Punch);
        }
    }

    fn ready(&self) -> bool {
        !self.events.is_empty()
            || (self.phase == SlotPhase::Up && !self.traffic.outgoing.is_empty() && self.writable(Kind::Call))
    }

    fn next_wake(&self) -> Option<u64> {
        if self.ready() {
            return Some(0);
        }
        let mut next = self.conn.as_ref().map(Connection::next_wake);
        let mut earlier = |t| next = Some(next.map_or(t, |v| v.min(t)));
        match self.phase {
            SlotPhase::Closing { until } => earlier(until),
            SlotPhase::Backoff { retry_at } => earlier(retry_at),
            _ => {}
        }
        if let Some(w) = &self.watch {
            earlier(w.next.min(w.expires));
            if let Some(t) = w.confirm_until {
                earlier(t);
            }
        }
        if let Some(t) = self.prove_until {
            earlier(t);
        }
        next
    }

    fn poll(&mut self, now: Now) -> Vec<Event> {
        if let Some(w) = &mut self.watch {
            if w.expired(now.ms) || w.confirm_until.is_some_and(|t| now.ms >= t) {
                self.words_changed();
            } else if now.ms >= w.next {
                w.next = now.ms.saturating_add(KEEP_MAPPING);
                self.events.push(Event::CheckAddress { addr: w.addr, include_lan: w.include_lan });
                return std::mem::take(&mut self.events);
            }
        }
        match self.phase {
            SlotPhase::Backoff { retry_at } if now.ms >= retry_at => {
                if let Some((pin, _)) = self.known {
                    self.phase = SlotPhase::Punching;
                    self.events.push(Event::Reconnecting(pin.device, now.unix_secs));
                    self.show(Status::Reconnecting);
                    self.events.push(Event::Punch);
                }
            }
            SlotPhase::Closing { until } if now.ms >= until => self.finish(),
            SlotPhase::Closing { .. } => {
                if let Some(c) = &mut self.conn {
                    self.events.extend(
                        c.poll(now.ms)
                            .into_iter()
                            .filter(|(_, bytes)| Connection::packet_stream_id(bytes).is_some())
                            .map(|(to, b)| Event::Send(to, b)),
                    );
                    if c.close_done() {
                        self.finish();
                    }
                }
            }
            SlotPhase::Punching | SlotPhase::Proving | SlotPhase::Up => {
                if let Some(mut c) = self.conn.take() {
                    let (stays, events) = self.poll_connection(&mut c, now);
                    if stays {
                        self.conn = Some(c);
                    }
                    return events;
                }
            }
            _ => {}
        }
        std::mem::take(&mut self.events)
    }

    /// Service the active carrier once, after delivering the socket's queued datagrams.
    pub fn poll_connection(&mut self, conn: &mut Connection, now: Now) -> (bool, Vec<Event>) {
        let active = self.service(conn, now);
        (active, std::mem::take(&mut self.events))
    }

    /// Claim the invite and publish the authenticated identity before exposing the first
    /// application bytes, including when proof and peer close arrive in one socket batch.
    fn establish(&mut self, peer: Option<connect::Peer>, observed: Option<Remembered>, now: u64) -> bool {
        let Some(peer) = peer else {
            self.fail("Missing link proof.".into());
            return false;
        };
        if self.known.is_some_and(|(pin, _)| peer.device != pin.device) {
            self.fail(refusal_text(&Refusal::KeyChanged));
            return false;
        }
        let words = if let Some(mut words) = self.words.take() {
            if !words.session.claim(now) {
                self.words_changed();
                return false;
            }
            words.matched.map(|(other, _)| (words.own, other))
        } else {
            None
        };
        self.watch = None;
        self.peer = Some(peer.device);
        self.phase = SlotPhase::Up;
        let mut path = self.known.map(|(_, path)| path).unwrap_or_default();
        if let Some(seen) = observed {
            path.public = seen.public.or(path.public);
            path.lan = seen.lan.or(path.lan);
        }
        self.known = Some((Pin { device: peer.device, min_version: peer.version }, path));
        self.events.push(Event::Linked { peer, words });
        true
    }

    fn service(&mut self, conn: &mut Connection, now: Now) -> bool {
        self.traffic.flush(conn);
        let waiting = self.shown.filter(|s| matches!(s, Status::Backoff { .. })).map(|s| (conn.next_wake(), s));
        self.repunch.transmit(conn.poll(now.ms), &mut self.events);
        if self.peer.is_none() && conn.phase() == &Phase::Linked && !self.establish(conn.peer(), conn.path(), now.ms) {
            return false;
        }
        if let Some(peer) = self.peer {
            receive_messages(conn, peer, &mut self.traffic, &mut self.events);
        }
        let status = match conn.phase() {
            Phase::Punching { .. } if self.tags.is_some() => {
                let next = conn.next_wake();
                let delay = next.saturating_sub(now.ms);
                if delay == connect::RECONNECT_IDLE_INTERVAL_MS {
                    // An unanswered reconnect keeps its nonce and sends one quiet probe per interval.
                    Status::retry_after(now, delay)
                } else if let Some((_, status)) = waiting.filter(|(due, _)| *due == next && next > now.ms) {
                    // Another host observation woke the slot before its probe was due.
                    status
                } else {
                    Status::Reconnecting
                }
            }
            Phase::Punching { since } => Status::Reaching { secs: now.ms.saturating_sub(*since) / 1000 },
            Phase::Securing => {
                self.phase = SlotPhase::Proving;
                Status::Securing
            }
            Phase::Proving => {
                self.phase = SlotPhase::Proving;
                let until = self.prove_until.get_or_insert_with(|| now.ms.saturating_add(PROVE_WAIT_MS + 1));
                if now.ms >= *until {
                    self.end(
                        conn,
                        "the other device didn't prove itself: check both sets of words and try again".into(),
                        None,
                        now,
                    );
                    return false;
                }
                Status::Proving
            }
            Phase::Linked => {
                self.prove_until = None;
                Status::Linked
            }
            Phase::Failed(f) if conn.dropped().is_some() || conn.done(now.ms) => {
                let refusal = match f {
                    Failure::Refused(r) => Some(r.clone()),
                    _ => None,
                };
                self.end(conn, describe(f), refusal, now);
                return false;
            }
            Phase::Failed(_) => Status::Proving,
        };
        if waiting.is_some() && matches!(status, Status::Reconnecting | Status::Securing | Status::Proving) {
            if let Some((pin, _)) = self.known {
                self.events.push(Event::Reconnecting(pin.device, now.unix_secs));
            }
        }
        self.show(status);
        if let (Status::Linked, Some(peer)) = (status, self.peer) {
            if let Some(path) = conn.path().filter(|p| Some(*p) != self.told_path) {
                self.told_path = Some(path);
                if let Some((_, known)) = &mut self.known {
                    known.public = path.public.or(known.public);
                    known.lan = path.lan.or(known.lan);
                }
                self.events.push(Event::Path(peer, path));
            }
        }
        true
    }

    fn backoff(&mut self, now: Now, delay: u64) {
        self.phase = SlotPhase::Backoff { retry_at: now.ms.saturating_add(delay) };
        self.show(Status::retry_after(now, delay));
    }

    fn end(&mut self, conn: &mut Connection, why: String, refusal: Option<Refusal>, now: Now) {
        if let Some(Refusal::Version { peer_newest: Some(version) }) = refusal {
            self.events.push(Event::PeerVersion(version));
        }
        if self.known.is_none() {
            self.fail(why);
            return;
        }
        let dropped = conn.dropped().is_some();
        self.restart = conn.take_restart_probe();
        let first_lost = self.first_tag.take().is_some() && self.peer.is_some();
        self.lost(
            format!("{why}, stream {:?}, peer end {:?}, phase {:?}", conn.stream_state(), conn.dropped(), conn.phase()),
            conn.sent_bytes(),
        );
        let delay = match refusal {
            Some(Refusal::KeyChanged) => {
                self.finish();
                self.events.push(Event::KeyChanged);
                return;
            }
            Some(Refusal::Version { .. }) => {
                self.events.push(Event::Refused(why));
                connect::RECONNECT_IDLE_INTERVAL_MS
            }
            Some(_) => {
                self.events.push(Event::Refused(GATE_REFUSED.into()));
                connect::RECONNECT_IDLE_INTERVAL_MS
            }
            None if dropped || first_lost => 0,
            None => RETRY_MS,
        };
        self.backoff(now, delay);
    }
}
/// When words replace a linked device or it is wanted again during its farewell, only
/// the old stream can still address this generation. It owns no application traffic.
struct Farewell {
    conn: Connection,
    until: u64,
    wake: u64,
}

#[derive(Default)]
struct TagTable(BTreeMap<[u8; 8], Vec<Carrier>>);

impl TagTable {
    fn insert(&mut self, tag: [u8; 8], carrier: Carrier) {
        let choices = self.0.entry(tag).or_default();
        if !choices.contains(&carrier) {
            choices.push(carrier);
        }
    }

    fn targets(&self, tag: &[u8; 8]) -> &[Carrier] {
        self.0.get(tag).map(Vec::as_slice).unwrap_or_default()
    }
}

/// All stream datagrams share this credit while a call reserves the uplink. An idle
/// period earns at most one sync batch. One packet may start with positive credit.
struct CallPace {
    bps: u64,
    credit: i128,
    at: u64,
    last: Option<Attempt>,
}

impl CallPace {
    fn refill(&mut self, now: u64) {
        let gain = i128::from(now.saturating_sub(self.at)).saturating_mul(i128::from(self.bps));
        // Keep the burst plus the last whole UDP datagram within one batch.
        let burst = (crate::sync::BATCH_BYTES - 2048) as i128 * 8000;
        self.credit = self.credit.saturating_add(gain).min(burst);
        self.at = now;
    }

    fn ready(&self) -> u64 {
        let debt = self.credit.saturating_neg().max(0) as u128;
        self.at.saturating_add(debt.div_ceil(u128::from(self.bps)).min(u128::from(u64::MAX)) as u64)
    }

    fn charge(&mut self, at: Attempt, bytes: u64) {
        if bytes > 0 {
            self.credit -= i128::from(bytes) * 8000;
            self.last = Some(at);
        }
    }
}

#[derive(Default)]
struct SyncInbox {
    pending: VecDeque<Output>,
    in_flight: Option<(Attempt, u64)>,
    // Includes the message currently held by the UI.
    cost: u64,
}

#[derive(Default)]
struct SyncInboxes {
    carriers: BTreeMap<Carrier, SyncInbox>,
    ready: VecDeque<Carrier>,
}

impl SyncInboxes {
    fn waiting_for_predecessor(&self, at: Attempt) -> bool {
        self.carriers.get(&at.carrier).and_then(|i| i.pending.front()).is_some_and(|o| o.attempt != at)
    }

    fn credit(&self, at: Attempt) -> u64 {
        if self.waiting_for_predecessor(at) { return 0 }
        (crate::sync::BATCH_BYTES as u64).saturating_sub(self.carriers.get(&at.carrier).map_or(0, |i| i.cost))
    }

    fn push(&mut self, at: Attempt, event: Event, output: &mut Vec<Output>) {
        if let Event::Sync(body) = &event {
            let inbox = self.carriers.entry(at.carrier).or_default();
            if inbox.in_flight.is_none() && inbox.pending.is_empty() {
                self.ready.push_back(at.carrier);
            }
            inbox.cost += connect::sync_cost(body);
            inbox.pending.push_back(Output { attempt: at, event });
            return
        }
        // The transport, including punching a replacement, never waits on the UI.
        // Preserve the last Sync -> Lost -> replacement Linked order on its event channel.
        match event {
            Event::Send(..) | Event::Reflect(..) | Event::Punch | Event::OpenInvite { .. }
                | Event::CheckAddress { .. } | Event::Media(..) => {}
            Event::CallReady | Event::Blob(_) if !self.waiting_for_predecessor(at) => {}
            _ => {
                if let Some(inbox) = self.carriers.get_mut(&at.carrier).filter(|i| !i.pending.is_empty()) {
                    inbox.pending.push_back(Output { attempt: at, event });
                    return
                }
            }
        }
        output.push(Output { attempt: at, event });
    }

    // A ready carrier takes one turn, then rejoins at the tail when the UI returns its credit.
    fn hand_over(&mut self, key: Carrier, output: &mut Vec<Output>) {
        let inbox = self.carriers.get_mut(&key).unwrap();
        while let Some(next) = inbox.pending.front() {
            if let Event::Sync(body) = &next.event {
                if inbox.in_flight.is_some() { break }
                inbox.in_flight = Some((next.attempt, connect::sync_cost(body)));
            }
            output.push(inbox.pending.pop_front().unwrap());
        }
    }

    fn consumed(&mut self, at: Attempt) -> bool {
        let Some(inbox) = self.carriers.get_mut(&at.carrier) else { return false };
        let Some((_, cost)) = inbox.in_flight.filter(|(owner, _)| *owner == at) else { return false };
        inbox.cost -= cost;
        inbox.in_flight = None;
        if inbox.pending.is_empty() {
            self.carriers.remove(&at.carrier);
        } else {
            self.ready.push_back(at.carrier);
        }
        true
    }
}

/// One UDP port. Wanted peers each own one slot alongside the optional words slot. Retiring streams remain reachable solely by their existing stream ID.
pub struct Hub {
    device: Arc<DeviceKey>,
    slots: BTreeMap<Carrier, Slot>,
    tags: TagTable,
    streams: BTreeMap<[u8; 8], Attempt>,
    farewells: BTreeMap<Attempt, Farewell>,
    retired_inbox: BTreeMap<Attempt, PeerTraffic>,
    sync_inboxes: SyncInboxes,
    attached: BTreeSet<Attempt>,
    generation: u64,
    hour: Option<u64>,
    hour_wake: u64,
    reflector: Option<(Attempt, Query, u64)>,
    events: Vec<Output>,
    due: Vec<Attempt>,
    pace: Option<CallPace>,
    stream_bytes: u64,
}

impl Hub {
    pub fn new(device: Arc<DeviceKey>) -> Self {
        Self {
            device,
            slots: BTreeMap::new(),
            tags: TagTable::default(),
            streams: BTreeMap::new(),
            farewells: BTreeMap::new(),
            retired_inbox: BTreeMap::new(),
            sync_inboxes: SyncInboxes::default(),
            attached: BTreeSet::new(),
            generation: 0,
            hour: None,
            hour_wake: 0,
            reflector: None,
            events: Vec::new(),
            due: Vec::new(),
            pace: None,
            stream_bytes: 0,
        }
    }

    fn fresh(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    pub fn attempt(&self, carrier: Carrier) -> Option<Attempt> {
        self.slots.get(&carrier).map(|s| Attempt { carrier, generation: s.generation })
    }

    fn current(&self, at: Attempt) -> Option<&Slot> {
        self.slots.get(&at.carrier).filter(|s| s.generation == at.generation)
    }

    fn current_mut(&mut self, at: Attempt) -> Option<&mut Slot> {
        self.slots.get_mut(&at.carrier).filter(|s| s.generation == at.generation)
    }

    fn command(&mut self, at: Attempt) -> Option<&mut Slot> {
        let s = self.current_mut(at).filter(|s| s.wanted && s.phase != SlotPhase::Stopped)?;
        s.wake = Some(0);
        Some(s)
    }

    pub fn phase(&self, key: Carrier) -> Option<SlotPhase> {
        self.slots.get(&key).map(|s| s.phase)
    }

    fn configure_tags(slot: &mut Slot, hour: u64) {
        let Some((pin, _)) = slot.known else { return };
        if let Some((key, tags)) = &mut slot.tags {
            if tags.hour == hour {
                return;
            }
            *tags = ProbeTags::from_key(key, hour);
        } else {
            match slot.device.link_secret(&pin.device) {
                Ok(secret) => {
                    let key = ProbeKey::new(&secret);
                    let tags = ProbeTags::from_key(&key, hour);
                    slot.tags = Some((key, tags));
                }
                Err(e) => {
                    slot.fail(format!("no link secret: {e}"));
                    return;
                }
            }
        }
        if let (Some(c), Some((_, tags))) = (&mut slot.conn, &slot.tags) {
            c.set_probe_tags(tags);
        }
    }

    fn rebuild_tags(&mut self) {
        self.tags.0.clear();
        for (&key, s) in &self.slots {
            if !s.wanted || s.phase == SlotPhase::Stopped {
                continue;
            }
            match key {
                Carrier::Words => {
                    if let Some(tag) = s.first_tag {
                        self.tags.insert(tag, key);
                    }
                }
                Carrier::Peer(_) => {
                    if let Some((_, tags)) = &s.tags {
                        for tag in [tags.send, tags.accept[0], tags.accept[1]] {
                            self.tags.insert(tag, key);
                        }
                    }
                }
            }
        }
    }

    /// Rotate the tag index once before routing a batch, including after a wall-clock jump.
    pub fn begin_batch(&mut self, now: Now) {
        let hour = connect::probe_hour(now.unix_secs);
        self.hour_wake = now.ms.saturating_add((3600 - now.unix_secs % 3600) * 1000);
        if self.hour == Some(hour) {
            return;
        }
        self.hour = Some(hour);
        for slot in self.slots.values_mut().filter(|s| s.wanted && s.phase != SlotPhase::Stopped) {
            Self::configure_tags(slot, hour);
        }
        self.rebuild_tags();
    }

    /// Reconcile a complete device snapshot. Unchanged carriers keep their generations.
    pub fn set_wanted(&mut self, devices: impl IntoIterator<Item = (Pin, Remembered)>, now: Now) {
        self.begin_batch(now);
        let wanted: BTreeMap<_, _> = devices.into_iter().map(|(pin, path)| (pin.device, (pin, path))).collect();
        let remove: Vec<_> = self
            .slots
            .iter()
            .filter_map(|(&k, s)| match k {
                Carrier::Peer(peer) if s.wanted && !wanted.contains_key(&peer) => Some(k),
                _ => None,
            })
            .collect();
        let mut changed = !remove.is_empty();
        for key in remove {
            let slot = self.slots.get_mut(&key).unwrap();
            slot.retire_peer("device is no longer wanted".into(), slot.sent_bytes());
            self.close(key, now.ms, true);
        }
        for (peer, (mut pin, mut path)) in wanted {
            let key = Carrier::Peer(peer);
            if let Some(s) = self.slots.get_mut(&key).filter(|s| s.wanted) {
                if let Some((known, memory)) = &mut s.known {
                    path.public = path.public.or(memory.public);
                    path.lan = path.lan.or(memory.lan);
                    // A punch at an obsolete destination can otherwise last forever.
                    let new_target = *memory != path && matches!(s.phase, SlotPhase::Idle | SlotPhase::Punching);
                    *memory = path;
                    if known.min_version >= pin.min_version && !new_target {
                        continue;
                    }
                    pin.min_version = pin.min_version.max(known.min_version);
                }
                self.close(key, now.ms, false);
            }
            self.displace(key);
            let mut s = Slot::new(self.device.clone(), Start::Known { pin, path });
            s.generation = self.fresh();
            Self::configure_tags(&mut s, connect::probe_hour(now.unix_secs));
            self.slots.insert(key, s);
            changed = true;
        }
        if changed {
            self.rebuild_tags();
        }
    }

    pub fn start_words(&mut self, same_network: bool, now: Now) -> Attempt {
        if self.slots.contains_key(&Carrier::Words) {
            self.close(Carrier::Words, now.ms, false);
            self.displace(Carrier::Words);
            self.rebuild_tags();
        }
        self.reflector = None;
        let mut slot = Slot::new(self.device.clone(), Start::Words { same_network });
        slot.generation = self.fresh();
        let at = Attempt { carrier: Carrier::Words, generation: slot.generation };
        slot.show(Status::FindingAddress);
        self.slots.insert(Carrier::Words, slot);
        at
    }

    pub fn cancel_words(&mut self, now: u64) {
        self.close(Carrier::Words, now, false);
        self.rebuild_tags();
    }

    pub fn fail_words(&mut self, at: Attempt, reason: String) {
        if let Some(s) = self.words_slot(at) { s.fail(reason); }
    }

    pub fn has_work(&self) -> bool {
        self.slots.values().any(|s| s.phase != SlotPhase::Stopped) || !self.farewells.is_empty()
    }

    fn archive(&mut self, at: Attempt, traffic: PeerTraffic) {
        if !traffic.incoming.is_empty() {
            self.retired_inbox.insert(at, traffic);
        }
    }

    fn collect(&mut self, at: Attempt, events: Vec<Event>) {
        for event in events {
            self.sync_inboxes.push(at, event, &mut self.events);
        }
        self.refresh_read_credit(at.carrier);
    }

    fn refresh_read_credit(&mut self, key: Carrier) {
        let Some(slot) = self.slots.get_mut(&key) else { return };
        if let Some(c) = &mut slot.conn {
            let at = Attempt { carrier: key, generation: slot.generation };
            if c.set_read_credit(self.sync_inboxes.credit(at)) { slot.wake = Some(0); }
        }
    }

    pub fn awaiting_sync(&self) -> bool { !self.sync_inboxes.carriers.is_empty() }

    /// Called once after the UI consumes a delivered Sync, including one from a retired attempt.
    pub fn sync_read(&mut self, at: Attempt) {
        if self.sync_inboxes.consumed(at) { self.refresh_read_credit(at.carrier); }
    }

    fn close(&mut self, key: Carrier, now: u64, drain_sync: bool) {
        let Some(at) = self.attempt(key) else { return };
        self.attached.remove(&at);
        let s = self.slots.get_mut(&key).unwrap();
        let id = s.conn.as_ref().and_then(Connection::stream_id);
        s.stop(now, drain_sync);
        s.wanted = false;
        s.wake = Some(0);
        let events = s.take_events();
        let stopped = s.phase == SlotPhase::Stopped;
        self.collect(at, events);
        if stopped {
            let s = self.slots.remove(&key).unwrap();
            self.archive(at, s.traffic);
            self.unregister(id, at);
        }
        if key == Carrier::Words {
            self.reflector = None;
        }
    }

    fn displace(&mut self, key: Carrier) {
        let Some(mut s) = self.slots.remove(&key) else { return };
        let at = Attempt { carrier: key, generation: s.generation };
        self.attached.remove(&at);
        s.retire_peer("replaced by a new link".into(), s.sent_bytes());
        self.collect(at, s.take_events());
        self.archive(at, std::mem::take(&mut s.traffic));
        if let (SlotPhase::Closing { until }, Some(conn)) = (s.phase, s.conn.take()) {
            self.farewells.insert(at, Farewell { conn, until, wake: 0 });
        }
    }

    pub fn stop(&mut self, now: u64) {
        let keys: Vec<_> = self.slots.keys().copied().collect();
        for key in keys {
            self.close(key, now, false);
        }
        for (at, fin) in std::mem::take(&mut self.farewells) {
            self.unregister(fin.conn.stream_id(), at);
        }
        self.rebuild_tags();
    }

    fn unregister(&mut self, id: Option<[u8; 8]>, at: Attempt) {
        if let Some(id) = id {
            if self.streams.get(&id) == Some(&at) {
                self.streams.remove(&id);
            }
        }
    }

    /// Claim an ID immediately, before another datagram in this batch can use it.
    fn sync_id(&mut self, at: Attempt, before: Option<[u8; 8]>) {
        let after = self.current(at).and_then(|s| s.conn.as_ref()).and_then(Connection::stream_id);
        if before == after {
            return;
        }
        self.unregister(before, at);
        let Some(id) = after else { return };
        if self.streams.get(&id).is_some_and(|owner| *owner != at) {
            let generation = self.fresh();
            let s = self.current_mut(at).unwrap();
            s.conn = None;
            s.phase = SlotPhase::Punching;
            s.prove_until = None;
            s.restart = None;
            s.generation = generation;
            s.repunch = Repunch::NewNonce;
            s.events.push(Event::Punch);
            s.wake = Some(0);
        } else {
            self.streams.insert(id, at);
        }
    }

    fn promote(&mut self, now: u64) -> Option<Attempt> {
        let peer = self.slots.get(&Carrier::Words)?.peer?;
        let key = Carrier::Peer(peer);
        if self.slots.get(&key).is_some_and(|s| s.wanted) {
            self.close(key, now, false);
        }
        self.displace(key);
        let mut s = self.slots.remove(&Carrier::Words).unwrap();
        let at = Attempt { carrier: key, generation: s.generation };
        if let Some(id) = s.conn.as_ref().and_then(Connection::stream_id) {
            self.streams.insert(id, at);
        }
        Self::configure_tags(&mut s, self.hour.unwrap_or(0));
        s.wake = Some(0);
        self.slots.insert(key, s);
        self.refresh_read_credit(key);
        self.reflector = None;
        self.rebuild_tags();
        Some(at)
    }

    fn handle_link(&mut self, at: Attempt, from: SocketAddr, bytes: &[u8], now: u64) {
        if let Some(s) = self.current_mut(at) {
            if matches!(s.phase, SlotPhase::Closing { until } if now >= until) { return; }
            let before = s.conn.as_ref().and_then(Connection::stream_id);
            s.handle_link(from, bytes, now);
            s.wake = Some(0);
            self.sync_id(at, before);
            if at.carrier == Carrier::Words {
                self.promote(now);
            }
        } else if let Some(fin) = self.farewells.get_mut(&at).filter(|f| now < f.until) {
            fin.conn.handle(from, bytes, now);
            fin.wake = 0;
        }
    }

    pub fn handle(&mut self, from: SocketAddr, bytes: &[u8], now: u64) {
        let media_at = if Connection::is_call(bytes) {
            self.attached.iter().copied().find(|&at| self.media_address(at) == Some(from))
        } else {
            None
        };
        match route(bytes, from, media_at.map(|_| from)) {
            Datagram::Reflect => {
                if let Some((_, q, _)) = &mut self.reflector {
                    q.handle(from, bytes);
                }
                if let Some(s) = self.slots.get_mut(&Carrier::Words) {
                    if let Some(w) = &mut s.watch {
                        if let Some(changed) = w.observation(from, bytes) {
                            if changed {
                                let before = s.conn.as_ref().and_then(Connection::stream_id);
                                let at = Attempt { carrier: Carrier::Words, generation: s.generation };
                                s.words_changed();
                                s.wake = Some(0);
                                self.sync_id(at, before);
                                return;
                            } else if w.confirm_until.take().is_some() && s.phase == SlotPhase::Punching {
                                s.events.push(Event::Punch);
                            }
                            s.wake = Some(0);
                        }
                    }
                }
            }
            Datagram::Link if bytes.first() == Some(&makepad_network::p2p::punch::KIND) => {
                let Some(tag) = Connection::punch_tag(bytes) else { return };
                match self.tags.targets(&tag) {
                    [] => {}
                    &[key] => {
                        if let Some(at) = self.attempt(key) {
                            self.handle_link(at, from, bytes, now);
                        }
                    }
                    // Promotion can change the table while delivering a matching probe.
                    list => {
                        let targets: Vec<_> = list.iter().filter_map(|&key| self.attempt(key)).collect();
                        for at in targets {
                            self.handle_link(at, from, bytes, now);
                        }
                    }
                }
            }
            Datagram::Call => {
                if let Some(at) = media_at {
                    self.events.push(Output { attempt: at, event: Event::Media(from, bytes.to_vec()) });
                }
            }
            Datagram::Drop => {}
            Datagram::Link => {
                if let Some(at) = Connection::packet_stream_id(bytes).and_then(|id| self.streams.get(&id)).copied() {
                    self.handle_link(at, from, bytes, now);
                }
            }
        }
    }

    pub fn punch(&mut self, at: Attempt, random: [u8; 72], now: Now) {
        let budget = self.pace.as_ref().map(|_| 0);
        if let Some(s) = self.command(at) {
            let before = s.conn.as_ref().and_then(Connection::stream_id);
            s.punch(random, now);
            if let Some(c) = &mut s.conn { c.transmit_turn(budget, 0); }
            self.refresh_read_credit(at.carrier);
            self.sync_id(at, before);
        }
    }

    pub fn words(&mut self, at: Attempt, own: Invite, peer: Invite, name: String, now: u64) {
        if let Some(s) = self.words_slot(at) {
            s.words(own, peer, name, now);
            if let Some(tag) = s.first_tag {
                self.tags.insert(tag, Carrier::Words);
            }
        }
    }

    /// Accept host results only for the words attempt that requested them.
    fn words_slot(&mut self, at: Attempt) -> Option<&mut Slot> {
        if at.carrier != Carrier::Words {
            return None;
        }
        self.command(at)
    }

    pub fn wait_for_words(&mut self, at: Attempt, session: InviteSession, own: Invite, watch: Option<AddressWatch>) {
        if let Some(s) = self.words_slot(at).filter(|s| s.phase == SlotPhase::Idle) {
            s.words = Some(Words { session, own, matched: None });
            s.watch = watch;
            s.show(Status::WaitingForWords);
        }
    }

    pub fn open_invite(
        &mut self,
        at: Attempt,
        mut session: InviteSession,
        server: SocketAddr,
        addr: SocketAddrV4,
        hint: Hint,
        lan: Option<SocketAddrV4>,
        opened: u64,
    ) {
        let Some(slot) = self.words_slot(at).filter(|s| s.phase == SlotPhase::Idle) else { return };
        let Some(key) = session.key(opened) else {
            slot.fail("No current invite key.".into());
            return;
        };
        let lan = lan.filter(|a| makepad_network::p2p::words::lan_candidate_ok(*a));
        let own = Invite { nat: hint, addr, lan, commit: Invite::commitment(hint, addr, lan, &key.public()) };
        slot.events.push(Event::Invite(own.clone()));
        let watch = AddressWatch::new(server, addr, lan, slot.same_network, opened);
        self.wait_for_words(at, session, own, Some(watch));
    }

    pub fn check_address(&mut self, at: Attempt, lan: Option<SocketAddrV4>, tx: [u8; 12]) {
        if let Some(s) = self.words_slot(at) {
            let id = s.conn.as_ref().and_then(Connection::stream_id);
            s.check_address(lan, tx);
            self.sync_id(at, id);
        }
    }

    pub fn reflecting(&self) -> bool {
        self.reflector.is_some()
    }
    pub fn reflect(&mut self, at: Attempt, q: Query, now: u64) {
        if at.carrier == Carrier::Words && self.current(at).is_some() {
            self.reflector = Some((at, q, now.saturating_add(3_000)));
        }
    }

    fn poll_reflector(&mut self, now: u64) {
        let Some((at, q, until)) = &mut self.reflector else { return };
        let at = *at;
        if !q.done() && now < *until {
            self.events
                .extend(q.poll(now).into_iter().map(|(to, b)| Output { attempt: at, event: Event::Reflect(to, b) }));
            return;
        }
        let result = q.result();
        self.reflector = None;
        let Some(s) = self.command(at) else { return };
        let reflected = match result.mapped() {
            Some(SocketAddr::V4(a)) => Some((a, result.mapping())),
            _ => None,
        };
        let choice = discover::choose(&Found { router: None, reflected });
        if let Some((addr, server)) = choice.address.and_then(|addr| {
            result.answers.iter().find(|(_, seen)| *seen == Some(addr.into())).map(|(server, _)| (addr, *server))
        }) {
            s.events.push(Event::OpenInvite { server, addr, hint: choice.hint, include_lan: s.same_network });
        } else {
            s.fail("couldn't learn this machine's public address: is it online?".into());
        }
    }

    fn after_turn(&mut self, at: Attempt, before: Option<[u8; 8]>, had_conn: bool, now: u64, events: Vec<Event>) {
        self.sync_id(at, before);
        let output_at = if at.carrier == Carrier::Words { self.promote(now).unwrap_or(at) } else { at };
        self.collect(output_at, events);
        let Some(s) = self.current_mut(output_at) else { return };
        s.wake = s.next_wake();
        if had_conn && s.conn.is_none() {
            let traffic = std::mem::take(&mut s.traffic);
            let retry = matches!(s.phase, SlotPhase::Backoff { .. } | SlotPhase::Punching);
            self.archive(output_at, traffic);
            self.attached.remove(&output_at);
            if retry {
                let generation = self.fresh();
                self.current_mut(output_at).unwrap().generation = generation;
            }
        }
        if let Some(s) = self.current(output_at).filter(|s| s.phase == SlotPhase::Stopped) {
            if !s.wanted || output_at.carrier == Carrier::Words {
                self.slots.remove(&output_at.carrier);
            }
            self.rebuild_tags();
        }
    }

    /// Service each dirty or due slot once. Only overdue streams may need another
    /// receive batch before evaluating their idle timeout.
    pub fn poll(&mut self, now: Now, batch: BatchEnd) -> Vec<Output> {
        self.begin_batch(now);
        self.poll_reflector(now.ms);
        let mut due = std::mem::take(&mut self.due);
        due.extend(self.slots.iter().filter(|(_, s)| s.wake.is_some_and(|t| t <= now.ms))
            .map(|(&carrier, s)| Attempt { carrier, generation: s.generation }));
        due.extend(self.farewells.iter().filter(|(_, f)| f.wake <= now.ms).map(|(&at, _)| at));
        if self.attached.is_empty() && self.pace.is_some() { self.set_call_cap(None, now.ms); }
        if let Some(pace) = &mut self.pace {
            pace.refill(now.ms);
            if let Some(last) = pace.last {
                due.sort_unstable();
                let start = due.partition_point(|a| *a <= last);
                due.rotate_left(start);
            }
        }
        for at in due.drain(..) {
            let budget = self.pace.as_ref().map(|p| usize::from(p.credit >= 0));
            if self.current(at).is_some() {
                let s = self.slots.get_mut(&at.carrier).unwrap();
                if batch == BatchEnd::MoreQueued && s.phase == SlotPhase::Up && s.idle_due(now.ms) { continue }
                let before = s.conn.as_ref().and_then(Connection::stream_id);
                let had_conn = s.conn.is_some();
                if let Some(c) = &mut s.conn { c.transmit_turn(budget, 0); }
                let events = s.poll(now);
                let bytes = events.iter().filter_map(|e| match e {
                    Event::Send(_, b) if Connection::packet_stream_id(b).is_some() => Some(b.len() as u64),
                    _ => None,
                }).sum();
                self.account_stream(at, bytes);
                let after = self.pace.as_ref().map_or(0, CallPace::ready);
                if let Some(c) = self.slots.get_mut(&at.carrier).and_then(|s| s.conn.as_mut()) {
                    c.transmit_turn(budget.map(|_| 0), after);
                }
                self.after_turn(at, before, had_conn, now.ms, events);
            } else if let Some(fin) = self.farewells.get_mut(&at) {
                fin.conn.transmit_turn(budget, 0);
                let mut sent = 0;
                if now.ms < fin.until {
                    for (to, bytes) in fin.conn.poll(now.ms) {
                        if Connection::packet_stream_id(&bytes).is_some() {
                            sent += bytes.len() as u64;
                            self.events.push(Output { attempt: at, event: Event::Send(to, bytes) });
                        }
                    }
                }
                if fin.conn.close_done() || now.ms >= fin.until {
                    if let Some(id) = fin.conn.stream_id() {
                        if self.streams.get(&id) == Some(&at) { self.streams.remove(&id); }
                    }
                    self.farewells.remove(&at);
                }
                self.account_stream(at, sent);
                if let Some(fin) = self.farewells.get_mut(&at) {
                    fin.conn.transmit_turn(budget.map(|_| 0), self.pace.as_ref().map_or(0, CallPace::ready));
                    fin.wake = fin.until.min(fin.conn.next_wake());
                }
            }
        }
        self.due = due;
        self.take_events()
    }

    pub fn take_events(&mut self) -> Vec<Output> {
        while let Some(key) = self.sync_inboxes.ready.pop_front() {
            self.sync_inboxes.hand_over(key, &mut self.events);
            self.refresh_read_credit(key);
        }
        std::mem::take(&mut self.events)
    }

    pub fn next_wake(&self) -> Option<u64> {
        if !self.events.is_empty() || !self.sync_inboxes.ready.is_empty() {
            return Some(0);
        }
        self.slots
            .values()
            .filter_map(|s| s.wake)
            .chain(self.farewells.values().map(|f| f.wake))
            .chain(self.reflector.as_ref().map(|(_, q, until)| q.next_wake().unwrap_or(0).min(*until)))
            .chain((!self.tags.0.is_empty()).then_some(self.hour_wake))
            .min()
    }

    pub fn starting_carrier(&self, key: Carrier, now: u64) -> bool {
        self.slots.get(&key).is_some_and(|s| s.starting_carrier(now))
    }

    pub fn socket_lost(&mut self, now: Now) {
        self.attached.clear();
        self.farewells.clear();
        self.streams.clear();
        let keys: Vec<_> = self.slots.keys().copied().collect();
        for key in keys {
            let generation = self.fresh();
            let s = self.slots.get_mut(&key).unwrap();
            if s.phase == SlotPhase::Stopped {
                continue;
            }
            let at = Attempt { carrier: key, generation: s.generation };
            s.socket_lost(now);
            let events = s.take_events();
            let traffic = std::mem::take(&mut s.traffic);
            let stopped = s.phase == SlotPhase::Stopped;
            // Even a pending host request for a punch or an observation belongs to
            // the old socket, whether or not its Connection had been built yet.
            s.generation = generation;
            s.wake = Some(0);
            self.collect(at, events);
            self.archive(at, traffic);
            if stopped {
                self.slots.remove(&key);
            }
        }
        self.rebuild_tags();
    }
    pub fn rebound(&mut self, now: u64) {
        for s in self.slots.values_mut().filter(|s| s.wanted && s.phase != SlotPhase::Stopped) {
            s.rebound(now);
            s.wake = Some(0);
        }
    }

    pub fn send_peer(&mut self, peer: PublicKey, topic: Topic, body: Vec<u8>) -> Result<(), Vec<u8>> {
        let Some(s) =
            self.slots.get_mut(&Carrier::Peer(peer)).filter(|s| s.phase == SlotPhase::Up && s.peer == Some(peer))
        else {
            return Err(body);
        };
        s.wake = Some(0);
        s.traffic.enqueue(topic, body)
    }

    pub fn take_peer(&mut self, topic: Topic) -> Vec<(PublicKey, Vec<u8>)> {
        let mut ready = Vec::new();
        self.retired_inbox.retain(|_, traffic| {
            ready.extend(traffic.take(topic));
            !traffic.incoming.is_empty()
        });
        for slot in self.slots.values_mut() {
            ready.extend(slot.traffic.take(topic));
        }
        ready
    }

    pub fn take_call_frames(&mut self, at: Attempt) -> Vec<(PublicKey, Vec<u8>)> {
        if let Some(mut old) = self.retired_inbox.remove(&at) {
            return old.take_call_frames().collect();
        }
        self.current_mut(at).map(|s| s.take_call_frames().collect()).unwrap_or_default()
    }

    pub fn send_sync(&mut self, at: Attempt, bytes: &[u8]) {
        if let Some(s) = self.command(at) {
            s.send(Kind::Sync, bytes);
        }
    }
    pub fn send_blob(&mut self, at: Attempt, bytes: &[u8]) {
        if let Some(s) = self.command(at) {
            s.send(Kind::Blob, bytes);
        }
    }
    pub fn writable(&self, at: Attempt, kind: Kind) -> bool {
        self.current(at).is_some_and(|s| s.phase == SlotPhase::Up && s.writable(kind))
            && !self.sync_inboxes.waiting_for_predecessor(at)
    }
    pub fn media_address(&self, at: Attempt) -> Option<SocketAddr> {
        self.current(at).filter(|s| s.phase == SlotPhase::Up)?.media_address()
    }
    pub fn attach_call(&mut self, at: Attempt, attached: bool) {
        if attached && self.current(at).is_some_and(|s| s.phase == SlotPhase::Up) {
            self.attached.insert(at);
        } else if !attached {
            self.attached.remove(&at);
        }
    }
    pub fn set_call_cap(&mut self, cap: Option<u64>, now: u64) {
        let cap = cap.filter(|&bps| bps > 0);
        if self.pace.as_ref().map(|p| p.bps) == cap { return }
        if let (Some(bps), Some(pace)) = (cap, &mut self.pace) {
            pace.refill(now);
            pace.bps = bps;
        } else {
            self.pace = cap.map(|bps| CallPace { bps, credit: 0, at: now, last: None });
        }
        for s in self.slots.values_mut() {
            if let Some(c) = &mut s.conn { c.transmit_turn(cap.map(|_| 0), 0); s.wake = Some(0); }
        }
        for f in self.farewells.values_mut() { f.conn.transmit_turn(cap.map(|_| 0), 0); f.wake = 0; }
    }

    fn account_stream(&mut self, at: Attempt, bytes: u64) {
        self.stream_bytes = self.stream_bytes.saturating_add(bytes);
        if let Some(pace) = &mut self.pace { pace.charge(at, bytes); }
    }

    pub fn stream_bytes(&self) -> u64 { self.stream_bytes }

}

pub const GATE_REFUSED: &str = "Couldn't prove this device. It may have been reset, or it removed this link.";
pub const VERSION_TEXT: &str = "Update Makepad Town on both computers, then try again.";
fn refusal_text(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Version { .. } | Refusal::Format(_) => VERSION_TEXT.into(),
        Refusal::Gate | Refusal::Signature | Refusal::Commitment => {
            "The other device couldn't be proven. Check both sets of words. If they're right, someone may be between you.".into()
        }
        Refusal::KeyChanged => "The other device's key is not the one you linked with before. Link again with fresh words only if you know it changed.".into(),
        Refusal::SelfLink => "Those are this device's own words. Read the other device's words instead.".into(),
        Refusal::Candidates => "Town couldn't tell which device this is. Start again.".into(),
        Refusal::Crypto(_) => "This device couldn't make the keys for the link. Start again, and restart Town if it keeps happening.".into(),
    }
}

pub fn describe(failure: &Failure) -> String {
    match failure {
        Failure::Refused(refusal) => refusal_text(refusal),
        Failure::Tls(_) => "The secure channel to the other device couldn't be set up. Check that both devices run the same version of Town, then start again.".into(),
        Failure::Stream(_) => "The connection to the other device dropped. Check that both devices are online, then start again.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_device_reports_backoff_reaching_and_stopped() {
        let ana = Arc::new(DeviceKey::generate().unwrap());
        let ben = DeviceKey::generate().unwrap();
        let peer = ben.public();
        let addr: SocketAddr = "192.0.2.2:4200".parse().unwrap();
        let pin = Pin { device: peer, min_version: crate::link::VERSION };
        let mut hub = Hub::new(ana);
        let time = |ms| Now { ms, unix_secs: 3600 + ms / 1000 };
        hub.set_wanted([(pin, Remembered { public: Some(addr), lan: None })], time(0));
        let at = hub.attempt(Carrier::Peer(peer)).unwrap();
        let start = hub.poll(time(0), BatchEnd::Drained);
        assert!(start.iter().any(|e| e.attempt == at && matches!(e.event, Event::Punch)));
        hub.punch(at, [7; 72], time(0));
        hub.poll(time(0), BatchEnd::Drained);

        // Nobody answers the fast probes. The advertised retry matches the actual next probe.
        let now = time(90_000);
        let quiet = hub.poll(now, BatchEnd::Drained);
        let next = hub.next_wake().unwrap();
        assert_eq!(next - now.ms, connect::RECONNECT_IDLE_INTERVAL_MS);
        assert!(quiet.iter().any(|e| e.attempt == at && matches!(e.event,
            Event::Phase(Status::Backoff { retry_at }) if retry_at == time(next).unix_secs)));

        // Ben sends a probe on the remembered path before that retry.
        let tag = quiet.iter().find_map(|e| match &e.event {
            Event::Send(_, bytes) => Connection::punch_tag(bytes),
            _ => None,
        }).expect("the quiet probe");
        let mut punch = makepad_network::p2p::punch::Punch::new(
            makepad_network::p2p::punch::Config::new(tag), addr, [8; 8], now.ms,
        );
        let returning = time(now.ms + 1000);
        let probe = punch.poll(returning.ms).0.pop().unwrap().1;
        hub.handle(addr, &probe, returning.ms);
        let resumed = hub.poll(returning, BatchEnd::Drained);
        assert!(resumed.iter().any(|e| e.attempt == at && matches!(e.event,
            Event::Reconnecting(key, since) if key == peer && since == returning.unix_secs)));
        hub.set_wanted([], returning);
        let removed = hub.poll(time(next), BatchEnd::Drained);
        assert!(removed.iter().any(|e| e.attempt == at && matches!(e.event, Event::Phase(Status::Stopped))));
        assert!(hub.attempt(at.carrier).is_none());
        assert!(!removed.iter().any(|e| matches!(e.event, Event::Punch | Event::Reconnecting(..) | Event::Send(..))));
    }

    #[test]
    fn stop_in_closing_or_backoff_needs_no_clock_or_ack() {
        let device = Arc::new(DeviceKey::generate().unwrap());
        let other = Arc::new(DeviceKey::generate().unwrap());
        let addr: SocketAddr = "192.0.2.1:4100".parse().unwrap();
        let remote: SocketAddr = "192.0.2.2:4200".parse().unwrap();
        let start = |peer, addr| Start::Known {
            pin: Pin { device: peer, min_version: crate::link::VERSION },
            path: Remembered { public: Some(addr), lan: None },
        };
        let ak = Carrier::Peer(other.public());
        let bk = Carrier::Peer(device.public());
        let mut now = Now { ms: 0, unix_secs: 3600 };
        let make = |me: Arc<DeviceKey>, peer, addr| {
            let mut h = Hub::new(me);
            if let Start::Known { pin, path } = start(peer, addr) {
                h.set_wanted([(pin, path)], now);
            }
            h
        };
        let mut a = make(device.clone(), other.public(), remote);
        let mut b = make(other.clone(), device.public(), addr);
        fn turn(sender: &mut Hub, receiver: &mut Hub, from: SocketAddr, seed: u8, now: Now) {
            for out in sender.poll(now, BatchEnd::Drained) {
                match out.event {
                    Event::Punch => sender.punch(out.attempt, [seed; 72], now),
                    Event::Send(_, bytes) => receiver.handle(from, &bytes, now.ms),
                    Event::Failed(why) => panic!("link failed: {why}"),
                    _ => {}
                }
            }
        }
        let advance = |a: &mut Hub, b: &mut Hub, now: &mut Now| {
            assert!(now.ms < 20_000, "both slots make progress");
            turn(a, b, addr, 7, *now);
            turn(b, a, remote, 8, *now);
            now.ms += 1;
        };
        while a.phase(ak).unwrap_or(SlotPhase::Stopped) != SlotPhase::Up
            || b.phase(bk).unwrap_or(SlotPhase::Stopped) != SlotPhase::Up
        {
            advance(&mut a, &mut b, &mut now);
        }
        assert!(a.send_peer(device.public(), Topic::Opaque(2), vec![1]).is_err(), "a send must name the proved peer");
        let body = other.public().to_vec();
        a.send_peer(other.public(), Topic::Opaque(2), body.clone()).unwrap();
        loop {
            advance(&mut a, &mut b, &mut now);
            assert!(b.take_peer(Topic::Opaque(1)).is_empty(), "taking another topic leaves this one in place");
            let arrived = b.take_peer(Topic::Opaque(2));
            if !arrived.is_empty() {
                assert_eq!(arrived, [(device.public(), body)], "only the authenticated stream supplies the sender");
                break;
            }
        }
        a.send_peer(other.public(), Topic::Opaque(1), vec![0; connect::MAX_CALL]).unwrap();
        a.stop(now.ms);
        assert!(matches!(a.phase(ak).unwrap_or(SlotPhase::Stopped), SlotPhase::Closing {until} if until > now.ms));
        assert!(
            a.poll(now, BatchEnd::Drained).iter().any(|e| matches!(e.event, Event::Send(..))),
            "the normal close is sent immediately"
        );
        a.stop(now.ms);
        assert_eq!(
            a.phase(ak).unwrap_or(SlotPhase::Stopped),
            SlotPhase::Stopped,
            "Stop cancels an unacknowledged farewell"
        );
        let unsent: Vec<_> = a
            .poll(now, BatchEnd::Drained)
            .into_iter()
            .filter_map(|e| match e.event {
                Event::PeerUndelivered(u) => Some(u),
                _ => None,
            })
            .collect();
        assert_eq!(
            unsent,
            [PeerUndelivered { peer: other.public(), topic: Topic::Opaque(1), count: 1 }],
            "count whole messages"
        );
        now.ms += 90_000;
        let idle = now.ms;
        while b.phase(bk).unwrap_or(SlotPhase::Stopped) == SlotPhase::Up {
            b.poll(now, BatchEnd::Drained);
            now.ms += 1;
            assert!(now.ms - idle < 1_000, "the stream reports the unchecked idle deadline");
        }
        assert!(
            matches!(b.phase(bk).unwrap_or(SlotPhase::Stopped), SlotPhase::Backoff {retry_at} if retry_at > now.ms),
            "idle expiry schedules a retry"
        );
        b.stop(now.ms);
        assert_eq!(b.phase(bk).unwrap_or(SlotPhase::Stopped), SlotPhase::Stopped);
        for h in [&mut a, &mut b] {
            assert!(!h.poll(now, BatchEnd::Drained).iter().any(|e| matches!(e.event, Event::Punch | Event::Send(..))));
            assert!(h.next_wake().is_none());
        }
    }
}

#[cfg(test)]
#[path = "hub/mesh_tests.rs"]
mod mesh_tests;
