//! Live mode's worker (E3 3, E-2): a free-running transient that applies commands between steps,
//! publishes `LiveValues` through `Latest` and `TraceDelta`s to a `TraceOut` (a bounded channel, or a
//! `TracePublisher` that merges them here and publishes the store), and never waits on a consumer. The host owns the thread: it runs `run_live` on one long-lived worker per
//! live bench. `LiveSim`'s methods are the loop's parts, also driven directly by tests and by a host
//! that steps in its own time.
//!
//! - Commands apply between steps; `Stepper::step` returns only on a confirmed point, so there is
//!   no provisional point to undo. A `Step` holds the commands after it until its time is reached
//!   (`Pause`, `Stop` and `Inputs` act at once). A `SetParam` cuts the history (one backward Euler step):
//!   no time restart, no new operating point.
//! - A `Retune` finishes a running periodic source's period; other sources change at once.
//! - Each accepted point goes to the host's sink (the instrument engines), to the trace (one signal
//!   per net, optionally followed by pin currents), and to the interval statistics the next
//!   `LiveValues` carries.
//! - `TraceDelta`s go out with `TraceOut::offer`; a channel's `try_send` refuses one when full, the
//!   points stay pending and the next attempt waits twice as long (up to a second), paused or settled
//!   too, so the copying a refused delta costs stays linear in the points however long the consumer
//!   stalls. A `TracePublisher` never refuses: the consumer's store is merged on this thread.
//! - A settled bench (inputs constant from now on, the state solving the static system by Newton's
//!   test) publishes once and then waits for a command like a paused one. The loop calls its `wake`
//!   after every publication of values or of a trace delta.
//! - A co-simulated process (`cosim`, `attach`) runs in this loop at the exact instants it asks
//!   for and at its watches' hits, driving GPIO pins and stepping the analog through a `Bus`. A
//!   `CoStep` runs unpaced to the device's next stop of its kind, then pauses and publishes at once.
//! - In rounds (`set_planner`, `crate::rounds`): the circuit runs as blocks, each on its own
//!   stepper, that meet at every round's end. The attached process runs with block 0 as it runs
//!   with the whole circuit; the other blocks follow to the round's end, on helper cores when the
//!   host gives some (`set_helpers`). Commands apply between rounds; a timed one, a `Step`'s end
//!   or a firmware stop ends a round early. The points merge into the one stream the sinks and the
//!   trace take. Without a planner, or while it returns no plan, nothing changes. The planner
//!   plans again after the firmware has set its pads and after the user's edits (`Replan`), from
//!   a snapshot of the run, never per step; a slow plan is made on a helper core meanwhile.
//! - Edits behind the front (`LiveCmd::Edit`, `LiveSpec::snapshots`): the run keeps snapshots of
//!   itself, spaced by the wall time it steps, dense near its front and thinner behind. An edit
//!   whose time the run already passed sends it back to the newest snapshot at or before that
//!   time; it steps on from there with the edit on its timeline, and the trace (a `Rewind`), the
//!   sinks (`Sink::rewind`) and the attached process's events go back with it.
//! - Streamed inputs (`LiveCmd::Inputs`, `crate::input`): at each loop top (in rounds, each round
//!   start) the run holds while a source does not yet cover what the next step and the attached
//!   process would read; `run_live` waits as it does for a holding sink and publishes why.

use crate::analysis::tran::{Accepted, Restarts, Stepper};
use crate::analysis::op::OpSeed;
use crate::analysis::partition::Land;
use crate::analysis::states::StateCache;
use crate::analysis::{HoldKind, Sink, SinkState};
use crate::circuit::{Options, PinMode, Retuned, SimCircuit, Source, Wave};
use crate::cosim::{Bus, CoAudio, CoRun, CoSim, CoSimDevice, StepTo};
use crate::input::InputSet;
use crate::live::{GridFrame, Latest, LiveCmd, LiveValues, ValuePlan};
use crate::newton::{Rescue, SimDiag, Solver};
use crate::rounds::{Carry, CheckPost, Checks, Helpers, MergeRows, OverWhy, PackCount, Planner, Rounds, Run};
use crate::stamp::UnknownInfo;
use crate::spill::LogicSpill;
use crate::budget::RECORD_FINE_BYTES;
use crate::trace::{fill_gap, writes, Chunk, Fold, Keep, LogicEdge, LogicPage, LogicTrace, Rewind, SignalId, TraceDelta, TracePublisher, TraceStore};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Fast's relative tolerance (E3 3.1): what a live view needs, and what a run with a time
/// resolution steps at unless its spec names one (`LiveSpec::reltol_at`). Exact steps at the
/// bench's.
pub const FAST_RELTOL: f64 = 1e-2;

/// The sample rate a design with an audio output or a listened net runs at unless the user picks
/// another, Hz: the host's default for `LiveSpec::sample_rate`.
pub const AUDIO_SAMPLE_RATE: f64 = 96_000.0;

/// The sample rate a heard design runs at in Realtime unless the user picks another, Hz. Realtime
/// must keep up, and a design that does at it may not at [`AUDIO_SAMPLE_RATE`]; its takes stay at
/// [`REALTIME_RECORDING_RATE`], so Fast's plan is judged for accuracy to half of that and for
/// stability to half of this (`PartOpts::band`). Its worst case on a 16 GB Mac, drum16's board in
/// Fast in rounds with every voice and the accent on every step at 240 bpm and its TEMPO turned:
/// 1.148 to 1.153 times real time at 60 kHz in one lease of interleaved runs, against the 1.15 the
/// rate asks. That bar is thin: one build reads up to 1.4 % apart from one lease to the next (0.3 %
/// within one), so its earlier rows of 1.17 to 1.20 were their leases', not the landings' since:
/// those read alike in one lease. The chain shadow's sweep costs 0.8 % of it. 1.13 at 64 kHz. Its
/// take at 60 kHz is within 1.99 dB of the reference to 16 kHz, 1.01 dB inside the 3 dB screen and
/// so 0.01 dB over the dB the rate asks: a change to Fast takes the 60 kHz take judge again before
/// it lands.
pub const REALTIME_SAMPLE_RATE: f64 = 60_000.0;

/// The rate a heard design's takes are recorded at in Realtime unless the user picks another, Hz:
/// the common audio rate. Half of it is the band Fast is held accurate to in Realtime.
pub const REALTIME_RECORDING_RATE: f64 = 48_000.0;

/// What running at the sim rate `rate` (Hz) does against `AUDIO_SAMPLE_RATE`, in plain words for
/// the user beside the rate's pick; None at that rate or with none chosen. A slower rate takes
/// fewer, longer steps: the circuit in Exact is the same, its noise reaches only half the rate,
/// and Fast, which places its steps on the rate's grid, loses the highest sounds and pitch
/// with each doubling of the period (drum16's board at 48 kHz: Fast 1.6 times as fast, its hats
/// 2 to 5 dB duller over 2 kHz and its metal 3.7 cents flat; Exact 1.3 times as fast).
pub fn sim_rate_cost(rate: f64) -> Option<String> {
    if !(rate > 0.0 && rate.is_finite()) || rate == AUDIO_SAMPLE_RATE {
        return None;
    }
    let khz = |hz: f64| format!("{} kHz", (hz / 100.0).round() / 10.0);
    let (at, default, noise) = (khz(rate), khz(AUDIO_SAMPLE_RATE), khz(rate / 2.0));
    Some(if rate < AUDIO_SAMPLE_RATE {
        format!("{at} runs faster than {default}. Noise reaches only {noise}, and in Fast the highest sounds are duller and pitches less exact.")
    } else {
        format!("{at} runs slower than {default}, with noise up to {noise} and Fast a little more exact.")
    })
}

/// The longest spacing a noise source's draws keep: the default audio rate's period. A run at
/// that rate or a slower one hears one sequence of draws, its means over each period: the rate
/// sets how far up the noise reaches, never which noise it is.
const NOISE_BASE: f64 = 1.0 / AUDIO_SAMPLE_RATE;

/// A noise source taking a value once a sample period at the density it was lowered with: white
/// noise of variance sigma^2 at spacing dt has the density sigma^2 dt, so sigma scales by
/// sqrt(dt / period). Its draws stay at the source's own spacing (as lowered, at most
/// [`NOISE_BASE`]) whatever the period, and a longer period takes their means
/// ([`Wave::Noise`]); a shorter one draws at the period itself.
fn noise_at(mut source: Source, period: f64) -> Source {
    if let Wave::Noise { sigma, dt, base, .. } = &mut source.wave {
        if period > 0.0 && period.is_finite() {
            let fixed = base.min(NOISE_BASE);
            *base = if period < fixed { period } else { fixed };
            if *dt != period {
                *sigma *= (*dt / period).sqrt();
                *dt = period;
            }
        }
    }
    source
}

/// The smallest spacing of the circuit's noise sources as lowered, seconds; infinity for none.
fn noise_spacing(circuit: &SimCircuit) -> f64 {
    circuit.devices.v.iter().chain(circuit.devices.i.iter()).filter_map(|device| match device.src.wave {
        Wave::Noise { dt, .. } => Some(dt),
        _ => None,
    }).fold(f64::INFINITY, f64::min)
}

/// Tunes every noise source of the circuit to the run's sample period (`noise_at`).
fn tune_noise(circuit: &mut SimCircuit, period: f64) {
    for device in circuit.devices.v.iter_mut().chain(circuit.devices.i.iter_mut()) {
        if matches!(device.src.wave, Wave::Noise { .. }) {
            device.src = noise_at(device.src.clone(), period);
        }
    }
}

/// How a live simulation runs.
#[derive(Clone, Debug)]
pub struct LiveSpec {
    /// Simulated seconds per real second; `f64::INFINITY` runs as fast as the solver goes.
    pub rate: f64,
    /// The largest step, from the instrument bandwidth setting (`1 / (10 bandwidth)`). The sample
    /// period also bounds it, and while the run is paced so does `rate * values_every`
    /// (`LiveSim::step_cap`).
    pub max_step: f64,
    /// A relative tolerance for both modes (a test's or a study's); None steps each at its own
    /// (`LiveSpec::reltol_at`).
    pub reltol: Option<f64>,
    /// Wall time between `LiveValues` publications and between `TraceDelta`s.
    pub values_every: Duration,
    pub trace_every: Duration,
    /// The longest the loop steps before it looks at commands and publications again.
    pub slice: Duration,
    /// Playing from the start (else paused at t = 0, on the operating point).
    pub playing: bool,
    /// The live front: simulated seconds the trace keeps of every signal (the instrument's record
    /// length, a visible setting); a signal not `whole` keeps only these. It also sets the
    /// stepper's rate window.
    pub record: f64,
    /// The signals the trace keeps over the whole run, by signal (`LiveCmd::SetWhole`: the views'
    /// probes and lanes): past the front the others' chunks are absent ([`LiveSpec::keep`]).
    /// Only what is kept follows it; the stepping does not.
    pub whole: Arc<[bool]>,
    /// The signals the trace writes points for at all, by signal (`LiveCmd::SetFine`; None, the
    /// default, every signal): a host that must keep up (Realtime) names the rows a view shows,
    /// and the others' points stay out of the trace and its deltas, a gap there
    /// (`TraceStore::fine`); a signal named again is written from its next point on. Each net's
    /// interval statistics (the take's coarse envelope) do not follow it, nor does the stepping.
    pub fine: Option<Arc<[bool]>>,
    /// Continuous's ring: simulated seconds of the newest record kept for every signal;
    /// infinity keeps the run's whole span.
    pub ring: f64,
    /// The take's end, simulated seconds (`LiveCmd::SetUntil`): a playing run steps exactly to it
    /// and rests there, and goes on again when an edit sends it back before it or the end moves
    /// later. A `Step` is not held by it. Infinity: an open run.
    pub until: f64,
    /// The spilled logic chunks the views show in detail (`LiveCmd::LogicDetail`), by signal and
    /// first time: the trace keeps them in memory while asked.
    pub logic_detail: Vec<(SignalId, f64)>,
    /// Records the trace and its deltas. An ensemble member feeds only its sinks: false.
    pub trace: bool,
    /// The trace also carries each pin's current (into the pin, by plan index) as signals after
    /// the nets: signal `nets + p` is pin `p`.
    pub trace_pins: bool,
    /// The grid in simulated time the run closes frames on (`LiveValues::frames`), seconds a
    /// frame; 0 for none. 4 ms (250 a simulated second): a frame holds many steps of a PWM, and a
    /// slowed view at a tenth of real time still shows 25 a second.
    pub grid: f64,
    /// Each `LiveValues` carries the attached device's state (`CoSim::state`, `LiveValues::co`).
    /// An ensemble member's views read the nominal run's: false.
    pub co_state: bool,
    /// The time resolution, seconds (`Stepper::set_res`): what is shorter than it is not
    /// resolved. 0 is Exact.
    pub res: f64,
    /// The run's sample rate (the sim rate), Hz, in Exact and Fast alike: no step is longer than
    /// its period, the steps land on its grid, rounds count in it, and every noise source takes a
    /// value once a period at the density it was lowered with: the mean of its draws over the
    /// period, the same draws at every rate up to `AUDIO_SAMPLE_RATE` (`noise_at`), so the rate
    /// sets how far up the noise reaches, never which noise it is. Anything but a positive finite
    /// rate (0, the default) is none chosen: the period is then the bandwidth step, or a noise
    /// source's own spacing where that is shorter (`LiveSim::sample_rate` says which). It is not
    /// the rate a recording of the run is made at: a take has its own (`RunContent`), its
    /// samples at this rate the run's read on its grid (`listen::Band`: the polyline it stepped,
    /// band-limited, less the grid's own corners where it did not resolve), at another that read
    /// through a band-limited rate change. The host gives the user's pick, else
    /// `AUDIO_SAMPLE_RATE` for a design with an audio output or a listened net. Fixed for the run's
    /// life: a new rate is a new run. What a rate other than `AUDIO_SAMPLE_RATE` costs, in the
    /// user's words: [`sim_rate_cost`].
    pub sample_rate: f64,
    /// A run in rounds (`LiveSim::set_planner`): the round's length, in sample periods.
    /// A coupling lagged by a round is this much late.
    pub round: u32,
    /// Fast only. Stages after block 0 run rounds behind it on the helpers. Stage `e` reads a
    /// back coupling from stage `s` (`e < s`) `s - e + 1` rounds late, with the same values on
    /// any number of helpers.
    pub pipeline: bool,
    /// Fast only. A block in rounds is fed another block's jumps (its crossings' edges, a value
    /// that changes at a landed corner) as the line through its own sample's means, a ramp a
    /// sample wide centred on the jump (`Table::split`): no instant to land on and restart at. A
    /// jump with a kink beside it is landed on, and restarts the history as at a crossing of the
    /// block's own. Off, a block lands on every one and restarts its history as at a corner.
    pub edge_restarts: bool,
    /// Every round's hub reads screened (`RoundSplit::hub_onsets`): a diagnostic, and the proofs'.
    /// Off, a block's hub reads are screened only in the rounds the merge gate's take asks for, so
    /// a steady round computes no late term.
    pub hub_screen: bool,
    /// Fast only, with `edge_restarts`. Each block of a run in rounds steps on its own bound,
    /// whatever its place in the plan: up to Fast's resolution and the bandwidth step, on a grid
    /// of whole sample periods, shorter while its own thresholds cross often (an oscillator in it
    /// keeps the sample period; `rounds::CROSSING_SHARE`), with its imports' crossings taken as
    /// data rather than instants to land on. Between its events its nets are resolved as in a run
    /// with no sample rate, but for a net a sink hears at the sample rate (`Sink::heard`: a Listen
    /// tap, the output jacks' recorder): while one of the block's is heard, or a block that solves
    /// one reads it (transitively), it steps on the sample period. A later stage's round goes by what was heard as block 0 opened it, so a run in
    /// rounds is the same on any number of helpers.
    pub own_bound: bool,
    /// Fast only, with `pipeline` and helpers. A round's points are merged on a helper while the
    /// next round steps, and reach the sinks at the next round's close or when `advance` returns:
    /// the same points in the same order, later.
    pub merge_job: bool,
    /// When a run with a planner plans again (`Replan`).
    pub replan: Replan,
    /// Someone keeps time with the run as it is made (the host says so: a player plays its
    /// output, or it is paced to the wall clock). After its first plan, a plan still out at its
    /// time goes in at the first round start where it is back, and that round is written
    /// (`LiveSim::go_ins`) for a run made again from the same inputs to put it in at.
    pub heard: bool,
    /// Where an edit behind the run's front can send it back to (`LiveCmd::Edit`).
    pub snapshots: Snapshots,
}

/// The snapshots a live run keeps of itself (`LiveSim::snapshot`), so that an edit at a time the
/// run already passed (`LiveCmd::Edit`) sends it back to the newest one at or before that time.
/// One is taken where `advance` returns once the run has stepped `every` of wall time since the
/// last, or a hundred times what the last one took if that is longer, so the wait before an
/// edit's effect is bounded at any speed. Within `span` simulated seconds of the front they are
/// all kept; behind it they thin out (of two neighbours that thinned as often, the newer goes),
/// so the past costs a number of them logarithmic in its length. Snapshots are passive: a run
/// that keeps them steps exactly as one that keeps none.
#[derive(Clone, Debug)]
pub struct Snapshots {
    /// Simulated seconds behind the front where every snapshot is kept; 0 keeps none (the
    /// default): an edit behind the front then applies at the next step boundary.
    pub span: f64,
    /// Wall time stepped between two snapshots, at least.
    pub every: Duration,
    /// The most of the stepping time that snapshots take: the next comes once the stepping since
    /// the last is its cost over this, at least (0.01, the default: 100 times its cost).
    pub share: f64,
    /// The most memory the kept snapshots hold, bytes (256 MiB, the default). Over it the thinning
    /// reaches into the dense span, the first and the newest kept: an edit then steps more from a
    /// snapshot further back, and is never refused (logged once each time the snapshots reach
    /// it). A snapshot's size is what the worker's thread allocated taking it, measured by the
    /// host's counting allocator (`crate::alloc::Counting`); with none installed each counts as
    /// [`UNMEASURED`].
    pub bytes: u64,
}

/// A snapshot's size in the budget when no counting allocator measures it: a board several times
/// drum16's (whose snapshot holds about 0.5 MB).
pub const UNMEASURED: u64 = 1 << 20;

impl Default for Snapshots {
    fn default() -> Self {
        Snapshots { span: 0.0, every: Duration::from_millis(250), share: 0.01, bytes: crate::budget::SNAPSHOT_BYTES }
    }
}

/// When a run with a planner plans again. A re-plan falls due after the user's edits (`SetParam`,
/// `Retune`), each time pushed out by the next, and after a pad's first change from the kind of
/// mode it was lowered with (at the start an MCU's pads are inputs: the useful plan comes once
/// its firmware sets them), at the first such change's time, never pushed out by edits and going
/// in through them (`LiveSim::guard_edit`). It is looked at only where a round starts (between
/// steps on one stepper), never per step. A due plan starts from a snapshot of the run (the
/// planner linearises there) and goes in at the first round start `lead` later, its blocks
/// seeded by name from the run as it is then; one that took longer than `slow` last time is made
/// on a helper core meanwhile.
/// Every run waits for the worker to make its first plan (`LiveSim::first_plan`). For later
/// plans, the round a plan goes in at is an input of the run, as a key press is (`LiveSim::go_ins`).
/// A heard run (`LiveSpec::heard`) keeps stepping while a later plan is out. It takes the plan
/// at the first round start at or after its time where it is back and records that round. A run
/// with a recorded round installs the plan there. Any other run installs it at its due time,
/// waiting if necessary in either case. A run made again from its inputs gets the same values.
/// `Replace` plans again at once.
#[derive(Clone, Debug)]
pub struct Replan {
    /// Simulated seconds after the last edit.
    pub edit: f64,
    /// Simulated seconds after a pad's first change of its mode's kind (the first of several).
    pub pins: f64,
    /// Simulated seconds from the snapshot to the plan going in.
    pub lead: f64,
    /// Wall seconds: a plan that took longer is made on a checker's helper the next time
    /// (`LiveSim::set_checks`), below the run's packs.
    pub slow: f64,
}

impl Default for Replan {
    fn default() -> Self {
        Replan { edit: 0.1, pins: 1e-3, lead: 0.05, slow: 0.02 }
    }
}

/// What a circuit can hold in rounds, as a multiple of its largest source (`SimCircuit::source_peak`,
/// a volt at least): a voltage past it, or a value not a number, is a loop running away (the growth
/// rule, `LiveSim::breach_taken`). Generous: a healthy board never comes near it, and a loop that
/// doubles each round passes it within a few rounds of leaving its range.
/// Only independent voltage sources count: a node a current source or a step-up winding drives
/// past it reads as a run-away, and the run refuses that cut (it steps on whole there).
pub const HOLD: f64 = 100.0;

impl LiveSpec {
    /// What the trace keeps (`TraceStore::keep`): with a whole signal, the ring's span, every
    /// signal over the front and the whole ones over all of it, folding past
    /// [`RECORD_FINE_BYTES`]; with none, every signal over the front only.
    pub fn keep(&self) -> Keep {
        let any = self.whole.iter().any(|&w| w);
        Keep { span: if any { self.ring } else { self.record }, budget: RECORD_FINE_BYTES, whole: Some(self.whole.clone()), front: self.record }
    }

    /// The relative tolerance a run steps at with time resolution `res`: the spec's own when it
    /// names one; else in Exact (0) the bench's, `Options::default().reltol`, which every bench
    /// run and oracle judgment uses, so Exact is one definition for the app and the bench; in Fast
    /// `FAST_RELTOL`.
    pub fn reltol_at(&self, res: f64) -> f64 {
        self.reltol.unwrap_or_else(|| if res > 0.0 { FAST_RELTOL } else { Options::default().reltol })
    }

    /// The sample period of `circuit`'s run, seconds: `1 / sample_rate`, or with none chosen the
    /// bandwidth step or the circuit's finest noise spacing as lowered, whichever is shorter.
    fn period(&self, circuit: &SimCircuit) -> f64 {
        if self.sample_rate.is_finite() && self.sample_rate > 0.0 {
            1.0 / self.sample_rate
        } else {
            self.max_step.min(noise_spacing(circuit))
        }
    }
}

impl Default for LiveSpec {
    fn default() -> Self {
        LiveSpec {
            rate: 1.0,
            max_step: 1e-4,
            reltol: None,
            values_every: Duration::from_millis(16),
            trace_every: Duration::from_millis(8),
            slice: Duration::from_millis(8),
            playing: true,
            record: 20e-3,
            whole: Arc::from([]),
            fine: None,
            ring: f64::INFINITY,
            until: f64::INFINITY,
            logic_detail: Vec::new(),
            trace: true,
            trace_pins: false,
            grid: 4e-3,
            co_state: true,
            res: 0.0,
            sample_rate: 0.0,
            round: 32,
            pipeline: true,
            edge_restarts: true,
            hub_screen: false,
            own_bound: true,
            merge_job: true,
            replan: Replan::default(),
            heard: false,
            snapshots: Snapshots::default(),
        }
    }
}

/// What the worker did, for a host's status line and for tests.
#[derive(Clone, Debug, Default)]
pub struct LiveStats {
    /// Accepted points.
    pub points: u64,
    pub published: u64,
    pub deltas: u64,
    /// Offers the trace outlet refused, a full channel (the points stayed pending).
    pub full: u64,
    /// Commands dropped because they name an older generation.
    pub stale: u64,
    /// `SetParam`, `Retune` or `PinMode` commands naming no valid device/key or refusing the value;
    /// `Input` commands refused by the attached process, or sent without one.
    pub unknown: u64,
    /// Host inputs accepted by the attached co-simulated process, including timeline replays.
    pub inputs: u64,
    pub replaced: u64,
    /// Source or GPIO mode retunes applied immediately or scheduled at the next period start.
    pub retuned: u64,
    /// Runs of the attached co-simulated process.
    pub co_runs: u64,
    /// Firmware steps that reached their stop (`CoRun::Stopped`).
    pub co_stops: u64,
    /// Advances that asked the attached process to settle and ended before it had (a hard end, a
    /// held input, a slice of wall time): their reads re-run its private stretch.
    pub co_unsettled: u64,
    /// The instants at which the attached process's writes changed a pad now; writes of several
    /// pins at one instant count once.
    pub write_instants: u64,
    /// Wall time in `advance` over the session, and the parts of it in the attached process's runs
    /// (without the analog steps a run asks for) and in the sinks (the scopes' acquisition, in the
    /// solver's steps and the process's alike); the solver had the rest.
    pub step_wall: Duration,
    pub co_wall: Duration,
    pub sink_wall: Duration,
    /// `CoStep` commands refused: no device attached, or it cannot step so (`CoSim::arm` false).
    pub refused: u64,
    /// Logic entries spilled from memory to the run's file past `trace::LOGIC_KEEP` a signal, in
    /// whole chunks (`TraceStore::spill_logic`): kept, read back on a view's zoom.
    pub logic_spilled: u64,
    /// In rounds: the rounds run, and of the packs posted to helper cores, those a helper ran and
    /// those the worker ran itself, no helper having started them.
    pub rounds: u64,
    pub packs_helped: u64,
    pub packs_claimed: u64,
    /// Wall times and point counts over all rounds in the session.
    pub round_split: crate::rounds::RoundSplit,
    /// The last `Replace` that failed to set up (the previous circuit kept running).
    pub replace_error: Option<SimDiag>,
    /// The last `Replace`'s hand-over of the run to the new circuit.
    pub handover: Option<crate::analysis::handover::HandOver>,
    /// Plans made (the planner's calls), of them those made on a helper core, the simulated time
    /// the last one went in or was kept (0 before any), and the last that failed after the start
    /// (the run went on as it was, on one stepper if it had left rounds).
    pub plans: u64,
    pub plans_helped: u64,
    /// Plans let go of while a helper was making them.
    pub plans_stopped: u64,
    /// Warms let go of before they were back.
    pub warms_stopped: u64,
    /// Heard starts that began on a cut carried from a plan of the same circuit at other part
    /// values (`Planner::carried`), their own plan made behind them; those that would have and were
    /// refused, and the last refusal's why.
    pub starts_carried: u64,
    pub carries_refused: u64,
    pub carry_refusal: Option<String>,
    /// Plans that cut the run as it already ran in rounds: its blocks ran on, with no handover.
    pub plans_kept: u64,
    /// Blocks a plan that cut the run anew shared with the running one, which ran on (`go_in`).
    pub blocks_kept: u64,
    /// The worker's wall putting plans in (`go_in`: the handover, the new blocks' start and the
    /// rounds), in all and the longest.
    pub install_wall: Duration,
    pub install_max: Duration,
    pub last_plan: f64,
    /// Round starts where a plan was to go in and its helper had not made it yet, so the worker
    /// waited (`LiveSim::at_round_start`): how many, the wall time in all and the longest. A heard
    /// run waits for a helper only at a previously recorded round (`LiveSim::go_ins`).
    pub plan_waits: u64,
    pub plan_wait_wall: Duration,
    pub plan_wait_max: Duration,
    /// Plans a heard run took after their time, at the first round start where they were back,
    /// and the most simulated time one went in late by.
    pub plans_late: u64,
    pub plan_late_max: f64,
    /// Edits that left rounds (on a loop of the plan's round map, whose proof failed or which the
    /// plan kept no proof for, `reprove_edits`), and the simulated time stepped on one stepper while
    /// the run had a planner: before its first plan went in, from a leave to the plan after it, and
    /// wherever the planner cut nothing.
    pub edit_leaves: u64,
    /// Times the growth rule took the run back (`LiveSim::breach_taken`): a value in rounds not a number,
    /// or a voltage past what the circuit can hold; the last, with the nodes it refused.
    pub breaches: u64,
    pub last_breach: Option<String>,
    /// Times a shadow round past its bar refused a cut (`LiveSim::shadow_taken`); the last, with
    /// its block, figure and the nodes it refused.
    pub shadow_acts: u64,
    pub last_shadow_act: Option<String>,
    /// Chain prefixes landed by the run (`LiveSim::chain_taken`), and the last with its born
    /// export, target coupling and the held member that convicted it.
    pub chain_acts: u64,
    pub last_chain_act: Option<String>,
    /// Anchors taken (`LiveSim::take_anchor`: the restore point where a plan's rounds begin, for a
    /// run that keeps no snapshot), the
    /// last one's bytes (`UNMEASURED` without a counting allocator) and the time it took.
    pub anchors: u64,
    pub anchor_bytes: u64,
    pub anchor_wall: Duration,
    pub off_rounds: f64,
    pub plan_error: Option<SimDiag>,
    /// Proofs of the plan made again after edits on its loops (`reprove_edits`): how many, their
    /// wall time on the worker in all and the longest, and the last one's map or why it failed.
    pub reproofs: u64,
    pub reproof_wall: Duration,
    pub reproof_max: Duration,
    pub last_reproof: Option<Result<crate::analysis::round::RoundMap, String>>,
    /// Re-proofs from a fixed start that held on the stability bar while their accuracy figure read
    /// over its bar at the run's state (`RoundProof`): counted, gating nothing.
    pub reproofs_over: u64,
    /// The steps taken only through the stepper's gmin ladder (`Rescue`), over the whole session.
    pub rescues: Vec<Rescue>,
    /// Snapshots taken (`LiveSpec::snapshots`), those kept now, and the wall time the last took;
    /// the bytes the kept ones hold, and the last one's (`Snapshots::bytes`).
    pub snapshots: u64,
    pub snapshots_kept: usize,
    pub snapshot_wall: Duration,
    pub snapshot_bytes: u64,
    pub snapshot_last: u64,
    /// Edits that sent the run back to a snapshot, and the wall time the last return took (the
    /// restore and the trace's cut; the stretch stepped again after it is stepping).
    pub rewinds: u64,
    pub rewind_wall: Duration,
    /// Edits behind the front with no snapshot at or before their time: applied at the next step
    /// boundary instead.
    pub late_edits: u64,
    /// In rounds, the advances that stopped at the wall clock (a paced run ahead of it), and the
    /// catch-ups of the stages behind block 0 at an advance's stop: a `Step`'s end, the take's
    /// end, an unpaced target. A clock stop makes none (the next advance goes on with them).
    pub clock_stops: u64,
    pub stop_catchups: u64,
    /// Flushes (`RoundSplit::flushes`) by cause: for an edit that needs the blocks at one instant,
    /// for a plan going in that reshapes the rounds.
    pub edit_flushes: u64,
    pub go_in_flushes: u64,
    /// Snapshots taken in rounds as the stages passed their round start (`LiveSim::mark_keep`).
    pub snapshots_marked: u64,
    /// Plans that went in before every stage had passed their marker: the go-in's flush took
    /// the stages there (`Rounds::mark`).
    pub marker_fallbacks: u64,
    /// Plans whose snapshot the stages took as they passed it, a stage behind at the start.
    pub plans_marked: u64,
}

/// The plan resolved against one solver: unknowns per net, device terminals per pin.
#[derive(Clone)]
pub(crate) struct Taps {
    map: Arc<TapMap>,
    /// (bank, instance, offset in `scratch`, terminal count).
    devices: Vec<(usize, usize, usize, usize)>,
    /// Per pin, a range of `slots`; each slot is an index into `scratch`. A pin whose devices the
    /// solver does not have (another block's) has none.
    pub(crate) pins: Vec<(u32, u32)>,
    /// The pins with a slot, in pin order: the ones `currents` writes (a block's own, not every
    /// pin of the plan).
    held: Vec<u32>,
    slots: Vec<u32>,
    scratch: Vec<f64>,
}

/// What a point's net values and part powers read of the taps: each net's unknown, each pin's net and
/// each part's pins. Shared with a helper core's interval job (`IntervalJob`).
pub(crate) struct TapMap {
    nodes: Vec<usize>,
    pin_net: Vec<Option<u32>>,
    part_pins: Vec<(u32, u32)>,
}

impl Taps {
    pub(crate) fn new(s: &Solver, plan: &ValuePlan) -> Taps {
        let nodes = plan.nets.iter().map(|n| if n == "0" { 0 } else { s.unknown(n).map_or(0, |u| u as usize) }).collect();
        let mut devices: Vec<(usize, usize, usize, usize)> = Vec::new();
        let mut scratch_len = 0;
        let (mut pins, mut slots) = (Vec::with_capacity(plan.pins.len()), Vec::new());
        for terms in &plan.pins {
            let first = slots.len() as u32;
            for term in terms {
                let Some((b, i)) = s.find_device(&term.device) else { continue };
                let n = s.banks[b].terminals();
                if term.terminal as usize >= n {
                    continue;
                }
                let off = match devices.iter().find(|d| d.0 == b && d.1 == i) {
                    Some(d) => d.2,
                    None => {
                        devices.push((b, i, scratch_len, n));
                        scratch_len += n;
                        scratch_len - n
                    }
                };
                slots.push((off + term.terminal as usize) as u32);
            }
            pins.push((first, slots.len() as u32 - first));
        }
        let held = pins.iter().enumerate().filter(|(_, &(_, n))| n > 0).map(|(p, _)| p as u32).collect();
        let map = Arc::new(TapMap { nodes, pin_net: plan.pin_net.clone(), part_pins: plan.part_pins.clone() });
        Taps { map, devices, pins, held, slots, scratch: vec![0.0; scratch_len] }
    }

    /// The current (into the pin) of every pin this solver has a device of, at its `x` with
    /// `xdot`, at time `t`, into `out` by pin index. A pin it has no device of is left as it is in
    /// `out` (every caller's buffer holds 0 there: only this writes those places). O(its devices +
    /// its pins), not O(the plan's pins): a block of a few unknowns pays for its own pins only.
    pub(crate) fn currents(&mut self, s: &Solver, xdot: &[f64], t: f64, out: &mut [f64]) {
        let mut cx = s.base_cx();
        cx.tran = true;
        cx.t = t;
        for &(b, i, off, n) in &self.devices {
            s.banks[b].terminal_currents(i, &s.x, xdot, &cx, &mut self.scratch[off..off + n]);
        }
        for &p in &self.held {
            let (first, n) = self.pins[p as usize];
            out[p as usize] = self.slots[first as usize..(first + n) as usize].iter().map(|&k| self.scratch[k as usize]).sum();
        }
    }
}

impl TapMap {
    /// Each component's power in through its pins: the sum of pin net voltage times pin current.
    fn powers(&self, x: &[f64], pin_i: &[f64], out: &mut [f64]) {
        for (o, &(first, n)) in out.iter_mut().zip(&self.part_pins) {
            *o = (first..first + n)
                .filter_map(|p| self.pin_net.get(p as usize).copied().flatten().map(|net| x[self.nodes[net as usize]] * pin_i[p as usize]))
                .sum();
        }
    }
}

/// The one accumulator of time-weighted statistics (trapezoids over the accepted points), read two
/// ways: a publication's over the points since the previous one, and the grid's frames over fixed
/// stretches of simulated time (`LiveSpec::grid`). Integrals run from the accumulator's start; a
/// window's mean is the difference across it, so a point costs one add a row whatever reads it.
/// A step that crosses a frame's edge is split there, the values at the edge interpolated.
#[derive(Default)]
struct Interval {
    /// The publication's window starts here; `last_t` is the newest point.
    since: f64,
    last_t: f64,
    /// The grid, seconds a frame (0: none), the open frame's place and where its window starts.
    grid: f64,
    k: i64,
    frame_since: f64,
    /// The newest point's values, by net, pin and part, then the integrals since the start, and
    /// their values where the publication's and the frame's windows start.
    last: Vec<f64>,
    int: Vec<f64>,
    int_at_since: Vec<f64>,
    int_at_frame: Vec<f64>,
    /// The nets' extremes over the publication's window and over the frame's.
    min: Vec<f64>,
    max: Vec<f64>,
    frame_min: Vec<f64>,
    frame_max: Vec<f64>,
    nets: usize,
    pins: usize,
    /// The frames closed since the last publication.
    frames: Vec<GridFrame>,
}

impl Interval {
    fn new(t: f64, net_v: &[f64], pins: usize, parts: usize, grid: f64) -> Interval {
        let nets = net_v.len();
        let mut last = vec![0.0; nets + pins + parts];
        last[..nets].copy_from_slice(net_v);
        let grid = if grid.is_finite() && grid > 0.0 { grid } else { 0.0 };
        Interval {
            since: t,
            last_t: t,
            grid,
            k: if grid > 0.0 { (t / grid).floor() as i64 } else { 0 },
            frame_since: t,
            int: vec![0.0; last.len()],
            int_at_since: vec![0.0; last.len()],
            int_at_frame: vec![0.0; last.len()],
            min: net_v.to_vec(),
            max: net_v.to_vec(),
            frame_min: net_v.to_vec(),
            frame_max: net_v.to_vec(),
            last,
            nets,
            pins,
            frames: Vec::new(),
        }
    }

    /// One accepted point: the nets' values, and the pins' currents and parts' dissipation when
    /// subscribed (else their integrals hold: their means are not read). O(rows), and O(rows) more
    /// per frame edge the step crosses.
    fn point(&mut self, t: f64, net_v: &[f64], pins: Option<(&[f64], &[f64])>) {
        let (nets, n_pins) = (self.nets, self.pins);
        loop {
            // to the open frame's edge when the step reaches it, else to `t`
            let edge = if self.grid > 0.0 { (self.k + 1) as f64 * self.grid } else { f64::INFINITY };
            if t >= edge && edge <= self.last_t {
                // the open frame ends where it starts (the run started or went back on its edge)
                self.k += 1;
                continue;
            }
            let to = t.min(edge);
            let f = if t > self.last_t { (to - self.last_t) / (t - self.last_t) } else { 1.0 };
            let dt = to - self.last_t;
            self.segment(dt, f, 0, net_v, true);
            if let Some((pin_i, part_p)) = pins {
                self.segment(dt, f, nets, pin_i, false);
                self.segment(dt, f, nets + n_pins, part_p, false);
            }
            self.last_t = to;
            if t < edge {
                return;
            }
            self.close_frame();
        }
    }

    /// The rows from `first` on take the step of `dt` toward `new`, `f` of the way: the integrals,
    /// the newest values and, for nets, both windows' extremes.
    fn segment(&mut self, dt: f64, f: f64, first: usize, new: &[f64], extremes: bool) {
        for (j, &v) in new.iter().enumerate() {
            let k = first + j;
            let a = self.last[k];
            let e = if f >= 1.0 { v } else { a + f * (v - a) };
            self.int[k] += 0.5 * dt * (a + e);
            self.last[k] = e;
            if extremes {
                self.min[k] = self.min[k].min(e);
                self.max[k] = self.max[k].max(e);
                self.frame_min[k] = self.frame_min[k].min(e);
                self.frame_max[k] = self.frame_max[k].max(e);
            }
        }
    }

    /// Closes the open frame at `last_t` (its edge) and opens the next.
    fn close_frame(&mut self) {
        let (nets, pins) = (self.nets, self.pins);
        let span = self.last_t - self.frame_since;
        let means: Vec<f32> = (0..self.last.len())
            .map(|k| if span > 0.0 { ((self.int[k] - self.int_at_frame[k]) / span) as f32 } else { self.last[k] as f32 })
            .collect();
        let f32s = |v: &[f64]| v.iter().map(|&x| x as f32).collect::<Vec<f32>>();
        self.frames.push(GridFrame {
            k: self.k,
            since: self.frame_since,
            t: self.last_t,
            net_mean: means[..nets].to_vec(),
            net_min: f32s(&self.frame_min),
            net_max: f32s(&self.frame_max),
            pin_mean: means[nets..nets + pins].to_vec(),
            part_mean: means[nets + pins..].to_vec(),
        });
        self.int_at_frame.copy_from_slice(&self.int);
        self.frame_min.copy_from_slice(&self.last[..nets]);
        self.frame_max.copy_from_slice(&self.last[..nets]);
        self.frame_since = self.last_t;
        self.k += 1;
    }

    /// Means over `[since, last_t]` (the newest values when the window is empty), then a new
    /// publication window from `last_t`.
    fn take(&mut self) -> (f64, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) {
        let (nets, pins) = (self.nets, self.pins);
        let span = self.last_t - self.since;
        let mean = |r: std::ops::Range<usize>| -> Vec<f32> {
            r.map(|k| if span > 0.0 { ((self.int[k] - self.int_at_since[k]) / span) as f32 } else { self.last[k] as f32 }).collect()
        };
        let f32s = |v: &[f64]| v.iter().map(|&x| x as f32).collect::<Vec<f32>>();
        let out = (self.since, mean(0..nets), f32s(&self.min), f32s(&self.max), mean(nets..nets + pins), mean(nets + pins..self.last.len()));
        self.since = self.last_t;
        self.int_at_since.copy_from_slice(&self.int);
        self.min.copy_from_slice(&self.last[..nets]);
        self.max.copy_from_slice(&self.last[..nets]);
        out
    }
}

/// A round's merged rows' share of the interval statistics: each point's part powers and its
/// trapezoids, on a helper core while block 0 steps the next round. It holds the interval while it
/// is out, and every reader of the interval brings it home first (`LiveSim::interval_home`): the
/// next delivery, a publication, a rewind. The same points in the same order as on the worker.
struct IntervalJob {
    interval: Interval,
    rows: MergeRows,
    map: Arc<TapMap>,
    net_v: Vec<f64>,
    part_p: Vec<f64>,
    subscribed: bool,
}

/// `buf` taken and cleared to `n` zeros: a buffer an interval job brought home, for the next.
fn zeroed(buf: &mut Vec<f64>, n: usize) -> Vec<f64> {
    let mut v = std::mem::take(buf);
    v.clear();
    v.resize(n, 0.0);
    v
}

impl IntervalJob {
    fn run(&mut self) {
        let n = self.rows.t.len();
        if n == 0 {
            return;
        }
        let (nx, np) = (self.rows.x.len() / n, self.rows.pin_i.len() / n);
        for (i, &t) in self.rows.t.iter().enumerate() {
            let x = &self.rows.x[i * nx..(i + 1) * nx];
            for (v, &u) in self.net_v.iter_mut().zip(&self.map.nodes) {
                *v = x[u];
            }
            if self.subscribed {
                let pins = &self.rows.pin_i[i * np..(i + 1) * np];
                self.map.powers(x, pins, &mut self.part_p);
                self.interval.point(t, &self.net_v, Some((pins, &self.part_p)));
            } else {
                self.interval.point(t, &self.net_v, None);
            }
        }
    }
}

/// A posted `IntervalJob`: a helper claims it, or the worker runs it itself when it comes home
/// unclaimed (a pool that refused it, or one slow to start it).
struct IntervalFlight {
    slot: Arc<(AtomicBool, Mutex<Option<IntervalJob>>)>,
    rx: mpsc::Receiver<IntervalJob>,
}

impl IntervalFlight {
    fn post(job: IntervalJob, h: &Helpers) -> Self {
        let slot = Arc::new((AtomicBool::new(false), Mutex::new(Some(job))));
        let theirs = slot.clone();
        let (tx, rx) = mpsc::channel();
        (h.post)(Box::new(move || {
            if theirs.0.swap(true, Ordering::AcqRel) {
                return;
            }
            let Some(mut job) = theirs.1.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
            job.run();
            let _ = tx.send(job);
        }));
        IntervalFlight { slot, rx }
    }

    fn land(self) -> IntervalJob {
        if !self.slot.0.swap(true, Ordering::AcqRel) {
            let mut job = self.slot.1.lock().unwrap_or_else(|e| e.into_inner()).take().expect("an unclaimed interval job waits in its slot");
            job.run();
            return job;
        }
        self.rx.recv().expect("a helper core lost a round's interval job (it panicked)")
    }
}

/// The points recorded since the last delta that went out.
struct Pending {
    from: usize,
    time: Vec<f64>,
    values: Vec<Vec<f32>>,
    closed: Vec<(SignalId, Arc<Chunk>)>,
    marks: Vec<f64>,
    /// By logic tap: its signal and the entries found since.
    logic: Vec<(SignalId, Vec<LogicEdge>)>,
    logic_closed: Vec<(SignalId, Arc<[LogicEdge]>)>,
    /// The logic chunks spilled or loaded back since (`TraceStore::spill_logic`, `load_logic`).
    logic_pages: Vec<LogicPage>,
    /// The record's folds since (`TraceStore::fold`), in order.
    folds: Vec<Fold>,
    /// The run went back since the last delta (`TraceStore::rewind_producer`): this delta's first
    /// act.
    rewind: Option<Rewind>,
    /// Deltas finished and not yet sent, oldest first: what was pending when the run went back
    /// goes out as it was, so every consumer cuts the store the producer cut.
    ready: VecDeque<TraceDelta>,
}

impl Pending {
    fn new(signals: usize) -> Pending {
        Pending { from: 0, time: Vec::new(), values: vec![Vec::new(); signals], closed: Vec::new(), marks: Vec::new(), logic: Vec::new(), logic_closed: Vec::new(), logic_pages: Vec::new(), folds: Vec::new(), rewind: None, ready: VecDeque::new() }
    }
}

/// A net a GPIO pad reads, watched as logic (`TraceStore::logic`): its signal, the pad's and its
/// supply rails' unknowns, the input's thresholds (fractions of the supply, `GpioParams::vih` and
/// `vil`) and the level it reads. A level changes where the pad crosses the threshold that
/// changes it (interpolated between the two points around it), as the input sees it: a slow edge
/// or a glitch inside the band changes nothing.
#[derive(Clone)]
struct LogicTap {
    id: SignalId,
    pad: usize,
    vdd: usize,
    vss: usize,
    vih: f64,
    vil: f64,
    high: Option<bool>,
    /// The last point: its time and the pad's fraction of the supply.
    last: Option<(f64, f64)>,
}

impl LogicTap {
    /// The entry at accepted point `t` (`x` the unknowns), when the level became known or changed.
    fn point(&mut self, t: f64, x: &[f64]) -> Option<LogicEdge> {
        let supply = x[self.vdd] - x[self.vss];
        if !(supply > 0.0) {
            // unpowered: the input reads nothing, and no edge spans the gap
            self.last = None;
            return None;
        }
        let f = (x[self.pad] - x[self.vss]) / supply;
        let last = self.last.replace((t, f));
        let (level, high) = match self.high {
            None if f >= self.vih => return self.set(LogicEdge { t, high: true }),
            None if f <= self.vil => return self.set(LogicEdge { t, high: false }),
            Some(false) if f >= self.vih => (self.vih, true),
            Some(true) if f <= self.vil => (self.vil, false),
            _ => return None,
        };
        let at = match last {
            Some((t0, f0)) if (f0 < level) == high && f != f0 => (t0 + (t - t0) * (level - f0) / (f - f0)).clamp(t0, t),
            _ => t,
        };
        self.set(LogicEdge { t: at, high })
    }

    fn set(&mut self, edge: LogicEdge) -> Option<LogicEdge> {
        self.high = Some(edge.high);
        Some(edge)
    }
}

/// The logic taps of `circuit` running in `s`: one per net of `plan` a GPIO pad sits on (the
/// first pad's thresholds), in GPIO order.
fn logic_taps(circuit: &SimCircuit, s: &Solver, plan: &ValuePlan) -> Vec<LogicTap> {
    let unknown = |node: u32| {
        let name = circuit.nodes.get(node as usize)?.name.as_str();
        if name == "0" { Some(0) } else { s.unknown(name).map(|u| u as usize) }
    };
    let mut taps: Vec<LogicTap> = Vec::new();
    for g in &circuit.devices.gpio {
        let Some(net) = circuit.nodes.get(g.pad as usize).and_then(|node| plan.nets.iter().position(|n| *n == node.name)) else { continue };
        let id = SignalId(net as u32);
        if taps.iter().any(|tap| tap.id == id) {
            continue;
        }
        let (Some(pad), Some(vdd), Some(vss)) = (unknown(g.pad), unknown(g.vdd), unknown(g.vss)) else { continue };
        taps.push(LogicTap { id, pad, vdd, vss, vih: g.params.vih, vil: g.params.vil, high: None, last: None });
    }
    taps
}

/// A heard run with a parked planner steps its set-up whole before its clock starts. Only a new
/// run can enter Preparing. An ordinary run keeps the first-plan behaviour of `round_start`.
#[derive(Clone, Copy)]
enum Start {
    New,
    Ordinary,
    Preparing { t0: f64, wall0: Duration, rounds: bool },
    Planning,
    /// Set-up has ended. A command can end it between steps, before the worker has its sink at hand.
    Done { notify: bool },
}

/// One live simulation: the solver, its stepper, the resolved plan, the trace and the statistics.
pub struct LiveSim {
    /// What decides the next accepted point and sample; a snapshot is its clone.
    core: Core,
    spec: LiveSpec,
    /// The sample period, seconds (`LiveSpec::sample_rate`), fixed for the run's life.
    period: f64,
    generation: u64,
    taps: Taps,
    store: TraceStore,
    /// Where the logic traces' oldest chunks go past `trace::LOGIC_KEEP` entries in memory.
    spill: LogicSpill,
    pending: Pending,
    interval: Interval,
    /// Subscribed pins and parts (`LiveCmd::Subscribe`); the means of the rest stay 0.
    sub_pins: Vec<u32>,
    sub_parts: Vec<u32>,
    pin_i: Vec<f64>,
    /// Rows waiting for the sinks, even after the rounds that made them have ended.
    merged: MergeRows,
    /// The interval while a helper core takes a round's share of it (`IntervalJob`), and the rows
    /// buffer its job hands back for the next round's rows.
    interval_out: Option<IntervalFlight>,
    spare_rows: MergeRows,
    /// The last interval job's buffers, for the next (`deliver_rows`).
    interval_spare: (Vec<f64>, Vec<f64>),
    /// The last identity given to installed rounds, kept across replacements and rewinds.
    rounds_id: u64,
    part_p: Vec<f64>,
    net_v: Vec<f64>,
    trace_values: Vec<f64>,
    /// The plan's net names by signal: what an attached process's logic nets are traced as.
    plan_nets: Vec<String>,
    /// The attached process's nets off the circuit, valued from their logic record
    /// (`LiveSim::co_logic_rows`).
    logic_rows: Vec<LogicRow>,
    /// Scratch for an attached process's level changes (`CoSim::take_logic`).
    co_logic: Vec<(u32, LogicEdge)>,
    /// Scratch for an attached process's sound (`CoSim::take_audio`).
    co_audio: CoAudio,
    /// Scratch for what the sinks hear at the sample rate (`Sink::heard`), sorted.
    heard: Vec<u32>,
    playing: bool,
    step_until: Option<f64>,
    /// The run stopped at the take's end (`LiveSpec::until`), not by a Pause: an edit before the
    /// end, or a later end, sets it playing again.
    ended: bool,
    /// Commands that came after a `Step`, applied once its time is reached, each with how many
    /// commands `run_live` had taken before it.
    held: VecDeque<(u64, LiveCmd)>,
    /// The run's timed value commands (`LiveCmd::At`) by time, those before `next_event` applied
    /// (at the times kept here).
    timeline: Vec<(f64, LiveCmd)>,
    /// How many commands `run_live` had taken before the one that put each timeline entry there
    /// (`taken`), entry by entry: a publication counts no command of an entry not yet applied
    /// (`claims`).
    timeline_taken: Vec<u64>,
    /// Where each plan went in (`go_ins`), by the time it was started: part of the timeline.
    go_ins: Vec<(f64, f64)>,
    /// A value changed: publish right after the next point.
    publish_soon: bool,
    /// The advance's target is the wall clock's (`advance_to_clock`, for its call): reaching it
    /// catches up nothing, as a budget's end does not.
    paced: bool,
    /// The run's start ended since the last publication: the next values say so and come at once.
    started: bool,
    /// The commands `run_live` has taken, and how many of them it had when the run last stepped:
    /// what a publication counts of them at most (`claims`).
    taken: u64,
    stepped_with: u64,
    /// The value commands applied while what a publication carries may not yet have been
    /// produced past where they applied: each one's instant and how many commands `run_live` had
    /// taken before it. A publication counts one only once every value it carries was produced
    /// past its instant (`claims`).
    unseen: Vec<(f64, u64)>,
    /// The newest point the sinks have from the rounds (`deliver_rows`, `merge_points`): the
    /// whole circuit's state the values publish. Where the rounds begin, and where the run goes
    /// back to, that state's own time.
    delivered: f64,
    /// `LiveValues::worker_split`, the latest window's.
    worker_split: Option<(u64, Arc<str>)>,
    /// The host's priority for the worker while a listener hears the run as it is made
    /// (`set_heard_priority`), and whether it is raised now.
    heard_priority: Option<Box<dyn FnMut(bool) + Send>>,
    heard_raised: bool,
    /// `LiveValues::plan_error`: `stats.plan_error` as text, made once per failure.
    plan_error_text: Option<Arc<str>>,
    /// The streamed inputs the run holds for (`LiveCmd::Inputs`, `crate::input`). The run's, not
    /// a snapshot's: a run sent back reads behind their horizons.
    inputs: Option<InputSet>,
    /// `LiveValues::held`: the run held since the last publication, and for what.
    hold_seen: Option<HoldKind>,
    stats: LiveStats,
    /// An armed `CoStep` runs: `running()`, unpaced, until the device stops or a `Pause`.
    co_step: bool,
    /// Over the interval since the last publication: the wall time spent stepping (`advance`), the
    /// part of it in the device's `run`s, without the analog steps those asked for (`co_share`),
    /// and the part in the sink (`sink_share`).
    step_wall: Duration,
    co_wall: Duration,
    sink_wall: Duration,
    /// The lowering's value plan: what the blocks' pins resolve against.
    vplan: ValuePlan,
    helpers: Option<Helpers>,
    checks: Option<Checks>,
    /// Where the host's runs remember their refusals and landings (`set_memory`).
    memory: Option<StateCache>,
    /// Rounds turned off (`set_rounds`): the planner waits here and the circuit steps whole.
    parked: Option<Option<Box<dyn Planner>>>,
    start: Start,
    /// When the planner in `parked` warms after the pads' set-up or the first whole-rate measurement.
    warm_due: Option<f64>,
    /// The handle for the posted `Planner::warm` job.
    warm_out: Option<crate::rounds::Warm>,
    /// Rounds were asked for during the warm and come when it finishes.
    rounds_asked: bool,
    /// The heard start, or a replacement of the circuit, begins on a carried cut
    /// (`Planner::can_carry`): no warm is posted, and its first plan is the carried one, its key's
    /// own made behind it.
    carrying: bool,
    /// The carry the run's start tried (`Planner::carried`), taken or refused, part of its timeline
    /// beside its go-in rounds: a run made again from them (`follow`) tries it, and no other.
    carried: Option<Arc<crate::analysis::round::Carry>>,
    pack_shuffle: Option<u64>,
    /// The job making `core.plan` and where it comes back (remade from the inputs by a restore).
    plan_run: Option<PlanRun>,
    /// The snapshots an edit behind the front goes back to (`LiveSpec::snapshots`).
    ring: Ring,
    /// The time the run went back to and the sinks' state there, for the sinks before the next
    /// point (`rewind_sinks`).
    sink_rewind: Option<(f64, Option<Box<dyn SinkState>>)>,
    /// The refusals of the growth rule (`breach_taken`) and the shadow rounds (`shadow_taken`), by
    /// the time they take effect from: part of the timeline, as the go-in rounds are (`refusals`,
    /// `follow`).
    refusals: Vec<(f64, Vec<String>)>,
    /// The chain shadow's landings (`chain_taken`), each with the time its prefix takes effect.
    /// Part of the timeline beside the refusals (`landings`, `follow`).
    landings: Vec<(f64, Land)>,
    /// Made again from a timeline (`follow`): its refusals and landings are the timeline's, and its
    /// shadow rounds and chain trees, back at other rounds than the run's were, count only.
    followed: bool,
    /// The pads its start warmed in are in the host's memory (`keep_start_pads`).
    pads_kept: bool,
    /// The stop an armed `CoStep` runs to: armed again after the run goes back.
    co_to: Option<StepTo>,
    /// Timed commands stamped from the host's view that came late (their time already passed), by
    /// device and key, with their time and the run's when they came: the next of the same device
    /// and key keeps its distance, whenever it comes (`schedule`).
    late: Vec<((String, String), f64, f64)>,
    /// A snapshot on its way in rounds (`mark_keep`).
    keeping: Option<Box<KeepCore>>,
}

/// The snapshots kept (`LiveSpec::snapshots`), oldest first, and when the last was taken.
#[derive(Default)]
struct Ring {
    kept: Vec<Kept>,
    /// The run's restore point that is never dropped when it keeps no snapshot (`take_anchor`):
    /// taken where a plan's rounds begin, so a run in rounds can always go back before any of them
    /// (`breach_taken`). A run that keeps snapshots has the ring's first for that (`fit`). The
    /// rounds it was looked at for (their identity).
    anchor: Option<Kept>,
    anchor_for: u64,
    /// `LiveStats::step_wall` at the last snapshot, and what that one took.
    at_wall: Duration,
    cost: Duration,
    /// The kept snapshots are at their byte budget (logged when they reach it).
    full: bool,
}

/// A kept snapshot, the sinks' state at it (`Sink::save`), how often it outlived a newer
/// neighbour (0 in the dense span until the budget thins it), and its size in the budget.
#[derive(Clone)]
struct Kept {
    snap: Snapshot,
    sinks: Option<Box<dyn SinkState>>,
    level: u32,
    bytes: u64,
}

/// A snapshot on its way in rounds (`LiveSim::mark_keep`): the run's state but for its rounds, taken
/// at its round start, the worker time it took there and the bytes (None: unmeasured).
struct KeepCore {
    core: Core,
    cost: Duration,
    bytes: Option<u64>,
}

/// A run's state at a loop top (`LiveSim::snapshot`), to go back to (`LiveSim::restore`).
#[derive(Clone)]
pub struct Snapshot {
    t: f64,
    core: Core,
}

impl Snapshot {
    /// The simulated time the run stood at.
    pub fn t(&self) -> f64 {
        self.t
    }
}

/// Everything that decides the next accepted point or sample: what a snapshot keeps
/// (`LiveSim::snapshot`). A field added here is cloned with it, so state that steers stepping
/// belongs here; the publication (trace, statistics, scratch, sinks) stays in `LiveSim`.
#[derive(Clone)]
struct Core {
    /// The whole circuit and its stepper (in rounds `s` is never stepped: see `rounds`).
    s: Solver,
    st: Stepper,
    /// The nets read as logic (`TraceStore::logic`), traced with the samples.
    logic: Vec<LogicTap>,
    /// Settled: playing, no Step running, no attached process with a next instant, and
    /// `Stepper::steady`; the loop stops stepping until a command.
    steady: bool,
    /// The last rest mark ([`REST_EVERY`]) the run checked, or `i64::MIN` before its first point.
    rest_mark: i64,
    /// Every solver's `x` at the last rest mark, the whole circuit's or the blocks' in block order, end to end.
    rest_x: Vec<f64>,
    next_event: usize,
    /// Each GPIO's mode as the circuit lowered it: a `Replace` writes only the pins whose lowered
    /// mode changed (the bench's stimulus) and keeps the running mode of the others.
    lowered_pins: Vec<(String, PinMode)>,
    /// The co-simulated process, if one is attached.
    co: Option<Co>,
    /// The latest instant the attached process changed a pad at (`LiveStats::write_instants`).
    write_t: f64,
    /// Watch hits accepted since the process last ran (`Bus::hits`).
    hits: Vec<(u32, f64)>,
    /// Warm starts under a time resolution (`set_warm_start`).
    warm: Warm,
    /// The circuit as lowered for this run, with the sources retuned since: what a planner cuts
    /// into blocks (shared with the snapshots and a plan's inputs until it is retuned).
    ckt: Arc<SimCircuit>,
    /// The run in rounds: its blocks, each with its stepper. None: the whole circuit steps on
    /// `st`, and `s` is what it solves. In rounds `s` stays the whole circuit, never stepped: it
    /// holds the merged stream's newest point (what the sinks and publications read), every
    /// edit, and the state a new plan or lowering starts from.
    rounds: Option<Box<Rounds>>,
    planner: Option<Box<dyn Planner>>,
    /// The values set since the lowering (`SetParam`, knobs, thermal noise), the newest per device
    /// and key: a plan's blocks, built from the lowered circuit, take them too.
    edits: Vec<(String, String, f64)>,
    /// When a re-plan falls due (`Replan`), the plan on its way (as its inputs: a restore makes it
    /// again from them), what the last plan took, and the pads whose first mode change has been
    /// seen.
    replan_due: Option<f64>,
    /// When a first plan waiting for the attached process to move its pads falls due
    /// (`LiveSim::first_plan`): made on the worker then, and in at once.
    first_due: Option<f64>,
    /// When a re-plan for a pad's first change of kind falls due (`LiveSim::written`): at the first
    /// such change's time, edits never pushing it on, its plan going in through them
    /// (`PlanInputs::through`). The plan in force was tested against other pads.
    kind_due: Option<f64>,
    plan: Option<PlanInputs>,
    plan_cost: Duration,
    pads_moved: Vec<String>,
    /// The nodes the run refused (`LiveSim::refusals`, applied in time by `apply_refusals`): no
    /// plan that reads a value late at one goes in. How many of the refusals are applied.
    refused: Vec<String>,
    refusals_done: usize,
    /// The chain prefixes each plan lands (`Planner::land_chain`), applied in timeline order by
    /// `apply_landings`, and how many timeline entries have been applied.
    landed: Vec<Land>,
    landings_done: usize,
}

/// A plan to make from a snapshot of the run: the planner, the lowered circuit and the whole
/// circuit with the blocks' state, and where the plan goes.
struct PlanJob {
    planner: Box<dyn Planner>,
    ckt: Arc<SimCircuit>,
    whole: Solver,
    /// The snapshot's time.
    t: f64,
    /// The plan in force when it started (`Planner::plan_from`).
    in_force: Option<crate::rounds::RoundPlan>,
    stop: Arc<AtomicBool>,
    tx: std::sync::mpsc::Sender<PlanDone>,
}

impl PlanJob {
    fn run(self, helped: bool) {
        let PlanJob { mut planner, ckt, whole, t, in_force, stop, tx } = self;
        let start = Instant::now();
        let got = crate::rounds::with_stop(stop, || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| planner.plan_from(&ckt, &whole, t, in_force.as_ref())))
                .unwrap_or_else(|_| Err(SimDiag::setup("round plan: the planner panicked".into())))
        });
        let _ = tx.send(PlanDone { planner, got, cost: start.elapsed(), helped });
    }
}

/// A plan made, and its planner back.
struct PlanDone {
    planner: Box<dyn Planner>,
    got: Result<Option<(crate::rounds::RoundPlan, Vec<Solver>)>, SimDiag>,
    cost: Duration,
    helped: bool,
}

/// A plan on its way, as what makes it: it goes in at the first round start at or after `at`,
/// made by (a fork of) `planner` from `ckt` and `whole` as they were when it started. `retunes`
/// are the sources retuned since, in order, which its blocks take as the run did. The same
/// inputs make the same plan, so a restored run makes it again (`LiveSim::restore`).
#[derive(Clone)]
struct PlanInputs {
    /// The snapshot's time.
    t: f64,
    at: f64,
    planner: Box<dyn Planner>,
    ckt: Arc<SimCircuit>,
    /// None while the rounds' marker takes it as the stages pass `t` (`Rounds::mark`).
    whole: Option<Solver>,
    retunes: Vec<(f64, String, Source)>,
    /// The plan in force when it started, as the planner takes it (`Planner::plan_from`).
    in_force: Option<crate::rounds::RoundPlan>,
    /// The devices edited since its snapshot while the run stepped whole, or while it was on its
    /// way through edits (`through`, `guard_edit`): its blocks go in with the edits (`install`),
    /// proved at its first round's end (`edited_before`).
    edited: Vec<String>,
    /// Whether it goes in through edits in rounds too, as on one stepper (`guard_edit`). It
    /// replaces a plan whose pads have since changed in kind (`Core::kind_due`). An edit in
    /// rounds recalls any other plan.
    through: bool,
    /// The key's own plan behind a carried start: made on the checkers' helpers whatever the last
    /// plan cost, as it pays the key's states.
    behind: bool,
}

/// The job making a plan on its way: `job` holds it until a core takes it (as a pack is: one
/// posted to a helper core that none has started, the worker makes itself); it comes back on
/// `rx`, and a heard run that looked for it keeps it in `back` (None inside: lost on its helper).
struct PlanRun {
    job: Arc<std::sync::Mutex<Option<PlanJob>>>,
    /// Set when the run lets go of the plan: a helper making it stops at its next unit.
    stop: Arc<AtomicBool>,
    rx: std::sync::mpsc::Receiver<PlanDone>,
    back: Option<Option<PlanDone>>,
    /// The first round start at or after the plan's time, in a heard run that looked for it there.
    due_round: Option<f64>,
}

impl PlanRun {
    /// The job if no core has taken it.
    fn take(&self) -> Option<PlanJob> {
        self.job.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

impl Drop for PlanRun {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// The solver and stepper the attached process works with: block 0's in rounds, else the run's.
fn ctl_of<'a>(rounds: &'a mut Option<Box<Rounds>>, s: &'a mut Solver, st: &'a mut Stepper) -> (&'a mut Solver, &'a mut Stepper) {
    match rounds {
        Some(r) => {
            let b = r.ctl_mut();
            (&mut b.s, &mut b.st)
        }
        None => (s, st),
    }
}

/// What one point updates besides the sink: the trace, the logic taps and the interval
/// statistics. A single stepper's accepted point and a round's merged point alike.
struct Outs<'a> {
    stats: &'a mut LiveStats,
    sink_wall: &'a mut Duration,
    pending: &'a mut Pending,
    net_v: &'a mut [f64],
    taps: &'a Taps,
    pin_i: &'a [f64],
    part_p: &'a mut [f64],
    store: &'a mut TraceStore,
    trace_values: &'a mut Vec<f64>,
    logic: &'a mut [LogicTap],
    /// None: a helper core takes the points' interval share later (`IntervalJob`).
    interval: Option<&'a mut Interval>,
    subscribed: bool,
    trace: bool,
    trace_pins: bool,
    /// The caller puts the points' traced values in itself, a batch of rows at once
    /// (`TraceStore::push_rows`); the marks and the logic taps still go in here.
    batch: bool,
}

/// One point at `t` with the whole circuit's values `x`, its pins' currents already in
/// `o.pin_i` when they are subscribed or traced. In a merge, `next_at` carries the start of
/// the call to `next_point` and returns the start of the next one.
fn put(o: Outs, sink: &mut dyn Sink, t: f64, x: &[f64], event: bool, coarse: bool, chosen: bool, next_at: Option<&mut Instant>) {
    o.stats.points += 1;
    // the sink's own time, apart from the solver's: two clock reads per accepted point, one pair
    // for a batch of rows (`deliver_rows`)
    let at = (!o.batch).then(Instant::now);
    sink.point(t, None, x);
    if event {
        sink.mark(t);
    }
    if coarse {
        sink.below_res(t);
    }
    if chosen {
        sink.chosen(t);
    }
    if let Some(at) = at {
        let spent = at.elapsed();
        *o.sink_wall += spent;
        o.stats.sink_wall += spent;
    }
    if event && o.trace {
        o.pending.marks.push(t);
    }
    // the nets' values, for the sinks that take them, the interval and the unbatched trace: with
    // none of them, no copy
    let values = sink.takes_values();
    if values || o.interval.is_some() || o.trace && !o.batch {
        for (v, &u) in o.net_v.iter_mut().zip(&o.taps.map.nodes) {
            *v = x[u];
        }
    }
    if values {
        sink.values(t, &o.net_v[..], (o.subscribed || o.trace_pins).then_some(&o.pin_i[..]));
    }
    let trace_at = (o.trace && next_at.is_some()).then(Instant::now);
    if o.trace {
        if !o.batch {
            o.trace_values.clear();
            o.trace_values.extend_from_slice(o.net_v);
            if o.trace_pins {
                o.trace_values.extend_from_slice(o.pin_i);
            }
            o.store.push(t, o.trace_values, &mut o.pending.closed);
            let at = o.pending.time.len();
            o.pending.time.push(t);
            let fine = o.store.fine.as_deref();
            for (_, (p, &y)) in o.pending.values.iter_mut().zip(o.trace_values.iter()).enumerate().filter(|(k, _)| writes(fine, *k)) {
                fill_gap(p, at);
                p.push(y as f32);
            }
        }
        for (tap, (_, found)) in o.logic.iter_mut().zip(o.pending.logic.iter_mut()) {
            if let Some(edge) = tap.point(t, x) {
                o.store.push_logic(tap.id, edge, &mut o.pending.logic_closed);
                found.push(edge);
            }
        }
    }
    let interval_at = next_at.as_ref().map(|_| Instant::now());
    if let Some(interval) = o.interval {
        if o.subscribed {
            o.taps.map.powers(x, o.pin_i, o.part_p);
            interval.point(t, o.net_v, Some((o.pin_i, o.part_p)));
        } else {
            interval.point(t, o.net_v, None);
        }
    }
    if let (Some(next_at), Some(interval_at), Some(at)) = (next_at, interval_at, at) {
        let end = Instant::now();
        let split = &mut o.stats.round_split;
        split.next_point_ns += (at - *next_at).as_nanos() as u64;
        split.put_ns += (end - at).as_nanos() as u64;
        if let Some(trace_at) = trace_at {
            split.put_trace_ns += (interval_at - trace_at).as_nanos() as u64;
        }
        split.put_interval_ns += (end - interval_at).as_nanos() as u64;
        split.merged_points += 1;
        *next_at = end;
    }
}

/// Warm starts, under a time resolution only: the first step after a pin write starts Newton from
/// the solution last converged under the same pin pattern. The pattern hashes every written pin's
/// mode (a PWM's by kind alone, so a duty change keeps the pattern); a pin never written counts as
/// its lowered mode. Patterns are as many as the mode combinations the pins take.
#[derive(Clone)]
struct Warm {
    on: bool,
    modes: HashMap<(usize, usize), u64>,
    pattern: u64,
    /// When the pattern was entered: one left at that same instant (pins written together) had no
    /// solution of its own.
    since: f64,
    kept: HashMap<u64, Vec<f64>>,
}

/// A pin's key before its first write.
const UNWRITTEN: u64 = u64::MAX;

/// How often a run a sink holds asks again (`Sink::holds`), between commands.
const HOLD_POLL: Duration = Duration::from_millis(5);

/// A board stepping whole below this multiple of the run's rate asks for rounds from its start.
/// Above it the board starts whole and the host's meter can still ask for rounds later.
const ROUNDS_FROM_START: f64 = 1.5;

/// The run checks for rest at its first point past each multiple of this much simulated time.
/// Fresh runs therefore rest at the same point whatever the machine's load.
const REST_EVERY: f64 = 0.016;

fn mode_key(mode: PinMode) -> u64 {
    match mode {
        PinMode::OutHigh => 1,
        PinMode::OutLow => 2,
        PinMode::Input(pull) => 3 | ((pull as u64) << 8),
        PinMode::OpenDrain { low, pull } => 4 | (u64::from(low) << 4) | ((pull as u64) << 8),
        PinMode::Pwm { .. } => 5,
    }
}

fn pin_hash(dev: (usize, usize), key: u64) -> u64 {
    let h = (dev.0 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (dev.1 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ key;
    let h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^ (h >> 31)
}

impl Warm {
    fn new() -> Warm {
        Warm { on: true, modes: HashMap::new(), pattern: 0, since: f64::NEG_INFINITY, kept: HashMap::new() }
    }

    /// A pin write that restarts the history at `t`: with `keep`, keeps `x` (the newest solution,
    /// under the pattern being left) unless that pattern was entered at `t`, and returns the
    /// solution kept under the new pattern. One copy and two lookups per write.
    fn enter(&mut self, dev: (usize, usize), mode: PinMode, t: f64, x: &[f64], keep: bool) -> Option<&[f64]> {
        if keep && t > self.since {
            let kept = self.kept.entry(self.pattern).or_default();
            kept.clear();
            kept.extend_from_slice(x);
        }
        let key = mode_key(mode);
        let old = self.modes.insert(dev, key).unwrap_or(UNWRITTEN);
        self.pattern ^= pin_hash(dev, old) ^ pin_hash(dev, key);
        self.since = t;
        if keep { self.kept.get(&self.pattern).map(Vec::as_slice) } else { None }
    }
}

/// Each GPIO's mode as lowered, by device name.
fn lowered_pins(circuit: &SimCircuit) -> Vec<(String, PinMode)> {
    circuit.devices.gpio.iter().map(|g| (g.name.clone(), g.mode)).collect()
}

/// An attached process's device. A clone of the run's `Core` forks it (`CoSim::fork`); whether
/// it can is found once, when it is attached, and a run whose device cannot makes no snapshots.
struct Dev {
    dev: Box<dyn CoSim>,
    forks: bool,
}

impl Dev {
    fn new(dev: Box<dyn CoSim>) -> Dev {
        let forks = dev.fork().is_some();
        Dev { dev, forks }
    }
}

impl Clone for Dev {
    fn clone(&self) -> Dev {
        let dev = self.dev.fork().expect("a device that forked when it was attached forks again (`LiveSim::snapshot` checks `forks`)");
        Dev { dev, forks: true }
    }
}

impl std::ops::Deref for Dev {
    type Target = dyn CoSim;
    fn deref(&self) -> &(dyn CoSim + 'static) {
        &*self.dev
    }
}

impl std::ops::DerefMut for Dev {
    fn deref_mut(&mut self) -> &mut (dyn CoSim + 'static) {
        &mut *self.dev
    }
}

/// A net an attached process keeps off the circuit (`CoSim::logic_nets`) and traces as logic: its
/// row (its signal's index), its signal, and the rails its levels stand at (the process's supply:
/// its pin table's first row whose rails the circuit has), unknowns of the whole circuit.
#[derive(Clone, Copy)]
struct LogicRow {
    row: usize,
    id: SignalId,
    vdd: usize,
    vss: usize,
}

/// A logic record's levels over `[a, b)` at the rails `vss` and `vdd`: the time-weighted mean,
/// the lowest and the highest (at `b` alone for an empty span). Before the record's first level
/// the span reads as its first; None when the record has no level in or before it. O(log
/// entries + the entries in the span).
fn logic_volts(trace: &LogicTrace, a: f64, b: f64, vss: f64, vdd: f64) -> Option<(f64, f64, f64)> {
    let at = |high: bool| if high { vdd } else { vss };
    if !(b > a) {
        let v = at(trace.level_at(b)?);
        return Some((v, v, v));
    }
    let mut i = trace.upto(a);
    let mut level = match trace.level_at(a) {
        Some(level) => level,
        None => trace.get(i).filter(|e| e.t < b)?.high,
    };
    let (mut from, mut high_time, mut low, mut high) = (a, 0.0, !level, level);
    while let Some(e) = trace.get(i).filter(|e| e.t < b) {
        if level {
            high_time += e.t - from;
        }
        (from, level) = (e.t.max(a), e.high);
        low |= !level;
        high |= level;
        i += 1;
    }
    if level {
        high_time += b - from;
    }
    Some((vss + high_time / (b - a) * (vdd - vss), at(!low), at(high)))
}

/// An attached process and its pin table resolved in the running solver.
#[derive(Clone)]
struct Co {
    dev: Dev,
    table: Vec<crate::cosim::CoSimPin>,
    pins: Vec<CoPin>,
    /// Per logic net of the process (`CoSim::logic_nets`): the signal it is traced as (None: the
    /// run keeps no trace, the plan has no such net, or a GPIO tap reads it from the circuit).
    nets: Vec<Option<SignalId>>,
    /// The watch generation registered with the GPIO bank (None: register again).
    watch_gen: Option<u32>,
}

/// A pin table row in the solver: its GPIO's handle, its pad and low rail unknowns.
#[derive(Clone, Copy)]
struct CoPin {
    gpio: Option<(usize, usize)>,
    pad: Option<usize>,
    vss: Option<usize>,
    vdd: Option<usize>,
}

fn resolve_pins(s: &Solver, table: &[crate::cosim::CoSimPin]) -> Vec<CoPin> {
    let node = |name: &str| if name == "0" { Some(0) } else { s.unknown(name).map(|u| u as usize) };
    table.iter().map(|row| CoPin {
        gpio: s.find_device(&row.gpio), pad: node(&row.pad), vss: node(&row.vss), vdd: node(&row.vdd),
    }).collect()
}

impl LiveSim {
    /// Sets up `circuit` (its reltol replaced by the spec's), solves its operating point and stands
    /// at t = 0, paused or playing as the spec says.
    pub fn new(circuit: SimCircuit, plan: ValuePlan, generation: u64, spec: LiveSpec) -> Result<LiveSim, SimDiag> {
        Self::new_from(circuit, plan, generation, spec, None)
    }

    /// `new`, the operating point first tried by plain Newton from `seed` (an ensemble member from
    /// its nominal's operating point, a run restarted after an edit from the last one).
    pub fn new_from(circuit: SimCircuit, plan: ValuePlan, generation: u64, spec: LiveSpec, seed: Option<&OpSeed>) -> Result<LiveSim, SimDiag> {
        Self::new_tuned(circuit, plan, generation, spec, seed, &[])
    }

    /// `new_from` with device values set before the operating point (`(device, key, value)` as a
    /// `LiveCmd::SetParam` sets them; unknown ones are skipped): a run restarted with knobs turned
    /// starts settled at their values, as if the circuit had them, rather than stepping to them
    /// at 0.
    pub fn new_tuned(mut circuit: SimCircuit, plan: ValuePlan, generation: u64, spec: LiveSpec, seed: Option<&OpSeed>, params: &[(String, String, f64)]) -> Result<LiveSim, SimDiag> {
        circuit.opts.reltol = spec.reltol_at(spec.res);
        let period = spec.period(&circuit);
        tune_noise(&mut circuit, period);
        let mut s = Solver::new(&circuit)?;
        let mut edits = Vec::new();
        for (device, key, value) in params {
            if s.set_param(device, key, *value) {
                edits.push((device.clone(), key.clone(), *value));
            }
        }
        let st = Stepper::start_from(&mut s, false, spec.max_step, seed)?;
        let pins = lowered_pins(&circuit);
        let mut sim = Self::assemble(s, st, circuit, plan, generation, spec, period);
        sim.start = Start::New;
        sim.core.lowered_pins = pins;
        sim.core.edits = edits;
        sim.watch_logic();
        Ok(sim)
    }

    /// The nets of the circuit read as logic, traced from now when the run keeps a trace.
    fn watch_logic(&mut self) {
        self.core.logic = if self.spec.trace { logic_taps(&self.core.ckt, &self.core.s, &self.vplan) } else { Vec::new() };
        self.pending.logic = self.core.logic.iter().map(|tap| (tap.id, Vec::new())).collect();
        self.plan_nets.clone_from(&self.vplan.nets);
    }

    /// The attached process's nets off the circuit (`LogicRow`): each of its traced logic nets the
    /// whole circuit has no unknown for (a net between the process and another it emulates, as an
    /// MCU's lines to a shift register), at the process's rails. None without a pin row whose rails
    /// the circuit has. O(logic nets).
    fn co_logic_rows(&self) -> Vec<LogicRow> {
        let Some(co) = &self.core.co else { return Vec::new() };
        let s = &self.core.s;
        let node = |name: &str| if name == "0" { Some(0) } else { s.unknown(name).map(|u| u as usize) };
        let Some((vdd, vss)) = co.table.iter().find_map(|p| Some((node(&p.vdd)?, node(&p.vss)?))) else { return Vec::new() };
        co.dev.logic_nets().iter().zip(&co.nets).filter_map(|(name, id)| {
            let id = (*id)?;
            node(name).is_none().then_some(LogicRow { row: id.0 as usize, id, vdd, vss })
        }).collect()
    }

    /// An attached process's logic nets as the run's signals (`Co::nets`), each traced from now
    /// after the GPIO taps. O(logic nets x plan nets).
    fn co_nets(&mut self, dev: &dyn CoSim) -> Vec<Option<SignalId>> {
        dev.logic_nets().iter().map(|name| {
            if !self.spec.trace {
                return None;
            }
            let id = SignalId(self.plan_nets.iter().position(|net| net == name)? as u32);
            if self.core.logic.iter().any(|tap| tap.id == id) {
                return None;
            }
            if !self.pending.logic.iter().any(|(of, _)| *of == id) {
                self.pending.logic.push((id, Vec::new()));
            }
            Some(id)
        }).collect()
    }

    /// Takes the attached process's level changes up to now into the trace; with `sync`, after it
    /// committed every one up to now (only while one of its nets is traced). Untraced nets' changes
    /// are dropped.
    fn take_co_logic(&mut self, sync: bool) {
        let t = self.t();
        let Some(co) = &mut self.core.co else { return };
        if co.nets.is_empty() || (sync && co.nets.iter().all(Option::is_none)) {
            return;
        }
        co.dev.take_logic(t, sync, &mut self.co_logic);
        self.trace_co_logic();
    }

    /// Hands the sound the attached process committed to the sinks (`Sink::co_audio`).
    fn take_co_audio(&mut self, sink: &mut dyn Sink) {
        let Some(co) = &mut self.core.co else { return };
        co.dev.take_audio(&mut self.co_audio);
        if !self.co_audio.is_empty() {
            sink.co_audio(&self.co_audio);
            self.co_audio.clear();
        }
    }

    /// A new record: the attached process's traced nets start at their levels now, so none is
    /// blank until its next change.
    fn restart_co_logic(&mut self) {
        let t = self.t();
        let Some(co) = &mut self.core.co else { return };
        if co.nets.iter().all(Option::is_none) {
            return;
        }
        co.dev.restart_logic(t, &mut self.co_logic);
        self.trace_co_logic();
    }

    /// The process's level changes in `co_logic` into the trace, by their nets' signals.
    fn trace_co_logic(&mut self) {
        let Some(co) = &self.core.co else { return };
        for (net, edge) in self.co_logic.drain(..) {
            let Some(&Some(id)) = co.nets.get(net as usize) else { continue };
            self.store.push_logic(id, edge, &mut self.pending.logic_closed);
            if let Some((_, found)) = self.pending.logic.iter_mut().find(|(of, _)| *of == id) {
                found.push(edge);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(mut s: Solver, mut st: Stepper, ckt: SimCircuit, vplan: ValuePlan, generation: u64, spec: LiveSpec, period: f64) -> LiveSim {
        Self::settle_stepper(&mut s, &mut st, &spec, period);
        let taps = Taps::new(&s, &vplan);
        let net_v: Vec<f64> = taps.map.nodes.iter().map(|&u| s.x[u]).collect();
        let (pins, parts) = (vplan.pins.len(), vplan.part_pins.len());
        let signals = net_v.len() + if spec.trace && spec.trace_pins { pins } else { 0 };
        let t = st.t();
        let core = Core {
            s,
            st,
            logic: Vec::new(),
            steady: false,
            rest_mark: i64::MIN,
            rest_x: Vec::new(),
            next_event: 0,
            lowered_pins: Vec::new(),
            co: None,
            write_t: f64::NAN,
            hits: Vec::new(),
            warm: Warm::new(),
            ckt: Arc::new(ckt),
            rounds: None,
            planner: None,
            edits: Vec::new(),
            replan_due: None,
            first_due: None,
            kind_due: None,
            plan: None,
            plan_cost: Duration::ZERO,
            pads_moved: Vec::new(),
            refused: Vec::new(),
            refusals_done: 0,
            landed: Vec::new(),
            landings_done: 0,
        };
        let mut sim = LiveSim {
            core,
            store: {
                let mut store = TraceStore::new(generation, &vec![false; signals]);
                store.fine = spec.fine.clone();
                store
            },
            spill: LogicSpill::new(),
            pending: Pending::new(signals),
            interval: Interval::new(t, &net_v, pins, parts, spec.grid),
            sub_pins: Vec::new(),
            sub_parts: Vec::new(),
            pin_i: vec![0.0; pins],
            merged: MergeRows::default(),
            interval_out: None,
            spare_rows: MergeRows::default(),
            interval_spare: (Vec::new(), Vec::new()),
            rounds_id: 0,
            part_p: vec![0.0; parts],
            net_v,
            trace_values: Vec::with_capacity(signals),
            plan_nets: Vec::new(),
            logic_rows: Vec::new(),
            co_logic: Vec::new(),
            co_audio: CoAudio::default(),
            heard: Vec::new(),
            playing: spec.playing,
            step_until: None,
            ended: false,
            held: VecDeque::new(),
            timeline: Vec::new(),
            timeline_taken: Vec::new(),
            go_ins: Vec::new(),
            publish_soon: true,
            paced: false,
            started: false,
            taken: 0,
            stepped_with: 0,
            unseen: Vec::new(),
            delivered: 0.0,
            worker_split: None,
            heard_priority: None,
            heard_raised: false,
            plan_error_text: None,
            inputs: None,
            hold_seen: None,
            stats: LiveStats::default(),
            co_step: false,
            step_wall: Duration::ZERO,
            co_wall: Duration::ZERO,
            sink_wall: Duration::ZERO,
            spec,
            period,
            generation,
            taps,
            vplan,
            helpers: None,
            checks: None,
            memory: None,
            parked: None,
            start: Start::Ordinary,
            warm_due: None,
            warm_out: None,
            rounds_asked: false,
            carrying: false,
            carried: None,
            pack_shuffle: None,
            plan_run: None,
            ring: Ring::default(),
            sink_rewind: None,
            refusals: Vec::new(),
            landings: Vec::new(),
            followed: false,
            pads_kept: false,
            co_to: None,
            late: Vec::new(),
            keeping: None,
        };
        sim.sync_step_cap();
        sim
    }

    /// A new stepper's run settings: the rate window, the resolution and the sample grid.
    fn settle_stepper(s: &mut Solver, st: &mut Stepper, spec: &LiveSpec, period: f64) {
        // the charge cap's current scale is the record's, not the session's (ESIM-Q9)
        st.set_rate_window(spec.record);
        st.set_res(s, spec.res);
        st.set_grid(period);
    }

    /// Runs the circuit in rounds of the blocks `planner` cuts it into (S4's partition), planned
    /// on the worker (`first_plan`) and again after each new lowering. With no planner or no plan,
    /// the whole circuit runs on one stepper.
    /// Every block starts from the present state with its history cut (one backward Euler step), as
    /// after an edit. A plan that fails leaves the run on one stepper and says why (here, or in
    /// `LiveStats::plan_error` for one made on a helper). While rounds are off (`set_rounds`) the
    /// planner waits for them.
    pub fn set_planner(&mut self, planner: Option<Box<dyn Planner>>) -> Result<(), SimDiag> {
        self.leave_rounds();
        if self.preparing() && self.parked.is_some() {
            // Replacing the parked planner ends its set-up. When the hold unparks its own planner
            // the slot has already been taken and first_plan sees Planning.
            self.finish_start();
        }
        // Parking the empty slot precedes the host's first planner. Only that planner can start
        // preparation, before any point, even if the run is later rewound to its operating point.
        if matches!(self.start, Start::New) && planner.is_some() {
            self.start = if self.parked.is_some() && self.stats.points == 0 && self.t() == 0.0 && self.spec.heard && self.spec.rate.is_finite() && self.spec.res > 0.0 && self.helpers.is_some() {
                Start::Preparing { t0: self.t(), wall0: self.stats.step_wall, rounds: false }
            } else {
                Start::Ordinary
            };
            self.sync_step_cap();
        }
        // a plan on its way or due was the old planner's
        self.leave_plan_run();
        self.leave_warm();
        (self.core.plan, self.core.replan_due, self.core.first_due, self.core.kind_due) = (None, None, None, None);
        (self.warm_due, self.rounds_asked) = (None, false);
        // what earlier runs of the circuit refused and landed, on the timeline from here; a run made
        // again from its timeline has them there, and both meet them before their first plan
        self.recall_memory();
        if let Some(parked) = &mut self.parked {
            *parked = planner;
            self.core.planner = None;
            self.apply_refusals();
            self.apply_landings();
            self.teach_planners();
            // warm when the first plan would be made, after the set-up if it waits for one
            if self.waits_for_setup() {
                self.warm_due = Some(self.t() + self.spec.replan.lead);
            } else if self.preparing() {
                self.warm_due = Some(self.t() + self.spec.replan.pins);
            } else {
                self.warm_planner();
            }
            return Ok(());
        }
        self.core.planner = planner;
        self.apply_refusals();
        self.apply_landings();
        self.teach_planners();
        self.first_plan()
    }

    /// Where the host's runs remember what they refused and landed (`StateCache::remember`, by
    /// `states::memory_key`): a run refusing a cut or landing a chain writes it there, and a run
    /// of the same circuit, part values and stepping, given a planner, starts with them on its
    /// timeline (`recall_memory`). They only seed its planner: its plans are new, judged by the
    /// shadow rounds and the chain shadow from their first round as any. None: nothing kept.
    /// Kept across a `Replace`.
    pub fn set_memory(&mut self, memory: Option<StateCache>) {
        self.memory = memory;
    }

    /// The key the run's refusals and landings are remembered by (`states::memory_key`): its
    /// circuit with its effective part values, its round's length, its step bound (the spec's, not
    /// the pace's: `step_cap` moves with slow motion) and its time resolution. O(the circuit's
    /// size).
    fn memory_key(&self) -> u64 {
        crate::analysis::states::memory_key(&self.core.ckt, &self.core.s, &(self.round_len(), self.spec.max_step.min(self.period), self.spec.res))
    }

    /// What earlier runs of the circuit refused and landed (`set_memory`), not yet the run's, on
    /// its timeline from now, met where the timeline's are (`apply_refusals`, `apply_landings`). A
    /// run made again from its timeline (`follow`) has them there already and recalls nothing.
    /// O(the circuit's size + what is remembered).
    fn recall_memory(&mut self) {
        let Some(memory) = self.memory.as_ref().filter(|_| !self.followed) else { return };
        let (refused, landed) = memory.recall(self.memory_key());
        let t = self.t();
        let on = |n: &String, refusals: &[(f64, Vec<String>)]| refusals.iter().any(|r| r.1.contains(n));
        let refused: Vec<String> = refused.into_iter().filter(|n| !on(n, &self.refusals)).collect();
        if !refused.is_empty() {
            let at = self.refusals.partition_point(|r| r.0 <= t);
            self.refusals.insert(at, (t, refused));
        }
        for land in landed {
            if !self.landings.iter().any(|l| l.1 == land) {
                let at = self.landings.partition_point(|l| l.0 <= t);
                self.landings.insert(at, (t, land));
            }
        }
    }

    /// The run's refusals and landings so far into the host's memory (`set_memory`): a run made
    /// again from its timeline writes nothing. O(the circuit's size + its refusals and landings).
    fn remember(&self) {
        if let Some(memory) = self.memory.as_ref().filter(|_| !self.followed) {
            memory.remember(self.memory_key(), &self.core.refused, &self.core.landed);
        }
    }

    /// A plan on its way at a refusal or a landing was made without it: it would go in reading
    /// late where the run now refuses (and be declined) or splitting a chain the run landed. It is
    /// dropped (`recall_planner`) and made again from here, with the run's refusals and landings,
    /// through edits if it was (`PlanInputs::through`). O(1).
    fn remake_plan_on_way(&mut self) {
        let Some(through) = self.core.plan.as_ref().map(|p| p.through) else { return };
        self.recall_planner();
        if through {
            let t = self.t();
            self.core.kind_due = Some(self.core.kind_due.map_or(t, |d| d.min(t)));
        } else {
            self.replan_after(0.0);
        }
    }

    /// Whether the run's first plan waits for its attached process to move a pad from its lowered
    /// mode's kind.
    fn waits_for_setup(&self) -> bool {
        self.helpers.is_some() && self.spec.res > 0.0 && self.core.co.is_some() && !self.core.lowered_pins.is_empty() && self.core.pads_moved.is_empty()
    }

    /// Whether a heard run is stepping its set-up or waiting for its first plan before playback.
    pub fn preparing(&self) -> bool {
        matches!(self.start, Start::Preparing { .. } | Start::Planning)
    }

    /// The first plan is in, or the run starts whole. Both the publication and the sinks hear
    /// this once, before the next step at the commanded pace.
    fn finish_start(&mut self) {
        self.carrying = false;
        self.start = Start::Done { notify: true };
        (self.started, self.publish_soon) = (true, true);
        self.sync_step_cap();
    }

    fn tell_start(&mut self, sink: &mut dyn Sink) -> bool {
        if let Start::Done { notify: true } = self.start {
            self.start = Start::Done { notify: false };
            sink.started();
            return true;
        }
        false
    }

    /// A planner's first plan, or its first after a new lowering: made on the worker and in at
    /// once, the run waiting for it; a failure is returned. Rounds are asked of a run that falls
    /// behind real time on one stepper, so stepping whole while a helper made the plan would be
    /// heard as the same gap, longer (drum16 at 48 kHz: x0.3 over a 50 ms lead, 170 ms, against a
    /// plan's 80 ms here).
    /// In Fast with helper cores, a run whose attached process has moved no pad from its lowered
    /// mode's kind yet waits for that first, stepping whole: the plan falls due `Replan::pins` after the
    /// first such write (a firmware's set-up writes its pads together), or a lead from now at the
    /// latest (`Core::first_due`, met at a round start). A plan of the pads as lowered cuts a
    /// circuit about to change: drum16's has 23 blocks and keeps x0.34, its set-up's 33 real time.
    fn first_plan(&mut self) -> Result<(), SimDiag> {
        // Planning already met the deadline, including a lead that elapsed with no pad moved.
        if !matches!(self.start, Start::Planning) && self.waits_for_setup() {
            self.core.first_due = Some(self.t() + self.spec.replan.lead);
            return Ok(());
        }
        self.replan()
    }

    /// Rounds on or off from this step boundary (`LiveCmd::SetRounds`): off steps the whole
    /// circuit on one stepper and keeps the planner aside (a host runs Exact whole this way), on
    /// plans again with it. Rounds asked for while a warm is out come when it finishes. The run
    /// steps whole meanwhile. Another stepping from here: the snapshots before it go.
    pub fn set_rounds(&mut self, on: bool) -> Result<(), SimDiag> {
        if let Start::Preparing { rounds, .. } = &mut self.start {
            *rounds = on;
            return Ok(());
        }
        if on && self.parked.is_none() {
            return Ok(());
        }
        if on && self.warm_out.as_ref().is_some_and(|w| !w.done()) {
            self.rounds_asked = true;
            return Ok(());
        }
        // a warm due stays due while rounds stay off; `set_planner` clears it when they change
        if !on {
            self.rounds_asked = false;
            if matches!(self.start, Start::Planning) {
                self.finish_start();
            }
            if self.parked.is_some() {
                return Ok(());
            }
        }
        self.forget_snapshots();
        if on {
            let planner = self.parked.take().flatten();
            let plan = self.set_planner(planner);
            if matches!(self.start, Start::Planning) {
                self.finish_start();
            }
            plan
        } else {
            let planner = self.core.planner.take();
            self.parked = Some(None);
            self.set_planner(planner)
        }
    }

    /// The host's hook that raises the worker's priority while a listener hears the run as it is
    /// made (a paced run running, a sink `listened`) and drops it back after: `run_live` calls it
    /// on the worker, with true or false, when that changes (a pause or a closed listener drops it
    /// back; a stop ends the worker). None (the default): the worker keeps the priority it was
    /// spawned with.
    pub fn set_heard_priority(&mut self, priority: Option<Box<dyn FnMut(bool) + Send>>) {
        self.heard_priority = priority;
    }

    /// Raises the worker (`heard`) or drops it back, through the host's hook, when that changes.
    fn hear(&mut self, heard: bool) {
        if heard != self.heard_raised {
            self.heard_raised = heard;
            if let Some(priority) = self.heard_priority.as_mut() {
                priority(heard);
            }
        }
    }

    /// The helper cores a run in rounds posts its packs to (the host's task pool), and how many
    /// it may keep busy beside the worker. None runs every block on the worker. Kept across a
    /// `Replace`. Give helpers only to a run on its own worker (`run_live`): the worker waits
    /// for packs a helper has started.
    pub fn set_helpers(&mut self, helpers: Option<Helpers>) {
        self.helpers = helpers;
    }

    /// Where a run in rounds checks itself (`Checks`): the shadow rounds and the chain trees, as
    /// background jobs below its packs, as many at once as `helpers` the packs leave free
    /// (`beside_packs`: they share the packs' pool, as the host's do); the rest owed and handed
    /// over as room comes (`Rounds::checks_post`): checking never holds the run up. A plan that is
    /// slow to make is made there too (`post_plan`), and the pyramids of the trace's closed chunks
    /// (`post_pyramids`). None runs them on the worker (a run with no pool). Kept across a
    /// `Replace`.
    pub fn set_checks(&mut self, helpers: Option<Helpers>, beside_packs: bool) {
        self.checks = helpers.map(|h| Checks::new(h, beside_packs));
        self.store.defer_pyramids(self.checks.is_some());
        if let Some(r) = &mut self.core.rounds {
            r.checks = self.checks.clone();
        }
    }

    /// The pyramids of the trace's chunks closed or folded since the last call, built by one job on
    /// the checks' pool (`set_checks`: background work, off the sound's cores): the worker builds
    /// none, and a view reads a chunk's values until its pyramid is home. Nothing is replaced, so
    /// nothing is dropped here; the job drops its list. O(chunks) to post.
    fn post_pyramids(&mut self) {
        let Some(checks) = &self.checks else { return };
        let chunks = self.store.take_unbuilt();
        if !chunks.is_empty() {
            (checks.helpers.post)(Box::new(move || {
                for c in &chunks {
                    c.build();
                }
            }));
        }
    }

    /// Under a host's budget, the checks go ahead of the run's plan work there, as many at once as
    /// it can start (`CheckPost`); none: on `set_checks`'s helpers, as many as the packs leave
    /// free. After `set_checks`; kept across a `Replace`.
    pub fn set_checks_ahead(&mut self, ahead: Option<CheckPost>) {
        if let Some(c) = &mut self.checks {
            c.ahead = ahead;
        }
        if let Some(r) = &mut self.core.rounds {
            r.checks = self.checks.clone();
        }
    }

    /// Packs a stage's blocks at random, seeded, instead of by measured cost: a run's values are
    /// the same however it packs (the determinism proof).
    pub fn set_pack_shuffle(&mut self, seed: Option<u64>) {
        self.pack_shuffle = seed;
        if let Some(r) = &mut self.core.rounds {
            r.shuffle = seed;
        }
    }

    /// In rounds: each block's work since its rounds began (a kept plan keeps them), with the
    /// rounds run and the round's length, seconds.
    pub fn block_work(&self) -> Option<(u64, f64, Vec<crate::rounds::BlockWork>)> {
        self.core.rounds.as_ref().map(|r| (r.n, r.round, r.work()))
    }

    /// In rounds: the block that solves the net or branch named `name` (its place in
    /// `block_work`); None on one stepper or for no such unknown. O(the plan's unknowns).
    pub fn block_of(&self, name: &str) -> Option<usize> {
        let u = self.core.s.unknown(name)?;
        self.core.rounds.as_ref()?.block_of(u)
    }

    /// The blocks the run steps: 1 unless in rounds.
    pub fn blocks(&self) -> usize {
        self.core.rounds.as_ref().map_or(1, |r| r.len())
    }

    /// The planner's plan from the present state, installed; nothing without a planner or when it
    /// keeps one stepper. Called with the run on one stepper.
    fn replan(&mut self) -> Result<(), SimDiag> {
        // a carried start: the cut kept for the circuit, built and proved at this state, goes in,
        // and the key's own plan is made behind it (`start_behind`); refused, the start plans here
        if std::mem::take(&mut self.carrying) {
            let (ckt, s, t) = (&self.core.ckt, &self.core.s, self.core.st.t());
            let from = self.carried.as_ref().filter(|_| self.followed);
            match self.core.planner.as_mut().and_then(|p| p.carried(ckt, s, t, from)) {
                Some(Ok(c)) => {
                    self.stats.starts_carried += 1;
                    self.carried = Some(c.carry);
                    // the chains the carried cut lands by name, on the timeline from here as a
                    // memory's are (`recall_memory`), so its planner and the plan behind it land
                    // them too and a run made again meets them at the same place
                    for land in c.landed {
                        if !self.landings.iter().any(|l| l.1 == land) {
                            let at = self.landings.partition_point(|l| l.0 <= t);
                            self.landings.insert(at, (t, land));
                        }
                    }
                    self.apply_landings();
                    self.install(c.plan, c.solvers, Vec::new(), None)?;
                    self.start_behind();
                    return Ok(());
                }
                Some(Err(r)) => {
                    self.stats.carries_refused += 1;
                    self.stats.carry_refusal = Some(r.why);
                    self.carried = Some(r.carry);
                }
                None => {}
            }
        }
        let Some(planner) = &mut self.core.planner else { return Ok(()) };
        let start = Instant::now();
        let got = planner.plan(&self.core.ckt, &self.core.s, self.core.st.t());
        self.core.plan_cost = start.elapsed();
        self.stats.plans += 1;
        self.stats.last_plan = self.core.st.t();
        let Some((plan, solvers)) = got? else { return Ok(()) };
        self.install(plan, solvers, Vec::new(), None)
    }

    /// The planner, at hand or kept aside, makes ahead what its plan of the board as it is now
    /// pays first (`Planner::warm`, off this thread): when it is kept aside for rounds to come.
    /// A run waiting for its set-up (`waits_for_setup`) warms at `warm_due`.
    fn warm_planner(&mut self) {
        if self.waits_for_setup() {
            return;
        }
        self.warm_now();
    }

    /// Posts the warm for the present state and keeps its handle for `set_rounds`. A heard start
    /// that can begin on a carried cut (`Planner::can_carry`, with the checkers' helpers to make its
    /// own plan behind it) posts none: its first plan is the carried one (`replan`).
    fn warm_now(&mut self) {
        self.leave_warm();
        if !self.pads_kept {
            self.pads_kept = true;
            self.keep_start_pads();
        }
        let planner = self.core.planner.as_ref().or(self.parked.as_ref().and_then(Option::as_ref));
        // a run made again carries where its timeline says, from the carry there
        let carry = if self.followed { self.carried.is_some() } else { planner.is_some_and(|p| p.can_carry(&self.core.ckt, &self.core.s)) };
        if self.preparing() && self.checks.is_some() && carry {
            self.carrying = true;
            return;
        }
        self.warm_out = planner.and_then(|p| p.warm(&self.core.ckt, &self.core.s, self.t()));
    }

    /// The pads' modes into the host's memory as the run's start warms in them (`set_memory`,
    /// `StateCache::keep_start_pads`): what its process's set-up left them in. A run made again from
    /// its timeline writes nothing. O(instances).
    fn keep_start_pads(&self) {
        let Some(memory) = self.memory.as_ref().filter(|_| !self.followed) else { return };
        let s = &self.core.s;
        memory.keep_start_pads(s.banks.iter().flat_map(|b| (0..b.len()).filter_map(move |i| b.pin_mode(i).map(|m| (b.inst_name(i).to_string(), m)))).collect());
    }

    /// Lets go of the warm, counting one that is still out.
    fn leave_warm(&mut self) {
        if self.warm_out.as_ref().is_some_and(|w| !w.done()) {
            self.stats.warms_stopped += 1;
        }
        self.warm_out = None;
    }

    /// A held start stays at the point the warm was posted. Once it is back, make the one planning
    /// call there: a cut, one stepper by choice and a failure all release the clock at this point.
    fn holds_for_plan(&mut self) -> bool {
        if !matches!(self.start, Start::Planning) {
            return false;
        }
        if self.warm_out.as_ref().is_some_and(|w| !w.done()) {
            return true;
        }
        self.take_asked_rounds();
        false
    }

    /// The parked planner's warm has returned: take up the request at the present step boundary.
    fn take_asked_rounds(&mut self) {
        self.rounds_asked = false;
        self.warm_out = None;
        if let Err(d) = self.set_rounds(true) {
            self.plan_failed(d);
        }
    }

    /// A plan's blocks go in at the run's present time (the run on one stepper), with the edits;
    /// `keep` (by block, possibly short) holds the running blocks that go on as the plan's, and
    /// `carry` what their rounds hand on (`Rounds::new`).
    fn install(&mut self, plan: crate::rounds::RoundPlan, mut solvers: Vec<Solver>, mut keep: Vec<Option<Run>>, carry: Option<(Carry, &[Option<usize>])>) -> Result<(), SimDiag> {
        crate::rounds::check(&plan, &solvers, &self.core.s)?;
        let ctl_kept = keep.first().is_some_and(Option::is_some);
        for (k, s) in solvers.iter_mut().enumerate() {
            for (device, key, value) in &self.core.edits {
                plan.set_param(k, s, device, key, *value);
            }
        }
        let t = self.core.st.t();
        // the whole state the values publish is the run's here until the rounds deliver
        self.delivered = t;
        let pins = self.vplan.pins.len();
        let reltol = self.core.s.opts.reltol;
        let runs = solvers.into_iter().zip(&plan.blocks).enumerate().map(|(k, (mut s, b))| {
            if let Some(mut old) = keep.get_mut(k).and_then(Option::take) {
                old.s.opts.reltol = reltol;
                let taps = Taps::new(&old.s, &self.vplan);
                return Run::kept(old, b.own.clone(), taps, pins);
            }
            // the run's tolerance now, for a plan cut before a switch of mode
            s.opts.reltol = reltol;
            let mut st = Stepper::resume(&mut s, t, self.spec.max_step);
            Self::settle_stepper(&mut s, &mut st, &self.spec, self.period);
            let taps = Taps::new(&s, &self.vplan);
            Run::new(s, st, b.own.clone(), taps, pins)
        }).collect();
        let pipeline = self.spec.pipeline && self.spec.res > 0.0;
        let mut rounds = Rounds::new(&plan, runs, f64::from(self.spec.round.max(1)) * self.period, pipeline, carry)?;
        if let Some(node) = rounds.reads_late_at(&self.core.refused) {
            return Err(SimDiag::setup(format!("round plan: it reads a value late at {node}, which the run refused (a value read late there left what the circuit can hold)")));
        }
        let born: Vec<String> = self.core.landed.iter().map(|land| land.born.clone()).collect();
        if rounds.chains_named(&born) {
            return Err(SimDiag::setup(format!("round plan: it splits a jump born at one of {}, whose chains the run landed (the chain shadow held a split of one past its bar)", born.join(", "))));
        }
        // a run reading late goes back where a value read late leaves what the circuit holds: one
        // whose attached process cannot be taken back reads nothing late, and needs no going back
        let late = rounds.late_nodes();
        if !late.is_empty() && !self.goes_back() {
            self.refuse_nodes(&late);
            return Err(SimDiag::setup(format!("round plan: it reads values late at {}, and the attached process cannot be taken back (`CoSim::fork`), as a run reading late must be (the growth rule): the run cuts there no more", late.join(", "))));
        }
        if !late.is_empty() {
            rounds.hold = HOLD * self.core.ckt.source_peak().max(1.0);
        }
        rounds.shadow_ctl(self.core.co.is_none());
        self.rounds_id = self.rounds_id.checked_add(1).expect("rounds identities exhausted");
        rounds.id = self.rounds_id;
        rounds.shuffle = self.pack_shuffle;
        rounds.checks = self.checks.clone();
        // its first round's checks are bounded as if every helper had a pack
        rounds.packs_last = self.helpers.as_ref().map_or(0, |h| h.count);
        rounds.edges = self.spec.edge_restarts && self.spec.res > 0.0;
        rounds.hub_screen = self.spec.hub_screen;
        self.core.rounds = Some(Box::new(rounds));
        if ctl_kept {
            // the control block ran on with the attached process: only the new blocks take the cap
            self.sync_step_cap();
            self.publish_soon = true;
        } else {
            self.after_handover();
        }
        Ok(())
    }

    /// Back to one stepper, from the blocks' state; the rounds left, every block in its place.
    fn leave_rounds(&mut self) -> Option<Box<Rounds>> {
        self.leave_rounds_keeping(false)
    }

    /// `leave_rounds`; with `ctl_kept` the control block runs on in the plan going in (`go_in`),
    /// so the attached process stays with it: no handover.
    fn leave_rounds_keeping(&mut self, ctl_kept: bool) -> Option<Box<Rounds>> {
        self.settle_rounds();
        let mut r = self.core.rounds.take()?;
        r.owed_lost(&mut self.stats.round_split);
        // a snapshot on its way in them goes nowhere: the run takes one whole when due
        self.keeping = None;
        // every stage has passed a plan's marker now; one a failed catch-up left short goes nowhere
        let marked = r.take_marked();
        let t = r.ctl().st.t();
        r.compose(&mut self.core.s);
        self.core.st = Stepper::resume(&mut self.core.s, t, self.spec.max_step);
        Self::settle_stepper(&mut self.core.s, &mut self.core.st, &self.spec, self.period);
        if !ctl_kept {
            self.after_handover();
        }
        if self.core.plan.as_ref().is_some_and(|p| p.whole.is_none()) {
            match marked {
                Some(whole) => {
                    self.core.plan.as_mut().expect("a plan on its way").whole = Some(whole);
                    self.post_plan();
                }
                None => {
                    self.recall_planner();
                    self.replan_after(0.0);
                }
            }
        }
        Some(r)
    }

    /// The control solver changed hands: the attached process's pins resolve in it and its
    /// watches register there; warm starts begin anew; the step cap reaches every stepper.
    fn after_handover(&mut self) {
        self.core.s.clear_watches();
        if let Some(r) = &mut self.core.rounds {
            r.ctl_mut().s.clear_watches();
        }
        self.core.warm = Warm { on: self.core.warm.on, ..Warm::new() };
        let ctl = match &self.core.rounds {
            Some(r) => &r.ctl().s,
            None => &self.core.s,
        };
        if let Some(co) = &mut self.core.co {
            co.pins = resolve_pins(ctl, &co.table);
            co.watch_gen = None;
        }
        self.sync_watches();
        self.sync_step_cap();
        self.publish_soon = true;
    }

    /// The solver the attached process reads and writes: block 0's in rounds.
    fn ctl_s(&self) -> &Solver {
        match &self.core.rounds {
            Some(r) => &r.ctl().s,
            None => &self.core.s,
        }
    }

    /// Sets a device's parameter in the whole circuit and, in rounds, in every block that has the
    /// device or its copy across a cut (marked to restart). True when it took.
    fn set_param_all(&mut self, device: &str, key: &str, value: f64) -> bool {
        let whole = self.core.s.set_param(device, key, value);
        if whole {
            match self.core.edits.iter_mut().find(|(d, k, _)| d == device && k == key) {
                Some(e) => e.2 = value,
                None => self.core.edits.push((device.into(), key.into(), value)),
            }
        }
        match &mut self.core.rounds {
            Some(r) => r.set_param(device, key, value),
            None => whole,
        }
    }

    /// The history cut after an edit: the whole stepper's, or in rounds each edited block's.
    fn restart_edited(&mut self) {
        match &mut self.core.rounds {
            None => self.core.st.restart(&self.core.s),
            Some(r) => {
                for b in r.runs_mut() {
                    if std::mem::take(&mut b.edited) {
                        b.st.restart(&b.s);
                    }
                }
            }
        }
    }

    /// Attaches a co-simulated process (replacing any attached one; None detaches). Its pins
    /// resolve by name in the running circuit; it first runs at its `next()`, or at once when that
    /// is now or earlier. A process that replaces one keeps that one's non-volatile memory as
    /// committed now (`CoSim::nonvolatile`), as a chip flashed with a new image keeps its EEPROM.
    pub fn attach(&mut self, mut device: Option<CoSimDevice>) {
        self.settle_rounds();
        let t = self.t();
        let kept = self.core.co.as_mut().and_then(|co| co.dev.nonvolatile(t));
        if let (Some(bytes), Some(d)) = (&kept, device.as_mut()) {
            d.dev.set_nonvolatile(bytes);
        }
        // another process: the run before it is another recording
        self.forget_snapshots();
        ctl_of(&mut self.core.rounds, &mut self.core.s, &mut self.core.st).0.clear_watches();
        self.core.hits.clear();
        // a new process is not armed: a step of the old one ends here
        self.co_step = false;
        self.core.co = match device {
            Some(d) => {
                let nets = self.co_nets(d.dev.as_ref());
                Some(Co { pins: resolve_pins(self.ctl_s(), &d.pins), table: d.pins, dev: Dev::new(d.dev), watch_gen: None, nets })
            }
            None => None,
        };
        self.logic_rows = self.co_logic_rows();
        self.sync_watches();
        self.rounds_with_co();
    }

    /// The rounds judged with the process attached now, as `install` judges a plan: block 0 of a
    /// run with a process steps with it and its shadow rounds are not taken (`shadow_ctl`: run
    /// again without the process, its pins' edges are missing, a miss of a whole source), and a
    /// process that cannot be taken back reads nothing late (it leaves the rounds and plans anew).
    /// Where the process changes after a plan went in: an attach, and a replacement, whose first
    /// plan goes in with the process out. O(the plan's couplings).
    fn rounds_with_co(&mut self) {
        let alone = self.core.co.is_none();
        if let Some(r) = self.core.rounds.as_mut() {
            r.shadow_ctl(alone);
        }
        // a process that cannot be taken back: the run reads nothing late from here (`install`)
        if let Some(late) = self.core.rounds.as_ref().map(|r| r.late_nodes()).filter(|l| !l.is_empty() && !self.goes_back()) {
            self.refuse_nodes(&late);
            self.leave_rounds();
            self.replan_after(0.0);
        }
    }

    pub fn t(&self) -> f64 {
        match &self.core.rounds {
            Some(r) => r.ctl().st.t(),
            None => self.core.st.t(),
        }
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn stats(&self) -> &LiveStats {
        &self.stats
    }
    /// The stepper's history restarts so far, by cause.
    pub fn restarts(&self) -> Restarts {
        match &self.core.rounds {
            Some(r) => r.restarts(),
            None => self.core.st.restarts,
        }
    }
    /// The time resolution from the next step on (`LiveSpec::res`); kept across a `Replace`.
    /// A switch between Fast and Exact also switches the tolerance (`LiveSpec::reltol_at`). When
    /// it changes a plan's pipeline or edge restarts, the run steps whole until a plan is
    /// installed for the new mode.
    pub fn set_res(&mut self, res: f64) {
        self.settle_rounds();
        // another resolution steps another run from here: the snapshots before it are of the old
        self.forget_snapshots();
        let res = if res > 0.0 { res } else { 0.0 };
        self.spec.res = res;
        if res == 0.0 && self.preparing() {
            self.rounds_asked = false;
            self.finish_start();
        }
        // Exact and Fast each step at their own tolerance (`LiveSpec::reltol_at`): the run's
        // steppers, and the circuit later plans cut, from the next step on
        let reltol = self.spec.reltol_at(res);
        if self.core.ckt.opts.reltol != reltol {
            Arc::make_mut(&mut self.core.ckt).opts.reltol = reltol;
        }
        self.core.s.opts.reltol = reltol;
        self.core.st.set_res(&mut self.core.s, res);
        if let Some(r) = &mut self.core.rounds {
            for b in r.runs_mut() {
                b.s.opts.reltol = reltol;
                b.st.set_res(&mut b.s, res);
            }
            if r.pipeline != (self.spec.pipeline && res > 0.0) || r.edges != (self.spec.edge_restarts && res > 0.0) {
                self.leave_rounds();
                if self.core.plan.is_none() {
                    self.core.replan_due = Some(self.t());
                }
            }
        }
        self.sync_step_cap();
    }
    /// The rate the run steps at, Hz: `LiveSpec::sample_rate`, or the one it fell to with none
    /// chosen. For the run settings to show.
    pub fn sample_rate(&self) -> f64 {
        1.0 / self.period
    }
    /// The spec the run steps by (its pace, resolution and rate as set since).
    pub fn spec(&self) -> &LiveSpec {
        &self.spec
    }
    /// Where its work goes: its helpers' count (`set_helpers`), and its checks' with whether
    /// they share the packs' pool (`set_checks`); None for either not set.
    pub fn pool_roles(&self) -> (Option<usize>, Option<(usize, bool)>) {
        (self.helpers.as_ref().map(|h| h.count), self.checks.as_ref().map(|c| (c.helpers.count, c.beside_packs)))
    }
    /// The solver's profile, when the circuit asked for one (`Options::profile`).
    pub fn profile_mut(&mut self) -> Option<&mut crate::newton::Profile> {
        self.core.s.profile.as_deref_mut()
    }

    pub fn solver(&self) -> &Solver {
        &self.core.s
    }

    /// The circuit the run simulates, as it set it up (`new_tuned`: its tolerance and noise).
    pub fn circuit(&self) -> &SimCircuit {
        &self.core.ckt
    }
    /// In-worker GPIO write between steps for a co-simulated MCU, using a handle from `find_device`.
    /// Writes at the current time; false for an invalid handle or mode.
    pub fn pin_write(&mut self, dev: (usize, usize), mode: PinMode) -> bool {
        let t = self.t();
        let (s, st) = ctl_of(&mut self.core.rounds, &mut self.core.s, &mut self.core.st);
        match s.pin_write(dev, t, mode) {
            Some(Retuned::Now) => {
                self.restart_written(dev, mode);
                self.publish_soon = true;
            }
            Some(Retuned::At(_)) => st.breaks_changed(s),
            None => return false,
        }
        self.written(dev, mode);
        true
    }

    /// A user's edit makes a re-plan due (`Replan::edit`) in a run with a planner.
    fn edit_replans(&mut self) {
        if self.core.planner.is_some() || self.core.plan.is_some() {
            self.replan_after(self.spec.replan.edit);
        }
    }

    /// An edit of a device's parameter can move the run off the state its plan's round map proved
    /// the loops at: a key closing a row pad onto a column pad makes the current its block returns
    /// a round late grow without bound. In rounds a plan on its way from before the edit goes
    /// nowhere (the cut in force runs on). On one stepper it goes in all the same, with the edit,
    /// which is proved there as one made in rounds (`edited_before`): keys pressed faster than a
    /// plan's lead would otherwise drop every plan, and keep the run whole. So does one in rounds
    /// that replaces a plan tested against another circuit (`PlanInputs::through`): keys pressed
    /// faster would otherwise keep that plan in force through the burst. An edit that reached a
    /// device on a loop of the map (in a part of its block that joins what the block reads across
    /// the cut to what it exports, `Rounds::reprove_due`) is proved at its round's end
    /// (`reprove_edits`), or, when the plan keeps no proof, leaves rounds now. Any other edit stays
    /// in the blocks. O(edits since the plan's snapshot) where the plan goes in through them.
    fn guard_edit(&mut self, device: &str) {
        if self.core.rounds.is_none() || self.core.plan.as_ref().is_some_and(|p| p.through) {
            if let Some(p) = self.core.plan.as_mut().filter(|p| !p.edited.iter().any(|d| d == device)) {
                p.edited.push(device.to_string());
            }
        } else {
            self.recall_planner();
        }
        if self.core.rounds.as_ref().is_some_and(|r| r.reprove_due() && !r.proves()) {
            self.stats.edit_leaves += 1;
            self.leave_rounds();
        }
    }

    /// The edits that reached a loop of the plan's round map, proved at the end of their round:
    /// the edited blocks have taken their restart step, and nothing they exported has come back
    /// to them yet (what they read a round late is from before the edit), so no loop has grown.
    /// The plan's proof with those blocks linearised where they stand (`Rounds::reprove`), its
    /// points shared with the helpers: when it holds, the run stays in rounds; when it fails, the
    /// run leaves them, and the re-plan after the edit (`edit_replans`) brings them back. From a
    /// fixed start the run's state only guards, on the stability bar (`RoundProof`): an accuracy
    /// figure over its bar there is counted (`LiveStats::reproofs_over`), and the re-plan judges it. The
    /// worker waits for it, as for a stage. The stages behind block 0 (`LiveSpec::pipeline`)
    /// finish the edit's round first, so every edited block stands at its end; their wall time
    /// counts in the proof's.
    fn reprove_edits(&mut self) {
        if !self.core.rounds.as_ref().is_some_and(|r| r.reprove_due()) {
            return;
        }
        let start = Instant::now();
        self.settle_rounds();
        // a catch-up that failed is reported at the next round's close, and a breach taken there
        let Some(r) = self.core.rounds.as_mut().filter(|r| r.failed.is_none() && r.breach.is_none()) else { return };
        let proved = r.reprove(self.helpers.as_ref());
        let wall = start.elapsed();
        self.stats.reproofs += 1;
        self.stats.reproof_wall += wall;
        self.stats.reproof_max = self.stats.reproof_max.max(wall);
        if proved.as_ref().is_ok_and(|m| m.present.0 > 1.0) {
            self.stats.reproofs_over += 1;
        }
        let failed = proved.is_err();
        self.stats.last_reproof = Some(proved);
        if failed {
            self.stats.edit_leaves += 1;
            self.leave_rounds();
        }
    }

    /// A pin was written: in rounds, the other blocks land on this instant of what block 0
    /// exports, as on an edge. A pad's first change from its lowered mode's kind makes a re-plan
    /// due a `Replan::pins` later, at the first such change's time (`Core::kind_due`): the plan
    /// in force was tested against other pads, so edits never push it on, and its plan goes in
    /// through them. One within the kind (a driven pad at its other level) makes the plan in
    /// force again, its states keyed by kind (`states::key`), so it makes none.
    fn written(&mut self, dev: (usize, usize), mode: PinMode) {
        if let Some(r) = &mut self.core.rounds {
            r.ctl_mut().kink = true;
        }
        // a planner kept aside (rounds off) notes the pads too: once rounds come, its first plan
        // sees them moved and is made at once
        let aside = matches!(self.parked, Some(Some(_)));
        if self.core.planner.is_none() && self.core.plan.is_none() && !aside {
            return;
        }
        let name = self.ctl_s().banks[dev.0].inst_name(dev.1).to_string();
        if self.core.pads_moved.contains(&name) || self.core.lowered_pins.iter().any(|(n, m)| *n == name && crate::analysis::states::same_kind(*m, mode)) {
            return;
        }
        self.core.pads_moved.push(name);
        if aside {
            if let Some(due) = self.warm_due {
                self.warm_due = Some(due.min(self.t() + self.spec.replan.pins));
            }
            return;
        }
        // a first plan waiting for the set-up falls due a `pins` after its first write
        if let Some(due) = self.core.first_due {
            self.core.first_due = Some(due.min(self.t() + self.spec.replan.pins));
            return;
        }
        let due = self.t() + self.spec.replan.pins;
        self.core.kind_due = Some(self.core.kind_due.map_or(due, |d| d.min(due)));
    }

    /// A re-plan falls due `wait` simulated seconds from now. In rounds, later if one already is: a
    /// burst of edits gives one plan, after its last (the run goes on in rounds meanwhile). On one
    /// stepper, the slow case, at the first due time: later ones merge into it, so no stream of
    /// edits keeps the run whole. A function of the run's inputs: a run made again plans alike.
    fn replan_after(&mut self, wait: f64) {
        let due = self.t() + wait;
        let keep = if self.core.rounds.is_some() { f64::max } else { f64::min };
        self.core.replan_due = Some(self.core.replan_due.map_or(due, |d| keep(d, due)));
    }

    /// Where a round starts (between steps on one stepper): a plan goes in where its time has come
    /// (`plan_goes_in`), or a due re-plan starts. One no core has started yet the worker makes
    /// now, and one a helper is making it waits for (`LiveStats::plan_waits`): a heard run takes
    /// a re-plan when it is back, and a replay puts it in where its inputs say, so a take made
    /// again is the same run. The round it went in at is written (`go_ins`). A first plan that
    /// waited for the pads (`Core::first_due`) is made and goes in now: the run's start ends
    /// there, and true says so (`Sink::started`).
    fn at_round_start(&mut self) -> bool {
        // a shadow round past its bar and a chain convicted are written on the timeline before a
        // plan can go in over their rounds
        self.shadow_taken();
        self.chain_taken();
        let started = self.round_start();
        // a refusal, then a landing, takes effect after what the round start did, wherever the run
        // went back from
        self.apply_refusals();
        self.apply_landings();
        started
    }

    /// `at_round_start` but for the refusals.
    fn round_start(&mut self) -> bool {
        // a plan whose snapshot the stages have passed is posted first: it may go in now
        self.take_marked_plan();
        let t = self.t();
        if self.core.first_due.is_some_and(|d| t >= d) {
            self.core.first_due = None;
            if let Err(d) = self.replan() {
                self.plan_failed(d);
            }
            // A run that started whole after measuring its set-up has already told its sinks.
            if matches!(self.start, Start::Done { .. }) {
                return false;
            }
            (self.started, self.publish_soon) = (true, true);
            return true;
        }
        if let Some(p) = &self.core.plan {
            let from = p.t;
            // a plan that keeps the cut goes in with the stages behind (`go_in_reshapes`)
            let behind = self.core.rounds.as_ref().is_some_and(|r| r.behind()) && self.go_in_reshapes();
            if !self.plan_goes_in(t, behind) {
                return false;
            }
            let run = self.plan_run.as_mut().expect("a plan on its way has its job");
            let late = run.due_round.filter(|&r| t > r).map(|r| t - r);
            if let Some(job) = run.take() {
                job.run(false);
            }
            let got = match run.back.take() {
                Some(back) => back,
                None => match run.rx.try_recv() {
                    Err(TryRecvError::Empty) => {
                        let wait = Instant::now();
                        let got = run.rx.recv().ok();
                        let waited = wait.elapsed();
                        self.stats.plan_waits += 1;
                        self.stats.plan_wait_wall += waited;
                        self.stats.plan_wait_max = self.stats.plan_wait_max.max(waited);
                        got
                    }
                    got => got.ok(),
                },
            };
            match got {
                Some(done) => {
                    if let Some(late) = late {
                        self.stats.plans_late += 1;
                        self.stats.plan_late_max = self.stats.plan_late_max.max(late);
                    }
                    self.write_go_in(from, t);
                    self.plan_run = None;
                    let (retunes, edited) = self.core.plan.take().map(|p| (p.retunes, p.edited)).unwrap_or_default();
                    let start = Instant::now();
                    self.go_in(done, &retunes, &edited);
                    let wall = start.elapsed();
                    self.stats.install_wall += wall;
                    self.stats.install_max = self.stats.install_max.max(wall);
                }
                None => {
                    self.recall_planner();
                    self.plan_failed(SimDiag::setup("round plan: the planner was lost on a helper core".into()));
                }
            }
            return false;
        }
        // a pad's change of kind: through edits, a burst's due edit re-plan made by it too
        let edit = self.core.replan_due.is_some_and(|d| t >= d);
        let kind = self.core.kind_due.is_some_and(|d| t >= d);
        if kind || edit {
            if kind {
                self.core.kind_due = None;
            }
            if edit {
                self.core.replan_due = None;
            }
            self.start_plan(kind);
        }
        false
    }

    /// A plan from a snapshot of the run (the whole circuit with the blocks' state), on its way to
    /// go in a lead later (`post_plan`). In rounds the snapshot is the rounds' marker
    /// (`Rounds::mark`): every block at this round start, each later stage's taken as it passes
    /// it, none caught up for it; the job starts at the first round start after the last has
    /// (`take_marked_plan`), at once when none is behind.
    /// `through`: it goes in through edits in rounds too (`PlanInputs::through`).
    fn start_plan(&mut self, through: bool) {
        self.start_plan_as(through, false);
    }

    /// The key's own plan behind a carried start, started as an edit's re-plan is (`start_plan`):
    /// the carried cut in force, so the keep rule holds, made on the checkers' helpers, in at the
    /// first round start where it is back (`plan_goes_in`).
    fn start_behind(&mut self) {
        self.start_plan_as(false, true);
    }

    /// `start_plan`, `behind` a carried start or not.
    fn start_plan_as(&mut self, through: bool, behind: bool) {
        let Some(planner) = self.core.planner.take() else { return };
        let in_force = self.core.rounds.as_ref().map(|r| r.cut().clone());
        let whole = match &mut self.core.rounds {
            Some(r) => {
                r.mark(self.core.s.clone());
                let whole = r.take_marked();
                self.stats.plans_marked += u64::from(whole.is_none());
                whole
            }
            None => Some(self.core.s.clone()),
        };
        self.stats.plans += 1;
        self.core.plan = Some(PlanInputs { t: self.t(), at: self.t() + self.spec.replan.lead, planner, ckt: self.core.ckt.clone(), whole, retunes: Vec::new(), in_force, edited: Vec::new(), through, behind });
        self.post_plan();
    }

    /// The plan on its way takes its snapshot from the rounds' marker once every stage has passed
    /// it, and its job is posted. O(1) until then.
    fn take_marked_plan(&mut self) {
        if !self.core.plan.as_ref().is_some_and(|p| p.whole.is_none()) {
            return;
        }
        let Some(whole) = self.core.rounds.as_mut().and_then(|r| r.take_marked()) else { return };
        self.core.plan.as_mut().expect("a plan on its way").whole = Some(whole);
        self.post_plan();
    }

    /// The job for the plan on its way (`Core::plan`), from a fork of its planner: made now on the
    /// worker or, if the last plan took longer than `Replan::slow`, posted to the checkers' helpers
    /// (`set_checks`: background jobs, below the run's packs). None
    /// while its snapshot is not taken yet (`take_marked_plan` posts it then).
    fn post_plan(&mut self) {
        let Some(p) = &self.core.plan else { return };
        let Some(whole) = &p.whole else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let job = Arc::new(std::sync::Mutex::new(Some(PlanJob {
            planner: p.planner.clone(), ckt: p.ckt.clone(), whole: whole.clone(), t: p.t, in_force: p.in_force.clone(), stop: stop.clone(), tx,
        })));
        // a slow plan is made below the run's packs, never on a pack's helper: on the checkers'
        // (`set_checks`), behind the checks under a host's budget (`set_checks_ahead`). The worker
        // makes a plan that is still wanted when it is due if no helper has started it (`round_start`).
        // The plan behind a carried start is behind work (`rounds::with_behind`), its job and its
        // units alike: the carried plan plays meanwhile, and a host's pool admits it below the work
        // the sound waits for.
        match &self.checks {
            Some(c) if p.behind || self.core.plan_cost.as_secs_f64() > self.spec.replan.slow => {
                let (job, behind) = (job.clone(), p.behind);
                crate::rounds::with_behind(behind, || {
                    (c.helpers.post)(Box::new(move || {
                        let job = job.lock().unwrap_or_else(|e| e.into_inner()).take();
                        if let Some(job) = job {
                            crate::rounds::with_behind(behind, || job.run(true));
                        }
                    }))
                });
            }
            _ => {
                if let Some(job) = job.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    job.run(false);
                }
            }
        }
        self.plan_run = Some(PlanRun { job, stop, rx, back: None, due_round: None });
    }

    /// A plan made from a snapshot goes in: its blocks seeded by name from the run as it is now,
    /// with the sources retuned since the snapshot. A failed plan leaves the run as it was, and so
    /// does one that cuts it as it already runs in rounds (no handover, no history cut).
    fn go_in(&mut self, done: PlanDone, retunes: &[(f64, String, Source)], edited: &[String]) {
        self.core.planner = Some(done.planner);
        // a plan made before a refusal came back with a planner that does not know it
        self.teach_planners();
        self.core.plan_cost = done.cost;
        self.stats.plans_helped += u64::from(done.helped);
        let got = match done.got {
            Ok(got) => got,
            Err(d) => {
                self.plan_failed(d);
                return;
            }
        };
        self.stats.last_plan = self.t();
        // the cut the run already runs: its blocks run on, their histories whole, under the new
        // plan's proof
        if let (Some(r), Some((plan, solvers))) = (&mut self.core.rounds, &got) {
            if r.runs_cut(plan, solvers) {
                r.renew_proof(plan.proof.clone());
                // its holder ports as fitted at the run's state now
                if r.ports_differ(&plan.ports) {
                    self.settle_rounds();
                    if let Some(r) = &mut self.core.rounds {
                        r.renew_ports(&plan.ports);
                    }
                }
                self.stats.plans_kept += 1;
                // the edits it came in through, proved under its proof
                self.edited_before(edited);
                return;
            }
        }
        // a new cut: the blocks it shares with the running one run on, their histories and their
        // couplings' histories whole (A-R3); the others start from the present state with their
        // histories cut. A kept control block keeps the attached process, its watches and warm
        // starts: no handover
        self.settle_rounds();
        let same = match (&self.core.rounds, &got) {
            (Some(r), Some((_, solvers))) => r.same_blocks(solvers),
            _ => Vec::new(),
        };
        let ctl_kept = same.first().is_some_and(Option::is_some);
        let old = self.leave_rounds_keeping(ctl_kept);
        let Some((plan, mut solvers)) = got else { return };
        let (carry, mut olds): (Option<Carry>, Vec<Option<Run>>) = match old {
            Some(r) => {
                let (carry, runs) = r.into_parts();
                (Some(carry), runs.into_iter().map(Some).collect())
            }
            None => (None, Vec::new()),
        };
        let keep: Vec<Option<Run>> = same.iter().map(|j| j.and_then(|j| olds[j].take())).collect();
        for (k, s) in solvers.iter_mut().enumerate() {
            if keep.get(k).is_some_and(Option::is_some) {
                continue;
            }
            crate::rounds::seed_from(s, &self.core.s);
            for (t, device, source) in retunes {
                s.retune(device, *t, source.clone());
            }
        }
        let kept = keep.iter().filter(|k| k.is_some()).count() as u64;
        match self.install(plan, solvers, keep, carry.map(|c| (c, same.as_slice()))) {
            Ok(()) => {
                self.stats.blocks_kept += kept;
                self.edited_before(edited);
            }
            Err(d) => {
                // the run steps whole: the attached process comes over to it
                if ctl_kept {
                    self.after_handover();
                }
                self.plan_failed(d);
            }
        }
    }

    /// The devices edited on one stepper while the plan that just went in was on its way
    /// (`guard_edit`): where one reached a loop of its round map, proved at its first round's end
    /// (`reprove_edits`) as an edit made in rounds; with no proof to prove it by, the run leaves
    /// rounds, and the re-plan after the edit brings them back. O(edited x blocks).
    fn edited_before(&mut self, edited: &[String]) {
        let Some(r) = self.core.rounds.as_mut() else { return };
        for device in edited {
            r.edited_before(device);
        }
        if r.reprove_due() && !r.proves() {
            self.stats.edit_leaves += 1;
            self.leave_rounds();
        }
    }

    /// A plan failed: the run stays whole, and `stats.plan_error` and the publications say why.
    fn plan_failed(&mut self, d: SimDiag) {
        self.plan_error_text = Some(Arc::from(d.message.as_str()));
        self.stats.plan_error = Some(d);
    }

    /// Takes back a queued plan job or stops a helper still making it.
    fn leave_plan_run(&mut self) {
        let Some(run) = self.plan_run.take() else { return };
        if run.take().is_none() && run.back.is_none() && matches!(run.rx.try_recv(), Err(TryRecvError::Empty)) {
            self.stats.plans_stopped += 1;
        }
    }

    /// The planner back from a plan on its way, as it was when the plan started; the plan dropped
    /// (a job no core has started yet is taken back, one a helper is making stops at its next unit).
    fn recall_planner(&mut self) {
        self.leave_plan_run();
        if let Some(p) = self.core.plan.take() {
            // it never goes in: a run made again holds it until the same call
            self.write_go_in(p.t, f64::INFINITY);
            self.core.planner = Some(p.planner);
            if let Some(r) = &mut self.core.rounds {
                r.unmark();
            }
        }
    }

    /// The restart of a pin write that changes its pad now, with a warm start's seed under a time
    /// resolution (`Warm`).
    fn restart_written(&mut self, dev: (usize, usize), mode: PinMode) {
        let keep = self.core.warm.on && self.spec.res > 0.0;
        let (s, st) = ctl_of(&mut self.core.rounds, &mut self.core.s, &mut self.core.st);
        let seed = self.core.warm.enter(dev, mode, st.t(), st.newest_x(), keep);
        st.restart(s);
        if let Some(x) = seed {
            st.seed(x);
        }
    }

    /// Warm starts on (the default) or off. Off, every first step after a pin write starts Newton
    /// from the predictor (what their proof and measurements compare against). Only a time
    /// resolution takes them: Exact never does. Kept across a `Replace`.
    pub fn set_warm_start(&mut self, on: bool) {
        self.core.warm.on = on;
    }

    /// In-worker watch registration between steps for a co-simulated MCU.
    pub fn pin_watch(&mut self, dev: (usize, usize), id: u32, level: f64, edge: crate::circuit::PinEdge) -> bool {
        ctl_of(&mut self.core.rounds, &mut self.core.s, &mut self.core.st).0.pin_watch(dev, id, level, edge)
    }

    /// In-worker removal of all pin watches between steps for a co-simulated MCU.
    pub fn clear_watches(&mut self) {
        ctl_of(&mut self.core.rounds, &mut self.core.s, &mut self.core.st).0.clear_watches();
    }

    /// In-worker drain of pending pin hits between steps for a co-simulated MCU.
    pub fn pin_hits(&mut self, out: &mut Vec<(u32, f64)>) {
        ctl_of(&mut self.core.rounds, &mut self.core.s, &mut self.core.st).0.pin_hits(&mut self.core.hits);
        out.append(&mut self.core.hits);
    }

    /// The producer's store (what the deltas sent so far rebuild).
    pub fn store(&self) -> &TraceStore {
        &self.store
    }
    /// Stepping now: playing, a `Step` not yet reached, or an armed `CoStep`.
    pub fn running(&self) -> bool {
        self.playing || self.step_until.is_some() || self.co_step
    }

    /// An armed `CoStep` runs (unpaced, until the device stops or a `Pause`).
    pub fn stepping(&self) -> bool {
        self.co_step
    }

    /// Whether the bench has settled and the loop waits for a command.
    pub fn steady(&self) -> bool {
        self.core.steady
    }

    fn could_rest(&self) -> bool {
        self.playing && self.step_until.is_none() && !self.co_step
            && self.core.next_event >= self.timeline.len()
            && self.core.co.as_ref().is_none_or(|co| co.dev.next().is_infinite())
    }

    /// Whether no source varies after now. The whole solver holds every source with its edits
    /// (`set_param_all`), so no block need be in its place.
    fn sources_quiet(&self) -> bool {
        self.core.s.banks.iter().all(|bank| !bank.varies_after(self.t()))
    }

    /// Compares the solvers with the last rest mark and saves their current values.
    /// In rounds every block must be in its place.
    fn moved_since_mark(&mut self) -> bool {
        let rest_x = &mut self.core.rest_x;
        if let Some(r) = &self.core.rounds {
            let mut offset = 0;
            let moved = r.runs().any(|b| {
                let start = offset;
                offset += b.s.x.len();
                rest_x.get(start..offset).is_none_or(|x0| b.s.moved_from(x0))
            }) || offset != rest_x.len();
            rest_x.clear();
            for b in r.runs() {
                rest_x.extend_from_slice(&b.s.x);
            }
            moved
        } else {
            let s = &self.core.s;
            let moved = s.moved_from(rest_x);
            rest_x.clear();
            rest_x.extend_from_slice(&s.x);
            moved
        }
    }

    fn check_steady(&mut self) -> bool {
        if !self.could_rest() || !self.sources_quiet() {
            self.core.steady = false;
            return false;
        }
        self.core.steady = match &self.core.rounds {
            Some(r) => r.runs().all(|b| b.st.steady(&b.s)),
            None => self.core.st.steady(&self.core.s),
        };
        self.core.steady
    }

    /// Checks for rest at the first point past each mark in simulated time. Fresh runs therefore
    /// give a take the same length and last frames regardless of wall-clock timing. If the flags
    /// allow rest and no source varies after now, the stages catch up. Solver copies run the
    /// static check only if nothing moved beyond Newton's tolerance since the last mark.
    fn rest_check(&mut self, sink: &mut dyn Sink) -> bool {
        let mark = (self.t() / REST_EVERY).floor() as i64;
        if mark == self.core.rest_mark {
            return false;
        }
        self.core.rest_mark = mark;
        if !self.could_rest() {
            self.core.steady = false;
            return false;
        }
        if !self.sources_quiet() {
            self.core.steady = false;
            return false;
        }
        if self.core.rounds.is_some() {
            self.flush_rounds(sink);
        }
        if self.moved_since_mark() {
            self.core.steady = false;
            return false;
        }
        self.check_steady()
    }

    /// The run's timed value commands in time order, with the time each applied at for those
    /// applied (`LiveCmd::At`).
    pub fn timeline(&self) -> &[(f64, LiveCmd)] {
        &self.timeline
    }

    /// Puts a timed command on the timeline: after every applied one and every pending one at or
    /// before `t`. A command whose time the run already passed takes effect at the next boundary.
    /// A command stamped from the host's view of the run (`viewed`: `LiveCmd::At`) that comes late
    /// moves the next command of the same device and key later by as much, however long after it
    /// that one comes, so the two keep their distance: a press's release, stamped by a host whose
    /// view of the run lags it, still gives the device the press's whole length. A command placed
    /// on the run's own time (an `Edit`, where the listener hears it) takes effect there and ends
    /// a move without taking it; a moved command starts none of its own. O(pending events + late
    /// devices).
    fn schedule(&mut self, t: f64, cmd: LiveCmd, viewed: bool, n: u64) {
        let key = match &cmd {
            LiveCmd::SetParam { device, key, .. } => (device.clone(), key.clone()),
            LiveCmd::Retune { device, .. } => (device.clone(), String::new()),
            LiveCmd::PinMode { device, .. } => (device.clone(), String::new()),
            LiveCmd::Input { key, .. } => (String::new(), key.clone()),
            _ => {
                self.stats.refused += 1;
                return;
            }
        };
        if !t.is_finite() {
            self.stats.refused += 1;
            return;
        }
        let now = self.t();
        let mut t = t;
        let partner = match self.late.iter().position(|(k, ..)| *k == key) {
            Some(i) => {
                let (_, stamped, took) = self.late.swap_remove(i);
                if viewed {
                    t += took - stamped;
                }
                true
            }
            None => false,
        };
        if viewed && !partner && t < now {
            self.late.push((key, t, now));
        }
        // the inputs change from there: the plans started since are another run's
        self.forget_go_ins(t.max(now));
        let at = self.core.next_event + self.timeline[self.core.next_event..].partition_point(|e| e.0 <= t);
        self.timeline.insert(at, (t, cmd));
        self.timeline_taken.insert(at, n);
    }

    /// Applies the timeline's commands due at this step boundary, keeping the boundary's time as
    /// theirs. Called at the top of each step, where the newest point is confirmed and no nested
    /// advance is open: the boundaries are the run's, not the slicing's.
    fn apply_due(&mut self) {
        let now = self.t();
        while self.timeline.get(self.core.next_event).is_some_and(|e| e.0 <= now) {
            let i = self.core.next_event;
            self.core.next_event += 1;
            self.timeline[i].0 = now;
            let cmd = self.timeline[i].1.clone();
            self.note_unseen(&cmd, self.timeline_taken[i]);
            self.run_cmd(cmd);
        }
    }

    /// Applies one command at this step boundary. Returns false for `Stop`.
    pub fn apply(&mut self, cmd: LiveCmd) -> bool {
        self.apply_as(cmd, self.taken)
    }

    /// `apply`, for the command `n` commands after the first `run_live` took: what a publication
    /// counts of the commands is told by it (`claims`), wherever it applies.
    fn apply_as(&mut self, cmd: LiveCmd, n: u64) -> bool {
        self.wake_for(&cmd);
        match cmd {
            LiveCmd::At(t, cmd) => {
                self.schedule(t, *cmd, true, n);
                return true;
            }
            LiveCmd::Edit(events) => {
                self.settle_rounds();
                self.edit(events, n);
                return true;
            }
            _ => {}
        }
        // the inputs change nothing a step computes, and a step reads them
        if self.step_until.is_some() && !matches!(cmd, LiveCmd::Pause | LiveCmd::Stop | LiveCmd::Inputs(_)) {
            self.held.push_back((n, cmd));
            return true;
        }
        // with snapshots kept, a value command goes on the timeline at the boundary it applies at,
        // so a run sent back before it applies it again there
        if self.spec.snapshots.span > 0.0 && matches!(cmd, LiveCmd::SetParam { .. } | LiveCmd::Retune { .. } | LiveCmd::PinMode { .. } | LiveCmd::Input { .. }) {
            let i = self.core.next_event;
            self.timeline.insert(i, (self.t(), cmd.clone()));
            self.timeline_taken.insert(i, n);
            self.core.next_event += 1;
            self.forget_go_ins(self.t());
        }
        self.note_unseen(&cmd, n);
        self.run_cmd(cmd)
    }

    /// A value command applies here, `taken` commands before it, in rounds or on one stepper: a
    /// publication counts it once every value it carries was produced past here (`claims`).
    fn note_unseen(&mut self, cmd: &LiveCmd, taken: u64) {
        if matches!(cmd, LiveCmd::SetParam { .. } | LiveCmd::Retune { .. } | LiveCmd::PinMode { .. } | LiveCmd::Input { .. }) {
            self.unseen.push((self.t(), taken));
        }
    }

    /// A settled run steps again for a command that changes the circuit or its time (a value, a
    /// lowering, a process, a step, Play, a timed event), not for one that changes only what is
    /// published or how (a subscription, a sink, the pace, the record, the detail, the
    /// resolution): the settled point stands, and the wait publishes it again.
    fn wake_for(&mut self, cmd: &LiveCmd) {
        let wakes = !matches!(cmd, LiveCmd::Pause | LiveCmd::Subscribe { .. } | LiveCmd::Sinks | LiveCmd::SetRate(_) | LiveCmd::SetWhole(_) | LiveCmd::SetFine(_) | LiveCmd::SetRing(_)
            | LiveCmd::LogicDetail(_) | LiveCmd::SetRes(_) | LiveCmd::SetRounds(_) | LiveCmd::Inputs(_) | LiveCmd::SetHeard(_) | LiveCmd::Stop);
        if wakes {
            self.core.steady = false;
        }
    }

    /// Whether `cmd` needs the stages running behind block 0 caught up to it first: every command
    /// but a value edit that reaches the control block alone (with the thermal noise source a
    /// resistance edit sets, `thermal_follows`). The later stages see such an edit only through what
    /// block 0 exports from its instant on, so they run on behind it as they were, the same values
    /// on any helpers. A key's press is one: its switch is in block 0.
    fn needs_settle(&self, cmd: &LiveCmd) -> bool {
        let Some(r) = &self.core.rounds else { return true };
        match cmd {
            LiveCmd::SetParam { device, key, .. } => {
                !(r.reaches_ctl_alone(device) && (!matches!(key.as_str(), "r" | "value") || r.reaches_ctl_alone(&format!("{device}#noise"))))
            }
            _ => true,
        }
    }

    fn run_cmd(&mut self, cmd: LiveCmd) -> bool {
        if self.needs_settle(&cmd) {
            self.settle_rounds();
        }
        self.wake_for(&cmd);
        // an edit or a press: the rounds after it run again, every one for a while (`Rounds::shadow_burst`)
        if matches!(cmd, LiveCmd::SetParam { .. } | LiveCmd::Retune { .. } | LiveCmd::PinMode { .. } | LiveCmd::Input { .. }) {
            if let Some(r) = self.core.rounds.as_mut() {
                r.shadow_burst();
            }
        }
        match cmd {
            LiveCmd::Play => self.playing = true,
            LiveCmd::SetUntil(until) => {
                self.spec.until = until;
                self.resume_before_end();
            }
            LiveCmd::Restart => {
                match self.ring.kept.first().map(|k| k.snap.t) {
                    Some(t) if t < self.t() => self.go_back(0),
                    None if self.t() > 0.0 => self.stats.late_edits += 1,
                    _ => {}
                }
                self.resume_before_end();
            }
            LiveCmd::Pause => {
                self.playing = false;
                self.ended = false;
                self.step_until = None;
                self.held.clear();
                self.publish_soon = true;
                if self.co_step {
                    self.co_step = false;
                    let t = self.t();
                    if let Some(co) = &mut self.core.co {
                        co.dev.arm(t, None);
                    }
                }
            }
            LiveCmd::CoStep(to) => {
                let t = self.t();
                if self.core.co.as_mut().is_some_and(|co| co.dev.arm(t, Some(to))) {
                    self.co_step = true;
                    self.co_to = Some(to);
                } else {
                    self.stats.refused += 1;
                }
            }
            LiveCmd::Step(dt) => {
                if dt > 0.0 {
                    self.step_until = Some(self.t() + dt);
                }
            }
            LiveCmd::SetParam { generation, device, key, value } => {
                if generation != self.generation {
                    self.stats.stale += 1;
                } else if self.set_param_all(&device, &key, value) {
                    if matches!(key.as_str(), "r" | "value") {
                        self.thermal_follows(&device, value);
                    }
                    self.guard_edit(&device);
                    self.restart_edited();
                    self.publish_soon = true;
                    self.edit_replans();
                } else {
                    self.stats.unknown += 1;
                }
            }
            LiveCmd::Retune { generation, device, source } => {
                if generation != self.generation {
                    self.stats.stale += 1;
                } else {
                    let source = noise_at(source, self.period);
                    let t = self.t();
                    // the lowered circuit runs it too: a plan builds its blocks from it
                    let ckt = &mut Arc::make_mut(&mut self.core.ckt).devices;
                    if let Some(d) = ckt.v.iter_mut().chain(ckt.i.iter_mut()).find(|d| d.name == device) {
                        d.src.retune(t, source.clone());
                    }
                    // and a plan made from an earlier snapshot of it
                    if let Some(p) = &mut self.core.plan {
                        p.retunes.push((t, device.clone(), source.clone()));
                    }
                    let retuned = match &mut self.core.rounds {
                        None => {
                            let retuned = self.core.s.retune(&device, t, source);
                            match retuned {
                                Some(Retuned::Now) => self.core.st.restart(&self.core.s),
                                Some(Retuned::At(_)) => self.core.st.breaks_changed(&self.core.s),
                                None => {}
                            }
                            retuned
                        }
                        Some(r) => {
                            // the whole circuit keeps it for a new plan; the block that has the
                            // source runs it
                            self.core.s.retune(&device, t, source.clone());
                            let mut retuned = None;
                            for b in r.runs_mut() {
                                match b.s.retune(&device, t, source.clone()) {
                                    Some(Retuned::Now) => b.st.restart(&b.s),
                                    Some(Retuned::At(_)) => b.st.breaks_changed(&b.s),
                                    None => continue,
                                }
                                retuned = retuned.or(Some(Retuned::Now));
                            }
                            retuned
                        }
                    };
                    match retuned {
                        Some(Retuned::Now) => {
                            self.publish_soon = true;
                            self.stats.retuned += 1;
                        }
                        Some(Retuned::At(_)) => self.stats.retuned += 1,
                        None => self.stats.unknown += 1,
                    }
                    if retuned.is_some() {
                        self.edit_replans();
                    }
                }
            }
            LiveCmd::PinMode { generation, device, mode } => {
                if generation != self.generation {
                    self.stats.stale += 1;
                } else if self.ctl_s().find_device(&device).is_some_and(|dev| self.pin_write(dev, mode)) {
                    self.stats.retuned += 1;
                } else {
                    self.stats.unknown += 1;
                }
            }
            LiveCmd::Input { generation, input, .. } => {
                let t = self.t();
                if generation != self.generation {
                    self.stats.stale += 1;
                } else if self.core.co.as_mut().is_some_and(|co| co.dev.input(t, &*input.0)) {
                    self.stats.inputs += 1;
                } else {
                    self.stats.unknown += 1;
                }
            }
            LiveCmd::Subscribe { generation, pins, parts } => {
                if generation != self.generation {
                    self.stats.stale += 1;
                } else {
                    self.sub_pins = pins.into_iter().filter(|&p| (p as usize) < self.pin_i.len()).collect();
                    self.sub_parts = parts.into_iter().filter(|&p| (p as usize) < self.part_p.len()).collect();
                }
            }
            LiveCmd::SetRate(r) => {
                if r > 0.0 {
                    self.spec.rate = r;
                }
            }
            LiveCmd::SetRes(res) => self.set_res(res),
            // a plan that fails leaves the run on one stepper, as at the start
            LiveCmd::SetRounds(on) => {
                // a plan that fails leaves the run whole and says why (`LiveStats::plan_error`)
                if let Err(d) = self.set_rounds(on) {
                    self.plan_failed(d);
                }
            }
            LiveCmd::SetWhole(whole) => self.spec.whole = whole,
            LiveCmd::SetFine(fine) => {
                self.store.fine.clone_from(&fine);
                self.spec.fine = fine;
            }
            LiveCmd::SetRing(seconds) => {
                self.spec.ring = seconds;
                self.publish_soon = true;
            }
            LiveCmd::SetHeard(heard) => self.spec.heard = heard,
            LiveCmd::LogicDetail(chunks) => {
                self.spec.logic_detail = chunks;
                self.store.load_logic(&mut self.spill, &self.spec.logic_detail, &mut self.pending.logic_pages);
            }
            LiveCmd::Replace { generation, circuit, plan } => self.replace(generation, *circuit, &plan),
            LiveCmd::CoSim(slot) => self.attach(slot.take()),
            LiveCmd::Inputs(inputs) => self.inputs = (!inputs.sources.is_empty()).then(|| InputSet::new(inputs)),
            LiveCmd::Sinks => {}
            LiveCmd::At(t, cmd) => self.schedule(t, *cmd, true, self.taken),
            LiveCmd::Edit(events) => self.edit(events, self.taken),
            LiveCmd::Stop => return false,
        }
        self.sync_step_cap();
        true
    }

    /// A run in rounds' round length, seconds: `LiveSpec::round` sample periods.
    pub fn round_len(&self) -> f64 {
        f64::from(self.spec.round.max(1)) * self.period
    }

    /// The longest step now: the bandwidth's (`spec.max_step`), and while paced (a finite rate, no
    /// firmware step or set-up running) at most `rate * values_every`, so that slow motion moves at least once
    /// per publication instead of jumping a bandwidth step and waiting for the clock. It is a
    /// function of the commanded rate, an input like any command, never of the wall clock.
    /// The sample period bounds it too (`LiveSpec::sample_rate`).
    pub fn step_cap(&self) -> f64 {
        let slow = self.spec.rate * self.spec.values_every.as_secs_f64();
        let max_step = self.spec.max_step.min(self.period);
        if self.spec.rate.is_finite() && !self.co_step && !self.preparing() && slow > 0.0 {
            max_step.min(slow)
        } else {
            max_step
        }
    }

    /// Whether a streamed input is short of what the run would read next (`crate::input`), asked
    /// where `run_live` steps on: from the reach of the next loop top of an advance (in rounds, of
    /// the next round). Asks the sources to cover it (`Inputs::want`).
    pub fn input_starved(&mut self) -> bool {
        if self.inputs.is_none() {
            return false;
        }
        let upto = if self.core.rounds.is_some() { self.round_end(self.t()) } else { self.whole_upto() };
        self.starved(upto)
    }

    /// Where the next whole step may end: at the attached process's next instant, at most a step
    /// cap ahead (at once when that instant is due).
    fn whole_upto(&self) -> f64 {
        let t = self.t();
        let next = self.core.co.as_ref().map_or(f64::INFINITY, |c| c.dev.next());
        t.max(next.min(t + self.step_cap()))
    }

    /// The end of the round that opens at `t0`: on the round grid, at the next timed command, at
    /// a `Step`'s end or at the take's end.
    fn round_end(&self, t0: f64) -> f64 {
        let mut end = self.core.rounds.as_ref().expect("in rounds").end_after(t0);
        if let Some(&(at, _)) = self.timeline.get(self.core.next_event) {
            if at > t0 {
                end = end.min(at);
            }
        }
        if let Some(u) = self.stop_at() {
            end = end.min(u);
        }
        end
    }

    /// The input rule at a loop top whose step (or round) may end at `upto`: the attached process
    /// reads up to its `input_span` past where the analog stands when it runs.
    fn starved(&mut self, upto: f64) -> bool {
        if self.inputs.is_none() {
            return false;
        }
        let span = self.core.co.as_ref().and_then(|c| c.dev.input_span()).map_or(0.0, |s| s.max(0.0));
        let paced = (self.spec.rate.is_finite() && !self.co_step && !self.preparing()).then(|| self.spec.rate * (self.spec.values_every + self.spec.slice).as_secs_f64());
        let t = self.t();
        self.inputs.as_mut().is_some_and(|inputs| inputs.starved(upto + span, paced, t))
    }

    /// A resistor's thermal noise follows its value: its companion current source (`#noise`, see
    /// `SimCircuit::thermal_noise`) takes the new resistance's density at the sample period,
    /// keeping its seed.
    fn thermal_follows(&mut self, resistor: &str, r: f64) {
        let noise = format!("{resistor}#noise");
        if self.core.s.find_device(&noise).is_none() {
            return;
        }
        if let Some(Source { wave: Wave::Noise { sigma, .. }, .. }) = crate::circuit::thermal_noise_source(r, self.core.s.temp - crate::newton::KELVIN, self.period, 0) {
            self.set_param_all(&noise, "sigma", sigma);
        }
    }

    fn sync_step_cap(&mut self) {
        let cap = self.step_cap();
        if self.core.st.max_step != cap {
            self.core.st.set_max_step(cap);
        }
        let period = self.period;
        let within = self.spec.res.min(self.spec.max_step);
        let own = (self.spec.own_bound && within > 0.0 && cap >= period).then(|| crate::rounds::OwnBound::new(period, within, self.spec.round.max(1))).flatten();
        if let Some(r) = &mut self.core.rounds {
            for b in r.runs_mut() {
                if b.st.max_step != cap {
                    b.st.set_max_step(cap);
                }
            }
            // edge restarts on: Fast, where the exporters mark their crossings
            r.own_bound = own.filter(|_| r.edges);
            r.renew_bounds(cap, period);
        }
    }

    /// A new lowering: voltages and currents carry over by unknown name, device state by device
    /// name, time continues; the trace starts over in the new generation and the subscription
    /// clears. In rounds it goes on on the cut kept for the new circuit when one is, else on its
    /// own plan, made here. A circuit that fails to set up leaves the running one in place.
    fn replace(&mut self, generation: u64, mut circuit: SimCircuit, plan: &ValuePlan) {
        self.settle_rounds();
        // in rounds, the blocks' state is the run's
        if let Some(r) = &self.core.rounds {
            r.compose(&mut self.core.s);
        }
        circuit.opts.reltol = self.spec.reltol_at(self.spec.res);
        tune_noise(&mut circuit, self.period);
        let pins = lowered_pins(&circuit);
        let mut ns = match Solver::new(&circuit) {
            Ok(ns) => ns,
            Err(d) => {
                self.stats.replace_error = Some(d);
                return;
            }
        };
        let t = self.t();
        let handover = crate::analysis::handover::hand_over(&mut ns, &self.core.s, t);
        let st = Stepper::resume(&mut ns, t, self.spec.max_step);
        self.recall_planner();
        self.leave_warm();
        let mut stats = std::mem::take(&mut self.stats);
        stats.handover = Some(handover);
        let (playing, held, co_step, co_to) = (self.playing, std::mem::take(&mut self.held), self.co_step, self.co_to);
        let old_pins = std::mem::take(&mut self.core.lowered_pins);
        let co = self.core.co.take();
        let warm_on = self.core.warm.on;
        let (planner, helpers, shuffle, checks, memory) = (self.core.planner.take(), self.helpers.take(), self.pack_shuffle, self.checks.take(), self.memory.take());
        // it plans again at once: a due re-plan is met
        let (plan_cost, pads_moved) = (self.core.plan_cost, std::mem::take(&mut self.core.pads_moved));
        let (rounds_id, merged) = (self.rounds_id, std::mem::take(&mut self.merged));
        // the run's commands count on: the host counts them over the run, not per circuit
        let (taken, stepped_with) = (self.taken, self.stepped_with);
        let (start, parked) = (self.start, self.parked.take());
        let started = self.started;
        *self = Self::assemble(ns, st, circuit, plan.clone(), generation, self.spec.clone(), self.period);
        match start {
            Start::Preparing { .. } | Start::Planning => self.finish_start(),
            Start::Done { .. } => (self.start, self.started) = (start, started),
            _ => {}
        }
        (self.rounds_id, self.merged) = (rounds_id, merged);
        (self.taken, self.stepped_with) = (taken, stepped_with);
        self.core.warm.on = warm_on;
        self.stats = stats;
        self.stats.replaced += 1;
        self.playing = playing;
        self.held = held;
        self.co_step = co_step;
        self.co_to = co_to;
        (self.core.planner, self.helpers, self.pack_shuffle, self.checks, self.memory) = (planner, helpers, shuffle, checks, memory);
        (self.core.plan_cost, self.core.pads_moved) = (plan_cost, pads_moved);
        // what earlier runs of the new circuit refused and landed, met before its first plan
        self.recall_memory();
        self.apply_refusals();
        self.apply_landings();
        // a cut kept for the new circuit carries the replacement (`Planner::can_carry`): the run
        // goes on at once on that cut, built and proved here, and the key's own plan is made behind
        // it, as a carried start's is (`replan`)
        self.carrying = self.checks.is_some() && self.core.planner.as_ref().is_some_and(|p| p.can_carry(&self.core.ckt, &self.core.s));
        if let Err(d) = self.first_plan() {
            self.stats.replace_error = Some(d);
        }
        self.sync_step_cap();
        // A GPIO's carried mode is the running one (a co-simulated process may have written it).
        // One whose lowered mode changed carries the bench's new stimulus: it is written now, as a
        // `PinMode` command would be (a PWM over a PWM preloads at the next period start).
        // (a write of the new lowered mode is no first change of a pad)
        self.core.lowered_pins = pins.clone();
        for (name, mode) in &pins {
            if old_pins.iter().any(|(n, m)| n == name && m != mode) {
                if let Some(dev) = self.ctl_s().find_device(name) {
                    self.pin_write(dev, *mode);
                }
            }
        }
        self.watch_logic();
        // the process carries on: its pins resolve again in the new circuit, its watches register
        // with the new GPIO bank
        self.core.co = match co {
            Some(c) => {
                let nets = self.co_nets(&*c.dev);
                Some(Co { pins: resolve_pins(self.ctl_s(), &c.table), watch_gen: None, nets, ..c })
            }
            None => None,
        };
        self.logic_rows = self.co_logic_rows();
        self.restart_co_logic();
        self.sync_watches();
        self.rounds_with_co();
        // The parked planner belongs to the run. Warm it for the new lowering and its carried
        // pins, with the start already decided even for a replacement at time zero.
        if let Some(planner) = parked {
            self.parked = Some(None);
            if let Err(d) = self.set_planner(planner) {
                self.stats.replace_error = Some(d);
            }
        }
    }

    /// Steps until `t >= target` or `budget` of wall time has passed, handing each accepted point
    /// to `sink`, the trace and the interval statistics. A `Step` that is reached pauses here and
    /// releases the commands it held; a `CoStep` whose stop is reached pauses here.
    pub fn advance(&mut self, target: f64, budget: Option<Duration>, sink: &mut dyn Sink) -> Result<(), SimDiag> {
        self.advance_direct(target, budget, sink, false)
    }

    fn advance_direct(&mut self, target: f64, budget: Option<Duration>, sink: &mut dyn Sink, paced: bool) -> Result<(), SimDiag> {
        // Direct steppers poll the same hold as run_live. No time passes through a planning call.
        if matches!(self.start, Start::Planning) {
            self.holds_for_plan();
            self.tell_start(sink);
            return Ok(());
        }
        self.tell_start(sink);
        let stepped = self.advance_steps(target, budget, sink, paced);
        self.tell_start(sink);
        stepped
    }

    /// The worker and direct callers have checked the plan hold. Step to the next boundary and
    /// leave a completed start there for the caller to announce before stepping again.
    fn advance_steps(&mut self, target: f64, budget: Option<Duration>, sink: &mut dyn Sink, paced: bool) -> Result<(), SimDiag> {
        if !self.preparing() && self.warm_due.is_some_and(|d| self.t() >= d) {
            self.warm_due = None;
            self.warm_now();
        } else if self.rounds_asked && self.warm_out.as_ref().is_none_or(|w| w.done()) {
            self.take_asked_rounds();
        }
        self.rewind_sinks(sink);
        let start = Instant::now();
        self.paced = paced;
        let stepped = self.advance_to(target, budget, start, sink);
        self.paced = false;
        let wall = start.elapsed();
        self.step_wall += wall;
        self.stats.step_wall += wall;
        stepped?;
        if let Start::Preparing { t0, wall0, rounds } = self.start {
            if self.warm_due.is_some_and(|due| self.t() >= due) {
                // Only time spent stepping counts: the operating point, waits for streamed inputs
                // and time paused are outside advance_to. The warm and the decision share this point.
                let wall = (self.stats.step_wall - wall0).as_secs_f64();
                self.warm_due = None;
                self.warm_now();
                if rounds || self.t() - t0 < ROUNDS_FROM_START * self.spec.rate * wall {
                    self.rounds_asked = true;
                    self.start = Start::Planning;
                } else {
                    self.finish_start();
                }
            } else if self.core.steady || (self.playing && self.at_end()) {
                // A take that ends or settles before the warm is due can start whole at its rest.
                self.finish_start();
            }
        }
        self.stats.logic_spilled += self.store.spill_logic(&mut self.spill, &self.spec.logic_detail, &mut self.pending.logic_pages) as u64;
        self.store.keep(&self.spec.keep(), &mut self.pending.folds);
        self.post_pyramids();
        if self.playing && self.at_end() {
            // the take's end: the run rests there, said at once
            self.playing = false;
            self.ended = true;
            self.publish_soon = true;
        }
        if self.step_until.is_some_and(|u| self.t() >= u) {
            self.step_until = None;
            self.playing = false;
            while let Some((n, cmd)) = self.held.pop_front() {
                if !self.apply_as(cmd, n) {
                    self.held.push_front((n, LiveCmd::Stop));
                    break;
                }
                if self.step_until.is_some() {
                    break;
                }
            }
        }
        Ok(())
    }

    /// `advance` for a run paced to the wall clock (`run_live`'s while the rate is finite):
    /// `clock`, where the clock stands in simulated time, is a soft end. In rounds a stop there
    /// leaves the stages behind block 0 posted, and the next advance goes on with them
    /// (`LiveStats::clock_stops`); a `Step`'s end and the take's end still catch them up.
    pub fn advance_to_clock(&mut self, clock: f64, budget: Option<Duration>, sink: &mut dyn Sink) -> Result<(), SimDiag> {
        self.advance_direct(clock, budget, sink, !self.preparing())
    }

    /// The run's state as it stands, for `restore`: everything that decides its next points
    /// (`Core`), its attached process forked. Taken between `advance` calls, where the run always
    /// stands at a loop top (in rounds, between rounds). Passive: a run that takes snapshots goes
    /// on exactly as one that takes none. None when the attached process cannot fork.
    pub fn snapshot(&self) -> Option<Snapshot> {
        if self.core.co.as_ref().is_some_and(|co| !co.dev.forks) {
            return None;
        }
        Some(Snapshot { t: self.t(), core: self.core.clone() })
    }

    /// The run goes back to `snap` (which stays usable): from here it steps as it did from there,
    /// a plan that was on its way made again from its inputs. The publication is left as it is
    /// (the trace, the statistics, the sinks), and so is the timeline with its go-in rounds
    /// (`go_ins`): each plan goes in where it did.
    pub fn restore(&mut self, snap: &Snapshot) {
        self.leave_plan_run();
        self.carrying = false;
        self.keeping = None;
        self.core = snap.core.clone();
        self.delivered = snap.t;
        if matches!(self.start, Start::Planning) {
            // The warm is for the abandoned state. An edit that restores a snapshot starts whole
            // there; the parked planner can still be asked for at the new front.
            self.rounds_asked = false;
            self.leave_warm();
            self.finish_start();
        }
        self.merged.clear();
        if let Some(r) = &self.core.rounds {
            self.rounds_id = self.rounds_id.max(r.id);
        }
        self.post_plan();
        self.publish_soon = true;
    }

    /// Takes a snapshot for an edit to go back to (`LiveSpec::snapshots`) when one is due: where
    /// `advance` returned, once the run has stepped the spacing in wall time since the last (the
    /// first at once: `run_live` takes it before the run's first step). `sink` is the run's: its
    /// state goes with the snapshot (`Sink::save`).
    /// Passive: the run steps on as it would have. A snapshot is of every block at one instant:
    /// one due while stages run behind block 0 is taken in rounds as they pass a round start
    /// (`mark_keep`), none caught up for it.
    pub fn keep_snapshot(&mut self, sink: &dyn Sink) {
        if self.keeping.is_some() || !self.snapshot_due(Duration::ZERO) || !self.merged.t.is_empty() || self.core.rounds.as_ref().is_some_and(|r| r.behind()) {
            return;
        }
        let start = Instant::now();
        // the process's changes up to now go into the trace first, committed: the snapshot's
        // process hands out none the trace has
        self.take_co_logic(true);
        let (taken, bytes) = crate::alloc::measure(|| self.snapshot().map(|snap| (snap, sink.save())));
        let Some((snap, sinks)) = taken else { return };
        self.keep_kept(Kept { snap, sinks, level: 0, bytes: bytes.unwrap_or(UNMEASURED) }, start.elapsed());
    }

    /// A snapshot taken into the ring, thinned and fitted to the budget, with what it took.
    fn keep_kept(&mut self, kept: Kept, cost: Duration) {
        let bytes = kept.bytes;
        self.ring.kept.push(kept);
        self.thin();
        self.fit();
        self.ring.cost = cost;
        self.ring.at_wall = self.stats.step_wall;
        self.stats.snapshots += 1;
        self.stats.snapshot_wall = self.ring.cost;
        self.stats.snapshot_last = bytes;
        self.ring_stats();
    }

    /// A snapshot due in rounds with the stages behind block 0, taken without catching them up
    /// (`Rounds::mark_keep`): the run's state but for its rounds here, at this round start (its
    /// attached process forked, its changes up to now committed to the trace first); the rounds
    /// as each stage passes this round, and the merged state, the logic taps and the sinks once
    /// the merged stream stands at its end (`complete_keep`). Nothing when the attached process
    /// cannot fork.
    fn mark_keep(&mut self) {
        if self.core.co.as_ref().is_some_and(|co| !co.dev.forks) {
            return;
        }
        let start = Instant::now();
        self.take_co_logic(true);
        let rounds = self.core.rounds.take();
        let (core, bytes) = crate::alloc::measure(|| self.core.clone());
        self.core.rounds = rounds;
        self.core.rounds.as_mut().expect("in rounds").mark_keep();
        self.keeping = Some(Box::new(KeepCore { core, cost: start.elapsed(), bytes }));
    }

    /// The snapshot on its way in rounds (`mark_keep`) once its rounds stand at its round end and
    /// the sinks have every point up to it (`Rounds::take_kept`), kept as `keep_snapshot` keeps
    /// one: the merged state and the logic taps as they stand now, the sinks saved now. True when
    /// it was. O(the merge side, the merged state and the sinks' save) then.
    fn complete_keep(&mut self, sink: &dyn Sink) -> bool {
        if self.keeping.is_none() || !self.merged.t.is_empty() {
            return false;
        }
        let start = Instant::now();
        let Some((rounds, t, rounds_bytes)) = self.core.rounds.as_mut().and_then(|r| r.take_kept()) else { return false };
        let mut keep = self.keeping.take().expect("a snapshot on its way");
        let (sinks, bytes) = crate::alloc::measure(|| {
            keep.core.s.x.copy_from_slice(&self.core.s.x);
            keep.core.logic = self.core.logic.clone();
            sink.save()
        });
        keep.core.rounds = Some(rounds);
        let bytes = match (keep.bytes, bytes) {
            (Some(a), Some(b)) => a + b + rounds_bytes,
            _ => UNMEASURED,
        };
        self.stats.snapshots_marked += 1;
        self.keep_kept(Kept { snap: Snapshot { t, core: keep.core }, sinks, level: 0, bytes }, keep.cost + start.elapsed());
        true
    }

    /// Whether a snapshot is due (`keep_snapshot`) once the run has stepped `more` wall time on top
    /// of `LiveStats::step_wall`.
    fn snapshot_due(&self, more: Duration) -> bool {
        let spec = &self.spec.snapshots;
        if spec.span <= 0.0 || !self.running() {
            return false;
        }
        let since = (self.stats.step_wall + more).saturating_sub(self.ring.at_wall);
        let paid = Duration::try_from_secs_f64(self.ring.cost.as_secs_f64() / spec.share).unwrap_or(Duration::MAX);
        self.ring.kept.is_empty() || since >= spec.every.max(paid)
    }

    /// Over the byte budget the thinning reaches into the dense span: the snapshot that goes is
    /// the one whose going leaves the smallest gap for how far back the gap starts, so the kept
    /// ones space out in proportion to their age (the oldest region thins first). The first always
    /// stays, the run's restore point that is never dropped (`breach_taken`, `take_anchor`): an edit
    /// can always go back as far as before, and steps more on the way; the newest stays while
    /// another can go (two over the budget stay). Logged once each time the snapshots reach the budget. O(kept) a
    /// removal.
    fn fit(&mut self) {
        let budget = self.spec.snapshots.bytes;
        let now = self.core.st.t();
        let kept = &mut self.ring.kept;
        let mut total: u64 = kept.iter().map(|k| k.bytes).sum();
        let over = total > budget;
        while total > budget && kept.len() > 2 {
            let gap = |i: usize| (kept[i + 1].snap.t - kept[i - 1].snap.t) / (now - kept[i - 1].snap.t).max(f64::MIN_POSITIVE);
            let Some(i) = (1..kept.len() - 1).min_by(|&a, &b| gap(a).total_cmp(&gap(b))) else { break };
            total -= kept.remove(i).bytes;
        }
        if over && !self.ring.full {
            let back = kept.first().map_or(0.0, |k| now - k.snap.t);
            eprintln!("fab-elec live: the snapshots reached their budget of {:.1} MB ({} kept, {:.1} MB, reaching {:.4} s back): an edit behind the front steps from further back",
                budget as f64 / 1048576.0, kept.len(), total as f64 / 1048576.0, back);
        }
        self.ring.full = over;
    }

    /// The kept snapshots' count and bytes, for the statistics.
    fn ring_stats(&mut self) {
        self.stats.snapshots_kept = self.ring.kept.len();
        self.stats.snapshot_bytes = self.ring.kept.iter().map(|k| k.bytes).sum();
    }

    /// Behind the dense span, of two neighbours that thinned as often the newer goes and the older
    /// counts once more: the past keeps a number of snapshots logarithmic in its length.
    fn thin(&mut self) {
        let edge = self.t() - self.spec.snapshots.span;
        let kept = &mut self.ring.kept;
        loop {
            let behind = kept.partition_point(|k| k.snap.t < edge);
            let Some(i) = (1..behind).rev().find(|&i| kept[i].level == kept[i - 1].level) else { break };
            drop(kept.remove(i));
            kept[i - 1].level += 1;
        }
    }

    /// Drops every snapshot: what follows is another recording (a new process, resolution or
    /// lowering), which an edit before it cannot go back across, and another run, so the go-in
    /// rounds written ahead of it go too. A run made again from a timeline (`follow`) keeps those
    /// it was given: the run it follows met the same change at the same place (a heard start
    /// turning its rounds on), with none written ahead of it there.
    fn forget_snapshots(&mut self) {
        if !self.followed {
            self.forget_go_ins(self.t());
        }
        self.ring.kept.clear();
        // the anchor too: the next round start takes one
        (self.ring.anchor, self.ring.anchor_for) = (None, 0);
        self.ring.full = false;
        self.ring_stats();
    }

    /// `LiveCmd::Edit`: the value commands go on the timeline at their times; when the earliest is
    /// one the run already passed, the run first goes back to the newest snapshot at or before it.
    /// With none kept that far back the edit is late: it moves to the run's time whole, keeping
    /// its events' distances.
    fn edit(&mut self, events: Vec<(f64, LiveCmd)>, n: u64) {
        let timed = |cmd: &LiveCmd| matches!(cmd, LiveCmd::SetParam { .. } | LiveCmd::Retune { .. } | LiveCmd::PinMode { .. } | LiveCmd::Input { .. });
        let earliest = events.iter().filter(|(t, cmd)| t.is_finite() && timed(cmd)).map(|e| e.0).fold(f64::INFINITY, f64::min);
        let mut late = 0.0;
        if earliest < self.t() {
            match self.ring.kept.iter().rposition(|k| k.snap.t <= earliest) {
                Some(i) => self.go_back(i),
                None => {
                    self.stats.late_edits += 1;
                    late = self.t() - earliest;
                }
            }
        }
        for (t, cmd) in events {
            self.schedule(t + late, cmd, false, n);
        }
        self.resume_before_end();
    }

    /// A run that rested at the take's end plays on when it now stands before it (an edit sent it
    /// back, or the end moved later). O(1).
    fn resume_before_end(&mut self) {
        if self.ended && !self.at_end() {
            self.playing = true;
            self.ended = false;
        }
    }

    /// Whether the run stands at the take's end: within a millionth of its largest step, so a
    /// step on the sample grid that lands a rounding below the end is at it. O(1).
    fn at_end(&self) -> bool {
        self.t() >= self.spec.until - self.end_slack()
    }

    fn end_slack(&self) -> f64 {
        self.spec.max_step.max(1e-9) * 1e-6
    }

    /// The take's end while the run plays toward it, else infinity: a step lands on it exactly.
    fn take_end(&self) -> f64 {
        if self.playing { self.spec.until } else { f64::INFINITY }
    }

    /// Where the run's stepping stops next: a `Step`'s end, and while it plays the take's end.
    fn stop_at(&self) -> Option<f64> {
        let end = Some(self.take_end()).filter(|u| u.is_finite());
        match (self.step_until, end) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The run goes back to kept snapshot `i` (which stays; the later ones go), with its
    /// publication: what was pending goes out as it was, then the trace is cut at the snapshot's
    /// time (`Rewind`), the values' interval starts there, and the sinks go back before the next
    /// point (`rewind_sinks`).
    fn go_back(&mut self, i: usize) {
        self.ring.kept.truncate(i + 1);
        let Some(kept) = self.ring.kept.pop() else { return };
        self.go_back_to(&kept);
        // an anchor past the return is another run's: the ring's first, kept for good, is the
        // restore point before the rounds the run is in now
        if self.ring.anchor.as_ref().is_some_and(|a| a.snap.t > kept.snap.t) {
            self.ring.anchor = None;
        }
        self.ring.kept.push(kept);
        self.ring_stats();
    }

    /// `go_back` to `kept`, the ring as it is.
    fn go_back_to(&mut self, kept: &Kept) {
        let start = Instant::now();
        let t = kept.snap.t;
        if let Some(d) = self.finish_delta() {
            self.pending.ready.push_back(d);
        }
        // the restored process takes over the running one's event stream and machine feeds
        let outputs = self.core.co.as_mut().and_then(|co| co.dev.take_outputs());
        self.restore(&kept.snap);
        if let (Some(outputs), Some(co)) = (outputs, self.core.co.as_mut()) {
            co.dev.put_outputs(outputs);
        }
        self.sink_rewind = Some((t, kept.sinks.clone()));
        let rewind = self.store.rewind_producer(t, &mut self.spill);
        // a logic chunk opened from the file is sent again up to the cut
        for &(id, _) in &rewind.resent {
            let entries = self.store.logic(id).map(|trace| trace.open.clone()).unwrap_or_default();
            if let Some((_, found)) = self.pending.logic.iter_mut().find(|(of, _)| *of == id) {
                *found = entries;
            }
        }
        self.pending.from = self.store.base + self.store.len();
        self.pending.rewind = Some(rewind);
        self.interval_home();
        for (v, &u) in self.net_v.iter_mut().zip(&self.taps.map.nodes) {
            *v = self.core.s.x[u];
        }
        self.interval = Interval::new(t, &self.net_v, self.pin_i.len(), self.part_p.len(), self.spec.grid);
        self.core.steady = false;
        // a firmware step under way runs on to its stop from here
        if self.co_step {
            let armed = match (self.co_to, &mut self.core.co) {
                (Some(to), Some(co)) => co.dev.arm(t, Some(to)),
                _ => false,
            };
            if !armed {
                self.co_step = false;
            }
        }
        self.sync_step_cap();
        self.ring.at_wall = self.stats.step_wall;
        self.ring_stats();
        self.stats.rewinds += 1;
        self.stats.rewind_wall = start.elapsed();
    }

    /// The growth rule (`HOLD`): a value a block exported in rounds that is not a number, or a
    /// voltage past what the circuit can hold (`Rounds::breach`), is a loop that reading values a
    /// round late set running away; a healthy board never comes near it. The breached round's
    /// points went nowhere (`close_round`, `flush_rounds`). The run goes back to its newest restore
    /// point at or before the round that breached: a kept snapshot (`LiveSpec::snapshots`) or the
    /// anchor taken where the rounds began (`take_anchor`), so there always is one. There the
    /// refusal of the nodes the breach blames (`Rounds::blame`) goes on the run's timeline
    /// (`refusals`), and takes effect as the run meets it (`apply_refusals`): it leaves rounds, no
    /// plan reads a value late at those nodes again, and it plans anew. A run made again from the
    /// timeline (`follow`) meets it at the same place: the same bits, no breach. The inputs change
    /// from the restore point on, as at an edit: the go-in rounds written since are another
    /// run's. True when it went back.
    fn breach_taken(&mut self, sink: &mut dyn Sink) -> Result<bool, SimDiag> {
        let Some(r) = self.core.rounds.as_ref() else { return Ok(false) };
        let Some(b) = r.breach.clone() else { return Ok(false) };
        let blamed = r.blame(b.block, Some(b.coupling));
        let what = if b.v.is_finite() { format!("past the {:.3e} V the circuit can hold ({HOLD} times its largest source)", r.hold) } else { "not a number".into() };
        let what = format!("{} = {:e} at {:.6} s in rounds, {what}; the cut read late at {} refused", b.name, b.v, b.t, blamed.join(", "));
        let ring = self.ring.kept.iter().rposition(|k| k.snap.t <= b.from);
        let anchor = self.ring.anchor.as_ref().map(|a| a.snap.t).filter(|&t| t <= b.from);
        let ts = match (ring, anchor) {
            (Some(i), a) if a.is_none_or(|a| self.ring.kept[i].snap.t >= a) => {
                self.go_back(i);
                self.ring.kept[i].snap.t
            }
            (_, Some(a)) => {
                self.go_back_to_anchor();
                a
            }
            // none: a plan that reads late goes in only on a run that can go back (`install`),
            // which keeps the first snapshot for good or takes the anchor where the rounds begin
            // (`take_anchor`), so this is a broken rule, said as such
            _ => return Err(SimDiag::setup(format!("{what}, and no restore point at or before {:.6} s: a run reading late keeps one (a bug)", b.from))),
        };
        self.rewind_sinks(sink);
        self.forget_go_ins(ts);
        let at = self.refusals.partition_point(|r| r.0 <= ts);
        self.refusals.insert(at, (ts, blamed));
        self.stats.breaches += 1;
        self.stats.last_breach = Some(what);
        Ok(true)
    }

    /// The shadow rounds' rule (A-S5, `Rounds::shadow_over`): a late-reading block whose round, run
    /// again with what it read late on time, ends past its bar from where it ran to, or whose edges
    /// lateness moves one way (drift) or by much (smear) over its rounds, is reading late where
    /// lateness changes it, past what the plan promised. The cut it reads late at
    /// (`Rounds::blame`) is refused from this round start on: written on the run's timeline
    /// (`refusals`) as the growth rule's refusal is, it takes effect here (`apply_refusals`), and the
    /// run leaves rounds and plans anew. The run does not go back: what it published was within
    /// what the circuit holds, and the round it was found in is a helper's turn behind. A run made
    /// again from its timeline (`follow`) meets the refusal at the same place, and its own shadow
    /// rounds, back at other rounds, refuse nothing. O(the block's late reads).
    fn shadow_taken(&mut self) {
        let Some(r) = self.core.rounds.as_mut() else { return };
        let Some(over) = r.shadow_over() else { return };
        if self.followed {
            return;
        }
        let blamed = r.blame(over.block, None);
        let t = self.t();
        let what = match over.why {
            OverWhy::Crossings(ran, again) => format!("{} crosses {again} times in its round to {:.6} s read on time, {ran} as it ran; the cut read late at {} refused at {t:.6} s",
                over.name, over.end, blamed.join(", ")),
            OverWhy::End(figure, bar) => format!("{} ends {figure:.3e} of the circuit's largest source from where its round to {:.6} s ran, read on time, however shifted within its step (bar {bar:.1e}); the cut read late at {} refused at {t:.6} s",
                over.name, over.end, blamed.join(", ")),
            OverWhy::Shifts(which, figure, bar, n) => format!("{}'s edges, read on time, {which} {figure:.2e} of its time from where they ran, over {n} rounds with its own crossings to {:.6} s (bar {bar:.1e}); the cut read late at {} refused at {t:.6} s",
                over.name, over.end, blamed.join(", ")),
        };
        let at = self.refusals.partition_point(|r| r.0 <= t);
        self.refusals.insert(at, (t, blamed));
        self.stats.shadow_acts += 1;
        self.stats.last_shadow_act = Some(what);
    }

    /// The chain shadow's act (Fable's H1/H2 answer, Q3; `Rounds::chain_act`): a born export
    /// whose tree, run again with its jump landed, held a member past its bar over the window
    /// after it ("own held") lands the prefix to each coupling that member read on its tree.
    /// Each distinct landing goes on the timeline once, taking effect at this round start
    /// (`apply_landings`). A run following the timeline (`follow`) meets it at the same place.
    fn chain_taken(&mut self) {
        let Some(r) = self.core.rounds.as_mut() else { return };
        let convicted = r.chain_act();
        if self.followed {
            return;
        }
        let t = self.t();
        for (land, what) in convicted {
            if self.core.landed.contains(&land) || self.landings.iter().any(|l| l.1 == land) {
                continue;
            }
            let at = self.landings.partition_point(|l| l.0 <= t);
            self.stats.chain_acts += 1;
            self.stats.last_chain_act = Some(format!("{}'s chain to {} held {what}; landed at {t:.6} s", land.born, land.to));
            self.landings.insert(at, (t, land));
        }
    }

    /// The landings on the timeline the run has met (`landings`, in time, from where it was
    /// applying them): its planners learn each prefix. A run in rounds with a chain born at one
    /// of those exports plans anew from here and steps on in rounds meanwhile, on the plan it was
    /// heard on: the landed plan goes in at its round start, as any re-plan does (a conviction is
    /// about a plan already heard; stepping whole for the plan's lead is a gap the listener hears);
    /// a plan on its way is made again (`remake_plan_on_way`); the host remembers them (`remember`). Called at
    /// the end of every round start, after the refusals (`at_round_start`), and where a planner is
    /// set or the circuit replaced, before its first plan, so a run made again from its timeline
    /// applies it at the same place. O(landings met + the circuit's size).
    fn apply_landings(&mut self) {
        let t = self.t();
        let mut born = Vec::new();
        while let Some((at, land)) = self.landings.get(self.core.landings_done).filter(|l| l.0 <= t) {
            eprintln!("fab-elec live: the run lands {}'s chain to {} from {at:.6} s", land.born, land.to);
            if !self.core.landed.contains(land) {
                self.core.landed.push(land.clone());
            }
            born.push(land.born.clone());
            self.core.landings_done += 1;
        }
        if born.is_empty() {
            return;
        }
        self.teach_planners();
        self.remember();
        self.remake_plan_on_way();
        if self.core.rounds.as_ref().is_some_and(|r| r.chains_named(&born)) {
            self.replan_after(0.0);
        }
    }

    /// The refusals on the timeline the run has met (`refusals`: the growth rule's and the shadow
    /// rounds', in time, from where it was
    /// applying them): their nodes join the run's refused ones and its planners learn them; a run
    /// in rounds that read a value late at one leaves them and plans anew from here; a plan on its
    /// way is made again (`remake_plan_on_way`); the host remembers them (`remember`). Called at
    /// the end of every round start (`at_round_start`), and where a planner is set or the circuit
    /// replaced, before its first plan, so a run that went back for the refusal and a run made
    /// again from its timeline apply it at the same place. O(refusals met + the circuit's size).
    fn apply_refusals(&mut self) {
        let t = self.t();
        let mut met = false;
        while let Some((at, nodes)) = self.refusals.get(self.core.refusals_done).filter(|r| r.0 <= t) {
            eprintln!("fab-elec live: the run refuses the cut read late at {} from {at:.6} s", nodes.join(", "));
            for node in nodes {
                if !self.core.refused.contains(node) {
                    self.core.refused.push(node.clone());
                }
            }
            self.core.refusals_done += 1;
            met = true;
        }
        if !met {
            return;
        }
        self.teach_planners();
        self.remember();
        self.remake_plan_on_way();
        if self.core.rounds.as_ref().is_some_and(|r| r.reads_late_at(&self.core.refused).is_some()) {
            self.leave_rounds();
            self.replan_after(0.0);
        }
    }

    /// Whether the run can be taken back: its attached process, if any, forks (`CoSim::fork`). One
    /// that cannot takes no snapshot and no plan that reads a value late (`install`, `attach`).
    fn goes_back(&self) -> bool {
        self.core.co.as_ref().is_none_or(|co| co.dev.forks)
    }

    /// The run refuses `nodes` (`Core::refused`), and its planners learn them. O(refusals).
    fn refuse_nodes(&mut self, nodes: &[String]) {
        for node in nodes {
            if !self.core.refused.contains(node) {
                self.core.refused.push(node.clone());
            }
        }
        self.teach_planners();
    }

    /// The planners learn the run's refusals (`Core::refused`) and landings (`Core::landed`): the
    /// one at hand, a plan's on its way and a parked one. O(refusals + landings).
    fn teach_planners(&mut self) {
        let planners = self.core.planner.iter_mut().chain(self.core.plan.iter_mut().map(|p| &mut p.planner)).chain(self.parked.iter_mut().flatten());
        for p in planners {
            for node in &self.core.refused {
                p.refuse_cut(node);
            }
            for land in &self.core.landed {
                p.land_chain(land);
            }
        }
    }

    /// The anchor, the restore point never dropped (`Ring::anchor`): taken at the round start where
    /// a plan's rounds begin, the stages caught up and the merged rows delivered (as a flush leaves
    /// them), with the sinks' state, so a breach in them can always go back before any. A plan
    /// whose rounds begin with stages behind (one that keeps blocks running) leaves the anchor where
    /// it was, before them. O(the run's state) once a plan; its bytes in `LiveStats::anchor_bytes`.
    fn take_anchor(&mut self, sink: &mut dyn Sink) {
        let Some(id) = self.core.rounds.as_ref().filter(|r| r.id != self.ring.anchor_for && !r.behind()).map(|r| r.id) else { return };
        // a kept snapshot is one already: the ring's first stays for good (`fit`, `thin`), at or
        // before here; a run that keeps snapshots takes no anchor (the worker keeps a run's start before it
        // steps: a heard run, which keeps snapshots, takes none)
        if !self.ring.kept.is_empty() {
            (self.ring.anchor, self.ring.anchor_for) = (None, id);
            return;
        }
        self.deliver_rows(sink, false);
        if !self.merged.t.is_empty() {
            return;
        }
        let start = Instant::now();
        // as for a kept snapshot: the process's changes up to now go into the trace first
        self.take_co_logic(true);
        let (taken, bytes) = crate::alloc::measure(|| self.snapshot().map(|snap| (snap, sink.save())));
        let Some((snap, sinks)) = taken else { return };
        self.ring.anchor_for = id;
        self.ring.anchor = Some(Kept { snap, sinks, level: 0, bytes: bytes.unwrap_or(UNMEASURED) });
        self.stats.anchors += 1;
        self.stats.anchor_bytes = bytes.unwrap_or(UNMEASURED);
        self.stats.anchor_wall = start.elapsed();
    }

    /// The run goes back to its anchor (`Ring::anchor`), which stays; the kept snapshots after it
    /// go (another run's).
    fn go_back_to_anchor(&mut self) {
        let Some(anchor) = self.ring.anchor.take() else { return };
        let t = anchor.snap.t;
        self.ring.kept.retain(|k| k.snap.t <= t);
        self.go_back_to(&anchor);
        self.ring.anchor = Some(anchor);
        self.ring_stats();
    }

    /// Hands the sinks a return the run made (`go_back`) before their next point: each goes back
    /// to its state at the snapshot (`Sink::rewind`). True when there was one.
    pub fn rewind_sinks(&mut self, sink: &mut dyn Sink) -> bool {
        let Some((t, saved)) = self.sink_rewind.take() else { return false };
        sink.rewind(t, saved.as_deref(), &self.core.s.unknowns);
        true
    }

    /// `advance`'s stepping: to `target`, or until the budget runs out, a publication is asked for
    /// or the device stops.
    fn advance_to(&mut self, target: f64, budget: Option<Duration>, start: Instant, sink: &mut dyn Sink) -> Result<(), SimDiag> {
        // a plan going in where a round starts may move the run onto rounds or off them
        loop {
            let moved = if self.core.rounds.is_some() { self.advance_rounds(target, budget, start, sink)? } else { self.advance_whole(target, budget, start, sink)? };
            if !moved {
                return Ok(());
            }
        }
    }

    /// `advance_to` on one stepper; true when a plan put the run in rounds.
    /// Whole steps toward `target`. At a soft end (the budget, a publication due, and `target`
    /// while it is the wall clock's, `paced`) an attached process that something will read is asked
    /// to settle, and the steps go on until it has (`settle_on`), so the reads re-run nothing. A
    /// hard end is never passed: `stop_at`, and a target that is not the clock's (a caller's).
    fn advance_whole(&mut self, target: f64, budget: Option<Duration>, start: Instant, sink: &mut dyn Sink) -> Result<bool, SimDiag> {
        let mut asked = None;
        let r = self.whole_steps(target, budget, start, &mut asked, sink);
        if asked.is_some() {
            if let Some(co) = self.core.co.as_mut().filter(|co| !co.dev.settled()) {
                co.dev.settle(false);
                self.stats.co_unsettled += 1;
            }
        }
        r
    }

    /// At a soft end of an advance: true while the steps go on for the attached process to settle
    /// (asked at the first, `asked` its wall instant), false to end here. Asks only when a read
    /// follows (`values`' state, a traced net of the process), and waits at most one more slice
    /// of wall time: a run that slow reads as it does unsettled, with its re-run.
    fn settle_on(&mut self, asked: &mut Option<Instant>) -> bool {
        let (co_state, slice) = (self.spec.co_state, self.spec.slice);
        let Some(co) = self.core.co.as_mut() else { return false };
        if co.dev.settled() || !(co_state || co.nets.iter().any(Option::is_some)) {
            return false;
        }
        match asked {
            None => {
                co.dev.settle(true);
                *asked = Some(Instant::now());
                true
            }
            Some(at) => at.elapsed() < slice,
        }
    }

    fn whole_steps(&mut self, target: f64, budget: Option<Duration>, start: Instant, asked: &mut Option<Instant>, sink: &mut dyn Sink) -> Result<bool, SimDiag> {
        let stop = self.stop_at().unwrap_or(f64::INFINITY);
        let hard = if self.paced { stop } else { stop.min(target) };
        let target = target.min(hard);
        while self.core.st.t() < hard {
            // Return to advance at the first boundary past the set-up, including after a nested
            // Bus::advance. Firmware gets its exact landing and the warm sees the state it left.
            if matches!(self.start, Start::Preparing { .. }) && self.warm_due.is_some_and(|due| self.core.st.t() >= due) {
                break;
            }
            if self.core.st.t() >= target && !self.settle_on(asked) {
                break;
            }
            self.apply_due();
            if self.at_round_start() {
                sink.started();
            }
            if self.core.rounds.is_some() {
                return Ok(true);
            }
            // a streamed input short of this step's reach holds the run here (`run_live` waits)
            if self.inputs.is_some() && self.starved(self.whole_upto()) {
                break;
            }
            // an attached process: land exactly on its next instant, and run it there or at a hit
            let next = self.core.co.as_ref().map_or(f64::INFINITY, |c| c.dev.next());
            let from = self.core.st.t();
            let ran = if next <= self.core.st.t() {
                let ran = self.run_co(sink)?;
                // a run at the present that only found the process's next instant (a read left it
                // on its mark) steps on before a soft end: an advance that a read follows moves
                if ran == CoRun::Go && self.core.co.as_ref().is_some_and(|c| c.dev.next() > self.core.st.t()) {
                    continue;
                }
                ran
            } else {
                // a whole step may not pass the take's end: the take is exactly its length
                self.step_once(next.min(self.take_end()), sink)?;
                if self.core.st.t() == next || !self.core.hits.is_empty() {
                    self.run_co(sink)?
                } else {
                    CoRun::Go
                }
            };
            if self.core.planner.is_some() || self.core.plan.is_some() {
                self.stats.off_rounds += self.core.st.t() - from;
            }
            if self.rest_check(sink) {
                break;
            }
            if ran == CoRun::Stopped || (self.publish_soon || budget.is_some_and(|b| start.elapsed() >= b)) && !self.settle_on(asked) {
                break;
            }
        }
        Ok(false)
    }

    /// One `Stepper::step` (at most to `t_end`): each accepted point goes to `sink`, the trace and
    /// the interval statistics; the GPIO watch hits it accepted join `hits`.
    fn step_once(&mut self, t_end: f64, sink: &mut dyn Sink) -> Result<(), SimDiag> {
        if let Some(r) = &self.core.rounds {
            // Block 0 steps no further than the open round's end, and its points wait for the
            // round's merge. Asked past the end (a nested `Bus::advance`), it closes the round
            // there and opens the next on the grid, so the attached process gets its exact time.
            let t = r.ctl().st.t();
            if t >= r.end && t_end > t {
                self.close_round(sink)?;
                let end = self.core.rounds.as_ref().expect("in rounds").end_after(t);
                self.open_round(t, end, sink);
            }
            let r = self.core.rounds.as_mut().expect("in rounds");
            let end = r.end;
            let b = r.ctl_mut();
            let at = Instant::now();
            let step = b.step(t_end.min(end));
            if step.is_ok() {
                b.s.pin_hits(&mut self.core.hits);
            }
            self.stats.round_split.ctl_ns += at.elapsed().as_nanos() as u64;
            self.stats.round_split.ctl_steps += 1;
            return step;
        }
        self.deliver_rows(sink, false);
        let subscribed = !self.sub_pins.is_empty() || !self.sub_parts.is_empty();
        let trace = self.spec.trace;
        let trace_pins = trace && self.spec.trace_pins;
        let LiveSim { core: Core { s, st, hits, logic, .. }, taps, store, pending, interval, pin_i, part_p, stats, net_v, trace_values, sink_wall, .. } = self;
        st.step(s, t_end, &mut |sv: &Solver, a: &Accepted| {
            if subscribed || trace_pins {
                taps.currents(sv, a.xdot, a.t, pin_i);
            }
            let o = Outs { stats: &mut *stats, sink_wall: &mut *sink_wall, pending: &mut *pending, net_v: &mut *net_v, taps: &*taps, pin_i: &*pin_i, part_p: &mut *part_p, store: &mut *store, trace_values: &mut *trace_values, logic: &mut *logic, interval: Some(&mut *interval), subscribed, trace, trace_pins, batch: false };
            put(o, sink, a.t, &sv.x, a.event, a.coarse, a.chosen, None);
        })?;
        s.pin_hits(hits);
        stats.rescues.append(&mut s.rescues);
        Ok(())
    }

    /// `advance_to` in rounds: whole rounds while short of `target`, each ended on the round grid,
    /// at the next timed command or at a `Step`'s end; the budget, a publication and a firmware
    /// stop are looked at between rounds. Block 0 steps with the attached process as the whole
    /// circuit does without rounds; then the other stages run to where it stands, and the round's
    /// points go out merged. A posted merge still out when it returns stays out, so it runs
    /// beside the next advance's first round (between advances, the sinks and the whole circuit's
    /// state are a round behind the blocks); a failure collects it, so its points are delivered.
    fn advance_rounds(&mut self, target: f64, budget: Option<Duration>, start: Instant, sink: &mut dyn Sink) -> Result<bool, SimDiag> {
        let advanced = self.run_rounds(target, budget, start, sink);
        let mut collected = Ok(());
        if let Some(r) = &mut self.core.rounds {
            if advanced.is_err() {
                collected = r.collect_merge(&mut self.stats.round_split);
            }
            r.take_rows(&mut self.merged);
        }
        self.deliver_rows(sink, false);
        let moved = advanced?;
        collected?;
        Ok(moved)
    }

    fn run_rounds(&mut self, target: f64, budget: Option<Duration>, start: Instant, sink: &mut dyn Sink) -> Result<bool, SimDiag> {
        let target = self.stop_at().map_or(target, |u| u.min(target));
        while self.t() < target {
            // An edit needs the blocks at the same instant, but for one of block 0 alone
            // (`needs_settle`), and so does a plan going in that reshapes the rounds (made first: it
            // goes in here whatever it takes); one that keeps the cut does not, nor does a re-plan's
            // start: its snapshot is the rounds' marker, taken as the stages pass it (`start_plan`)
            let t = self.t();
            if self.core.rounds.as_ref().is_some_and(|r| r.behind()) {
                let edit = self.timeline.iter().skip(self.core.next_event).take_while(|e| e.0 <= t).any(|e| self.needs_settle(&e.1));
                let go_in = !edit && self.plan_lands() && {
                    self.wait_plan();
                    self.go_in_reshapes()
                };
                if edit || go_in {
                    if edit {
                        self.stats.edit_flushes += 1;
                    } else {
                        self.stats.go_in_flushes += 1;
                        // a plan going in before every stage passed its marker: the flush takes them there
                        self.stats.marker_fallbacks += u64::from(self.core.plan.as_ref().is_some_and(|p| p.whole.is_none()));
                    }
                    self.flush_rounds(sink);
                }
            }
            if self.breach_taken(sink)? {
                continue;
            }
            self.apply_due();
            let at = Instant::now();
            if self.at_round_start() {
                sink.started();
            }
            self.stats.round_split.round_start_ns += at.elapsed().as_nanos() as u64;
            if self.core.rounds.is_none() {
                return Ok(true);
            }
            self.take_anchor(sink);
            // a snapshot due with the stages behind is taken as they pass this round start, none
            // caught up for it
            if self.keeping.is_none() && self.core.rounds.as_ref().is_some_and(|r| r.behind()) && self.snapshot_due(start.elapsed()) {
                self.mark_keep();
            }
            let t0 = self.t();
            let end = self.round_end(t0);
            // the whole round is granted at its start: a streamed input short of its end plus the
            // process's span holds the run here (`run_live` waits)
            if self.starved(end) {
                break;
            }
            self.open_round(t0, end, sink);
            // the control block and the attached process to the round's end (a nested advance
            // may close rounds and leave a later one open)
            let mut stopped = false;
            loop {
                let end = self.core.rounds.as_ref().expect("in rounds").end;
                if self.t() >= end {
                    break;
                }
                let next = self.core.co.as_ref().map_or(f64::INFINITY, |c| c.dev.next());
                let ran = if next <= self.t() {
                    self.run_co(sink)?
                } else {
                    self.step_once(next.min(end), sink)?;
                    if self.t() == next || !self.core.hits.is_empty() {
                        self.run_co(sink)?
                    } else {
                        CoRun::Go
                    }
                };
                if ran == CoRun::Stopped {
                    stopped = true;
                    break;
                }
            }
            // a firmware stop ends the round where block 0 is
            self.close_round(sink)?;
            if self.breach_taken(sink)? {
                continue;
            }
            if stopped {
                self.flush_rounds(sink);
            }
            self.reprove_edits();
            if self.breach_taken(sink)? {
                continue;
            }
            if self.rest_check(sink) {
                break;
            }
            if stopped || self.publish_soon || budget.is_some_and(|b| start.elapsed() >= b) {
                break;
            }
        }
        // a paced run at the clock goes on with its next advance: the stages behind block 0 stay
        // posted and its merge out, as at a budget's end, rather than caught up on the worker at
        // every stop the clock makes (the more the run is ahead, the more stops); a hard stop (a
        // `Step`'s end, the take's end) catches them up, and so does a run that stops running
        // (`run_live`)
        let hard = self.stop_at().is_some_and(|u| self.t() >= u);
        let at_clock = self.paced && !hard && self.t() >= target;
        self.stats.clock_stops += u64::from(at_clock);
        if budget.is_none() || (self.t() >= target && !at_clock) {
            let flushes = self.stats.round_split.flushes;
            self.flush_rounds(sink);
            self.stats.stop_catchups += self.stats.round_split.flushes - flushes;
            // a breach the catch-up found goes back at once, and the run steps on to the target
            if self.breach_taken(sink)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A round from `t0` to `end` begins, its points carrying pin currents when the trace or a
    /// subscription takes them, its blocks told what the sinks hear at the sample rate
    /// (`Sink::heard`). O(sinks + voices) beside the round's own start.
    fn open_round(&mut self, t0: f64, end: f64, sink: &mut dyn Sink) {
        let at = Instant::now();
        let pins = self.spec.trace && self.spec.trace_pins || !self.sub_pins.is_empty() || !self.sub_parts.is_empty();
        let mut heard = std::mem::take(&mut self.heard);
        heard.clear();
        sink.heard(&mut heard);
        heard.sort_unstable();
        heard.dedup();
        let r = self.core.rounds.as_mut().expect("in rounds");
        r.hear(&heard);
        self.heard = heard;
        self.stats.round_split.open_heard_ns += at.elapsed().as_nanos() as u64;
        r.begin(t0, end, pins, self.helpers.as_ref(), &mut self.stats.round_split);
        r.take_rows(&mut self.merged);
        self.stats.round_split.open_ns += at.elapsed().as_nanos() as u64;
    }

    /// The plan on its way, made: its job run here if no core has started it, else waited for
    /// (`LiveStats::plan_waits`). The result stays with it for `at_round_start`.
    fn wait_plan(&mut self) {
        let Some(run) = self.plan_run.as_mut().filter(|r| r.back.is_none()) else { return };
        if let Some(job) = run.take() {
            job.run(false);
        }
        let got = match run.rx.try_recv() {
            Err(TryRecvError::Empty) => {
                let wait = Instant::now();
                let got = run.rx.recv().ok();
                let waited = wait.elapsed();
                self.stats.plan_waits += 1;
                self.stats.plan_wait_wall += waited;
                self.stats.plan_wait_max = self.stats.plan_wait_max.max(waited);
                got
            }
            got => got.ok(),
        };
        run.back = Some(got);
    }

    /// Whether the plan back from its way changes the rounds where it goes in, so that every
    /// block must stand at that instant first: a new cut, or the run leaving rounds. One that cuts
    /// the run as it runs (`Rounds::runs_cut`) changes no block, and a failed one changes nothing:
    /// neither needs the stages caught up. True for a plan not back. O(unknowns + instances).
    fn go_in_reshapes(&self) -> bool {
        let Some(r) = &self.core.rounds else { return true };
        match self.plan_run.as_ref().and_then(|run| run.back.as_ref()) {
            Some(Some(done)) => match &done.got {
                Ok(Some((plan, solvers))) => !r.runs_cut(plan, solvers),
                Ok(None) => true,
                Err(_) => false,
            },
            Some(None) => false,
            None => true,
        }
    }

    /// Whether the plan on its way needs the stages caught up at this round start: it goes in
    /// there (`plan_goes_in`).
    fn plan_lands(&mut self) -> bool {
        let t = self.t();
        self.plan_goes_in(t, false)
    }

    /// Whether the plan on its way goes in at round start `t`: at the round written for it
    /// (`go_ins`); with none written, in a heard run at the first round start at or after its
    /// time where it is back (one that reshapes the rounds, back while the stages are `behind` the
    /// check that would have caught them up, waits a round), and in any other run at its time.
    /// O(log go-ins).
    fn plan_goes_in(&mut self, t: f64, behind: bool) -> bool {
        let Some(p) = &self.core.plan else { return false };
        if t < p.at {
            return false;
        }
        if let Some(at) = self.go_in_written(p.t) {
            return t >= at;
        }
        if !self.spec.heard {
            return true;
        }
        // a plan whose snapshot the stages have not all passed yet is not back (`take_marked_plan`)
        let Some(run) = self.plan_run.as_mut() else { return false };
        run.due_round.get_or_insert(t);
        if run.back.is_none() {
            match run.rx.try_recv() {
                Ok(done) => run.back = Some(Some(done)),
                Err(TryRecvError::Disconnected) => run.back = Some(None),
                Err(TryRecvError::Empty) => {}
            }
        }
        run.back.is_some() && !behind
    }

    /// The round start written for the plan started at `from` (`go_ins`): infinite for one an edit
    /// called back before it went in. O(log go-ins).
    fn go_in_written(&self, from: f64) -> Option<f64> {
        let i = self.go_ins.partition_point(|g| g.0 < from);
        self.go_ins.get(i).filter(|g| g.0 == from).map(|g| g.1)
    }

    /// Writes where the plan started at `from` went in (`go_ins`). O(log go-ins), and O(go-ins
    /// after it) for one started before the newest written.
    fn write_go_in(&mut self, from: f64, at: f64) {
        let i = self.go_ins.partition_point(|g| g.0 < from);
        match self.go_ins.get_mut(i) {
            Some(g) if g.0 == from => g.1 = at,
            _ => self.go_ins.insert(i, (from, at)),
        }
    }

    /// The run's inputs change from `t` on: the go-in rounds written for plans started there or
    /// later (ahead of the run after it went back) are of another run, and go. O(go-ins after t).
    fn forget_go_ins(&mut self, t: f64) {
        let i = self.go_ins.partition_point(|g| g.0 < t);
        self.go_ins.truncate(i);
    }

    /// Where each plan went in, by the time it started (`Replan`): the round start it went in at,
    /// infinite for one an edit called back before it went in, and for a heard run's plan still on
    /// its way (in nowhere so far). Part of the run's timeline: a run made again from the same
    /// inputs and these (`follow`) puts each plan in at its round, and holds one still out. O(go-ins).
    pub fn go_ins(&self) -> Vec<(f64, f64)> {
        let mut go_ins = self.go_ins.clone();
        if let Some(p) = self.core.plan.as_ref().filter(|p| self.spec.heard && self.go_in_written(p.t).is_none()) {
            go_ins.insert(go_ins.partition_point(|g| g.0 < p.t), (p.t, f64::INFINITY));
        }
        go_ins
    }

    /// Makes this run again from a timeline (`timeline`, `go_ins`, `refusals` and `landings` of a
    /// run with the same circuit, spec and start): its timed commands go on this run's timeline at
    /// their times, each plan goes in at the round written for it, waited for there if it must, and
    /// each refusal and landing takes effect where it did: its own shadow rounds and chain trees,
    /// back at other rounds than the run's were, refuse and land nothing (`shadow_taken`,
    /// `chain_taken`).
    /// A run whose start tried a carried cut (`carried_cut`) tries that carry again, built and
    /// proved as it was (taken or refused there as it was), whatever this run's planner keeps; one
    /// that tried none carries none.
    pub fn follow(&mut self, timeline: &[(f64, LiveCmd)], go_ins: &[(f64, f64)], refusals: &[(f64, Vec<String>)], landings: &[(f64, Land)], carried: Option<&Arc<crate::analysis::round::Carry>>) {
        for (t, cmd) in timeline {
            self.schedule(*t, cmd.clone(), false, self.taken);
        }
        self.go_ins = go_ins.to_vec();
        self.refusals = refusals.to_vec();
        self.landings = landings.to_vec();
        self.carried = carried.cloned();
        self.followed = true;
    }

    /// The carry the run's start tried, if it tried one (`Planner::carried`), taken or refused:
    /// part of its timeline beside the go-in rounds, refusals and landings (`follow`).
    pub fn carried_cut(&self) -> Option<&Arc<crate::analysis::round::Carry>> {
        self.carried.as_ref()
    }

    /// The growth rule's and the shadow rounds' refusals, by the time each takes effect from
    /// (`breach_taken`, `shadow_taken`): part of the run's timeline, as the go-in rounds are. A run
    /// made again from them (`follow`) refuses the same cuts at the same places, and never
    /// breaches there.
    pub fn refusals(&self) -> &[(f64, Vec<String>)] {
        &self.refusals
    }

    /// The chain shadow's landings, by the time each takes effect from (`chain_taken`): part of the
    /// run's timeline beside the refusals. A run made again from them (`follow`) lands the same
    /// chains at the same places.
    pub fn landings(&self) -> &[(f64, Land)] {
        &self.landings
    }

    /// The open round ends where block 0 stands. Collect the stages and merge the points
    /// available from every block. With the merge on the helpers, the round before's merged rows
    /// go to the sinks and the trace while the helpers still run this round's later stages: the
    /// worker's put overlaps their packs instead of following them.
    fn close_round(&mut self, sink: &mut dyn Sink) -> Result<(), SimDiag> {
        let end = self.t();
        let posts = self.posts_merge();
        let r = self.core.rounds.as_mut().expect("in rounds");
        let mut count = PackCount::default();
        let mut ran = r.close_ctl(end, &mut count, &mut self.stats.round_split);
        if ran.is_ok() {
            if posts {
                r.take_rows(&mut self.merged);
                self.deliver_rows(sink, true);
            }
            let r = self.core.rounds.as_mut().expect("in rounds");
            ran = r.close_stages(self.helpers.as_ref(), &mut count, &mut self.stats.round_split);
        }
        let r = self.core.rounds.as_mut().expect("in rounds");
        r.take_rows(&mut self.merged);
        self.stats.packs_helped += count.helped;
        self.stats.packs_claimed += count.claimed;
        // a breached round's points go nowhere: the growth rule takes the run back (`breach_taken`);
        // a block that failed on a value past the bar (a reader stepping on a not-a-number) failed
        // of the breach
        if self.core.rounds.as_ref().is_some_and(|r| r.breach.is_some()) {
            return Ok(());
        }
        ran?;
        self.merge_round(sink);
        Ok(())
    }

    /// Bring all stages to block 0's round end before reading or changing the blocks together.
    /// A failure is reported at the next round's close.
    fn settle_rounds(&mut self) {
        let Some(r) = &mut self.core.rounds else { return };
        if !r.behind() {
            return;
        }
        let mut count = PackCount::default();
        let ran = r.catch_up(self.helpers.as_ref(), &mut count, &mut self.stats.round_split);
        r.take_rows(&mut self.merged);
        self.stats.packs_helped += count.helped;
        self.stats.packs_claimed += count.claimed;
        if let Err(d) = ran {
            r.failed.get_or_insert(d);
        }
    }

    /// Catch up the stages and send their remaining merged points to the sink.
    fn flush_rounds(&mut self, sink: &mut dyn Sink) {
        if self.core.rounds.as_ref().is_some_and(|r| r.behind()) {
            let at = Instant::now();
            self.settle_rounds();
            // a breached round's points go nowhere (`breach_taken`)
            if self.core.rounds.as_ref().is_some_and(|r| r.merge_home() && r.breach.is_none()) {
                self.merge_points(sink);
            }
            self.stats.round_split.flush_ns += at.elapsed().as_nanos() as u64;
            self.stats.round_split.flushes += 1;
        }
    }

    /// Whether a round's merge is posted to the helpers (`LiveSpec::merge_job`, a pipeline with
    /// helper cores) rather than run inline.
    fn posts_merge(&self) -> bool {
        self.spec.merge_job && self.core.rounds.as_ref().is_some_and(|r| r.pipeline) && self.helpers.as_ref().is_some_and(|h| h.count > 0)
    }

    /// Post the merge after delivering the preceding rows when the pipeline has helpers.
    /// An inline merge delivers its points here. Both paths count the round and its rescues.
    fn merge_round(&mut self, sink: &mut dyn Sink) {
        if self.posts_merge() {
            // the rows are out already (`close_round`), unless the round had none to hand back
            if !self.merged.t.is_empty() {
                self.deliver_rows(sink, true);
            }
            // the merged stream may stand at a snapshot's round end now: it is taken before the merge
            // goes on past it
            self.complete_keep(&*sink);
            let r = self.core.rounds.as_mut().expect("in rounds");
            let at = Instant::now();
            r.post_merge(self.helpers.as_ref().expect("the pipeline has helpers"), &self.core.s.x, &self.pin_i);
            self.stats.round_split.post_merge_ns += at.elapsed().as_nanos() as u64;
        } else {
            let end_at = self.merge_points(sink);
            self.stats.round_split.end_merge_ns += end_at.elapsed().as_nanos() as u64;
        }
        let r = self.core.rounds.as_mut().expect("in rounds");
        r.n += 1;
        for b in r.runs_mut() {
            self.stats.rescues.append(&mut b.s.rescues);
        }
        self.stats.rounds += 1;
    }

    /// The interval back from a helper's `IntervalJob`, its rows' buffer kept for the next round; the
    /// worker runs the job itself when no helper has started it. Counted in the delivery's time.
    fn interval_home(&mut self) {
        let Some(flight) = self.interval_out.take() else { return };
        let at = Instant::now();
        let mut job = flight.land();
        self.interval = job.interval;
        job.rows.clear();
        self.spare_rows = job.rows;
        self.interval_spare = (job.net_v, job.part_p);
        let spent = at.elapsed().as_nanos() as u64;
        self.stats.round_split.put_ns += spent;
        self.stats.round_split.put_interval_ns += spent;
    }

    /// Deliver a posted merge in order. Its last row is the whole circuit's state while the
    /// rounds that produced it are still installed. With `post` and helper cores, a helper takes the
    /// rows' interval share while block 0 steps on (`IntervalJob`): the sinks and the trace get
    /// them here, in order, and the interval before its next reader.
    fn deliver_rows(&mut self, sink: &mut dyn Sink, post: bool) {
        self.interval_home();
        let n = self.merged.t.len();
        if n == 0 {
            return;
        }
        let at = Instant::now();
        let post = post && self.helpers.as_ref().is_some_and(|h| h.count > 0);
        let subscribed = !self.sub_pins.is_empty() || !self.sub_parts.is_empty();
        let trace = self.spec.trace;
        let trace_pins = trace && self.spec.trace_pins;
        self.delivered = self.merged.t[n - 1];
        let LiveSim { core: Core { s, rounds, logic, .. }, merged, spare_rows, interval_out, interval_spare, helpers, taps, store, pending, interval, pin_i, part_p, stats, net_v, trace_values, sink_wall, .. } = self;
        let (nx, np) = (merged.x.len() / n, merged.pin_i.len() / n);
        // the rows go to the sinks as one burst, timed as one (`sink_wall`: the sinks' calls and
        // what goes with them)
        let sinks_at = Instant::now();
        sink.rows_begin();
        for (i, (&t, &flags)) in merged.t.iter().zip(&merged.flags).enumerate() {
            let x = &merged.x[i * nx..(i + 1) * nx];
            let pins = &merged.pin_i[i * np..(i + 1) * np];
            let o = Outs { stats: &mut *stats, sink_wall: &mut *sink_wall, pending: &mut *pending, net_v: &mut *net_v, taps: &*taps, pin_i: pins, part_p: &mut *part_p, store: &mut *store, trace_values: &mut *trace_values, logic: &mut *logic, interval: (!post).then_some(&mut *interval), subscribed, trace, trace_pins, batch: true };
            // a merged step is a piece of each block's own step, cut at the other blocks' points:
            // no one step error control vouches for (a voice's would need its own block's)
            put(o, sink, t, x, flags & 1 != 0, flags & 2 != 0, false, None);
        }
        sink.rows_end();
        let spent = sinks_at.elapsed();
        *sink_wall += spent;
        stats.sink_wall += spent;
        if trace {
            // the traced values of all the rows, signal by signal: each net's, then each pin's
            let trace_at = Instant::now();
            let (nodes, xs, ps) = (&taps.map.nodes, &merged.x, &merged.pin_i);
            let nets = nodes.len();
            let y = |r: usize, k: usize| if k < nets { xs[r * nx + nodes[k]] } else { ps[r * np + k - nets] };
            // each value read once, into the store's tail and the delta's pending values
            let at = pending.time.len();
            pending.time.extend_from_slice(&merged.t);
            store.push_rows_into(&merged.t, &y, &mut pending.closed, &mut pending.values, at);
            stats.round_split.put_trace_ns += trace_at.elapsed().as_nanos() as u64;
        }
        if rounds.as_ref().is_some_and(|r| r.id == merged.rounds_id) {
            s.x.copy_from_slice(&merged.x[(n - 1) * nx..]);
            pin_i.copy_from_slice(&merged.pin_i[(n - 1) * np..]);
        }
        if post {
            let job = IntervalJob {
                interval: std::mem::take(interval),
                rows: std::mem::replace(merged, std::mem::take(spare_rows)),
                map: taps.map.clone(),
                net_v: zeroed(&mut interval_spare.0, net_v.len()),
                part_p: zeroed(&mut interval_spare.1, part_p.len()),
                subscribed,
            };
            *interval_out = Some(IntervalFlight::post(job, helpers.as_ref().expect("posted with helpers")));
        } else {
            merged.clear();
        }
        stats.round_split.merged_points += n as u64;
        stats.round_split.put_ns += at.elapsed().as_nanos() as u64;
    }

    /// Deliver points up to the latest time every block has reached. Return the instant the
    /// last point was delivered so a round close can account for `end_merge`.
    fn merge_points(&mut self, sink: &mut dyn Sink) -> Instant {
        self.deliver_rows(sink, false);
        let subscribed = !self.sub_pins.is_empty() || !self.sub_parts.is_empty();
        let trace = self.spec.trace;
        let trace_pins = trace && self.spec.trace_pins;
        let LiveSim { core: Core { s, rounds, logic, .. }, taps, store, pending, interval, pin_i, part_p, stats, net_v, trace_values, sink_wall, delivered, .. } = self;
        let r = rounds.as_mut().expect("in rounds");
        let mut next_at = Instant::now();
        while let Some((t, flags)) = r.next_point(&mut s.x, pin_i, &mut stats.round_split) {
            *delivered = t;
            let o = Outs { stats: &mut *stats, sink_wall: &mut *sink_wall, pending: &mut *pending, net_v: &mut *net_v, taps: &*taps, pin_i: &*pin_i, part_p: &mut *part_p, store: &mut *store, trace_values: &mut *trace_values, logic: &mut *logic, interval: Some(&mut *interval), subscribed, trace, trace_pins, batch: false };
            put(o, sink, t, &s.x, flags & 1 != 0, flags & 2 != 0, false, Some(&mut next_at));
        }
        let end_at = Instant::now();
        stats.round_split.next_point_ns += (end_at - next_at).as_nanos() as u64;
        r.end_merge(&mut stats.round_split);
        // a snapshot whose rounds stand at its round end now is taken, and the merge goes on past it
        if self.complete_keep(&*sink) {
            return self.merge_points(sink);
        }
        end_at
    }

    /// Runs the attached process at the present instant; its hits are delivered and cleared, and
    /// its watches register again if they changed. A stop pauses the run and asks for a
    /// publication at once.
    fn run_co(&mut self, sink: &mut dyn Sink) -> Result<CoRun, SimDiag> {
        let Some(mut co) = self.core.co.take() else { return Ok(CoRun::Go) };
        self.stats.co_runs += 1;
        let start = Instant::now();
        let mut nested = Duration::ZERO;
        let r = co.dev.run(&mut LiveBus { sim: self, pins: &co.pins, sink, nested: &mut nested });
        let wall = start.elapsed().saturating_sub(nested);
        self.co_wall += wall;
        self.stats.co_wall += wall;
        self.core.hits.clear();
        self.core.co = Some(co);
        self.take_co_logic(false);
        self.take_co_audio(sink);
        self.sync_watches();
        if r.as_ref().is_ok_and(|&ran| ran == CoRun::Stopped) {
            self.stats.co_stops += 1;
            self.playing = false;
            self.co_step = false;
            self.publish_soon = true;
            // the step cap changes for every block: the stages first stand where block 0's last
            // round ended, as on the worker alone
            self.settle_rounds();
            self.sync_step_cap();
        }
        r
    }

    /// Registers the attached process's watches with the GPIO bank when their generation moved.
    fn sync_watches(&mut self) {
        let Some(co) = &mut self.core.co else { return };
        let generation = co.dev.watch_gen();
        if co.watch_gen == Some(generation) {
            return;
        }
        co.watch_gen = Some(generation);
        let s = ctl_of(&mut self.core.rounds, &mut self.core.s, &mut self.core.st).0;
        s.clear_watches();
        for w in co.dev.watches() {
            if let Some(gpio) = co.pins.get(w.pin as usize).and_then(|p| p.gpio) {
                s.pin_watch(gpio, w.id, w.level, w.edge);
            }
        }
    }

    /// Whether a `Stop` was held behind a `Step` and is now due.
    fn stop_due(&self) -> bool {
        self.step_until.is_none() && matches!(self.held.front(), Some((_, LiveCmd::Stop)))
    }

    /// The values to publish now; `ratio` is what the host measured. Starts the next interval.
    pub fn values(&mut self, ratio: f64) -> LiveValues {
        self.interval_home();
        // an attached process's traced nets are complete up to now: its nets off the circuit read
        // their record below
        self.take_co_logic(true);
        self.publish_soon = false;
        self.stats.published += 1;
        let stepped_with = self.claims();
        let t = self.t();
        for (v, &u) in self.net_v.iter_mut().zip(&self.taps.map.nodes) {
            *v = self.core.s.x[u];
        }
        let mut pin_i = vec![0.0; self.pin_i.len()];
        let mut part_p = vec![0.0; self.part_p.len()];
        match &mut self.core.rounds {
            Some(r) => r.currents_now(&mut pin_i),
            None => {
                let xdot = self.core.st.hist.newest().xdot.clone();
                self.taps.currents(&self.core.s, &xdot, t, &mut pin_i);
            }
        }
        self.taps.map.powers(&self.core.s.x, &pin_i, &mut part_p);
        let (since, mut net_mean, mut net_min, mut net_max, pins_mean, parts_mean) = self.interval.take();
        let mut frames = std::mem::take(&mut self.interval.frames);
        // the process's nets off the circuit read their logic record at its rails, where the
        // circuit has no unknown for them: the instant, the publication's window and each frame
        for r in &self.logic_rows {
            let Some(trace) = self.store.logic(r.id) else { continue };
            let (vss, vdd) = (self.core.s.x[r.vss], self.core.s.x[r.vdd]);
            if let (Some(v), Some((_, _, now))) = (self.net_v.get_mut(r.row), logic_volts(trace, t, t, vss, vdd)) {
                *v = now;
            }
            let put = |a: f64, b: f64, mean: &mut [f32], min: &mut [f32], max: &mut [f32]| {
                if let (Some(m), Some(lo), Some(hi), Some((v, l, h))) = (mean.get_mut(r.row), min.get_mut(r.row), max.get_mut(r.row), logic_volts(trace, a, b, vss, vdd)) {
                    (*m, *lo, *hi) = (v as f32, l as f32, h as f32);
                }
            };
            put(since, t, &mut net_mean, &mut net_min, &mut net_max);
            for f in &mut frames {
                put(f.since, f.t, &mut f.net_mean, &mut f.net_min, &mut f.net_max);
            }
        }
        let share = |part: Duration| if self.step_wall.is_zero() { 0.0 } else { (part.as_secs_f64() / self.step_wall.as_secs_f64()).min(1.0) as f32 };
        let (co_share, sink_share) = (share(self.co_wall), share(self.sink_wall));
        (self.step_wall, self.co_wall, self.sink_wall) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
        let keep = |all: Vec<f32>, sub: &[u32]| -> Vec<f32> {
            let mut out = vec![0.0; all.len()];
            for &k in sub {
                out[k as usize] = all[k as usize];
            }
            out
        };
        LiveValues {
            t,
            generation: self.generation,
            ratio,
            steady: self.core.steady,
            net_v: self.net_v.iter().map(|&v| v as f32).collect(),
            branch_i: pin_i.iter().map(|&v| v as f32).collect(),
            part_p: part_p.iter().map(|&v| v as f32).collect(),
            since,
            net_mean,
            net_min,
            net_max,
            pin_mean: keep(pins_mean, &self.sub_pins),
            part_mean: keep(parts_mean, &self.sub_parts),
            rescues: self.stats.rescues.len() as u64,
            rescue: self.stats.rescues.last().cloned(),
            stepping: self.co_step,
            running: self.running(),
            can_restart: self.ring.kept.first().is_some_and(|kept| kept.snap.t == 0.0),
            co: if self.spec.co_state { self.core.co.as_mut().and_then(|co| co.dev.state(t)) } else { None },
            co_share,
            sink_share,
            res: self.spec.res,
            grid: self.interval.grid,
            frames,
            worker_split: self.worker_split.clone(),
            plan_error: self.plan_error_text.clone(),
            held: self.hold_seen.take(),
            preparing: self.preparing(),
            in_rounds: self.core.rounds.is_some(),
            started: std::mem::take(&mut self.started),
            stepped_with,
        }
    }

    /// One command `run_live` took, applied: false ends the run ([`Self::apply`]). It is counted
    /// once applied, so a timed one it puts on the timeline keeps the count before it there.
    fn take(&mut self, cmd: LiveCmd) -> bool {
        let go = self.apply(cmd);
        self.taken += 1;
        go
    }

    /// What a publication built now counts of the commands taken (`LiveValues::stepped_with`): those
    /// taken when the run last stepped (`stepped_with`), but from the first one a value it carries
    /// may not have yet. It claims a command only if every value it carries at its instant (the
    /// nets, the pin currents and the part powers made of them, the process's state) was produced
    /// past the instant the command applied at (`produced`): a value command counts once it has
    /// applied (`unseen`) at an instant before every one of those values', never while it is held
    /// behind a `Step` or waits on the timeline, whatever its time. A boundary at or past a
    /// command's time applies it as the next step begins, so the values at that boundary are the
    /// run's before it. One rule for every path the values take: a whole step; rounds, a stage
    /// behind block 0 giving its currents from where it stands and the merge the nets; a plan that
    /// goes in at an edit's instant, its blocks starting from the run's state there, made before
    /// the edit reached it; a run sent back before an edit. The interval's means are not values at
    /// its instant: they are the window since the last publication, which a host blends over
    /// publications. O(the commands held, on the timeline and unseen, and the blocks).
    fn claims(&mut self) -> u64 {
        let produced = self.produced();
        self.unseen.retain(|e| e.0 >= produced);
        let held = self.held.iter().map(|h| h.0);
        let pending = self.timeline_taken[self.core.next_event..].iter().copied();
        let unseen = self.unseen.iter().map(|e| e.1);
        held.chain(pending).chain(unseen).fold(self.stepped_with, u64::min)
    }

    /// The earliest instant the values a publication carries at its instant were produced at: the
    /// run's time on one stepper; in rounds the earliest of each block's newest point (its pin
    /// currents) and of the merge's (the nets), less what the rounds read late (`Rounds::lag`): a
    /// block reading a coupling late steps with what another block exported that much before.
    /// O(the blocks and couplings).
    fn produced(&self) -> f64 {
        match &self.core.rounds {
            Some(r) => r.stepped_to().map_or(f64::NEG_INFINITY, |t| t.min(self.delivered) - r.lag()),
            None => self.t(),
        }
    }

    /// The points recorded since the last delta that went out, as a delta; None when there are none.
    pub fn take_delta(&mut self) -> Option<TraceDelta> {
        if let Some(d) = self.pending.ready.pop_front() {
            return Some(d);
        }
        // an attached process's traced nets are complete up to the delta's end
        self.take_co_logic(true);
        self.finish_delta()
    }

    /// What is pending, as a delta; None when nothing is.
    fn finish_delta(&mut self) -> Option<TraceDelta> {
        let p = &mut self.pending;
        // a page alone goes out too: a paused run answers a view's zoom; so does a rewind
        if p.time.is_empty() && p.marks.is_empty() && p.logic_pages.is_empty() && p.rewind.is_none() {
            return None;
        }
        // a signal written in this interval has a value at every point, a gap where it was not
        let n = p.time.len();
        let d = TraceDelta {
            generation: self.generation,
            from: p.from,
            time: std::mem::take(&mut p.time).into(),
            values: p.values.iter_mut().map(|v| {
                if !v.is_empty() {
                    fill_gap(v, n);
                }
                Arc::from(std::mem::take(v))
            }).collect(),
            closed: std::mem::take(&mut p.closed),
            marks: std::mem::take(&mut p.marks),
            logic: p.logic.iter_mut().filter(|(_, found)| !found.is_empty()).map(|(id, found)| (*id, Arc::from(std::mem::take(found)))).collect(),
            logic_closed: std::mem::take(&mut p.logic_closed),
            logic_pages: std::mem::take(&mut p.logic_pages),
            folds: std::mem::take(&mut p.folds),
            rewind: p.rewind.take(),
        };
        p.from += d.time.len();
        Some(d)
    }

    /// Puts back a delta the outlet refused; the next one starts where it started.
    pub fn restore_delta(&mut self, d: TraceDelta) {
        let p = &mut self.pending;
        if d.generation != self.generation {
            return;
        }
        // one finished before a rewind, or carrying one, goes out again whole and first
        if d.rewind.is_some() || !p.ready.is_empty() {
            p.ready.push_front(d);
            return;
        }
        p.from = d.from;
        let mut time = d.time.to_vec();
        time.append(&mut p.time);
        p.time = time;
        for (v, dv) in p.values.iter_mut().zip(&d.values) {
            let mut w = dv.to_vec();
            // one the delta has no point of but written since: a gap over the delta first
            if !v.is_empty() {
                fill_gap(&mut w, d.time.len());
            }
            w.append(v);
            *v = w;
        }
        let mut closed = d.closed;
        closed.append(&mut p.closed);
        p.closed = closed;
        let mut marks = d.marks;
        marks.append(&mut p.marks);
        p.marks = marks;
        for (id, entries) in d.logic.iter() {
            if let Some((_, found)) = p.logic.iter_mut().find(|(of, _)| of == id) {
                let mut all = entries.to_vec();
                all.append(found);
                *found = all;
            }
        }
        let mut logic_closed = d.logic_closed;
        logic_closed.append(&mut p.logic_closed);
        p.logic_closed = logic_closed;
        let mut logic_pages = d.logic_pages;
        logic_pages.append(&mut p.logic_pages);
        p.logic_pages = logic_pages;
        let mut folds = d.folds;
        folds.append(&mut p.folds);
        p.folds = folds;
    }

    /// One offer of the pending points. False when the outlet was full (the points stay).
    fn send_delta(&mut self, out: &mut dyn TraceOut) -> bool {
        let Some(d) = self.take_delta() else { return true };
        match out.offer(d, &self.spec.keep()) {
            Ok(()) => {
                self.stats.deltas += 1;
                true
            }
            Err(d) => {
                self.stats.full += 1;
                self.restore_delta(d);
                false
            }
        }
    }
}

/// Where a live run's trace deltas go. `keep` is what the run's record keeps (`LiveSpec::keep`).
pub trait TraceOut {
    /// Takes `d`, or hands it back when full (its points stay pending for the next offer).
    fn offer(&mut self, d: TraceDelta, keep: &Keep) -> Result<(), TraceDelta>;
}

/// A bounded channel: the consumer applies the deltas to its own store.
impl TraceOut for SyncSender<TraceDelta> {
    fn offer(&mut self, d: TraceDelta, _keep: &Keep) -> Result<(), TraceDelta> {
        match self.try_send(d) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(d)) => Err(d),
            // no consumer: nothing to keep the points for
            Err(TrySendError::Disconnected(_)) => Ok(()),
        }
    }
}

/// The consumer's store merged on the run's thread and published whole; never full.
impl TraceOut for TracePublisher {
    fn offer(&mut self, d: TraceDelta, keep: &Keep) -> Result<(), TraceDelta> {
        self.publish(d, keep);
        Ok(())
    }
}

/// The live loop: runs `sim` until `Stop` or until the command channel closes, then returns it
/// with any points still pending (`take_delta`). The worker may block on `cmds` while paused,
/// settled or ahead of its target time; it never blocks on `values` or `traces`. `wake` is called
/// after each publication of values or a trace delta. A solver failure (a time step too small)
/// ends the loop with its diagnostic after a last publication.
/// The wall of running `run_live` sums into one window before it reports where it went
/// (`LiveValues::worker_split`).
pub const SPLIT_EVERY: Duration = Duration::from_secs(5);

/// `run_live`'s window of running: its start, the run's time and counts then, and the wall of the
/// loop's parts outside the steps. A wait (paused, settled, held) is no part of a window.
struct LoopSplit {
    since: Instant,
    t0: f64,
    stats: LiveStats,
    blocks: usize,
    advance: Duration,
    snapshots: Duration,
    values: Duration,
    traces: Duration,
    pacing: Duration,
    windows: u64,
}

impl LoopSplit {
    fn new(sim: &LiveSim) -> LoopSplit {
        LoopSplit {
            since: Instant::now(), t0: sim.t(), stats: sim.stats.clone(), blocks: sim.blocks(), advance: Duration::ZERO, snapshots: Duration::ZERO,
            values: Duration::ZERO, traces: Duration::ZERO, pacing: Duration::ZERO, windows: 0,
        }
    }

    /// A new window from now (after a wait, a rewind or a report).
    fn restart(&mut self, sim: &LiveSim) {
        let windows = self.windows;
        *self = LoopSplit { windows, ..LoopSplit::new(sim) };
    }

    /// The window's report once it spans `SPLIT_EVERY` with the same blocks, into `sim.worker_split`.
    /// A few dozen subtractions and one line, every few seconds.
    fn report_due(&mut self, sim: &mut LiveSim, now: Instant) {
        let wall = now - self.since;
        if wall < SPLIT_EVERY {
            return;
        }
        if sim.blocks() == self.blocks {
            self.windows += 1;
            let line = self.line(sim, wall);
            sim.worker_split = Some((self.windows, Arc::from(line)));
        }
        self.restart(sim);
    }

    fn line(&self, sim: &LiveSim, wall: Duration) -> String {
        let (a, b) = (&self.stats, &sim.stats);
        let w = wall.as_secs_f64();
        let pct = |d: Duration| 100.0 * d.as_secs_f64() / w;
        let step = b.step_wall.saturating_sub(a.step_wall);
        let (co, sinks) = (b.co_wall.saturating_sub(a.co_wall), b.sink_wall.saturating_sub(a.sink_wall));
        let solver = step.saturating_sub(co).saturating_sub(sinks);
        let store = self.advance.saturating_sub(step);
        let other = wall.saturating_sub(self.advance + self.snapshots + self.values + self.traces + self.pacing);
        let dt = sim.t() - self.t0;
        let mut line = format!("worker over {w:.1} s: x{:.3} ({:.3} s simulated, {} points); solver {:.1} %, firmware {:.1} %, sinks {:.1} %, store {:.1} %, snapshots {:.1} %, values {:.1} %, traces {:.1} %, pacing {:.1} %, other {:.1} %",
            dt / w, dt, b.points - a.points, pct(solver), pct(co), pct(sinks), pct(store), pct(self.snapshots), pct(self.values), pct(self.traces), pct(self.pacing), pct(other));
        let rounds = b.rounds - a.rounds;
        if rounds > 0 {
            let (r, q) = (&a.round_split, &b.round_split);
            let us = |x: u64, y: u64| (y - x) as f64 / 1e3 / rounds as f64;
            let wakes = q.wake_n - r.wake_n;
            // the solver's wall a round less every item named: what the line does not yet say (a
            // flush catches the stages up and merges, so its wall is in the items it runs)
            let named = [(r.ctl_ns, q.ctl_ns), (r.round_start_ns, q.round_start_ns), (r.export_ns, q.export_ns), (r.feed_ns, q.feed_ns), (r.pack_ns, q.pack_ns),
                (r.own_ns, q.own_ns), (r.claimed_ns, q.claimed_ns), (r.wait_ns, q.wait_ns), (r.stage_rest_ns, q.stage_rest_ns), (r.next_point_ns, q.next_point_ns),
                (r.put_ns, q.put_ns), (r.end_merge_ns, q.end_merge_ns), (r.open_ns, q.open_ns), (r.post_merge_ns, q.post_merge_ns), (r.merge_wait_ns, q.merge_wait_ns)].iter().map(|&(x, y)| us(x, y)).sum::<f64>();
            let unnamed = step.saturating_sub(co).as_secs_f64() * 1e6 / rounds as f64 - named;
            line += &format!("; in rounds, {rounds} rounds on {} blocks, us a round: block 0 {:.1} ({:.1} steps), round start {:.1}, round open {:.1} (heard {:.1}, stages behind {:.1}, of it posts {:.1}), exports {:.1}, feeds {:.1}, packing {:.1}, own packs {:.1}, claimed {:.1}, wait {:.1}, stage rest {:.1}, next point {:.1}, put {:.1} (trace {:.1}, interval {:.1}), merge post {:.1}, merge wait {:.1} ({} of {} merges run here), end merge {:.1}, flushes {:.1} (in the items above), unnamed {unnamed:.1}; packs {} on helpers, {} taken back; {wakes} helper wake-ups, mean {:.1} us (max {:.1} us over the run); helper packs {:.1} us a round",
                self.blocks, us(r.ctl_ns, q.ctl_ns), (q.ctl_steps - r.ctl_steps) as f64 / rounds as f64, us(r.round_start_ns, q.round_start_ns), us(r.open_ns, q.open_ns),
                us(r.open_heard_ns, q.open_heard_ns), us(r.open_behind_ns, q.open_behind_ns), us(r.open_post_ns, q.open_post_ns), us(r.export_ns, q.export_ns),
                us(r.feed_ns, q.feed_ns), us(r.pack_ns, q.pack_ns), us(r.own_ns, q.own_ns), us(r.claimed_ns, q.claimed_ns), us(r.wait_ns, q.wait_ns), us(r.stage_rest_ns, q.stage_rest_ns),
                us(r.next_point_ns, q.next_point_ns), us(r.put_ns, q.put_ns), us(r.put_trace_ns, q.put_trace_ns), us(r.put_interval_ns, q.put_interval_ns),
                us(r.post_merge_ns, q.post_merge_ns), us(r.merge_wait_ns, q.merge_wait_ns), q.merge_claimed - r.merge_claimed, q.merge_jobs - r.merge_jobs,
                us(r.end_merge_ns, q.end_merge_ns), us(r.flush_ns, q.flush_ns),
                b.packs_helped - a.packs_helped, b.packs_claimed - a.packs_claimed, (q.wake_ns - r.wake_ns) as f64 / 1e3 / wakes.max(1) as f64, q.wake_max_ns as f64 / 1e3,
                us(r.helper_run_ns, q.helper_run_ns));
            line += &format!("; checks: {} rounds run again, {} by the worker, {} replaced, {} stale, {} thinned ({} could start at the last round); {} trees, {} skipped",
                q.shadows - r.shadows, q.shadow_floor - r.shadow_floor, q.shadow_replaced - r.shadow_replaced, q.shadow_stale - r.shadow_stale, q.shadow_thinned - r.shadow_thinned, q.checkers,
                q.chain_trees - r.chain_trees, q.chain_skipped - r.chain_skipped);
        }
        let waits = b.plan_waits - a.plan_waits;
        if waits > 0 {
            line += &format!("; {waits} plan waits, {:.1} ms (the run's longest {:.1} ms)",
                (b.plan_wait_wall - a.plan_wait_wall).as_secs_f64() * 1e3, b.plan_wait_max.as_secs_f64() * 1e3);
        }
        let late = b.plans_late - a.plans_late;
        if late > 0 {
            line += &format!("; {late} plans in late (the run's latest by {:.1} ms simulated)", b.plan_late_max * 1e3);
        }
        line
    }
}

pub fn run_live(mut sim: LiveSim, cmds: &Receiver<LiveCmd>, values: &Latest<LiveValues>, traces: &mut dyn TraceOut, sink: &mut dyn Sink, wake: &dyn Fn()) -> Result<LiveSim, SimDiag> {
    sink.begin(&sim.core.s.unknowns);
    sink.sample_period(sim.period);
    let mut split = LoopSplit::new(&sim);
    let mut anchor = (Instant::now(), sim.t());
    let mut last_values = Instant::now() - sim.spec.values_every;
    let mut last_trace = Instant::now();
    let mut trace_wait = sim.spec.trace_every;
    let mut published_t = sim.t();
    let mut generation = sim.generation;
    let mut holding = false;
    loop {
        let was_running = sim.running();
        let rate = sim.spec.rate;
        loop {
            match cmds.try_recv() {
                Ok(cmd) => {
                    if !sim.take(cmd) {
                        return Ok(sim);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(sim),
            }
        }
        if sim.stop_due() {
            return Ok(sim);
        }
        // a run paced in real time that a listener hears as it is made raises its worker, so the
        // box's other work does not take the worker's core from under what is heard. Paced by its
        // rate, or by its player's ring (`LiveSpec::heard` with the run itself unpaced: the host's
        // Listen tap, as Fab's Record live, whose worker was never raised); a listened run nothing
        // paces (a render flat out) stays as it is, and a pause or a closed listener drops it back
        sim.hear(sim.running() && (sim.spec.rate.is_finite() || sim.spec.heard) && sink.listened());
        if sim.generation != generation {
            generation = sim.generation;
            sink.begin(&sim.core.s.unknowns);
            sink.sample_period(sim.period);
        }
        // an edit sent the run back: the sinks go back with it, and the clock counts from there
        let rewound = sim.rewind_sinks(sink);
        if rewound {
            published_t = sim.t();
        }
        let plan = sim.holds_for_plan();
        let started = sim.tell_start(sink);
        if started || rewound || !was_running && sim.running() || sim.spec.rate != rate {
            anchor = (Instant::now(), sim.t());
            split.restart(&sim);
        }
        if started {
            // The first interval of playback counts from the warm's point, even when the board
            // starts whole without holding. Publish there before either pacing or asking the sinks.
            (last_values, published_t) = anchor;
            values.publish(Arc::new(sim.values(0.0)));
            wake();
            holding = false;
            continue;
        }
        // a streamed input short of the next step's reach (`crate::input`), or a sink (a recording
        // a window past what is heard), holds the run; every sink is asked, so each sends what waited
        let hold = if plan {
            Some(HoldKind::Plan)
        } else if sim.running() && !sim.core.steady {
            let sinks = !sim.preparing() && sink.holds();
            if sim.input_starved() { Some(HoldKind::Input) } else { sinks.then_some(HoldKind::Other) }
        } else {
            None
        };
        if let Some(kind) = hold {
            // what is done goes out, then the run waits for a command, asking again every few
            // milliseconds; the publication after the hold says so too
            if kind == HoldKind::Input || sim.hold_seen.is_none() {
                sim.hold_seen = Some(kind);
            }
            if !holding {
                holding = true;
                let deltas = sim.stats.deltas;
                sim.send_delta(traces);
                if sim.stats.deltas != deltas {
                    wake();
                }
                values.publish(Arc::new(sim.values(0.0)));
                wake();
            }
            match cmds.recv_timeout(HOLD_POLL) {
                Ok(cmd) => {
                    if !sim.take(cmd) {
                        return Ok(sim);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(sim),
            }
            continue;
        }
        if holding {
            // the wait is no interval of the run
            holding = false;
            (anchor, last_values) = ((Instant::now(), sim.t()), Instant::now());
            split.restart(&sim);
        }
        if !sim.running() || sim.core.steady {
            // the last points first: a host that reads these values has them in the channel (the
            // stages a paced stop left behind block 0 caught up)
            sim.flush_rounds(sink);
            let deltas = sim.stats.deltas;
            let mut sent = sim.send_delta(traces);
            if sim.stats.deltas != deltas {
                wake();
            }
            // and the sinks have the run's rest before the host reads it: a take is not at rest
            // until its recorders have taped its end (an `ended` pads the last chunk), so a Listen
            // asked on the first frame that reads the rest finds the take whole
            if sim.core.steady {
                sink.steady(sim.t(), &sim.core.s.x);
                // a settled point stands for every command the run took: one that changes it
                // wakes the run, which steps with it
                sim.unseen.clear();
                sim.stepped_with = sim.taken;
            }
            if sim.ended {
                sink.ended(sim.t());
            }
            sink.idle();
            values.publish(Arc::new(sim.values(0.0)));
            wake();
            published_t = sim.t();
            // points a full outlet refused, and what a sink's consumer refused, are offered again at
            // the trace back-off until a command
            let cmd = loop {
                let cmd = if sent && !sink.pending() {
                    cmds.recv().ok()
                } else {
                    match cmds.recv_timeout(trace_wait) {
                        Ok(cmd) => Some(cmd),
                        Err(RecvTimeoutError::Timeout) => {
                            if !sent {
                                let deltas = sim.stats.deltas;
                                sent = sim.send_delta(traces);
                                if sim.stats.deltas != deltas {
                                    wake();
                                }
                            }
                            let progress = sink.progress();
                            if sink.pending() {
                                sink.idle();
                            }
                            // from at least a millisecond: an idle loop never spins on a stalled consumer;
                            // a sink that sent something asks again soon (a held take longer than its
                            // queue goes out a queue a pass, never a second apart)
                            trace_wait = if sent && !sink.pending() {
                                sim.spec.trace_every
                            } else if sink.progress() != progress {
                                Duration::from_millis(1)
                            } else {
                                (trace_wait * 2).max(Duration::from_millis(1)).min(Duration::from_secs(1))
                            };
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => None,
                    }
                };
                // a changed sink set is installed now (a settled run hands the new sinks its held
                // values) and the run keeps waiting: nothing else changed
                if matches!(cmd, Some(LiveCmd::Sinks)) {
                    sink.idle();
                    // a new sink with something to send (a held take) is asked again soon
                    if sink.pending() {
                        trace_wait = Duration::from_millis(1);
                    }
                    continue;
                }
                break cmd;
            };
            let Some(cmd) = cmd else { return Ok(sim) };
            if !sim.take(cmd) {
                return Ok(sim);
            }
            // the wait is no interval of the run: the next publication's ratio counts from here
            (anchor, last_values) = ((Instant::now(), sim.t()), Instant::now());
            split.restart(&sim);
            continue;
        }
        let now = Instant::now();
        // the set-up runs at the whole solver's rate, as a firmware step runs to its stop
        let paced = sim.spec.rate.is_finite() && !sim.stepping() && !sim.preparing();
        let target = if paced { anchor.1 + sim.spec.rate * (now - anchor.0).as_secs_f64() } else { f64::INFINITY };
        // the run's start is kept before its first step (and a new recording's, after one that
        // dropped them): an edit anywhere after it can go back, however far the first slice goes
        if sim.ring.kept.is_empty() {
            sim.keep_snapshot(&*sink);
        }
        let at = Instant::now();
        let from = sim.t();
        let stepped = sim.advance_steps(target, Some(sim.spec.slice), sink, paced);
        let stepped_at = Instant::now();
        split.advance += stepped_at - at;
        if stepped.is_ok() {
            sim.keep_snapshot(&*sink);
        }
        // the values from here on were stepped with every command taken so far, as far as each was
        // produced past where it applied (`claims`)
        if sim.t() != from {
            sim.stepped_with = sim.taken;
        }
        let now = Instant::now();
        split.snapshots += now - stepped_at;
        if let Err(d) = stepped {
            values.publish(Arc::new(sim.values(0.0)));
            wake();
            let deltas = sim.stats.deltas;
            sim.send_delta(traces);
            if sim.stats.deltas != deltas {
                wake();
            }
            return Err(d);
        }
        if matches!(sim.start, Start::Done { notify: true }) {
            // The whole decision ended set-up in advance. The loop top starts playback there.
            continue;
        }
        if !sim.running() {
            // a step ended (a firmware stop, or a `Step`'s time): the waiting branch sends the last
            // points and publishes at once
            continue;
        }
        if sim.publish_soon || now - last_values >= sim.spec.values_every {
            if sim.running() && sim.core.steady {
                continue;
            }
            let wall = (now - last_values).as_secs_f64();
            let ratio = if wall > 0.0 { (sim.t() - published_t) / wall } else { 0.0 };
            split.report_due(&mut sim, now);
            values.publish(Arc::new(sim.values(ratio)));
            wake();
            last_values = now;
            published_t = sim.t();
            split.values += now.elapsed();
        }
        if now - last_trace >= trace_wait {
            let at = Instant::now();
            let deltas = sim.stats.deltas;
            trace_wait = if sim.send_delta(traces) { sim.spec.trace_every } else { (trace_wait * 2).min(Duration::from_secs(1)) };
            if sim.stats.deltas != deltas {
                wake();
            }
            last_trace = now;
            split.traces += at.elapsed();
        }
        if sim.running() && paced {
            let t = sim.t();
            if t >= target {
                // ahead: wait for the clock, a command wakes it at once
                let ahead = Duration::from_secs_f64(((t - target) / sim.spec.rate).min(1.0));
                let due = sim.spec.values_every.min(trace_wait).saturating_sub(now - last_values.max(last_trace));
                let at = Instant::now();
                match cmds.recv_timeout(ahead.min(due).max(Duration::from_micros(100))) {
                    Ok(cmd) => {
                        if !sim.take(cmd) {
                            return Ok(sim);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return Ok(sim),
                }
                let waited = at.elapsed();
                sink.clock_wait(waited);
                split.pacing += waited;
            } else if target - t > sim.spec.rate * sim.spec.slice.as_secs_f64() {
                // behind by more than a slice: re-anchor, `ratio` reports what was reached
                anchor = (now, t);
            }
        }
    }
}

/// A change to a `SinkSet`, sent by the host: a scope's acquisition engine goes in when the scope
/// becomes visible and out when it is hidden (a scope nobody sees costs the worker nothing).
pub enum SinkCmd {
    Add(u64, Box<dyn Sink + Send>),
    Remove(u64),
}

/// The sink a host hands `run_live`: the sinks installed now, changed between points through the
/// bounded channel `SinkSet::new` returns (the host `try_send`s; a change is a pointer move).
pub struct SinkSet {
    rx: Receiver<SinkCmd>,
    sinks: Vec<(u64, Box<dyn Sink + Send>)>,
    unknowns: Vec<UnknownInfo>,
    /// A settled run's time and held values, until its next point: a sink installed meanwhile
    /// starts from them.
    steady: Option<(f64, Vec<f64>)>,
    /// The run's sample period, as last told (0: none yet).
    period: f64,
}

impl SinkSet {
    pub fn new() -> (SyncSender<SinkCmd>, SinkSet) {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        (tx, SinkSet { rx, sinks: Vec::new(), unknowns: Vec::new(), steady: None, period: 0.0 })
    }

    /// How many sinks are installed.
    pub fn len(&self) -> usize {
        self.sinks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }

    fn poll(&mut self) {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                SinkCmd::Add(id, mut sink) => {
                    sink.begin(&self.unknowns);
                    if self.period > 0.0 {
                        sink.sample_period(self.period);
                    }
                    if let Some((at, x)) = &self.steady {
                        sink.point(*at, None, x);
                        sink.steady(*at, x);
                    }
                    self.sinks.retain(|(k, _)| *k != id);
                    self.sinks.push((id, sink));
                }
                SinkCmd::Remove(id) => self.sinks.retain(|(k, _)| *k != id),
            }
        }
    }
}

impl Sink for SinkSet {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.unknowns = unknowns.to_vec();
        self.steady = None;
        // the installed sinks begin the new generation; a sink queued since begins once, as poll
        // installs it
        for (_, s) in &mut self.sinks {
            s.begin(unknowns);
        }
        self.poll();
    }
    fn point(&mut self, at: f64, outer: Option<f64>, x: &[f64]) {
        self.steady = None;
        self.poll();
        for (_, s) in &mut self.sinks {
            s.point(at, outer, x);
        }
    }
    fn values(&mut self, at: f64, net_v: &[f64], pin_i: Option<&[f64]>) {
        for (_, s) in &mut self.sinks {
            s.values(at, net_v, pin_i);
        }
    }
    fn takes_values(&self) -> bool {
        self.sinks.iter().any(|(_, s)| s.takes_values())
    }
    fn rows_begin(&mut self) {
        for (_, s) in &mut self.sinks {
            s.rows_begin();
        }
    }
    fn rows_end(&mut self) {
        for (_, s) in &mut self.sinks {
            s.rows_end();
        }
    }
    fn mark(&mut self, at: f64) {
        for (_, s) in &mut self.sinks {
            s.mark(at);
        }
    }
    fn heard(&mut self, out: &mut Vec<u32>) {
        self.poll();
        for (_, s) in &mut self.sinks {
            s.heard(out);
        }
    }
    fn co_audio(&mut self, audio: &CoAudio) {
        for (_, s) in &mut self.sinks {
            s.co_audio(audio);
        }
    }
    fn below_res(&mut self, at: f64) {
        for (_, s) in &mut self.sinks {
            s.below_res(at);
        }
    }
    fn chosen(&mut self, at: f64) {
        for (_, s) in &mut self.sinks {
            s.chosen(at);
        }
    }
    fn sample_period(&mut self, period: f64) {
        self.period = period;
        for (_, s) in &mut self.sinks {
            s.sample_period(period);
        }
    }
    fn idle(&mut self) {
        self.poll();
        for (_, s) in &mut self.sinks {
            s.idle();
        }
    }
    fn started(&mut self) {
        for (_, s) in &mut self.sinks {
            s.started();
        }
    }
    fn ended(&mut self, at: f64) {
        for (_, s) in &mut self.sinks {
            s.ended(at);
        }
    }
    fn steady(&mut self, at: f64, x: &[f64]) {
        for (_, s) in &mut self.sinks {
            s.steady(at, x);
        }
        // kept for sinks installed while the run rests (one copy per settling, never per point);
        // the ones queued since the last point are installed from it now
        self.steady = Some((at, x.to_vec()));
        self.poll();
    }
    fn pending(&self) -> bool {
        self.sinks.iter().any(|(_, s)| s.pending())
    }
    fn progress(&self) -> u64 {
        self.sinks.iter().map(|(_, s)| s.progress()).fold(0, u64::wrapping_add)
    }
    fn holds(&mut self) -> bool {
        // every sink is asked, so each sends what waited
        self.sinks.iter_mut().fold(false, |held, (_, s)| s.holds() | held)
    }
    fn clock_wait(&mut self, wall: Duration) {
        for (_, s) in &mut self.sinks {
            s.clock_wait(wall);
        }
    }
    fn listened(&self) -> bool {
        self.sinks.iter().any(|(_, s)| s.listened())
    }
    fn save(&self) -> Option<Box<dyn SinkState>> {
        let saved: Vec<(u64, Box<dyn SinkState>)> = self.sinks.iter().filter_map(|(id, s)| Some((*id, s.save()?))).collect();
        Some(Box::new(SetSaved(saved)))
    }
    fn rewind(&mut self, at: f64, saved: Option<&dyn SinkState>, unknowns: &[UnknownInfo]) {
        self.unknowns = unknowns.to_vec();
        self.steady = None;
        let saved = saved.and_then(|s| s.downcast_ref::<SetSaved>());
        // each installed sink goes back to its state then; one installed since begins again
        for (id, s) in &mut self.sinks {
            let mine = saved.and_then(|set| set.0.iter().find(|(of, _)| of == id)).map(|(_, state)| &**state);
            s.rewind(at, mine, unknowns);
        }
        self.poll();
    }
}

/// A `SinkSet`'s state at a snapshot: its sinks' own, by id.
#[derive(Clone)]
struct SetSaved(Vec<(u64, Box<dyn SinkState>)>);

/// The analog as an attached process sees it while it runs (`cosim::Bus`).
struct LiveBus<'a> {
    sim: &'a mut LiveSim,
    pins: &'a [CoPin],
    sink: &'a mut dyn Sink,
    /// The wall time of the analog steps the run asked for (`advance`), not the device's own.
    nested: &'a mut Duration,
}

impl Bus for LiveBus<'_> {
    fn t(&self) -> f64 {
        self.sim.t()
    }
    fn v(&self, pin: u32) -> f64 {
        let x = &self.sim.ctl_s().x;
        match self.pins.get(pin as usize) {
            Some(&CoPin { pad: Some(pad), vss: Some(vss), .. }) => x[pad] - x[vss],
            _ => f64::NAN,
        }
    }
    fn supply(&self, pin: u32) -> f64 {
        let x = &self.sim.ctl_s().x;
        match self.pins.get(pin as usize) {
            Some(&CoPin { vdd: Some(vdd), vss: Some(vss), .. }) => x[vdd] - x[vss],
            _ => f64::NAN,
        }
    }
    fn advance(&mut self, t: f64) -> Result<(), SimDiag> {
        if self.sim.t() >= t {
            return Ok(());
        }
        let start = Instant::now();
        let mut stepped = Ok(());
        while stepped.is_ok() && self.sim.t() < t {
            stepped = self.sim.step_once(t, self.sink);
        }
        *self.nested += start.elapsed();
        stepped
    }
    fn write(&mut self, pin: u32, mode: PinMode) -> bool {
        // in-worker: no early publication (a UI command's `pin_write` asks for one)
        let Some(gpio) = self.pins.get(pin as usize).and_then(|p| p.gpio) else { return false };
        let sim = &mut *self.sim;
        let t = sim.t();
        let (s, st) = ctl_of(&mut sim.core.rounds, &mut sim.core.s, &mut sim.core.st);
        match s.pin_write(gpio, t, mode) {
            Some(Retuned::Now) => {
                if sim.core.write_t != t {
                    sim.core.write_t = t;
                    sim.stats.write_instants += 1;
                }
                sim.restart_written(gpio, mode);
            }
            Some(Retuned::At(_)) => st.breaks_changed(s),
            None => return false,
        }
        sim.written(gpio, mode);
        true
    }
    fn hits(&self) -> &[(u32, f64)] {
        &self.sim.core.hits
    }
}
