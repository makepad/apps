//! The live bench of the committed design and what views read from it. ELEC's host calls
//! `on_commit` once per committed generation and `frame` on UI signals and its own frames, which
//! writes the views' `ElecLive::sim` (the rows, the numbering, the play state) and reads the segment
//! trees views registered there; views ask for the rest with `SimAction`s
//! (commands by design id, scope sinks, emphasis, the ghost), which the app routes to `act`.
//!
//! - A commit is lowered on a job (newest wins). The first lowering starts the one long-lived worker
//!   (`run_live`); later ones go to it as `Replace` (state carried by name, time continues). A
//!   scope is a design unit or a block bench's own circuit; its content hash is computed on the
//!   job in either case. A commit whose circuit, scope and bench stimulus are the running
//!   lowering's is not lowered again. A probe's tip load (its R and C) is lowered at the probed
//!   points, so a bench edit that adds, removes or edits only ideal probes (a bandwidth is the
//!   channel's) lowers to the same circuit and keeps the run, its probes coming and going as scope
//!   sinks, and an edit of a tip load is a `Replace`. One with the same circuit and scope but
//!   another stimulus is lowered on the job and compared with the running lowering: when only
//!   independent sources' values differ (a frequency, a level, a pulse's timing, a pressed button's
//!   control), they go to the worker as `Retune`s (a periodic source switches at its next period
//!   start). A pin's mode is retuned the same way with `PinMode` (PWM over PWM preloads at the next
//!   period start on the running grid). The run, its numbering and its sinks go on; otherwise it
//!   is a `Replace`. Each installed lowering takes the host's next number for its index, values,
//!   traces and commands, independent of the scope and source generation.
//! - While the recording plays (`set_rendering`), a value edit (a knob, a press, a bench retune, a
//!   value edited in the circuit: `SimCircuit::value_changes`) goes to the worker as one
//!   `LiveCmd::Edit` at the run's time the listener hears (`SampleRing::heard_at`): the run goes back
//!   to its newest snapshot before it (a run with audio keeps two windows of them) and renders on
//!   with it, so the take is heard with the edit from there, with no restart. An edit that is no
//!   value (a part, a wire, a setting, another program) starts the run over from 0, once the edits
//!   pause. Live, every edit goes in at the run's front.
//! - Commands go out with `try_send` at once (a value site's `SetParam` on the pointer sample, not a
//!   frame later); on a full queue they wait in order and a newer `SetParam` of the same device and
//!   key, `Retune` of the same source, `PinMode` of the same GPIO, or replaceable timed `Input`
//!   of the same stream replaces the waiting one (logged once per episode).
//! - The run's one take (`Take`): every grid frame the worker closes (`LiveValues::frames`, from
//!   `Latest::keep_all`) and the device's states, kept whole within `TAKE_BYTES` (past it the
//!   oldest stretch keeps fewer frames, never less span). While the recording plays the rows show
//!   the instant heard (`SampleRing::heard_now`), not the render's front, and while the cursor
//!   reviews (`cursor`, paused inside the take) the instant it is on: the rows, the device's
//!   state, the LED glow, the inputs held and `SimLive::t` come from the take's frames between the
//!   instant shown last and the one shown now (their means, each weighted by its time there; one
//!   frame's at a parked cursor), so lights, values and the time read where the sound or the cursor
//!   is, round a loop too. A review never pauses the run.
//! - Otherwise the rows (`rows`, `shared/live-rows.md`) come from the newest `LiveValues`' interval means
//!   (the eye: a box over that interval, no tail, so a frame shows its own stretch of the run), the
//!   segment currents from the currents worker. While the cursor reviews, `SimLive::trace` holds
//!   the trace store for views that draw around the cursor (kept while the run rolls on past it).
//! - The trace store is merged on the worker (`TracePublisher`: the deltas applied there, the
//!   rolling record kept there) and published whole; a frame takes the newest with one `Arc` clone.
//! - A scope's acquisition sink is installed when the scope shows and removed when it hides.
//! - A co-simulated process (`set_cosim`: the virtual MCU running the bench's firmware) is made
//!   for each worker at its start, the nominal run's and every member's its own; a new maker swaps
//!   the running ones' at their current time, and a `Replace` keeps the attached process.
//! - The nominal run's device reports its firmware events through the feed `set_events` returns
//!   (the maker gives it to that device). Every offer wakes the UI, and each `step` drains the log
//!   without waiting, so steady play that draws no frames still takes them. A full feed holds the
//!   device and is the status's limit; nothing is dropped.
//! - Ensemble feeds keep independent long-lived members, with stable tolerance draws, running
//!   beside the nominal worker. Only their sinks are consumed; a member can end alone. A member's
//!   worker starts once the nominal has its operating point and solves its own from there (plain
//!   Newton, a few iterations); its commands and sinks queue from the moment it is due. A restart of
//!   the bench on an unchanged unknown layout starts from the last run's operating point.
//!
//! Per frame on the UI thread: queued commands and sink changes pumped (`try_send`), the newest trace
//! store taken (no merging; the mini traces' live ring follows its new points), at most one
//! `LiveValues` into net and part rows
//! (O(nets + pins + parts)), its device state into pin rows when that is a new `Arc` (a pointer
//! compare, else O(the state's pins)), and one currents job (sent on a frame: `frame`'s `tick`), at
//! most one currents result into segment rows (O(segments)), the changed rows' upload. Members add one completion check and queue pumps each;
//! their tolerance draws and circuit copies happen only at starts and replacements. Nothing here waits.

use crate::currents::CurrentsLink;
use crate::events::{EventFeed, EventLog, EventPump, EventSource, EventStore};
use crate::take::{EnvSlot, Means, Take};
use fab_elec_sim::cosim::CoState;
use crate::rate::{Limit, RateMeter, Split, KEEP_UP};
use crate::rows::{CoPinReader, Emitter, LiveTextures, RowPlan, RowValues};
use crate::edits::{circuit_edit, param_edit, press_edit};
use crate::Spawn;
use fab_elec_bench::tolerance::{member_circuit, member_noise, member_param, member_values, noise_seed};
use fab_elec_bench::audio::output_jacks;
use fab_elec_bench::bench::{BenchSource, SourceKind};
use fab_elec_bench::expect::{Expect, Probe};
use fab_elec_bench::judge::{Judged, LiveJudge};
use fab_elec_bench::{lower, lower_circuit, BenchSpec, BoardInput, LowerDiag, Lowered, ModelLibs, SimCmd, Touches, ValueIndex};
use fab_elec_circuit::Circuit;
use fab_elec_core::canon::{Canon, ContentHash, Writer};
use fab_elec_core::ids::{BoardId, CompRef, NetRef, PinRef};
use fab_elec_design::{CursorSource, Design, TimeCursor, UnitKind};
use fab_elec_lang::sys_sim::{AudioLoad, SimMode, SimSettings};
use fab_elec_lang::BenchScope;
use fab_elec_sim::analysis::round::{take_band, BlockPlanner};
use fab_elec_sim::analysis::states::StateCache;
use fab_elec_sim::circuit::thermal_noise_source;
use fab_elec_sim::rounds::{CheckPost, Helpers, Post};
use fab_elec_sim::sparse::compiled::{CodeCache, PlanJit};
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use fab_elec_sim::sparse::compiled::{CompiledLu, LuPlan, PlanCompiler};
use fab_elec_sim::cosim::{CoSimDevice, CoSimPin, CoSimSlot};
use fab_elec_sim::input::{InputSource, Inputs};
use fab_elec_sim::live::{Latest, LiveCmd, LiveValues, ValuePlan};
use fab_elec_sim::livesim::{run_live, LiveSim, LiveSpec, SinkCmd, SinkSet, Snapshots, AUDIO_SAMPLE_RATE, REALTIME_RECORDING_RATE, REALTIME_SAMPLE_RATE};
use fab_elec_sim::listen::{clock_now, AudioTap, HeardNow, ListenBus, TapMix, TapSpec, TapVoice, LOOP_SECONDS};
use fab_elec_sim::budget::{KEEP_BYTES, TAKE_BYTES};
use fab_elec_sim::recording::{window_frames, RunContent, RunSlot};
use fab_elec_sim::trace::{SignalId, TracePublisher, TraceStore};
use fab_elec_sim::{Change, OpSeed, Rescue, SimCircuit};
use fab_elec_view::live::segs::SegRows;
use fab_elec_view::live::sim::{LiveLed, LiveLimit, LiveSplit, LiveStatus, NetEnvelope, SimAction, SimLive, SinkFeed};
use fab_elec_view::canvas::emitter::emitter_rgb;
use fab_elec_view::live::listen::{jack_tap_voices, probe_tap_voices, JACK_TAP, LISTEN_DC_HZ, LISTEN_LATENCY};
use fab_elec_view::live::LiveLook;
use fab_elec_view::{DesignSnapshot, ElecLive, ViewAction};
use makepad_widgets_core::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};

pub use fab_elec_bench::values::RING_SECONDS;
pub use fab_elec_view::live::sim::Preparing;

/// Commands the worker's channel holds before the host queues on its side.
const COMMANDS: usize = 64;

/// The host's own probe ask (`SimAction::Probes` ids are the views'): the bench's voltage probes.
const BENCH_LANES: u64 = 0x4245_4e43_484c_414e;
/// The shortest live press of a momentary input, in simulated seconds: about a person's press, so
/// a click on a run slower than real time still lasts longer than a firmware's button poll.
pub const MIN_PRESS: f64 = 50e-3;

/// A lowering ready to run on a new worker (`SimHost::start_run`): taken in, its worker not started.
/// `glue_built`: the firmware glue said it builds this lowering's process.
struct RunStart {
    lowered: Arc<Lowered>,
    glue_built: bool,
    circuit: SimCircuit,
    plan: ValuePlan,
    index: ValueIndex,
    subscribe: LiveCmd,
}
/// A warm made ahead of the next start while the run is stopped (`SimHost::warm_ahead`): set to
/// stop it, and true while its lowering runs.
struct AheadWarm {
    stop: Arc<std::sync::atomic::AtomicBool>,
    lowering: Arc<std::sync::atomic::AtomicBool>,
}

/// A lowering that replaces the running circuit, held back while its key is warmed ahead
/// (`SimHost::hold_replace`): the lowering's message as it came, what the lowering's bookkeeping
/// takes when it goes in, and its number.
struct HeldReplace {
    id: u64,
    lowered: Box<Lowered>,
    circuit: Option<ContentHash>,
    made: (Arc<SimCircuit>, Arc<ValuePlan>),
    pending: Option<(BoardId, u64, Arc<BenchSpec>)>,
    pending_circuit: Option<Arc<Circuit>>,
    logic: bool,
}

/// The held replacement's warm, sending its return when the job ends however it ends (a warm
/// that fails or panics lets the lowering in as soon), unless it was stopped.
struct WarmReturn {
    id: u64,
    stop: Arc<std::sync::atomic::AtomicBool>,
    done: Sender<Msg>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Drop for WarmReturn {
    fn drop(&mut self) {
        if !self.stop.load(std::sync::atomic::Ordering::Acquire) {
            let _ = self.done.send(Msg::Warmed(self.id));
            (self.wake)();
        }
    }
}

/// While a recording plays, an edit that is no value restarts the run once the edits have paused
/// this long (wall seconds): a run of such edits restarts once, with the last, not once a frame.
pub const EDIT_QUIET: f64 = 0.3;

/// A restart an edit asked for while the recording plays: the edit's name for the status,
/// when the edits paused (set by the first `step` after the newest), and whether the knobs keep
/// their values (a new lowering puts the circuit's own back, as a live edit does).
struct Restart {
    name: Option<String>,
    since: Option<f64>,
    keep_params: bool,
}
/// The instant the rows show from the take (heard, or under the reviewing cursor): its time,
/// whether it moves (the sound plays on), the device's state there, the end of the frame it was
/// read from and the take's revision then (a frame that came or changed since shows again).
struct Shown {
    t: f64,
    moving: bool,
    co: Option<CoState>,
    slot_t: f64,
    revision: u64,
}

/// The silent playback clock: a held take played back with nobody listening, as the player plays
/// it with sound. The head (the run's seconds), whether it moves, the frame time it was there
/// (None: the next frame anchors it), and when the player was last asked to take over there.
struct Silent {
    t: f64,
    moving: bool,
    at: Option<f64>,
    sought: Option<f64>,
}

/// How close the player's instant comes to the silent clock's for it to take over (its device
/// latency trails it), seconds; and how often a seek it did not take is asked again (a recorded
/// window covers the head only once the run renders past it).
const HANDOVER: f64 = 0.25;

/// Two vectors take turns so a view can retain the previous publication.
struct KeptEnvelope {
    revision: Option<u64>,
    published: NetEnvelope,
    spare: Arc<Vec<EnvSlot>>,
    tail: Vec<EnvSlot>,
    copying: Option<Receiver<Vec<EnvSlot>>>,
}

type EnvelopeCopy = (Arc<Vec<EnvSlot>>, usize, SyncSender<Vec<EnvSlot>>);

#[derive(Default)]
struct EnvelopeCopies {
    worker: Option<SyncSender<Vec<EnvelopeCopy>>>,
    pending: Vec<EnvelopeCopy>,
    full: bool,
}

impl EnvelopeCopies {
    fn request(&mut self, slots: &Arc<Vec<EnvSlot>>, needed: usize) -> Receiver<Vec<EnvSlot>> {
        let (reply, copy) = mpsc::sync_channel(1);
        self.pending.push((slots.clone(), needed.saturating_mul(2), reply));
        copy
    }

    /// Copy pinned buffers and grow full ones off the UI thread.
    fn pump(&mut self, spawn: &dyn Spawn) {
        if self.pending.is_empty() {
            return;
        }
        let worker = self.worker.get_or_insert_with(|| {
            let (tx, rx) = mpsc::sync_channel::<Vec<EnvelopeCopy>>(1);
            let wake = spawn.waker();
            spawn.worker("take envelope copies", Box::new(move || {
                for batch in rx {
                    for (source, capacity, reply) in batch {
                        let mut spare = Vec::with_capacity(capacity);
                        spare.extend_from_slice(&source);
                        let _ = reply.try_send(spare);
                    }
                    wake();
                }
            }));
            tx
        });
        match worker.try_send(std::mem::take(&mut self.pending)) {
            Ok(()) => self.full = false,
            Err(TrySendError::Full(batch)) => {
                if !self.full {
                    log!("elec sim: envelope copies queued until the worker catches up");
                }
                self.full = true;
                self.pending = batch;
            }
            Err(TrySendError::Disconnected(batch)) => {
                self.worker = None;
                self.pending = batch;
            }
        }
    }
}

impl KeptEnvelope {
    fn new(id: u64, net: NetRef) -> Self {
        Self {
            revision: None,
            published: NetEnvelope { id, net, slots: Arc::default(), grid: 0.0 },
            spare: Arc::default(),
            tail: Vec::new(),
            copying: None,
        }
    }

    fn update(&mut self, take: &Take, row: usize, copies: &mut EnvelopeCopies) {
        let current = &mut self.published;
        if self.revision != Some(take.revision()) {
            let mut slots = Vec::new();
            take.net_envelope(row, f64::NEG_INFINITY, &mut slots);
            // Re-rendering can change an interior slot and leave both ends alone.
            if slots != *current.slots {
                slots.reserve(slots.len());
                let mut spare = Vec::with_capacity(slots.capacity());
                spare.extend_from_slice(&slots);
                self.spare = Arc::new(spare);
                current.slots = Arc::new(slots);
                self.copying = None;
            }
            current.grid = take.grid();
            self.revision = Some(take.revision());
            return;
        }
        let Some(last) = current.slots.last() else { return };
        if take.span().is_some_and(|(_, end)| end == last.t1) {
            return;
        }
        // Keep showing the last complete envelope while a pinned or full spare is replaced.
        if let Some(copy) = &self.copying {
            match copy.try_recv() {
                Ok(slots) => self.spare = Arc::new(slots),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {}
            }
            self.copying = None;
        }
        let last_spare = self.spare.last();
        let prefix = self.spare.len() - usize::from(last_spare.is_some_and(|s| s.t0 < s.t1));
        let from = last_spare.map_or(f64::NEG_INFINITY, |slot| slot.t0);
        self.tail.clear();
        take.net_envelope(row, from, &mut self.tail);
        let needed = prefix + self.tail.len();
        if needed == current.slots.len() && self.tail.last() == current.slots.last() {
            return;
        }
        let Some(slots) = Arc::get_mut(&mut self.spare).filter(|v| v.capacity() >= needed) else {
            self.copying = Some(copies.request(&current.slots, needed));
            return;
        };
        slots.truncate(prefix);
        slots.extend_from_slice(&self.tail);
        std::mem::swap(&mut current.slots, &mut self.spare);
    }
}

/// The stretch of the playback timebase the rows' light must resolve, seconds (about two frames):
/// Fast steps at least four times within `EYE * rate` simulated seconds, so a frame's mean (the
/// eye, [`Eye`]) is made of steps, not one. A bench run as fast as it goes (an unbounded rate)
/// has no playback timebase and asks no resolution for it.
pub const EYE: f64 = 0.03;

/// Makes a run's co-simulated process (the virtual MCU running the bench's firmware) from its
/// lowering, its pins by name in it (`Lowered::cosim_pins`): once for each worker that starts
/// (`member` 0 is the nominal run, 1.. the ensemble members, each with a process of its own), and
/// for the running ones when `SimHost::set_cosim` changes it. It only instantiates: anything slow
/// (compiling the firmware) is done before `set_cosim`, off the UI thread.
pub type CoSimMaker = Arc<dyn Fn(&Lowered, u32) -> Option<CoSimDevice> + Send + Sync>;

/// The rows' light: the newest publication's interval means, a box over that stretch of the run
/// with no tail. An LED switched hard is fully lit (or dark) on the first frame whose publication
/// starts after its edge, never fading in over later frames; a PWM-driven LED glows at its mean
/// over the frame's stretch: steady at 1x, where a frame spans many periods, and blinking as the
/// slowed PWM does where a frame spans less than a period (a run far below real time, a slow
/// playback rate). The host reads only the newest publication on a frame, so a late or slow frame
/// (a busy UI, a hidden window) misses some: the newest means stand for the time it missed. A
/// publication with no simulated time (the run held, or a command's right after another) carries
/// the sample at `t`, not a mean, and changes nothing.
#[derive(Default)]
pub struct Eye {
    generation: Option<u64>,
    net: Vec<f32>,
    pin: Vec<f32>,
    part: Vec<f32>,
}

impl Eye {
    /// The blended means by net, pin and component (`LiveValues::net_mean`, `pin_mean`, `part_mean`).
    pub fn net(&self) -> &[f32] {
        &self.net
    }
    pub fn pin(&self) -> &[f32] {
        &self.pin
    }
    pub fn part(&self) -> &[f32] {
        &self.part
    }

    /// Takes one publication's interval means (none from one with no simulated time); a new
    /// numbering starts over from its means.
    pub fn see(&mut self, lv: &LiveValues) {
        let same = self.generation == Some(lv.generation) && self.net.len() == lv.net_mean.len() && self.pin.len() == lv.pin_mean.len() && self.part.len() == lv.part_mean.len();
        if !same {
            self.generation = Some(lv.generation);
            self.net.clone_from(&lv.net_mean);
            self.pin.clone_from(&lv.pin_mean);
            self.part.clone_from(&lv.part_mean);
            return;
        }
        // An interval with no simulated time (the run held, or a command's publication right after
        // another) carries the sample at `t`, not a mean: it changes nothing
        if !(lv.t - lv.since > 0.0) {
            return;
        }
        for (seen, new) in [(&mut self.net, &lv.net_mean), (&mut self.pin, &lv.pin_mean), (&mut self.part, &lv.part_mean)] {
            seen.copy_from_slice(new);
        }
    }
}

/// How far short of the take's end a run still stands at it, seconds: its last step lands on the
/// sample grid a rounding below the end (`LiveSim`'s slack is a millionth of its largest step).
pub const TAKE_SLACK: f64 = 1e-9;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunMode {
    Loop,
    Continuous,
    Realtime,
}

/// The transport's state (`SimHost::status`, `elec.sim.status`).
#[derive(Clone, Debug, PartialEq)]
pub struct SimStatus {
    /// The newest values' simulated time (0 before any).
    pub t: f64,
    /// Play was asked and the transport can advance the run or play its recording.
    pub playing: bool,
    /// The take plays back ([`SimHost::plays_back`]): the views follow the instant it plays, by
    /// the player with sound or by the silent clock with none, moving or held.
    pub plays_back: bool,
    /// A firmware step runs.
    pub stepping: bool,
    /// The wait before playback, while the newest values say the run is preparing.
    pub preparing: Option<Preparing>,
    /// Simulated seconds per wall second asked (infinite: as fast as it goes).
    pub rate_asked: f64,
    /// Simulated seconds per wall second reached over about the last half second of publications;
    /// None until a run has stepped that long (`RateMeter::reached`).
    pub rate_achieved: Option<f64>,
    /// What holds the rate while the run goes on (None while preparing or at rest).
    pub limited_by: Option<Limit>,
    /// Where the stepping time went over the rate's window (None while preparing or at rest):
    /// `limited_by` names its largest part unless the run is paced or its event feed is full.
    pub split: Option<Split>,
    /// The bench has settled.
    pub steady: bool,
    /// The time resolution the newest values were stepped at, seconds (`LiveValues::res`): 0 is
    /// Exact.
    pub res: f64,
    /// The user's setting (`SimHost::set_sim_mode`).
    pub mode: SimMode,
    /// The timing cells the running lowering has (Logic's).
    pub cells: usize,
    /// The sample rate the run steps on, Hz (0: none, its bandwidth step); with no run, the rate
    /// the next one takes (`SimHost::sample_rate`).
    pub sample_rate: f64,
    /// The user's pick (`SimHost::set_sample_rate`; None: the host's choice).
    pub sample_rate_chosen: Option<f64>,
    /// The rate the run's takes are recorded at, Hz; with no run, the rate the next one's are
    /// (`SimHost::recording_rate`).
    pub recording_rate: f64,
    /// The user's pick (`SimHost::set_recording_rate`; None: the default audio rate).
    pub recording_rate_chosen: Option<f64>,
    /// The default load for the output jacks (`SimHost::set_jack_load`; None: a line input).
    pub jack_load: Option<AudioLoad>,
    /// The take's length, simulated seconds (`set_take_len`; None: an open run).
    pub take: Option<f64>,
    /// Continuous is in effect. The run is open and keeps a rolling record.
    pub continuous: bool,
    /// The open run plays directly as it is simulated.
    pub realtime: bool,
    /// Continuous's ring, simulated seconds.
    pub ring: f64,
    /// The time the newest heard edit goes in at (`heard_edit`), while the run renders the take
    /// again from there.
    pub rerender: Option<f64>,
    /// Why the take renders again for a Listen, in plain words, while it does (`relisten`): the
    /// Listen could not play it from the hearing recorder's tape ("voice not recorded in this take
    /// (QUIET)", "the tape stopped short (1.000 of 2.000 s)", "the take is not rendered yet").
    pub rerender_reason: Option<Arc<str>>,
    /// Auto re-record is on (`SimHost::set_auto_rerecord`).
    pub auto_rerecord: bool,
    /// With it off, the edits and presses made while the recording played since the run started:
    /// the next record takes them in.
    pub waiting: u32,
}

impl From<SimStatus> for LiveStatus {
    fn from(status: SimStatus) -> Self {
        Self {
            t: status.t, playing: status.playing, stepping: status.stepping, preparing: status.preparing,
            rate_asked: status.rate_asked, rate_achieved: status.rate_achieved,
            limited_by: status.limited_by.map(|limit| match limit {
                Limit::Paced => LiveLimit::Paced,
                Limit::Emulator => LiveLimit::Emulator,
                Limit::Scope => LiveLimit::Scope,
                Limit::Solver => LiveLimit::Solver,
                Limit::Events => LiveLimit::Events,
                Limit::Input => LiveLimit::Input,
            }),
            split: status.split.map(|split| LiveSplit { solver: split.solver, scope: split.scope, emulator: split.emulator }),
            steady: status.steady,
            res: status.res,
            mode: status.mode,
            cells: status.cells,
            sample_rate: status.sample_rate,
            sample_rate_chosen: status.sample_rate_chosen,
            recording_rate: status.recording_rate,
            recording_rate_chosen: status.recording_rate_chosen,
            jack_load: status.jack_load,
            take: status.take,
            continuous: status.continuous,
            realtime: status.realtime,
            ring: status.ring,
            rerender: status.rerender,
            rerender_reason: status.rerender_reason,
            auto_rerecord: status.auto_rerecord,
            waiting: status.waiting,
            plays_back: status.plays_back,
        }
    }
}

/// An LED as the run shows it (`SimHost::leds`, `elec.sim.leds`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LedLive {
    pub part: CompRef,
    /// The anode current at the newest values' time, amperes.
    pub i: f32,
    /// Its mean over the newest publication's interval, amperes.
    pub i_mean: f32,
    /// The light output its part row shows now, 0..1.
    pub brightness: f32,
    /// The emitter's linear colour at unit brightness.
    pub rgb: Option<[f32; 3]>,
}

/// Where the commands sent to the run stood ([`SimHost::sent_mark`]): the run they went to and
/// how many it takes with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SentMark {
    run: u64,
    count: u64,
}

/// Commands in order, sent without blocking.
#[derive(Default)]
struct Commands {
    tx: Option<SyncSender<LiveCmd>>,
    queue: VecDeque<LiveCmd>,
    /// How many went to the worker of `tx` (`LiveValues::stepped_with` counts them as it takes them).
    sent: u64,
    /// Value commands replaced while waiting, this episode and in all.
    coalescing: bool,
    coalesced: u64,
}

impl Commands {
    fn send(&mut self, cmd: LiveCmd) {
        let replaced = match &cmd {
            LiveCmd::SetParam { generation, device, key, value } => self.queue.iter_mut().find_map(|c| match c {
                LiveCmd::SetParam { generation: g, device: d, key: k, value: v } if g == generation && d == device && k == key => {
                    *v = *value;
                    Some(())
                }
                _ => None,
            }),
            LiveCmd::Retune { generation, device, source } => self.queue.iter_mut().find_map(|c| match c {
                LiveCmd::Retune { generation: g, device: d, source: s } if g == generation && d == device => {
                    s.clone_from(source);
                    Some(())
                }
                _ => None,
            }),
            LiveCmd::PinMode { generation, device, mode } => self.queue.iter_mut().find_map(|c| match c {
                LiveCmd::PinMode { generation: g, device: d, mode: m } if g == generation && d == device => {
                    *m = *mode;
                    Some(())
                }
                _ => None,
            }),
            LiveCmd::At(t, cmd) => match &**cmd {
                LiveCmd::Input { generation, key, input, replaceable: true } => {
                    // The newest input of this stream is the only candidate: an up or down
                    // between moves must keep them in their separate gestures.
                    self.queue.iter_mut().rev().find(|c| matches!(c.untimed(),
                        LiveCmd::Input { generation: g, key: k, .. } if g == generation && k == key
                    )).and_then(|c| match c {
                        LiveCmd::At(at, cmd) => match &mut **cmd {
                            LiveCmd::Input { input: queued, replaceable: true, .. } => {
                                *at = *t;
                                queued.clone_from(input);
                                Some(())
                            }
                            _ => None,
                        },
                        _ => None,
                    })
                }
                _ => None,
            },
            _ => None,
        };
        if replaced.is_some() {
            self.coalesced += 1;
            if !self.coalescing {
                self.coalescing = true;
                log!("elec sim: the command queue is full; value changes wait and the newest per device is kept");
            }
            return;
        }
        self.queue.push_back(cmd);
        self.pump();
    }

    fn pump(&mut self) {
        let Some(tx) = &self.tx else { return };
        while let Some(cmd) = self.queue.pop_front() {
            match tx.try_send(cmd) {
                Ok(()) => self.sent += 1,
                Err(TrySendError::Full(cmd)) => {
                    self.queue.push_front(cmd);
                    return;
                }
                Err(TrySendError::Disconnected(_)) => {
                    // the worker ended; the next lowering starts a new one
                    self.tx = None;
                    self.queue.clear();
                    return;
                }
            }
        }
        self.coalescing = false;
    }
}

/// Sink changes in order, retried on later frames when the worker's channel is full.
#[derive(Default)]
struct Sinks {
    tx: Option<SyncSender<SinkCmd>>,
    queue: VecDeque<SinkCmd>,
    full: bool,
}

impl Sinks {
    /// Queues a change; true when changes reached the worker, which a waiting run installs once
    /// told (`LiveCmd::Sinks`).
    #[must_use]
    fn send(&mut self, cmd: SinkCmd) -> bool {
        self.queue.push_back(cmd);
        self.pump()
    }

    /// True when queued changes reached the worker.
    #[must_use]
    fn pump(&mut self) -> bool {
        let Some(tx) = &self.tx else { return false };
        let mut sent = false;
        while let Some(cmd) = self.queue.pop_front() {
            match tx.try_send(cmd) {
                Ok(()) => sent = true,
                Err(TrySendError::Full(cmd)) => {
                    self.queue.push_front(cmd);
                    if !self.full {
                        log!("elec sim: the sink queue is full; changes wait for the worker");
                        self.full = true;
                    }
                    return sent;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.tx = None;
                    self.queue.clear();
                    return false;
                }
            }
        }
        self.full = false;
        sent
    }
}

/// One member's inputs and completion. Its values and traces stay on its worker.
struct Member {
    cmds: Commands,
    sinks: Sinks,
    values: Vec<f64>,
    seed: u64,
    noise_dt: f64,
    ended: Receiver<Option<String>>,
    /// The worker's body until the run's operating point is in: it starts from that solution
    /// (`SimHost::start_members`); commands and sink changes queue in the channels meanwhile.
    start: Option<Box<dyn FnOnce(Arc<(u64, OpSeed)>, Arc<dyn Fn() + Send + Sync>) + Send>>,
}

impl Member {
    /// The nominal run's command as this member takes it: a value at its tolerance draw, and a
    /// resistor's noise with it; an edit's every value so, its noise at the value's time. The
    /// member keeps no snapshots: an edit behind its front applies at its front.
    fn send(&mut self, mut cmd: LiveCmd, lowered: &Lowered) {
        if let LiveCmd::Edit(events) = cmd {
            let mut edit = Vec::with_capacity(events.len());
            for (t, mut cmd) in events {
                let noise = self.tolerate(&mut cmd, lowered);
                edit.push((t, cmd));
                edit.extend(noise.map(|noise| (t, noise)));
            }
            return self.cmds.send(LiveCmd::Edit(edit));
        }
        let noise = self.tolerate(cmd.untimed_mut(), lowered).map(|retune| match &cmd {
            // its noise changes with it, on the timeline when it is
            LiveCmd::At(t, _) => LiveCmd::At(*t, Box::new(retune)),
            _ => retune,
        });
        self.cmds.send(cmd);
        if let Some(noise) = noise {
            self.cmds.send(noise);
        }
    }

    /// A `SetParam` at this member's tolerance draw, and a resistor's thermal noise retune at its
    /// new value (this member's seed). Anything else as it is.
    fn tolerate(&self, cmd: &mut LiveCmd, lowered: &Lowered) -> Option<LiveCmd> {
        let LiveCmd::SetParam { generation, device, key, value } = cmd else { return None };
        *value = member_param(&lowered.tol, &self.values, device, key, *value);
        if !(matches!(key.as_str(), "value" | "r") && lowered.circuit.devices.r.iter().any(|r| r.name == *device)) {
            return None;
        }
        thermal_noise_source(*value, lowered.circuit.temp_c, self.noise_dt, noise_seed(self.seed, device))
            .map(|source| LiveCmd::Retune { generation: *generation, device: format!("{device}#noise"), source })
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        self.cmds.send(LiveCmd::Stop);
        // If Stop is queued, closing the channel also wakes and ends the worker.
        self.cmds.tx = None;
    }
}

/// A lowering job's result, or the worker's end.
enum Msg {
    /// A lowering, its scope circuit's content hash, and copies of its kernel circuit and plan (made
    /// on the job) for telling a later stimulus retune from a new lowering.
    Lowered(Box<Lowered>, Option<ContentHash>, (Arc<SimCircuit>, Arc<ValuePlan>)),
    /// The commit's lowering inputs are the running lowering's: nothing to replace.
    Unchanged,
    /// The same scope, its circuit and stimulus changed only in values (independent sources,
    /// GPIO modes, passive values: `SimCircuit::value_changes`): each change by device name with
    /// its new value, the new lowering, and the scope circuit's new content hash when the circuit
    /// changed (a value edited in it), None when only the stimulus did.
    Retuned(Vec<Change>, Box<Lowered>, Option<ContentHash>),
    /// The pin table of the co-simulated device the nominal worker made at its start.
    CoPins(Vec<CoSimPin>),
    /// The nominal worker of run `.0` ended (with why, when it failed), handing back its scopes'
    /// sinks, which the next run takes over.
    Ended(u64, Option<String>, Box<SinkSet>),
    /// The warm made ahead of a held replacement is back (`SimHost::hold_replace`), by its number.
    Warmed(u64),
}

impl Msg {
    fn lowered(l: Lowered, circuit: Option<ContentHash>) -> Msg {
        let made = (Arc::new(l.circuit.clone()), Arc::new(l.plan.clone()));
        Msg::Lowered(Box::new(l), circuit, made)
    }
}

/// The running worker's outputs.
struct Worker {
    values: Arc<Latest<LiveValues>>,
    /// The trace store its `TracePublisher` merges and publishes.
    traces: Arc<Latest<TraceStore>>,
}

/// What a lowering was made from (its scope circuit's content hash, the scope, the bench and
/// whether for Logic), and what it made (the kernel circuit and plan, for telling a stimulus retune
/// from a new lowering).
struct LowerKey {
    circuit: ContentHash,
    scope: BoardId,
    bench: Arc<BenchSpec>,
    logic: bool,
    sim: Arc<SimCircuit>,
    plan: Arc<ValuePlan>,
}

/// The current lowering's facts the host keeps.
#[derive(Default)]
struct Facts {
    generation: u64,
    /// The running lowering's scope id and source generation.
    source: (BoardId, u64),
    rows: RowPlan,
}

pub struct SimHost {
    spawn: Option<Box<dyn Spawn>>,
    libs: Arc<ModelLibs>,
    bench: Arc<BenchSpec>,
    scope: Option<BenchScope>,
    spec: LiveSpec,
    /// The current lowering's numbering (views get it through `SimLive::index`).
    index: Arc<ValueIndex>,
    /// The running lowering's LEDs (its emitters' parts, in emitter order), made once per numbering.
    glow_parts: Arc<Vec<CompRef>>,
    /// Each of `glow_parts`' anode pin and on-current (the largest it drew under this numbering and
    /// these values; a retune forgets it).
    glow_rows: Vec<(usize, f32)>,
    glow_for: Option<u64>,
    /// The number of the last lowering installed.
    /// Every installed lowering takes the next one as its `ValueIndex.generation`, which is the number
    /// the worker, its values, its traces and every command carry: two lowerings never share one,
    /// whatever their scopes and source generations, so nothing queued or published for one is taken
    /// for another across a scope switch.
    numbering: u64,
    cmds: Commands,
    /// Whether the bench plays (the last `Play`/`Pause` sent).
    playing: bool,
    inputs: Option<Inputs>,
    mic_input: Option<MicSources>,
    /// The take's length in effect: the person's (`take_user`), else the bench's (`take_bench`);
    /// None: an open run.
    take_asked: Option<f64>,
    /// The take's length the bench asks (`set_take_len`, its `stop`).
    take_bench: Option<f64>,
    /// The take's length the person set (`set_take_user`: the `sim` statement's
    /// `recording_length`, or `SimCmd::SetTakeLen`), in place of the bench's.
    take_user: Option<f64>,
    /// The transport's choice, or None to follow the bench (`run_mode`).
    mode: Option<RunMode>,
    /// Whether the bench's take opens live (`set_take_live`, its `run: {live: true}`).
    take_live: bool,
    ring: f64,
    /// Check for a run past the restored Loop end, including a worker ahead of its last publication.
    check_loop_end: bool,
    /// A Pause while the recording plays a take of finite length: what is heard holds while the
    /// worker renders on to the take's end (`SimCmd::Pause`). The transport's Stop (`Hold`) and
    /// Play end it.
    finish: bool,
    sinks: Sinks,
    /// The Listen taps installed (`SimAction::Listen`): while any is, the nominal worker runs
    /// unpaced and the tap's ring paces it (`worker_rate`).
    listening: BTreeSet<u64>,
    feeds: BTreeMap<u64, Box<dyn SinkFeed + Send>>,
    member_count: Option<usize>,
    /// An empty slot is an ended member, kept empty until its slot is removed.
    members: Vec<Option<Member>>,
    ensemble_lowered: Option<Arc<Lowered>>,
    run_spec: Option<LiveSpec>,
    /// Ensemble noise keeps the run's original bandwidth bound by the lowering's spacing.
    run_noise_dt: f64,
    /// The co-simulated process maker (`set_cosim`), used by every worker start.
    cosim: Option<CoSimMaker>,
    /// How the pin rows read the device's published state (`set_co_pins`), and the pin table of
    /// the nominal run's device (joined to each lowering's pins in `RowPlan::co_pins`).
    co_reader: Option<CoPinReader>,
    co_table: Vec<CoSimPin>,
    /// The nominal worker's operating point at its start, tagged with its run (`runs` counts the
    /// worker starts): members start from the current run's, a restart from the last run's.
    op_seed: Arc<Latest<(u64, OpSeed)>>,
    /// The round planner's kept states, shared by every run the host starts (`StateCache`): a
    /// run of a board planned before (opened, then Record) finds its plan's first cost made, and
    /// starts with what runs of it refused and landed (`LiveSim::set_memory`).
    plan_states: StateCache,
    /// While stopped: the warm made ahead of the next start for the newest design (`warm_ahead`),
    /// and whether one committed since wants one.
    ahead: Option<AheadWarm>,
    ahead_due: bool,
    /// While playing: a lowering that replaces the circuit (a structure edit), held back while
    /// its key is warmed ahead (`hold_replace`), and the number of the last one held.
    held_replace: Option<HeldReplace>,
    replaces_held: u64,
    /// When the newest design was committed while the run played, then the numbering of the
    /// lowering that replaced the circuit with it: its commit-to-heard line.
    edit_committed: Option<f64>,
    edit_hearing: Option<(u64, f64)>,
    runs: u64,
    /// Board scopes this host has started a run for, across changes to their values.
    played_scopes: BTreeSet<BoardId>,
    /// This run was the first started for its scope.
    first_play: bool,
    /// Wall time when the host first saw the current run preparing (`clock_now`).
    preparing_since: Option<f64>,
    /// A member waits for the current run's operating point (`start_members` retries when the host wakes).
    members_wait: bool,
    /// Current knob values, replayed when a member starts after the knob moved.
    params: BTreeMap<(String, String), f64>,
    /// The committed design to lower next (newest wins), the generation last asked for, whether a
    /// lowering job is out.
    want: Option<Arc<Design>>,
    asked: Option<u64>,
    lowering: bool,
    /// The running lowering's inputs, and the scope, source generation and bench of the lowering job out.
    key: Option<LowerKey>,
    pending: Option<(BoardId, u64, Arc<BenchSpec>)>,
    done_tx: Sender<Msg>,
    done_rx: Receiver<Msg>,
    live: Option<Worker>,
    facts: Facts,
    diags: Vec<LowerDiag>,
    error: Option<String>,
    store: Arc<TraceStore>,
    mini: crate::mini::MiniWriter,
    ghost: Option<Arc<TraceStore>>,
    values: Option<Arc<LiveValues>>,
    seen: u64,
    /// The steps the live run took only through the stepper's gmin ladder: how many, the newest,
    /// and the worker's count last seen (a new worker counts from 0 again).
    rescues: (u64, Option<Rescue>),
    rescues_seen: u64,
    /// The number of the worker's newest split the log has (`LiveValues::worker_split`).
    split_seen: u64,
    /// Why the live run's fast mode is off (`LiveValues::plan_error`), as the log has it.
    plan_error: Option<Arc<str>>,
    /// Momentary inputs held down live: (input, numbering, the host's `t` at the press, the
    /// release's time once released). A press is two events on the run's timeline, down at the
    /// run's time and up at least `MIN_PRESS` later; the input shows held until the run passes
    /// its release.
    held: Vec<(CompRef, u64, f64, Option<f64>)>,
    touches: Touches,
    /// Every press of the take (as `held`, kept after its release), for the inputs held at the
    /// instant shown; cleared with the take.
    presses: Vec<(CompRef, u64, f64, Option<f64>)>,
    /// Knobs turned live: (knob, numbering, its place from 0 to 1). The run's values
    /// (`params`) are the source of truth and these places follow them: a Stop keeps both, the
    /// Play after it and a restart for an edit carry both into the new numbering (the run starts
    /// the knobs there), and any other lowering puts the circuit's own values back and the knobs
    /// where the design has them ([`Self::lowered_values`]).
    turned: Vec<(CompRef, u64, f64)>,
    /// Every knob turn of the take: (knob, numbering, the time it applies at, its place), for
    /// the knobs' places at the instant shown; cleared with the take. `places` is the scratch
    /// those places are gathered in (its storage reused).
    turns: Vec<(CompRef, u64, f64, f64)>,
    places: Vec<(CompRef, f64, f64)>,
    /// The run's one take (`Take`), the publications last taken into it (their storage reused),
    /// the instant shown from it and its means.
    take: Take,
    kept: Vec<Arc<LiveValues>>,
    shown: Option<Shown>,
    /// The silent playback clock (`Silent`), and the player's instant last frame, which the clock
    /// goes on from when the listener stops over a held take.
    silent: Option<Silent>,
    heard_last: Option<HeardNow>,
    means: Means,
    /// The take's cost said once a take: bytes a simulated second.
    take_said: bool,
    /// The trace publication last taken.
    traces_seen: u64,
    eye: Eye,
    /// The achieved rate over the newest publications, and what limits it.
    meter: RateMeter,
    /// The firmware event log of the nominal run's device (`set_events`).
    events: Option<Box<dyn EventPump>>,
    tex: LiveTextures,
    currents: Option<CurrentsLink>,
    /// The pin currents of the newest instant shown, and the generation they are numbered in: the
    /// currents job the next frame sends (`frame`'s `tick`).
    currents_due: Option<(Arc<[f32]>, u64)>,
    /// The first currents job of the current generation and that generation (older results are
    /// dropped: their pin indices are of another lowering).
    currents_from: u64,
    currents_gen: u64,
    /// The cursor's time while it reviews inside the take (paused there): the rows show the take
    /// at it, and the run goes on.
    scrub: Option<f64>,
    /// Moves with each emphasis change; `SimLive::emphasis` follows it.
    emphasis_rev: u64,
    /// Each open scope's seconds per pixel column (`SimAction::ShowScope`), by its id: the time
    /// resolution follows the finest (`wanted_res`).
    columns: BTreeMap<u64, f64>,
    /// The signals each view draws over the whole run (`SimAction::Probes`), by its id: the
    /// record keeps all of them whole (`LiveSpec::whole`).
    probes: BTreeMap<u64, Vec<usize>>,
    /// The rows Realtime's trace writes, as last worked out (`sync_fine`): kept, so a frame that
    /// changes nothing allocates nothing.
    fine_rows: Vec<bool>,
    /// The net each view wants to review over the whole take.
    envelope_asks: BTreeMap<u64, NetRef>,
    envelopes: BTreeMap<u64, KeptEnvelope>,
    envelope_copies: EnvelopeCopies,
    /// The spilled logic chunks each view shows in detail (`SimAction::LogicDetail`), by its id:
    /// the run keeps all of them in memory (`LiveSpec::logic_detail`).
    details: BTreeMap<u64, Vec<(SignalId, f64)>>,
    /// The user's setting: Fast (the default) steps at `wanted_res`; Exact at resolution 0, every
    /// edge and tail resolved; Logic as Fast, on a lowering with the timing cells.
    sim_mode: SimMode,
    /// Whether the lowering on its way was made for Logic, and the timing cells the running one
    /// has.
    lowering_logic: bool,
    cells: usize,
    /// This run's rounds: None while undecided, true when the run starts in them or the host
    /// asks for them (Exact turns them off again), false for a run with no helper cores.
    rounds: Option<bool>,
    /// The circuit (its content hash and scope) whose last run went in rounds: the next run of it
    /// starts in them (`start_run`), at any rate.
    rounds_for: Option<(ContentHash, BoardId)>,
    /// The user's sample rate, Hz (None: the host's choice, `sample_rate`).
    chosen_rate: Option<f64>,
    /// The user's recording rate, Hz (None: the default audio rate, `recording_rate`).
    chosen_recording_rate: Option<f64>,
    /// The default load for the scope's output jacks (None: a line input).
    jack_load: Option<AudioLoad>,
    /// The jacks with their own loads, in reference order.
    jack_loads: Vec<(String, AudioLoad)>,
    /// The last lowering's output jack count, including open jacks. A scope with a jack runs at
    /// the audio rate.
    scope_jacks: usize,
    /// Whether the last lowering's device plays through a speaker (`FwBinding::speakers` of the
    /// binding its process is made for). A speaker is heard by default once its device runs, so
    /// its run starts at the audio rate: the speaker's Listen, which comes once the device runs,
    /// then finds the rate it wants and the run is not made twice.
    scope_speakers: bool,
    /// The hearing recorder (`listen::JACK_TAP`): it tapes every voice a Listen could ask for, the
    /// voices of the scope's output jacks and of the bench's voltage probes as of the last lowering
    /// (`probe_voices`), so a Listen over a take at rest plays the take from it with no run again.
    /// The voices and window of the one installed (None: none), the Listen bus it tapes into (the
    /// views', `SimLive::listen_bus`, taken at the first scope with a jack or a probe), and the
    /// takes it stopped at their bound that were logged.
    jack_voices: Vec<TapVoice>,
    probe_voices: Vec<TapVoice>,
    jack_sink: Option<(Vec<TapVoice>, f64)>,
    /// Why the last Listen renders the take again (it could not play it from the hearing
    /// recorder), in the status while the take renders again (`SimStatus::rerender_reason`).
    relisten: Option<Arc<str>>,
    listen_bus: Option<Arc<ListenBus>>,
    bounded_seen: u64,
    /// The project's run settings as last followed (`follow_settings`; None: no project).
    settings_seen: Option<SimSettings>,
    /// Stopped by the user: nothing lowers or runs until a Play, Step or firmware step.
    stopped: bool,
    /// The stopped run whose worker has not ended yet: no new run starts before it hands its sinks
    /// back.
    stopping: Option<u64>,
    /// A stopped run's sinks, for the next run.
    parked_sinks: Option<Box<SinkSet>>,
    /// The lowering in flight at a Stop: its answer is dropped.
    discard_lowering: bool,
    /// The recording plays (`set_rendering`): a value edit goes in where it is heard (an `Edit`
    /// at the Listen bus's heard time), the run going back there; an edit that is no value starts
    /// the run over (`restart`).
    rendering: bool,
    /// The newest edit taken in where it was heard: its name and the run's time it applies from.
    heard_edit: Option<(Arc<str>, f64)>,
    /// Auto re-record ([`SimHost::set_auto_rerecord`]) and, with it off, the edits and presses
    /// waiting for the next record.
    auto_rerecord: bool,
    waiting: u32,
    restart: Option<Restart>,
    /// The numbering the live rows (values or wire flows) were last written under (None: cleared).
    rows_for: Option<u64>,
    /// The knobs a restart keeps, and the name of the edit that restarted the run, for the next
    /// run's start.
    keep_params: Option<BTreeMap<(String, String), f64>>,
    restart_edit: Option<Arc<str>>,
    /// The co-simulated process's identity (`set_cosim`), 0 for none.
    cosim_id: u128,
    /// The app's firmware glue (`cosim_building`): whether one makes the processes, the lowering
    /// it last read and whether it was still building that one's process then; a run of a
    /// lowering with firmware waiting for the glue (a first run for its first process, a run that
    /// starts over for the image the glue builds for its lowering), so its take starts with it;
    /// and whether the next run starts over (`restart_run`).
    cosim_glue: bool,
    cosim_building: (usize, bool),
    cosim_wait: Option<Box<RunStart>>,
    restart_hold: bool,
    /// The last committed design, the scope circuit of the running lowering and of the one in
    /// flight: an edit's name compares them.
    design: Option<Arc<Design>>,
    run_circuit: Option<Arc<Circuit>>,
    pending_circuit: Option<Arc<Circuit>>,
    /// What the nominal run simulates (`RunContent`), and where the next run's goes up for the
    /// Listen tap's recorder (`SimLive::runs`).
    content: Option<Arc<RunContent>>,
    run_slot: Arc<RunSlot>,
    /// The bench's expectations, judged on every run by a sink installed with each lowering
    /// (`install_judge`), and the slot its verdicts go up in. `judges` counts the judges installed.
    expects: Vec<Expect>,
    judged: Arc<Mutex<Judged>>,
    judges: u64,
}

/// The expectations judge's sink id (`SinkCmd`): a tag apart from the views' ids.
const JUDGE_SINK: u64 = 0x4A55_4447_0000_0000;

// Only the nominal run drains the microphone. Ensemble members hold for the same PCM
// through a track reader, including a member that joins after the codec was attached.
struct MicSources {
    pump: Arc<dyn InputSource>,
    readers: Inputs,
    attach: Option<LiveCmd>,
}

impl Default for SimHost {
    fn default() -> SimHost {
        SimHost::new()
    }
}

impl SimHost {
    /// A host that takes its threads from the Cx at the first `frame`.
    pub fn new() -> SimHost {
        let (done_tx, done_rx) = mpsc::channel();
        let spec = LiveSpec { ring: RING_SECONDS, ..LiveSpec::default() };
        let mut host = SimHost {
            spawn: None,
            libs: Arc::new(ModelLibs::generic()),
            bench: Arc::new(BenchSpec::default()),
            scope: None,
            spec,
            index: Arc::new(ValueIndex::default()),
            glow_parts: Arc::default(),
            glow_rows: Vec::new(),
            glow_for: None,
            numbering: 0,
            cmds: Commands::default(),
            playing: true,
            inputs: None,
            mic_input: None,
            take_asked: None,
            take_bench: None,
            take_live: false,
            take_user: None,
            mode: None,
            ring: RING_SECONDS,
            check_loop_end: false,
            finish: false,
            sinks: Sinks::default(),
            listening: BTreeSet::new(),
            feeds: BTreeMap::new(),
            member_count: None,
            members: Vec::new(),
            ensemble_lowered: None,
            run_spec: None,
            run_noise_dt: f64::INFINITY,
            cosim: None,
            co_reader: None,
            co_table: Vec::new(),
            op_seed: Arc::new(Latest::new()),
            plan_states: StateCache::default(),
            ahead: None,
            ahead_due: false,
            held_replace: None,
            replaces_held: 0,
            edit_committed: None,
            edit_hearing: None,
            runs: 0,
            played_scopes: BTreeSet::new(),
            first_play: false,
            preparing_since: None,
            expects: Vec::new(),
            judged: Arc::default(),
            judges: 0,
            members_wait: false,
            params: BTreeMap::new(),
            want: None,
            asked: None,
            lowering: false,
            key: None,
            pending: None,
            done_tx,
            done_rx,
            live: None,
            facts: Facts::default(),
            diags: Vec::new(),
            error: None,
            rescues: (0, None),
            rescues_seen: 0,
            split_seen: 0,
            plan_error: None,
            held: Vec::new(),
            touches: Touches::default(),
            presses: Vec::new(),
            turns: Vec::new(),
            places: Vec::new(),
            turned: Vec::new(),
            take: Take::new(TAKE_BYTES),
            kept: Vec::new(),
            shown: None,
            silent: None,
            heard_last: None,
            means: Means::default(),
            take_said: false,
            store: Arc::new(TraceStore::new(0, &[])),
            mini: crate::mini::MiniWriter::new(),
            ghost: None,
            values: None,
            seen: 0,
            traces_seen: 0,
            eye: Eye::default(),
            meter: RateMeter::default(),
            events: None,
            tex: LiveTextures::new(0.0, LiveLook::dark()),
            currents: None,
            currents_due: None,
            currents_from: 0,
            currents_gen: 0,
            scrub: None,
            emphasis_rev: 0,
            columns: BTreeMap::new(),
            probes: BTreeMap::new(),
            fine_rows: Vec::new(),
            envelope_asks: BTreeMap::new(),
            envelopes: BTreeMap::new(),
            envelope_copies: EnvelopeCopies::default(),
            details: BTreeMap::new(),
            sim_mode: SimMode::Fast,
            lowering_logic: false,
            cells: 0,
            rounds: None,
            rounds_for: None,
            chosen_rate: None,
            chosen_recording_rate: None,
            jack_load: None,
            jack_loads: Vec::new(),
            scope_jacks: 0,
            scope_speakers: false,
            jack_voices: Vec::new(),
            probe_voices: Vec::new(),
            jack_sink: None,
            relisten: None,
            listen_bus: None,
            bounded_seen: 0,
            settings_seen: None,
            stopped: false,
            stopping: None,
            parked_sinks: None,
            discard_lowering: false,
            rendering: false,
            heard_edit: None,
            auto_rerecord: true,
            waiting: 0,
            restart: None,
            rows_for: None,
            keep_params: None,
            restart_edit: None,
            cosim_id: 0,
            cosim_glue: false,
            cosim_building: (0, false),
            cosim_wait: None,
            restart_hold: false,
            design: None,
            run_circuit: None,
            pending_circuit: None,
            content: None,
            run_slot: Arc::default(),
        };
        // Fast from the start: the resolution the views ask for, not the spec's 0 (Exact)
        host.spec.res = host.wanted_res();
        host
    }

    /// A host on these threads (a test's, or a host stepped outside a Cx).
    pub fn with_spawn(spawn: Box<dyn Spawn>) -> SimHost {
        let mut host = SimHost::new();
        host.spawn = Some(spawn);
        host
    }

    /// On the host UI's signal and its own frames, after the snapshot drain: commits `design`,
    /// then everything `step` does with the `ElecLive` global's `sim`, then the rows' upload into it
    /// (the global lives in the Cx, so the host reaches it through `cx` rather than the call site
    /// holding both). `design` is the
    /// one the app runs: the committed design, or a preview it chose to run (a bench-only preview,
    /// whose stimulus change retunes the running lowering as a commit's would). Before the global
    /// exists (no view yet) only the commit is noted. `tick` says this call is a frame's (its
    /// `NextFrame`): the segment currents' job goes to the worker only then, at most one a frame
    /// whatever the signals. Returns true when the views have something new to read or a currents
    /// job waits: ask for one more frame (`cx.new_next_frame()`) so they look. Nothing needs to
    /// poll at rest.
    pub fn frame(&mut self, cx: &mut Cx, design: &Arc<Design>, tick: bool) -> bool {
        if self.mic_input.as_ref().is_some_and(|mic| mic.pump.horizon().is_none()) {
            self.mic_input = None;
            if self.run_live() { self.send_inputs(); }
            for member in self.members.iter_mut().flatten() {
                member.cmds.send(LiveCmd::Inputs(Inputs { sources: Vec::new(), want: Arc::default() }));
            }
        }
        if self.spawn.is_none() {
            self.spawn = Some(Box::new(CxSpawn { threads: cx.thread_spawner(), run: RunPool::new(cx.task_pool()) }));
        }
        self.commit(design);
        if cx.has_global::<ElecLive>() {
            let settings = cx.get_global::<ElecLive>().snap.docs.as_ref().map(|docs| &docs.sim);
            if settings != self.settings_seen.as_ref() {
                self.follow_settings(settings.cloned());
            }
        }
        self.set_rendering(fab_elec_view::live::listen::frame(cx));
        if self.restart.is_some() {
            cx.new_next_frame();
        }
        if !cx.has_global::<ElecLive>() {
            return false;
        }
        let now = cx.seconds_since_app_start();
        let previous = self.values.clone();
        let seen = self.seen;
        let shown_before = self.shown.as_ref().map(|s| (s.t, s.moving, s.slot_t, s.revision));
        let mut changed = self.step_on(now, &mut cx.get_global::<ElecLive>().sim, tick);
        // No take is coming (the Scope's "No record: press Play": nothing recorded under this
        // numbering, no run recording, looping or live): the views show no live state rather than
        // claim one that is not recorded. While a run re-renders after an edit it plays, and the
        // last rows stay (no flicker). O(rows) once; a few comparisons a frame otherwise.
        let no_take = self.take.is_empty() && !self.continuous() && !self.realtime()
            && !self.plays();
        if no_take && self.rows_for.is_some() {
            self.tex.clear();
            self.rows_for = None;
            changed = true;
        }
        let shown_fresh = self.shown.as_ref().map(|s| (s.t, s.moving, s.slot_t, s.revision)) != shown_before;
        let fresh = match (&previous, &self.values) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        let was = cx.get_global::<ElecLive>().sim.status.clone();
        let was_rested = (!was.playing && !was.stepping) || was.steady;
        // Between publications the elapsed wait advances while preparing. Outside that wait an
        // unchanged frame reuses the status without repeating the rate-window arithmetic.
        let status = if fresh || was.preparing.is_some() || seen != self.seen || was.playing != cx.get_global::<ElecLive>().sim.playing
            || was.continuous != self.continuous() || was.realtime != self.realtime() || was.ring != self.ring || was.take != self.take_len()
            || was.rate_asked != self.spec.rate || was.mode != self.sim_mode || was.cells != self.cells || was.sample_rate_chosen != self.chosen_rate || was.jack_load != self.jack_load
            || was.plays_back != self.plays_back() || was.recording_rate_chosen != self.chosen_recording_rate || was.recording_rate != self.running_recording_rate().unwrap_or_else(|| self.recording_rate())
            || was.sample_rate != self.running_rate().unwrap_or_else(|| self.sample_rate()) || (self.live.is_none() && was.rate_achieved.is_some()) {
            LiveStatus::from(self.status())
        } else { was };
        let rested = (!status.playing && !status.stepping) || status.steady;
        // Pause is asynchronous: wait for the worker's stopped publication, whose part rows
        // show the instant rather than the eye's running mean. One Vec per stop (O(LEDs)),
        // or new values at rest (a step); no per-item work on an unchanged frame.
        let leds = (self.shown.is_none() && rested && (!was_rested || fresh)
            && self.values.as_ref().is_some_and(|v| !v.running || v.steady)).then(|| {
            Arc::new(self.led_values(design).map(|led| LiveLed {
                part: led.part, i: led.i, i_mean: led.i_mean,
                brightness: led.brightness, rgb: led.rgb,
            }).collect())
        });
        // Each LED's duty over the newest publication's interval, running or not (a Bench's LED
        // row): its mean anode current over its on-current, the largest current it drew under this
        // numbering and these values (an LED never lit reads 0). The chaser's lit LED reads full
        // and a PWM LED its duty whatever its resistor. Its parts once per numbering (O(LEDs x
        // components)), its levels one Vec per fresh publication (O(LEDs)); nothing on an
        // unchanged frame.
        let numbered = self.facts.generation == self.index.generation;
        if numbered && self.glow_for != Some(self.index.generation) {
            let (rows, parts): (Vec<(usize, f32)>, Vec<CompRef>) = self.facts.rows.emitters.iter()
                .filter_map(|e| Some(((e.pin as usize, 0.0), self.index.comp_at(e.part as usize)?))).unzip();
            (self.glow_rows, self.glow_parts) = (rows, Arc::new(parts));
            self.glow_for = Some(self.index.generation);
        }
        // At the instant shown from the take (heard, or reviewed), the duty there: its pin means.
        let glow = if self.shown.is_some() {
            (shown_fresh && numbered).then(|| {
                let pin = &self.means.pin;
                Arc::new(self.glow_rows.iter_mut().map(|(p, on)| {
                    let mean = pin.get(*p).copied().unwrap_or(0.0);
                    if mean.is_finite() { *on = on.max(mean); }
                    if *on > 1e-9 && mean.is_finite() { (mean / *on).clamp(0.0, 1.0) } else { 0.0 }
                }).collect::<Vec<f32>>())
            })
        } else {
            self.values.clone().filter(|v| fresh && numbered && v.generation == self.index.generation).map(|v| {
                Arc::new(self.glow_rows.iter_mut().map(|(pin, on)| {
                    let (now, mean) = (v.branch_i.get(*pin).copied().unwrap_or(0.0), v.pin_mean.get(*pin).copied().unwrap_or(0.0));
                    for i in [now, mean] { if i.is_finite() { *on = on.max(i); } }
                    if *on > 1e-9 && mean.is_finite() { (mean / *on).clamp(0.0, 1.0) } else { 0.0 }
                }).collect::<Vec<f32>>())
            })
        };
        let [nets, segs, parts, pins] = self.tex.upload(cx);
        let mini = self.mini.upload(cx);
        let live = &mut cx.get_global::<ElecLive>().sim;
        changed |= live.status != status || leds.is_some() || glow.is_some();
        live.status = status;
        // the jacks' own loads, for the Bench without a project; O(jacks), a new Arc on a change
        if *live.jack_loads != self.jack_loads {
            live.jack_loads = Arc::new(self.jack_loads.clone());
            changed = true;
        }
        let edit = self.heard_edit.as_ref().map(|e| e.0.clone());
        if live.edit.as_ref().map(Arc::as_ptr) != edit.as_ref().map(Arc::as_ptr) {
            live.edit = edit;
            changed = true;
        }
        if live.plan_error.as_ref().map(Arc::as_ptr) != self.plan_error.as_ref().map(Arc::as_ptr) {
            live.plan_error = self.plan_error.clone();
            changed = true;
        }
        if let Some(leds) = leds { live.leds = leds; }
        if !Arc::ptr_eq(&live.glow_parts, &self.glow_parts) {
            live.glow_parts = self.glow_parts.clone();
            changed = true;
        }
        if let Some(glow) = glow { live.glow = glow; }
        (live.nets, live.segs, live.parts, live.pins) = (Some(nets), Some(segs), Some(parts), Some(pins));
        live.mini = Some(mini);
        changed || self.currents_due.is_some() || self.currents.as_ref().is_some_and(CurrentsLink::waiting)
    }

    /// A view's request.
    pub fn act(&mut self, a: &SimAction) {
        match a {
            SimAction::Cmd(cmd) => {
                self.command(cmd);
            }
            SimAction::ShowScope { id, sink, column } => {
                if let Some(sink) = sink.take() {
                    self.sink(SinkCmd::Add(*id, sink));
                }
                if column.is_finite() && *column > 0.0 {
                    self.columns.insert(*id, *column);
                    self.sync_res();
                }
            }
            SimAction::Ensemble { id, feed } => {
                if let Some(mut feed) = feed.take() {
                    for (m, member) in self.members.iter_mut().enumerate() {
                        if let Some(member) = member {
                            if member.sinks.send(SinkCmd::Add(*id, feed.member(m as u32))) {
                                member.cmds.send(LiveCmd::Sinks);
                            }
                        }
                    }
                    self.feeds.insert(*id, feed);
                    self.sync_members();
                }
            }
            SimAction::HideScope { id } => {
                self.sink(SinkCmd::Remove(*id));
                if self.columns.remove(id).is_some() {
                    self.sync_res();
                }
                for member in self.members.iter_mut().flatten() {
                    if member.sinks.send(SinkCmd::Remove(*id)) {
                        member.cmds.send(LiveCmd::Sinks);
                    }
                }
                self.feeds.remove(id);
                self.sync_members();
            }
            SimAction::Emphasis { nets, parts } => self.emphasis(nets, parts),
            SimAction::EmphasisDevicePin(pin) => self.emphasis_device_pin(pin.as_deref()),
            SimAction::BeginGesture => self.begin_gesture(),
            SimAction::ClearGhost => self.clear_ghost(),
            SimAction::Listen { id, sink } => {
                if let Some(mut sink) = sink.take() {
                    // a take of finite length is heard from its start: a take at rest or still
                    // rendering plays from the hearing recorder's tape with no run again, else the
                    // run goes back there and the tap records it, the reason on the bar; a run with
                    // no take end (its windows go on past the first) the tap records as it runs, and
                    // a sink that hears only a running take goes in as it is
                    let rerecord = self.take_len().filter(|_| !sink.live_only()).map(|_| self.play_held(&mut *sink));
                    self.sink(SinkCmd::Add(*id, sink));
                    match rerecord {
                        Some(Ok(())) => {
                            log!("elec sim: Listen plays the take from its recording");
                            self.relisten = None;
                        }
                        Some(Err(why)) => {
                            log!("elec sim: Listen records the take again: {why}");
                            (self.relisten, self.heard_edit) = (Some(Arc::from(why)), None);
                            self.send_nominal(LiveCmd::Restart);
                        }
                        None => {}
                    }
                    if self.listening.insert(*id) && self.listening.len() == 1 {
                        self.send_nominal(LiveCmd::SetRate(self.spec.rate));
                        self.rate_check("the audio rate");
                    }
                }
            }
            SimAction::Unlisten { id } => {
                self.sink(SinkCmd::Remove(*id));
                if self.listening.remove(id) && self.listening.is_empty() {
                    self.send_nominal(LiveCmd::SetRate(self.spec.rate));
                }
            }
            SimAction::Probes { id, rows } => {
                if rows.is_empty() {
                    self.probes.remove(id);
                } else {
                    self.probes.insert(*id, rows.clone());
                }
                self.sync_whole();
            }
            SimAction::Envelope { id, net } => {
                if let Some(net) = net {
                    self.envelope_asks.insert(*id, *net);
                } else {
                    self.envelope_asks.remove(id);
                }
            }
            SimAction::LogicDetail { id, chunks } => {
                if chunks.is_empty() {
                    self.details.remove(id);
                } else {
                    self.details.insert(*id, chunks.clone());
                }
                self.sync_detail();
            }
        }
    }

    /// Sets the bench's take length (its `stop`). A change restores the default mode
    /// (`run_mode`): Realtime for a bench that opens live, else Loop for a named length and
    /// Continuous otherwise.
    pub fn set_take_len(&mut self, seconds: Option<f64>) {
        let bench = seconds.filter(|s| s.is_finite() && *s > 0.0);
        if self.take_bench != bench {
            self.take_bench = bench;
            if self.sync_take() {
                self.set_mode(None);
            }
        }
    }

    /// Sets the person's take length in place of the bench's (None: the bench's). The mode stays:
    /// a longer take renders on from where the run is, a shorter one keeps what is rendered.
    pub fn set_take_user(&mut self, seconds: Option<f64>) {
        let user = seconds.filter(|s| s.is_finite() && *s > 0.0);
        if self.take_user != user {
            self.take_user = user;
            if self.sync_take() {
                self.sync_until();
            }
        }
    }

    /// The take's length in effect from the person's and the bench's; whether it changed.
    fn sync_take(&mut self) -> bool {
        let asked = self.take_user.or(self.take_bench);
        let changed = self.take_asked != asked;
        self.take_asked = asked;
        changed
    }

    /// Sets whether the bench's take opens live (its `run: {live: true}`): with no choice of the
    /// transport's it runs in Realtime, as Record live. A change restores the default mode.
    pub fn set_take_live(&mut self, live: bool) {
        if self.take_live != live {
            self.change_mode(|host| (host.take_live, host.mode) = (live, None));
        }
    }

    /// The mode in effect: the transport's choice, else Realtime for a bench that opens live; None
    /// follows the take's length (Loop for a named one, Continuous otherwise).
    fn run_mode(&self) -> Option<RunMode> {
        self.mode.or(self.take_live.then_some(RunMode::Realtime))
    }

    fn continuous(&self) -> bool {
        self.run_mode().map_or(self.take_asked.is_none(), |mode| mode != RunMode::Loop)
    }

    fn realtime(&self) -> bool {
        self.run_mode() == Some(RunMode::Realtime)
    }

    fn set_mode(&mut self, mode: Option<RunMode>) {
        self.change_mode(|host| host.mode = mode);
    }

    /// Applies a change of the mode's inputs, then what entering or leaving Realtime asks.
    fn change_mode(&mut self, change: impl FnOnce(&mut Self)) {
        let was_realtime = self.realtime();
        change(self);
        self.sync_until();
        if self.realtime() != was_realtime {
            self.rendering = false;
            self.sync_jack_sink();
            // the rates left to the host follow the mode; a run nobody hears keeps its own
            if self.sample_rate() > 0.0 {
                self.rate_check("Realtime");
            }
            self.recording_rate_check("Realtime");
            // the scopes' zoom asks a resolution outside Realtime only
            self.sync_res();
            // Realtime traces only what the views show
            self.sync_fine();
        }
    }

    /// Limits the record in Continuous and keeps the whole take in Loop.
    fn sync_ring(&mut self) {
        let ring = if self.continuous() { self.ring } else { f64::INFINITY };
        if self.spec.ring != ring {
            self.spec.ring = ring;
            self.send(LiveCmd::SetRing(ring));
        }
    }

    /// Opens the run in Continuous. Loop ends at the bench's chunk-aligned take length, or at
    /// the ring length when the bench gives none.
    fn sync_until(&mut self) {
        self.sync_ring();
        let rate = self.recording_rate();
        let until = if self.continuous() {
            if self.finish {
                self.finish = false;
                self.send(LiveCmd::Pause);
            }
            f64::INFINITY
        } else {
            self.take_asked.map_or(self.ring, |seconds| window_frames(seconds, rate) as f64 / rate)
        };
        if until != self.spec.until {
            self.check_loop_end = until < self.spec.until && self.run_live();
            self.spec.until = until;
            self.send(LiveCmd::SetUntil(until));
            // the hearing recorder's window is the take's
            self.sync_jack_sink();
        }
    }

    /// The take's length (`set_take_len`), None for an open run. A run within [`TAKE_SLACK`] of
    /// it stands at its end.
    pub fn take_len(&self) -> Option<f64> {
        Some(self.spec.until).filter(|u| u.is_finite())
    }

    /// A finite run has finished and there is no Listen tap to play its recording.
    fn rests_without_listener(&self) -> bool {
        self.listening.is_empty() && self.values.as_ref().is_some_and(|v| !v.running && self.spec.until - v.t <= TAKE_SLACK)
    }

    /// Play was asked and something advances: the run, the player, or the silent clock.
    fn plays(&self) -> bool {
        self.playing && (!self.rests_without_listener() || self.silent.as_ref().is_some_and(|s| s.moving))
    }

    /// The take plays back: the views follow the instant it plays (`SimLive::heard`), by the
    /// player with sound (the Listen output plays the recording) or by the silent clock, moving or
    /// held. False while the views follow the run's front, or with no take. O(1).
    pub fn plays_back(&self) -> bool {
        !self.take.is_empty() && (self.silent.is_some() || self.rendering)
    }

    /// Play on a held take nobody hears: the silent clock plays it from the play head (the
    /// clock's, else the instant shown, else the run's front), from the take's start at its end.
    fn play_silent(&mut self) {
        let Some((a, b)) = self.take.span() else { return };
        let head = self.silent.as_ref().map(|s| s.t).or(self.shown.as_ref().map(|s| s.t))
            .unwrap_or_else(|| self.values.as_ref().map_or(a, |v| v.t));
        let t = if head < a || head >= b - TAKE_SLACK { a } else { head };
        self.silent = Some(Silent { t, moving: true, at: None, sought: None });
    }

    /// [`SimCmd::Seek`]: the silent clock's head to `t` within the take, moving or held as it was;
    /// over a held take nobody hears with no clock yet, a held one there. With a listener the
    /// views seek the player (`SampleRing::seek`). O(1).
    fn seek_silent(&mut self, t: f64) {
        let Some((a, b)) = self.take.span() else { return };
        let t = t.clamp(a, b);
        let held = self.rests_without_listener();
        match &mut self.silent {
            Some(s) => (s.t, s.at, s.sought) = (t, None, None),
            None if held => self.silent = Some(Silent { t, moving: false, at: None, sought: None }),
            None => {}
        }
    }

    /// The instant the take plays back this frame: the player's while it plays the recording
    /// (`heard`), else the silent clock's, moved on to `now` at the playback speed (the run's rate,
    /// 1 when unpaced) and round the take. The clock starts at Play over a held take nobody hears
    /// ([`Self::play_silent`]) and goes on from the player's last instant when the listener stops
    /// over one. When a listener starts it hands over: the ring is sought to its head (again each
    /// [`HANDOVER`] while no recorded window covers it) until the player plays there. A
    /// scrub holds it (the scrub shows). O(1).
    ///
    /// An edit at the head renders the take again from there and the clock plays on through it,
    /// waiting at the run's front while the run is behind it, as the player waits for the ring; it
    /// goes round only a whole take. It stops when the take goes, the run opens (Continuous), it
    /// starts over from its start (a shorter take, a Stop), or a listener takes over.
    fn playback(&mut self, now: f64, heard: Option<HeardNow>) -> Option<HeardNow> {
        let last = std::mem::replace(&mut self.heard_last, heard);
        let held = self.rests_without_listener();
        if self.silent.is_none() {
            let Some(h) = last.filter(|_| heard.is_none() && held && self.scrub.is_none()) else { return heard };
            self.silent = Some(Silent { t: h.t, moving: h.moving && self.playing, at: Some(now), sought: None });
        }
        let Some((a, b)) = self.take.span().filter(|_| held || !self.listening.is_empty() || self.take_len().is_some()) else {
            self.silent = None;
            return heard;
        };
        let speed = if self.spec.rate.is_finite() && self.spec.rate > 0.0 { self.spec.rate } else { 1.0 };
        let scrub = self.scrub.is_some();
        let front = self.values.as_ref().filter(|v| v.running && self.listening.is_empty()).map(|v| v.t);
        let s = self.silent.as_mut().expect("the clock runs");
        if let Some(h) = heard {
            if (h.t - s.t).abs() <= HANDOVER {
                self.silent = None;
                return heard;
            }
            if s.sought.is_none_or(|at| now - at >= HANDOVER) {
                if let Some(bus) = &self.listen_bus {
                    bus.ring.seek(s.t);
                }
                s.sought = Some(now);
            }
        }
        if scrub {
            s.at = None;
            return None;
        }
        if let (true, Some(at)) = (s.moving, s.at) {
            let t = s.t + (now - at).max(0.0) * speed;
            s.t = front.map_or(t, |f| t.min(f.max(s.t)));
        }
        s.at = Some(now);
        if s.t >= b && front.is_none() {
            s.t = if b > a { a + (s.t - a) % (b - a) } else { a };
        }
        Some(HeardNow { t: s.t, moving: s.moving, stamped: now })
    }

    /// A Listen over a take of finite length (never in Realtime, a run with no take end), at rest
    /// or still rendering: the tap plays the take from the hearing recorder's tape
    /// (`Sink::play_recorded`), its voices' columns bit for bit, when that tapes the take with them
    /// from its start, and follows it as it is made; otherwise why not, for the bar. On the UI
    /// thread before the tap goes in; no I/O, O(voices).
    fn play_held(&self, sink: &mut (dyn fab_elec_sim::Sink + Send)) -> Result<(), String> {
        let v = self.values.as_ref().filter(|_| self.run_live() && !self.stopped).ok_or_else(|| "the take is not rendered yet".to_owned())?;
        let at_rest = !v.running && self.take_len().is_some_and(|end| end - v.t <= TAKE_SLACK);
        let bus = self.listen_bus.as_ref().filter(|_| self.jack_sink.is_some()).ok_or_else(|| "nothing was recorded in this take".to_owned())?;
        sink.play_recorded(&bus.tapes, JACK_TAP, at_rest)
    }

    /// Returns to the start even on boards that run without rewind snapshots.
    fn restart_take(&mut self) {
        self.check_loop_end = false;
        self.heard_edit = None;
        self.silent = None;
        self.relisten = None;
        // the take records again from its start, with every voice there is to hear now
        self.sync_jack_sink();
        // Ensemble members keep no snapshots. A replaced circuit or a device that cannot fork
        // can also leave the nominal worker without its starting point.
        if self.members.is_empty() && self.values.as_ref().is_some_and(|v| v.can_restart) {
            self.send_nominal(LiveCmd::Restart);
        } else {
            let restart = self.restart.take().unwrap_or(Restart { name: None, since: None, keep_params: true });
            self.restart_run(restart);
        }
    }

    /// The bench's voltage probes (its Scope lanes, its strip's rows) are kept whole over the take
    /// whether a view shows them or not, under their own ask: a Scope opened later finds its lanes
    /// from the run's start. Called when the numbering changes; O(probes + rows).
    fn sync_bench_probes(&mut self) {
        let index = self.index.clone();
        self.bench_rows(&index);
        self.sync_whole();
    }

    /// The bench's voltage probes as `index`'s rows, among the probes kept whole. O(probes).
    fn bench_rows(&mut self, index: &ValueIndex) {
        let rows: Vec<usize> = self.bench.probes.iter().filter_map(|p| match p.probe {
            Probe::V(net) => index.net(net),
            _ => None,
        }).collect();
        if rows.is_empty() {
            self.probes.remove(&BENCH_LANES);
        } else {
            self.probes.insert(BENCH_LANES, rows);
        }
    }

    /// The signals any view draws over the whole run, by row. O(rows asked).
    fn whole_rows(&self) -> Arc<[bool]> {
        let len = self.probes.values().flatten().map(|&row| row + 1).max().unwrap_or(0);
        let mut whole = vec![false; len];
        for &row in self.probes.values().flatten() {
            whole[row] = true;
        }
        whole.into()
    }

    /// Sends the signals any view draws over the whole run to the runs when they changed
    /// (`LiveCmd::SetWhole`); new runs start with them (`start_run`). O(rows asked).
    fn sync_whole(&mut self) {
        let whole = self.whole_rows();
        if *whole != *self.spec.whole {
            self.spec.whole = whole;
            self.send(LiveCmd::SetWhole(self.spec.whole.clone()));
            let n = self.spec.whole.iter().filter(|&&w| w).count();
            log!("elec sim: the views probe {n} signals, kept whole over the take (the fine record {:.1} MB now)", self.store.bytes() as f64 / 1e6);
        }
        self.sync_fine();
    }

    /// The rows the trace writes (`LiveSpec::fine`): in Realtime, which must keep up, the rows a
    /// view shows, the probes kept whole and the nets with a strip; every net's interval
    /// statistics, the take's coarse envelope, go on and fill what the trace leaves out (a
    /// strip's gap, `MiniRing::fill_gaps`; a Scope lane's), into `out`. Out of Realtime, every
    /// row: false, `out` empty. O(rows).
    fn fine_rows_into(&self, out: &mut Vec<bool>) -> bool {
        out.clear();
        if !self.realtime() {
            return false;
        }
        out.extend_from_slice(&self.spec.whole);
        for row in self.mini.signals() {
            if out.len() <= row {
                out.resize(row + 1, false);
            }
            out[row] = true;
        }
        true
    }

    fn fine_rows(&mut self) -> Option<Arc<[bool]>> {
        let mut rows = std::mem::take(&mut self.fine_rows);
        let fine = self.fine_rows_into(&mut rows).then(|| Arc::from(&rows[..]));
        self.fine_rows = rows;
        fine
    }

    /// Sends the rows the trace writes to the runs when they changed (`LiveCmd::SetFine`): a
    /// view or a strip added has its rows at full rate from the run's next point on. O(rows) a
    /// call in Realtime, allocating only on a change; O(1) out of it.
    fn sync_fine(&mut self) {
        let mut rows = std::mem::take(&mut self.fine_rows);
        let on = self.fine_rows_into(&mut rows);
        let same = match &self.spec.fine {
            Some(fine) => on && fine[..] == rows[..],
            None => !on,
        };
        let fine = (!same).then(|| on.then(|| Arc::<[bool]>::from(&rows[..])));
        self.fine_rows = rows;
        if let Some(fine) = fine {
            self.spec.fine = fine.clone();
            match &fine {
                Some(rows) => log!("elec sim: Realtime traces the {} rows the views show", rows.iter().filter(|&&w| w).count()),
                None => log!("elec sim: the trace writes every row"),
            }
            self.send(LiveCmd::SetFine(fine));
        }
    }

    /// Sends the chunks all views show in detail to the nominal run when they changed
    /// (`LiveCmd::LogicDetail`). O(chunks asked).
    fn sync_detail(&mut self) {
        let mut chunks: Vec<(SignalId, f64)> = self.details.values().flatten().copied().collect();
        chunks.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        chunks.dedup();
        if chunks != self.spec.logic_detail {
            self.spec.logic_detail = chunks.clone();
            self.send(LiveCmd::LogicDetail(chunks));
        }
    }

    /// The signals the record keeps over the whole run (`LiveSpec::whole`), by signal.
    pub fn whole(&self) -> &[bool] {
        &self.spec.whole
    }

    /// The rows the trace writes (`LiveSpec::fine`): None, every one.
    pub fn fine(&self) -> Option<&[bool]> {
        self.spec.fine.as_deref()
    }

    /// The rate the nominal worker runs at: the bench's, or unpaced while a Listen tap is
    /// installed (the tap's ring then paces it on the audio device's clock). The eye's timebase
    /// and the ensemble members keep the bench's rate. O(1).
    fn worker_rate(&self) -> f64 {
        if self.listening.is_empty() { self.spec.rate } else { f64::INFINITY }
    }

    /// Whether someone keeps time with the nominal run as it is made (`LiveSpec::heard`): a Listen
    /// tap plays it, or it is paced to the wall clock. Not the hearing recorder, which never paces
    /// or holds it, and never an ensemble member. O(1).
    fn heard(&self) -> bool {
        !self.listening.is_empty() || self.spec.rate.is_finite()
    }

    /// The app's routing of a batch of actions: every `SimAction`, and the time cursor
    /// (`ViewAction::Cursor`).
    pub fn handle_actions(&mut self, actions: &Actions) {
        for a in actions {
            if let Some(s) = a.downcast_ref::<SimAction>() {
                self.act(s);
            } else if let Some(ViewAction::Cursor(c)) = a.downcast_ref::<ViewAction>() {
                self.cursor(*c);
            }
        }
    }

    /// A bench command by design id, sent at once. False for a `SetParam` this lowering cannot
    /// apply (the component has no tunable value: the change has no simulation effect), or board
    /// motion or sound without a matching part. A touch that produces no input is still accepted.
    pub fn command(&mut self, cmd: &SimCmd) -> bool {
        if matches!(cmd, SimCmd::Step(_) | SimCmd::StepFw(_)) {
            self.check_loop_end = false;
        }
        // a knob's new place, for every view that draws the knob where it is
        if let SimCmd::SetParam { comp, value } = *cmd {
            if self.index.knobs().iter().any(|k| k.0 == comp) {
                let generation = self.index.generation;
                let place = value.clamp(0.0, 1.0);
                self.turned.retain(|t| t.0 != comp && t.1 == generation);
                self.turned.push((comp, generation, place));
                // where it applies: where the take plays back, else the run's front
                let at = self.heard_at().unwrap_or_else(|| self.values.as_ref().map_or(0.0, |v| v.t));
                self.turns.push((comp, generation, at, place));
            }
        }
        // Play and either stop end a Pause that lets the take finish (a Pause may begin one again)
        if matches!(cmd, SimCmd::Play | SimCmd::Hold | SimCmd::Pause) {
            self.finish = false;
        }
        // Pause and the transport's Stop hold the silent clock where it is, as they hold the player
        if let (SimCmd::Hold | SimCmd::Pause, Some(s)) = (cmd, &mut self.silent) {
            s.moving = false;
        }
        match *cmd {
            // A board input goes on the run's timeline at the run's time as the host
            // sees it, or, while the recording plays, where the listener hears it: one edit there
            SimCmd::Board(BoardInput::Touch { comp, x, y, down }) => {
                if let Some(cmd) = self.touches.command(&self.index, comp, x, y, down) {
                    self.input_heard(vec![cmd]);
                }
                return true;
            }
            SimCmd::Board(BoardInput::Sound { ref track, .. }) if !track.0.is_closed() => {
                let cmds = self.index.command(cmd);
                if cmds.is_empty() { return false; }
                // Capture starts at the live front. Keep its attach there even when the
                // listener is hearing an older window that a WAV play would edit.
                let front = self.values.as_ref().map_or(0.0, |v| v.t);
                for cmd in cmds {
                    let attach = LiveCmd::At(front, Box::new(cmd));
                    if let Some(mic) = &mut self.mic_input { mic.attach = Some(attach.clone()); }
                    self.send(attach);
                }
                return true;
            }
            SimCmd::Board(BoardInput::Motion { .. } | BoardInput::Track { .. } | BoardInput::Sound { .. }) => {
                let cmds = self.index.command(cmd);
                if cmds.is_empty() { return false; }
                self.input_heard(cmds);
                return true;
            }
            // A press of a momentary input is two events on the run's timeline: down at the run's
            // time as the host sees it (the worker runs at most a publication ahead), or where the
            // listener hears the recording, up at least `MIN_PRESS` of simulated time later. Both
            // go at once: the release never waits on the wall clock, so a recording of the run
            // replays the same press.
            SimCmd::SetParam { comp, value } if self.index.momentary().contains(&comp) => {
                let heard = self.heard_at();
                if value != 0.0 {
                    self.note_waiting();
                }
                let now = heard.unwrap_or_else(|| self.values.as_ref().map_or(0.0, |v| v.t));
                let at = self.held.iter().position(|h| h.0 == comp && h.3.is_none());
                let t = if value != 0.0 {
                    self.held.retain(|h| h.0 != comp);
                    self.held.push((comp, self.index.generation, now, None));
                    self.presses.push((comp, self.index.generation, now, None));
                    now
                } else if let Some(at) = at {
                    let up = now.max(self.held[at].2 + MIN_PRESS);
                    self.held[at].3 = Some(up);
                    if let Some(p) = self.presses.iter_mut().rev().find(|p| p.0 == comp && p.3.is_none()) {
                        p.3 = Some(up);
                    }
                    up
                } else {
                    now
                };
                let cmds = self.index.command(cmd);
                if cmds.is_empty() {
                    return false;
                }
                // heard, the run goes back to the press: one edit at its time, said on the bar
                match heard {
                    Some(_) => {
                        self.relisten = None;
                        if value != 0.0 {
                            self.heard_edit = self.scope_circuit(comp.board).map(|c| (Arc::from(press_edit(c, comp)), t));
                        }
                        self.send(LiveCmd::Edit(cmds.into_iter().map(|cmd| (t, cmd)).collect()));
                    }
                    None => {
                        for cmd in cmds {
                            self.send(LiveCmd::At(t, Box::new(cmd)));
                        }
                    }
                }
                return true;
            }
            // While the take plays back (heard, or on the silent clock), a value edit goes in where it
            // plays: the run goes back there and renders on with it, played from there as the take
            // plays on
            SimCmd::SetParam { comp, value } if self.plays_take() => {
                let cmds = self.index.command(cmd);
                if cmds.is_empty() {
                    return false;
                }
                let knob = self.index.knobs().iter().any(|k| k.0 == comp);
                let name = self.scope_circuit(comp.board).map(|c| param_edit(c, comp, value, knob));
                self.edit_heard(cmds, name);
                return true;
            }
            SimCmd::Stop => {
                self.restart = None;
                self.stop();
                return true;
            }
            // Pause holds what is heard (or the silent clock) and lets the take render to its end
            SimCmd::Pause if self.take_len().is_some() && self.plays_take() => {
                self.playing = false;
                self.finish = true;
                return true;
            }
            SimCmd::Play | SimCmd::Step(_) | SimCmd::StepFw(_) if self.stopped => {
                // After a Stop the run starts again from t = 0: a new lowering of the same design,
                // once the stopped worker has handed its sinks back, with the run's values: the
                // knobs where they stand, turned before the Stop or while stopped.
                self.stopped = false;
                self.leave_ahead();
                self.keep_params = Some(self.params.clone());
                self.relower();
            }
            SimCmd::SetSimMode(mode) => {
                self.set_sim_mode(mode);
                return true;
            }
            SimCmd::SetContinuous(on) => {
                self.set_mode(Some(if on { RunMode::Continuous } else { RunMode::Loop }));
                return true;
            }
            SimCmd::SetRealtime(on) => {
                self.set_mode(Some(if on { RunMode::Realtime } else { RunMode::Continuous }));
                return true;
            }
            SimCmd::SetRing(seconds) => {
                if seconds.is_finite() && seconds > 0.0 && seconds != self.ring {
                    self.ring = seconds;
                    self.sync_until();
                }
                return true;
            }
            // Play on a held take nobody hears plays it back by the silent clock: the run is not
            // simulated again (an edit or a record does that); a clock held while an edit renders
            // the take again moves on, and the run goes on rendering
            SimCmd::Play if self.rests_without_listener() || self.silent.is_some() => self.play_silent(),
            SimCmd::SetSampleRate(rate) => {
                self.set_sample_rate(rate);
                return true;
            }
            SimCmd::SetRecordingRate(rate) => {
                self.set_recording_rate(rate);
                return true;
            }
            SimCmd::SetJackLoad(load) => {
                self.set_jack_load(load);
                return true;
            }
            SimCmd::SetJackLoadOf(ref jack, load) => {
                let mut loads = self.jack_loads.clone();
                loads.retain(|(name, _)| name != jack);
                if let Some(load) = load {
                    loads.push((jack.clone(), load));
                }
                self.set_jack_loads(&loads);
                return true;
            }
            SimCmd::SetTakeLen(seconds) => {
                self.set_take_user(seconds);
                return true;
            }
            SimCmd::SetAutoRerecord(on) => {
                self.set_auto_rerecord(on);
                return true;
            }
            SimCmd::Seek(t) => {
                self.seek_silent(t);
                return true;
            }
            _ => {}
        }
        let cmds = self.index.command(cmd);
        if cmds.is_empty() {
            return false;
        }
        for cmd in cmds {
            self.send(cmd);
        }
        true
    }

    /// Stop: the run ends (its worker and the ensemble's, as their command channels close) and its
    /// state goes: values, record, the take and its review, rows (off, `lights` moved), held
    /// presses and fingers. The play state is paused. The lowering and the scopes stay: the next
    /// Play, Step or firmware step runs the same design again from t = 0, the scopes' sinks taken
    /// over from this run. The lowering staying keeps the firmware bound to it ([`Self::lowered`]
    /// unchanged), so the next run makes its process at its start, the same program not built
    /// again. O(rows) for the cleared rows.
    fn stop(&mut self) {
        self.inputs = None;
        self.mic_input = None;
        self.check_loop_end = false;
        self.content = None;
        self.run_slot.set(None);
        self.held.clear();
        self.touches.clear();
        self.presses.clear();
        // the take's turns go with it; the knobs stay where they stand, as the run's values do
        self.turns.clear();
        self.clear_envelopes();
        self.take.clear();
        self.take_said = false;
        self.shown = None;
        self.silent = None;
        self.heard_last = None;
        self.playing = false;
        self.finish = false;
        self.stopped = true;
        // a lowering in flight belongs to the stopped run, and so does one waiting for its process
        // or for its warm ahead (the warm goes on: a Play's start may find its key planned)
        self.discard_lowering = self.lowering;
        self.drop_held(false);
        (self.edit_committed, self.edit_hearing) = (None, None);
        self.cosim_wait = None;
        self.restart_hold = false;
        if self.live.take().is_some() {
            self.stopping = Some(self.runs);
        }
        self.cmds.tx = None;
        self.cmds.queue.clear();
        self.members.clear();
        self.run_spec = None;
        self.values = None;
        self.preparing_since = None;
        self.scrub = None;
        self.store = Arc::default();
        self.eye = Eye::default();
        self.meter.clear();
        self.tex.clear();
        self.rows_for = None;
    }

    /// The inputs whose release the run reached by `t` stop showing held; a press of another
    /// numbering is gone with its run. O(held inputs) per fresh publication.
    fn release_held(&mut self, t: f64) {
        let generation = self.index.generation;
        self.held.retain(|h| h.1 == generation && h.3.is_none_or(|up| t < up));
    }

    /// Every publication since the last frame into the take: its grid frames and the device's
    /// state; while the recording plays, the instant heard (`SampleRing::heard_now`), None while
    /// the cursor reviews or nobody listens. O(new frames x rows), plus a fold of the oldest
    /// stretch now and then once the take is past its budget.
    fn take_frames(&mut self) -> Option<HeardNow> {
        if let Some(w) = &self.live {
            w.values.take_kept(&mut self.kept);
        }
        let generation = self.facts.generation;
        let mut started_in_rounds = false;
        for lv in self.kept.drain(..) {
            if lv.generation != generation {
                continue;
            }
            // The one publication that says started may fall between two frames.
            started_in_rounds |= lv.started && lv.in_rounds;
            for frame in &lv.frames {
                self.take.push(generation, lv.grid, frame);
            }
            if let Some(co) = &lv.co {
                self.take.push_co(lv.t, co);
            }
        }
        if started_in_rounds && self.rounds.is_none() && !self.exact() {
            self.rounds = Some(true);
            self.rounds_for = self.key.as_ref().map(|k| (k.circuit, k.scope));
            log!("elec sim: the run started in rounds; the board's next runs start in them");
        }
        if self.take.generation().is_some_and(|g| g != generation) {
            self.take.clear();
            self.clear_envelopes();
        }
        if !self.take_said {
            if let Some((a, b)) = self.take.span().filter(|(a, b)| b - a >= 1.0) {
                self.take_said = true;
                let rows = self.values.as_ref().map_or((0, 0, 0), |v| (v.net_mean.len(), v.pin_mean.len(), v.part_mean.len()));
                let probes = self.spec.whole.iter().filter(|&&w| w).count();
                log!("elec sim: the take: {:.2} s simulated, {:.1} MB, {:.2} MB a simulated second; rows {} nets, {} pins, {} parts; the fine record {:.1} MB ({} probes whole)",
                    b - a, self.take.bytes() as f64 / 1e6, self.take.bytes() as f64 / 1e6 / (b - a), rows.0, rows.1, rows.2, self.store.bytes() as f64 / 1e6, probes);
            }
        }
        let ring = self.listen_bus.as_ref().filter(|bus| bus.ring.is_open())?;
        ring.ring.heard_now(clock_now()).filter(|_| self.rendering && self.scrub.is_none() && !self.take.is_empty())
    }

    /// The rows at the instant the take shows: the cursor's while it reviews (one frame's means),
    /// else the instant heard (the means since the instant shown last; one frame's when the sound
    /// holds or went back), with the device's state, the segment currents' job and the time. Again
    /// when a frame came or changed there since. True when something changed. O(frames in the
    /// interval x rows).
    fn show_at(&mut self, heard: Option<HeardNow>) -> bool {
        let at = match (self.scrub, heard) {
            (Some(t), _) => Some((t, false)),
            (None, Some(h)) => Some((h.t, h.moving)),
            (None, None) => None,
        };
        let Some((t, moving)) = at.filter(|_| !self.take.is_empty()) else {
            if self.shown.take().is_some() {
                // back to the front: its newest publication writes the rows again
                self.seen = 0;
                return true;
            }
            return false;
        };
        let revision = self.take.revision();
        let end = self.take.span().map_or(t, |s| s.1);
        if self.shown.as_ref().is_some_and(|s| s.t == t && s.moving == moving && s.revision == revision && (s.slot_t >= t || end <= s.slot_t)) {
            return false;
        }
        let from = match &self.shown {
            Some(s) if moving && s.revision == revision => s.t,
            _ => t,
        };
        let Some(slot_t) = self.take.means(from, t, &mut self.means) else {
            return false;
        };
        let m = &self.means;
        let co = self.take.co_at(t).cloned();
        let values = RowValues { net_v: &m.net, pin_i: &m.pin, led_i: &m.pin, part_p: &m.part };
        self.tex.write_values(&self.facts.rows, &values);
        self.rows_for = Some(self.facts.generation);
        self.tex.write_pins(&self.facts.rows, co.as_ref(), self.co_reader.as_ref());
        let pin_i = Arc::from(&m.pin[..]);
        self.due_currents(pin_i);
        self.shown = Some(Shown { t, moving, co, slot_t, revision });
        true
    }

    fn clear_envelopes(&mut self) {
        self.envelopes.clear();
        self.envelope_copies.pending.clear();
    }

    /// Publish the requested nets from the take with the current numbering.
    fn publish_envelopes(&mut self, live: &mut SimLive) -> bool {
        let numbered = self.take.generation() == Some(self.index.generation);
        self.envelopes.retain(|id, kept| numbered && self.envelope_asks.get(id) == Some(&kept.published.net));
        if numbered {
            for (&id, &net) in &self.envelope_asks {
                if let Some(row) = self.index.net(net) {
                    self.envelopes.entry(id).or_insert_with(|| KeptEnvelope::new(id, net))
                        .update(&self.take, row, &mut self.envelope_copies);
                } else {
                    self.envelopes.remove(&id);
                }
            }
        }
        if let Some(spawn) = self.spawn.as_deref() {
            self.envelope_copies.pump(spawn);
        }
        let requested = self.envelopes.values().map(|e| &e.published).filter(|e| !e.slots.is_empty());
        let key = |e: &NetEnvelope| (e.id, e.net, e.grid, Arc::as_ptr(&e.slots));
        if requested.clone().map(key).eq(live.envelopes.iter().map(key)) {
            return false;
        }
        live.envelopes = Arc::new(requested.cloned().collect());
        true
    }

    /// A lowering's values: a restart for a knob keeps the run's values and the knobs' places,
    /// carried into the new numbering (a knob the lowering no longer has drops out); any other
    /// lowering puts the circuit's own values back, and with them the knobs where the design has
    /// them. The one place the run's values and the knobs' faces change together.
    fn lowered_values(&mut self, index: &ValueIndex) {
        match self.keep_params.take() {
            Some(params) => {
                self.params = params;
                self.turned.retain(|t| index.knobs().iter().any(|k| k.0 == t.0));
                for t in &mut self.turned {
                    t.1 = index.generation;
                }
            }
            None => {
                self.params = Default::default();
                self.turned.clear();
            }
        }
    }

    /// A new take's turns: each turned knob at t = 0, where the run starts it (its value is in
    /// the run's start), so the take shows it there from its first instant.
    fn start_take_turns(&mut self, generation: u64) {
        self.turns.clear();
        self.turns.extend(self.turned.iter().filter(|t| t.1 == generation).map(|&(comp, numbering, place)| (comp, numbering, 0.0, place)));
    }

    /// The knobs turned in this numbering, published when their places change: at the instant
    /// heard while the recording plays (each knob's newest turn of the take at or before it), else
    /// at the run's front. O(turns of the take × turned knobs), a Vec only when they change.
    fn publish_turned(&mut self, live: &mut SimLive) -> bool {
        let generation = self.index.generation;
        self.turned.retain(|t| t.1 == generation);
        let Some(t) = self.shown.as_ref().map(|s| s.t) else {
            if live.turned.len() == self.turned.len() && self.turned.iter().zip(live.turned.iter()).all(|(t, l)| (t.0, t.2) == *l) {
                return false;
            }
            live.turned = Arc::new(self.turned.iter().map(|t| (t.0, t.2)).collect());
            return true;
        };
        self.places.clear();
        for &(comp, numbering, at, place) in &self.turns {
            if numbering != generation || at > t {
                continue;
            }
            match self.places.iter_mut().find(|p| p.0 == comp) {
                Some(p) if at >= p.1 => (p.1, p.2) = (at, place),
                Some(_) => {}
                None => self.places.push((comp, at, place)),
            }
        }
        if live.turned.len() == self.places.len() && self.places.iter().zip(live.turned.iter()).all(|(p, l)| (p.0, p.2) == *l) {
            return false;
        }
        live.turned = Arc::new(self.places.iter().map(|p| (p.0, p.2)).collect());
        true
    }

    /// The inputs held: at the instant heard while the recording plays (the presses kept), else
    /// at the run's front. O(held inputs), a Vec only when they change.
    fn publish_held(&self, live: &mut SimLive) -> bool {
        let generation = self.index.generation;
        let shown = self.shown.as_ref().map(|s| s.t);
        let held_at = |p: &(CompRef, u64, f64, Option<f64>), t: f64| p.1 == generation && p.2 <= t && p.3.is_none_or(|up| t < up);
        let (n, same) = match shown {
            Some(t) => (
                self.presses.iter().filter(|p| held_at(p, t)).count(),
                self.presses.iter().filter(|p| held_at(p, t)).all(|p| live.held.contains(&p.0)),
            ),
            None => (self.held.len(), self.held.iter().all(|h| live.held.contains(&h.0))),
        };
        if n == live.held.len() && same {
            return false;
        }
        live.held = Arc::new(match shown {
            Some(t) => self.presses.iter().filter(|p| held_at(p, t)).map(|p| p.0).collect(),
            None => self.held.iter().map(|h| h.0).collect(),
        });
        true
    }

    /// Once per committed generation.
    pub fn on_commit(&mut self, snap: &DesignSnapshot) {
        self.commit(&snap.design);
    }

    /// A committed design to simulate (the same generation twice is one lowering; one whose lowering
    /// inputs are the running lowering's keeps it).
    pub fn commit(&mut self, design: &Arc<Design>) {
        if !self.design.as_ref().is_some_and(|d| Arc::ptr_eq(d, design)) {
            self.design = Some(design.clone());
        }
        if self.asked != Some(design.generation) {
            self.asked = Some(design.generation);
            self.want = Some(design.clone());
            if self.stopped {
                self.edited_stopped();
            } else if self.live.is_some() {
                self.edit_committed = Some(clock_now());
            }
        }
    }

    /// The bench's stimulus (stage-0 MCU stubs, pressed buttons): from the next commit on.
    pub fn set_bench(&mut self, bench: BenchSpec) {
        self.bench = Arc::new(bench);
        // an ideal probe lowers nothing: its net is kept whole under the run's numbering now
        self.sync_bench_probes();
        self.relower();
    }

    /// The circuit to simulate: a design unit, or a block bench's own circuit (default: the design's first board).
    pub fn set_scope(&mut self, scope: Option<BenchScope>) {
        self.scope = scope;
        self.relower();
    }

    /// The live run's settings (rate, largest step from the scope bandwidth, record length): from
    /// the next start of the worker on. The trace carries nets only: the take has the pins.
    pub fn set_live_spec(&mut self, spec: LiveSpec) {
        // the views' probes stay asked
        self.spec = LiveSpec { trace_pins: false, whole: self.spec.whole.clone(), fine: self.spec.fine.clone(), until: self.spec.until, ring: self.spec.ring, ..spec };
        self.spec.res = self.wanted_res();
    }

    /// Exact (resolution 0: every edge and tail resolved, today's stepper), Fast (the time
    /// resolution follows the finest view, `res`) or Logic (Fast on a lowering with the timing
    /// cells, `Lowered::with_cells`); the running workers change their resolution at their next
    /// step boundary. A switch to or from Logic lowers again, and a running run takes the new
    /// lowering as a `Replace`: time goes on, node voltages and device states carry over (a cell
    /// takes its input's voltage and its switches' positions, and gives them back).
    pub fn set_sim_mode(&mut self, mode: SimMode) {
        let was = std::mem::replace(&mut self.sim_mode, mode);
        self.sync_res();
        // Exact steps the whole circuit: rounds are Fast's
        if (was == SimMode::Exact) != (mode == SimMode::Exact) && self.rounds == Some(true) {
            self.send(LiveCmd::SetRounds(mode != SimMode::Exact));
        }
        if (was == SimMode::Logic) != (mode == SimMode::Logic) {
            self.relower();
            if let Some(design) = self.design.clone() {
                self.commit(&design);
            }
        }
    }

    pub fn sim_mode(&self) -> SimMode {
        self.sim_mode
    }

    /// Fast goes in rounds, its blocks spread over the helper cores, once the whole run falls
    /// behind real time (or the slower rate asked) over a full meter window with the solver its
    /// bound: rounds trade a round's lag at their cuts for speed, which a run that keeps up does
    /// not need. Once a run; a switch to Exact and back keeps the choice. A few comparisons a
    /// fresh publication.
    fn ask_rounds(&mut self, lv: &LiveValues) {
        if self.rounds.is_some() || self.exact() || !lv.running || lv.steady || !self.meter.full() {
            return;
        }
        let Some(achieved) = self.meter.achieved() else { return };
        if achieved >= KEEP_UP * self.spec.rate.min(1.0) || self.meter.split().is_some_and(|s| s.largest() != Limit::Solver) {
            return;
        }
        self.rounds = Some(true);
        self.rounds_for = self.key.as_ref().map(|k| (k.circuit, k.scope));
        log!("elec sim: the run reaches x{achieved:.2} of real time; it goes on in rounds");
        self.send(LiveCmd::SetRounds(true));
    }

    pub fn exact(&self) -> bool {
        self.sim_mode == SimMode::Exact
    }

    /// This run's rounds (`ask_rounds`): None while undecided, true in them, false with no helper
    /// cores to spread them over.
    pub fn rounds(&self) -> Option<bool> {
        self.rounds
    }

    /// The project's run settings (its system document's `sim` statement,
    /// `fab_elec_lang::sys_sim`), once a frame: applied when they change, a mode left out being
    /// Fast and a rate left out the host's choice. None (no project, a standalone scope) leaves
    /// the commands' settings (`SimCmd::SetSimMode`, `SimCmd::SetSampleRate`) as they are.
    /// O(jack loads) when unchanged.
    pub fn follow_settings(&mut self, settings: Option<SimSettings>) {
        if settings == self.settings_seen {
            return;
        }
        if let Some(settings) = &settings {
            let mode = settings.mode.unwrap_or_default();
            if mode != self.sim_mode {
                self.set_sim_mode(mode);
            }
            self.set_sample_rate(settings.sample_rate);
            self.set_recording_rate(settings.recording_rate);
            self.set_jack_load(settings.audio_load);
            self.set_jack_loads(&settings.jack_loads);
            self.set_take_user(settings.recording_length);
        }
        self.settings_seen = settings;
    }

    /// What the scope's output jacks drive (None: a line input): from the next lowering on, which
    /// this asks for. A playing run with jacks takes it in place, as a bench edit; while its
    /// recording plays, the run starts over with it.
    pub fn set_jack_load(&mut self, load: Option<AudioLoad>) {
        if load != self.jack_load {
            self.jack_load = load;
            if self.scope_jacks > 0 {
                self.relower();
            }
        }
    }

    /// The loads chosen for individual output jacks. A change relowers a scope with jacks.
    pub fn set_jack_loads(&mut self, loads: &[(String, AudioLoad)]) {
        let loads: Vec<_> = loads.iter().cloned().collect::<BTreeMap<_, _>>().into_iter().collect();
        if loads != self.jack_loads {
            self.jack_loads = loads;
            if self.scope_jacks > 0 {
                self.relower();
            }
        }
    }

    /// The bench with a resistance from each loaded channel to its jack's reference, after the
    /// bench's own sources, and the number of output jacks (open jacks included). Reuses the
    /// bench when no channel is loaded.
    fn with_jack_loads(bench: &Arc<BenchSpec>, default: Option<AudioLoad>, loads: &[(String, AudioLoad)], scope: BoardId, circuit: Option<&Circuit>) -> (Arc<BenchSpec>, usize) {
        let jacks = circuit.map(output_jacks).unwrap_or_default();
        let mut loaded = bench.clone();
        for jack in &jacks {
            let load = loads.binary_search_by(|(name, _)| name.cmp(&jack.refdes))
                .map_or(default.unwrap_or(AudioLoad::Line), |i| loads[i].1);
            let Some(ohms) = load.ohms() else { continue };
            let sources = &mut Arc::make_mut(&mut loaded).sources;
            for channel in jack.channels() {
                sources.push(BenchSource {
                    a: NetRef { board: scope, net: channel },
                    b: NetRef { board: scope, net: jack.reference },
                    kind: SourceKind::LoadR(ohms),
                });
            }
        }
        (loaded, jacks.len())
    }

    /// The user's sample rate, Hz (None: the host's choice, [`Self::sample_rate`]). A run at
    /// another rate starts over at this one, as after a value edit: the rate is fixed for a run's
    /// life.
    pub fn set_sample_rate(&mut self, rate: Option<f64>) {
        let rate = rate.filter(|hz| hz.is_finite() && *hz > 0.0);
        if rate != self.chosen_rate {
            self.chosen_rate = rate;
            self.rate_check("the sample rate");
        }
    }

    /// The sample rate a run that starts now takes, Hz: the user's pick, else the default audio
    /// rate ([`Self::default_rate`]) while a net is heard, the scope has an output jack or its
    /// device a speaker, else none (0: the run steps at its bandwidth).
    pub fn sample_rate(&self) -> f64 {
        match self.chosen_rate {
            Some(hz) => hz,
            None if !self.listening.is_empty() || self.scope_jacks > 0 || self.scope_speakers => self.default_rate(),
            None => 0.0,
        }
    }

    /// The audio rate the host picks for a heard run: [`REALTIME_SAMPLE_RATE`] in Realtime, which
    /// must keep up, else [`AUDIO_SAMPLE_RATE`].
    fn default_rate(&self) -> f64 {
        if self.realtime() { REALTIME_SAMPLE_RATE } else { AUDIO_SAMPLE_RATE }
    }

    /// The rate the host picks for a run's takes: [`REALTIME_RECORDING_RATE`] in Realtime, else
    /// [`AUDIO_SAMPLE_RATE`].
    fn default_recording_rate(&self) -> f64 {
        if self.realtime() { REALTIME_RECORDING_RATE } else { AUDIO_SAMPLE_RATE }
    }

    /// The running run's sample rate (None between runs).
    fn running_rate(&self) -> Option<f64> {
        self.run_spec.as_ref().filter(|_| self.run_live()).map(|spec| spec.sample_rate)
    }

    /// The user's recording rate, Hz (None: the default audio rate): the rate the run's takes
    /// (what Listen plays, the jacks' recording, a saved WAV) are made at, each sample the run's
    /// read at its instant (`listen::Band`), through a band-limited rate change at another rate
    /// than the run's. A run recording at another rate starts over, as after a value edit; its
    /// samples are the same, only its takes differ.
    pub fn set_recording_rate(&mut self, rate: Option<f64>) {
        let rate = rate.filter(|hz| hz.is_finite() && *hz > 0.0);
        if rate == self.chosen_recording_rate {
            return;
        }
        self.chosen_recording_rate = rate;
        self.sync_until();
        self.recording_rate_check("the recording rate");
    }

    /// A run recording at another rate than [`Self::recording_rate`] starts over at it, named `why`.
    fn recording_rate_check(&mut self, why: &str) {
        let want = self.recording_rate();
        if self.running_recording_rate().is_some_and(|hz| hz != want) {
            log!("elec sim: the run starts over recording at {want} Hz for {why}");
            self.restart_after(Some(why.to_owned()), true);
        }
    }

    /// The rate a run that starts now records at, Hz: the user's pick, else the host's
    /// ([`Self::default_recording_rate`]).
    pub fn recording_rate(&self) -> f64 {
        self.chosen_recording_rate.unwrap_or_else(|| self.default_recording_rate())
    }

    /// The running run's recording rate (None between runs).
    fn running_recording_rate(&self) -> Option<f64> {
        self.content.as_ref().filter(|_| self.run_live()).map(|content| content.recording_rate)
    }

    /// A run at another rate than [`Self::sample_rate`] starts over at it, named `why`: a picked
    /// rate, or the audio rate when Listen joins a run without one (its take then starts with
    /// the run, keyed). A heard run that stops being heard keeps its rate.
    fn rate_check(&mut self, why: &str) {
        let want = self.sample_rate();
        if self.running_rate().is_some_and(|hz| hz != want) {
            log!("elec sim: the run starts over at {} for {why}", if want > 0.0 { format!("{want} Hz") } else { "its bandwidth step".to_owned() });
            self.restart_after(Some(why.to_owned()), true);
        }
    }

    /// The time resolution the runs are asked to step at, seconds (`LiveCmd::SetRes`): 0 is Exact.
    pub fn res(&self) -> f64 {
        self.spec.res
    }

    /// The live spec the runs start with (the resolution as `res` says): Exact's is the bench's
    /// (`fab_elec_bench::bench_spec`).
    pub fn live_spec(&self) -> &LiveSpec {
        &self.spec
    }

    /// Exact asks 0. Fast asks a quarter of the finest pixel column a live view shows: the open
    /// scopes' and the eye's window (`EYE` of the playback timebase, what the LED glow and the rows
    /// integrate; none when unpaced), at most the largest step. In Realtime, which must keep up
    /// with what is heard, the scopes ask none: a view's zoom never sets what the run costs there,
    /// and a scope draws the record as it was stepped at any zoom. Loop and Continuous need not keep
    /// up, so a scope zoomed in on an edge gets it resolved. O(open scopes).
    fn wanted_res(&self) -> f64 {
        if self.exact() {
            return 0.0;
        }
        let eye = self.spec.rate.is_finite().then(|| EYE * self.spec.rate);
        let scopes = self.columns.values().copied().filter(|_| !self.realtime());
        let finest = scopes.chain(eye).filter(|c| c.is_finite() && *c > 0.0).fold(f64::INFINITY, f64::min);
        (finest / 4.0).min(self.spec.max_step)
    }

    /// Sends the wanted resolution to the runs when it changed: one command per change.
    fn sync_res(&mut self) {
        let res = self.wanted_res();
        if res != self.spec.res {
            self.send(LiveCmd::SetRes(res));
        }
    }

    /// A firmware event log for the runs' device, showing the live record's length of simulated
    /// time, and the feed the co-simulation maker gives the nominal run's device as its event sink
    /// (an ensemble member's device reports none). `source` says where a frame came from. It
    /// replaces an earlier log, whose feed then takes and discards. Each offer wakes the UI and
    /// `step` drains the log on that signal, never waiting; a full feed is the run's limit
    /// (`Limit::Events`). A new run (not a `Replace`) starts the store's visible span afresh.
    pub fn set_events<F: Send + 'static>(&mut self, source: fn(&F) -> EventSource) -> EventFeed<F> {
        let wake = self.spawn.as_deref().map_or_else(|| Arc::new(|| {}) as Arc<dyn Fn() + Send + Sync>, |s| s.waker());
        let (log, feed) = EventLog::new(self.spec.record, source, wake);
        self.events = Some(Box::new(log));
        feed
    }

    /// No event log; a feed made earlier takes and discards.
    pub fn clear_events(&mut self) {
        self.events = None;
    }

    /// The firmware event store, for the code that knows its frame type: its readers, its events
    /// and its visible length. None without a log or for another frame type.
    pub fn events<F: 'static>(&mut self) -> Option<&mut EventStore<F>> {
        self.events.as_mut()?.as_any_mut().downcast_mut::<EventLog<F>>().map(|log| &mut log.store)
    }

    /// The running lowering, shared with ensemble starts; after a Stop the last run's, until the
    /// next run's replaces it (the firmware is bound to it). O(1), without copying its contents.
    pub fn lowered(&self) -> Option<&Arc<Lowered>> {
        self.ensemble_lowered.as_ref()
    }

    /// The maker of the runs' co-simulated process (`set_cosim`), for a run of the app's own
    /// beside them (the counted bench check). O(1).
    pub fn cosim_maker(&self) -> Option<&CoSimMaker> {
        self.cosim.as_ref()
    }

    /// The co-simulated process of the runs (None: none). Workers that start from now on get
    /// theirs from `maker`; the running ones (the nominal run and every member) swap theirs at
    /// their current time (`LiveCmd::CoSim`), and a `Replace` keeps the attached process.
    ///
    /// `identity` names the process the maker makes (the firmware image, its clock and settings):
    /// equal for makers that make the same process. It goes into the run's content key.
    pub fn set_cosim(&mut self, cosim: Option<(CoSimMaker, u128)>) {
        let (maker, identity) = match cosim {
            Some((maker, identity)) => (Some(maker), identity),
            None => (None, 0),
        };
        let same = match (&self.cosim, &maker) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.cosim = maker.clone();
        let program = std::mem::replace(&mut self.cosim_id, identity);
        // a run waiting for its process starts with it, from t = 0
        if let Some(start) = self.cosim_wait.take() {
            self.start_run(*start);
            return;
        }
        // while the recording plays another program restarts the run, which makes its process;
        // the same program built again (a new maker) is no edit: the run has it already
        if self.rendering && self.run_live() {
            if identity != program {
                let edit = if program == 0 { "the firmware's first build" } else { "the firmware edit" };
                self.restart_after(Some(edit.to_owned()), true);
            }
            return;
        }
        let Some(lowered) = self.ensemble_lowered.clone() else { return };
        let make = |m: u32| maker.as_ref().and_then(|mk| mk(&lowered, m));
        if self.live.is_some() && self.cmds.tx.is_some() {
            let device = make(0);
            self.set_co_table(device.as_ref().map_or_else(Vec::new, |d| d.pins.clone()));
            self.send(LiveCmd::CoSim(CoSimSlot::new(device)));
        }
        let slots: Vec<Option<CoSimSlot>> = (0..self.members.len())
            .map(|m| self.members[m].as_ref().map(|_| CoSimSlot::new(make(m as u32 + 1)))).collect();
        for (member, slot) in self.members.iter_mut().zip(slots) {
            if let (Some(member), Some(slot)) = (member, slot) {
                member.cmds.send(LiveCmd::CoSim(slot));
            }
        }
    }

    /// The app's firmware glue says, each frame, the lowering it last read (`lowered`) and whether
    /// it is still building that one's process. With a glue, a run of a lowering with firmware
    /// waits for it: a first run for its first process, and a run that starts over (an edit while
    /// the recording plays) for the image the glue builds for the new lowering (an edit of what
    /// the image compiles in rebuilds it), so the run and its take start with that process rather
    /// than restarting again when it comes (`set_cosim`). Once the glue has read the lowering and
    /// builds nothing, the run starts with the process it has: an unchanged image, or after a
    /// failed build the last one (or none), which it says. A glue says so first as the host is
    /// made (`None`, false): a lowering can be taken in by the step that asked for it, before the
    /// glue's first frame. O(1).
    pub fn cosim_building(&mut self, lowered: Option<&Arc<Lowered>>, building: bool) {
        self.cosim_glue = true;
        self.cosim_building = (lowered.map_or(0, |l| Arc::as_ptr(l) as usize), building);
        let Some(wait) = self.cosim_wait.as_mut().filter(|w| Arc::as_ptr(&w.lowered) as usize == self.cosim_building.0) else { return };
        if building {
            wait.glue_built = true;
            return;
        }
        let Some(start) = self.cosim_wait.take() else { return };
        match (&self.cosim, start.glue_built) {
            (None, _) => log!("elec sim: the firmware did not build; the run starts without it"),
            (Some(_), true) => log!("elec sim: the firmware did not build; the run starts with the last image"),
            (Some(_), false) => {}
        }
        self.start_run(*start);
    }

    /// Fix the member count, or use available parallelism minus two (zero if unknown).
    /// Members run only while a lowering is live and a scope holds an ensemble feed.
    pub fn set_members(&mut self, n: Option<usize>) {
        self.member_count = n;
        self.sync_members();
    }

    /// The look (theme) the rows map values with.
    pub fn set_look(&mut self, look: LiveLook) {
        self.tex.set_look(look);
    }

    /// The hovered or probed nets and parts.
    pub fn emphasis(&mut self, nets: &[NetRef], parts: &[CompRef]) {
        let nets: Vec<u32> = nets.iter().filter_map(|&n| self.index.net(n)).map(|i| i as u32).collect();
        let parts: Vec<u32> = parts.iter().filter_map(|&c| self.index.comp(c)).map(|i| i as u32).collect();
        self.tex.emphasis(&nets, &parts);
        self.emphasis_rev += 1;
    }

    /// The hovered or probed pins (a pin's firmware state brightens).
    pub fn emphasis_pins(&mut self, pins: &[PinRef]) {
        let pins: Vec<u32> = pins.iter().filter_map(|&p| self.index.pin(p)).map(|i| i as u32).collect();
        self.tex.emphasis_pins(&pins);
        self.emphasis_rev += 1;
    }

    /// The co-simulated device's pin by its pin table name, and the net its pad is on: their rows
    /// are emphasised and every other net, part and pin row's emphasis cleared. None, or a name the
    /// lowering gives no pad, clears them all. O(nets + parts + pins) row writes.
    pub fn emphasis_device_pin(&mut self, name: Option<&str>) {
        let pin = name.and_then(|name| self.facts.rows.co_pins.get(name).copied());
        let net = pin.and_then(|pin| self.facts.rows.pin_net.get(pin as usize).copied().flatten());
        self.tex.emphasis(net.as_slice(), &[]);
        self.tex.emphasis_pins(pin.as_slice());
        self.emphasis_rev += 1;
    }

    /// The net texture's CPU rows (`live_nets`), by `ValueIndex::net`.
    pub fn nets(&self) -> &fab_elec_view::live::LiveRows {
        self.tex.nets()
    }

    /// How the pin rows read the co-simulated device's published state (None: the pin rows stay
    /// off). The app that makes the device (`set_cosim`) knows its state's type.
    pub fn set_co_pins(&mut self, reader: Option<CoPinReader>) {
        self.co_reader = reader;
        self.rewrite_pins();
    }

    /// A new pin table for the nominal run's device: the join to the running lowering's pins is
    /// built again, and the pin rows with it.
    fn set_co_table(&mut self, table: Vec<CoSimPin>) {
        if table == self.co_table {
            return;
        }
        self.co_table = table;
        if let Some(l) = &self.ensemble_lowered {
            self.facts.rows.co_pins = co_join(&self.co_table, &l.plan);
        }
        self.rewrite_pins();
    }

    /// The pin rows written again from the newest values (none while a scrub shows another instant).
    fn rewrite_pins(&mut self) {
        self.tex.clear_pins();
        if self.scrub.is_none() {
            let co = self.values.as_ref().and_then(|v| v.co.as_ref());
            self.tex.write_pins(&self.facts.rows, co, self.co_reader.as_ref());
        }
    }

    /// The shared time cursor. On the simulation's cursor, paused inside the take: a review, the
    /// rows show the take at its time and the run goes on; playing, or at the take's newest frame:
    /// the live values.
    pub fn cursor(&mut self, c: TimeCursor) {
        if c.source != CursorSource::Sim {
            return;
        }
        let end = self.take.span().map(|s| s.1);
        if !c.playing && end.is_some_and(|e| c.t < e) {
            // a review: the take shows the cursor's instant; the run goes on
            let first = self.take.span().map_or(c.t, |s| s.0);
            self.scrub = Some(c.t.max(first));
        } else if self.scrub.take().is_some() {
            // back to live: the newest values write the rows again
            self.seen = 0;
        }
    }

    /// A gesture starts (a knob press, a drag): the trace as it stands is kept as the ghost.
    pub fn begin_gesture(&mut self) {
        self.ghost = Some(self.store.clone());
    }

    pub fn clear_ghost(&mut self) {
        self.ghost = None;
    }

    pub fn ghost(&self) -> Option<&TraceStore> {
        self.ghost.as_deref()
    }

    /// The instant a review shows (the cursor paused inside the take, `cursor`), simulated
    /// seconds; None while the views follow the run or the take plays. O(1).
    pub fn review_instant(&self) -> Option<f64> {
        self.scrub
    }

    /// The run's one take: its grid frames and device states, the instant views review.
    pub fn take(&self) -> &Take {
        &self.take
    }

    /// The trace as last published: one signal per net, the rolling record's length of simulated
    /// time.
    pub fn store(&self) -> &TraceStore {
        &self.store
    }

    /// The mini-trace texture's CPU rows, addressed by subscribed slot.
    pub fn mini(&self) -> &fab_elec_view::live::LiveRows {
        self.mini.rows()
    }

    /// The pin texture's CPU rows (`live_pins`), by `ValueIndex::pin`.
    pub fn pins(&self) -> &fab_elec_view::live::LiveRows {
        self.tex.pins()
    }

    /// The newest values of the current lowering.
    pub fn values(&self) -> Option<&Arc<LiveValues>> {
        self.values.as_ref()
    }

    /// The commands sent to the run so far, those still queued for it included: a mark for
    /// [`Self::stepped_past`]. O(1).
    pub fn sent_mark(&self) -> SentMark {
        SentMark { run: self.runs, count: self.cmds.sent + self.cmds.queue.len() as u64 }
    }

    /// Whether the newest values were stepped with every command up to `mark`: the run stepped
    /// after it took them, or a run started since has published. What a wait for an edit's effect
    /// waits on, however slowly the run goes or the values reach the host. O(1).
    pub fn stepped_past(&self, mark: SentMark) -> bool {
        self.values.as_ref().is_some_and(|v| self.runs != mark.run || v.stepped_with >= mark.count)
    }

    /// What the transport shows (`elec.sim.status`): the newest values' time and state, the asked
    /// rate, the rate reached over the newest publications and what limits it while the run goes
    /// on. O(publications in the window).
    pub fn status(&self) -> SimStatus {
        let v = self.values.as_deref();
        let (t, stepping, running, steady) = v.map_or((0.0, false, false, false), |v| (v.t, v.stepping, v.running, v.steady));
        let paced = self.spec.rate.is_finite() && !stepping;
        let preparing = self.preparing_since.map(|since| Preparing { first: self.first_play, wall: clock_now() - since });
        let measuring = running && !steady && preparing.is_none();
        SimStatus {
            t,
            playing: self.plays(),
            plays_back: self.plays_back(),
            stepping,
            preparing,
            rate_asked: self.spec.rate,
            rate_achieved: self.meter.reached(),
            limited_by: if !measuring {
                None
            } else if self.events.as_ref().is_some_and(|log| log.full()) {
                Some(Limit::Events)
            } else if v.is_some_and(|v| v.held == Some(fab_elec_sim::HoldKind::Input)) {
                Some(Limit::Input)
            } else {
                self.meter.limited_by(self.spec.rate, paced)
            },
            split: if measuring { self.meter.split() } else { None },
            steady,
            res: v.map_or(0.0, |v| v.res),
            mode: self.sim_mode,
            cells: self.cells,
            sample_rate: self.running_rate().unwrap_or_else(|| self.sample_rate()),
            sample_rate_chosen: self.chosen_rate,
            recording_rate: self.running_recording_rate().unwrap_or_else(|| self.recording_rate()),
            recording_rate_chosen: self.chosen_recording_rate,
            jack_load: self.jack_load,
            take: self.take_len(),
            continuous: self.continuous(),
            realtime: self.realtime(),
            ring: self.ring,
            rerender: self.heard_edit.as_ref().map(|e| e.1).filter(|_| self.take_len().is_some_and(|end| end - t > TAKE_SLACK)),
            rerender_reason: self.relisten.clone().filter(|_| self.take_len().is_some_and(|end| end - t > TAKE_SLACK)),
            auto_rerecord: self.auto_rerecord,
            waiting: self.waiting,
        }
    }

    /// Each LED of the running lowering (`elec.sim.leds`): its anode current at `t`, the newest
    /// publication's interval mean, the light output its row shows now and its colour (the part's
    /// emitter; None without one). Empty before the lowering's first values. O(LEDs + components).
    pub fn leds(&self, design: &Design) -> Vec<LedLive> {
        self.led_values(design).collect()
    }

    fn led_values<'a>(&'a self, design: &'a Design) -> impl Iterator<Item = LedLive> + 'a {
        let values = self.values.as_deref().filter(|v| v.generation == self.facts.generation);
        values.into_iter().flat_map(move |v| {
            self.facts.rows.emitters.iter().filter_map(move |e| {
                let part = self.index.comp_at(e.part as usize)?;
                let comp = design.unit(part.board).and_then(|u| u.circuit.comps.get(part.comp.ix()));
                Some(LedLive {
                    part,
                    i: v.branch_i.get(e.pin as usize).copied().unwrap_or(f32::NAN),
                    i_mean: v.pin_mean.get(e.pin as usize).copied().unwrap_or(f32::NAN),
                    brightness: self.tex.parts().row(e.part as usize)[1],
                    rgb: comp.and_then(|c| c.part.emitter.as_ref()).map(|em| emitter_rgb(&em.light)),
                })
            })
        })
    }

    /// The latest values say the bench has settled: it publishes nothing until a command or an edit.
    pub fn settled(&self) -> bool {
        self.values.as_ref().is_some_and(|v| v.steady)
    }

    /// Whether a commit is still being lowered (or checked against the running lowering).
    pub fn lowering(&self) -> bool {
        self.lowering || self.want.is_some() || self.cosim_wait.is_some()
    }

    /// The source generation of the running lowering: the committed design's, or the block bench circuit's
    /// (0 before the first lowering). Values and the views' numbering carry the host's own number
    /// (`index().generation`) instead.
    pub fn generation(&self) -> u64 {
        self.facts.source.1
    }

    /// The running lowering's scope id (None before the first lowering).
    pub fn scope_id(&self) -> Option<BoardId> {
        (self.numbering != 0).then_some(self.facts.source.0)
    }

    /// The current lowering's diagnostics (parts not simulated, and why).
    pub fn diags(&self) -> &[LowerDiag] {
        &self.diags
    }

    /// The steps the live run took only through the stepper's gmin ladder (`Rescue`): how many
    /// over the session, and the newest. A run that goes on only because of it is visible here
    /// and in the log; it would have stopped with "time step too small".
    pub fn rescues(&self) -> (u64, Option<&Rescue>) {
        (self.rescues.0, self.rescues.1.as_ref())
    }

    /// Why the worker stopped, when it did (a circuit that does not solve).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Nothing runs and nothing will until a design is committed ([`Self::commit`]): not
    /// stopped, no run live, nothing lowering or waiting to lower, no stopped worker handing its
    /// sinks back, no run waiting for its firmware process. After a Play that follows a Stop the
    /// run starts again from the next commit; the app's `frame` commits every frame, so there
    /// this lasts a frame, while a caller that only steps the host must commit itself. O(1).
    pub fn awaits_commit(&self) -> bool {
        !self.stopped && self.live.is_none() && !self.lowering && self.want.is_none() && self.stopping.is_none()
            && self.cosim_wait.is_none() && self.restart.is_none()
    }

    /// `SetParam`s replaced while waiting for room in the command queue.
    pub fn coalesced(&self) -> u64 {
        self.cmds.coalesced
    }

    /// The current lowering's numbering.
    pub fn index(&self) -> &Arc<ValueIndex> {
        &self.index
    }

    /// The rows as written (for tests and CPU drawing).
    pub fn rows(&self) -> &LiveTextures {
        &self.tex
    }

    fn relower(&mut self) {
        self.asked = None;
    }

    fn send(&mut self, cmd: LiveCmd) {
        if let Some(spec) = &mut self.run_spec {
            match &cmd {
                LiveCmd::Play => spec.playing = true,
                LiveCmd::Pause | LiveCmd::Step(_) | LiveCmd::CoStep(_) => spec.playing = false,
                LiveCmd::SetRate(rate) if *rate > 0.0 => spec.rate = *rate,
                LiveCmd::SetRes(res) => spec.res = *res,
                LiveCmd::SetWhole(whole) => spec.whole = whole.clone(),
                LiveCmd::SetFine(fine) => spec.fine = fine.clone(),
                LiveCmd::SetRing(seconds) => spec.ring = *seconds,
                LiveCmd::SetUntil(until) => spec.until = *until,
                LiveCmd::LogicDetail(chunks) => spec.logic_detail = chunks.clone(),
                _ => {}
            }
        }
        // The eye's timebase follows the same positive rates as the worker, and with it the
        // resolution (after this command, in order).
        let mut rate_changed = false;
        if let LiveCmd::SetRate(rate) = &cmd {
            if *rate > 0.0 && *rate != self.spec.rate {
                self.spec.rate = *rate;
                rate_changed = true;
            }
        }
        if let LiveCmd::SetRes(res) = &cmd {
            self.spec.res = *res;
        }
        let events: &[(f64, LiveCmd)] = match &cmd {
            LiveCmd::Edit(events) => events,
            _ => &[],
        };
        // a knob turned while stopped warms nothing ahead: its circuit's kept cut carries the next
        // start (`warm_ahead`)
        for cmd in std::iter::once(cmd.untimed()).chain(events.iter().map(|e| &e.1)) {
            if let LiveCmd::SetParam { device, key, value, .. } = cmd {
                self.params.insert((device.clone(), key.clone()), *value);
            }
        }
        if matches!(cmd.untimed(), LiveCmd::Play | LiveCmd::Pause | LiveCmd::Step(_) | LiveCmd::CoStep(_) | LiveCmd::SetRate(_) | LiveCmd::SetRes(_) | LiveCmd::SetUntil(_) | LiveCmd::SetRing(_) | LiveCmd::Stop | LiveCmd::SetParam { .. } | LiveCmd::Retune { .. } | LiveCmd::PinMode { .. } | LiveCmd::Input { .. } | LiveCmd::Edit(_)) {
            if let Some(lowered) = &self.ensemble_lowered {
                for member in self.members.iter_mut().flatten() {
                    member.send(cmd.clone(), lowered);
                }
            }
        }
        if matches!(&cmd, LiveCmd::Stop) {
            self.ensemble_lowered = None;
        }
        self.send_nominal(cmd);
        if rate_changed {
            self.sync_res();
        }
    }

    fn send_nominal(&mut self, cmd: LiveCmd) {
        let cmd = match cmd {
            LiveCmd::SetRate(rate) if rate > 0.0 => LiveCmd::SetRate(self.worker_rate()),
            cmd => cmd,
        };
        // the pace and the Listen taps decide whether the run is heard
        let heard = matches!(cmd, LiveCmd::SetRate(_)).then(|| LiveCmd::SetHeard(self.heard()));
        match cmd {
            // Play ends a review: the line follows what is heard again (the player goes on from
            // where a scrub left it)
            LiveCmd::Play => {
                self.playing = true;
                if self.scrub.take().is_some() {
                    self.seen = 0;
                }
            }
            // a firmware step ends paused, like a `Step`
            LiveCmd::Pause | LiveCmd::Step(_) | LiveCmd::CoStep(_) => self.playing = false,
            _ => {}
        }
        self.taint(&cmd);
        self.cmds.send(cmd);
        if let Some(heard) = heard {
            self.cmds.send(heard);
        }
    }

    fn sink(&mut self, cmd: SinkCmd) {
        if self.sinks.send(cmd) {
            self.cmds.send(LiveCmd::Sinks);
        }
    }

    /// The hearing recorder (Fable's J3, generalised): one sink, `listen::JACK_TAP`, for every
    /// voice a Listen could ask for, the scope's output jack channels and the bench's voltage
    /// probes, taping every run from its start into the Listen bus's shelf whether or not anyone
    /// listens, at the take's window, so its tape ends where a Listen tap's take ends. It stays
    /// across runs while the voices and the take are the same, and goes in Realtime or when there
    /// is nothing to hear. It never paces or holds the run. O(voices) when they change, else
    /// O(voices) to compare.
    fn sync_jack_sink(&mut self) {
        let mut voices = self.jack_voices.clone();
        for v in &self.probe_voices {
            if !voices.contains(v) {
                voices.push(v.clone());
            }
        }
        let want = (!self.realtime() && !voices.is_empty()).then(|| (voices, self.take_len().unwrap_or(LOOP_SECONDS)));
        if want == self.jack_sink {
            return;
        }
        match (&want, &self.listen_bus) {
            (Some((voices, window)), Some(bus)) => {
                let spec = TapSpec { voices: voices.clone(), latency: LISTEN_LATENCY, dc_block_hz: LISTEN_DC_HZ, mix: TapMix::new(voices.len()) };
                let tap = AudioTap::recorder(spec, bus.ring.clone(), bus.record(JACK_TAP, JACK_TAP, *window));
                self.sink(SinkCmd::Add(JACK_TAP, Box::new(tap)));
            }
            (Some(_), None) => return,
            (None, _) => self.sink(SinkCmd::Remove(JACK_TAP)),
        }
        self.jack_sink = want;
    }

    fn pump_sinks(&mut self) {
        if self.sinks.pump() {
            self.cmds.send(LiveCmd::Sinks);
        }
    }

    fn sync_members(&mut self) {
        let count = if self.feeds.is_empty() || self.live.is_none() || self.ensemble_lowered.is_none() {
            0
        } else {
            match self.member_count {
                Some(n) => n,
                None => std::thread::available_parallelism().map_or(0, |n| n.get().saturating_sub(2)),
            }
        };
        self.members.truncate(count);
        let (Some(lowered), Some(spec)) = (&self.ensemble_lowered, &self.run_spec) else {
            self.start_members();
            return;
        };
        while self.members.len() < count {
            let m = self.members.len();
            let values = member_values(&lowered.tol, m as u64 + 1);
            let (cmd_tx, cmd_rx) = mpsc::sync_channel(COMMANDS);
            let (sink_tx, mut sinks) = SinkSet::new();
            let (done_tx, done_rx) = mpsc::sync_channel(1);
            let mut member = Member {
                cmds: Commands { tx: Some(cmd_tx), ..Commands::default() },
                sinks: Sinks { tx: Some(sink_tx), ..Sinks::default() },
                values,
                seed: m as u64 + 1,
                noise_dt: self.run_noise_dt,
                ended: done_rx,
                start: None,
            };
            // queued before the member's run starts, which installs them first
            if let Some(mic) = &self.mic_input {
                member.cmds.send(LiveCmd::Inputs(mic.readers.clone()));
                if let Some(attach) = &mic.attach { member.cmds.send(attach.clone()); }
            }
            for (&id, feed) in &mut self.feeds {
                let _ = member.sinks.send(SinkCmd::Add(id, feed.member(m as u32)));
            }
            for ((device, key), &value) in &self.params {
                member.send(LiveCmd::SetParam {
                    generation: lowered.index.generation, device: device.clone(), key: key.clone(), value,
                }, lowered);
            }
            let lowered = lowered.clone();
            let values = member.values.clone();
            let noise_dt = member.noise_dt;
            // no snapshots: a member's run is not heard, and one each would multiply the memory
            let snapshots = Snapshots { span: 0.0, ..spec.snapshots.clone() };
            let spec = LiveSpec { trace: false, trace_pins: false, co_state: false, heard: false, snapshots, ..spec.clone() };
            let maker = self.cosim.clone();
            member.start = Some(Box::new(move |seed: Arc<(u64, OpSeed)>, wake: Arc<dyn Fn() + Send + Sync>| {
                let latest = Latest::new();
                let mut traces = TracePublisher::new(Arc::new(Latest::new()));
                let mut circuit = member_circuit(&lowered, &values);
                member_noise(&mut circuit, m as u64 + 1, noise_dt);
                let sim = LiveSim::new_from(circuit, lowered.plan.clone(), lowered.index.generation, spec, Some(&seed.1));
                let co = maker.as_ref().and_then(|mk| mk(&lowered, m as u32 + 1));
                drop(lowered);
                drop(values);
                drop(seed);
                let end = match sim {
                    Ok(mut sim) => {
                        sim.attach(co);
                        run_live(sim, &cmd_rx, &latest, &mut traces, &mut sinks, &*wake).err()
                    }
                    Err(d) => Some(d),
                };
                let _ = done_tx.try_send(end.map(|d| d.to_string()));
                wake();
            }));
            self.members.push(Some(member));
        }
        self.start_members();
    }

    /// Starts the members' workers from this run's nominal operating point, once it is in: plain
    /// Newton in a few iterations where a cold solve may take the whole homotopy (ESIM-Q3). Until
    /// then `members_wait` has `step` retry when the host wakes.
    fn start_members(&mut self) {
        self.members_wait = false;
        if !self.members.iter().flatten().any(|m| m.start.is_some()) {
            return;
        }
        let Some(spawn) = self.spawn.as_deref() else { return };
        let Some(seed) = self.op_seed.latest().map(|(_, s)| s).filter(|s| s.0 == self.runs) else {
            self.members_wait = true;
            return;
        };
        for member in self.members.iter_mut().flatten() {
            if let Some(start) = member.start.take() {
                let seed = seed.clone();
                let wake = spawn.waker();
                spawn.worker("elec ensemble member", Box::new(move || start(seed, wake)));
            }
        }
    }

    fn pump_members(&mut self) {
        for (m, slot) in self.members.iter_mut().enumerate() {
            let Some(member) = slot else { continue };
            member.cmds.pump();
            if member.sinks.pump() {
                member.cmds.send(LiveCmd::Sinks);
            }
            let error = match member.ended.try_recv() {
                Ok(error) => error,
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => Some("worker disconnected".to_owned()),
            };
            if let Some(error) = error {
                log!("elec sim: ensemble member {m} stopped: {error}");
            } else {
                log!("elec sim: ensemble member {m} stopped");
            }
            *slot = None;
        }
    }

    /// Everything `frame` does but the upload, at frame time `now` (seconds): `live`'s trees are
    /// read, its numbering, play state, `live_time`, simulated time, record span and ratio written.
    /// True when something new was written for views, excluding the animation clock alone. Each
    /// call is a frame's (`frame`'s `tick`).
    pub fn step(&mut self, now: f64, live: &mut SimLive) -> bool {
        self.step_on(now, live, true)
    }

    /// [`Self::step`]; on a `tick` the newest instant's currents job goes to the worker.
    fn step_on(&mut self, now: f64, live: &mut SimLive, tick: bool) -> bool {
        // a value edit's restart, once the edits paused
        let due = match self.restart.as_mut() {
            Some(r) if r.since.is_none() => {
                r.since = Some(now);
                false
            }
            Some(r) => r.since.is_some_and(|since| now - since >= EDIT_QUIET),
            None => false,
        };
        let restarted = due && self.restart.take().map(|r| self.restart_run(r)).is_some();
        let Some(spawn) = self.spawn.as_deref() else { return false };
        let mut changed = restarted;
        // the device's events, on every step (the feed's offers signal one): views read the store
        // on their own frames, so this alone asks for no redraw
        if let Some(log) = self.events.as_mut() {
            log.pump();
        }
        // the app's Listen bus, once anything made it: its player says where the listener is
        if self.listen_bus.is_none() {
            self.listen_bus = live.listen.clone();
        }
        // a jack recording nobody heard stopped at its bound: said each time one does
        if let Some(bus) = &self.listen_bus {
            let bounded = bus.tapes.bounded.load(std::sync::atomic::Ordering::Relaxed);
            if bounded != self.bounded_seen {
                self.bounded_seen = bounded;
                log!("elec listen: the jack recording nobody hears stopped taping at {} MB; its start is kept, and the run goes on", KEEP_BYTES >> 20);
            }
        }
        // Stopped, nothing lowers or runs until a Play (and the stopped worker's sinks are back).
        // a scope that gained or lost its output jacks: a running run at the wrong rate starts over
        let mut jacks_changed = false;
        if !self.lowering && !self.stopped && self.stopping.is_none() {
            if let Some(design) = self.want.take() {
                let target = self.scope.clone().or_else(|| design.units.iter().find(|u| matches!(u.kind, UnitKind::Board)).map(|u| BenchScope::Design(u.id)));
                if let Some(target) = target {
                    let (scope_id, generation) = match &target {
                        BenchScope::Design(id) => (*id, design.generation),
                        BenchScope::Own { circuit, generation } => (circuit.unit, *generation),
                    };
                    // a running lowering of the same scope and stimulus is kept when the circuit's
                    // content is too, and one of another stimulus may be a retune of its sources;
                    // a stopped bench always lowers again
                    let scope_circuit = match &target {
                        BenchScope::Design(id) => design.unit(*id).map(|u| u.circuit.clone()),
                        BenchScope::Own { circuit, .. } => Some(circuit.clone()),
                    };
                    // the bench with the output jacks' loads: it is the key's, so the load is in
                    // the run's content key
                    let jacks_before = self.scope_jacks;
                    let (bench, jacks) = Self::with_jack_loads(&self.bench, self.jack_load, &self.jack_loads, scope_id, scope_circuit.as_deref());
                    self.scope_jacks = jacks;
                    self.jack_voices = scope_circuit.as_deref().filter(|_| jacks > 0).map_or_else(Vec::new, |c| jack_tap_voices(c, scope_id));
                    // the bench's voltage probes on the scope's board, heard as its Scope lanes hear them
                    let probes = self.bench.probes.iter().filter_map(|p| match p.probe {
                        Probe::V(net) if net.board == scope_id => Some(net.net),
                        _ => None,
                    });
                    self.probe_voices = scope_circuit.as_deref().map_or_else(Vec::new, |c| probe_tap_voices(c, probes));
                    if (jacks > 0 || !self.probe_voices.is_empty()) && self.listen_bus.is_none() {
                        self.listen_bus = Some(live.listen_bus());
                    }
                    jacks_changed = (jacks_before > 0) != (self.scope_jacks > 0);
                    // a lowering for Logic has the timing cells: one of the other mode is no base
                    let logic = self.sim_mode == SimMode::Logic;
                    let running = self.key.as_ref().filter(|k| self.live.is_some() && k.scope == scope_id && k.logic == logic);
                    let same_bench = running.is_some_and(|k| Arc::ptr_eq(&k.bench, &bench) || *k.bench == *bench);
                    let known = running.map(|k| (k.circuit, k.sim.clone(), k.plan.clone()));
                    let (libs, done) = (self.libs.clone(), self.done_tx.clone());
                    self.lowering = true;
                    self.lowering_logic = logic;
                    self.pending = Some((scope_id, generation, bench.clone()));
                    self.pending_circuit = scope_circuit;
                    spawn.job(Box::new(move || {
                        let circuit = match &target {
                            BenchScope::Design(id) => design.unit(*id).map(|u| u.circuit.content_hash()),
                            BenchScope::Own { circuit, .. } => Some(circuit.content_hash()),
                        };
                        let lower_target = || {
                            let mut l = match &target {
                                BenchScope::Design(id) => lower(&design, *id, &bench, &libs),
                                BenchScope::Own { circuit, generation } => lower_circuit(circuit, &bench, *generation, &libs),
                            };
                            if logic {
                                let (cells, analog) = l.with_cells();
                                let why: Vec<String> = analog.iter().take(3).map(|(input, why)| format!("{input}: {why}")).collect();
                                log!("elec sim: Logic runs {} threshold inverter(s) as timing cells; {} stay analog{}{}", cells.len(), analog.len(),
                                    if why.is_empty() { "" } else { " (" }, if why.is_empty() { String::new() } else { format!("{}{})", why.join("; "), if analog.len() > 3 { "; ..." } else { "" }) });
                            }
                            l
                        };
                        let msg = match known {
                            Some((hash, ..)) if same_bench && circuit == Some(hash) => Msg::Unchanged,
                            // the running scope: a lowering that differs only in values keeps the run
                            Some((hash, sim, plan)) if circuit.is_some() => {
                                let l = lower_target();
                                match if l.plan == *plan { sim.value_changes(&l.circuit) } else { None } {
                                    Some(changes) => Msg::Retuned(changes, Box::new(l), circuit.filter(|&c| c != hash)),
                                    None => Msg::lowered(l, circuit),
                                }
                            }
                            _ => Msg::lowered(lower_target(), circuit),
                        };
                        let _ = done.send(msg);
                    }));
                }
            }
        }
        // stopped, the newest edit is warmed ahead of the next start: once the warm before it has
        // lowered and the stopped run has ended
        if self.stopped && self.ahead_due && self.stopping.is_none() && self.ahead.as_ref().is_none_or(|a| !a.lowering.load(std::sync::atomic::Ordering::Acquire)) {
            self.warm_ahead();
        }
        if jacks_changed {
            self.rate_check("the audio output");
        }
        while let Ok(msg) = self.done_rx.try_recv() {
            // The lowering a Stop interrupted answers for nothing: a Play lowers again.
            if self.discard_lowering && matches!(msg, Msg::Lowered(..) | Msg::Unchanged | Msg::Retuned(..)) {
                self.discard_lowering = false;
                self.lowering = false;
                self.pending = None;
                continue;
            }
            match msg {
                // while the recording plays, a new lowering is an edit that is no value: the run
                // starts over with it (the restart lowers the newest design again)
                Msg::Lowered(..) if self.rendering && self.run_live() => {
                    self.drop_held(true);
                    let switched = self.key.as_ref().is_some_and(|k| k.logic != self.lowering_logic);
                    let name = match (&self.run_circuit, &self.pending_circuit) {
                        _ if switched => Some(if self.lowering_logic { "the switch to Logic" } else { "the switch from Logic" }.to_owned()),
                        (Some(old), Some(new)) => circuit_edit(old, new).or_else(|| Some("the circuit edit".to_owned())),
                        _ => Some("the circuit edit".to_owned()),
                    };
                    self.lowering = false;
                    self.pending = None;
                    self.pending_circuit = None;
                    self.restart_after(name, false);
                    changed = true;
                }
                Msg::Lowered(lowered, circuit, made) => {
                    changed = true;
                    self.lowering = false;
                    let held = HeldReplace { id: 0, lowered, circuit, made, pending: self.pending.take(), pending_circuit: self.pending_circuit.take(), logic: self.lowering_logic };
                    self.drop_held(true);
                    if self.replace_warms() {
                        self.hold_replace(held);
                    } else {
                        self.take_lowered(held);
                    }
                }
                Msg::Warmed(id) => {
                    if let Some(held) = self.held_replace.take_if(|h| h.id == id) {
                        changed = true;
                        self.take_lowered(held);
                    }
                }
                Msg::CoPins(table) => {
                    changed = true;
                    self.set_co_table(table);
                }
                // values only (a bench stimulus, a value edited in the circuit): the run goes on and
                // takes them as commands, where the listener hears them while the recording plays
                Msg::Retuned(changes, l, circuit) => {
                    changed = true;
                    self.lowering = false;
                    self.drop_held(true);
                    let mut l = *l;
                    // the run reflects the new source once it has the commands below
                    if let Some((scope, source, bench)) = self.pending.take() {
                        self.facts.source = (scope, source);
                        if let Some(key) = self.key.as_mut() {
                            key.bench = bench;
                            key.sim = Arc::new(l.circuit.clone());
                            if let Some(circuit) = circuit {
                                key.circuit = circuit;
                            }
                        }
                    }
                    self.diags = std::mem::take(&mut l.diags);
                    let generation = self.facts.generation;
                    // Later members start with the retuned values and the running generation.
                    // The LEDs' on-currents follow the new values.
                    self.glow_for = None;
                    // Existing members keep their state and receive only the commands below. An
                    // edited circuit's index (a pot's new ends) takes the running numbering.
                    let mut index = std::mem::replace(&mut l.index, ValueIndex::new(generation));
                    let name = match (circuit, &self.run_circuit, &self.pending_circuit) {
                        (Some(_), Some(old), Some(new)) => circuit_edit(old, new).or_else(|| Some("the circuit edit".to_owned())),
                        (Some(_), ..) => Some("the circuit edit".to_owned()),
                        (None, ..) => Some("the bench edit".to_owned()),
                    };
                    if circuit.is_some() {
                        self.run_circuit = self.pending_circuit.take();
                        index.generation = generation;
                        self.index = Arc::new(index);
                        self.sync_bench_probes();
                    }
                    self.cells = l.circuit.devices.cell.len();
                    self.ensemble_lowered = Some(Arc::new(l));
                    let mut cmds: Vec<LiveCmd> = changes.into_iter().map(|change| match change {
                        Change::Source(device, source) => LiveCmd::Retune { generation, device, source },
                        Change::Pin(device, mode) => LiveCmd::PinMode { generation, device, mode },
                        Change::Value(device, value) => LiveCmd::SetParam { generation, device, key: "value".to_owned(), value },
                    }).collect();
                    // a turned knob whose parts the edit sets keeps its place: a pot's halves are
                    // its new value split where the knob stands, not where the circuit has it
                    let set = |cmd: &LiveCmd, cmds: &[LiveCmd]| match cmd {
                        LiveCmd::SetParam { device, .. } => cmds.iter().any(|c| matches!(c, LiveCmd::SetParam { device: d, .. } if d == device)),
                        _ => false,
                    };
                    let knobs: Vec<LiveCmd> = self.turned.iter().filter(|t| t.1 == generation)
                        .map(|&(comp, _, value)| self.index.command(&SimCmd::SetParam { comp, value }))
                        .filter(|knob| knob.iter().any(|k| set(k, &cmds)))
                        .flatten().collect();
                    cmds.extend(knobs);
                    self.edit_heard(cmds, name);
                }
                Msg::Unchanged => {
                    self.lowering = false;
                    self.drop_held(true);
                    if let Some((scope, generation, _)) = self.pending.take() {
                        // The running lowering already has this source's content.
                        self.facts.source = (scope, generation);
                    }
                }
                // A run a Stop ended: its sinks wait for the next run.
                Msg::Ended(run, _, sinks) if self.stopping == Some(run) => {
                    self.stopping = None;
                    self.parked_sinks = Some(sinks);
                }
                Msg::Ended(run, error, _) if run == self.runs => {
                    changed = true;
                    if let Some(e) = &error {
                        log!("elec sim: the live bench stopped: {e}");
                    }
                    self.error = error;
                    self.live = None;
                    self.preparing_since = None;
                    self.inputs = None;
                    self.mic_input = None;
                    self.tex.clear();
                    self.rows_for = None;
                    self.meter.clear();
                    self.cmds.tx = None;
                    self.sinks.tx = None;
                    self.members.clear();
                    self.ensemble_lowered = None;
                    self.run_spec = None;
                }
                Msg::Ended(..) => {}
            }
        }
        self.cmds.pump();
        self.pump_sinks();
        self.pump_members();
        if self.members_wait {
            self.start_members();
        }
        // the newest trace store, merged and trimmed on the worker; the one it replaces is dropped
        // here only when no store of the worker's turns is it (a ghost or scrub kept it)
        if let Some(w) = self.live.as_ref().filter(|w| w.traces.published() != self.traces_seen) {
            // a reviewed instant stays in the Scope's record: a store that rolled past it waits
            // until the review ends
            let rolled_past = |store: &TraceStore| self.scrub.is_some_and(|t| store.generation == self.store.generation
                && !self.store.is_empty() && (store.is_empty() || store.time.at(0) > t));
            if let Some((n, store)) = w.traces.latest().filter(|(_, store)| !rolled_past(&**store)) {
                self.traces_seen = n;
                self.mini.follow(&store);
                self.store = store;
                changed = true;
            }
        }
        // Read the newest values first so their start, if any, is in the batch taken before
        // the host consults its meter.
        let fresh = self.live.as_ref().and_then(|w| w.values.latest()).filter(|(n, _)| *n != self.seen);
        let heard = self.take_frames();
        let heard = self.playback(now, heard);
        if let Some((n, lv)) = fresh {
            self.seen = n;
            // where the worker's wall went, a line every few seconds of running (`run_live`)
            if let Some((k, line)) = lv.worker_split.as_ref().filter(|(k, _)| *k != self.split_seen) {
                self.split_seen = *k;
                log!("elec sim: {line}");
            }
            // a failed plan leaves the run whole: the log and the bar say why, once per failure
            if lv.plan_error.as_ref().map(Arc::as_ptr) != self.plan_error.as_ref().map(Arc::as_ptr) {
                if let Some(why) = &lv.plan_error {
                    log!("elec sim: fast mode off: {why}");
                }
                self.plan_error = lv.plan_error.clone();
            }
            if lv.rescues != self.rescues_seen {
                if lv.rescues > self.rescues_seen {
                    self.rescues.0 += lv.rescues - self.rescues_seen;
                    if let Some(r) = &lv.rescue {
                        log!("elec sim: the live bench went on at t = {:e} s only through the gmin ladder (at {}), settled at gmin {:e} S", r.t, r.at.join(", "), r.gmin);
                        self.rescues.1 = Some(r.clone());
                    }
                }
                self.rescues_seen = lv.rescues;
            }
            if lv.generation == self.facts.generation {
                changed = true;
                if lv.preparing {
                    self.preparing_since.get_or_insert_with(clock_now);
                } else {
                    self.preparing_since = None;
                }
                if let Some((_, at)) = self.edit_hearing.take_if(|h| h.0 == lv.generation && !lv.preparing) {
                    log!("elec sim: the edit is heard {:.2} s after its commit", clock_now() - at);
                }
                self.eye.see(&lv);
                self.meter.see(&lv);
                self.ask_rounds(&lv);
                if self.scrub.is_none() && heard.is_none() {
                    let eye = &self.eye;
                    // while the run goes on, the eye's means (a PWM net sampled at the frame rate
                    // aliases); at rest (paused, a step's end, settled) the sample at `t`, written once
                    // as the run stops: the instant the device's state, its pins and the scopes show,
                    // so a net never shows the level it left just before the pause
                    let values = if lv.running && !lv.steady {
                        RowValues { net_v: &eye.net, pin_i: &eye.pin, led_i: &eye.pin, part_p: &eye.part }
                    } else {
                        RowValues { net_v: &lv.net_v, pin_i: &lv.branch_i, led_i: &lv.branch_i, part_p: &lv.part_p }
                    };
                    self.tex.write_values(&self.facts.rows, &values);
                    self.rows_for = Some(self.facts.generation);
                    self.tex.write_pins(&self.facts.rows, lv.co.as_ref(), self.co_reader.as_ref());
                    let pin_i = Arc::from(values.pin_i);
                    self.due_currents(pin_i);
                }
                self.values = Some(lv);
            }
            if let Some(t) = self.values.as_ref().map(|v| v.t).filter(|_| !self.held.is_empty()) {
                self.release_held(t);
            }
        }
        if self.check_loop_end && self.values.as_ref().is_some_and(|v| v.t > self.spec.until + TAKE_SLACK) {
            self.restart_take();
        }
        changed |= self.show_at(heard);
        // a moving silent clock needs the next frame, as the player's instant does
        changed |= self.silent.as_ref().is_some_and(|s| s.moving);
        if let Some(spawn) = self.spawn.as_deref() {
            changed |= self.mini.step(spawn, now, &mut live.minis, &self.store, &self.take, &self.index, self.spec.rate,
                self.values.as_ref().map(|v| v.ratio), self.spec.record, self.scrub, self.tex.look());
        }
        // a strip added or gone changes the rows Realtime's trace writes
        self.sync_fine();
        if let Some((w, u)) = self.mini.last_fill() {
            let (w, u) = (w * 1e3, u * 1e3);
            log!("elec sim: mini traces filled in {w:.2} ms on a worker, {u:.2} ms on the UI thread");
        }
        self.tex.resize_segs(live.trees.len());
        let freed = live.trees.take_freed();
        self.tex.clear_segs(&freed);
        changed |= !freed.is_empty();
        if tick {
            self.send_currents(&mut live.trees);
        }
        if let Some(c) = &mut self.currents {
            if tick {
                c.pump();
            }
            if let Some(r) = c.take() {
                if r.seq >= self.currents_from {
                    self.tex.write_segs(now, &r.rows, &r.i);
                    self.rows_for = Some(self.facts.generation);
                    changed = true;
                }
            }
        }
        live.time = self.tex.live_time(now);
        live.t = self.shown.as_ref().map(|s| s.t).or(self.values.as_ref().map(|v| v.t)).unwrap_or(0.0);
        let heard_live = self.shown.as_ref().map(|s| (s.t, s.moving));
        changed |= live.heard != heard_live;
        live.heard = heard_live;
        // The device's state: the publication's Arc, so an unchanged frame changes nothing.
        let co = match &self.shown {
            Some(s) => s.co.clone(),
            None => self.values.as_ref().and_then(|v| v.co.clone()),
        };
        if live.co != co {
            live.co = co;
            changed = true;
        }
        live.emission = self.tex.emission();
        live.lights = self.tex.lights();
        live.ratio = self.values.as_ref().map(|v| v.ratio);
        // the stretch a time strip reviews: the take's, else the trace's; in Continuous no more
        // than the ring behind the front (the take keeps older frames folded, held since the start)
        live.record = match (self.take.span(), self.store.len()) {
            (Some(span), _) => span,
            (None, 0) => (0.0, 0.0),
            (None, n) => (self.store.time.at(0), self.store.time.at(n - 1)),
        };
        if self.continuous() {
            live.record.0 = live.record.0.max(live.record.1 - self.ring);
        }
        let playing = self.plays();
        changed |= live.playing != playing;
        live.playing = playing;
        changed |= self.publish_held(live);
        changed |= self.publish_turned(live);
        changed |= self.publish_envelopes(live);
        // The record a scrub reads: taken when the scrub begins, taken again on a frame whose store
        // has the late points that follow a pause or went back (an edit behind the run's front),
        // dropped when it ends or the numbering changes.
        // A take is the published store's Arc.
        changed |= live.scrub != self.scrub;
        live.scrub = self.scrub;
        match self.scrub {
            Some(_) if self.store.generation == self.index.generation => {
                let end = self.store.base + self.store.len();
                let stale = live.trace.as_ref().is_none_or(|s| s.generation != self.store.generation || s.rewinds != self.store.rewinds || s.base + s.len() != end);
                if stale {
                    live.trace = Some(self.store.clone());
                    changed = true;
                }
            }
            _ => changed |= live.trace.take().is_some(),
        }
        // The whole record for the Scope tab: the store's Arc whenever the worker published one.
        let store = (!self.store.is_empty()).then_some(&self.store);
        if live.store.as_ref().map(Arc::as_ptr) != store.map(Arc::as_ptr) {
            live.store = store.cloned();
            changed = true;
        }
        if !Arc::ptr_eq(&live.index, &self.index) {
            live.index = self.index.clone();
            changed = true;
        }
        // the views' slot is the one the Listen tap reads: the host writes runs into it
        if !Arc::ptr_eq(&live.runs, &self.run_slot) {
            live.runs.set(self.run_slot.current());
            self.run_slot = live.runs.clone();
        }
        // the views read the judge's verdicts from the host's slot
        if !Arc::ptr_eq(&live.judged, &self.judged) {
            live.judged = self.judged.clone();
            changed = true;
        }
        if live.emphasis != self.emphasis_rev {
            live.emphasis = self.emphasis_rev;
            changed = true;
        }
        changed
    }

    /// Whether the recording plays (the Listen output loops the run's recording below real time):
    /// value edits then go in where the listener hears them (`heard_edit`), and an edit that is no
    /// value restarts the run from 0. The app's `frame` sets it from the Listen output.
    pub fn set_rendering(&mut self, rendering: bool) {
        self.rendering = rendering && !self.realtime();
    }

    /// The edit that restarts the next run, while the recording plays: its name, or None.
    pub fn restart_pending(&self) -> Option<Option<&str>> {
        self.restart.as_ref().map(|r| r.name.as_deref())
    }

    /// What the nominal run simulates (None between runs).
    pub fn content(&self) -> Option<&Arc<RunContent>> {
        self.content.as_ref()
    }

    /// Stopped by the person: the next Play, Step or firmware step is a new run from reset.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// The live run's number, new with each run the host starts; None with no run live. What a
    /// link sent a run (its streamed inputs, a timed input) is that run's only.
    pub fn live_run(&self) -> Option<u64> {
        self.run_live().then_some(self.runs)
    }

    /// The runs started so far: the next run from t = 0 is this plus one.
    pub fn runs(&self) -> u64 {
        self.runs
    }

    /// The run still makes points: the transport plays it and it has not said it rests (at a
    /// steady state or its take's end); a run starting, with nothing published yet, will. O(1).
    pub fn advancing(&self) -> bool {
        self.plays() && self.values.as_deref().is_none_or(|v| v.running && !v.steady)
    }

    /// The bench's expectations, judged on every run from now on (`LiveJudge`): from the next
    /// run's start, and on the running one at once (from where it is). O(expectations) to compare;
    /// a change installs a judge.
    pub fn set_expects(&mut self, expects: Vec<Expect>) {
        if expects == self.expects {
            return;
        }
        self.expects = expects;
        if let Some(l) = self.ensemble_lowered.clone().filter(|_| self.live.is_some()) {
            let index = self.index.clone();
            self.install_judge(&l.plan, &index, self.runs);
        }
    }

    /// Where the judge's verdicts go up (`Judged`), the same slot for the host's life.
    pub fn judged(&self) -> &Arc<Mutex<Judged>> {
        &self.judged
    }

    /// A judge of the bench's expectations on runs of a lowering (its plan and numbering; the
    /// lowering's own index is moved out when it is taken in), as run `run`, in place of the last
    /// one (none without expectations): it takes the slot over now and judges from the run's next
    /// begin. O(expectations).
    fn install_judge(&mut self, plan: &ValuePlan, index: &ValueIndex, run: u64) {
        self.judges += 1;
        let judge = LiveJudge::new(self.judges, run, self.expects.clone(), plan, index, self.judged.clone());
        if self.expects.is_empty() {
            self.sink(SinkCmd::Remove(JUDGE_SINK));
        } else {
            self.sink(SinkCmd::Add(JUDGE_SINK, Box::new(judge)));
        }
    }

    /// Whether the bench plays (the last Play or Pause sent).
    pub fn playing(&self) -> bool {
        self.playing
    }

    /// The run's time an input made now applies from: where the listener hears the recording
    /// while it plays, else the newest values' time, the run's front as the host sees it (the
    /// worker may be past it, and applies the input at its next boundary). O(1).
    pub fn input_time(&self) -> f64 {
        self.heard_at().unwrap_or_else(|| self.values.as_ref().map_or(0.0, |v| v.t))
    }

    /// Replaces the linked world's streamed inputs, alongside any microphone the person opened.
    /// False with no run live (each run starts with none). They go before the
    /// timed input that hands their reader a source (`BoardInput::Track`).
    pub fn set_inputs(&mut self, inputs: Inputs) -> bool {
        if !self.run_live() {
            return false;
        }
        self.inputs = Some(inputs);
        self.send_inputs();
        true
    }

    /// Adds the live microphone alongside the motion link. The next frame after close removes it.
    pub fn set_mic_input(&mut self, pump: Arc<dyn InputSource>, reader: Arc<dyn InputSource>) -> bool {
        if !self.run_live() { return false; }
        let readers = Inputs { sources: vec![reader], want: Arc::default() };
        for member in self.members.iter_mut().flatten() {
            member.cmds.send(LiveCmd::Inputs(readers.clone()));
        }
        self.mic_input = Some(MicSources { pump, readers, attach: None });
        self.send_inputs();
        true
    }

    fn send_inputs(&mut self) {
        let mut inputs = self.inputs.clone().unwrap_or_else(|| Inputs { sources: Vec::new(), want: Arc::default() });
        if let Some(mic) = &self.mic_input { inputs.sources.push(mic.pump.clone()); }
        self.send(LiveCmd::Inputs(inputs));
    }

    fn run_live(&self) -> bool {
        self.live.is_some() && self.cmds.tx.is_some()
    }

    /// The circuit of unit `board` as the run has it: the bench's own, or the design's.
    fn scope_circuit(&self, board: BoardId) -> Option<&Circuit> {
        match &self.scope {
            Some(BenchScope::Own { circuit, .. }) if circuit.unit == board => Some(&**circuit),
            _ => self.design.as_ref()?.unit(board).map(|u| &*u.circuit),
        }
    }

    /// While the take plays back, the run's time it plays now: the silent clock's head, moving or
    /// held (the instant the views show), else where the listener hears the recording
    /// (`SampleRing::heard_at`). An edit made now applies from there. None at the run's front, with
    /// auto re-record off, or before a window of the recording plays. O(1).
    fn heard_at(&self) -> Option<f64> {
        if !self.run_live() || !self.auto_rerecord {
            return None;
        }
        match &self.silent {
            Some(s) => Some(s.t),
            None => self.listen_bus.as_ref().filter(|_| self.rendering)?.ring.heard_at(),
        }
    }

    /// The take plays back while its run is live: the recording heard (`rendering`), or the silent
    /// clock with nobody listening. Value edits and presses then go in where it plays
    /// ([`Self::heard_at`]). O(1).
    fn plays_take(&self) -> bool {
        (self.rendering || self.silent.is_some()) && self.run_live()
    }

    /// Auto re-record ([`SimCmd::SetAutoRerecord`]): on, an edit or a press while the recording
    /// plays goes in where it is heard; off, it goes in at the run's front and waits for the next
    /// record, counted in `SimStatus::waiting`.
    pub fn set_auto_rerecord(&mut self, on: bool) {
        self.auto_rerecord = on;
    }

    /// An edit or a press while the recording plays with auto re-record off: one more waiting.
    fn note_waiting(&mut self) {
        if !self.auto_rerecord && self.plays_take() {
            self.waiting += 1;
        }
    }

    /// Value commands of one edit, named `name`: while the recording plays, one `Edit` at the
    /// time the listener hears, so the run goes back there (to its newest snapshot at or before
    /// it) and renders on with them, and the take is heard with them from there; otherwise at
    /// the run's front, each as it comes.
    fn edit_heard(&mut self, cmds: Vec<LiveCmd>, name: Option<String>) {
        if cmds.is_empty() {
            return;
        }
        self.note_waiting();
        match self.heard_at() {
            Some(t) => {
                // the edit goes in where it is heard (a Listen playing the hearing record follows
                // the take as it renders again), its own line in place of a Listen's reason
                self.relisten = None;
                self.heard_edit = name.map(|name| (Arc::from(name), t));
                self.send(LiveCmd::Edit(cmds.into_iter().map(|cmd| (t, cmd)).collect()));
            }
            None => {
                for cmd in cmds {
                    self.send(cmd);
                }
            }
        }
    }

    /// A board's inputs (a touch, a motion), all at one time: where the listener hears the
    /// recording, as one `Edit` (the run goes back there), else at the run's time as the host
    /// sees it, each on the timeline.
    fn input_heard(&mut self, cmds: Vec<LiveCmd>) {
        match self.heard_at() {
            Some(t) => {
                self.relisten = None;
                self.send(LiveCmd::Edit(cmds.into_iter().map(|cmd| (t, cmd)).collect()));
            }
            None => {
                let now = self.values.as_ref().map_or(0.0, |v| v.t);
                for cmd in cmds {
                    self.send(LiveCmd::At(now, Box::new(cmd)));
                }
            }
        }
    }

    /// The newest edit taken in where it was heard while the recording played: its name ("R7 =
    /// 22k") and the run's time it applies from.
    pub fn heard_edit(&self) -> Option<(&str, f64)> {
        self.heard_edit.as_ref().map(|(name, t)| (&**name, *t))
    }

    /// A design committed while stopped (a new generation): the warm made ahead for the last one
    /// stops, and one for this one is due (`warm_ahead`).
    fn edited_stopped(&mut self) {
        if let Some(a) = &self.ahead {
            a.stop.store(true, std::sync::atomic::Ordering::Release);
        }
        self.ahead_due = true;
    }

    /// Stops the warm made ahead, if one runs, and wants none: a Play starts the run itself.
    fn leave_ahead(&mut self) {
        if let Some(a) = &self.ahead {
            a.stop.store(true, std::sync::atomic::Ordering::Release);
        }
        self.ahead_due = false;
    }

    /// Warms the next start ahead while the run is stopped, so that a Play after an edit the
    /// running cut cannot carry (a part added or taken out, a net changed) starts on its key's
    /// own plan rather than paying its warm (`warm_ahead_of`). A job of the pool's Background class
    /// lowers the newest committed design as a Play would and makes its run with the knobs, then
    /// warms it. A design committed while it lowers stops it before it warms, so the warm starts
    /// only once the edits have paused for as long as a lowering takes, and the next lowering
    /// waits for this one; a newer design or a Play stops it at its next unit, and a warm that is
    /// stopped costs only the work it did (states it finished stay kept for their key). A knob
    /// turned while stopped neither stops nor asks for one: a knob is a part value, which the
    /// circuit's kept cut carries across. Nothing of it reaches the run: the Play lowers again and
    /// starts as before, on the cut the warm kept for its circuit when the keys match. Exact, a
    /// host without helper cores or a pool, and an edit a kept cut carries warm nothing. O(1)
    /// here; the job takes a lowering and a warm.
    fn warm_ahead(&mut self) {
        self.ahead_due = false;
        let Some(design) = self.want.clone().or_else(|| self.design.clone()) else { return };
        let Some(target) = self.scope.clone().or_else(|| design.units.iter().find(|u| matches!(u.kind, UnitKind::Board)).map(|u| BenchScope::Design(u.id))) else { return };
        let Some(spawn) = self.spawn.as_deref() else { return };
        let (Some(pool), true, false, plan_jit) = (spawn.background(), spawn.helpers().is_some(), self.exact(), spawn.plan_jit()) else { return };
        let scope_id = match &target {
            BenchScope::Design(id) => *id,
            BenchScope::Own { circuit, .. } => circuit.unit,
        };
        let scope_circuit = match &target {
            BenchScope::Design(id) => design.unit(*id).map(|u| u.circuit.clone()),
            BenchScope::Own { circuit, .. } => Some(circuit.clone()),
        };
        let (bench, _) = Self::with_jack_loads(&self.bench, self.jack_load, &self.jack_loads, scope_id, scope_circuit.as_deref());
        let (logic, libs, states) = (self.sim_mode == SimMode::Logic, self.libs.clone(), self.plan_states.clone());
        let (spec, band) = (self.ahead_spec(), self.run_band(true));
        let params: Vec<(String, String, f64)> = self.params.iter().map(|((device, key), &value)| (device.clone(), key.clone(), value)).collect();
        let seed = self.op_seed.latest().map(|(_, s)| s);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lowering = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (job_stop, job_lowering, job_pool) = (stop.clone(), lowering.clone(), pool.clone());
        self.ahead = Some(AheadWarm { stop, lowering });
        (pool.post)(Box::new(move || {
            let mut l = match &target {
                BenchScope::Design(id) => lower(&design, *id, &bench, &libs),
                BenchScope::Own { circuit, generation } => lower_circuit(circuit, &bench, *generation, &libs),
            };
            if logic {
                let _ = l.with_cells();
            }
            job_lowering.store(false, std::sync::atomic::Ordering::Release);
            if job_stop.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            l.circuit.opts.plan_jit = plan_jit;
            let generation = l.index.generation;
            let Ok(sim) = LiveSim::new_tuned(l.circuit, l.plan, generation, spec, seed.as_ref().map(|s| &s.1), &params) else { return };
            let t0 = std::time::Instant::now();
            if warm_ahead_of(sim, Some(job_pool), states, band, job_stop) {
                log!("elec sim: the edit warmed while stopped, in {:.2} s: a Play starts on its plan", t0.elapsed().as_secs_f64());
            }
        }));
    }

    /// A lowering came back (`Msg::Lowered`), or its warm ahead did (`Msg::Warmed`): the
    /// lowering's bookkeeping, then the run starts with it or takes it as a `Replace`.
    fn take_lowered(&mut self, held: HeldReplace) {
        let HeldReplace { lowered: mut l, circuit, made: (sim, plan), pending, pending_circuit, logic, .. } = held;
        if let Some((scope, generation, _)) = &pending {
            self.facts.source = (*scope, *generation);
        }
        self.key = circuit.zip(pending).map(|(circuit, (scope, _, bench))| LowerKey { circuit, scope, bench, logic, sim, plan });
        self.run_circuit = pending_circuit;
        self.numbering += 1;
        l.index.generation = self.numbering;
        self.accept_lowered(*l);
    }

    /// Whether a lowering that replaces the running circuit waits for its key's warm ahead
    /// (`hold_replace`): a run in rounds in Fast, on a host with helper cores and a pool.
    fn replace_warms(&self) -> bool {
        self.live.is_some() && self.cmds.tx.is_some() && self.rounds == Some(true) && !self.exact()
            && self.spawn.as_deref().is_some_and(|s| s.helpers().is_some() && s.background().is_some())
    }

    /// Holds a lowering that replaces the running circuit (a structure edit made while playing)
    /// while its key is warmed ahead on the pool's Background class (`warm_ahead_of`), below the
    /// heard run's packs, beside its own checks and plans. The run goes on, heard, with the
    /// circuit it has; when the warm is back (`Msg::Warmed`) the lowering goes in as it came, and
    /// the replaced run begins on the cut the warm kept for the new circuit (`LiveSim::replace`
    /// carries it), proved where it goes in, instead of standing still while its plan is made.
    /// A newer lowering's result (`drop_held`) stops the warm and takes its place; a Stop lets
    /// the held lowering go (a Play lowers again) and the warm go on. A warm that fails, or finds
    /// nothing to make, lets the lowering in as soon as it ends. O(1) here; the job takes a run
    /// made and a warm.
    fn hold_replace(&mut self, mut held: HeldReplace) {
        let Some(spawn) = self.spawn.as_deref() else { return self.take_lowered(held) };
        let Some(pool) = spawn.background() else { return self.take_lowered(held) };
        let (background, wake, plan_jit) = (pool.clone(), spawn.waker(), spawn.plan_jit());
        self.replaces_held += 1;
        held.id = self.replaces_held;
        let spec = match &self.run_spec {
            Some(spec) => LiveSpec { rate: self.worker_rate(), heard: self.heard(), ..spec.clone() },
            None => self.ahead_spec(),
        };
        let band = self.run_band(true);
        let params: Vec<(String, String, f64)> = self.params.iter().map(|((device, key), &value)| (device.clone(), key.clone(), value)).collect();
        let seed = self.op_seed.latest().map(|(_, s)| s);
        let (states, (circuit, plan)) = (self.plan_states.clone(), held.made.clone());
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Some(a) = &self.ahead {
            a.stop.store(true, std::sync::atomic::Ordering::Release);
        }
        self.ahead = Some(AheadWarm { stop: stop.clone(), lowering: Arc::new(std::sync::atomic::AtomicBool::new(false)) });
        let back = WarmReturn { id: held.id, stop: stop.clone(), done: self.done_tx.clone(), wake };
        self.held_replace = Some(held);
        (pool.post)(Box::new(move || {
            let back = back;
            let mut circuit = (*circuit).clone();
            circuit.opts.plan_jit = plan_jit;
            let t0 = std::time::Instant::now();
            let planned = LiveSim::new_tuned(circuit, (*plan).clone(), 0, spec, seed.as_ref().map(|s| &s.1), &params)
                .is_ok_and(|sim| warm_ahead_of(sim, Some(background), states, band, stop));
            if planned && !back.stop.load(std::sync::atomic::Ordering::Acquire) {
                log!("elec sim: the edit warmed ahead while playing, in {:.2} s: it goes in on its cut", t0.elapsed().as_secs_f64());
            }
        }));
    }

    /// The held replacement goes, for a newer lowering's result or a Stop; with `stop_warm` its
    /// warm stops too (a Stop leaves it to fill the state cache for a Play).
    fn drop_held(&mut self, stop_warm: bool) {
        if self.held_replace.take().is_some() && stop_warm {
            if let Some(a) = &self.ahead {
                a.stop.store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }

    /// The spec a run started now has (`start_run`), as far as its key reads it: its pace,
    /// resolution and sample rate, and the bench's largest step and round.
    fn ahead_spec(&self) -> LiveSpec {
        LiveSpec { sample_rate: self.sample_rate(), res: self.wanted_res(), playing: self.playing || self.finish, rate: self.worker_rate(), heard: self.heard(), ..self.spec.clone() }
    }

    /// Asks for a restart with the newest edit, once the edits pause (`step`). A newer edit
    /// renames it and waits again; the knobs stay only if every edit kept them.
    fn restart_after(&mut self, name: Option<String>, keep_params: bool) {
        let keep_params = keep_params && self.restart.as_ref().is_none_or(|r| r.keep_params);
        let name = name.or_else(|| self.restart.take().and_then(|r| r.name));
        self.restart = Some(Restart { name, since: None, keep_params });
    }

    /// The run starts over from 0 with the edits: the last committed design is lowered again
    /// once the stopped worker has handed its sinks back, and the new run starts with the knobs
    /// (when kept) set before its operating point. The play state carries over; the Listen tap
    /// takes it as a new take, the old one heard until the new one plays.
    fn restart_run(&mut self, r: Restart) {
        let (playing, finish) = (self.playing, self.finish);
        let params = r.keep_params.then(|| self.params.clone());
        // the stop keeps the lowering until the new one replaces it: the firmware bound to it
        // stays, so the new run has its process from t = 0 (unbound, the firmware attaching
        // again would be one more restart, every time)
        self.stop();
        self.stopped = false;
        self.restart_hold = true;
        (self.playing, self.finish) = (playing, finish);
        self.keep_params = params;
        self.restart_edit = r.name.map(Arc::from);
        if let Some(design) = self.design.clone() {
            self.asked = Some(design.generation);
            self.want = Some(design);
        } else {
            self.relower();
        }
    }

    /// The content key of a run that starts now: the scope circuit's content hash (every
    /// component value), the scope, the bench, the knobs, the co-simulated process and the
    /// settings that decide the samples (resolution, largest step, tolerance, sample rate, and
    /// the `band` Fast's plan is held accurate to: [`Self::run_band`]). Keys are compared within
    /// this process only (`RunContent`), so no build identity is in it.
    fn content_key(&self, band: f64) -> u128 {
        let mut w = Writer::hasher();
        match &self.key {
            Some(k) => {
                w.tag(1);
                w.raw(&k.circuit.0);
                w.str(&format!("{:?}", k.scope));
                w.str(&format!("{:?}", k.bench));
            }
            None => w.tag(0),
        }
        w.count(self.params.len());
        for ((device, key), value) in &self.params {
            w.str(device);
            w.str(key);
            w.u64(value.to_bits());
        }
        w.u128(self.cosim_id);
        for v in [self.spec.res, self.spec.max_step, self.spec.reltol_at(self.spec.res), self.spec.sample_rate, band] {
            w.u64(v.to_bits());
        }
        u128::from_le_bytes(w.finish().0)
    }

    /// The band a run that starts now holds Fast's plan accurate to, Hz (`PartOpts::band`): half
    /// the rate its takes leave at, at most half its own. Its stability is judged to half its own
    /// rate whatever this is. A run that never goes in rounds (`rounds` false: no helper cores) has
    /// no plan, and one with no sample rate delivers no take: neither has a band.
    fn run_band(&self, rounds: bool) -> f64 {
        if rounds && self.spec.sample_rate > 0.0 { take_band(self.spec.sample_rate, self.recording_rate()) } else { f64::INFINITY }
    }

    /// A command that changes what the nominal run simulates ends its clean stretch where it
    /// lands: a timed event at its time, an edit at its earliest, anything else at the run's time
    /// as the host last saw it (the worker applies it there or later). A run sent back further
    /// (to a snapshot before the edit) ends it there too: its recorder taints it at the snapshot.
    fn taint(&self, cmd: &LiveCmd) {
        let Some(content) = &self.content else { return };
        let now = self.values.as_ref().map_or(0.0, |v| v.t);
        match cmd {
            LiveCmd::At(t, _) => content.taint(*t),
            LiveCmd::Edit(events) => content.taint(events.iter().map(|e| e.0).fold(f64::INFINITY, f64::min)),
            LiveCmd::SetParam { .. } | LiveCmd::Retune { .. } | LiveCmd::PinMode { .. } | LiveCmd::Input { .. }
            | LiveCmd::Replace { .. } | LiveCmd::SetRes(_) | LiveCmd::SetRounds(_) | LiveCmd::CoSim(_) => content.taint(now),
            _ => {}
        }
    }

    /// A lowering came back: the worker starts with it, or takes it as a `Replace`.
    fn accept_lowered(&mut self, mut l: Lowered) {
        let generation = l.index.generation;
        // The host owns the numbering; members need only its generation.
        let index = std::mem::replace(&mut l.index, ValueIndex::new(generation));
        self.diags = std::mem::take(&mut l.diags);
        self.cells = l.circuit.devices.cell.len();
        // before the run starts below: its rate counts the device's speaker (`sample_rate`)
        self.scope_speakers = l.firmware.first().is_some_and(|binding| binding.speakers().next().is_some());
        // Fast's sparse LUs compile their plans on the pool; Exact never asks. The run, its
        // replacements and the ensemble's members all clone this circuit.
        l.circuit.opts.plan_jit = self.spawn.as_deref().and_then(|s| s.plan_jit());
        let l = Arc::new(l);
        let (circuit, plan) = (l.circuit.clone(), l.plan.clone());
        let (nets, pins, parts) = index.counts();
        let emitters = l.leds.iter().map(|e| Emitter { part: e.part, pin: e.anode, i_rated: e.i_rated as f32 }).collect();
        let rows = RowPlan { pin_net: plan.pin_net.clone(), emitters, p_rated: l.p_rated.clone(), co_pins: co_join(&self.co_table, &plan) };
        self.facts = Facts { generation, source: self.facts.source, rows };
        self.error = None;
        self.tex.resize(nets, parts);
        self.tex.resize_pins(pins);
        self.values = None;
        self.preparing_since = None;
        self.scrub = None;
        self.lowered_values(&index);
        self.ensemble_lowered = Some(l.clone());
        let subscribe = LiveCmd::Subscribe { generation, pins: (0..pins as u32).collect(), parts: (0..parts as u32).collect() };
        if self.live.is_some() && self.cmds.tx.is_some() {
            self.index = Arc::new(index);
            self.edit_hearing = self.edit_committed.take().map(|at| (generation, at));
            self.send(LiveCmd::Replace { generation, circuit: Box::new(circuit), plan: Box::new(plan) });
            // the replaced circuit's judge, after the replace: the run begins it again there
            let index = self.index.clone();
            self.install_judge(&l.plan, &index, self.runs);
            self.sync_bench_probes();
            // the running worker keeps its resolution: brought to the wanted one if it differs
            self.sync_res();
            // the jack recorder goes on with the same voices; other voices are a new take
            self.sync_jack_sink();
            for (m, member) in self.members.iter_mut().enumerate() {
                if let Some(member) = member {
                    member.values = member_values(&l.tol, m as u64 + 1);
                    let mut circuit = member_circuit(&l, &member.values);
                    member_noise(&mut circuit, m as u64 + 1, member.noise_dt);
                    member.cmds.send(LiveCmd::Replace {
                        generation, circuit: Box::new(circuit), plan: Box::new(l.plan.clone()),
                    });
                }
            }
            self.send(subscribe);
            self.sync_members();
            return;
        }
        let start = RunStart { lowered: l, glue_built: false, circuit, plan, index, subscribe };
        // the glue builds this board's firmware: a first run waits for its process, and a run
        // that starts over for the glue to read its lowering (and build its image, if it changed)
        if self.cosim_glue && (self.cosim.is_none() || self.restart_hold) && !start.lowered.firmware.is_empty() {
            self.cosim_wait = Some(Box::new(start));
            return;
        }
        self.start_run(start);
    }

    /// Starts a lowering's run on a new worker, with the co-simulated process the maker makes.
    fn start_run(&mut self, start: RunStart) {
        let RunStart { lowered: l, circuit, plan, index, subscribe, .. } = start;
        self.restart_hold = false;
        let generation = index.generation;
        if self.spawn.is_none() {
            return;
        }
        let (cmd_tx, cmd_rx) = mpsc::sync_channel(COMMANDS);
        let traces = Arc::new(Latest::new());
        let mut traces_out = TracePublisher::new(traces.clone());
        // A run after a Stop takes over the stopped run's sinks, the scopes' acquisition with them.
        let mut sinks = match self.parked_sinks.take() {
            Some(sinks) => *sinks,
            None => {
                let (tx, sinks) = SinkSet::new();
                self.sinks.tx = Some(tx);
                sinks
            }
        };
        // the jack recorder goes in before the worker starts: its take starts with the run
        self.sync_jack_sink();
        // a new run is a new take, which keeps every publication's frames from the run's first
        self.take.clear();
        self.take_said = false;
        self.shown = None;
        self.presses.clear();
        self.start_take_turns(generation);
        self.clear_envelopes();
        // a new take has every edit made so far
        self.waiting = 0;
        self.relisten = None;
        let values = Arc::new(Latest::new());
        values.keep_all(true);
        // The ensemble's noise as lowered; LiveSim tunes it to the sample period at its density.
        self.run_noise_dt = self.spec.max_step.min(l.source_step());
        // fixed for the run's life, and in its content key
        self.spec.sample_rate = self.sample_rate();
        // a run with audio keeps snapshots of the two windows a listener's edit lands in (the one
        // heard and the one rendering), so an edit there goes back at most a snapshot's spacing
        let window = self.listen_bus.as_ref().map_or(0.0, |bus| f64::from(bus.ring.play_status().window_s)).max(LOOP_SECONDS);
        self.spec.snapshots.span = if self.spec.sample_rate > 0.0 { 2.0 * window } else { 0.0 };
        // the resolution the views and the setting ask for now (later changes go as `SetRes`)
        self.spec.res = self.wanted_res();
        // the bench's probes under this run's numbering are in its spec: its first slice keeps
        // them whole, however far it goes before a command could reach it (`sync_bench_probes`
        // after the start then finds nothing to send)
        self.bench_rows(&index);
        self.spec.whole = self.whole_rows();
        self.spec.fine = self.fine_rows();
        let (spec, done, latest) = (LiveSpec { playing: self.playing || self.finish, ..self.spec.clone() }, self.done_tx.clone(), values.clone());
        self.run_spec = Some(spec.clone());
        let spec = LiveSpec { rate: self.worker_rate(), heard: self.heard(), ..spec };
        self.runs += 1;
        self.first_play = self.played_scopes.insert(self.facts.source.0);
        // the expectations' judge goes in before the worker starts: it judges the run from t = 0
        self.install_judge(&l.plan, &index, self.runs);
        self.inputs = None;
        self.mic_input = None;
        // the last run's events stay in its run; this run's time starts again
        if let Some(log) = self.events.as_mut() {
            log.pump();
            log.restart();
        }
        // what this run simulates, up before its worker starts (the Listen tap stamps its take
        // and records at its rate)
        // the helper cores this run's rounds go to (none: it never goes in rounds), and the band
        // its plan is held accurate to: both in its key
        let helpers = self.spawn.as_deref().and_then(|spawn| spawn.helpers());
        let band = self.run_band(helpers.is_some());
        let content = Arc::new(RunContent::new(self.content_key(band), self.restart_edit.take(), self.recording_rate()));
        self.run_slot.set(Some(content.clone()));
        self.content = Some(content);
        let params: Vec<(String, String, f64)> = self.params.iter().map(|((device, key), &value)| (device.clone(), key.clone(), value)).collect();
        // a restart on an unchanged unknown layout starts from the last run's operating point
        let (op_seed, run, last) = (self.op_seed.clone(), self.runs, self.op_seed.latest().map(|(_, s)| s));
        let (maker, lowered) = (self.cosim.clone(), l.clone());
        let Some(spawn) = self.spawn.as_deref() else { return };
        let (wake, background, ahead, pool, heard) = (spawn.waker(), spawn.background(), spawn.checks(), spawn.pool_name(), spawn.heard());
        let plan_states = self.plan_states.clone();
        self.rounds = helpers.is_none().then_some(false);
        spawn.worker(
            "elec live bench",
            Box::new(move || {
                let sim = LiveSim::new_tuned(circuit, plan, generation, spec, last.as_ref().map(|s| &s.1), &params);
                drop(last);
                let co = maker.as_ref().and_then(|mk| mk(&lowered, 0));
                drop(lowered);
                let _ = done.send(Msg::CoPins(co.as_ref().map_or_else(Vec::new, |c| c.pins.clone())));
                let end = match sim {
                    Ok(mut sim) => {
                        sim.attach(co);
                        op_seed.publish(Arc::new((run, OpSeed::of(sim.solver()))));
                        // Park the planner. A heard run can choose rounds at its start, and the
                        // host can ask for them when the meter shows the run falling behind.
                        if let Err(d) = host_rounds(&mut sim, helpers, background, ahead, plan_states, band) {
                            log!("elec sim: no round plan ({}); the circuit steps whole", d.message);
                        }
                        log!("elec sim: {}", setup_line(&sim, band, &pool));
                        sim.set_heard_priority(heard);
                        wake();
                        run_live(sim, &cmd_rx, &latest, &mut traces_out, &mut sinks, &*wake).err()
                    }
                    Err(d) => Some(d),
                };
                let _ = done.send(Msg::Ended(run, end.map(|d| d.to_string()), Box::new(sinks)));
                wake();
            }),
        );
        self.live = Some(Worker { values, traces });
        self.seen = 0;
        self.traces_seen = 0;
        self.index = Arc::new(index);
        self.cmds.tx = Some(cmd_tx);
        self.cmds.sent = 0;
        self.send(subscribe);
        // A run of a circuit whose last run went in rounds starts in them: that run measured it
        // falls behind, and this one (a record, a new rate) would only
        // measure it again over a meter window stepped whole, heard in Realtime as its first
        // second dropping out.
        if self.rounds.is_none() && !self.exact() && self.rounds_for.is_some() && self.rounds_for == self.key.as_ref().map(|k| (k.circuit, k.scope)) {
            self.rounds = Some(true);
            log!("elec sim: the run starts in rounds, as the last run of this circuit went");
            self.send(LiveCmd::SetRounds(true));
        }
        self.sync_bench_probes();
        self.pump_sinks();
        self.sync_members();
    }

    /// The newest instant's pin currents, for the next frame's currents job.
    fn due_currents(&mut self, pin_i: Arc<[f32]>) {
        self.currents_due = Some((pin_i, self.facts.generation));
    }

    /// The due currents job to the worker (on a frame); pin currents of an older numbering are
    /// dropped (the newer generation's instant is due soon).
    fn send_currents(&mut self, segs: &mut SegRows) {
        let Some((pin_i, generation)) = self.currents_due.take() else { return };
        if generation != self.facts.generation {
            return;
        }
        let trees = segs.resolved(&self.index);
        if trees.is_empty() {
            return;
        }
        let Some(spawn) = self.spawn.as_deref() else { return };
        let c = self.currents.get_or_insert_with(|| CurrentsLink::new(spawn));
        let seq = c.submit(trees, pin_i);
        if self.currents_gen != self.facts.generation {
            self.currents_gen = self.facts.generation;
            self.currents_from = seq;
        }
    }

}

impl Drop for SimHost {
    fn drop(&mut self) {
        // the worker is told to stop rather than left to a closed channel, and a warm made ahead
        self.send(LiveCmd::Stop);
        self.leave_ahead();
        self.cmds.tx = None;
        self.sinks.tx = None;
    }
}

/// Where a live run's work goes, as the host's live bench gives it: its packs on `helpers`, its
/// checks (shadow rounds, chain trees) and a slow plan as background jobs below them, the checks
/// ahead of the plan work under the budget and as many as it can start (`ahead`; with none, as
/// many as the packs leave free). A bench rig that runs a circuit as the host does calls this, or
/// [`host_rounds`], with a [`RunPool`]'s roles.
pub fn host_pool(sim: &mut LiveSim, helpers: Option<Helpers>, background: Option<Helpers>, ahead: Option<CheckPost>) {
    sim.set_helpers(helpers);
    sim.set_checks(background, true);
    sim.set_checks_ahead(ahead);
}

/// Sets a live run up as the host's live bench does, on `run_live`'s worker: its work where
/// [`host_pool`] puts it; out of rounds to start; `states` its memory of what earlier runs of the
/// circuit refused and landed and its planner's cache, so it starts with them; and its planner's
/// work on `background` too, behind the checks (`ahead`), its plan held accurate to `band`. The
/// planner's refusal comes back: the run then steps whole.
pub fn host_rounds(sim: &mut LiveSim, helpers: Option<Helpers>, background: Option<Helpers>, ahead: Option<CheckPost>, states: StateCache, band: f64) -> Result<(), fab_elec_sim::SimDiag> {
    host_pool(sim, helpers, background.clone(), ahead);
    let _ = sim.set_rounds(false);
    sim.set_memory(Some(states.clone()));
    let planner = BlockPlanner { pool: background, states, ..BlockPlanner::for_run(sim, band) };
    sim.set_planner(Some(Box::new(planner)))
}

/// What a start or a replacement of the circuit pays first, made ahead: while the host's run is
/// stopped, for the next start (`SimHost::warm_ahead`), and while it plays, for a structure edit's
/// replacement (`SimHost::hold_replace`). `sim` is the run made as the host makes it but never
/// stepped. The state it warms at is its board settled with the pads in the modes the newest run's
/// start warmed in (`StateCache::start_pads`, by name: what the process's set-up left them in), and
/// its planner's warm (`round::warm_plan`) is made there on this thread under `stop`, with the
/// options [`host_rounds`] gives it and its work on `background`; the warm keeps the new circuit's
/// cut, which carries the start or the replacement. A circuit that can carry a kept cut already
/// (`round::can_carry`) is left to it, and nothing is made. Whether the run's key is planned when
/// it returns.
pub fn warm_ahead_of(sim: LiveSim, background: Option<Helpers>, states: StateCache, band: f64, stop: Arc<std::sync::atomic::AtomicBool>) -> bool {
    let mut whole = sim.solver().clone();
    for (name, mode) in states.start_pads() {
        if let Some(dev) = whole.find_device(&name) {
            whole.pin_write(dev, -1.0, mode);
        }
    }
    // with no rest there, the present as it stands: only the warm's own plan, which is dropped, reads it
    let _ = fab_elec_sim::analysis::op::operating_point(&mut whole, false);
    let planner = BlockPlanner { pool: background, states, ..BlockPlanner::for_run(&sim, band) };
    if fab_elec_sim::analysis::round::can_carry(sim.circuit(), &whole, &planner.opts, &planner.states) {
        return false;
    }
    fab_elec_sim::rounds::with_stop(stop, || fab_elec_sim::analysis::round::warm_plan(sim.circuit(), &whole, sim.t(), &planner.opts, &planner.states, planner.pool.as_ref()))
}

/// How a live run is set up, in one line that the host's live bench and a bench rig each print as
/// a run starts, so a rate row that did not run as the host does shows it: its pace, sample rate
/// and resolution, the band its plan is held to, where its work goes (`pool`: `Spawn::pool_name`,
/// `RunPool::name`; `host_pool`), and whether its LU plans compile (`Options::plan_jit`).
pub fn setup_line(sim: &LiveSim, band: f64, pool: &str) -> String {
    let spec = sim.spec();
    let pace = if spec.rate.is_finite() { format!("paced {}x", spec.rate) } else { "unpaced".into() };
    let res = if spec.res > 0.0 { format!("Fast {:.0} us", spec.res * 1e6) } else { "Exact".into() };
    let band = if band.is_finite() { format!("band {:.1} kHz", band / 1e3) } else { "every point".into() };
    let work = match sim.pool_roles() {
        (None, _) => "whole, no helpers".to_string(),
        (Some(h), Some((c, true))) => format!("{pool}: packs on {h} helpers, checks and planning below them on {c}"),
        (Some(h), Some((c, false))) => format!("{pool}: packs on {h} helpers, checks and planning on {c} others"),
        (Some(h), None) => format!("{pool}: packs on {h} helpers, checks on the run's worker"),
    };
    let lu = if sim.solver().opts.plan_jit.is_some() { "LU plans compiled" } else { "LU plans interpreted" };
    format!("set-up: {pace}, {:.1} kHz, {res}, {band}; {work}; {lu}", sim.sample_rate() / 1e3)
}

/// The app's task pool as its live runs use it: one pool with priority classes, a run's packs
/// at `Class::Visible` (`helpers`), its planner's work at `Class::Visible` too and its checks at
/// `Class::Background` on the same workers (`background`, `checks`), and Fast's LU plans
/// compiled at `Class::Background` (`plan_jit`), all of them but the packs under one [`Budget`].
/// A bench rig without a window takes the same (`of_process`).
#[derive(Clone)]
pub struct RunPool {
    pool: TaskPool,
    /// The LU plans compiled for this app's runs, kept across its plans and lowerings.
    codes: Arc<CodeCache>,
    budget: Arc<Budget>,
    /// The live run its helpers serve is heard as it is made (`heard_priority`): its packs post at
    /// `Class::Interactive` then, at `Class::Visible` otherwise.
    heard: Arc<std::sync::atomic::AtomicBool>,
}

type WaitingJobs = VecDeque<(Box<dyn FnOnce() + Send>, RunJob)>;

/// What a job of a run's [`Budget`] is, for the pool: its class, its name and how it is admitted.
#[derive(Clone, Copy)]
struct RunJob {
    class: Class,
    label: &'static str,
    kind: JobKind,
}

/// How the budget admits a job (`BudgetState::may_start`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    /// Within the budget's places, or as the one check past awaited work holding them all.
    Check,
    /// What the sound waits for: within the budget's places.
    Awaited,
    /// What nothing waits for: within the places awaited work leaves, and at most `behind_slots`
    /// at once while packs post.
    Behind,
}

/// A check runs again what a round already ran: nothing waits for it (one that is late is owed,
/// and the run goes on).
const CHECK: RunJob = RunJob { class: Class::Background, label: "elec run check", kind: JobKind::Check };
/// The planner's work the run's sound waits for: a run that starts in rounds holds its start until
/// the plan is in (a cold or a rewired start), and a refusal or a breach is wrong until its plan
/// is. Visible, as a firmware compile is (on a first launch, at Background it waited 0.45 s behind
/// the shader pipeline compiles' CPU tokens with every pool worker idle, and the first sound with
/// it).
const PLAN_WORK: RunJob = RunJob { class: Class::Visible, label: "elec run plan work", kind: JobKind::Awaited };
/// The planner's work nothing waits for (`rounds::behind`): the key's own plan behind a carried
/// start, which already plays. Background, behind while it runs (`set_thread_work`: on macOS at
/// QoS BACKGROUND while the run's packs play, so on Apple silicon on the efficiency cores), and admitted below
/// awaited work: on a large box its units at Visible on every free place slowed the run's own
/// block 0 by a fifth and its packs by a third while they ran (they shared the packs' cores'
/// sibling threads), and a start that carried its plan played gaps until its own plan was in; on
/// a base M4 at UTILITY its units ran on the performance cores beside the packs (11-22).
const BEHIND_WORK: RunJob = RunJob { class: Class::Background, label: "elec run plan work behind", kind: JobKind::Behind };
/// An LU plan's compile: the plan runs interpreted until its code is back.
const LU_COMPILE: RunJob = RunJob { class: Class::Background, label: "elec run LU compile", kind: JobKind::Awaited };

/// What the live runs post beside their packs (checks, slow plans, the planner's units, LU plan
/// compiles), each at its class (`RunJob`), at most `slots` of it running at once while packs are
/// posting: the pool's workers but one, less the packs' helpers. A job holds its worker until it
/// ends, so such work on every worker makes a round's pack wait for one to end. With no pack posted
/// for `PACKS_RECENT` (a run stepping whole, making its first plan, or none running) there is no
/// pack to keep a worker for, and `open` run at once: the pool's workers but one. A check never
/// waits behind plan work: with plan work in every place, one check runs past the budget, on the
/// worker kept for a lowering or a render (a check job takes microseconds a round; a slow plan can
/// hold its place for seconds).
///
/// Behind work (`JobKind::Behind`) takes places only within `slots` and, while packs post, at most
/// `behind_slots` at once; checks and awaited work count past it, so they never wait behind it
/// (they run over `slots` by the behind jobs running, `behind_slots` at most). Jobs past the budget
/// wait here. As places open, waiting checks go first, then waiting awaited work, then behind work,
/// each in posting order. Nothing is dropped: a bound only makes a job wait for a place, and every
/// place a waiting job needs is held by a running job whose end starts the next. A post takes
/// O(1), holding the lock for three counters and a queue's end.
struct Budget {
    pool: TaskPool,
    slots: usize,
    open: usize,
    /// The behind jobs that may run at once while packs post: the fast cores less the UI's, the
    /// run's worker's and the packs' helpers, at least one (`RunPool::behind_count`).
    behind_slots: usize,
    /// When a pack was last posted (`packed`): ns past `since`, plus one; 0 for none yet.
    since: std::time::Instant,
    last_pack: std::sync::atomic::AtomicU64,
    state: Mutex<BudgetState>,
}

#[derive(Default)]
struct BudgetState {
    /// The jobs posted to the pool and not ended, and the checks and behind jobs among them.
    running: usize,
    checks: usize,
    behind: usize,
    waiting_checks: WaitingJobs,
    waiting_plans: WaitingJobs,
    waiting_behind: WaitingJobs,
}

/// How many jobs may run now (`Budget::limits`): all of them, and the behind ones.
#[derive(Clone, Copy)]
struct Limits {
    all: usize,
    behind: usize,
}

impl BudgetState {
    /// Whether a job of `kind` may start under `limits`: awaited work within them past the behind
    /// jobs, a check within them or as the one check past awaited work holding every place, behind
    /// work within them and within the behind limit.
    fn may_start(&self, limits: Limits, kind: JobKind) -> bool {
        let awaited = self.running - self.behind;
        match kind {
            JobKind::Check => awaited < limits.all || self.checks == 0,
            JobKind::Awaited => awaited < limits.all,
            JobKind::Behind => self.running < limits.all && self.behind < limits.behind,
        }
    }

    fn start(&mut self, kind: JobKind) {
        self.running += 1;
        self.checks += usize::from(kind == JobKind::Check);
        self.behind += usize::from(kind == JobKind::Behind);
    }

    fn end(&mut self, kind: JobKind) {
        self.running -= 1;
        self.checks -= usize::from(kind == JobKind::Check);
        self.behind -= usize::from(kind == JobKind::Behind);
    }

    fn waiting(&mut self, kind: JobKind) -> &mut WaitingJobs {
        match kind {
            JobKind::Check => &mut self.waiting_checks,
            JobKind::Awaited => &mut self.waiting_plans,
            JobKind::Behind => &mut self.waiting_behind,
        }
    }

    /// The next waiting job that may start, checks first, then awaited work, then behind work,
    /// counted as running.
    fn take_next(&mut self, limits: Limits) -> Option<(Box<dyn FnOnce() + Send>, RunJob)> {
        let kind = [JobKind::Check, JobKind::Awaited, JobKind::Behind].into_iter().find(|&k| self.may_start(limits, k) && !self.waiting(k).is_empty())?;
        let job = self.waiting(kind).pop_front()?;
        self.start(kind);
        Some(job)
    }
}

/// How long after a run's last pack post its packs count as running: past any pause a paced run
/// makes between its advances (one publication interval and its slice).
const PACKS_RECENT: std::time::Duration = std::time::Duration::from_millis(50);

impl Budget {
    fn new(pool: TaskPool, slots: usize, open: usize, behind_slots: usize) -> Self {
        Budget { pool, slots, open, behind_slots, since: std::time::Instant::now(), last_pack: Default::default(), state: Mutex::default() }
    }

    /// A pack was posted. One store.
    fn packed(&self) {
        self.last_pack.store(self.since.elapsed().as_nanos() as u64 + 1, std::sync::atomic::Ordering::Relaxed);
    }

    /// How many may run now: `slots`, and `behind_slots` of them behind, while packs post; `open`
    /// otherwise.
    fn limits(&self) -> Limits {
        let last = self.last_pack.load(std::sync::atomic::Ordering::Relaxed);
        let posting = last != 0 && (self.since.elapsed().as_nanos() as u64) < last + PACKS_RECENT.as_nanos() as u64;
        if posting {
            Limits { all: self.slots, behind: self.behind_slots }
        } else {
            Limits { all: self.open, behind: self.open }
        }
    }

    fn post_check(self: &Arc<Self>, job: Box<dyn FnOnce() + Send>) {
        self.post(job, CHECK);
    }

    fn post(self: &Arc<Self>, job: Box<dyn FnOnce() + Send>, kind: RunJob) {
        let limits = self.limits();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.may_start(limits, kind.kind) {
            state.start(kind.kind);
            drop(state);
            self.submit(job, kind);
        } else {
            state.waiting(kind.kind).push_back((job, kind));
        }
    }

    fn check_room(&self) -> usize {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.limits().all.saturating_sub(state.running - state.behind) + usize::from(state.waiting_checks.is_empty())
    }

    /// One job to the pool in a place already counted; the place goes to the next job when it
    /// ends, by unwinding too. Internal, as the packs are (`RunPool::helpers`).
    fn submit(self: &Arc<Self>, job: Box<dyn FnOnce() + Send>, kind: RunJob) {
        struct Slot(Arc<Budget>, JobKind);
        impl Drop for Slot {
            fn drop(&mut self) {
                self.0.next(self.1);
            }
        }
        let slot = Slot(self.clone(), kind.kind);
        let deferred = kind.kind == JobKind::Behind;
        match self.pool.submit_internal_named(kind.class, kind.label, Box::new(move || {
            let _slot = slot;
            // behind work runs behind: on macOS off the performance cores the packs and the
            // worker play on (`set_thread_work`), unchanged elsewhere
            struct Deferred(bool);
            impl Drop for Deferred {
                fn drop(&mut self) {
                    if self.0 {
                        makepad_cpu_budget::set_thread_work(makepad_cpu_budget::WORK_PLAIN);
                    }
                }
            }
            if deferred {
                makepad_cpu_budget::set_thread_work(makepad_cpu_budget::WORK_BEHIND);
            }
            let _deferred = Deferred(deferred);
            job();
        })) {
            Ok(h) => h.detach(),
            Err(_) => log!("elec sim: the pool is closed; a run's job ({}) does not run", kind.label),
        }
    }

    /// A job ended. Waiting checks, then awaited work, then behind work take the places free now,
    /// more than its own when the packs have stopped.
    fn next(self: &Arc<Self>, kind: JobKind) {
        let limits = self.limits();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.end(kind);
        while let Some((job, kind)) = state.take_next(limits) {
            drop(state);
            self.submit(job, kind);
            state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl RunPool {
    pub fn new(pool: TaskPool) -> Self {
        // the workers the packs leave, but one so a lowering or a render still finds one free
        let open = pool.worker_count().saturating_sub(1).max(1);
        let helpers = Self::helper_count(&pool);
        let slots = open.saturating_sub(helpers).max(1);
        let budget = Arc::new(Budget::new(pool.clone(), slots, open, Self::behind_count(helpers)));
        Self { pool, codes: Arc::default(), budget, heard: Arc::default() }
    }

    /// A tool's without a window (a bench rig): the app's pool as a Cx makes it
    /// (`Cx::task_pool`), that Cx kept for the process.
    pub fn of_process() -> Self {
        let cx: &'static Cx = Box::leak(Box::new(Cx::new(Box::new(|_, _| {}))));
        Self::new(cx.task_pool())
    }

    /// The helper cores a live run in rounds posts its packs to (`Spawn::helpers`).
    pub fn helpers(&self) -> Option<Helpers> {
        // a pack the pool refuses stays posted, and the run's worker takes it back itself; the
        // run's jobs are internal: their results go back over the run's own channels, never to a
        // handle the UI polls, so a job's end wakes no widget (a `submit` raises `Event::Signal`
        // through the whole tree at each: thousands a second from a run's packs); they go under
        // their own name, which the pool's summary and the CPU budget tell them from any other
        // boxed job by
        let count = Self::helper_count(&self.pool);
        if count == 0 {
            return None;
        }
        let (pool, budget, heard) = (self.pool.clone(), self.budget.clone(), self.heard.clone());
        let post: Post = Arc::new(move |pack: Box<dyn FnOnce() + Send>| {
            budget.packed();
            let class = if heard.load(std::sync::atomic::Ordering::Relaxed) { Class::Interactive } else { Class::Visible };
            if let Ok(h) = pool.submit_internal_named(class, "elec pack", pack) {
                h.detach();
            }
        });
        Some(Helpers { post, count })
    }

    /// What a live run heard as it is made does on its worker as that starts and ends
    /// (`LiveSim::set_heard_priority`, `Spawn::heard`): its worker takes the UI thread's class
    /// (`CxThreadPriority::UserInteractive`) and its packs `Class::Interactive`, and a spawned
    /// worker's (`Normal`) and `Class::Visible` after. The worker's class is set as `ThreadOptions`
    /// sets a spawned thread's (`Cx::set_thread_priority`): on macOS the user-interactive QoS
    /// against the default, on Windows the highest thread priority against normal; on Linux both
    /// are the process's own nice (an unprivileged thread cannot go below it), so there it changes
    /// nothing. A refusal is logged, and the worker runs on as it was. Called on a change only.
    pub fn heard_priority(&self) -> Box<dyn FnMut(bool) + Send> {
        let packs = self.heard.clone();
        Box::new(move |heard| {
            packs.store(heard, std::sync::atomic::Ordering::Relaxed);
            // a worker heard as it runs holds a core against a deadline: the budget places the
            // JIT's builds of hot code around it (`fast_room`)
            makepad_cpu_budget::set_thread_deadline(heard);
            let priority = if heard { CxThreadPriority::UserInteractive } else { CxThreadPriority::Normal };
            let status = Cx::set_thread_priority(priority);
            if status != makepad_platform::thread::PriorityStatus::Applied {
                log!("elec sim: the live worker's priority stays as it was ({priority:?} asked: {status:?})");
            }
        })
    }

    /// The pool's workers but one, so a lowering or a render still finds one free, and the fastest
    /// cores but one: a round waits for its slowest pack, and a helper on a slower tier of cores
    /// (an Apple chip's second level) holds every round back. If two or more helpers would hold
    /// every general worker, leave one the packs never hold for the budget's checks and plans.
    fn helper_count(pool: &TaskPool) -> usize {
        let fastest = makepad_platform::thread::machine_topology().performance;
        let general = pool.worker_count().saturating_sub(1);
        let count = general.min(fastest.saturating_sub(1));
        if count == general && count > 1 { count - 1 } else { count }
    }

    /// The behind jobs that may run at once beside `helpers` while packs post: the fast cores
    /// less one for the UI and the audio callback, one for the run's worker and the helpers, at
    /// least one. (With today's helpers that is one on every box: the helpers take the fast cores
    /// but one.)
    fn behind_count(helpers: usize) -> usize {
        makepad_platform::thread::machine_topology().performance.saturating_sub(2 + helpers).max(1)
    }

    /// The background jobs a live run's checks and planner go to (`Spawn::background`), under the
    /// budget. Its count is what the checks and the planner's units may have posted at once (the
    /// pool's workers but one); the budget bounds how many of them run. A unit at a time each
    /// (`fan_out`), so the pool takes the run's packs between them. Work posted from behind work
    /// (`rounds::behind`: a plan behind a carried start, and its units) goes as behind work.
    pub fn background(&self) -> Option<Helpers> {
        let count = self.pool.worker_count().saturating_sub(1);
        if count == 0 {
            return None;
        }
        let budget = self.budget.clone();
        let post: Post = Arc::new(move |unit: Box<dyn FnOnce() + Send>| budget.post(unit, if fab_elec_sim::rounds::behind() { BEHIND_WORK } else { PLAN_WORK }));
        Some(Helpers { post, count })
    }

    /// Where a live run posts its checks ahead of waiting plan work and asks how many the budget
    /// has room for (`Spawn::checks`).
    pub fn checks(&self) -> Option<CheckPost> {
        if self.pool.worker_count() <= 1 {
            return None;
        }
        let (post, room) = (self.budget.clone(), self.budget.clone());
        Some(CheckPost { post: Arc::new(move |job| post.post_check(job)), room: Arc::new(move || room.check_room()) })
    }

    /// Where a Fast run's LU plans compile (`Spawn::plan_jit`, `Options::plan_jit`), under the
    /// budget.
    pub fn plan_jit(&self) -> Option<PlanJit> {
        plan_jit(&self.budget, &self.codes)
    }

    /// What it is, for a run's set-up line (`Spawn::pool_name`).
    pub fn name(&self) -> String {
        format!("the app's task pool of {} workers, packs and plan work at Visible (packs Interactive while heard), checks, LU compiles and plan work behind a carried start at Background, {} of them at once while packs run ({} of them behind) and a check past plan work",
            self.pool.worker_count(), self.budget.slots, self.budget.behind_slots)
    }
}

/// The app's threads: the Cx's spawner for long-lived loops, its pool for jobs and runs.
struct CxSpawn {
    threads: ThreadSpawner,
    run: RunPool,
}

impl Spawn for CxSpawn {
    fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| SignalToUI::set_ui_signal())
    }

    fn worker(&self, name: &'static str, f: Box<dyn FnOnce() + Send>) {
        match self.threads.spawn_worker(ThreadOptions { name: Some(name.into()), ..Default::default() }, f) {
            Ok(h) => h.detach(),
            Err(e) => log!("elec sim: no thread for {name} ({e:?}); the live bench does not run"),
        }
    }

    fn helpers(&self) -> Option<Helpers> {
        self.run.helpers()
    }

    fn heard(&self) -> Option<Box<dyn FnMut(bool) + Send>> {
        Some(self.run.heard_priority())
    }

    fn background(&self) -> Option<Helpers> {
        self.run.background()
    }

    fn checks(&self) -> Option<CheckPost> {
        self.run.checks()
    }

    fn plan_jit(&self) -> Option<PlanJit> {
        self.run.plan_jit()
    }

    fn pool_name(&self) -> String {
        self.run.name()
    }

    fn job(&self, f: Box<dyn FnOnce() + Send>) {
        // a target without threads refuses the job: it then runs here (a lowering: milliseconds)
        let slot = Arc::new(Mutex::new(Some(f)));
        let theirs = slot.clone();
        let submitted = self.run.pool.submit(Class::Visible, move || {
            if let Some(f) = theirs.lock().unwrap_or_else(|e| e.into_inner()).take() {
                f();
            }
        });
        match submitted {
            Ok(h) => h.detach(),
            Err(_) => {
                if let Some(f) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    f();
                }
            }
        }
    }
}

/// Fast's compiled LU plans: each compiles in a background job of the pool while the interpreted
/// plan goes on running, and the run takes the code at a step boundary once it is back.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn plan_jit(budget: &Arc<Budget>, codes: &Arc<CodeCache>) -> Option<PlanJit> {
    let budget = budget.clone();
    let post: Post = Arc::new(move |job: Box<dyn FnOnce() + Send>| budget.post(job, LU_COMPILE));
    Some(PlanJit { post, compiler: Arc::new(LoggedCompiler), codes: codes.clone() })
}

/// No code where the relax backend emits none: every plan stays interpreted.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn plan_jit(_budget: &Arc<Budget>, _codes: &Arc<CodeCache>) -> Option<PlanJit> {
    None
}

/// The plan compiler, with one log line per plan: what it compiled to and how long it took.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
struct LoggedCompiler;

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
impl PlanCompiler for LoggedCompiler {
    fn compile(&self, plan: &LuPlan) -> Option<Arc<dyn CompiledLu>> {
        let code = fab_elec_simjit::Compiler.compile(plan);
        match &code {
            Some(code) => {
                let s = code.stats();
                log!("elec sim: an LU plan of {} unknowns compiled: {} ops, {} B in {:.2} ms", plan.n, s.ops, s.code_bytes, s.compile_ns as f64 * 1e-6);
            }
            None => log!("elec sim: an LU plan of {} unknowns ({} ops) stays interpreted: no code for it", plan.n, plan.ops()),
        }
        code
    }
}

/// Each co-simulated pin's row: its pin table name to the pin index whose pad its GPIO drives
/// (the GPIO's terminal 0 in `ValuePlan::pins`). Once per lowering or pin table, O(terminals).
fn co_join(table: &[CoSimPin], plan: &ValuePlan) -> HashMap<String, u32> {
    let by_gpio: HashMap<&str, &str> = table.iter().map(|row| (row.gpio.as_str(), row.name.as_str())).collect();
    let mut join = HashMap::new();
    if by_gpio.is_empty() {
        return join;
    }
    for (pin, terminals) in plan.pins.iter().enumerate() {
        for t in terminals.iter().filter(|t| t.terminal == 0) {
            if let Some(name) = by_gpio.get(t.device.as_str()) {
                join.insert((*name).to_owned(), pin as u32);
            }
        }
    }
    join
}

#[cfg(test)]
mod press_tests {
    use super::*;
    use fab_elec_core::CompId;

    #[test]
    fn envelope_asks_follow_the_take_and_keep_unchanged_snapshots() {
        use fab_elec_core::NetId;
        use fab_elec_sim::live::GridFrame;

        let mut host = SimHost::new();
        let mut live = SimLive::default();
        let net = |n| NetRef { board: BoardId(0), net: NetId(n) };
        let index = Arc::make_mut(&mut host.index);
        index.add_net(net(3));
        index.add_net(net(9));
        let generation = index.generation;
        let push = |host: &mut SimHost, k: i64, v: f32| host.take.push(generation, 0.25, &GridFrame {
            k, since: k as f64 * 0.25, t: (k + 1) as f64 * 0.25,
            net_mean: vec![1.0, v], net_min: vec![1.0, v - 1.0], net_max: vec![1.0, v + 1.0],
            ..GridFrame::default()
        });
        push(&mut host, 0, 4.0);
        let empty = live.envelopes.clone();
        host.act(&SimAction::Envelope { id: 8, net: Some(net(5)) });
        assert!(!host.publish_envelopes(&mut live) && Arc::ptr_eq(&empty, &live.envelopes), "an unknown net publishes nothing");

        host.act(&SimAction::Envelope { id: 8, net: Some(net(9)) });
        assert!(host.publish_envelopes(&mut live), "a numbered net publishes its take");
        let first = live.envelopes.clone();
        assert_eq!((first[0].id, first[0].net, first[0].grid), (8, net(9), 0.25));
        assert_eq!(first[0].slots[..], [EnvSlot { t0: 0.0, t1: 0.25, mean: 4.0, min: 3.0, max: 5.0 }]);
        assert!(!host.publish_envelopes(&mut live) && Arc::ptr_eq(&first, &live.envelopes), "an unchanged take keeps its Arc");
        push(&mut host, 2, 6.0);
        assert!(host.publish_envelopes(&mut live), "a new tail publishes while a reader holds the previous one");
        assert_eq!((first[0].slots.len(), live.envelopes[0].slots.len()), (1, 2));
        drop(first);
        push(&mut host, 1, 5.0);
        assert!(host.publish_envelopes(&mut live), "filling an older gap invalidates the tail cache");
        push(&mut host, 1, 8.0);
        assert!(host.publish_envelopes(&mut live), "a rewritten interior publishes even with equal length and last slot");
        assert_eq!(live.envelopes[0].slots.iter().map(|s| s.mean).collect::<Vec<_>>(), [4.0, 8.0, 6.0]);
        let rewritten = live.envelopes.clone();
        push(&mut host, 1, 8.0);
        assert!(!host.publish_envelopes(&mut live) && Arc::ptr_eq(&rewritten, &live.envelopes), "an identical rewrite keeps its Arc");
        host.act(&SimAction::Envelope { id: 8, net: None });
        assert!(host.publish_envelopes(&mut live) && live.envelopes.is_empty(), "withdrawing removes the envelope");
    }

    #[test]
    fn held_publication_follows_minimum_press_and_stop() {
        let mut host = SimHost::new();
        let comp = CompRef { board: BoardId(0), comp: CompId(0) };
        let index = Arc::make_mut(&mut host.index);
        index.add_comp(comp, 0, Some(fab_elec_bench::values::Tune::device("button", "value")));
        index.add_momentary(comp);
        let mut live = SimLive::default();
        let empty = live.held.clone();
        assert!(!host.publish_held(&mut live), "an unchanged empty set needs no publication");
        assert!(Arc::ptr_eq(&empty, &live.held), "empty frames retain the Arc");
        assert!(host.command(&SimCmd::press(comp, true)), "a numbered momentary input accepts down");
        assert!(host.publish_held(&mut live), "down publishes a changed set");
        assert_eq!(&*live.held, &[comp], "every view sees the held component");
        let down = live.held.clone();
        assert!(!host.publish_held(&mut live), "an unchanged held set needs no publication");
        assert!(Arc::ptr_eq(&down, &live.held), "holding keeps the same Arc");
        assert!(host.command(&SimCmd::press(comp, false)), "an early up is accepted");
        let times: Vec<(f64, f64)> = host.cmds.queue.iter().filter_map(|c| match c {
            LiveCmd::At(t, cmd) => match &**cmd {
                LiveCmd::SetParam { value, .. } => Some((*t, *value)),
                _ => None,
            },
            _ => None,
        }).collect();
        assert_eq!(times, [(0.0, 1.0), (MIN_PRESS, 0.0)], "the press is two timed events, sent at once: down now, up MIN_PRESS later");
        host.release_held(MIN_PRESS * 0.5);
        assert!(!host.publish_held(&mut live), "an early release remains held before MIN_PRESS");
        assert!(Arc::ptr_eq(&down, &live.held), "a waiting release retains the held Arc");
        host.release_held(MIN_PRESS);
        assert!(host.publish_held(&mut live), "reaching MIN_PRESS publishes the release");
        assert!(live.held.is_empty(), "the released input leaves the set");
        assert!(!Arc::ptr_eq(&down, &live.held), "release replaces the Arc");
        host.command(&SimCmd::press(comp, true));
        host.publish_held(&mut live);
        host.command(&SimCmd::Stop);
        assert!(host.publish_held(&mut live), "Stop publishes the cleared set immediately");
        assert!(live.held.is_empty(), "Stop leaves no held inputs");
        let stopped = live.held.clone();
        assert!(!host.publish_held(&mut live), "stopped frames need no new publication");
        assert!(Arc::ptr_eq(&stopped, &live.held), "stopped frames retain the Arc");
    }

    #[test]
    fn a_turned_knob_publishes_its_place_until_a_new_numbering_or_stop() {
        let mut host = SimHost::new();
        let (knob, other) = (CompRef { board: BoardId(0), comp: CompId(0) }, CompRef { board: BoardId(0), comp: CompId(1) });
        let index = Arc::make_mut(&mut host.index);
        index.add_comp(knob, 0, Some(fab_elec_bench::values::Tune::device("pot", "pos")));
        index.add_knob(knob, 0.3);
        index.add_comp(other, 0, Some(fab_elec_bench::values::Tune::device("r1", "value")));
        let mut live = SimLive::default();
        assert!(!host.publish_turned(&mut live), "nothing turned, nothing to publish");
        host.command(&SimCmd::SetParam { comp: other, value: 2.0e3 });
        assert!(!host.publish_turned(&mut live), "a value that is no knob's turns nothing");
        host.command(&SimCmd::SetParam { comp: knob, value: 0.6 });
        host.command(&SimCmd::SetParam { comp: knob, value: 0.7 });
        assert!(host.publish_turned(&mut live), "a turn publishes");
        assert_eq!(&*live.turned, &[(knob, 0.7)], "a knob shows its newest place");
        let turned = live.turned.clone();
        assert!(!host.publish_turned(&mut live) && Arc::ptr_eq(&turned, &live.turned), "an unchanged place keeps its Arc");
        let next = host.index.generation.wrapping_add(1);
        Arc::make_mut(&mut host.index).generation = next;
        assert!(host.publish_turned(&mut live) && live.turned.is_empty(), "a new numbering starts the knobs where the design puts them");
        host.command(&SimCmd::SetParam { comp: knob, value: 0.9 });
        assert!(host.publish_turned(&mut live) && &*live.turned == &[(knob, 0.9)], "the new numbering's turn publishes");
        // while the recording plays, a knob stands where the take had it at the instant heard
        host.turns = vec![(knob, next, 0.5, 0.2), (knob, next, 1.5, 0.8), (other, next, 0.1, 0.4)];
        host.shown = Some(Shown { t: 1.0, moving: true, co: None, slot_t: 1.0, revision: 0 });
        assert!(host.publish_turned(&mut live) && &*live.turned == &[(knob, 0.2), (other, 0.4)], "the turns at or before the instant heard publish");
        host.shown = Some(Shown { t: 2.0, moving: true, co: None, slot_t: 2.0, revision: 0 });
        assert!(host.publish_turned(&mut live) && &*live.turned == &[(knob, 0.8), (other, 0.4)], "a later turn shows once heard");
        host.shown = None;
        host.command(&SimCmd::Stop);
        assert!(host.turns.is_empty(), "Stop forgets the take's turns");
        assert!(host.publish_turned(&mut live) && &*live.turned == &[(knob, 0.9)], "Stop keeps the knob where it was turned: the next run starts it there");
    }

    /// The guard for a knob's face: it always shows the value the run uses (the host's `params`),
    /// through a Stop, a turn while stopped and the Play after it, a new take, a restart that
    /// keeps the knobs and a lowering that does not.
    #[test]
    fn a_knob_shows_the_value_the_run_uses() {
        let mut host = SimHost::new();
        let knob = CompRef { board: BoardId(0), comp: CompId(0) };
        let index = Arc::make_mut(&mut host.index);
        index.add_comp(knob, 0, Some(fab_elec_bench::values::Tune::device("pot", "pos")));
        index.add_knob(knob, 0.3);
        let mut live = SimLive::default();
        let pos = |host: &SimHost| host.params.get(&("pot".to_owned(), "pos".to_owned())).copied();
        host.command(&SimCmd::SetParam { comp: knob, value: 0.7 });
        assert_eq!(pos(&host), Some(0.7), "a turn sets the run's value");
        host.command(&SimCmd::Stop);
        assert!(host.publish_turned(&mut live) && &*live.turned == &[(knob, 0.7)] && pos(&host) == Some(0.7), "Stop: the value stays and so does the face");
        // turned while stopped, then Play: the design lowered again with the run's values
        host.command(&SimCmd::SetParam { comp: knob, value: 0.9 });
        assert!(host.publish_turned(&mut live) && &*live.turned == &[(knob, 0.9)] && pos(&host) == Some(0.9), "a turn while stopped: the value and the face");
        host.command(&SimCmd::Play);
        let renumbered = |host: &mut SimHost| {
            let mut next = ValueIndex::new(host.index.generation.wrapping_add(1));
            next.add_comp(knob, 0, Some(fab_elec_bench::values::Tune::device("pot", "pos")));
            next.add_knob(knob, 0.3);
            host.lowered_values(&next);
            host.index = Arc::new(next);
            host.index.generation
        };
        let generation = renumbered(&mut host);
        assert!(pos(&host) == Some(0.9) && !host.publish_turned(&mut live) && &*live.turned == &[(knob, 0.9)], "Play after a Stop: the run starts with the value the face shows");
        // the new take starts the knob where the run's value puts it, from its first instant
        host.start_take_turns(generation);
        host.shown = Some(Shown { t: 0.0, moving: true, co: None, slot_t: 0.0, revision: 0 });
        live.turned = Arc::default();
        assert!(host.publish_turned(&mut live) && &*live.turned == &[(knob, 0.9)], "a new take shows the knob at its turned place from t = 0");
        host.shown = None;
        // a restart for an edit keeps the run's values (`restart_run`): the face comes along
        host.keep_params = Some(host.params.clone());
        renumbered(&mut host);
        assert!(pos(&host) == Some(0.9) && !host.publish_turned(&mut live) && &*live.turned == &[(knob, 0.9)], "a kept restart: the value and the face both stay");
        // a lowering that puts the circuit's own values back puts the knob back with them
        let index = host.index.clone();
        host.lowered_values(&index);
        assert!(pos(&host).is_none() && host.publish_turned(&mut live) && live.turned.is_empty(), "the design's values back: the knob at the design's place");
    }
}
