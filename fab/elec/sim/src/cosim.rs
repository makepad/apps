//! Co-simulation: a process that runs inside the live worker at exact simulated instants (the
//! virtual MCU), reading pad voltages and writing GPIO pin modes through a [`Bus`].
//!
//! - Time. `LiveSim::advance` steps the analog so that a step lands bit-exactly on
//!   [`CoSim::next`] (a stepper breakpoint), then calls [`CoSim::run`]; `next` is read again after
//!   every `run`. Inside `run`, [`Bus::advance`] steps to exactly the time it is given (a time at or
//!   before now is a no-op) through the same per-point body as the worker's own steps (sinks,
//!   trace, interval statistics), so a nested advance is an ordinary stretch of the record.
//! - Pins. A pin is a row of the device's pin table ([`CoSimPin`]): its GPIO device, its pad, its
//!   rails and its input thresholds; a device sets its watch levels from [`Bus::supply`].
//!   [`Bus::write`] sets a GPIO mode at the bus's time; a PWM written over a PWM is a preload at the
//!   next period start of the running grid (the GPIO bank's rule).
//! - Watches. [`CoSim::watches`] lists pad voltage thresholds. The GPIO bank localises a crossing
//!   the way the stepper localises a switching instant (the accepted point lies within 0.1 % of
//!   its step past the crossing; the hit carries the interpolated crossing time). A hit found while
//!   the worker steps on its own stops the stepping there and calls `run` with the hit in
//!   [`Bus::hits`]; a hit found inside a nested `advance` is appended to `hits()` and never
//!   re-enters `run`. The hits are cleared when `run` returns. The watch list is read again when
//!   [`CoSim::watch_gen`] changes, at the end of each `run` (a watch set during a `run` applies
//!   from its end).
//! - Determinism: everything is a function of simulated time; the worker's wall-clock budget only
//!   decides when it pauses between steps, never inside a `run`.
//! - Stepping (`LiveCmd::CoStep`). A stop is a predicate armed on the device ([`CoSim::arm`]),
//!   never a time: it holds only at a cycle after the arming cycle, so a stretch re-run after an
//!   input hit finds it again. The device ends its private run before the stopping instruction,
//!   names that instant as `next()`, and when the analog has landed there `run` returns
//!   [`CoRun::Stopped`] without executing it and disarms. The worker then pauses and publishes at
//!   once. Steps are not paced by the rate, and commands are not held behind them (a button press
//!   reaches the circuit while a step waits for its handler).
//! - State. [`CoSim::state`] is the device's state at the analog instant of a publication, carried
//!   opaque in `LiveValues::co` ([`CoState`]); the kernel learns no firmware types.
//! - Logic nets. A device may keep nets off the circuit ([`CoSim::logic_nets`]: a shift chain's
//!   clock and data beside an MCU). Their level changes come out committed, never from a run the
//!   device discards, through [`CoSim::take_logic`]: the worker drains them after each `run` and,
//!   with a sync, before a trace delta goes out, so a delta holds every change up to its end. They
//!   become the logic traces of the plan's nets of those names. A new record (`LiveCmd::Replace`)
//!   starts from every net's level at its first instant ([`CoSim::restart_logic`]).

use crate::circuit::{PinEdge, PinMode};
use crate::newton::SimDiag;
use crate::trace::LogicEdge;
use std::any::Any;
use std::fmt;
use std::sync::{Arc, Mutex};

/// Where a step stops the device (`LiveCmd::CoStep`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StepTo {
    /// The next statement that begins executing (into called functions; from sleep, the first
    /// statement of the next handler entered).
    Statement,
    /// The next entry of any handler.
    HandlerEntry,
    /// The return of the handler running at the arming; refused when none runs.
    HandlerExit,
}

/// How a `run` ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoRun {
    /// Ran what was due; the device goes on at `next()`.
    Go,
    /// An armed step's stop: the analog stands on it, nothing at that instant was executed, and
    /// the device has disarmed.
    Stopped,
}

/// A device's state for one publication (`CoSim::state`), opaque to the kernel: a view downcasts
/// it to the device's own type. Two are equal when they are the same `Arc`.
#[derive(Clone)]
pub struct CoState(pub Arc<dyn Any + Send + Sync>);

impl CoState {
    /// The state as the device's own type.
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl fmt::Debug for CoState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CoState")
    }
}

impl PartialEq for CoState {
    fn eq(&self, other: &CoState) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Sound a device played (its speaker), committed: runs of consecutive samples back to back in
/// `samples`, each run from its first sample's run time at its exact rate, samples at the
/// speaker's full scale. Never from a stretch the device may still run again. Buffers are reused:
/// `clear` keeps their storage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoAudio {
    pub runs: Vec<CoAudioRun>,
    pub samples: Vec<i16>,
}

/// One run of [`CoAudio`]: its first sample's run time, seconds; its rate in Hz as a ratio
/// (numerator, denominator); its sample count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoAudioRun {
    pub t0: f64,
    pub rate: (u64, u64),
    pub len: usize,
}

impl CoAudio {
    pub fn clear(&mut self) {
        self.runs.clear();
        self.samples.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// The samples of each run, in order.
    pub fn each(&self) -> impl Iterator<Item = (&CoAudioRun, &[i16])> {
        self.runs.iter().scan(0, |at, run| {
            let start = *at;
            *at += run.len;
            Some((run, &self.samples[start..*at]))
        })
    }
}

/// A pad voltage threshold the device wants to hear about.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Watch {
    /// The device's own id, returned with each hit.
    pub id: u32,
    /// The pin table row.
    pub pin: u32,
    /// Volts above the pin's low rail.
    pub level: f64,
    pub edge: PinEdge,
}

/// One row of a device's pin table, by name in the lowering: the package pin, its GPIO device, its
/// pad node, the low rail node pad voltages are read against ("0" for ground), its supply node and
/// its input thresholds as fractions of the supply (the part's GPIO card, or the generic profile).
#[derive(Clone, Debug, PartialEq)]
pub struct CoSimPin {
    pub name: String,
    pub gpio: String,
    pub pad: String,
    pub vss: String,
    pub vdd: String,
    pub vih: f64,
    pub vil: f64,
}

/// A process co-simulated with the analog in the live worker.
pub trait CoSim: Send {
    /// A host input at the run's time `t`, a step boundary; false means refused, or not this
    /// process's kind.
    fn input(&mut self, _t: f64, _input: &dyn Any) -> bool {
        false
    }
    /// The next simulated time this process must run at, seconds (INFINITY: none). The analog
    /// lands on exactly this f64 and never passes it; a time at or before now runs at once.
    fn next(&self) -> f64;
    /// How far past the analog this process reads a streamed input (`crate::input`), seconds: run
    /// with the analog at `t`, it reads none later than `t + span`. A constant of the process.
    /// None (the default): it reads none ahead of the analog.
    fn input_span(&self) -> Option<f64> {
        None
    }
    /// The kernel reads this process soon (a publication, a snapshot): at the next instant it
    /// stands committed with the analog, it stays there, running no further privately, until its
    /// next `run`. `false` withdraws the request. Moves no point: only where the work is done.
    fn settle(&mut self, _want: bool) {}
    /// A read at the analog's present (`state`, `take_logic` with sync) costs no re-run. A process
    /// that never runs ahead of the analog is always settled.
    fn settled(&self) -> bool {
        true
    }
    /// A copy that goes on exactly as this one would, with no event sink (a run's snapshot,
    /// `LiveSim::snapshot`). None: this process cannot be copied, and its run takes no snapshots.
    fn fork(&self) -> Option<Box<dyn CoSim>> {
        None
    }
    /// The analog stands at `bus.t()`: `next()`, or an accepted watch hit (`bus.hits()`).
    /// `Stopped` when `bus.t()` is an armed step's stop (see the module). A fault in the device
    /// never stops the simulation: a device that cannot go on answers `next() = INFINITY`.
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag>;
    /// The watches wanted now.
    fn watches(&self) -> &[Watch];
    /// Changes whenever `watches` does.
    fn watch_gen(&self) -> u32;
    /// The analog stands at `t`. Some: stop at the next such point after the cycle `t` falls in
    /// (rounded up), which becomes `next()`; None: disarm. False when this device cannot: no
    /// statement map, a `HandlerExit` with no handler running, or nothing will ever run (an
    /// infinite `next()` and no watches).
    fn arm(&mut self, t: f64, to: Option<StepTo>) -> bool;
    /// The device's state at the analog instant `t`, for the publication made now (None: it has
    /// none to show).
    fn state(&mut self, t: f64) -> Option<CoState>;
    /// The nets the device keeps off the circuit, by index in `take_logic`, by name.
    fn logic_nets(&self) -> &[String] {
        &[]
    }
    /// The analog stands at `t`: appends the committed level changes of `logic_nets` at or before
    /// `t`, in time order, and removes them; later ones wait. With `sync` the device first commits
    /// up to `t` (as `state` does), so every change at or before `t` is among them. A caller that
    /// runs the device drains it.
    fn take_logic(&mut self, _t: f64, _sync: bool, _out: &mut Vec<(u32, LogicEdge)>) {}
    /// A new record starts at the analog instant `t`: commits up to `t`, drops the changes at or
    /// before it not yet taken, and appends every logic net's level at `t` as an entry at `t` (a
    /// net with no level yet has none).
    fn restart_logic(&mut self, _t: f64, _out: &mut Vec<(u32, LogicEdge)>) {}
    /// Appends the sound it played and committed since the last take, in time order, and
    /// removes it. A caller that runs the device drains it after each `run`.
    fn take_audio(&mut self, _out: &mut CoAudio) {}
    /// The analog stands at `t` and another process replaces this one (a new image, flashed into
    /// the running chip): its non-volatile memory (an MCU's EEPROM) as committed at `t`, which the
    /// new one keeps (`set_nonvolatile`). None: it has none.
    fn nonvolatile(&mut self, _t: f64) -> Option<Arc<[u8]>> {
        None
    }
    /// Before its first run, takes `bytes` into its non-volatile memory, and nowhere else: the
    /// memory of the process it replaces (`nonvolatile`), or the bytes kept from earlier runs.
    /// False, and nothing changed: it has none of that size, or it has run.
    fn set_nonvolatile(&mut self, _bytes: &[u8]) -> bool {
        false
    }
    /// The run goes back to an earlier copy of this process (`fork`; a live edit behind the
    /// front, `LiveCmd::Edit`): hands over the event stream (its sink and what it has not taken
    /// yet) and the machine's feeds, such as screens, for the copy to take over (`put_outputs`).
    /// None: it has neither. A fork carries neither sink nor feeds.
    fn take_outputs(&mut self) -> Option<Box<dyn Any + Send>> {
        None
    }
    /// Takes over the event stream and machine feeds from `take_outputs` on the process this is an
    /// earlier copy of. What that one sent past the point this copy stands at is void, since this
    /// one sends it again: its first offer says so, in the stream, and what that one had not sent
    /// yet goes only up to that point. Each feed sends this copy's whole state at its next publication.
    fn put_outputs(&mut self, _outputs: Box<dyn Any + Send>) {}
}

/// The analog as a co-simulated process sees it during `CoSim::run`.
pub trait Bus {
    /// Simulated time, seconds.
    fn t(&self) -> f64;
    /// A pin's pad voltage above its low rail, now (NaN for a row whose pad is not in the circuit).
    fn v(&self, pin: u32) -> f64;
    /// A pin's supply, its vdd above its low rail, now: what its `vih` and `vil` are fractions of
    /// (NaN for a row whose rails are not in the circuit).
    fn supply(&self, pin: u32) -> f64;
    /// Steps the analog to exactly `t`; a `t` at or before `self.t()` is a no-op.
    fn advance(&mut self, t: f64) -> Result<(), SimDiag>;
    /// Writes a pin's mode at `t()`; false for a row without a GPIO or an invalid mode.
    fn write(&mut self, pin: u32, mode: PinMode) -> bool;
    /// The watch hits of this `run`, in time order: (watch id, interpolated crossing time).
    fn hits(&self) -> &[(u32, f64)];
}

/// A co-simulated process and its pin table, as a live bench is given it.
pub struct CoSimDevice {
    pub dev: Box<dyn CoSim>,
    pub pins: Vec<CoSimPin>,
}

/// A process carried by a command (`LiveCmd::CoSim`, which is `Clone` and `PartialEq`): the worker
/// that applies the command takes it, once. Clones share the one process; two slots are equal
/// when they are the same slot. An empty slot (or one already taken) detaches.
#[derive(Clone, Default)]
pub struct CoSimSlot(Arc<Mutex<Option<CoSimDevice>>>);

impl CoSimSlot {
    pub fn new(device: Option<CoSimDevice>) -> CoSimSlot {
        CoSimSlot(Arc::new(Mutex::new(device)))
    }

    /// The process, the first time.
    pub fn take(&self) -> Option<CoSimDevice> {
        self.0.lock().ok()?.take()
    }
}

impl fmt::Debug for CoSimSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CoSimSlot")
    }
}

impl PartialEq for CoSimSlot {
    fn eq(&self, other: &CoSimSlot) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
