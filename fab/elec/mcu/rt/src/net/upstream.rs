//! The real network behind a world's upstream endpoints, through an input log: what arrives enters
//! with a virtual time, so a replay is exact, and a rewound run that then runs differently cannot
//! rewind the server, so its connections read as reset.
//!
//! - A [`Request`] is what a run asks of the network, an [`Arrival`] what the network answers, each
//!   for one attempt (`gen`) of a connection, so an answer to an ended attempt is dropped.
//! - [`UpstreamLog`] holds both and outlives rewinds; a run's place in it ([`Cursor`]) is part of
//!   the run's state, so a rewind takes it back.
//! - A run at the log's end is live: what it asks goes out and is logged. A run behind it replays:
//!   each request must be the logged one (same time, same content), nothing goes out, and the
//!   arrivals come from the log at their times.
//! - A replaying run that asks something else, or reaches an arrival without having asked what was
//!   asked before it, ran differently. The owner (the one copy of a run that holds the network) cuts
//!   the log there: the network's connections are closed, and the run's are logged as ended with
//!   reset (code 10, the epoch steps), so a replay of the new past meets the same ends; the run is
//!   live from there. Any other copy (a scrub, an ensemble member) never cuts the owner's log and
//!   never goes live: once it runs differently, reaches the log's end or runs past the owner's
//!   time, its upstream connections read as reset.
//! - Every call takes `done`, the newest time the run has processed (device and host alike). An
//!   arrival is stamped just after the owner's newest `done`, so everything already processed comes
//!   before it, in a replay as it did live; a host takes an arrival before anything else at its time
//!   ([`deliver`] first).

use super::{code, Host};
use alloc::vec::Vec;

/// What a run asks of the real network, for attempt `gen` of connection `c`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Open { c: u8, gen: u32 },
    /// One part of a message as the device sent it (`flags` are `PART_*`), so a message of any size
    /// streams through the network's window.
    Send { c: u8, gen: u32, flags: u8, bytes: Vec<u8> },
    Close { c: u8, gen: u32 },
}

/// What the real network answers, for attempt `gen` of connection `c`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arrival {
    /// Connected (a WebSocket's upgrade accepted).
    Opened { c: u8, gen: u32 },
    /// The attempt failed, or the connection ended, with this code and detail.
    Ended { c: u8, gen: u32, code: u8, detail: u32 },
    /// A whole message from the server.
    Message { c: u8, gen: u32, binary: bool, bytes: Vec<u8> },
    /// The network took this many bytes of the connection's messages.
    Taken { c: u8, gen: u32, bytes: u32 },
}

impl Request {
    pub fn attempt(&self) -> (u8, u32) {
        match *self {
            Request::Open { c, gen } | Request::Send { c, gen, .. } | Request::Close { c, gen } => (c, gen),
        }
    }
}

impl Arrival {
    pub fn attempt(&self) -> (u8, u32) {
        match *self {
            Arrival::Opened { c, gen } | Arrival::Ended { c, gen, .. } | Arrival::Message { c, gen, .. } | Arrival::Taken { c, gen, .. } => (c, gen),
        }
    }
}

/// A run's place in an [`UpstreamLog`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    requests: usize,
    arrivals: usize,
    /// A copy that is not the owner left the log: it has no network from here on.
    off: bool,
}

/// What to do with a run's request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asked {
    /// The run asked what it asked before: nothing goes out.
    Replayed,
    /// The run is live: send it.
    Live,
    /// The owner ran differently from here, and the log is cut: close every connection on the
    /// network, log each of the run's upstream attempts as ended with reset, and send nothing more
    /// of what the run asked until then (the ends cover it). [`exchange`] does all of it.
    Cut,
    /// A copy that is not the owner has no network from here: cut the run's upstream, send nothing.
    Off,
}

/// The run's next logged arrival, or why there is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replayed {
    Arrival(u64, Arrival),
    /// Nothing due.
    None,
    /// The owner reached an arrival without having asked what was asked before it: cut as for
    /// [`Asked::Cut`].
    Cut,
    /// A copy that is not the owner did: it has no network from here.
    Off,
}

/// Everything the owner's run asked of the real network and everything it answered, with times.
#[derive(Clone, Debug, Default)]
pub struct UpstreamLog {
    requests: Vec<(u64, Request)>,
    arrivals: Vec<(u64, Arrival)>,
    /// The newest time the owner has processed (None: nothing yet).
    done: Option<u64>,
    /// How many times the owner ran differently from its past.
    pub cuts: u64,
}

impl UpstreamLog {
    pub fn new() -> UpstreamLog { UpstreamLog::default() }

    /// The run has processed everything up to `done`. The owner's moves the log's time on; a copy
    /// that is not the owner and runs past it leaves the log, and the answer is true: cut its
    /// upstream now.
    pub fn ran_to(&mut self, at: &mut Cursor, owner: bool, done: u64) -> bool {
        if owner {
            self.done = Some(self.done.map_or(done, |d| d.max(done)));
            false
        } else if !at.off && self.done.is_none_or(|d| done > d) {
            at.off = true;
            true
        } else {
            false
        }
    }

    /// The run's request at `t`, having processed up to `done`.
    pub fn ask(&mut self, at: &mut Cursor, owner: bool, done: u64, t: u64, request: &Request) -> Asked {
        if at.off { return Asked::Off; }
        match self.requests.get(at.requests) {
            Some((logged_t, logged)) if *logged_t == t && logged == request => {
                at.requests += 1;
                Asked::Replayed
            }
            None if owner => {
                self.requests.push((t, request.clone()));
                at.requests += 1;
                Asked::Live
            }
            _ if owner => {
                self.cut(at, done);
                self.requests.push((t, request.clone()));
                at.requests += 1;
                Asked::Cut
            }
            _ => {
                at.off = true;
                Asked::Off
            }
        }
    }

    /// When the run's next logged arrival is due (None: none to replay).
    pub fn next_arrival(&self, at: &Cursor) -> Option<u64> {
        if at.off { None } else { self.arrivals.get(at.arrivals).map(|(t, _)| *t) }
    }

    /// The run's next logged arrival at or before `now`, having processed up to `done`.
    pub fn replay(&mut self, at: &mut Cursor, owner: bool, done: u64, now: u64) -> Replayed {
        let Some((t, arrival)) = self.arrivals.get(at.arrivals).filter(|(t, _)| !at.off && *t <= now) else { return Replayed::None };
        // A logged request before this arrival can no longer be asked: requests come in time order.
        if self.requests.get(at.requests).is_some_and(|(asked, _)| asked < t) {
            if !owner {
                at.off = true;
                return Replayed::Off;
            }
            self.cut(at, done);
            return Replayed::Cut;
        }
        let out = Replayed::Arrival(*t, arrival.clone());
        at.arrivals += 1;
        out
    }

    /// The network answered: logged just after the owner's newest time (or `done`, if later),
    /// which it returns. The owner's run takes it from the log like any other arrival.
    pub fn record(&mut self, done: u64, arrival: Arrival) -> u64 {
        let done = self.done.map_or(done, |d| d.max(done));
        self.done = Some(done);
        self.arrivals.push((done + 1, arrival));
        done + 1
    }

    /// Everything logged from the run's place on belongs to a past it no longer has.
    fn cut(&mut self, at: &Cursor, done: u64) {
        self.requests.truncate(at.requests);
        self.arrivals.truncate(at.arrivals);
        self.done = Some(done);
        self.cuts += 1;
    }

    /// Requests and arrivals logged.
    pub fn len(&self) -> (usize, usize) { (self.requests.len(), self.arrivals.len()) }

    /// Network arrivals in log order, stamped in board nanoseconds.
    pub fn arrivals(&self) -> &[(u64, Arrival)] { &self.arrivals }

    pub fn is_empty(&self) -> bool { self.requests.is_empty() && self.arrivals.is_empty() }
}

/// The network the owner's run reaches: the real transport, or a test's stand-in. Its answers go to
/// [`UpstreamLog::record`].
pub trait Network {
    fn send(&mut self, request: &Request);
    /// Closes every connection it holds; what they still answer is never handed out.
    fn close_all(&mut self);
    /// Appends everything it answered since the last call, in order.
    fn answers(&mut self, out: &mut Vec<Arrival>);
}

/// The owner's cut: the network closes everything, and the run's upstream attempts are logged as
/// reset, so a replay of the new past meets the same ends.
fn cut<'n>(host: &Host, log: &mut UpstreamLog, done: u64, network: Option<&mut (dyn Network + 'n)>) {
    if let Some(network) = network { network.close_all(); }
    for (c, gen) in host.world().upstream_attempts() {
        log.record(done, Arrival::Ended { c, gen, code: code::RESET, detail: 0 });
    }
}

/// Takes everything the host asked of the network since the last call: replayed, sent (the owner's
/// run, live), or cut. `network` is the owner's; any other copy passes None. Call it after every
/// call into the host, with the newest time the run has processed.
pub fn exchange(host: &mut Host, log: &mut UpstreamLog, at: &mut Cursor, done: u64, mut network: Option<&mut dyn Network>) {
    let owner = network.is_some();
    let mut cutting = false;
    while let Some((t, request)) = host.request() {
        match log.ask(at, owner, done, t, &request) {
            Asked::Replayed => {}
            Asked::Live => if !cutting {
                if let Some(network) = network.as_deref_mut() { network.send(&request); }
            },
            Asked::Cut => cutting = true,
            Asked::Off => host.cut_upstream(t),
        }
    }
    if cutting {
        cut(host, log, done, network);
    } else if log.ran_to(at, owner, done) {
        host.cut_upstream(done);
    }
}

/// Gives the host every logged arrival of the run due at or before `now`: call it before anything
/// else the host or the device does at `now`.
pub fn deliver(host: &mut Host, log: &mut UpstreamLog, at: &mut Cursor, done: u64, now: u64, mut network: Option<&mut dyn Network>) {
    let owner = network.is_some();
    loop {
        match log.replay(at, owner, done, now) {
            Replayed::Arrival(t, arrival) => host.arrive(t, arrival),
            Replayed::None => return,
            Replayed::Cut => cut(host, log, done, network.as_deref_mut()),
            Replayed::Off => host.cut_upstream(now),
        }
    }
}
