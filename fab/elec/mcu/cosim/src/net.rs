//! The host end of an image's network ports in the co-simulation: the image's NET frames on link 0
//! reach an `rt::net::Host` over a `World` (the bench's fake servers, and for the run that holds
//! the network the real ones, through an input log), and the host's messages go back down link 0
//! as a board's host sends them on the tether.
//!
//! - Taken at every commit, answered at paced ones. The device's frames are committed output, so
//!   any commit takes them, each stamped with its own tick. The host speaks only at a paced commit
//!   (a deadline, a stop, a barrier, a halt, a reset or the attach), whose place does not depend on
//!   when a publication synced the device: a re-run gives the device the same bytes at the same
//!   ticks. A message the host has due goes down at the first paced commit at or after its time,
//!   at most one stretch late, as a USB host's answer waits for its next poll.
//! - Time is board time from the attach, in ns, across chip resets.
//! - The network ([`Upstream`]) belongs to one copy of a run, the owner, and moves with its outputs
//!   (`CoSim::take_outputs`, `put_outputs`); a fork holds none. Every copy of a run shares its log
//!   (`rt::net::upstream`), so a copy replays what the network answered until it runs differently.
//!
//! Cost per paced commit: the frames taken since the last one, plus one poll of the host; nothing
//! when the image sent nothing and the host has nothing due.

use fab_elec_mcu_rt::net::{self, Cursor, Dir, Host, NetMsg, UpstreamLog, World};
/// What a view needs to read a [`NetStatus`], and a host's network what it speaks.
pub use fab_elec_mcu_rt::net::{code, code_text, HostError, CONNECTING, LINK_JOINING, LINK_NONE, LINK_UP, OFFLINE, READY};
pub use fab_elec_mcu_rt::net::{Arrival, Network, Request};
use fab_elec_mcu_rt::watch;
use std::sync::{Arc, Mutex};

/// The real network the run holding it reaches (the app's transport, or a test's stand-in).
pub type Upstream = Box<dyn Network + Send>;

/// A run's input log, shared by every copy of the run.
pub type SharedLog = Arc<Mutex<UpstreamLog>>;

/// What a device's host end starts with.
#[derive(Clone, Debug)]
pub struct NetSetup {
    /// The servers each connection reaches, and the link's timeline.
    pub world: World,
    /// Per connection, the receive slots the image declares for it.
    pub slots: Vec<u16>,
    /// The run's log: a new one per run from reset, shared by its copies.
    pub log: SharedLog,
}

/// A connection as the host last told the device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetConn {
    pub c: u8,
    /// `net::OFFLINE`, `CONNECTING` or `READY`.
    pub state: u8,
    /// `net::code`; with `detail`, why it is in that state.
    pub code: u8,
    pub detail: u32,
}

/// The host end's account, published with the device's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetStatus {
    /// The link as the host last told the device: (state, code, detail); None before the first Hello.
    pub link: Option<(u8, u8, u32)>,
    /// Every connection the host has told the device about, by number.
    pub conns: Vec<NetConn>,
    /// Messages and data bytes each way.
    pub up: u64,
    pub down: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
    /// Device messages the host refused (a ports-layer fault, never a race), and the last refusal.
    pub refused: u64,
    pub last_refusal: Option<HostError>,
    /// This copy holds the network.
    pub owner: bool,
    /// Times the owner ran differently from what the network had seen, and the log was cut.
    pub cuts: u64,
}

#[derive(Clone, Debug)]
enum Taken {
    /// A device Hello with its tick, echoed in every host packet after it.
    Hello(u32),
    Msg(NetMsg),
}

#[derive(Clone)]
pub(crate) struct NetEnd {
    host: Host,
    at: Cursor,
    pub(super) log: SharedLog,
    /// Ticks of the epochs before the current one (each chip reset restarts the tick at zero).
    base: u64,
    /// Committed device frames the host has not taken, with their board time.
    taken: Vec<(u64, Taken)>,
    /// The device's latest Hello tick (None: none yet, and the host stays silent).
    epoch: Option<u32>,
    seq: u8,
    /// The newest time this run has processed.
    done: u64,
    answers: Vec<Arrival>,
    status: NetStatus,
}

impl NetEnd {
    pub(crate) fn new(setup: NetSetup) -> NetEnd {
        NetEnd {
            host: Host::new(setup.world, &setup.slots), at: Cursor::default(), log: setup.log, base: 0,
            taken: Vec::new(), epoch: None, seq: 0, done: 0, answers: Vec::new(), status: NetStatus::default(),
        }
    }

    fn ns(&self, hz: u64, tick: u64) -> u64 {
        let ns = u128::from(self.base + tick) * 1_000_000_000 / u128::from(hz);
        u64::try_from(ns).expect("board time in ns fits 64 bits")
    }

    /// A committed frame of link 0 at `tick`, decoded; `body` is the reader's lent Net payload.
    pub(crate) fn take(&mut self, hz: u64, tick: u64, frame: &watch::Frame, body: &[u8]) {
        let t = self.ns(hz, tick);
        match frame.event {
            watch::Event::Hello { .. } => self.taken.push((t, Taken::Hello(frame.tick))),
            watch::Event::Net { .. } => {
                // the reader passes only well-formed device messages
                let msg = NetMsg::decode(frame.kind, Dir::Up, body).expect("the watch reader checked the message");
                self.taken.push((t, Taken::Msg(msg)));
            }
            _ => {}
        }
    }

    /// A chip reset at tick `at` of the epoch it ends.
    pub(crate) fn reset(&mut self, at: u64) {
        self.base += at;
    }

    /// A paced commit at tick `now`: the host takes what the device sent and what the network
    /// answered, then sends the device everything it has due, through `down` (whole packets).
    pub(crate) fn step(&mut self, hz: u64, now: u64, mut network: Option<&mut Upstream>, down: &mut dyn FnMut(&[u8])) {
        let now = self.ns(hz, now);
        let NetEnd { host, at, log, taken, epoch, seq, done, answers, status, .. } = self;
        let mut log = log.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        macro_rules! network { () => { network.as_mut().map(|n| &mut ***n as &mut dyn Network) } }
        if let Some(n) = network.as_mut() {
            n.answers(answers);
            for arrival in answers.drain(..) { log.record(*done, arrival); }
        }
        for (t, item) in taken.drain(..) {
            net::deliver(host, &mut log, at, *done, t, network!());
            match item {
                Taken::Hello(tick) => {
                    host.hello(t);
                    *epoch = Some(tick);
                }
                Taken::Msg(msg) => {
                    status.up += 1;
                    if let NetMsg::Data { bytes, .. } = &msg { status.bytes_up += bytes.len() as u64; }
                    if let Err(refusal) = host.from_device(t, &msg) {
                        status.refused += 1;
                        status.last_refusal = Some(refusal);
                    }
                }
            }
            *done = (*done).max(t);
            net::exchange(host, &mut log, at, *done, network!());
        }
        net::deliver(host, &mut log, at, *done, now, network!());
        *done = (*done).max(now);
        // the host's packets echo the device's Hello: before one it has nothing to say
        if let Some(epoch) = *epoch {
            let mut wire = [0; watch::ETH_WIRE];
            while let Some((_, msg)) = host.poll(now) {
                let len = watch::encode_net(*seq, epoch, &msg, &mut wire);
                *seq = seq.wrapping_add(1);
                down(&wire[..len]);
                status.down += 1;
                match msg {
                    NetMsg::Data { bytes, .. } => status.bytes_down += bytes.len() as u64,
                    NetMsg::Link { state, code, detail } => status.link = Some((state, code, detail)),
                    NetMsg::State { c, state, code, detail } => {
                        let conn = NetConn { c, state, code, detail };
                        match status.conns.binary_search_by_key(&c, |k| k.c) {
                            Ok(k) => status.conns[k] = conn,
                            Err(k) => status.conns.insert(k, conn),
                        }
                    }
                    _ => {}
                }
            }
        }
        net::exchange(host, &mut log, at, *done, network!());
        status.cuts = log.cuts;
    }

    pub(crate) fn status(&self, owner: bool) -> NetStatus {
        NetStatus { owner, ..self.status.clone() }
    }
}

/// The run's network: the copy that holds it is the owner. A fork's clone holds none.
pub(crate) struct Owned(pub(crate) Option<Upstream>);

impl Clone for Owned {
    fn clone(&self) -> Self {
        Owned(None)
    }
}
