//! A link on one UDP socket, sans-IO: the punch, the reliable stream, strict TLS and the key proof
//! ([`crate::link`]), then whole messages. Datagrams and the time in; datagrams, messages and the
//! phase out. The caller owns the socket (the one the reflector saw) and the monotonic clock: the
//! app's link worker runs this on a thread of its own, and the tests drive it through the NAT
//! simulator.
//!
//! - **Demultiplexing:** a punch datagram (`0xE0`, [`punch::is_punch`]) goes to the punch, a
//!   stream datagram (`0xE1`) to the stream. The socket owner routes call datagrams (`0xE2`).
//! - **The stream id** is fresh per session: `sha256(label | nonce_lower | nonce_higher)[..8]` over
//!   both punch nonces, so it names no invite and no key. The peer's nonce is read from its probe.
//! - **TLS** is the strict profile with a fresh server certificate. The proof binds itself to that
//!   certificate's hash. Once the proof completes and all our TLS output is in the stream, the
//!   stream takes the proof's stream keys and authenticates every datagram from then on.
//! - **Messages** use `head u8 | len u16 BE | fragment` after the proof. The head's low seven bits
//!   name the kind. Its high bit is set on every non-final fragment, each exactly 4096 bytes.
//! - A failure is final. After a version refusal, poll until [`Connection::done`] so the
//!   reliable stream can deliver the notice.
//! - **Reconnect** ([`Connection::reconnect`]) punches with a tag that rotates by the hour
//!   ([`ProbeTags`]) toward the addresses the caller remembered, and accepts the neighbouring hours'
//!   tags too. A deliberate stop ([`Connection::close`]) sends the stream's own close, so the other
//!   side sees the drop at once ([`Connection::dropped`]); there is no wire change for it.

use crate::link::{self, label, Linked, Mode, Proof, Refusal, Role};
use makepad_network::digest::sha256_hash;
use makepad_network::p2p::punch::{self, Punch};
use makepad_network::p2p::stream::{self, CloseReason, Stream};
use makepad_network::p2p::words::Invite;
use makepad_network::tls::{generate_p256, TlsEngine, TlsIdentity};
use makepad_network::tunnel::hmac_sha256;
use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, SocketAddr, SocketAddrV4};

/// The first byte of a stream datagram.
const STREAM_KIND: u8 = 0xE1;
/// The first byte of a datagram sealed by the call engine.
pub const CALL_DATAGRAM: u8 = 0xE2;
/// Maximum reliable Call control message size in bytes. Media travels in datagrams.
pub const MAX_CALL: usize = 64 * 1024;
const FRAGMENT: usize = 4096;
const FRAME_HEADER: usize = 3;
const MORE: u8 = 0x80;
const WINDOW_MS: u64 = 50;
/// One read from the stream or from TLS.
const CHUNK: usize = 16 * 1024;
const NOTICE_DRAIN_MS: u64 = 2_000;
/// A reconnect punches at the usual pace for this long, then probes once every
/// [`RECONNECT_IDLE_INTERVAL_MS`]: often enough to keep a NAT mapping, rarely enough to be quiet.
const RECONNECT_FAST_MS: u64 = 60_000;
pub const RECONNECT_IDLE_INTERVAL_MS: u64 = 17_000;

/// The recipient of a reliable message inside the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Sync = 1,
    Call = 2,
    Blob = 3,
}

impl Kind {
    fn index(self) -> usize {
        self as usize - 1
    }

    fn maximum(self) -> usize {
        match self {
            Self::Sync => crate::sync::MAX_MESSAGE,
            Self::Call => MAX_CALL,
            Self::Blob => crate::blob::MAX_MESSAGE,
        }
    }
}

#[derive(Default)]
struct SendQueue {
    messages: VecDeque<Vec<u8>>,
    offset: usize,
}

#[derive(Default)]
struct Incoming {
    header: [u8; FRAME_HEADER],
    header_len: usize,
    /// Kind, continuation bit and bytes still to take from the current frame.
    frame: Option<(Kind, bool, usize)>,
    partial: [Vec<u8>; 3],
    inbox: VecDeque<(Kind, Vec<u8>)>,
    sync_cost: u64,
    // Plaintext already taken from TLS when a complete Sync used the last credit.
    remainder: VecDeque<u8>,
}

/// A short (even empty) message occupies a queue entry. Charge at least one
/// fragment so a stream of tiny messages also runs out of receive credit.
pub(crate) fn sync_cost(body: &[u8]) -> u64 { body.len().max(FRAGMENT) as u64 }

impl Incoming {
    fn start_frame(&mut self) -> Result<(Kind, bool, usize), Failure> {
        let [head, hi, lo] = self.header;
        let malformed = |why| Failure::Refused(Refusal::Format(why));
        let kind = match head & !MORE {
            1 => Kind::Sync,
            2 => Kind::Call,
            3 => Kind::Blob,
            _ => return Err(malformed("unknown message kind")),
        };
        let len = usize::from(u16::from_be_bytes([hi, lo]));
        let more = head & MORE != 0;
        if len > FRAGMENT { return Err(malformed("fragment too large")); }
        if more && len != FRAGMENT { return Err(malformed("short non-final fragment")); }
        let partial = &mut self.partial[kind.index()];
        let maximum = kind.maximum();
        if len > maximum - partial.len() { return Err(malformed("message too large")); }
        let needed = partial.len() + len;
        if needed > partial.capacity() {
            // Grow geometrically, including near the limit where Vec's usual doubling
            // could allocate past the largest message for this kind.
            let growth = partial.capacity().saturating_mul(2).min(maximum).min(isize::MAX as usize);
            partial.try_reserve_exact(needed.max(growth) - partial.len())
                .map_err(|_| Failure::Stream("cannot allocate incoming message".into()))?;
        }
        Ok((kind, more, len))
    }

    fn feed(&mut self, plain: &[u8], credit: u64) -> Result<(), Failure> {
        let used = self.parse(plain, credit)?;
        self.remainder.extend(&plain[used..]);
        Ok(())
    }

    fn resume(&mut self, credit: u64) -> Result<(), Failure> {
        if !self.remainder.is_empty() && self.sync_cost < credit {
            let mut chunk = std::mem::take(&mut self.remainder);
            let used = self.parse(chunk.make_contiguous(), credit)?;
            chunk.drain(..used);
            self.remainder = chunk;
        }
        Ok(())
    }

    fn parse(&mut self, mut plain: &[u8], credit: u64) -> Result<usize, Failure> {
        let total = plain.len();
        while !plain.is_empty() && self.sync_cost < credit {
            let (kind, more, remaining) = if let Some(frame) = self.frame { frame } else {
                let n = (FRAME_HEADER - self.header_len).min(plain.len());
                self.header[self.header_len..self.header_len + n].copy_from_slice(&plain[..n]);
                self.header_len += n;
                plain = &plain[n..];
                if self.header_len < FRAME_HEADER { break; }
                self.header_len = 0;
                self.start_frame()?
            };
            let (piece, rest) = plain.split_at(remaining.min(plain.len()));
            let partial = &mut self.partial[kind.index()];
            partial.extend_from_slice(piece);
            if piece.len() < remaining {
                self.frame = Some((kind, more, remaining - piece.len()));
                plain = rest;
                break;
            }
            self.frame = None;
            if !more {
                if kind == Kind::Sync { self.sync_cost += sync_cost(partial); }
                self.inbox.push_back((kind, std::mem::take(partial)));
            }
            plain = rest;
        }
        Ok(total - plain.len())
    }
}

/// How far a connection got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Probing the peer's address since `since` (ms), nothing acknowledged yet.
    Punching { since: u64 },
    /// The path is open; the stream and TLS are being set up.
    Securing,
    /// TLS is up; the key proof runs inside it.
    Proving,
    /// The peer proved its key. Messages flow.
    Linked,
    Failed(Failure),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The key proof refused the peer (or itself: a self link, candidates that fit no role).
    Refused(Refusal),
    /// TLS failed: a broken record, a refused handshake, or the OS's TLS stack.
    Tls(String),
    /// The stream closed or timed out.
    Stream(String),
}

/// Who the peer proved to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub device: crate::key::PublicKey,
    pub name: String,
    pub version: u16,
}

/// Why a link that was up ended without a failure of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// The other device closed the stream: a deliberate stop.
    Closed,
    /// The other device probed again with a new nonce: it started over, so this stream is dead.
    Restarted,
}

/// The hour a reconnect tag is for: UTC hours since the epoch.
pub fn probe_hour(unix_secs: u64) -> u64 {
    unix_secs / 3600
}

/// The tag a reconnect probe carries in `hour`: `HMAC(k_probe, label | hour)[..8]`, with
/// `k_probe = HMAC(link_secret, label)`. Only the two devices can make it, and it changes every
/// hour, so no tag is a handle an observer can follow across networks.
pub fn probe_tag(link_secret: &[u8; 32], hour: u64) -> [u8; 8] {
    ProbeKey::new(link_secret).tag(hour)
}

/// Kept in memory for the lifetime of a wanted device.
pub(crate) struct ProbeKey([u8; 32]);

impl ProbeKey {
    pub(crate) fn new(secret: &[u8; 32]) -> Self {
        Self(hmac_sha256(secret, label::PROBE_KEY))
    }

    fn tag(&self, hour: u64) -> [u8; 8] {
        hmac_sha256(&self.0, &[label::PROBE_TAG, &hour.to_be_bytes()].concat())[..8].try_into().unwrap()
    }
}

/// The tags of one hour of a reconnect: the one probes carry, and the neighbouring hours' that a
/// probe may carry when the two clocks are not on the same side of the hour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeTags {
    pub send: [u8; 8],
    pub accept: [[u8; 8]; 2],
    pub hour: u64,
}

impl ProbeTags {
    pub fn new(link_secret: &[u8; 32], hour: u64) -> ProbeTags {
        Self::from_key(&ProbeKey::new(link_secret), hour)
    }

    pub(crate) fn from_key(key: &ProbeKey, hour: u64) -> Self {
        ProbeTags {
            send: key.tag(hour),
            accept: [key.tag(hour.wrapping_sub(1)), key.tag(hour.wrapping_add(1))],
            hour,
        }
    }
}

/// Where a device was last reached, as this side saw it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Remembered {
    /// The peer's address on a public path.
    pub public: Option<SocketAddr>,
    /// The peer's LAN candidate, only when the link ran over the LAN.
    pub lan: Option<SocketAddrV4>,
}

pub struct Connection {
    role: Role,
    punch: Punch,
    lan: Option<SocketAddrV4>,
    nonce: [u8; 8],
    /// A reconnect: probes carry a rotating tag, and a link watches for the peer starting over.
    reconnect: bool,
    peer_nonce: Option<[u8; 8]>,
    restart_probe: Option<(SocketAddr, Vec<u8>)>,
    /// We asked the stream to close.
    closing: bool,
    drain_sync: bool,
    dropped: Option<Dropped>,
    /// Derived once, when the peer's probe supplies the second nonce.
    stream_id: Option<[u8; 8]>,
    stream: Option<Stream>,
    rate_cap: Option<u64>,
    // The hub grants complete datagrams before poll_transmit starts their retransmission clocks.
    packet_budget: Option<usize>,
    transmit_after: u64,
    rate: u64,
    window_at: u64,
    window_sent: u64,
    held: bool,
    tls: Option<TlsEngine>,
    /// Our certificate's hash, when we serve.
    own_cert_hash: Option<[u8; 32]>,
    mode: Option<Mode>,
    proof: Option<Proof>,
    proof_random: [u8; 64],
    #[cfg(test)]
    make_proof: fn(Mode, [u8; 32], [u8; 64]) -> Result<Proof, Refusal>,
    linked: Option<Linked>,
    keys_installed: bool,
    /// TLS ciphertext the stream hasn't taken yet.
    to_stream: Vec<u8>,
    /// Pending proof bytes or, once linked, the unaccepted part of one frame.
    to_tls: Vec<u8>,
    queues: [SendQueue; 3],
    incoming: Incoming,
    read_credit: u64,
    out: Vec<(SocketAddr, Vec<u8>)>,
    phase: Phase,
    notice_until: Option<u64>,
}

/// The punch tag of a first contact: both sides derive it from both invites. It names the pairing
/// to the punch and is not a secret.
pub fn first_contact_tag(mode: &Mode) -> Option<[u8; 8]> {
    let Mode::FirstContact(f) = mode else { return None };
    Some(invite_tag(&f.own_invite, &f.peer_invite))
}

pub(crate) fn invite_tag(own: &Invite, peer: &Invite) -> [u8; 8] {
    let (a, b) = (link::invite_id(own), link::invite_id(peer));
    let (lower, higher) = if a < b { (a, b) } else { (b, a) };
    sha256_hash(&[label::FIRST_TAG, &lower, &higher].concat())[..8].try_into().unwrap()
}

fn stream_id(own: [u8; 8], peer: [u8; 8]) -> [u8; 8] {
    let (lower, higher) = if own < peer { (own, peer) } else { (peer, own) };
    sha256_hash(&[label::STREAM_ID, &lower, &higher].concat())[..8].try_into().unwrap()
}

/// The peer's LAN candidate when both invites offer one and share a public IPv4 address.
pub fn lan_candidate(own: &Invite, peer: &Invite) -> Option<SocketAddrV4> {
    if own.lan.is_some() && own.addr.ip() == peer.addr.ip() { peer.lan } else { None }
}

fn is_local_source(from: SocketAddr) -> bool {
    match from.ip().to_canonical() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback() || ip.is_unicast_link_local(),
    }
}

impl Connection {
    /// A connection for `mode`, punching toward `peer` with `tag` from `now`. `random` is 72 bytes
    /// from the OS: the punch nonce, then the proof's nonce and key contribution.
    pub fn new(mode: Mode, peer: SocketAddr, tag: [u8; 8], random: [u8; 72], now: u64) -> Result<Connection, Refusal> {
        let lan = match &mode {
            Mode::FirstContact(f) => lan_candidate(&f.own_invite, &f.peer_invite),
            Mode::Reconnect(_) => None,
        };
        Self::build(mode, peer, lan, punch::Config::new(tag), random, now)
    }

    /// A reconnect for `mode` (which must be [`Mode::Reconnect`]) toward what was remembered of
    /// the peer. The LAN candidate is probed and its local sources accepted only because it was
    /// remembered: it was recorded when a link ran over the LAN under the first contact's rules.
    /// It punches with `tags`, at the usual pace and then quietly, for as long as the caller runs it.
    pub fn reconnect(mode: Mode, remembered: Remembered, tags: &ProbeTags, random: [u8; 72], now: u64) -> Result<Connection, Refusal> {
        if !matches!(mode, Mode::Reconnect(_)) {
            return Err(Refusal::Format("a reconnect needs a reconnect mode"));
        }
        let Some(first) = remembered.public.or(remembered.lan.map(SocketAddr::V4)) else {
            return Err(Refusal::Format("no remembered address"));
        };
        let config = punch::Config::new(tags.send).accepting(&tags.accept).idle(RECONNECT_FAST_MS, RECONNECT_IDLE_INTERVAL_MS);
        let mut conn = Self::build(mode, first, remembered.lan, config, random, now)?;
        conn.reconnect = true;
        Ok(conn)
    }

    fn build(mode: Mode, peer: SocketAddr, lan: Option<SocketAddrV4>, config: punch::Config, random: [u8; 72], now: u64) -> Result<Connection, Refusal> {
        let role = link::role(&mode)?;
        let nonce: [u8; 8] = random[..8].try_into().unwrap();
        let mut punch = Punch::new(config, peer, nonce, now);
        if let Some(addr) = lan {
            punch.prefer(SocketAddr::V4(addr));
        }
        Ok(Connection {
            role,
            punch,
            lan,
            nonce,
            reconnect: false,
            peer_nonce: None,
            restart_probe: None,
            closing: false,
            drain_sync: false,
            dropped: None,
            stream_id: None,
            stream: None,
            rate_cap: None,
            packet_budget: None,
            transmit_after: 0,
            rate: 0,
            window_at: now,
            window_sent: 0,
            held: false,
            tls: None,
            own_cert_hash: None,
            mode: Some(mode),
            proof: None,
            proof_random: random[8..].try_into().unwrap(),
            #[cfg(test)]
            make_proof: Proof::new,
            linked: None,
            keys_installed: false,
            to_stream: Vec::new(),
            to_tls: Vec::new(),
            queues: Default::default(),
            incoming: Incoming::default(),
            read_credit: u64::MAX,
            out: Vec::new(),
            phase: Phase::Punching { since: now },
            notice_until: None,
        })
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// If a linked stream has reached its idle deadline, drain the socket before polling it.
    pub fn idle_due(&self, now: u64) -> bool {
        self.phase == Phase::Linked && self.stream.as_ref().is_some_and(|s| s.idle_due(now))
    }

    /// Stream state, including its precise close reason, for link diagnostics.
    pub fn stream_state(&self) -> Option<stream::State> {
        self.stream.as_ref().map(Stream::state)
    }

    /// A failed connection may be dropped once its final notice has had time to retransmit.
    pub fn done(&self, now: u64) -> bool {
        matches!(self.phase, Phase::Failed(_)) && self.notice_until.is_none_or(|end| now >= end)
    }

    /// The LAN candidate this connection is allowed to probe.
    pub fn lan_probing(&self) -> Option<SocketAddrV4> {
        self.lan
    }

    /// The proven peer, once linked.
    pub fn peer(&self) -> Option<Peer> {
        self.linked.as_ref().map(|l| Peer { device: l.peer_device, name: l.peer_name.clone(), version: l.version })
    }

    /// Rotates the reconnect tags: probes carry the new hour's, and the neighbouring hours' are
    /// accepted.
    pub fn set_probe_tags(&mut self, tags: &ProbeTags) {
        self.punch.set_tags(tags.send, &tags.accept);
    }

    /// Where the peer is on the open path, as this side sees it: a public address, or the LAN
    /// candidate when the link runs over the LAN. `None` before a path is open.
    pub fn path(&self) -> Option<Remembered> {
        match self.peer_address()? {
            SocketAddr::V4(addr) if self.lan.is_some() && is_local_source(SocketAddr::V4(addr)) => Some(Remembered { public: None, lan: Some(addr) }),
            addr if !is_local_source(addr) => Some(Remembered { public: Some(addr), lan: None }),
            _ => None,
        }
    }

    /// Asks the stream to close. The peer sees its end at once ([`Connection::dropped`]); poll to
    /// send it. Before the keys are in there is no stream close to send, and nothing happens.
    pub fn close(&mut self) {
        if self.keys_installed && !matches!(self.phase, Phase::Failed(_)) {
            if let Some(stream) = &mut self.stream {
                stream.close();
                self.closing = true;
            }
        }
    }

    /// Finish the already authorized sync prefix before queuing FIN. The owning slot
    /// bounds this drain by its farewell deadline, including at zero receive credit.
    pub(crate) fn close_after_sync(&mut self) {
        self.queues[Kind::Call.index()] = SendQueue::default();
        self.queues[Kind::Blob.index()] = SendQueue::default();
        self.drain_sync = true;
        self.finish_sync_if_ready();
    }

    fn finish_sync_if_ready(&mut self) {
        if self.drain_sync && self.queues[Kind::Sync.index()].messages.is_empty()
            && self.to_tls.is_empty() && self.to_stream.is_empty()
        {
            self.drain_sync = false;
            self.close();
        }
    }

    /// Whether the stream's close we asked for has finished (or the stream is gone).
    pub fn close_done(&self) -> bool {
        matches!(self.phase, Phase::Failed(_)) || (!self.drain_sync && !self.closing)
            || self.stream.as_ref().is_none_or(|s| matches!(s.state(), stream::State::Closed(_)))
    }

    /// Why a link that was up ended, when it ended by the other device's doing.
    pub fn dropped(&self) -> Option<Dropped> {
        self.dropped
    }

    /// The probe that showed a restarted peer, to hand to the connection that replaces this one: it
    /// is the only one that carries the peer's new nonce.
    pub fn take_restart_probe(&mut self) -> Option<(SocketAddr, Vec<u8>)> {
        self.restart_probe.take()
    }

    #[cfg(test)]
    pub(crate) fn transport_mut(&mut self) -> Option<&mut Stream> { self.stream.as_mut() }

    /// The path's current peer address, once the punch opened it.
    pub fn peer_address(&self) -> Option<SocketAddr> {
        self.stream.as_ref().map(|s| s.peer()).or_else(|| self.punch.peer())
    }

    pub fn stream_id(&self) -> Option<[u8; 8]> {
        self.stream_id
    }

    /// A routing key only. Punch and stream still validate the datagram they receive.
    pub(crate) fn punch_tag(packet: &[u8]) -> Option<[u8; 8]> {
        if !punch::is_punch(packet) || !matches!(packet[1], 1 | 2) { return None }
        packet.get(2..10)?.try_into().ok()
    }

    pub(crate) fn packet_stream_id(packet: &[u8]) -> Option<[u8; 8]> {
        if packet.first() != Some(&STREAM_KIND) { return None }
        packet.get(2..10)?.try_into().ok()
    }

    /// Whether `packet` belongs to a connection (a punch or a stream datagram).
    pub fn is_ours(packet: &[u8]) -> bool {
        punch::is_punch(packet) || packet.first() == Some(&STREAM_KIND)
    }

    pub fn is_call(packet: &[u8]) -> bool {
        packet.first() == Some(&CALL_DATAGRAM)
    }

    /// The stream's send cap in bits per second, including for a stream still to be opened.
    pub fn set_rate_cap(&mut self, cap: Option<u64>) {
        self.rate_cap = cap;
        if let Some(cap) = cap { self.rate = self.rate.min(cap / 8); }
        if let Some(stream) = &mut self.stream {
            stream.set_rate_cap(cap);
        }
    }

    pub(crate) fn transmit_turn(&mut self, budget: Option<usize>, after: u64) {
        self.packet_budget = budget;
        self.transmit_after = after;
    }

    /// Wire bytes handed out by the stream, including its opening handshake.
    pub fn sent_bytes(&self) -> u64 {
        self.stream.as_ref().map_or(0, Stream::sent_bytes)
    }

    fn measure_rate(&mut self, now: u64) {
        let elapsed = now.saturating_sub(self.window_at);
        if elapsed < WINDOW_MS { return; }
        let sent = self.sent_bytes();
        if self.held {
            let sample = sent.saturating_sub(self.window_sent).saturating_mul(1000) / elapsed;
            self.rate = if sample >= self.rate { sample } else { self.rate - (self.rate - sample).div_ceil(4) };
            if let Some(cap) = self.rate_cap { self.rate = self.rate.min(cap / 8); }
        }
        self.window_at = now;
        self.window_sent = sent;
        self.held = false;
    }

    fn mark(&self) -> usize {
        let bytes = self.rate / (1000 / WINDOW_MS);
        usize::try_from(bytes).unwrap_or(usize::MAX).max(FRAGMENT + FRAME_HEADER)
    }

    fn committed_unsent(&self) -> usize {
        self.to_tls.len().saturating_add(self.to_stream.len())
            .saturating_add(self.stream.as_ref().map_or(0, Stream::unsent_bytes))
    }

    /// Called only with an empty TLS plaintext buffer. Commit a single frame so the next
    /// decision sees the ciphertext (including record overhead) of this one.
    fn stage_frame(&mut self) {
        for kind in [Kind::Call, Kind::Sync, Kind::Blob] {
            let queue = &self.queues[kind.index()];
            let Some(message) = queue.messages.front() else { continue };
            let rest = &message[queue.offset..];
            let len = rest.len().min(FRAGMENT);
            let more = len < rest.len();
            if kind != Kind::Call {
                let unsent = self.committed_unsent();
                if unsent != 0 && FRAME_HEADER + len > self.mark().saturating_sub(unsent) {
                    self.held = true;
                    return;
                }
            }
            self.to_tls.reserve(FRAME_HEADER + len);
            self.to_tls.push(kind as u8 | if more { MORE } else { 0 });
            self.to_tls.extend_from_slice(&(len as u16).to_be_bytes());
            self.to_tls.extend_from_slice(&rest[..len]);
            let queue = &mut self.queues[kind.index()];
            if more {
                queue.offset += len;
            } else {
                queue.messages.pop_front();
                queue.offset = 0;
            }
            return;
        }
    }

    fn install_stream(&mut self, mut stream: Stream) {
        stream.set_rate_cap(self.rate_cap);
        self.stream = Some(stream);
        self.phase = Phase::Securing;
    }

    /// A datagram from the socket.
    pub fn handle(&mut self, from: SocketAddr, packet: &[u8], now: u64) {
        if matches!(self.phase, Phase::Failed(_)) {
            if !self.done(now) && packet.first() == Some(&STREAM_KIND) {
                if let Some(stream) = &mut self.stream {
                    stream.handle_datagram(now, from, packet);
                }
            }
            return;
        }
        if punch::is_punch(packet) {
            if self.lan.is_none() && is_local_source(from) {
                return;
            }
            if packet[1] == 1 && self.punch.accepts(&packet[2..10]) {
                let peer_nonce: [u8; 8] = packet[10..18].try_into().unwrap();
                if self.stream.is_none() {
                    // The latest probe names the peer's connection: an older one's can still be
                    // in flight when the peer started over.
                    self.stream_id = Some(stream_id(self.nonce, peer_nonce));
                    self.peer_nonce = Some(peer_nonce);
                } else if self.reconnect && self.peer_nonce.is_some_and(|known| known != peer_nonce) && self.phase == Phase::Linked
                    && self.stream.as_ref().is_some_and(|s| s.peer() == from) {
                    // The tag is the pair's own, so only the peer makes a probe under it. A new
                    // nonce from the address it is linked at means it started over: this stream is
                    // dead, and waiting for its idle time would keep the two apart for a minute.
                    self.restart_probe = Some((from, packet.to_vec()));
                    self.dropped = Some(Dropped::Restarted);
                    self.fail(Failure::Stream("the other device started over".into()));
                    return;
                }
            }
            let (datagrams, _) = self.punch.handle(from, packet, now);
            self.out.extend(datagrams);
        } else if packet.first() == Some(&STREAM_KIND) {
            if let Some(stream) = &mut self.stream {
                stream.handle_datagram(now, from, packet);
                if self.keys_installed {
                    self.punch.note_heard(from, now);
                }
            } else if let (Role::Server, Some(id)) = (self.role, self.stream_id) {
                // The client's OPEN chooses the path, even if our punch opened on another.
                if self.punch.targets().contains(&from) {
                    let mut stream = Stream::new(stream::Role::Server, id, from, now);
                    stream.handle_datagram(now, from, packet);
                    if stream.state() == stream::State::Open {
                        self.install_stream(stream);
                        self.punch.select(from, now);
                    }
                }
            }
        }
        // While the socket is being drained, an unrelated packet before the peer's
        // authenticated keepalive must not cause the stream to expire prematurely.
        if !self.idle_due(now) { self.pump(now); }
    }

    /// Queues one whole message for the peer. Before the link is up it waits.
    pub fn send(&mut self, kind: Kind, message: &[u8]) {
        if matches!(self.phase, Phase::Failed(_)) {
            return;
        }
        if message.len() > kind.maximum() {
            debug_assert!(message.len() <= kind.maximum(), "outgoing {kind:?} message too large");
            return;
        }
        self.queues[kind.index()].messages.push_back(message.to_vec());
    }

    /// Whether all previously queued messages of this kind have been committed.
    pub fn writable(&self, kind: Kind) -> bool {
        self.queues[kind.index()].messages.is_empty() && (kind == Kind::Call ||
            self.committed_unsent() < crate::sync::BATCH_BYTES)
    }

    /// The next whole message from the peer.
    pub fn recv(&mut self) -> Option<(Kind, Vec<u8>)> {
        let (kind, body) = self.incoming.inbox.pop_front()?;
        if kind == Kind::Sync { self.incoming.sync_cost -= sync_cost(&body); }
        Some((kind, body))
    }

    pub(crate) fn set_read_credit(&mut self, credit: u64) -> bool {
        let paused = self.read_paused();
        self.read_credit = credit;
        paused && !self.read_paused()
    }

    fn read_paused(&self) -> bool {
        self.linked.is_some() && self.incoming.sync_cost >= self.read_credit
    }

    /// The datagrams due at `now`, as (to, bytes).
    pub fn poll(&mut self, now: u64) -> Vec<(SocketAddr, Vec<u8>)> {
        if !matches!(self.phase, Phase::Failed(_)) {
            let (datagrams, _) = self.punch.poll(now);
            self.out.extend(datagrams);
            self.pump(now);
        }
        if self.notice_until.is_some_and(|end| now < end) {
            if self.flush_notice().is_ok() {
                self.poll_stream(now);
            } else {
                self.notice_until = None;
            }
        }
        std::mem::take(&mut self.out)
    }

    /// When [`Connection::poll`] next has something to do (ms).
    pub fn next_wake(&self) -> u64 {
        let stream = self.stream.as_ref().and_then(|s| s.next_timeout()).unwrap_or(u64::MAX).max(self.transmit_after);
        if matches!(self.phase, Phase::Failed(_)) {
            return self.notice_until.map_or(u64::MAX, |end| end.min(stream));
        }
        self.punch.next_wake().min(stream)
    }

    fn fail(&mut self, failure: Failure) {
        self.queues = Default::default();
        self.to_tls.clear();
        self.phase = Phase::Failed(failure);
        self.out.clear();
    }

    /// Only the refusal notice remains in to_tls. Keep partial writes across polls.
    fn flush_notice(&mut self) -> io::Result<()> {
        while self.flush_output()? {}
        Ok(())
    }

    fn flush_output(&mut self) -> io::Result<bool> {
        let Some(tls) = self.tls.as_mut() else { return Ok(false) };
        let mut moved = false;
        if !self.to_tls.is_empty() {
            match tls.write_plaintext(&self.to_tls) {
                Ok(n) => {
                    self.to_tls.drain(..n);
                    moved = n > 0;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
        }
        tls.take_ciphertext(&mut self.to_stream)?;
        if !self.to_stream.is_empty() {
            let n = self.stream.as_mut().expect("TLS runs over an open stream").write(&self.to_stream);
            self.to_stream.drain(..n);
            moved |= n > 0;
        }
        Ok(moved)
    }

    /// Moves bytes between the layers until nothing changes.
    fn pump(&mut self, now: u64) {
        self.measure_rate(now);
        if let Err(failure) = self.step(now) {
            if self.dropped == Some(Dropped::Closed) {
                // Keep acknowledging the peer's close for a moment, so it sees it finish.
                self.queues = Default::default();
                self.to_tls.clear();
                self.phase = Phase::Failed(failure);
                self.notice_until = Some(now.saturating_add(NOTICE_DRAIN_MS));
                return;
            }
            if let (Failure::Refused(_), Some(proof)) = (&failure, &mut self.proof) {
                let notice = proof.take_output();
                if !notice.is_empty() {
                    // Discard queued application bytes. Earlier stream datagrams still go out
                    // in order, followed by the notice.
                    self.queues = Default::default();
                    self.to_tls = notice;
                    self.phase = Phase::Failed(failure);
                    self.notice_until = Some(now.saturating_add(NOTICE_DRAIN_MS));
                    return;
                }
            }
            self.fail(failure);
        }
    }

    fn step(&mut self, now: u64) -> Result<(), Failure> {
        let tls_error = |e: io::Error| Failure::Tls(e.to_string());
        // Only the client initiates a stream on its open punch path.
        if self.stream.is_none() {
            let (Role::Client, Some(peer), Some(id)) = (self.role, self.punch.peer(), self.stream_id) else { return Ok(()) };
            self.install_stream(Stream::new(stream::Role::Client, id, peer, now));
        }
        let stream = self.stream.as_mut().unwrap();
        if let stream::State::Closed(reason) = stream.state() {
            if self.linked.is_some() && reason == CloseReason::Finished {
                if !self.closing {
                    // The transport can finish its FIN exchange while the UI
                    // holds credit. Its contiguous pre-FIN bytes are still readable.
                    self.read_layers()?;
                    if self.read_paused() { return Ok(()) }
                    self.dropped = Some(Dropped::Closed);
                    return Err(Failure::Stream("the other device closed the link".into()));
                }
                return Ok(());
            }
            return Err(Failure::Stream(format!("{reason:?}")));
        }
        // TLS starts once the stream is open.
        if self.tls.is_none() && stream.state() == stream::State::Open {
            let identity = TlsIdentity::in_memory(generate_p256().map_err(tls_error)?, "", None).map_err(tls_error)?;
            self.tls = Some(if self.role == Role::Server {
                self.own_cert_hash = Some(sha256_hash(&identity.cert_der));
                TlsEngine::server(identity).map_err(tls_error)?
            } else {
                TlsEngine::client(identity, None).map_err(tls_error)?
            });
        }
        self.read_layers()?;
        // The peer closed after sending all it had: everything it sent is in the inbox by now.
        if self.linked.is_some() && !self.closing && !self.read_paused()
            && self.stream.as_ref().is_some_and(|s| s.state() == stream::State::Closing) {
            self.dropped = Some(Dropped::Closed);
            return Err(Failure::Stream("the other device closed the link".into()));
        }
        loop {
            self.commit().map_err(tls_error)?;
            self.finish_sync_if_ready();
            let sent = self.poll_stream(now);
            if !sent || !self.keys_installed || (self.to_tls.is_empty() && self.to_stream.is_empty()
                && self.queues.iter().all(|q| q.messages.is_empty())) {
                break;
            }
        }
        Ok(())
    }

    fn poll_stream(&mut self, now: u64) -> bool {
        let stream = self.stream.as_mut().unwrap();
        let before = self.out.len();
        while self.packet_budget != Some(0) {
            let Some(datagram) = stream.poll_transmit(now) else { break };
            if let Some(remaining) = &mut self.packet_budget { *remaining -= 1; }
            self.out.push(datagram);
            self.punch.note_sent(now);
        }
        self.out.len() > before
    }

    fn commit(&mut self) -> io::Result<()> {
        loop {
            if self.keys_installed && self.to_tls.is_empty() {
                self.stage_frame();
            }
            let mut moved = self.flush_output()?;
            if !self.keys_installed && self.to_tls.is_empty() && self.to_stream.is_empty() {
                if let (Some(done), Some(stream)) = (&self.linked, &mut self.stream) {
                    if stream.set_keys(done.keys.stream_send, done.keys.stream_recv) {
                        self.keys_installed = true;
                        self.phase = Phase::Linked;
                        moved = true;
                    }
                }
            }
            if !moved { return Ok(()); }
        }
    }

    /// Drain available stream input into TLS, then into the proof or message reassembly.
    fn read_layers(&mut self) -> Result<(), Failure> {
        let Some(tls) = self.tls.as_mut() else { return Ok(()) };
        let stream = self.stream.as_mut().unwrap();
        #[cfg(not(test))]
        let make_proof = Proof::new;
        #[cfg(test)]
        let make_proof = self.make_proof;
        let tls_error = |e: io::Error| Failure::Tls(e.to_string());
        let mut buf = [0u8; CHUNK];
        self.incoming.resume(self.read_credit)?;
        while self.linked.is_none() || self.incoming.sync_cost < self.read_credit {
            // Stream to TLS.
            let n = stream.read(&mut buf);
            if n > 0 {
                tls.feed_ciphertext(&buf[..n]).map_err(tls_error)?;
            }
            // The proof starts when TLS is up, with the server certificate's hash.
            if self.proof.is_none() && tls.handshake_done() {
                let cert_hash = match self.own_cert_hash {
                    Some(hash) => hash,
                    None => sha256_hash(tls.server_certificate_der().ok_or_else(|| Failure::Tls("no server certificate".into()))?),
                };
                let taken = self.mode.take().expect("the mode waits for the proof");
                self.proof = Some(make_proof(taken, cert_hash, self.proof_random).map_err(Failure::Refused)?);
                self.phase = Phase::Proving;
            }
            if let Some(running) = self.proof.as_mut() {
                // TLS plaintext to the proof, then to messages.
                while self.linked.is_none() || self.incoming.sync_cost < self.read_credit {
                    match tls.read_plaintext(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if self.linked.is_some() {
                                self.incoming.feed(&buf[..n], self.read_credit)?;
                            } else if let Some(done) = running.feed(&buf[..n]).map_err(Failure::Refused)? {
                                self.incoming.feed(&done.leftover, self.read_credit)?;
                                self.linked = Some(done);
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(tls_error(e)),
                    }
                }
                // Proof output goes directly to TLS, ahead of every queued message.
                if !self.keys_installed {
                    self.to_tls.append(&mut running.take_output());
                }
            }
            if n == 0 { break; }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
