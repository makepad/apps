//! Any microsplash machine as a co-simulated device in the circuit simulator's live worker: a
//! `fab_elec_mcu_emu::Machine` behind `fab_elec_sim::cosim::CoSim`.
//!
//! - Time. One conversion, `t = t_reset + tick / tick_hz` (`emu::time`), and its inverse
//!   `emu::tick_at` (the first tick at or after `t`). `t_reset` is the analog time of the attach,
//!   then of each chip reset.
//! - Outward. The CPU runs privately to its next effect: a pin's drive change (each at its own
//!   tick, even inside one instruction), an armed step's stop, a barrier, a reset, a fault, or the
//!   stretch's deadline. `next()` is that effect's time, and `run` applies what is due once the
//!   analog stands there. A stop ends the run before its instruction, which has not executed.
//! - Inward. Every coupled pin that is an input has two watches, rising at VIH and falling at VIL
//!   (its pin table row's thresholds, fractions of `Bus::supply` read in `run`), so between the two
//!   the pin keeps its level. A hit that changes a level is raised at the first instruction boundary at or
//!   after tick `ceil(max(hit, bus.t()))`. When the CPU has run past it, the adapter rewinds to the
//!   stretch's start, replays the stretch's earlier inputs at their ticks and runs there; effects
//!   the analog already took are not applied again. A pin that turns into an input is sampled once
//!   from its pad at that effect.
//! - Analog inputs. Sample requests join the drive queue's deadlines. At their exact ticks,
//!   after same-tick drive edges, the adapter gives the machine the pad voltage and supply.
//!   Answers since the mark are logged and supplied before replay, so a replayed request never
//!   reads a later voltage. Analog-only pins have no digital watches or input-level samples.
//! - Commits. A commit takes the machine's trace, link output and notes (so observations count
//!   committed execution only, never a re-run twice) and marks: the stretch's new start. It is
//!   taken only where no later hit can fall before it, the CPU standing at the analog's cycle: at
//!   a sync, a stop, a barrier, a reset, the deadline, and at effects and inputs that land there.
//!   When a live run goes back to an earlier copy of the device (an edit behind its front), the
//!   copy takes over the event sink and the machine's feeds (`take_outputs`, `put_outputs`). Its
//!   first offer voids what the other sent past the copy's place in the stream, which the copy
//!   sends again: the sink holds each committed event of the run it shows once. A feed (a panel's
//!   screen) publishes the copy's whole state next.
//! - Sync. `arm` and `state` first bring the CPU to the first boundary at or after `tick_at(t)` by
//!   re-running from the stretch's start, and commit there, settled (below): the reads after it
//!   re-run nothing, since a CPU on its mark stands on the first boundary at or after the analog's
//!   instant at that commit, behind no later one. Such a commit keeps the pace: the
//!   stretch deadline, an instant the analog steps to, counts from the last commit at a deadline,
//!   a stop, a barrier, a halt, a reset or the attach, so a publication made whenever the wall
//!   clock asks leaves the simulation's points as they are. A commit at an input or an effect
//!   keeps the pace too: whether it is taken depends on where a publication left the mark.
//! - Settle. A kernel about to read asks the device to settle (`CoSim::settle`): at its next
//!   commit with the analog, standing on the mark, `run` stops there instead of running ahead, so
//!   the reads that follow find the CPU at the analog's cycle and re-run nothing. A step's stop
//!   and a sync settle unasked. The next `run` goes on ahead: settling postpones the private run
//!   and moves no point, deadline or commit.
//! - Faults never stop the simulation: once the analog reaches a fault, `next()` is infinite.
//! - Board nets. The machine's nets (`Machine::nets`: a board's, a logic side's digital nets) are
//!   the device's logic nets. Their level changes are taken at each commit, as the trace is, timed
//!   in board time from the attach, so a change from a run the adapter rewound never comes out and
//!   a replayed stretch gives each change once. `take_logic` hands them over up to the analog's
//!   instant, after a sync when asked.
//!
//! Cost: per `run`, the due effects and one machine run to the next effect; per hit, and per
//! publication of an unsettled device, at most one re-run of a stretch (bounded by
//! `Config::stretch` ticks).

use fab_elec_mcu_emu::{
    tick_at, time, ChipState, CodeMap, Drive, DriveEdge, ExecCounts, FwTimer, Halt, LinkOut, Machine, Note, PcSet,
    Pending, Pull, Ran, SampleAt, Status, Until, Wake, TIME_EXACT_LIMIT,
};
use fab_elec_mcu_emu::board::{Audio, BusEvent};
use fab_elec_sim::cosim::{Bus, CoAudio, CoAudioRun, CoRun, CoSim, CoSimDevice, CoSimPin, CoState, StepTo, Watch};
use fab_elec_sim::trace::LogicEdge;
pub use fab_elec_mcu_emu::{FwCond, FwDebug, FwValue, PinUse, StepMap};
pub use fab_elec_mcu_rt::watch;
use fab_elec_sim::{PinEdge, PinMode, PinPull, SimDiag};
use std::any::Any;
use std::collections::VecDeque;
use std::sync::Arc;
pub mod eeprom;
pub mod logic;
pub mod net;
pub use net::{NetConn, NetSetup, NetStatus, SharedLog, Upstream};
pub use logic::{KeyPress, LogicKey, LogicKind, LogicMachine, LogicNet, LogicPart, LogicPin, LogicPlan, NetDriver};

/// One firmware event, for the host's event store. Only committed execution produces events,
/// so a re-run never sends one twice; each goes with its time, `t_reset + tick / f_clk`.
#[derive(Clone, Debug, PartialEq)]
pub enum FwFrame {
    /// A frame of the image's own watch stream (link 0), decoded as a board's reader would.
    Image(watch::Frame),
    /// Link 0 bytes the board's reader would reject.
    ImageBad(watch::Bad),
    /// A handler began: the device stood on its entry.
    Enter { handler: u32 },
    /// The running handler returned: the device stood on its return address.
    Exit { handler: u32 },
    /// A statement began, at its first address (when the sink asked for statements).
    Statement { pc: u32 },
    /// A modelled assumption the run met (a hardware race the model resolved).
    Note(&'static str),
}

impl FwFrame {
    /// Whether the frame is the image's (a board would send it) rather than the emulator's trace.
    pub fn is_image(&self) -> bool {
        matches!(self, FwFrame::Image(_) | FwFrame::ImageBad(_))
    }
}

/// Takes a batch of events, emptying it, and returns true; or leaves it as it was and returns
/// false (the host's channel is full). Nothing is ever dropped: refused events stay in order and
/// are offered again first, and the device does not advance until they are taken. The first
/// argument, `void`, is how many of the newest events the sink took before are void, to be dropped
/// before the batch goes in: the live run went back to an earlier copy of the device
/// (`CoSim::put_outputs`), which sends them again. Usually 0; a batch may be empty when it is not.
/// A refused offer drops nothing: its `void` comes again with the batch. The count is of the
/// device's stream, which starts at its reset, so a sink set on a device that had sent events
/// already (`set_events` after a run) can be told more than it took: it then drops all it took.
pub type EventSink = Box<dyn FnMut(u64, &mut Vec<(f64, FwFrame)>) -> bool + Send>;

/// The event stream, machine feeds and network handed to the earlier copy (`CoSim::take_outputs`).
struct Handoff {
    stream: Option<Stream>,
    feeds: Option<Box<dyn Any + Send>>,
    upstream: Option<Upstream>,
}

/// The sink and its place in the committed event stream.
struct Stream {
    sink: EventSink,
    streamed: u64,
    void: u64,
    spill: Vec<(f64, FwFrame)>,
}

/// The device's event sink. A fork takes none: its clone is empty.
struct Events(Option<EventSink>);

impl Clone for Events {
    fn clone(&self) -> Self {
        Events(None)
    }
}

/// How the device runs against the circuit.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Ticks a private stretch may run past its start before it commits: the bound on one
    /// re-run's work. It changes speed, never results.
    pub stretch: u64,
    /// The image's table (its step map, statements, handlers and variables), without which steps
    /// are refused and states name addresses only.
    pub debug: Option<Arc<FwDebug>>,
}

impl Config {
    /// The image's step map.
    pub fn map(&self) -> Option<&StepMap> {
        self.debug.as_ref().and_then(|d| d.map.as_ref())
    }
}

/// Where the device stands at a publication.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum At {
    /// In a handler's code, in the statement that began last (its id).
    Statement(u32),
    /// In the runtime, in its named routine.
    Runtime(&'static str),
    Sleeping,
    Halted(Halt),
    /// No statement has begun since the device's reset.
    Reset,
    /// The image has no table to place the device by.
    Unmapped,
}

/// One pin of the device: its name (`CoSimPin::name`), how the firmware uses it, its level
/// (None: unknown) and its PWM duty (zero unless `Pwm`). A driven pin reports the level it drives,
/// an input the level it samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FwPin {
    pub pin: &'static str,
    pub mode: PinUse,
    pub level: Option<bool>,
    pub duty: f32,
}

/// A Fault event the image reported in its watch stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FwFault {
    /// The cycle (since the device's reset) its frame began to go out.
    pub cycle: u64,
    /// `watch::FAULT_*`.
    pub code: u8,
    /// The port site (`FwDebug::sites`) or the link or timer the code names.
    pub site: u16,
    pub arg: i32,
}

/// Radio starts and timer failures observed in committed watch-stream order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RadioStarts {
    /// Number of start markers seen, including the first.
    pub markers: u64,
    /// The newest marker and the cycle of its report.
    pub latest: Option<(watch::RadioStart, u64)>,
    /// The newest stopping report since the last marker, or since attach for older images.
    pub degraded: Option<FwFault>,
}

impl RadioStarts {
    /// Observe one Fault event. Returns true for a start marker, which has its own tally.
    pub fn observe(&mut self, fault: &FwFault) -> bool {
        if fault.code == watch::FAULT_RADIO_STARTED {
            self.markers += 1;
            self.latest = Some((watch::radio_start(fault.arg), fault.cycle));
            self.degraded = None;
            return true;
        }
        if watch::FAULT_KERNEL_STOPPING.contains(&fault.code) || fault.code == watch::FAULT_RADIO_START_FAILED {
            self.degraded = Some(*fault);
        }
        false
    }
}

/// One kind of fault at one site, as often as the image reported it on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultCount {
    pub code: u8,
    pub site: u16,
    pub count: u64,
    /// Its latest report.
    pub last: FwFault,
}

/// One fault code's RAM counter in an unobserved (`off`) image, read at this publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultCell {
    /// `watch::FAULT_*`.
    pub code: u8,
    /// Firings since the chip's reset; saturates at 0xFFFF, meaning "at least 65535".
    pub count: u16,
    /// The latest fault's site, updated even when the count saturates; meaningful only when
    /// `count` is nonzero. Like `FwFault::site`, a port site or the link or timer the code names.
    pub site: u16,
}

/// A source condition's tally on this device: how often it was found true and false, and its
/// newest value (its branch's counts, read through `FwCond::taken_true`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FwCondCount {
    /// Its index in `FwDebug::conds`.
    pub cond: u32,
    pub trues: u64,
    pub falses: u64,
    pub last: bool,
}

/// The device's state at one publication (`CoSim::state`), chip-neutral: what the firmware panel
/// and `elec.fw.state` read. Statement ids index `debug.stmts`; counts are of committed execution.
#[derive(Clone, Debug)]
pub struct FwState {
    /// The image's table (None: the device was given none).
    pub debug: Option<Arc<FwDebug>>,
    pub image_id: u32,
    pub source_rev: u64,
    /// The analog instant of the publication.
    pub t: f64,
    /// Cycles since the device's reset: the first instruction boundary at or after `t`.
    pub cycle: u64,
    /// The simulated time of that reset.
    pub t_reset: f64,
    /// Cycles per second: `t = t_reset + cycle / f_clk` at an instruction boundary.
    pub f_clk: f64,
    pub at: At,
    /// The machine's own account of its halt (`Machine::halt_detail`), when halted and it has one.
    pub halt_detail: Option<String>,
    /// The handler running, from the committed trace.
    pub handler: Option<u32>,
    /// Active shared functions, outermost first, from committed trace marks.
    pub chain: Vec<u32>,
    /// The statement that began last (innermost where several begin at one address).
    pub last: Option<u32>,
    /// (statement, count): statements begun since the previous publication, by id.
    pub ran: Vec<(u32, u32)>,
    /// (statement, count): every statement begun on this device, by id, tallied the way
    /// `fault_counts` tallies faults: a reader that misses publications (the newest replaces an
    /// unread one) still sees each statement that began in them, by its count having grown. Its
    /// size is the number of distinct statements that have run.
    pub ran_counts: Vec<(u32, u64)>,
    /// Every source condition (`debug.conds`) the device has branched on, by index, tallied the
    /// way `ran_counts` tallies statements; empty where the machine counts no branches.
    pub cond_counts: Vec<FwCondCount>,
    /// The program's variables read from the chip's RAM, as `debug.vars` lays them out.
    pub vars: Vec<FwValue>,
    /// An unobserved (`off`) image's RAM fault counters, in `debug.fault_cells` order. Cells
    /// whose four bytes cannot be read are omitted, never reported as zero; match by `code`.
    /// Empty without a debug table or for images that report faults through the watch stream.
    pub fault_cells: Vec<FaultCell>,
    pub pins: Vec<FwPin>,
    pub timers: Vec<FwTimer>,
    pub pending: Vec<Pending>,
    pub wake: Option<Wake>,
    /// Faults the image reported since the previous publication.
    pub faults: Vec<FwFault>,
    /// Every fault the image reported on this device, one row per code and site in the order
    /// each first came: a reader that misses publications (the newest replaces an unread one)
    /// still shows each. Its size is the number of distinct codes and sites.
    pub fault_counts: Vec<FaultCount>,
    /// Radio start markers and the current start's degraded status. Markers are counted here
    /// only. Degrading reports also remain in the run's `fault_counts`.
    pub radio_starts: RadioStarts,
    /// PC from the latest radio record header when its cause was IllegalInstruction.
    pub radio_illegal_pc: Option<u32>,
    /// This publication is a step's stop.
    pub stopped: Option<StepTo>,
    pub pc: u32,
    pub regs: Vec<(&'static str, u32)>,
    /// The chip's non-volatile memory (its EEPROM) as committed at `t`, shared with the device:
    /// a new allocation only after the chip wrote it (None: the chip has none).
    pub eeprom: Option<Arc<[u8]>>,
    /// The host end of the image's network ports (None: the device has none, `with_net`).
    pub net: Option<NetStatus>,
    /// The run's upstream input log, shared across publications and replay.
    pub net_log: Option<SharedLog>,
}

impl FwState {
    /// Whether the device takes a statement or handler-entry step from this state (`CoSim::arm`):
    /// the image has its step map and the device has not halted.
    pub fn steps(&self) -> bool {
        !matches!(self.at, At::Halted(_)) && self.debug.as_ref().is_some_and(|d| d.map.is_some())
    }

    /// The handler a step out (`StepTo::HandlerExit`) returns from, from this state, by the rule
    /// `arm` applies ([`handler_out`]); None: the device refuses it.
    pub fn steps_out_of(&self) -> Option<u32> {
        if !self.steps() {
            return None;
        }
        handler_out(self.debug.as_ref()?.map.as_ref()?, self.pc, self.handler)
    }
}

/// The handler a step out returns from, standing at `pc` with `handler` running by the committed
/// trace: standing on a handler's entry it has been called; on a return it has returned.
pub fn handler_out(map: &StepMap, pc: u32, handler: Option<u32>) -> Option<u32> {
    if map.returns.binary_search(&pc).is_ok() {
        None
    } else if let Ok(k) = map.entries.binary_search_by_key(&pc, |e| e.0) {
        Some(map.entries[k].1)
    } else {
        handler
    }
}

/// Reports each pin of a state this adapter published: its name (`CoSimPin::name`), its use,
/// its level (None: unknown) and its PWM duty. Nothing for another device's state.
pub fn pin_rows(state: &CoState, f: &mut dyn FnMut(&str, PinUse, Option<bool>, f32)) {
    let Some(s) = state.get::<FwState>() else {
        return;
    };
    for p in &s.pins {
        f(p.pin, p.mode, p.level, p.duty);
    }
}

/// Counters for measuring the device's share of the worker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub runs: u64,
    pub writes: u64,
    pub hits: u64,
    pub reruns: u64,
    /// Re-runs a sync made (a publication's read, an arm, an input), of `reruns`.
    pub sync_reruns: u64,
    /// The private run the re-runs threw away: ticks from the mark to where the machine stood.
    pub rerun_ticks: u64,
    pub commits: u64,
}

/// The next effect after the queued drive changes, at its tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Stop(u64),
    Barrier(u64),
    /// At this tick of the old epoch; the machine already stands in the new one.
    Reset(u64),
    Halt(u64),
    Commit(u64),
}

impl Effect {
    fn tick(self) -> u64 {
        match self {
            Effect::Stop(t) | Effect::Barrier(t) | Effect::Reset(t) | Effect::Halt(t) | Effect::Commit(t) => t,
        }
    }
}

#[derive(Clone)]
struct Armed {
    to: StepTo,
    after: u64,
    stops: Vec<u32>,
}

/// A machine co-simulated with the circuit. A clone is a fork (`CoSim::fork`): the machine and
/// everything the co-simulation decides with, without the event sink.
#[derive(Clone)]
pub struct CoMachine<M: Machine> {
    m: M,
    names: Arc<[&'static str]>,
    hz: u64,
    cfg: Config,
    marks: PcSet,
    attached: bool,
    halted: bool,
    t_reset: f64,
    /// The tick of the mark.
    start: u64,
    /// The tick the stretch deadline counts from: the last commit at a deadline, a stop, a barrier,
    /// a halt, a reset or the attach (a sync's and `maybe_commit`'s keep it).
    paced: u64,
    /// Drive changes not yet applied, in tick order; the first `held` were taken before the mark.
    queue: VecDeque<DriveEdge>,
    held: usize,
    /// Untaken analog requests; the first `samples_held` were taken before the mark.
    samples: VecDeque<SampleAt>,
    samples_held: usize,
    /// Samples answered since the mark, replayed before running a rewound stretch.
    sample_log: Vec<(SampleAt, f64, f64)>,
    /// Changes taken after the mark and applied: a re-run skips that many.
    applied_post: usize,
    pending: Option<Effect>,
    /// Inputs raised since the mark: (tick, pin, high).
    log: Vec<(u64, u16, bool)>,
    /// Per pin: the drive the bus has, the level the machine samples, and whether its pad is in
    /// the circuit.
    applied: Vec<Option<Drive>>,
    level: Vec<Option<bool>>,
    wired: Vec<bool>,
    /// Per machine pin: its pin table row and (vih, vil) as fractions of its supply.
    rows: Vec<Option<(u32, f64, f64)>>,
    watches: Vec<Watch>,
    gen: u32,
    rewatch: bool,
    armed: Option<Armed>,
    stopped: Option<StepTo>,
    handler: Option<u32>,
    chain: Vec<u32>,
    last: Option<u32>,
    ran: Vec<(u32, u32)>,
    ran_counts: Vec<(u32, u64)>,
    /// Scratch for the machine's block and branch counts, drained in `observe`.
    counts: ExecCounts,
    /// The machine's code map the conditions were matched on, and per map condition (its id less
    /// one) the source condition its branch tests (`FwDebug::conds` index) and whether taken
    /// means true; matched again only when the machine's map changes.
    cond_map: Option<Arc<CodeMap>>,
    cond_of: Vec<Option<(u32, bool)>>,
    /// Every source condition branched on, by index (`FwState::cond_counts`).
    cond_counts: Vec<FwCondCount>,
    /// Scratch for the machine's link output and notes, drained in `observe`.
    out: Vec<LinkOut>,
    notes: Vec<Note>,
    faults: Vec<FwFault>,
    fault_counts: Vec<FaultCount>,
    radio_starts: RadioStarts,
    radio_illegal_pc: Option<u32>,
    stats: Stats,
    /// Asked to stand still at its next commit with the analog (`CoSim::settle`).
    settle: bool,
    /// Standing on its mark at the analog's present, not ahead: a read costs no re-run. Its next
    /// `run` goes on.
    settled: bool,
    edges: Vec<DriveEdge>,
    sample_requests: Vec<SampleAt>,
    trace: Vec<(u64, u32)>,
    changes: Vec<(u16, bool)>,
    sink: Events,
    statements: bool,
    /// Events not yet taken by the sink, in order.
    spill: Vec<(f64, FwFrame)>,
    /// The events of this device's stream before the spill (taken by the sink, or dropped with
    /// it), its copies' included: the spill's first event is the stream's `streamed`-th.
    streamed: u64,
    /// The newest events the sink took that are void (the run went back; `put_outputs`), said with
    /// the next offer.
    void: u64,
    /// Link 0 decoded as the image's watch stream.
    watch: watch::Reader,
    /// The analog instant of the last run, where a device with a spill waits.
    last_t: f64,
    backpressure: bool,
    /// The machine's board nets by name (`CoSim::logic_nets`).
    nets: Vec<String>,
    /// The analog time of the attach: board time's zero.
    t_attach: f64,
    /// Scratch for the board's records, drained in `observe`.
    bus: Vec<BusEvent>,
    /// The board nets' committed level changes not yet taken (`take_logic`), in time order.
    levels: VecDeque<(u32, LogicEdge)>,
    /// Per board net, the level of its last change taken (`restart_logic`).
    taken: Vec<Option<bool>>,
    /// Scratch for the board's sound, drained in `observe`, and the committed sound not yet
    /// taken (`take_audio`), in run time.
    board_audio: Audio,
    audio: CoAudio,
    /// The host end of the image's network ports, and the run's network if this copy holds it.
    net: Option<net::NetEnd>,
    upstream: net::Owned,
}

fn input_like(d: Drive) -> bool {
    matches!(d, Drive::Input(_) | Drive::OpenDrain { .. })
}

fn pull(p: Pull) -> PinPull {
    match p {
        Pull::None => PinPull::None,
        Pull::Up => PinPull::Up,
        Pull::Down => PinPull::Down,
    }
}

fn mode(d: Drive) -> PinMode {
    match d {
        Drive::Low => PinMode::OutLow,
        Drive::High => PinMode::OutHigh,
        Drive::Input(p) => PinMode::Input(pull(p)),
        Drive::OpenDrain { low, pull: p } => PinMode::OpenDrain { low, pull: pull(p) },
    }
}

/// The machine as a live bench's device with its pin table (`Lowered::cosim_pins`).
pub fn device<M: Machine>(m: M, cfg: Config, pins: Vec<CoSimPin>) -> Result<CoSimDevice, String> {
    Ok(CoSimDevice { dev: Box::new(CoMachine::new(m, cfg, &pins)?), pins })
}

impl<M: Machine> CoMachine<M> {
    /// A device for a machine fresh from its reset (tick 0), which the attach puts at the
    /// analog's instant. A machine pin is coupled through the table row of its name, with that
    /// row's input thresholds; a pin without a row is not in the circuit.
    pub fn new(mut m: M, cfg: Config, pins: &[CoSimPin]) -> Result<CoMachine<M>, String> {
        if m.now() != 0 {
            return Err("the machine must start at its reset".into());
        }
        let mut rows = Vec::with_capacity(m.pins().len());
        for name in m.pins() {
            let row = pins.iter().position(|p| p.name == *name);
            if let Some(p) = row.map(|r| &pins[r]) {
                if !(0.0 < p.vil && p.vil < p.vih && p.vih < 1.0) {
                    return Err(format!("pin {name}: input thresholds need 0 < vil < vih < 1, got vil {} vih {}", p.vil, p.vih));
                }
            }
            rows.push(row.map(|r| (r as u32, pins[r].vih, pins[r].vil)));
        }
        if cfg.stretch == 0 {
            return Err("a stretch needs at least one tick".into());
        }
        if let Some(map) = cfg.map() {
            map.check()?;
            // the step map's addresses are where the image is entered other than by direct flow
            // (handlers through dispatch tables): roots of the machine's block map, so each
            // statement starts a block
            let mut roots: Vec<u32> = map.statements.iter().copied()
                .chain(map.entries.iter().map(|e| e.0)).chain(map.returns.iter().copied())
                .chain(map.funcs.iter().map(|f| f.0)).chain(map.calls.iter().map(|c| c.0)).collect();
            roots.sort_unstable();
            roots.dedup();
            m.set_code_roots(&roots);
        }
        let marks = cfg.map().map(StepMap::marks).unwrap_or_default();
        let n = m.pins().len();
        let names = m.pins().iter().copied().collect();
        let nets: Vec<String> = m.nets().iter().map(|net| net.name.to_string()).collect();
        let taken = vec![None; nets.len()];
        Ok(CoMachine {
            names,
            hz: m.tick_hz(),
            m,
            cfg,
            marks,
            attached: false,
            halted: false,
            t_reset: 0.0,
            start: 0,
            paced: 0,
            queue: VecDeque::new(),
            held: 0,
            samples: VecDeque::new(),
            samples_held: 0,
            sample_log: Vec::new(),
            applied_post: 0,
            pending: None,
            log: Vec::new(),
            applied: vec![None; n],
            level: vec![None; n],
            wired: vec![false; n],
            rows,
            watches: Vec::new(),
            gen: 0,
            rewatch: false,
            armed: None,
            stopped: None,
            handler: None,
            chain: Vec::new(),
            last: None,
            ran: Vec::new(),
            ran_counts: Vec::new(),
            counts: ExecCounts::default(),
            cond_map: None,
            cond_of: Vec::new(),
            cond_counts: Vec::new(),
            out: Vec::new(),
            notes: Vec::new(),
            faults: Vec::new(),
            fault_counts: Vec::new(),
            radio_starts: RadioStarts::default(),
            radio_illegal_pc: None,
            stats: Stats::default(),
            settle: false,
            settled: false,
            edges: Vec::new(),
            sample_requests: Vec::new(),
            trace: Vec::new(),
            changes: Vec::new(),
            sink: Events(None),
            statements: false,
            spill: Vec::new(),
            streamed: 0,
            void: 0,
            watch: watch::Reader::new(),
            last_t: 0.0,
            backpressure: false,
            nets,
            t_attach: 0.0,
            bus: Vec::new(),
            levels: VecDeque::new(),
            taken,
            board_audio: Audio::default(),
            audio: CoAudio::default(),
            net: None,
            upstream: net::Owned(None),
        })
    }

    /// The device with a host end for its image's network ports (link 0's NET frames), reaching
    /// `upstream` when given: this device is then its run's owner, the one copy that holds the
    /// network. Only before it runs.
    pub fn with_net(mut self, setup: NetSetup, upstream: Option<Upstream>) -> Self {
        assert!(!self.attached, "a device takes its network before it runs");
        self.net = Some(net::NetEnd::new(setup));
        self.upstream = net::Owned(upstream);
        self
    }

    /// Where the device's events go (one batch per commit), and whether they include every
    /// statement begun. Without a sink nothing is collected, nothing waits to be taken and
    /// nothing is held back.
    pub fn set_events(&mut self, sink: Option<EventSink>, statements: bool) {
        self.sink = Events(sink);
        self.statements = statements;
        self.void = 0;
        if self.sink.0.is_none() {
            self.streamed += self.spill.len() as u64;
            self.spill.clear();
            self.backpressure = false;
        }
    }

    /// Whether the sink refused the last batch: the device waits until it takes it.
    pub fn backpressure(&self) -> bool {
        self.backpressure
    }

    pub fn machine(&self) -> &M {
        &self.m
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    fn time(&self, tick: u64) -> f64 {
        let t = time(self.t_reset, tick, self.hz);
        assert!(t < TIME_EXACT_LIMIT, "simulated time {t} s is past the exact tick domain");
        t
    }

    fn tick(&self, t: f64) -> u64 {
        tick_at(self.t_reset, t.max(self.t_reset), self.hz)
    }

    fn is_input(&self, pin: usize) -> bool {
        !self.m.analog_only(pin) && self.applied[pin].is_some_and(input_like)
    }

    /// The level a pin's pad reads as now, keeping `was` between the thresholds (None: the pad or
    /// its rails are not in the circuit).
    fn sample(&self, bus: &dyn Bus, pin: usize, was: Option<bool>) -> Option<bool> {
        let (row, vih, vil) = self.rows[pin]?;
        let (v, supply) = (bus.v(row), bus.supply(row));
        if !v.is_finite() || !supply.is_finite() {
            return None;
        }
        let (hi, lo) = (vih * supply, vil * supply);
        Some(if v >= hi {
            true
        } else if v <= lo {
            false
        } else {
            was.unwrap_or(v >= 0.5 * (hi + lo))
        })
    }

    /// The machine stands later than the first boundary at or after tick `c`, or in a newer epoch.
    /// On its mark it never does: a commit is taken where the machine stands on the first boundary
    /// at or after the analog's instant, and `c` is no earlier than that instant.
    fn ahead_of(&self, c: u64) -> bool {
        matches!(self.pending, Some(Effect::Reset(_))) || self.m.now() > c && self.m.now() != self.start
    }

    /// Takes the machine's drive changes into the queue, skipping the first `skip`.
    fn take_edges(&mut self, skip: usize) {
        self.m.take_drives(&mut self.edges);
        self.queue.extend(self.edges.drain(..).skip(skip));
    }

    fn take_samples(&mut self) {
        self.m.take_samples(&mut self.sample_requests);
        for at in self.sample_requests.drain(..) {
            // A replay uses the original voltage, even if the pad has since changed.
            if !self.sample_log.iter().any(|(given, _, _)| *given == at) {
                self.samples.push_back(at);
            }
        }
    }

    /// Runs without stops to the first boundary at or after `tick`, or to an earlier terminal
    /// effect (recorded as pending).
    fn run_plain(&mut self, tick: u64) {
        while self.m.now() < tick {
            let until = Until { tick, marks: &self.marks, stops: &[], stops_after: 0, on_drive: false };
            match self.m.run(&until) {
                Ran::Tick => return,
                Ran::Halted => {
                    self.pending = Some(Effect::Halt(self.halt_tick()));
                    return;
                }
                Ran::Reset { at } => {
                    self.pending = Some(Effect::Reset(at));
                    return;
                }
                Ran::Barrier => {
                    self.pending = Some(Effect::Barrier(self.m.now()));
                    return;
                }
                Ran::Pc | Ran::Drive { .. } => {}
            }
        }
    }

    fn halt_tick(&self) -> u64 {
        match self.m.status() {
            Status::Halted(h) => h.tick,
            _ => self.m.now(),
        }
    }

    /// Back to the mark, the stretch's inputs replayed, then to the first boundary at or after
    /// tick `c`; drive changes the analog already took are skipped.
    fn rerun_to(&mut self, c: u64) {
        self.stats.reruns += 1;
        self.stats.rerun_ticks += self.m.now().saturating_sub(self.start);
        self.m.rewind();
        self.queue.truncate(self.held);
        self.samples.truncate(self.samples_held);
        for &(at, volts, supply) in &self.sample_log { self.m.set_sample(at, volts, supply); }
        self.pending = None;
        let log = std::mem::take(&mut self.log);
        for &(at, pin, high) in &log {
            self.run_plain(at);
            assert_eq!(self.m.now(), at, "a replayed input lands on its tick");
            self.m.set_input(pin as usize, high);
        }
        self.log = log;
        self.run_plain(c);
        self.take_edges(self.applied_post);
        self.take_samples();
    }

    /// Raises input levels at the first boundary at or after tick `c`.
    fn raise(&mut self, c: u64, changes: &[(u16, bool)]) {
        if self.ahead_of(c) {
            self.rerun_to(c);
        } else {
            self.run_plain(c);
        }
        let at = self.m.now();
        for &(pin, high) in changes {
            self.m.set_input(pin as usize, high);
            self.log.push((at, pin, high));
        }
    }

    /// The stretch's new start, where the machine stands; observations become committed. The next
    /// deadline counts from here.
    fn commit(&mut self) {
        self.commit_at(true);
        self.paced = self.start;
    }

    /// A commit that leaves the next deadline where it was (a sync's, `maybe_commit`'s).
    fn commit_unpaced(&mut self) {
        self.commit_at(false);
    }

    /// A paced commit is also where the network's host end speaks: its place never depends on a
    /// publication's.
    fn commit_at(&mut self, paced: bool) {
        self.stats.commits += 1;
        self.observe();
        if paced {
            self.net_step();
        }
        self.take_edges(0);
        self.take_samples();
        self.m.mark();
        self.start = self.m.now();
        self.held = self.queue.len();
        self.samples_held = self.samples.len();
        self.applied_post = 0;
        self.log.clear();
        self.sample_log.clear();
    }

    /// The host end takes what the device sent, and its due messages go down link 0 now, before
    /// the mark: a re-run from it has them.
    fn net_step(&mut self) {
        let Some(end) = self.net.as_mut() else { return };
        let (hz, now) = (self.hz, self.m.now());
        let m = &mut self.m;
        end.step(hz, now, self.upstream.0.as_mut(), &mut |packet| {
            for &byte in packet {
                m.link_in(0, byte);
            }
        });
    }

    /// Takes the machine's committed trace, branch counts, link output and notes: the handler
    /// running, the statements begun, the conditions' values, and the events, timed in the
    /// current epoch.
    fn observe(&mut self) {
        self.m.take_trace(&mut self.trace);
        self.m.take_counts(&mut self.counts);
        self.tally_conds();
        let sink = self.sink.0.is_some();
        let (t_reset, hz) = (self.t_reset, self.hz);
        if let Some(map) = self.cfg.map() {
            for &(tick, pc) in &self.trace {
                if let Ok(k) = map.entries.binary_search_by_key(&pc, |e| e.0) {
                    self.handler = Some(map.entries[k].1);
                    self.chain.clear();
                    if sink {
                        self.spill.push((time(t_reset, tick, hz), FwFrame::Enter { handler: map.entries[k].1 }));
                    }
                }
                if map.returns.binary_search(&pc).is_ok() {
                    if let (true, Some(handler)) = (sink, self.handler) {
                        self.spill.push((time(t_reset, tick, hz), FwFrame::Exit { handler }));
                    }
                    self.handler = None;
                    self.chain.clear();
                }
                if let Ok(k) = map.calls.binary_search_by_key(&pc, |c| c.0) {
                    if self.chain.last() == Some(&map.calls[k].1) { self.chain.pop(); }
                }
                if let Ok(k) = map.funcs.binary_search_by_key(&pc, |f| f.0) {
                    self.chain.push(map.funcs[k].2);
                }
                if map.statements.binary_search(&pc).is_ok() {
                    self.last = Some(pc);
                    match self.ran.binary_search_by_key(&pc, |r| r.0) {
                        Ok(k) => self.ran[k].1 += 1,
                        Err(k) => self.ran.insert(k, (pc, 1)),
                    }
                    if sink && self.statements {
                        self.spill.push((time(t_reset, tick, hz), FwFrame::Statement { pc }));
                    }
                }
            }
        }
        self.trace.clear();
        // link 0 is the image's watch stream: its faults reach the state, and with a sink every
        // frame goes out as an event
        self.m.take_out(&mut self.out);
        self.m.take_notes(&mut self.notes);
        // the board nets' levels, in board time from the attach; each commit's in time order
        self.m.take_bus(&mut self.bus);
        let first = self.levels.len();
        for event in self.bus.drain(..) {
            if let BusEvent::Level { t: tick, net, high } = event {
                self.levels.push_back((u32::from(net), LogicEdge { t: time(self.t_attach, tick, hz), high }));
            }
        }
        self.levels.make_contiguous()[first..].sort_by(|a, b| a.1.t.total_cmp(&b.1.t));
        // the board's sound, in board time from the attach, as the levels
        self.m.take_audio(&mut self.board_audio);
        for (run, samples) in self.board_audio.each() {
            self.audio.runs.push(CoAudioRun { t0: time(self.t_attach, run.t0, hz), rate: run.rate, len: run.len });
            self.audio.samples.extend_from_slice(samples);
        }
        self.board_audio.clear();
        for o in self.out.drain(..) {
            if o.link != 0 {
                continue;
            }
            let frame = match self.watch.push(o.byte) {
                Some(Ok(frame)) => {
                    if let Some(end) = self.net.as_mut() {
                        end.take(hz, o.tick, &frame, self.watch.net_body());
                    }
                    if let watch::Event::RadioRecord { index: 0, words: [_, cause, pc, _] } = frame.event {
                        self.radio_illegal_pc = (cause == 0).then_some(pc);
                    }
                    if let watch::Event::Fault { code, site, arg } = frame.event {
                        let fault = FwFault { cycle: o.tick, code, site, arg };
                        if !self.radio_starts.observe(&fault) {
                            self.faults.push(fault);
                            match self.fault_counts.iter_mut().find(|c| c.code == code && c.site == site) {
                                Some(c) => {
                                    c.count += 1;
                                    c.last = fault;
                                }
                                None => self.fault_counts.push(FaultCount { code, site, count: 1, last: fault }),
                            }
                        }
                    }
                    FwFrame::Image(frame)
                }
                Some(Err(bad)) => FwFrame::ImageBad(bad),
                None => continue,
            };
            if sink {
                self.spill.push((time(t_reset, o.tick, hz), frame));
            }
        }
        for n in self.notes.drain(..) {
            if sink {
                self.spill.push((time(t_reset, n.tick, hz), FwFrame::Note(n.what)));
            }
        }
        if sink {
            self.flush();
        }
    }

    /// Folds the branch counts taken into the source conditions' tallies: each branch counted is
    /// the condition it tests, its taken and fall-through counts that condition's trues and falses
    /// by the branch's sense, its last outcome the condition's newest value. O(branches counted),
    /// plus O(the map's conditions) when the machine's map is new.
    fn tally_conds(&mut self) {
        self.counts.blocks.clear();
        if self.counts.conds.is_empty() {
            return;
        }
        if let Some(map) = self.m.code_map().filter(|map| !self.cond_map.as_ref().is_some_and(|known| Arc::ptr_eq(known, map))) {
            let debug = self.cfg.debug.as_deref();
            self.cond_of = map.conds.iter().map(|c| {
                let d = debug?;
                let k = d.cond_at(c.pc)?;
                Some((k, d.conds[k as usize].taken_true))
            }).collect();
            self.cond_map = Some(map);
        }
        for &(id, taken, not_taken, last_taken) in &self.counts.conds {
            let Some(&Some((cond, taken_true))) = id.checked_sub(1).and_then(|i| self.cond_of.get(i as usize)) else { continue };
            let (trues, falses) = if taken_true { (taken, not_taken) } else { (not_taken, taken) };
            let k = match self.cond_counts.binary_search_by_key(&cond, |c| c.cond) {
                Ok(k) => k,
                Err(k) => {
                    self.cond_counts.insert(k, FwCondCount { cond, trues: 0, falses: 0, last: false });
                    k
                }
            };
            let tally = &mut self.cond_counts[k];
            tally.trues += trues;
            tally.falses += falses;
            tally.last = last_taken == taken_true;
        }
        self.counts.conds.clear();
    }

    /// Offers the spill to the sink, with the void events before it. Returns whether both are
    /// gone.
    fn flush(&mut self) -> bool {
        let Some(sink) = self.sink.0.as_mut() else {
            return true;
        };
        if self.spill.is_empty() && self.void == 0 {
            return true;
        }
        let n = self.spill.len() as u64;
        if sink(self.void, &mut self.spill) {
            self.spill.clear();
            self.streamed += n;
            self.void = 0;
            self.backpressure = false;
            true
        } else {
            if !self.backpressure {
                eprintln!("elec cosim: the event sink is full; the device waits until it takes {} events", self.spill.len());
            }
            self.backpressure = true;
            false
        }
    }

    /// Commits when the machine stands at the analog's cycle and the stretch is worth closing,
    /// keeping the pace: whether it closes depends on where a publication's commit left the mark.
    fn maybe_commit(&mut self, t: f64) {
        if self.halted || matches!(self.pending, Some(Effect::Reset(_))) || self.m.now() != self.tick(t) {
            return;
        }
        if !self.log.is_empty() || !self.sample_log.is_empty() || self.m.now() - self.start >= self.cfg.stretch / 2 {
            self.commit_unpaced();
        }
    }

    /// Runs the machine privately until the next effect is known.
    fn ahead(&mut self) {
        while !self.halted && self.queue.is_empty() && self.samples.is_empty() && self.pending.is_none() {
            let deadline = self.paced.saturating_add(self.cfg.stretch);
            let (stops, after) = match &self.armed {
                Some(a) => (&a.stops[..], a.after),
                None => (&[][..], 0),
            };
            let until = Until { tick: deadline, marks: &self.marks, stops, stops_after: after, on_drive: true };
            let ran = self.m.run(&until);
            self.take_edges(0);
            self.take_samples();
            let now = self.m.now();
            self.pending = match ran {
                // a change of drive generation without a change of drive queues nothing
                Ran::Drive { .. } => None,
                Ran::Pc => Some(Effect::Stop(now)),
                Ran::Barrier => Some(Effect::Barrier(now)),
                Ran::Reset { at } => Some(Effect::Reset(at)),
                Ran::Halted => Some(Effect::Halt(self.halt_tick())),
                Ran::Tick => Some(Effect::Commit(now)),
            };
        }
    }

    /// Writes a pin's drive; a pin that turns into an input is sampled from its pad.
    fn apply(&mut self, bus: &mut dyn Bus, pin: usize, drive: Drive, changes: &mut Vec<(u16, bool)>) {
        let was = self.applied[pin];
        if was == Some(drive) {
            return;
        }
        self.applied[pin] = Some(drive);
        self.stats.writes += 1;
        if let Some((row, _, _)) = self.rows[pin] {
            bus.write(row, mode(drive));
        }
        let input = !self.m.analog_only(pin) && input_like(drive);
        if input != was.is_some_and(input_like) || was.is_none() {
            self.rewatch = true;
        }
        if input && !was.is_some_and(input_like) && self.wired[pin] {
            if let Some(level) = self.sample(bus, pin, self.level[pin]) {
                if self.level[pin] != Some(level) {
                    self.level[pin] = Some(level);
                    changes.push((pin as u16, level));
                }
            }
        }
    }

    fn attach(&mut self, bus: &mut dyn Bus) {
        self.attached = true;
        self.t_reset = bus.t();
        self.t_attach = self.t_reset;
        assert!(self.t_reset >= 0.0 && self.t_reset < TIME_EXACT_LIMIT, "an attach inside the exact tick domain");
        for pin in 0..self.applied.len() {
            self.wired[pin] = self.rows[pin].is_some_and(|(row, _, _)| bus.v(row).is_finite() && bus.supply(row).is_finite());
        }
        // the machine's drives as they stand are the start state, written now
        self.m.take_drives(&mut self.edges);
        self.edges.clear();
        let mut changes = std::mem::take(&mut self.changes);
        for pin in 0..self.applied.len() {
            let d = self.m.drive(pin);
            self.apply(bus, pin, d, &mut changes);
        }
        for &(pin, high) in &changes {
            self.m.set_input(pin as usize, high);
        }
        changes.clear();
        self.changes = changes;
        self.commit();
        self.ahead();
    }

    /// A chip reset reaches the analog: the new epoch starts here, every pin in its reset drive.
    fn reset_epoch(&mut self, at: u64, bus: &mut dyn Bus) {
        // what ran before the reset is timed in its own epoch
        let t_reset = self.time(at);
        self.observe();
        if let Some(end) = self.net.as_mut() {
            end.reset(at);
        }
        self.t_reset = t_reset;
        self.queue.clear();
        self.held = 0;
        self.samples.clear();
        self.samples_held = 0;
        self.sample_log.clear();
        self.applied_post = 0;
        self.log.clear();
        self.handler = None;
        self.chain.clear();
        if let Some(armed) = &mut self.armed {
            // every tick of the new epoch is after the arming
            armed.after = 0;
        }
        let mut changes = std::mem::take(&mut self.changes);
        for pin in 0..self.applied.len() {
            let d = self.m.drive(pin);
            self.apply(bus, pin, d, &mut changes);
        }
        // the chip samples its inputs afresh
        for pin in 0..self.applied.len() {
            if let (true, Some(high)) = (self.is_input(pin), self.level[pin]) {
                self.m.set_input(pin, high);
            }
        }
        changes.clear();
        self.changes = changes;
        self.commit();
    }

    fn hits(&mut self, bus: &mut dyn Bus) {
        let t = bus.t();
        let mut changes = std::mem::take(&mut self.changes);
        let mut latest = f64::NEG_INFINITY;
        for &(id, h) in bus.hits() {
            self.stats.hits += 1;
            let (pin, high) = ((id / 2) as usize, id % 2 == 0);
            if pin >= self.level.len() || !self.is_input(pin) || self.level[pin] == Some(high) {
                continue;
            }
            self.level[pin] = Some(high);
            changes.push((pin as u16, high));
            latest = latest.max(h.max(t));
        }
        if !changes.is_empty() {
            let c = self.tick(latest);
            self.raise(c, &changes);
        }
        changes.clear();
        self.changes = changes;
    }

    fn rebuild_watches(&mut self, bus: &dyn Bus) {
        if !self.rewatch {
            return;
        }
        self.rewatch = false;
        self.watches.clear();
        for pin in 0..self.applied.len() {
            if let (true, true, Some((row, vih, vil))) = (self.wired[pin], self.is_input(pin), self.rows[pin]) {
                let supply = bus.supply(row);
                let id = 2 * pin as u32;
                self.watches.push(Watch { id, pin: row, level: vih * supply, edge: PinEdge::Rising });
                self.watches.push(Watch { id: id + 1, pin: row, level: vil * supply, edge: PinEdge::Falling });
            }
        }
        self.gen = self.gen.wrapping_add(1);
    }

    /// Brings the machine to the first boundary at or after `t`'s tick and commits there, keeping
    /// the pace, settled: the next `run` goes on ahead. `t` is the analog's present from now on.
    fn sync(&mut self, t: f64) {
        self.last_t = t;
        if self.halted {
            return;
        }
        let c = self.tick(t);
        if self.ahead_of(c) {
            self.stats.sync_reruns += 1;
            self.rerun_to(c);
        } else {
            self.run_plain(c);
        }
        if matches!(self.pending, Some(Effect::Stop(_))) {
            // found under the previous arming
            self.pending = None;
        }
        if !matches!(self.pending, Some(Effect::Reset(_))) {
            self.commit_unpaced();
            // a commit with the analog, standing on the mark: what a settle asks for
            self.settle = false;
            self.settled = true;
        }
    }
}

impl<M: Machine> CoSim for CoMachine<M> {
    fn input(&mut self, t: f64, input: &dyn Any) -> bool {
        if !self.attached { return false; }
        self.sync(t);
        let taken = self.m.host_input(input);
        if taken { self.commit_unpaced(); }
        taken
    }

    /// The private run is the only one past the analog, and it stops at `paced + stretch`, where
    /// `paced` is an instant the analog stood on: a call with the analog at `t` reads no later than
    /// `t + stretch` ticks. The S3 machine stamps an access at its instruction's start, before
    /// the deadline, and a peripheral event at or before it, so the clock's overshoot of the
    /// deadline (one instruction) reads nothing. The tick rate is the machine's, fixed for its life.
    fn input_span(&self) -> Option<f64> {
        Some(self.cfg.stretch as f64 / self.hz as f64)
    }
    fn settle(&mut self, want: bool) {
        self.settle = want;
    }
    fn settled(&self) -> bool {
        self.settled || self.halted || !self.attached
    }
    fn next(&self) -> f64 {
        if !self.attached {
            return 0.0;
        }
        // a settled device runs at once: its run goes on ahead
        if !self.spill.is_empty() || self.void > 0 || self.settled {
            return self.last_t;
        }
        if self.halted {
            return f64::INFINITY;
        }
        self.queue.front().map(|e| e.tick).into_iter()
            .chain(self.samples.front().map(|s| s.tick))
            .chain(self.pending.map(|p| p.tick())).min()
            .map_or(f64::INFINITY, |tick| self.time(tick))
    }
    fn fork(&self) -> Option<Box<dyn CoSim>> {
        // a fork takes no sink (the events not yet taken stay the original's), and its spill must
        // go with it: while a spill waits, `next()` sits at `last_t` and a fork would hang there.
        // It keeps its place in the stream (`streamed`), for a takeover (`put_outputs`)
        let mut fork = self.clone();
        fork.set_events(None, fork.statements);
        Some(Box::new(fork))
    }

    fn nonvolatile(&mut self, t: f64) -> Option<Arc<[u8]>> {
        if self.attached {
            self.sync(t);
        }
        self.m.eeprom_bytes()
    }

    fn set_nonvolatile(&mut self, bytes: &[u8]) -> bool {
        // only before it runs: a device that has run keeps its own history
        !self.attached && self.m.load_eeprom(bytes)
    }

    fn take_outputs(&mut self) -> Option<Box<dyn Any + Send>> {
        let stream = self.sink.0.take().map(|sink| {
            let stream = Stream { sink, streamed: self.streamed, void: self.void, spill: std::mem::take(&mut self.spill) };
            self.set_events(None, self.statements);
            stream
        });
        let feeds = self.m.take_feeds();
        let upstream = self.upstream.0.take();
        if stream.is_none() && feeds.is_none() && upstream.is_none() { None }
        else { Some(Box::new(Handoff { stream, feeds, upstream })) }
    }

    fn put_outputs(&mut self, outputs: Box<dyn Any + Send>) {
        let Ok(handoff) = outputs.downcast::<Handoff>() else { return };
        let Handoff { stream, feeds, upstream } = *handoff;
        if let Some(feeds) = feeds { self.m.put_feeds(feeds); }
        // the network's owner from here: it replays the log until it runs differently
        if upstream.is_some() { self.upstream = net::Owned(upstream); }
        let Some(Stream { sink, streamed, void, mut spill }) = stream else { return };
        // this copy stands at the stream's `at`-th event (it has no sink, so no spill): the
        // other's events from there on, sent or not, are what this one sends again
        let at = self.streamed + self.spill.len() as u64;
        let kept = streamed.min(at);
        spill.truncate((at - kept) as usize);
        spill.append(&mut self.spill);
        self.spill = spill;
        self.void = streamed + void - kept;
        self.streamed = kept;
        self.sink = Events(Some(sink));
        self.backpressure = false;
    }

    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        self.stats.runs += 1;
        self.settled = false;
        if !self.attached {
            self.attach(bus);
            self.rebuild_watches(bus);
            return Ok(CoRun::Go);
        }
        let t = bus.t();
        self.last_t = t;
        if !self.flush() {
            // no data is dropped: the device waits here until the sink takes its events
            return Ok(CoRun::Go);
        }
        if self.halted {
            return Ok(CoRun::Go);
        }
        self.hits(bus);
        let mut result = CoRun::Go;
        loop {
            let mut changes = std::mem::take(&mut self.changes);
            while let Some(&e) = self.queue.front() {
                if self.time(e.tick) > t {
                    break;
                }
                self.queue.pop_front();
                if self.held > 0 {
                    self.held -= 1;
                } else {
                    self.applied_post += 1;
                }
                self.apply(bus, e.pin as usize, e.drive, &mut changes);
            }
            if !changes.is_empty() {
                let c = self.tick(t);
                self.raise(c, &changes);
                changes.clear();
            }
            self.changes = changes;
            if self.queue.front().is_some_and(|e| self.time(e.tick) <= t) {
                continue;
            }
            // Apply all same-tick drives first. The solver lands exactly on next(), so these
            // voltages belong to the sample-and-hold tick, not the instruction boundary.
            while let Some(&at) = self.samples.front() {
                if self.time(at.tick) > t { break; }
                self.samples.pop_front();
                self.samples_held = self.samples_held.saturating_sub(1);
                let (volts, supply) = self.rows[usize::from(at.pin)]
                    .map_or((f64::NAN, f64::NAN), |(row, _, _)| (bus.v(row), bus.supply(row)));
                self.m.set_sample(at, volts, supply);
                self.sample_log.push((at, volts, supply));
            }
            match self.pending {
                Some(p) if self.time(p.tick()) <= t => {
                    self.pending = None;
                    match p {
                        Effect::Stop(_) => {
                            let armed = self.armed.take().expect("a stop is found only while armed");
                            self.stopped = Some(armed.to);
                            result = CoRun::Stopped;
                            self.commit();
                            // the run pauses here: its publication reads the stop without a re-run
                            self.settled = true;
                            break;
                        }
                        Effect::Barrier(_) | Effect::Commit(_) => self.commit(),
                        Effect::Reset(at) => self.reset_epoch(at, bus),
                        Effect::Halt(_) => {
                            self.halted = true;
                            self.commit();
                            break;
                        }
                    }
                }
                Some(_) => break,
                None => {
                    if !self.queue.is_empty() || !self.samples.is_empty() { break; }
                    self.maybe_commit(t);
                    // asked to settle and standing on the mark with the analog: no further for now
                    if self.settle && self.start == self.m.now() {
                        self.settle = false;
                        self.settled = true;
                        break;
                    }
                    self.ahead();
                }
            }
        }
        self.rebuild_watches(bus);
        Ok(result)
    }

    fn watches(&self) -> &[Watch] {
        &self.watches
    }

    fn watch_gen(&self) -> u32 {
        self.gen
    }

    fn arm(&mut self, t: f64, to: Option<StepTo>) -> bool {
        if !self.attached {
            return false;
        }
        let Some(to) = to else {
            self.armed = None;
            if matches!(self.pending, Some(Effect::Stop(_))) {
                self.pending = None;
                if !self.settled { self.ahead(); }
            }
            return true;
        };
        if self.halted || self.cfg.map().is_none() {
            return false;
        }
        self.sync(t);
        let map = self.cfg.map().expect("checked above");
        let stops: Vec<u32> = match to {
            StepTo::Statement => map.statements.clone(),
            StepTo::HandlerEntry => map.entries.iter().map(|e| e.0).collect(),
            StepTo::HandlerExit => match handler_out(map, self.m.pc(), self.handler) {
                Some(_) => map.returns.clone(),
                None => Vec::new(),
            },
        };
        let refused = stops.is_empty();
        if !refused {
            self.armed = Some(Armed { to, after: self.tick(t), stops });
        }
        !refused
    }

    fn logic_nets(&self) -> &[String] {
        &self.nets
    }

    fn take_audio(&mut self, out: &mut CoAudio) {
        out.runs.append(&mut self.audio.runs);
        out.samples.append(&mut self.audio.samples);
    }

    fn take_logic(&mut self, t: f64, sync: bool, out: &mut Vec<(u32, LogicEdge)>) {
        if sync && self.attached && !self.nets.is_empty() {
            // as `state`: a refused batch from this commit holds the device here, settled
            self.sync(t);
        }
        while let Some(&(net, edge)) = self.levels.front().filter(|(_, edge)| edge.t <= t) {
            if let Some(level) = self.taken.get_mut(net as usize) {
                *level = Some(edge.high);
            }
            out.push((net, edge));
            self.levels.pop_front();
        }
    }

    fn restart_logic(&mut self, t: f64, out: &mut Vec<(u32, LogicEdge)>) {
        // the old record's changes: taken (so `taken` holds the levels at `t`) and dropped
        let mut old = Vec::new();
        self.take_logic(t, true, &mut old);
        out.extend(self.taken.iter().enumerate().filter_map(|(net, level)| Some((net as u32, LogicEdge { t, high: (*level)? }))));
    }

    fn state(&mut self, t: f64) -> Option<CoState> {
        if !self.attached {
            return None;
        }
        // the analog stands at `t`: a refused batch from this commit holds the device here
        self.sync(t);
        self.m.publish(t);
        let mut regs = Vec::new();
        self.m.regs(&mut regs);
        let mut chip = ChipState::default();
        self.m.chip(&mut chip);
        let debug = self.cfg.debug.clone();
        let pc = self.m.pc();
        let status = self.m.status();
        // statement addresses to ids; several begin at one address, outermost first
        let ran_at = std::mem::take(&mut self.ran);
        let (last, ran) = match &debug {
            Some(d) => {
                let mut ran: Vec<(u32, u32)> = Vec::new();
                for (addr, count) in ran_at {
                    ran.extend(d.starting_at(addr).iter().map(|&id| (id, count)));
                }
                ran.sort_unstable();
                (self.last.and_then(|addr| d.starting_at(addr).last().copied()), ran)
            }
            None => (None, Vec::new()),
        };
        for &(id, count) in &ran {
            match self.ran_counts.binary_search_by_key(&id, |r| r.0) {
                Ok(k) => self.ran_counts[k].1 += u64::from(count),
                Err(k) => self.ran_counts.insert(k, (id, u64::from(count))),
            }
        }
        let at = match (status, &debug) {
            (Status::Halted(h), _) => At::Halted(h),
            (Status::Sleeping { .. }, _) => At::Sleeping,
            (Status::Running, None) => At::Unmapped,
            (Status::Running, Some(d)) => match (d.routine(pc), last) {
                (Some(routine), _) => At::Runtime(routine),
                (None, Some(id)) => At::Statement(id),
                (None, None) => At::Reset,
            },
        };
        let mut bytes = Vec::new();
        let vars = debug.as_ref().map_or(Vec::new(), |d| d.vars.iter().map(|v| {
            bytes.resize(v.bytes(), 0);
            if self.m.read_data(v.addr, &mut bytes) { v.decode(&bytes) } else { FwValue::Unread }
        }).collect());
        let fault_cells = debug.as_ref().map_or(Vec::new(), |d| d.fault_cells.iter().filter_map(|cell| {
            let mut bytes = [0; 4];
            if !self.m.read_data(cell.addr, &mut bytes) { return None; }
            Some(FaultCell {
                code: cell.code,
                count: u16::from_le_bytes([bytes[0], bytes[1]]),
                site: u16::from_le_bytes([bytes[2], bytes[3]]),
            })
        }).collect());
        let pins = self.names.iter().enumerate().map(|(k, &pin)| {
            let drive = self.m.drive(k);
            let driven = match drive {
                Drive::High => Some(true),
                Drive::Low | Drive::OpenDrain { low: true, .. } => Some(false),
                _ => None,
            };
            let (mode, duty) = chip.pins.get(k).copied().unwrap_or(match drive {
                Drive::High | Drive::Low | Drive::OpenDrain { low: true, .. } => (PinUse::Output, 0.0),
                Drive::Input(Pull::Up) | Drive::OpenDrain { low: false, pull: Pull::Up } => (PinUse::PullUp, 0.0),
                Drive::Input(_) | Drive::OpenDrain { low: false, .. } => (PinUse::Input, 0.0),
            });
            // a driven pad shows what it drives; anything else what it samples
            FwPin { pin, mode, level: driven.or(self.level[k]), duty }
        }).collect();
        let state = FwState {
            image_id: debug.as_ref().map_or(0, |d| d.image_id),
            source_rev: debug.as_ref().map_or(0, |d| d.source_rev),
            debug,
            t,
            cycle: self.m.now(),
            t_reset: self.t_reset,
            f_clk: self.hz as f64,
            at,
            halt_detail: if matches!(status, Status::Halted(_)) { self.m.halt_detail() } else { None },
            handler: self.handler,
            chain: self.chain.clone(),
            last,
            ran,
            ran_counts: self.ran_counts.clone(),
            cond_counts: self.cond_counts.clone(),
            vars,
            fault_cells,
            pins,
            timers: chip.timers,
            pending: chip.pending,
            wake: chip.wake,
            faults: std::mem::take(&mut self.faults),
            fault_counts: self.fault_counts.clone(),
            radio_starts: self.radio_starts,
            radio_illegal_pc: self.radio_illegal_pc,
            stopped: self.stopped.take(),
            pc,
            regs,
            // committed at `t`, before the device runs ahead of it
            eeprom: self.m.eeprom_bytes(),
            net: self.net.as_ref().map(|end| end.status(self.upstream.0.is_some())),
            net_log: self.net.as_ref().map(|end| end.log.clone()),
        };
        Some(CoState(Arc::new(state)))
    }
}
