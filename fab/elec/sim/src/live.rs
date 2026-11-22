//! Live mode's seam (E3 3, the ESIM brief): what a running simulation publishes and the commands
//! it takes, in kernel terms. Nets, pins and components are indices of the lowered scope;
//! `fab_elec_bench::values::ValueIndex` maps the design's `NetRef` / `PinRef` / `CompRef` onto
//! them, and the bench's `SimCmd` (by `CompRef`) becomes a `LiveCmd` (by kernel device).
//!
//! Threads: one long-lived worker owns the running transient. Commands reach it through a bounded
//! channel the UI sends to without blocking; values leave it through `Latest` (the newest replaces
//! the last, a pointer swap under a lock nobody holds across work) and trace deltas through a
//! channel read with `try_recv`. The solver never waits on a consumer.

use crate::circuit::{PinMode, SimCircuit, Source};
use crate::cosim::{CoSimSlot, CoState, StepTo};
use crate::newton::Rescue;
use std::any::Any;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// What every view reads of a running simulation at one simulated time, published at a steady
/// rate (30 to 60 a second) as `Arc<LiveValues>`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveValues {
    /// Simulated time, seconds.
    pub t: f64,
    /// The lowering this belongs to: it bumps when the scope changes shape (a topology change),
    /// and the indices below are those of that generation's `ValuePlan`.
    pub generation: u64,
    /// Simulated seconds per real second reached over the last publication interval.
    pub ratio: f64,
    /// The bench has settled (`LiveSim::steady`): no publication follows until a command.
    pub steady: bool,
    /// Volts, by net index, at `t`: the instantaneous values, for read-outs.
    pub net_v: Vec<f32>,
    /// Amperes by component-pin index, signed: positive flows into the pin (out of the net), at `t`.
    pub branch_i: Vec<f32>,
    /// Watts dissipated by component index (negative for a source delivering power), at `t`.
    pub part_p: Vec<f32>,
    /// The simulated time the interval statistics below start at: they cover `[since, t]`, the
    /// points accepted since the previous publication. What a view lights (colour, flow, glow) reads
    /// these, never the sample: a PWM net sampled at the frame rate aliases, its mean is steady.
    pub since: f64,
    /// By net: the time-weighted mean (trapezoids over the accepted points), the minimum and the
    /// maximum over the interval.
    pub net_mean: Vec<f32>,
    pub net_min: Vec<f32>,
    pub net_max: Vec<f32>,
    /// By component pin and by component: the interval means of the subscribed pins' currents and
    /// components' dissipation (`LiveCmd::Subscribe`); 0 for the rest.
    pub pin_mean: Vec<f32>,
    pub part_mean: Vec<f32>,
    /// The steps the session took only through the stepper's gmin ladder so far, and the newest
    /// (`Rescue`): a host that sees the count move records the newest one.
    pub rescues: u64,
    pub rescue: Option<Rescue>,
    /// An armed firmware step runs (`LiveCmd::CoStep`): the transport shows "stepping".
    pub stepping: bool,
    /// The run goes on after this publication (playing, a `Step` or a firmware step under way);
    /// false when it stands (paused, or a step's end): a view then shows the sample at `t`.
    pub running: bool,
    /// The worker still has its starting snapshot for `LiveCmd::Restart`.
    pub can_restart: bool,
    /// The attached device's state at `t` (`CoSim::state`), with every publication while one is
    /// attached (`LiveSpec::co_state`): the code view and the LEDs show the same instant.
    pub co: Option<CoState>,
    /// The device's share of the wall time the worker spent stepping over the interval (its
    /// `run`s, not counting the analog steps a run asks for), 0..1.
    pub co_share: f32,
    /// The sink's share of that time (the scopes' acquisition, in every step), 0..1. The solver had
    /// the rest: the three name what limits the rate reached.
    pub sink_share: f32,
    /// The time resolution the run steps at (`LiveSpec::res`): 0 is Exact.
    pub res: f64,
    /// The run's grid in simulated time (`LiveSpec::grid`), seconds a frame; 0 for none.
    pub grid: f64,
    /// The grid frames the run closed since the previous publication, in time order: what the
    /// one take keeps (a host that keeps every publication, `Latest::keep_all`, has them all).
    pub frames: Vec<GridFrame>,
    /// Where the worker's wall went over its latest window of running (`run_live`, one every
    /// `livesim::SPLIT_EVERY` of wall), numbered: a host logs each new one.
    pub worker_split: Option<(u64, Arc<str>)>,
    /// Why the run's fast mode (its round plan) is off: the newest plan failure
    /// (`LiveStats::plan_error`), the same Arc until another; None while no plan failed.
    pub plan_error: Option<Arc<str>>,
    /// The run held since the previous publication, and for what: a streamed input when any of
    /// its holds was for one (`crate::input`), else a sink (`Sink::holds`). Set on the publication
    /// made as a hold begins and on the next after it; None when the run did not hold.
    pub held: Option<crate::analysis::HoldKind>,
    /// The heard run is stepping its set-up or waiting for its first plan (`LiveSim::preparing`).
    pub preparing: bool,
    /// The run is stepping its circuit in rounds at this publication.
    pub in_rounds: bool,
    /// The run's start ended since the previous publication: its first plan went in or it
    /// started whole. Its speed counts from here, after set-up and planning.
    pub started: bool,
    /// How many of the commands the worker took (counted from its start, in the order sent) these
    /// values reflect: a host that counts what it sends knows which of its commands they do. Each
    /// value they carry at their instant (the nets, the pin currents, the part powers, the
    /// process's state) was produced past where every counted command applied. Not counted, nor
    /// any after it: a command taken since the newest step (an edit published at once, before its
    /// first step), one held behind a `Step`, a timed one not yet applied (`LiveCmd::At`, an
    /// `Edit` where it is heard; it applies as the step after its time begins), and one applied
    /// where a value they carry was not yet produced past (a stage behind block 0, a block of a
    /// plan that went in at its instant, a block reading it late).
    pub stepped_with: u64,
}

/// One frame of the run's grid in simulated time: the statistics over `[since, t]`, from the same
/// accepted points (trapezoids) as a publication's. Frames are where they are on the grid however
/// fast the box steps and however often it publishes, so a run renders the same frames each time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GridFrame {
    /// Its place: it covers `[k * grid, (k + 1) * grid]` (`LiveValues::grid`).
    pub k: i64,
    /// Where its statistics start: `k * grid`, or later in the frame the run started or went
    /// back in.
    pub since: f64,
    /// Where they end: `(k + 1) * grid`.
    pub t: f64,
    /// By net: the time-weighted mean, the minimum and the maximum.
    pub net_mean: Vec<f32>,
    pub net_min: Vec<f32>,
    pub net_max: Vec<f32>,
    /// By component pin and by component: the means of the subscribed pins' currents and
    /// components' dissipation; 0 for the rest.
    pub pin_mean: Vec<f32>,
    pub part_mean: Vec<f32>,
}

/// One terminal of a kernel device. Terminal numbers by device kind: R, C, L, switch and diode
/// `0 = a (p, anode), 1 = b (n, cathode)`; V and I `0 = p, 1 = n`; E, F, G and H `0 = p, 1 = n`
/// (their control terminals carry no current); Q `0 = c, 1 = b, 2 = e, 3 = s`;
/// M `0 = d, 1 = g, 2 = s, 3 = b`; J `0 = d, 1 = g, 2 = s`; GPIO `0 = pad, 1 = vdd, 2 = vss`;
/// a line (T, O) `0 = 1+, 1 = 1-, 2 = 2+, 3 = 2-`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Terminal {
    pub device: String,
    pub terminal: u8,
}

/// How a lowered scope's nets, pins and components read from the kernel, built by the lowering
/// beside its `SimCircuit` (same generation). A component may lower to several kernel devices (an
/// averaged regulator, a stub driver): its pin current is the sum over the terminals listed for
/// that pin, its dissipation the power through its pins.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValuePlan {
    /// Net index -> kernel node name.
    pub nets: Vec<String>,
    /// Component-pin index -> the kernel terminals whose currents sum to that pin's current.
    pub pins: Vec<Vec<Terminal>>,
    /// Component index -> the kernel devices it lowered to.
    pub parts: Vec<Vec<String>>,
    /// Component-pin index -> the net index it is on (None: unconnected).
    pub pin_net: Vec<Option<u32>>,
    /// Component index -> its pins, `(first pin index, count)`: a component's dissipation is the
    /// power into it through its pins, the sum of each pin's net voltage times its current.
    pub part_pins: Vec<(u32, u32)>,
}

/// A host input in the attached process's own type, opaque to the kernel.
#[derive(Clone)]
pub struct DeviceInput(pub Arc<dyn Any + Send + Sync>);

impl fmt::Debug for DeviceInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceInput")
    }
}

impl PartialEq for DeviceInput {
    fn eq(&self, other: &DeviceInput) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A command to the running simulation, applied by the worker at the next step boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveCmd {
    /// Run free at the set rate.
    Play,
    /// Hold simulated time; the last values stay published.
    Pause,
    /// Advance this many simulated seconds, then pause.
    Step(f64),
    /// A device value between steps: no restart of time and no new operating point; the history
    /// is cut and the next step is backward Euler. `key` as the device's `set_param` reads it
    /// (`value` for R, C and L; `dc` for a source). `generation` is the lowering the device name
    /// was resolved in: the worker drops a command of an older generation (it names a device of a
    /// circuit that was replaced); commands are applied in the order sent, so one sent after a
    /// `Replace` waits behind it.
    SetParam { generation: u64, device: String, key: String, value: f64 },
    /// Retunes an independent source between steps, preserving time, generation, plan and sinks.
    /// A running periodic Pulse (positive finite period) or Sin (positive finite frequency)
    /// finishes its current period: the switch is the next period start strictly after now,
    /// with the source breakpoint rounding rule, and needs no early history cut. Other running
    /// waveforms change at once with a history cut. A new periodic waveform's origin moves to
    /// the switch time; other waveforms keep their origin. A second retune before the switch
    /// replaces the pending source at the same time. Trace and values continue without a new
    /// operating point or lowering; commands naming older generations are dropped.
    Retune { generation: u64, device: String, source: Source },
    /// Changes a GPIO's mode between steps at the worker's current time, preserving time,
    /// generation, plan and sinks. A PWM written over a PWM preloads at the next period start
    /// on the running grid; a second PWM write before then replaces that preload. Any other change
    /// ramps from now. Trace and values continue without a new operating point or lowering;
    /// commands naming older generations are dropped.
    PinMode { generation: u64, device: String, mode: PinMode },
    /// A host input to the attached co-simulated process, opaque here (the process's own type).
    /// Applied at a step boundary through `CoSim::input` at the run's time. `key` names the
    /// input's stream on the timeline: as for other timed commands, a later one with the same
    /// key keeps its distance (`schedule`). While it waits in the host's queue, a newer
    /// replaceable input with the same generation and key replaces it if this is the newest
    /// queued input in that stream and is also `replaceable`. A command of an older generation
    /// is dropped.
    Input { generation: u64, key: String, input: DeviceInput, replaceable: bool },
    /// Which pins' currents and components' dissipation get interval means (`LiveValues::pin_mean`,
    /// `part_mean`), by their `ValuePlan` indices of `generation`: the visible ones. Replaces the
    /// previous subscription; a `Replace` clears it.
    Subscribe { generation: u64, pins: Vec<u32>, parts: Vec<u32> },
    /// Simulated seconds per real second; `f64::INFINITY` runs as fast as the solver can.
    SetRate(f64),
    /// The time resolution (`LiveSpec::res`, `Stepper::set_res`) from this step boundary on, with
    /// no history cut and no new operating point; 0 is Exact. A `Replace` keeps it.
    SetRes(f64),
    /// Rounds on or off (`LiveSim::set_rounds`) from this step boundary on: off steps the whole
    /// circuit on one stepper. A `Replace` keeps it.
    SetRounds(bool),
    /// The signals the trace keeps over the whole run, by signal (`LiveSpec::whole`: the views'
    /// probes and lanes; the others keep the live front): from the next keep on, a newly whole
    /// signal from what it still has. The stepping does not change.
    SetWhole(std::sync::Arc<[bool]>),
    /// The signals the trace writes points for (`LiveSpec::fine`; None: every one), from the next
    /// point on: the others' points stay out of the trace, a gap there. The stepping does not change.
    SetFine(Option<std::sync::Arc<[bool]>>),
    /// Continuous's ring (`LiveSpec::ring`): the record keeps this many simulated seconds behind
    /// its newest point.
    SetRing(f64),
    /// Whether someone keeps time with the run as it is made (`LiveSpec::heard`).
    SetHeard(bool),
    /// The take's end (`LiveSpec::until`): a playing run rests there; a run that rested before a
    /// later end plays on. Infinity: an open run.
    SetUntil(f64),
    /// The run goes back to its start (its first kept snapshot), as an edit at its start would:
    /// the trace is cut there and the sinks go back with it, so a sink installed since (a new
    /// Listen tap) records the take from its start. A run that keeps no snapshot from before its
    /// time goes on (counted as a late edit).
    Restart,
    /// The spilled logic chunks the views show in detail, by signal and first time
    /// (`LiveSpec::logic_detail`, `TraceStore::spilled_in`): the worker loads them back into the
    /// trace, and lets go of those loaded before that no view wants any more.
    LogicDetail(Vec<(crate::trace::SignalId, f64)>),
    /// A new lowering of the same scope (a topology change), of `generation`: node voltages and
    /// device state carry over by name, time continues.
    Replace { generation: u64, circuit: Box<SimCircuit>, plan: Box<ValuePlan> },
    /// Attaches the slot's co-simulated process in place of the running one (an empty slot
    /// detaches it) at the worker's current time: its pins resolve by name in the running circuit
    /// and it first runs at its `next()`, at once when that is now or earlier. A `Replace` keeps
    /// the attached process (its pins resolve again in the new circuit).
    CoSim(CoSimSlot),
    /// The streamed inputs the run holds for, in place of the previous ones (`crate::input`): from
    /// the next loop top it steps only where every open source covers what it would read. Applied
    /// at once, during a `Step` too; a `Replace` keeps them. Changes nothing the run computes.
    Inputs(crate::input::Inputs),
    /// Runs until the attached device's next stop of this kind (`CoSim::arm`), then pauses and
    /// publishes at once. Unpaced (as fast as the solver goes); `Pause` cancels it; commands are
    /// not held behind it. With no device, or one that cannot step so, it is refused
    /// (`LiveStats::refused`) and nothing runs.
    CoStep(StepTo),
    /// The host changed the sink set (`SinkCmd`): a run waiting for a command (paused or settled)
    /// installs the queued sinks now, handing a settled run's held values to the new ones; a
    /// running one installs them at its next point. Changes nothing else.
    Sinks,
    /// Ends the worker loop.
    Stop,
    /// A value command on the run's timeline: applied at the first step boundary at or after
    /// simulated time `t` (a step is never forced to land there), so a run given the same
    /// timeline is the same run however its worker is sliced, paced or published. Only `SetParam`,
    /// `Retune`, `PinMode` and `Input` are timed; any other is refused (`LiveStats::refused`). A time
    /// the run already passed applies at its next boundary, and the timeline keeps that boundary's
    /// time (`LiveSim::timeline`), so a replay is the same run; the next command of the same device
    /// and key moves later as much, keeping its distance (a quick press's release stays its length
    /// after the press). Not held behind a `Step`; events at one time apply in the order sent; a
    /// pending one keeps a settled run stepping.
    At(f64, Box<LiveCmd>),
    /// Value commands and host inputs at simulated times, as `At` each (a press is two: down and
    /// up), sent as one edit. When the earliest is a time the run already passed and it keeps
    /// snapshots (`LiveSpec::snapshots`), the run first goes back to the newest snapshot at or
    /// before that time and steps on from there with the edit on its timeline: what it renders from
    /// that time on is the device's life with the edit, and the trace, the sinks and the attached
    /// process's events go back with it. With no snapshot early enough it moves to the run's time
    /// whole, keeping its events' distances (`LiveStats::late_edits`). Not held behind a `Step`.
    Edit(Vec<(f64, LiveCmd)>),
}

impl LiveCmd {
    /// The command itself, under its time if it is timed (`At`).
    pub fn untimed(&self) -> &LiveCmd {
        match self {
            LiveCmd::At(_, cmd) => cmd.untimed(),
            cmd => cmd,
        }
    }

    pub fn untimed_mut(&mut self) -> &mut LiveCmd {
        match self {
            LiveCmd::At(_, cmd) => cmd.untimed_mut(),
            cmd => cmd,
        }
    }
}

/// A single-slot publication: the producer replaces the value, any number of readers take the
/// newest. The lock only guards a pointer swap or clone, never work, so neither side waits on the
/// other's work; a reader that missed values simply sees the newest one.
pub struct Latest<T> {
    slot: Mutex<Option<Arc<T>>>,
    count: AtomicU64,
    /// Every value published while a reader keeps them all ([`Latest::keep_all`]), oldest first,
    /// until it takes them; None while none does.
    kept: Mutex<Option<Vec<Arc<T>>>>,
    keeping: AtomicBool,
}

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Latest { slot: Mutex::new(None), count: AtomicU64::new(0), kept: Mutex::new(None), keeping: AtomicBool::new(false) }
    }
}

impl<T> Latest<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes `v`; returns the value it replaced for the producer to dispose of. While a reader
    /// keeps every value, `v` is kept for it too (one `Arc` pushed).
    pub fn publish(&self, v: Arc<T>) -> Option<Arc<T>> {
        if self.keeping.load(Ordering::Acquire) {
            if let Some(kept) = self.kept.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                kept.push(v.clone());
            }
        }
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        let old = slot.replace(v);
        self.count.fetch_add(1, Ordering::Release);
        old
    }

    /// From now on every value published is kept until [`Latest::take_kept`] takes it (`on`), or
    /// none is and what was kept is dropped (off). Constant time; the drop is the caller's.
    pub fn keep_all(&self, on: bool) -> Option<Vec<Arc<T>>> {
        if self.keeping.load(Ordering::Relaxed) == on {
            return None;
        }
        self.keeping.store(on, Ordering::Release);
        let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
        if on {
            kept.get_or_insert_with(Vec::new);
            None
        } else {
            kept.take()
        }
    }

    /// The values kept since the last call, oldest first, swapped into `into` (cleared first, so
    /// the producer reuses its storage). Constant time under a lock the producer holds only for a
    /// push.
    pub fn take_kept(&self, into: &mut Vec<Arc<T>>) {
        into.clear();
        if let Some(kept) = self.kept.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            std::mem::swap(kept, into);
        }
    }

    /// The newest value and its publication number (1 for the first), if any.
    pub fn latest(&self) -> Option<(u64, Arc<T>)> {
        let slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        let n = self.count.load(Ordering::Acquire);
        slot.clone().map(|v| (n, v))
    }

    /// How many values have been published; a reader compares it with the number it last saw
    /// before taking the lock at all.
    pub fn published(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }
}
