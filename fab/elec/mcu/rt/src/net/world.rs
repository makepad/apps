//! `World`: the played network behind a host end. Deterministic (a seed for jitter, virtual
//! nanoseconds, no clock of its own) and `Clone`, so a run can be marked and replayed exactly.
//!
//! - The link follows a timeline of conditions: joining, up, failing to join, lost.
//! - Endpoint `c` is a fake server, a [`Responder`] at the message level, and its opens end as the
//!   timeline says ([`Outcome`]): ready, or the code of the stage that failed. A [`Rules`] fake
//!   answers by a table; its sends under way are part of the world, so a clone replays them.
//! - Message bytes are shared ([`Payload`]): a clone (a run's mark) copies none of them.
//! - Each way, a connection has a latency (with seeded jitter) and a rate. Upstream it holds a
//!   window of [`super::FIRST_GRANT`] bytes; [`World::room`] is what it can take now, so a host
//!   grants from it and never buffers without bound.
//! - Every open is one attempt: retrying is the device's ports layer's business, never the world's.
//! - An [`Responder::Upstream`] endpoint is a real server outside: the world asks the network
//!   ([`World::request`]) and takes its answers ([`World::arrive`]) through [`super::upstream`]'s
//!   input log. The real network has its own timing, so no latency, rate or open outcome applies.

use super::upstream::{Arrival, Request};
use super::{code, FIRST_GRANT, LINK_JOINING, LINK_NONE, LINK_UP, OFFLINE, PART_BINARY, PART_FIRST, PART_LAST, READY};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// A whole message's bytes, shared between a world and its clones.
pub type Payload = Arc<[u8]>;

/// How long an open waits for a server that never answers before it reads as timed out.
pub const OPEN_TIMEOUT_NS: u64 = 10_000_000_000;

/// A fake server at the message level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Responder {
    /// Sends every whole message straight back, with its kind.
    Echo,
    /// Takes everything and sends nothing.
    Sink,
    /// Answers by a table.
    Rules(Arc<Rules>),
    /// Answers in host code.
    Service(Served),
    /// A real server, reached through the input log.
    Upstream,
}

/// A fake server in host code. It answers each message from that message alone (a cache is its
/// own business), so the world stays deterministic and a clone replays it.
pub trait Service: Send + Sync {
    /// What it sends when a connection goes ready: (binary, bytes) in order.
    fn ready(&self) -> Vec<(bool, Payload)>;
    /// Its answer to one whole message.
    fn answer(&self, binary: bool, bytes: &[u8]) -> Vec<(bool, Payload)>;
}

/// A [`Service`] as a responder, equal only to itself.
#[derive(Clone)]
pub struct Served(pub Arc<dyn Service>);

impl PartialEq for Served {
    fn eq(&self, other: &Served) -> bool { Arc::as_ptr(&self.0) as *const () == Arc::as_ptr(&other.0) as *const () }
}

impl Eq for Served {}

impl core::fmt::Debug for Served {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result { f.write_str("Served") }
}

/// A fake server's table. The connection going ready, then each whole message from the device, is
/// matched against the rules in order, and the first that matches runs: it stops the connection's
/// sends under way if it says so, then makes its own sends in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rules(pub Vec<Rule>);

impl Rules {
    /// Sends these in order once the connection is ready, and answers nothing.
    pub fn at_ready(then: Vec<Then>) -> Rules { Rules(alloc::vec![Rule { when: When::Ready, stop: false, then }]) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub when: When,
    /// Ends every send under way on the connection first; then the rule's own sends go only if
    /// one was under way (a stop that finds nothing running answers nothing).
    pub stop: bool,
    pub then: Vec<Then>,
}

/// What a rule answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum When {
    /// The connection went ready.
    Ready,
    /// A message of this kind starting with `bytes` (with `whole`, equal to them).
    Message { binary: bool, bytes: Vec<u8>, whole: bool },
}

/// One send of a rule, `after_ns` after the rule matched or its previous send ended: `body` led by
/// `head`. With a `block` size the body goes as messages of that many bytes (the last may be
/// shorter), `every_ns` apart, each led by the head; with none, or an empty body, as one message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Then {
    pub after_ns: u64,
    pub binary: bool,
    pub head: Vec<Field>,
    pub body: Payload,
    pub block: usize,
    pub every_ns: u64,
    /// The bytes of one unit of the body, as [`Field::Offset32`] and [`Field::Sent`] count it (1: bytes).
    pub unit: usize,
}

impl Then {
    /// One message of `bytes`, `after_ns` after the previous send.
    pub fn message(after_ns: u64, binary: bool, bytes: impl Into<Payload>) -> Then {
        Then { after_ns, binary, head: Vec::new(), body: bytes.into(), block: 0, every_ns: 0, unit: 1 }
    }
}

/// A part of a send's head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Bytes(Vec<u8>),
    /// Word `n` of the matched text message (split at spaces, word 0 its first) as written; nothing
    /// if it has fewer.
    Word(u8),
    /// Word `n` read as a decimal number (0 if it is not one), little-endian, in 16 or 32 bits.
    Word16(u8),
    Word32(u8),
    /// Where this message's part of the body starts, in units, little-endian in 32 bits.
    Offset32,
    /// In decimal, the units of binary body the rule's sends have sent so far, with those of the
    /// sends it stopped (a text body is not counted).
    Sent,
}

/// What connection `c` reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub responder: Responder,
}

/// How an open of a connection ends, by the stage that answers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ready,
    NotConfigured,
    /// The name did not resolve; the detail is the DNS rcode (3: no such name).
    NameNotFound,
    Refused,
    TimedOut,
    /// The server refused the upgrade or the token with this HTTP status.
    Rejected(u16),
}

/// One entry of a world's timeline, in effect from its time on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    /// The link joins for `ns`; then it is up, or, with `fails`, down with that code and detail.
    Join { ns: u64, fails: Option<(u8, u32)> },
    /// The link goes down now with this code and detail; every connection ends as a link loss.
    LinkDown { code: u8, detail: u32 },
    /// Opens of `c` from now on end this way.
    Open { c: u8, outcome: Outcome },
    /// Each way, a message on `c` takes `ns` plus up to `jitter_ns` (seeded) to cross.
    Latency { c: u8, ns: u64, jitter_ns: u64 },
    /// Each way, `c` carries this many bytes a second (0: no limit).
    Rate { c: u8, bytes_per_s: u64 },
    /// `c`, if open or opening, ends now with this code and detail.
    Disconnect { c: u8, code: u8, detail: u32 },
}

/// What a world tells its host end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorldEvent {
    Link { state: u8, code: u8, detail: u32 },
    /// A connection went ready, or ended (offline, with why).
    State { c: u8, state: u8, code: u8, detail: u32 },
    /// A whole message from the server arrived.
    Message { c: u8, binary: bool, bytes: Payload },
}

#[derive(Clone, Debug)]
enum Due {
    JoinEnd,
    /// The open of this generation ends.
    OpenEnd { c: u8, gen: u32 },
    /// A message reaches the server.
    Serve { c: u8, gen: u32, binary: bool, bytes: Payload },
    /// The server sends a message.
    Send { c: u8, gen: u32, binary: bool, bytes: Payload },
    /// A rule's next send.
    Step { c: u8, gen: u32, run: Run },
    /// A message reaches the host end.
    Arrive { c: u8, gen: u32, binary: bool, bytes: Payload },
}

/// A rule's sends under way.
#[derive(Clone, Debug)]
struct Run {
    rule: usize,
    /// The message it answers (empty for a ready).
    matched: Payload,
    /// The send it is at, and the bytes of its body already sent.
    then: usize,
    at: usize,
    sent: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    /// Opened while the link was joining: the attempt starts when it is up.
    WaitLink,
    Opening,
    Ready,
}

#[derive(Clone, Debug)]
struct Conn {
    outcome: Outcome,
    latency: u64,
    jitter: u64,
    rate: u64,
    phase: Phase,
    /// Bumped by every end, so whatever an ended attempt had in flight is dropped.
    gen: u32,
    /// Upstream bytes not yet drained: (done time, length) in order.
    up: VecDeque<(u64, u32)>,
    up_free: u64,
    down_free: u64,
    /// The upstream message being assembled from parts.
    partial: Vec<u8>,
    partial_binary: bool,
    /// An upstream endpoint's bytes sent and not yet taken by the network.
    held: u64,
}

#[derive(Clone, Debug)]
pub struct World {
    rng: u64,
    endpoints: Vec<Endpoint>,
    timeline: Vec<(u64, Condition)>,
    next_condition: usize,
    link: (u8, u8, u32),
    join: Option<(u8, u32)>,
    conns: Vec<Conn>,
    due: Vec<(u64, u64, Due)>,
    seq: u64,
    events: VecDeque<(u64, WorldEvent)>,
    requests: VecDeque<(u64, Request)>,
}

fn take_time(len: usize, rate: u64) -> u64 {
    if rate == 0 { 0 } else { (len as u64 * 1_000_000_000).div_ceil(rate) }
}

impl World {
    /// A world whose link is up at time 0 unless the timeline says otherwise at 0. Connection `c` is
    /// `endpoints[c]`; an open of a connection beyond them reads as not configured. The timeline
    /// is applied in time order (entries at one time in their given order).
    pub fn new(seed: u64, endpoints: Vec<Endpoint>, mut timeline: Vec<(u64, Condition)>) -> World {
        timeline.sort_by_key(|(t, _)| *t);
        let conn = Conn {
            outcome: Outcome::Ready, latency: 0, jitter: 0, rate: 0, phase: Phase::Idle, gen: 0,
            up: VecDeque::new(), up_free: 0, down_free: 0, partial: Vec::new(), partial_binary: false, held: 0,
        };
        let conns = alloc::vec![conn; endpoints.len().max(1)];
        let mut world = World {
            rng: seed ^ 0x9e37_79b9_7f4a_7c15, endpoints, timeline, next_condition: 0,
            link: (LINK_UP, code::NONE, 0), join: None, conns, due: Vec::new(), seq: 0, events: VecDeque::new(),
            requests: VecDeque::new(),
        };
        world.apply_conditions(0);
        world.events.clear();
        world
    }

    /// The link as it stands: state, code, detail.
    pub fn link(&self) -> (u8, u8, u32) { self.link }

    /// When something next happens (None: nothing will without a call).
    pub fn next_due(&self) -> Option<u64> {
        let condition = self.timeline.get(self.next_condition).map(|(t, _)| *t);
        let due = self.due.iter().map(|(t, _, _)| *t).min();
        let event = self.events.front().map(|(t, _)| *t);
        [condition, due, event].into_iter().flatten().min()
    }

    /// The next event at or before `now`, in time order.
    pub fn poll(&mut self, now: u64) -> Option<(u64, WorldEvent)> {
        loop {
            if let Some(&(t, _)) = self.events.front() {
                if t <= now { return self.events.pop_front(); }
            }
            let condition = self.timeline.get(self.next_condition).map(|(t, _)| *t).filter(|t| *t <= now);
            let due = self.due.iter().enumerate().filter(|(_, d)| d.0 <= now).min_by_key(|(_, d)| (d.0, d.1)).map(|(k, d)| (k, d.0));
            match (condition, due) {
                (Some(tc), Some((_, td))) if tc <= td => self.apply_conditions(tc),
                (Some(tc), None) => self.apply_conditions(tc),
                (_, Some((k, _))) => {
                    let (t, _, due) = self.due.remove(k);
                    self.run(t, due);
                }
                (None, None) => return None,
            }
        }
    }

    /// Starts one attempt to open `c`. An attempt already under way for `c` is ended first.
    pub fn open(&mut self, t: u64, c: u8) {
        let k = c as usize;
        if k >= self.endpoints.len() {
            self.events.push_back((t, WorldEvent::State { c, state: OFFLINE, code: code::NOT_CONFIGURED, detail: 0 }));
            return;
        }
        self.end(t, k, true);
        match self.link.0 {
            LINK_UP => self.attempt(t, k),
            LINK_JOINING => self.conns[k].phase = Phase::WaitLink,
            _ => {
                let (_, code, detail) = self.link;
                self.events.push_back((t, WorldEvent::State { c, state: OFFLINE, code, detail }));
            }
        }
    }

    /// Ends `c` at the device's word; nothing is reported back.
    pub fn close(&mut self, t: u64, c: u8) {
        if (c as usize) < self.endpoints.len() { self.end(t, c as usize, true); }
    }

    /// The next request for the real network, in the order asked.
    pub fn request(&mut self) -> Option<(u64, Request)> { self.requests.pop_front() }

    /// An answer from the real network at `t` (time order with the other calls). An answer to an
    /// attempt that has ended is dropped.
    pub fn arrive(&mut self, t: u64, arrival: Arrival) {
        let (c, gen) = arrival.attempt();
        let k = c as usize;
        if !self.upstream(k) || self.conns[k].gen != gen { return; }
        let phase = self.conns[k].phase;
        match arrival {
            Arrival::Opened { .. } => if phase == Phase::Opening {
                self.conns[k].phase = Phase::Ready;
                self.events.push_back((t, WorldEvent::State { c, state: READY, code: code::NONE, detail: 0 }));
            },
            Arrival::Ended { code, detail, .. } => if matches!(phase, Phase::Opening | Phase::Ready) {
                self.end(t, k, false);
                self.events.push_back((t, WorldEvent::State { c, state: OFFLINE, code, detail }));
            },
            Arrival::Message { binary, bytes, .. } => if phase == Phase::Ready {
                self.events.push_back((t, WorldEvent::Message { c, binary, bytes: bytes.into() }));
            },
            Arrival::Taken { bytes, .. } => self.conns[k].held = self.conns[k].held.saturating_sub(u64::from(bytes)),
        }
    }

    /// The network under every upstream connection was closed at `t` (a rewound run asked what its
    /// past did not): each one opening or open reads as reset. Nothing is asked of the network.
    pub fn cut_upstream(&mut self, t: u64) {
        for k in 0..self.endpoints.len() {
            if self.upstream(k) && matches!(self.conns[k].phase, Phase::Opening | Phase::Ready) {
                self.end(t, k, false);
                self.events.push_back((t, WorldEvent::State { c: k as u8, state: OFFLINE, code: code::RESET, detail: 0 }));
            }
        }
    }

    /// The upstream connections opening or open: (c, attempt).
    pub fn upstream_attempts(&self) -> Vec<(u8, u32)> {
        (0..self.endpoints.len()).filter(|&k| self.upstream(k) && matches!(self.conns[k].phase, Phase::Opening | Phase::Ready))
            .map(|k| (k as u8, self.conns[k].gen)).collect()
    }

    fn upstream(&self, k: usize) -> bool { self.endpoints.get(k).is_some_and(|e| e.responder == Responder::Upstream) }

    /// One part of a message from the device on a ready `c`; `flags` are `PART_*`.
    pub fn send(&mut self, t: u64, c: u8, flags: u8, bytes: &[u8]) {
        let k = c as usize;
        let upstream = self.upstream(k);
        let Some(conn) = self.conns.get_mut(k).filter(|conn| conn.phase == Phase::Ready) else { return };
        if upstream {
            conn.held += bytes.len() as u64;
            self.requests.push_back((t, Request::Send { c, gen: conn.gen, flags, bytes: bytes.to_vec() }));
            return;
        }
        let done = conn.up_free.max(t) + take_time(bytes.len(), conn.rate);
        conn.up_free = done;
        conn.up.push_back((done, bytes.len() as u32));
        if flags & PART_FIRST != 0 {
            conn.partial.clear();
            conn.partial_binary = flags & PART_BINARY != 0;
        }
        conn.partial.extend_from_slice(bytes);
        if flags & PART_LAST != 0 {
            let message = core::mem::take(&mut conn.partial).into();
            let (gen, binary) = (conn.gen, conn.partial_binary);
            let at = done + self.delay(k);
            self.schedule(at, Due::Serve { c, gen, binary, bytes: message });
        }
    }

    /// The upstream bytes `c` takes at `t`: its window less what it has not yet drained.
    pub fn room(&self, t: u64, c: u8) -> u32 {
        if self.upstream(c as usize) {
            return FIRST_GRANT.saturating_sub(self.conns[c as usize].held.min(u64::from(FIRST_GRANT)) as u32);
        }
        let held: u64 = self.conns.get(c as usize).map_or(0, |conn| conn.up.iter().filter(|(done, _)| *done > t).map(|(_, n)| u64::from(*n)).sum());
        FIRST_GRANT.saturating_sub(held as u32)
    }

    /// The first time at or after `t` when `c`'s room reaches `room` (None: never, as things stand).
    pub fn room_at(&self, t: u64, c: u8, room: u32) -> Option<u64> {
        if self.room(t, c) >= room { return Some(t); }
        if self.upstream(c as usize) { return None; }
        let conn = self.conns.get(c as usize)?;
        let mut held: u64 = conn.up.iter().filter(|(done, _)| *done > t).map(|(_, n)| u64::from(*n)).sum();
        for &(done, n) in conn.up.iter().filter(|(done, _)| *done > t) {
            held -= u64::from(n);
            if FIRST_GRANT as u64 - held.min(FIRST_GRANT as u64) >= u64::from(room) { return Some(done); }
        }
        None
    }

    fn schedule(&mut self, t: u64, due: Due) {
        self.seq += 1;
        self.due.push((t, self.seq, due));
    }

    fn delay(&mut self, k: usize) -> u64 {
        let (latency, jitter) = (self.conns[k].latency, self.conns[k].jitter);
        if jitter == 0 { return latency; }
        // xorshift64*: one draw per crossing, in call order, so a seed replays exactly.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        latency + self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d) % (jitter + 1)
    }

    /// Ends `c`'s attempt; with `tell`, an upstream endpoint's network is asked to close it.
    fn end(&mut self, t: u64, k: usize, tell: bool) {
        if tell && self.upstream(k) && matches!(self.conns[k].phase, Phase::Opening | Phase::Ready) {
            self.requests.push_back((t, Request::Close { c: k as u8, gen: self.conns[k].gen }));
        }
        let conn = &mut self.conns[k];
        conn.phase = Phase::Idle;
        conn.gen = conn.gen.wrapping_add(1);
        conn.up.clear();
        conn.partial.clear();
        conn.held = 0;
    }

    fn attempt(&mut self, t: u64, k: usize) {
        if self.upstream(k) {
            self.conns[k].phase = Phase::Opening;
            self.requests.push_back((t, Request::Open { c: k as u8, gen: self.conns[k].gen }));
            return;
        }
        let delay = self.delay(k);
        let conn = &mut self.conns[k];
        conn.phase = Phase::Opening;
        let at = match conn.outcome {
            Outcome::NotConfigured => t,
            Outcome::NameNotFound | Outcome::Refused => t + delay,
            Outcome::TimedOut => t + OPEN_TIMEOUT_NS,
            Outcome::Ready | Outcome::Rejected(_) => t + 2 * delay,
        };
        let gen = conn.gen;
        self.schedule(at, Due::OpenEnd { c: k as u8, gen });
    }

    /// Ends every connection that is not idle with `code` and `detail`.
    fn end_all(&mut self, t: u64, code: u8, detail: u32) {
        for k in 0..self.endpoints.len() {
            if self.conns[k].phase != Phase::Idle {
                self.end(t, k, true);
                self.events.push_back((t, WorldEvent::State { c: k as u8, state: OFFLINE, code, detail }));
            }
        }
    }

    fn set_link(&mut self, t: u64, link: (u8, u8, u32)) {
        if link != self.link {
            self.link = link;
            self.events.push_back((t, WorldEvent::Link { state: link.0, code: link.1, detail: link.2 }));
        }
    }

    fn apply_conditions(&mut self, t: u64) {
        while let Some((at, condition)) = self.timeline.get(self.next_condition).filter(|(at, _)| *at <= t).cloned() {
            self.next_condition += 1;
            match condition {
                Condition::Join { ns, fails } => {
                    self.end_all(at, code::LINK_LOST, 0);
                    self.due.retain(|(_, _, due)| !matches!(due, Due::JoinEnd));
                    self.join = fails;
                    self.set_link(at, (LINK_JOINING, code::NONE, 0));
                    self.schedule(at + ns, Due::JoinEnd);
                }
                Condition::LinkDown { code, detail } => {
                    self.due.retain(|(_, _, due)| !matches!(due, Due::JoinEnd));
                    self.end_all(at, code::LINK_LOST, detail);
                    self.set_link(at, (LINK_NONE, code, detail));
                }
                Condition::Open { c, outcome } => if let Some(conn) = self.conns.get_mut(c as usize) { conn.outcome = outcome },
                Condition::Latency { c, ns, jitter_ns } => if let Some(conn) = self.conns.get_mut(c as usize) {
                    conn.latency = ns;
                    conn.jitter = jitter_ns;
                },
                Condition::Rate { c, bytes_per_s } => if let Some(conn) = self.conns.get_mut(c as usize) { conn.rate = bytes_per_s },
                Condition::Disconnect { c, code, detail } => {
                    let k = c as usize;
                    if self.conns.get(k).is_some_and(|conn| conn.phase != Phase::Idle) {
                        self.end(at, k, true);
                        self.events.push_back((at, WorldEvent::State { c, state: OFFLINE, code, detail }));
                    }
                }
            }
        }
    }

    /// A rules endpoint's answer to its connection going ready (`message` None) or to a message.
    fn answer(&mut self, t: u64, c: u8, gen: u32, message: Option<(bool, Payload)>) {
        let Responder::Rules(rules) = &self.endpoints[c as usize].responder else { return };
        let rules = rules.clone();
        let found = rules.0.iter().position(|rule| match (&rule.when, &message) {
            (When::Ready, None) => true,
            (When::Message { binary, bytes, whole }, Some((kind, got))) =>
                binary == kind && got.starts_with(bytes) && (!whole || got.len() == bytes.len()),
            _ => false,
        });
        let Some(k) = found else { return };
        let rule = &rules.0[k];
        let mut sent = 0;
        if rule.stop {
            let before = self.due.len();
            self.due.retain(|(_, _, due)| match due {
                Due::Step { c: on, run, .. } if *on == c => {
                    sent += run.sent;
                    false
                }
                _ => true,
            });
            if self.due.len() == before { return; }
        }
        let Some(first) = rule.then.first() else { return };
        let matched = message.map_or_else(|| Payload::from(&[][..]), |(_, bytes)| bytes);
        self.schedule(t + first.after_ns, Due::Step { c, gen, run: Run { rule: k, matched, then: 0, at: 0, sent } });
    }

    /// Makes a rule's next send at `t` and schedules the one after.
    fn step(&mut self, t: u64, c: u8, gen: u32, mut run: Run) {
        let Responder::Rules(rules) = &self.endpoints[c as usize].responder else { return };
        let rules = rules.clone();
        let then = &rules.0[run.rule].then;
        let Some(send) = then.get(run.then) else { return };
        let left = send.body.len() - run.at;
        let len = if send.block == 0 { left } else { send.block.min(left) };
        let unit = send.unit.max(1);
        let bytes = if send.head.is_empty() && len == send.body.len() {
            send.body.clone()
        } else {
            let mut bytes = Vec::with_capacity(len + 16);
            for field in &send.head {
                let word = |n: u8| run.matched.split(|b| *b == b' ').nth(usize::from(n)).unwrap_or(&[]);
                let number = |n: u8| core::str::from_utf8(word(n)).ok().and_then(|w| w.parse::<u64>().ok()).unwrap_or(0);
                match field {
                    Field::Bytes(b) => bytes.extend_from_slice(b),
                    Field::Word(n) => bytes.extend_from_slice(word(*n)),
                    Field::Word16(n) => bytes.extend_from_slice(&(number(*n) as u16).to_le_bytes()),
                    Field::Word32(n) => bytes.extend_from_slice(&(number(*n) as u32).to_le_bytes()),
                    Field::Offset32 => bytes.extend_from_slice(&((run.at / unit) as u32).to_le_bytes()),
                    Field::Sent => decimal(&mut bytes, run.sent),
                }
            }
            bytes.extend_from_slice(&send.body[run.at..run.at + len]);
            bytes.into()
        };
        self.run(t, Due::Send { c, gen, binary: send.binary, bytes });
        if send.binary { run.sent += (len / unit) as u64; }
        run.at += len;
        if run.at < send.body.len() {
            self.schedule(t + send.every_ns, Due::Step { c, gen, run });
        } else if let Some(next) = then.get(run.then + 1) {
            run.then += 1;
            run.at = 0;
            self.schedule(t + next.after_ns, Due::Step { c, gen, run });
        }
    }

    fn run(&mut self, t: u64, due: Due) {
        match due {
            Due::JoinEnd => match self.join.take() {
                None => {
                    self.set_link(t, (LINK_UP, code::NONE, 0));
                    for k in 0..self.endpoints.len() {
                        if self.conns[k].phase == Phase::WaitLink { self.attempt(t, k); }
                    }
                }
                Some((code, detail)) => {
                    for k in 0..self.endpoints.len() {
                        if self.conns[k].phase == Phase::WaitLink {
                            self.end(t, k, false);
                            self.events.push_back((t, WorldEvent::State { c: k as u8, state: OFFLINE, code, detail }));
                        }
                    }
                    self.set_link(t, (LINK_NONE, code, detail));
                }
            },
            Due::OpenEnd { c, gen } => {
                let k = c as usize;
                if self.conns[k].gen != gen || self.conns[k].phase != Phase::Opening { return; }
                let (code, detail) = match self.conns[k].outcome {
                    Outcome::Ready => (code::NONE, 0),
                    Outcome::NotConfigured => (code::NOT_CONFIGURED, 0),
                    Outcome::NameNotFound => (code::NAME_NOT_FOUND, 3),
                    Outcome::Refused => (code::REFUSED, 0),
                    Outcome::TimedOut => (code::TIMED_OUT, 0),
                    Outcome::Rejected(status) => (code::REJECTED, u32::from(status)),
                };
                if code != code::NONE {
                    self.end(t, k, false);
                    self.events.push_back((t, WorldEvent::State { c, state: OFFLINE, code, detail }));
                    return;
                }
                let conn = &mut self.conns[k];
                conn.phase = Phase::Ready;
                conn.up_free = t;
                conn.down_free = t;
                self.events.push_back((t, WorldEvent::State { c, state: READY, code: code::NONE, detail: 0 }));
                if let Responder::Service(served) = &self.endpoints[k].responder {
                    let sends = served.0.ready();
                    for (binary, bytes) in sends { self.run(t, Due::Send { c, gen, binary, bytes }); }
                }
                self.answer(t, c, gen, None);
            }
            Due::Serve { c, gen, binary, bytes } => {
                if self.conns[c as usize].gen != gen { return; }
                match self.endpoints[c as usize].responder {
                    Responder::Echo => self.run(t, Due::Send { c, gen, binary, bytes }),
                    Responder::Rules(_) => self.answer(t, c, gen, Some((binary, bytes))),
                    Responder::Service(ref served) => {
                        let sends = served.0.answer(binary, &bytes);
                        for (binary, bytes) in sends { self.run(t, Due::Send { c, gen, binary, bytes }); }
                    }
                    Responder::Sink | Responder::Upstream => {}
                }
            }
            Due::Step { c, gen, run } => if self.conns[c as usize].gen == gen { self.step(t, c, gen, run) },
            Due::Send { c, gen, binary, bytes } => {
                let k = c as usize;
                if self.conns[k].gen != gen { return; }
                let delay = self.delay(k);
                let conn = &mut self.conns[k];
                let at = conn.down_free.max(t + delay) + take_time(bytes.len(), conn.rate);
                conn.down_free = at;
                self.schedule(at, Due::Arrive { c, gen, binary, bytes });
            }
            Due::Arrive { c, gen, binary, bytes } => {
                if self.conns[c as usize].gen == gen {
                    self.events.push_back((t, WorldEvent::Message { c, binary, bytes }));
                }
            }
        }
    }
}

fn decimal(out: &mut Vec<u8>, mut n: u64) {
    let mut digits = [0u8; 20];
    let mut k = digits.len();
    loop {
        k -= 1;
        digits[k] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 { break; }
    }
    out.extend_from_slice(&digits[k..]);
}
