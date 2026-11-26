//! The host half of the audio owner.
//!
//! [`AudioOwner`] prepares (worker thread). [`AudioRuntime`] belongs to the
//! host's thread: it reserves a slot and ring space when a schedule is
//! staged, sends it when it is committed, writes control targets, queues
//! note packets, reads the engine's telemetry and drops every schedule the
//! engine retires. [`AudioDevice`] is what a host with a device holds: it
//! says when a device is wanted and gives the engine away, once, to the
//! callback that will own it.
//!
//! One engine mixes every live instance. An instance keeps sounding for as
//! long as it lives; which package or pane a window shows is not known here.

use crate::deck::{DeckCommand, DeckSource, TrackLease};
use crate::paged::{PageOp, PageOpKind, PageOrigin, PageOutcome, PagePacket, PageReturn, PagedCommand, PagedSource, PagedSourceKey, PrepareRefusal, PAGE_ROUTES};
use makepad_stage_asset_stream::window::MAX_WINDOW_SLOTS;
use makepad_stage_asset_stream::PcmPage;
use crate::engine::{AudioEngine, AudioProbePacket, Command, Packet, Retired, Telemetry, TrackReturn, AUDIO_PROBE_QUEUE};
use crate::nodes::OWNER;
use crate::schedule::{compile_controlled, control_range, AudioBudget, ControlBank, Layout, NodeState, Schedule};
use makepad_flow::document::ident::Generation;
use makepad_flow::live::{
    BaseInfo, Endpoint, EndpointValue, LiveOwner, NotReady, OwnerCommand, OwnerRuntime, PlanError, PrepareRequest,
    PreparedSlice, ProbeValue, ResourceState, SessionKey, StageRefusal, WriteRefusal,
};
use makepad_lock_free::{ring, triple_buffer, Consumer, Producer, Reader};
use std::any::Any;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Installs and removals on their way to the engine.
const COMMANDS: usize = 16;
/// Schedules on their way back. A full ring makes the engine wait, not drop.
const RETIRED: usize = 16;
const PACKETS: usize = 256;

/// What a prepare needs from the device.
pub struct AudioContext {
    pub format: u64,
    pub budget: Arc<AudioBudget>,
}

struct Shared {
    wanted: AtomicBool,
    format: Arc<AtomicU64>,
    budget: Arc<AudioBudget>,
}

/// Host-thread-only device handoff. Clones share a local RefCell, so neither
/// this handle nor offline rendering can race the UI from another thread.
/// After take_engine, the callback owns the engine until device teardown;
/// keep pumping this runtime through each session's removal acknowledgement.
#[derive(Clone)]
pub struct AudioDevice {
    shared: Arc<Shared>,
    engine: Rc<RefCell<Option<AudioEngine>>>,
    #[cfg(not(target_arch = "wasm32"))]
    clocks: Rc<RefCell<ClockView>>,
    pages: Rc<RefCell<PageLedger>>,
    exchanges: Rc<RefCell<ExchangeView>>,
}

/// The newest answered deck exchanges, by serial: host thread only.
#[derive(Default)]
struct ExchangeView {
    answered: VecDeque<(u64, bool)>,
}

/// Answers kept for the caller to read; far more than one caller has open.
const EXCHANGE_ANSWERS: usize = 16;

/// A Transport's clock as the device callback last rendered it: the beat at
/// the end of the rendered buffer, the tempo and play state it ran with,
/// and the callback's output frame count, rate, serial and finish time. The
/// same publication repeats unchanged until another callback renders, so a
/// stalled device ages instead of looking fresh.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioTransportClock {
    pub beats: f64,
    pub bpm: f64,
    pub playing: bool,
    pub output_frame: u64,
    pub sample_rate: u32,
    pub publication: u64,
    pub published_at: f64,
    /// The transport's locates so far.
    pub located: u32,
}

/// A deck observation with the callback publication it came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioDeckObservation {
    pub deck: crate::engine::DeckObservation,
    pub output_frame: u64,
    pub sample_rate: u32,
    pub publication: u64,
    /// `None` when the host installed no timestamp sampler.
    pub published_at: Option<f64>,
}

/// Host-thread mirror of the newest telemetry and of which generation each
/// session's slot runs, refreshed by the runtime's pump. The callback never
/// touches it.
#[derive(Default)]
struct ClockView {
    /// The newest published callback count, frame count and device rate.
    callbacks: u64,
    frames: u64,
    rate: u32,
    slots: Vec<crate::engine::SlotTelemetry>,
    sessions: Vec<(SessionKey, u16, Vec<(u64, Arc<Layout>, Board)>)>,
}

/// A generation's board, read on the host thread only.
type Board = Rc<RefCell<crate::board::BoardReader>>;

impl AudioDevice {
    /// A revision with audio nodes was applied and no device runs yet.
    pub fn wanted(&self) -> bool {
        self.shared.wanted.load(Ordering::Relaxed)
    }

    /// The engine, once. Move it into the device callback and call
    /// [`AudioEngine::render`] for every buffer; never install it twice.
    /// Final engine destruction must be off the realtime callback. This
    /// handle does not provide a stopped-device recovery or return protocol.
    pub fn take_engine(&self) -> Option<AudioEngine> {
        self.engine.try_borrow_mut().ok()?.take()
    }

    /// The device's published progress, once it runs the engine itself:
    /// `(callbacks, frames, rate)`. `None` while this host still drives the
    /// engine (no device yet), which then settles every command each pump.
    pub fn rendered_progress(&self) -> Option<(u64, u64, u32)> {
        if self.engine.try_borrow().ok()?.is_some() {
            return None;
        }
        #[cfg(not(target_arch = "wasm32"))]
        return self.clocks.try_borrow().ok().map(|view| (view.callbacks, view.frames, view.rate));
        #[cfg(target_arch = "wasm32")]
        None
    }

    /// Whether an owner cue on deck `node` (qualified) still waits for its
    /// answer from the engine.
    pub fn cue_outstanding(&self, node: &str) -> bool {
        self.pages.try_borrow().map_or(true, |ledger| ledger.routes.iter().flatten().any(|route| route.kind == PageOpKind::Cue && route.node == node))
    }

    /// The engine's answer to deck exchange `serial`: `Some(true)` applied
    /// (both decks' states, transports and host mirrors traded), `Some(false)`
    /// not applied (a deck left, or another schedule runs), `None` pending.
    pub fn exchange_outcome(&self, serial: u64) -> Option<bool> {
        let view = self.exchanges.try_borrow().ok()?;
        view.answered.iter().find(|(answered, _)| *answered == serial).map(|(_, applied)| *applied)
    }

    /// Reserve `max_pcm_bytes` of the shared track budget before producing
    /// a clip. The lease is `Send`; finish it on the worker with the clip.
    pub fn reserve_track(&self, max_pcm_bytes: usize) -> Result<TrackLease, String> {
        TrackLease::reserve(&self.shared.budget, max_pcm_bytes)
    }

    /// The oldest answered page operation, with its full origin, freeing its
    /// place in the owner's bounded ledger. Receipts of a session or layout
    /// that is gone are still here until taken.
    pub fn take_paged_return(&self) -> Option<PagedReceipt> {
        let mut ledger = self.pages.try_borrow_mut().ok()?;
        while let Some(index) = ledger.ready.pop_front() {
            let Some(route) = ledger.routes.get_mut(index as usize).and_then(Option::take) else { continue };
            let Some(back) = route.ready else { continue };
            return Some(PagedReceipt {
                session: route.session, generation: route.generation, node: route.node, continuity: route.continuity,
                source: route.source, ticket: route.ticket, kind: route.kind, outcome: back.outcome, page: back.page, bundle: back.bundle,
            });
        }
        None
    }

    /// Reserve one paged load's whole page capacity
    /// ([`PAGED_GROUP_PAGES`](crate::paged::PAGED_GROUP_PAGES) pages) on this
    /// device's track budget, inside a new page group. The lease lives in
    /// the group until its last handle and page are dropped. Clone the
    /// group only on the host and the reader worker.
    pub fn lease_page_group(&self) -> Result<makepad_stage_asset_stream::PageGroup, String> {
        crate::paged::lease_group(&self.shared.budget)
    }

    /// The only way to make a [`PagedSource`]: a fresh group leased from
    /// this device and a structurally valid timeline snapshot with audio.
    /// Refused, the group comes back unchanged. The caller (the reader)
    /// proves the snapshot's publication; this checks structure, rate and
    /// device budget. See [`crate::paged`] for the checkpoint status.
    pub fn prepare_paged(
        &self,
        group: makepad_stage_asset_stream::PageGroup,
        source_sequence: u64,
        load: makepad_stage_asset_stream::ConsumerLoad,
        snapshot: &makepad_stage_asset_stream::TimelineSnapshot,
    ) -> Result<Box<PagedSource>, PrepareRefusal> {
        let start = crate::paged::PagedStart::History { frame: snapshot.retained_start };
        self.prepare_paged_at(group, source_sequence, load, snapshot, start)
    }

    /// As [`prepare_paged`](Self::prepare_paged), starting at `start`: a
    /// History frame inside the retained/committed range, or a Tail
    /// prebuffer on a growing, open, non-empty timeline. The start and its
    /// follow mode are fixed with serial 0 before any read, so pages
    /// prefilled for it stay valid through the load. Refused, the group
    /// comes back unchanged.
    ///
    /// Until the Box is sent with `LoadPaged` the host or reader worker owns
    /// it exclusively: only then may [`PagedSource::prefill`] and
    /// [`PagedSource::prefill_notes`] run (the Box is `Send`; this device is
    /// not). Nothing is acknowledged before the load's own source receipt.
    pub fn prepare_paged_at(
        &self,
        group: makepad_stage_asset_stream::PageGroup,
        source_sequence: u64,
        load: makepad_stage_asset_stream::ConsumerLoad,
        snapshot: &makepad_stage_asset_stream::TimelineSnapshot,
        start: crate::paged::PagedStart,
    ) -> Result<Box<PagedSource>, PrepareRefusal> {
        crate::paged::prepare(&self.shared.budget, group, source_sequence, load, snapshot, start)
    }

    /// The rendered observation of the AudioDeck whose `source` endpoint
    /// `target` addresses, in exactly the target's session and generation,
    /// from a buffer that generation itself rendered at the device rate.
    /// Numeric state needs no timestamp; `published_at` is the host
    /// sampler's stamp when one is installed. Native-only, as `WriteTarget` is.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn deck_observation(&self, target: &makepad_flow::live::WriteTarget) -> Option<AudioDeckObservation> {
        let endpoint = target.endpoint();
        if endpoint.owner != OWNER || endpoint.addr.param != "source" { return None; }
        let node = makepad_flow::live::plan::qualified(&endpoint.addr.node);
        let view = self.clocks.try_borrow().ok()?;
        let (_, slot, gens) = view.sessions.iter().find(|(key, _, _)| key == target.session())?;
        let generation = target.generation().0;
        let (_, layout, board) = gens.iter().find(|(gen, _, _)| *gen == generation)?;
        layout.nodes.iter().find(|item| item.id == node && item.type_name == "AudioDeck")?;
        let index = *layout.decks.get(&node)? as usize;
        let telemetry = view.slots.get(*slot as usize)?;
        if telemetry.incarnation != target.session().incarnation || telemetry.generation != generation || telemetry.rate_mismatch {
            return None;
        }
        let mut board = board.try_borrow_mut().ok()?;
        let board = board.latest();
        if board.publication == 0 {
            return None;
        }
        Some(AudioDeckObservation {
            deck: *board.decks.get(index)?,
            output_frame: board.frame,
            sample_rate: board.rate,
            publication: board.publication,
            published_at: board.at,
        })
    }

    /// The rendered clock of the Transport `target` addresses: its `bpm`
    /// endpoint in exactly the target's session and generation, and only
    /// when that generation itself rendered the published clock. `None` too
    /// when the host installed no timestamp sampler (web audio): freshness
    /// cannot be judged, so no clock is offered rather than one stamped at
    /// host arrival. This exact-target host lookup is native-only, as its
    /// `WriteTarget` is; the engine's clock telemetry itself is portable.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn transport_clock(&self, target: &makepad_flow::live::WriteTarget) -> Option<AudioTransportClock> {
        let endpoint = target.endpoint();
        if endpoint.owner != OWNER || endpoint.addr.param != "bpm" { return None; }
        let node = makepad_flow::live::plan::qualified(&endpoint.addr.node);
        let view = self.clocks.try_borrow().ok()?;
        let (_, slot, gens) = view.sessions.iter().find(|(key, _, _)| key == target.session())?;
        let generation = target.generation().0;
        let (_, layout, board) = gens.iter().find(|(gen, _, _)| *gen == generation)?;
        layout.nodes.iter().find(|item| item.id == node && item.type_name == "Transport")?;
        let index = *layout.transports.get(&node)? as usize;
        let telemetry = view.slots.get(*slot as usize)?;
        // The slot must run exactly this generation now, at the device rate,
        // and that generation must itself have rendered the clock (its board).
        if telemetry.incarnation != target.session().incarnation || telemetry.generation != generation || telemetry.rate_mismatch {
            return None;
        }
        let mut board = board.try_borrow_mut().ok()?;
        let board = board.latest();
        if board.publication == 0 {
            return None;
        }
        let clock = board.clocks.get(index)?;
        Some(AudioTransportClock {
            beats: clock.beats,
            bpm: clock.bpm,
            playing: clock.playing,
            output_frame: board.frame,
            sample_rate: board.rate,
            publication: board.publication,
            published_at: board.at?,
            located: clock.located,
        })
    }

    /// Render without a device, on the caller's thread: for a host that has
    /// none (a headless diagnostic). `None` once a callback owns the engine.
    pub fn render_offline(&self, rate: u32, left: &mut [f32], right: &mut [f32]) -> Option<()> {
        let mut engine = self.engine.try_borrow_mut().ok()?;
        engine.as_mut()?.render(rate as f64, left, right);
        Some(())
    }
}

pub struct AudioOwner;

struct PreparedAudio {
    schedule: Box<Schedule>,
    layout: Arc<Layout>,
    board: crate::board::BoardReader,
}

impl PreparedSlice for PreparedAudio {
    fn base(&self) -> Option<Generation> {
        self.schedule.base.map(Generation)
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

impl LiveOwner for AudioOwner {
    fn key(&self) -> &str {
        OWNER
    }

    fn prepare(&self, request: PrepareRequest<'_>) -> Result<Box<dyn PreparedSlice>, PlanError> {
        let context = request
            .context
            .downcast_ref::<AudioContext>()
            .ok_or_else(|| PlanError::general("the audio owner was given another owner's context"))?;
        let base = request.base.as_ref().and_then(|base| base.layout.downcast_ref::<Layout>());
        let (compiled, board) = compile_controlled(request.slice, request.generation.0, request.session.incarnation, context.format, base, &context.budget, Some(request.control))?;
        Ok(Box::new(PreparedAudio { schedule: compiled.schedule, layout: compiled.layout, board }))
    }
}

struct Gen {
    generation: u64,
    nested_change_reported: bool,
    layout: Arc<Layout>,
    bank: Arc<ControlBank>,
    board: Board,
}

#[derive(PartialEq)]
enum Stop {
    No,
    Wanted,
    Sent,
}

/// Exchange what two decks play: sent to one deck's `source` endpoint,
/// naming the other deck by its qualified node id (`with`, `deck_b` for a
/// top-level node). The engine
/// trades both decks' whole states and transport controls at the point both
/// dips are silent; [`AudioDevice::exchange_outcome`] answers `serial`, which
/// the caller chooses (strictly increasing, never 0).
pub struct DeckExchange {
    pub with: String,
    pub serial: u64,
}

/// Withdraw the unanswered exchange `serial` (sent to either of its decks'
/// `source` endpoints). The engine answers it: not applied if it withdrew it
/// in time, applied if it had already traded the decks. Never assumed.
pub struct DeckExchangeCancel {
    pub serial: u64,
}

/// The transport controls a deck exchange trades (and `cue`, which reads
/// `cue_at`): written only outside an exchange of their deck.
pub const EXCHANGED_CONTROLS: [&str; 9] = ["play", "speed", "loop_in", "loop_out", "loop_enabled", "cue_at", "cue", "scratch", "scratch_at"];

/// Whether `node.param` is fenced by the unanswered exchange, if any.
fn exchange_fences(exchange: Option<&(u64, (String, u64), (String, u64))>, node: &str, type_name: &str, param: &str) -> bool {
    type_name == "AudioDeck"
        && exchange.is_some_and(|(_, a, b)| a.0 == node || b.0 == node)
        && EXCHANGED_CONTROLS.contains(&param)
}

/// What the host knows of one deck, by the sequences its commands carried.
/// Acknowledgements may arrive out of order (a superseded swap after the
/// one that replaced it): a sequence only advances `active`/`error` when it
/// is newer than what they name, and only its own pending entry is cleared.
/// Host copy of what a load put on a deck (no PCM; a paged source's group
/// handle, so page admission can check it).
#[derive(Clone, Default)]
enum SourceMirror {
    #[default]
    None,
    Clip,
    Paged(PagedMirror),
    Feed,
}

/// Immutable identity of a loaded paged source, kept by the host.
#[derive(Clone)]
struct PagedMirror {
    key: PagedSourceKey,
    sample_rate: u32,
    group: makepad_stage_asset_stream::PageGroup,
}

impl PagedMirror {
    fn of(source: &PagedSource) -> Self {
        Self { key: source.key(), sample_rate: source.sample_rate(), group: source.group().clone() }
    }
}

/// One admitted page operation, from admission until its receipt is taken.
/// Kept here, not in the session, so it outlives its session and layout.
struct PageRoute {
    session: SessionKey,
    generation: u64,
    node: String,
    continuity: u64,
    source: PagedSourceKey,
    ticket: u64,
    kind: PageOpKind,
    /// Exactly what the callback was given; a receipt is delivered only if
    /// it carries this origin and kind.
    origin: PageOrigin,
    ready: Option<PageReturn>,
}

/// The owner's page-operation ledger: at most `PAGE_ROUTES` operations
/// outstanding across every deck and session, queued, in the callback,
/// pending as a seek, returned or ready. Host thread only.
struct PageLedger {
    routes: Vec<Option<PageRoute>>,
    /// Routes with a receipt, oldest first; never longer than `routes`.
    ready: VecDeque<u16>,
    cue_ticket: u64,
}

impl PageLedger {
    fn new() -> Self {
        Self { routes: (0..PAGE_ROUTES).map(|_| None).collect(), ready: VecDeque::with_capacity(PAGE_ROUTES), cue_ticket: 0 }
    }

    fn free_route(&self) -> Option<usize> {
        self.routes.iter().position(Option::is_none)
    }

    /// An outstanding operation of the caller on `source` with `ticket`.
    fn outstanding(&self, source: &PagedSourceKey, ticket: u64) -> bool {
        self.routes.iter().flatten().any(|route| route.kind != PageOpKind::Cue && route.source == *source && route.ticket == ticket)
    }
}

/// The answer to one page operation, with its full origin. `page` is a
/// refused install's own page or a retired page, for the reader to recycle.
pub struct PagedReceipt {
    pub session: SessionKey,
    pub generation: u64,
    pub node: String,
    pub continuity: u64,
    pub source: PagedSourceKey,
    pub ticket: u64,
    pub kind: PageOpKind,
    pub outcome: PageOutcome,
    pub page: Option<Arc<PcmPage>>,
    /// A refused or retired stem bundle, for the host to hand back to the
    /// worker that disposes of it.
    pub bundle: Option<Arc<crate::bundle::PreparedStemBundle>>,
}

fn page_kind(op: &PageOp) -> PageOpKind {
    match op {
        PageOp::Install { .. } => PageOpKind::Install,
        PageOp::Retire { .. } => PageOpKind::Retire,
        PageOp::Notes { .. } => PageOpKind::Notes,
        PageOp::Frontier(_) => PageOpKind::Frontier,
        PageOp::Seek { .. } => PageOpKind::Seek,
        PageOp::Tail { .. } => PageOpKind::Tail,
        PageOp::InstallStems { .. } => PageOpKind::InstallStems,
        PageOp::RetireStems { .. } => PageOpKind::RetireStems,
        PageOp::StemTail { .. } => PageOpKind::StemTail,
    }
}

#[derive(Default)]
struct DeckHost {
    /// `(sequence, what it loads)` sent and not yet acknowledged, in order.
    pending: Vec<(u64, SourceMirror)>,
    /// What the newest accepted load put on the deck. Kept until a newer
    /// source is acknowledged, so a cue pins the accepted source only.
    /// The highest sequence ever sent on this identity: a later command
    /// must exceed it, whatever became of the earlier ones.
    highest_sent: u64,
    active: Option<u64>,
    active_source: SourceMirror,
    /// The newest sequence that was accepted (a load or an eject).
    accepted: u64,
    /// The newest failure, kept only while no later sequence succeeded.
    error: Option<(u64, String)>,
}

impl DeckHost {
    /// Take the source another deck held, from a deck exchange: its own
    /// sequence stays its identity; this deck's order watermarks move to
    /// `sent`/`accepted` (at least), so a later command is newer than both.
    fn import(&mut self, active: Option<u64>, source: SourceMirror, sent: u64, accepted: u64) {
        self.active = active;
        self.active_source = source;
        self.highest_sent = self.highest_sent.max(sent);
        self.accepted = self.accepted.max(accepted);
        self.error = None;
    }

    fn acknowledge(&mut self, sequence: u64, outcome: Result<(), String>) {
        let Some(at) = self.pending.iter().position(|(pending, _)| *pending == sequence) else { return };
        let (_, source) = self.pending.remove(at);
        match outcome {
            Ok(()) if sequence > self.accepted => {
                self.accepted = sequence;
                self.active = (!matches!(source, SourceMirror::None)).then_some(sequence);
                self.active_source = source;
                if self.error.as_ref().is_some_and(|(failed, _)| *failed < sequence) {
                    self.error = None;
                }
            }
            Ok(()) => {}
            Err(reason) if sequence > self.accepted && self.error.as_ref().is_none_or(|(failed, _)| sequence > *failed) => self.error = Some((sequence, reason)),
            Err(_) => {}
        }
    }

    fn state(&self) -> ResourceState {
        ResourceState {
            active: self.active,
            pending: self.pending.last().map(|(sequence, _)| *sequence),
            error: self.error.clone(),
            accepted: (self.accepted != 0).then_some(self.accepted),
        }
    }
}

struct AudioSession {
    slot: u16,
    staged: Option<(Gen, Box<Schedule>)>,
    /// Generations handed to the engine and not yet retired, oldest first.
    gens: Vec<Gen>,
    stop: Stop,
    staged_stop: Option<Generation>,
    rejected: Option<(Generation, String)>,
    /// `(qualified node, continuity)` of a deck to what its commands did.
    /// The continuity makes a deck removed and added again under the same
    /// name a different deck: the old entry answers only its own
    /// acknowledgements and is pruned once nothing refers to it.
    decks: BTreeMap<(String, u64), DeckHost>,
    /// An exchange sent and not yet answered: `(serial, deck, other deck)`.
    exchange: Option<(u64, (String, u64), (String, u64))>,
}

pub struct AudioRuntime {
    commands: Producer<Command>,
    retired: Consumer<Retired>,
    packets: Producer<Packet>,
    telemetry: Reader<Telemetry>,
    latest: Telemetry,
    /// Slots the engine has (it grows when every one holds an instance).
    capacity: usize,
    audio_probes: Consumer<AudioProbePacket>,
    /// The newest visual packet by (slot, span).
    latest_audio: HashMap<(u16, u16), AudioProbePacket>,
    shared: Arc<Shared>,
    engine: Rc<RefCell<Option<AudioEngine>>>,
    clocks: Rc<RefCell<ClockView>>,
    pages: Rc<RefCell<PageLedger>>,
    exchanges: Rc<RefCell<ExchangeView>>,
    page_commands: Producer<PagePacket>,
    page_returns: Consumer<PageReturn>,
    context_format: u64,
    sessions: BTreeMap<SessionKey, AudioSession>,
    /// Command slots promised to staged schedules.
    reserved: usize,
    /// Committed commands the ring could not take at once. Bounded by the
    /// reservations, and sent in order on the next pump.
    outbox: VecDeque<Command>,
}

/// The audio owner with its runtime and device handle. `offline_rate` is
/// for a host that will never have a device.
pub fn audio_owner(offline_rate: Option<u32>) -> (Arc<dyn LiveOwner>, Box<dyn OwnerRuntime>, AudioDevice) {
    let (commands, command_queue) = ring::<Command>(COMMANDS);
    let (retire, retired) = ring::<Retired>(RETIRED);
    let (packets, packet_queue) = ring::<Packet>(PACKETS);
    let (writer, telemetry) = triple_buffer(Telemetry::default());
    let (audio_writer, audio_probes) = ring::<AudioProbePacket>(AUDIO_PROBE_QUEUE);
    let initial = offline_rate.map(|rate| (1u64 << 32) | rate as u64).unwrap_or(0);
    let format = Arc::new(AtomicU64::new(initial));
    let (page_commands, page_command_queue) = ring::<PagePacket>(PAGE_ROUTES);
    let (page_return_writer, page_returns) = ring::<PageReturn>(PAGE_ROUTES);
    let engine = Rc::new(RefCell::new(Some(AudioEngine::new(command_queue, retire, packet_queue, writer, audio_writer, format.clone(), page_command_queue, page_return_writer))));
    let pages = Rc::new(RefCell::new(PageLedger::new()));
    let shared = Arc::new(Shared { wanted: AtomicBool::new(false), format, budget: Arc::new(AudioBudget::default()) });
    let clocks = Rc::new(RefCell::new(ClockView::default()));
    let exchanges = Rc::new(RefCell::new(ExchangeView::default()));
    let runtime = AudioRuntime {
        commands, retired, packets, telemetry,
        latest: Telemetry::default(),
        capacity: crate::engine::INITIAL_SLOTS,
        audio_probes,
        latest_audio: HashMap::new(),
        shared: shared.clone(),
        engine: engine.clone(),
        clocks: clocks.clone(),
        pages: pages.clone(),
        exchanges: exchanges.clone(),
        page_commands,
        page_returns,
        context_format: initial,
        sessions: BTreeMap::new(),
        reserved: 0,
        outbox: VecDeque::new(),
    };
    (Arc::new(AudioOwner), Box::new(runtime), AudioDevice { shared, engine, #[cfg(not(target_arch = "wasm32"))] clocks, pages, exchanges })
}

impl AudioRuntime {
    /// Admit one deck exchange. Refused (the command back untouched) while
    /// another exchange is unanswered, while either deck has a source on its
    /// way, or when `with` is not another deck of the running generation.
    fn admit_exchange(&mut self, session: &SessionKey, generation: Generation, slot: u16, incarnation: u64, (node, target, continuity): (String, u32, u64), command: OwnerCommand) -> Result<(), (WriteRefusal, OwnerCommand)> {
        macro_rules! refuse {
            ($refusal:expr) => {
                return Err(($refusal, command))
            };
        }
        let Some(DeckExchange { with, serial }) = command.payload.downcast_ref::<DeckExchange>() else {
            refuse!(WriteRefusal::WrongKind { expected: "a deck exchange" });
        };
        let (with, serial) = (with.clone(), *serial);
        let Some(entry) = self.sessions.get(session) else { refuse!(WriteRefusal::NotRunning) };
        let Some(origin) = entry.gens.iter().find(|gen| gen.generation == generation.0) else { refuse!(WriteRefusal::NotRunning) };
        let other = origin.layout.nodes.iter().find(|item| item.id == with && item.type_name == "AudioDeck").and(origin.layout.triggers.get(&with).copied());
        let Some((other_target, other_continuity)) = other.filter(|_| with != node) else {
            refuse!(WriteRefusal::WrongKind { expected: "another AudioDeck of the running graph" });
        };
        if serial == 0 || entry.exchange.is_some() {
            refuse!(WriteRefusal::Busy);
        }
        let loading = |key: &(String, u64)| entry.decks.get(key).is_some_and(|host| !host.pending.is_empty());
        let (a, b) = ((node, continuity), (with, other_continuity));
        if loading(&a) || loading(&b) {
            refuse!(WriteRefusal::Busy);
        }
        // Settled first: an owner cue still out on either deck, or a schedule
        // staged or on its way in (its bank was copied before the exchange).
        let cueing = self.pages.try_borrow().map_or(true, |ledger| ledger.routes.iter().flatten().any(|route| {
            route.kind == PageOpKind::Cue && &route.session == session && (route.node == a.0 || route.node == b.0)
        }));
        if cueing || entry.staged.is_some() || entry.gens.len() != 1 {
            refuse!(WriteRefusal::Busy);
        }
        if self.commands.free().saturating_sub(self.outbox.len()) <= self.reserved {
            refuse!(WriteRefusal::Busy);
        }
        drop(command);
        self.sessions.get_mut(session).expect("checked above").exchange = Some((serial, a, b));
        self.send(Command::Exchange { slot, incarnation, a: (target, continuity), b: (other_target, other_continuity), serial });
        Ok(())
    }

    /// Answer every session's exchange the engine reported: applied, the two
    /// decks' host mirrors (what each holds) trade places as the states did.
    fn settle_exchanges(&mut self) {
        let mut answered = Vec::new();
        for entry in self.sessions.values_mut() {
            let Some((serial, a, b)) = entry.exchange.clone() else { continue };
            let telemetry = &self.latest.slots[entry.slot as usize];
            if telemetry.exchange.0 != serial {
                continue;
            }
            let applied = telemetry.exchange.1;
            if applied {
                let take = |decks: &mut BTreeMap<(String, u64), DeckHost>, key: &(String, u64)| {
                    let host = decks.entry(key.clone()).or_default();
                    (host.active.take(), std::mem::take(&mut host.active_source))
                };
                let from_a = take(&mut entry.decks, &a);
                let from_b = take(&mut entry.decks, &b);
                // Sources keep their own sequences; each deck's command order
                // moves past everything either deck ever carried, so no later
                // command can reuse an imported sequence.
                let watermark = |decks: &BTreeMap<(String, u64), DeckHost>| [&a, &b].iter()
                    .filter_map(|key| decks.get(*key)).map(|host| (host.highest_sent, host.accepted))
                    .fold((0, 0), |(sent, accepted), (s, acc)| (sent.max(s), accepted.max(acc)));
                let (sent, accepted) = watermark(&entry.decks);
                for (key, (active, source)) in [(&a, from_b), (&b, from_a)] {
                    let host = entry.decks.entry(key.clone()).or_default();
                    host.import(active, source, sent, accepted);
                }
            }
            entry.exchange = None;
            answered.push((serial, applied));
        }
        if answered.is_empty() {
            return;
        }
        if let Ok(mut view) = self.exchanges.try_borrow_mut() {
            for answer in answered {
                if view.answered.len() >= EXCHANGE_ANSWERS {
                    view.answered.pop_front();
                }
                view.answered.push_back(answer);
            }
        }
    }

    /// Admit one page operation for the deck's accepted paged source. Every
    /// refusal hands the command back untouched, before its payload is taken.
    fn admit_page(&mut self, session: &SessionKey, generation: Generation, slot: u16, incarnation: u64, target: u32, continuity: u64, node: String, command: OwnerCommand) -> Result<(), (WriteRefusal, OwnerCommand)> {
        macro_rules! refuse {
            ($refusal:expr) => {
                return Err(($refusal, command))
            };
        }
        let Some(PagedCommand { source, op, ticket }) = command.payload.downcast_ref::<PagedCommand>() else {
            refuse!(WriteRefusal::WrongKind { expected: "a paged command" });
        };
        let source = *source;
        let host = self.sessions.get(session).and_then(|entry| entry.decks.get(&(node.clone(), continuity)));
        // After an exchange the source plays on the other deck; the engine
        // routes the operation to whichever deck holds its key.
        let holder = self.sessions.get(session).and_then(|entry| entry.decks.values().find_map(|host| match &host.active_source {
            SourceMirror::Paged(mirror) if mirror.key == source => Some(mirror),
            _ => None,
        }));
        let mirror = match (host.map(|host| &host.active_source), holder) {
            (Some(SourceMirror::Paged(mirror)), _) if mirror.key == source => mirror,
            (_, Some(mirror)) => mirror,
            _ => {
                // A load still on its way: not yet this source's deck.
                let coming = host.is_some_and(|host| host.pending.iter().any(|(_, pending)| matches!(pending, SourceMirror::Paged(mirror) if mirror.key == source)));
                refuse!(if coming { WriteRefusal::Busy } else { WriteRefusal::WrongKind { expected: "the deck's accepted paged source" } });
            }
        };
        let valid = match op {
            PageOp::Install { placement, page } => (placement.slot as usize) < MAX_WINDOW_SLOTS && page.is_admitted_by(&mirror.group)
                && page.tag() == placement.tag && placement.tag.media == source.media && placement.tag.load == source.load
                && page.sample_rate() == mirror.sample_rate && page.range() == placement.range,
            PageOp::Retire { slot, .. } => (*slot as usize) < MAX_WINDOW_SLOTS,
            PageOp::Notes { notes, .. } => notes.iter().flatten().all(|note| note.range.start < note.range.end),
            PageOp::Frontier(frontier) => frontier.retained_start <= frontier.committed_end,
            PageOp::Tail { prebuffer, .. } => *prebuffer <= crate::paged::tail_prebuffer_limit(mirror.sample_rate),
            PageOp::Seek { .. } => true,
            PageOp::InstallStems { slot, bundle, .. } => (*slot as usize) < crate::bundle::STEM_SLOTS && bundle.leased_from(&self.shared.budget)
                && bundle.media() == source.media && bundle.sample_rate() == mirror.sample_rate
                && !bundle.range().is_empty() && bundle.stem(0).len() as u64 == bundle.range().len(),
            PageOp::RetireStems { slot, .. } => (*slot as usize) < crate::bundle::STEM_SLOTS,
            PageOp::StemTail { cushion, .. } => *cushion > 0 && *cushion <= crate::paged::stem_cushion_limit(mirror.sample_rate),
        };
        if !valid {
            refuse!(WriteRefusal::WrongKind { expected: "a page operation matching this source: its group, tag, rate, range and slot" });
        }
        let kind = page_kind(op);
        if self.pages.borrow().outstanding(&source, *ticket) {
            refuse!(WriteRefusal::WrongKind { expected: "a ticket not outstanding on this source" });
        }
        let Some(route) = self.pages.borrow().free_route() else { refuse!(WriteRefusal::Busy) };
        if self.page_commands.free() == 0 { refuse!(WriteRefusal::Busy); }
        let PagedCommand { source, ticket, op } = match command.take::<PagedCommand>() {
            Ok(paged) => *paged,
            Err(command) => return Err((WriteRefusal::WrongKind { expected: "a paged command" }, command)),
        };
        let origin = PageOrigin { route: route as u16, slot, incarnation, target, continuity, origin_generation: generation.0, source, ticket };
        let mut ledger = self.pages.borrow_mut();
        ledger.routes[route] = Some(PageRoute { session: session.clone(), generation: generation.0, node, continuity, source, ticket, kind, origin, ready: None });
        if let Err(PagePacket { op, .. }) = self.page_commands.push(PagePacket { origin, kind, op }) {
            ledger.routes[route] = None;
            return Err((WriteRefusal::Busy, OwnerCommand::new(OWNER, PagedCommand { source, ticket, op })));
        }
        Ok(())
    }

    fn send(&mut self, command: Command) {
        if !self.outbox.is_empty() {
            self.outbox.push_back(command);
            return;
        }
        if let Err(command) = self.commands.push(command) {
            self.outbox.push_back(command);
        }
    }

    fn gen_for<'a>(session: &'a AudioSession, generation: u64) -> Option<&'a Gen> {
        session.gens.iter().find(|gen| gen.generation == generation)
    }

    fn audio_probe(&self, session: &SessionKey, generation: Generation, name: &str) -> Option<ProbeValue> {
        if self.active(session) != Some(generation) { return None; }
        let entry = self.sessions.get(session)?;
        let telemetry = &self.latest.slots[entry.slot as usize];
        if telemetry.rate_mismatch { return None; }
        let gen = Self::gen_for(entry, generation.0)?;
        let index = *gen.layout.audio_probes.get(name)? as usize;
        let packet = self.latest_audio.get(&(entry.slot, index as u16))?;
        if packet.incarnation != session.incarnation || packet.generation != generation.0
            || packet.value.sample_rate != self.latest.rate
        { return None; }
        Some(ProbeValue::Audio(packet.value))
    }
}

impl OwnerRuntime for AudioRuntime {
    fn key(&self) -> &str {
        OWNER
    }

    fn context(&mut self) -> Result<Arc<dyn Any + Send + Sync>, NotReady> {
        let format = self.shared.format.load(Ordering::Acquire);
        if format as u32 == 0 {
            // No callback has reported a rate: ask the host for a device.
            self.shared.wanted.store(true, Ordering::Relaxed);
            return Err(NotReady);
        }
        Ok(Arc::new(AudioContext { format, budget: self.shared.budget.clone() }))
    }

    fn base(&self, session: &SessionKey) -> Option<BaseInfo> {
        let entry = self.sessions.get(session)?;
        let active = self.latest.slots[entry.slot as usize];
        if active.incarnation != session.incarnation { return None; }
        let gen = Self::gen_for(entry, active.generation)?;
        Some(BaseInfo { generation: Generation(gen.generation), layout: gen.layout.clone() })
    }

    fn stage(
        &mut self,
        session: &SessionKey,
        generation: Generation,
        prepared: Box<dyn PreparedSlice>,
    ) -> Result<(), (StageRefusal, Box<dyn PreparedSlice>)> {
        let prepared = match prepared.into_any().downcast::<PreparedAudio>() {
            Ok(prepared) => prepared,
            Err(_) => {
                // Not ours, and its concrete type is gone with the cast: an
                // empty slice goes back so the refusal has something to carry.
                struct Nothing;
                impl PreparedSlice for Nothing {
                    fn base(&self) -> Option<Generation> { None }
                    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> { self }
                }
                return Err((StageRefusal::Refused("the slice was not prepared by the audio owner".into()), Box::new(Nothing)));
            }
        };
        let refuse = |reason, prepared: Box<PreparedAudio>| Err((reason, prepared as Box<dyn PreparedSlice>));
        if prepared.schedule.format != self.shared.format.load(Ordering::Acquire)
            || prepared.schedule.incarnation != session.incarnation
            || prepared.schedule.generation != generation.0
        { return refuse(StageRefusal::StaleBase, prepared); }
        let free = self.commands.free().saturating_sub(self.outbox.len());
        let existing = self.sessions.get(session);
        // A deck exchange runs on the active schedule's bank: a new schedule
        // (whose bank would be copied now) waits until it is answered.
        if existing.is_some_and(|entry| entry.exchange.is_some()) {
            return refuse(StageRefusal::Busy, prepared);
        }
        if existing.is_some_and(|entry| entry.stop != Stop::No || entry.staged_stop.is_some()) {
            return refuse(StageRefusal::Busy, prepared);
        }
        let base = existing.and_then(|entry| entry.gens.last()).map(|gen| gen.generation);
        if prepared.schedule.base != base { return refuse(StageRefusal::StaleBase, prepared); }
        if existing.is_some_and(|entry| entry.gens.len() > 1 || (!entry.gens.is_empty() && self.active(session) != base.map(Generation)))
            || (existing.is_none_or(|entry| entry.staged.is_none()) && free <= self.reserved)
        { return refuse(StageRefusal::Busy, prepared); }
        if existing.is_none() {
            let free_slot = (0..self.capacity as u16).find(|slot| !self.sessions.values().any(|entry| entry.slot == *slot));
            let slot = match free_slot {
                Some(slot) => slot,
                // Every slot holds an instance: the engine grows (twice as
                // many, prepared here), ahead of this instance's install.
                None if self.capacity * 2 <= u16::MAX as usize => {
                    let grown = self.capacity * 2;
                    let (writer, reader) = triple_buffer(Telemetry::with_slots(grown));
                    self.send(Command::Grow(crate::engine::Grow::new(grown, writer)));
                    self.telemetry = reader;
                    self.latest = Telemetry::with_slots(grown);
                    let slot = self.capacity as u16;
                    self.capacity = grown;
                    slot
                }
                None => return refuse(StageRefusal::Refused("more live audio instances than slots can be indexed".into()), prepared),
            };
            self.sessions.insert(session.clone(), AudioSession { slot, staged: None, gens: Vec::new(), stop: Stop::No, staged_stop: None, rejected: None, decks: BTreeMap::new(), exchange: None });
        }
        let entry = self.sessions.get_mut(session).expect("admitted above");
        let PreparedAudio { schedule, layout, board } = *prepared;
        let bank = schedule.controls.clone();
        // Remember the manual value across driver changes, only while the
        // authored value and base meaning survive. A wire changes only the
        // effective slot; disconnect starts from this remembered manual value.
        if let Some(base) = entry.gens.last() {
            for (name, (index, authored)) in &layout.controls {
                if let Some((base_index, base_authored)) = base.layout.controls.get(name) {
                    if base_authored.to_bits() == authored.to_bits() && same_control_meaning(&base.layout, &layout, name) {
                        bank.set_manual(*index, base.bank.manual(*base_index));
                        if compatible_control(&base.layout, &layout, name) {
                            bank.set_effective(*index, base.bank.get(*base_index));
                        }
                    }
                }
            }
        }
        if entry.staged.replace((Gen { generation: generation.0, nested_change_reported: false, layout, bank, board: Rc::new(RefCell::new(board)) }, schedule)).is_none() {
            self.reserved += 1;
        }
        Ok(())
    }

    fn commit(&mut self, session: &SessionKey, generation: Generation) {
        let Some(entry) = self.sessions.get_mut(session) else { return };
        if entry.staged.as_ref().map(|(gen, _)| gen.generation) != Some(generation.0) {
            return;
        }
        let (gen, schedule) = entry.staged.take().expect("checked above");
        let slot = entry.slot;
        entry.gens.push(gen);
        self.reserved -= 1;
        self.send(Command::Install { slot, schedule });
    }

    fn abort(&mut self, session: &SessionKey, generation: Generation) {
        let Some(entry) = self.sessions.get_mut(session) else { return };
        if entry.staged.as_ref().map(|(gen, _)| gen.generation) == Some(generation.0) {
            entry.staged = None;
            self.reserved -= 1;
        }
        if entry.gens.is_empty() && entry.staged.is_none() && entry.stop == Stop::No {
            self.sessions.remove(session);
        }
    }

    fn active(&self, session: &SessionKey) -> Option<Generation> {
        let entry = self.sessions.get(session)?;
        let generation = self.latest.slots[entry.slot as usize].generation;
        // Only a generation of this session counts: a slot is reused.
        (self.latest.slots[entry.slot as usize].incarnation == session.incarnation)
            .then(|| Self::gen_for(entry, generation).map(|gen| Generation(gen.generation))).flatten()
    }

    fn write(
        &mut self,
        session: &SessionKey,
        generation: Generation,
        endpoint: &Endpoint,
        value: EndpointValue,
    ) -> Result<Option<f64>, WriteRefusal> {
        let entry = self.sessions.get(session).ok_or(WriteRefusal::NotRunning)?;
        if entry.stop != Stop::No || endpoint.owner != OWNER { return Err(WriteRefusal::NotRunning); }
        let origin = entry.gens.iter().chain(entry.staged.iter().map(|(gen, _)| gen))
            .find(|gen| gen.generation == generation.0).ok_or(WriteRefusal::NotRunning)?;
        let name = endpoint.qualified();
        let node = makepad_flow::live::plan::qualified(&endpoint.addr.node);
        let kind = origin.layout.nodes.iter().find(|item| item.id == node).ok_or(WriteRefusal::UnknownEndpoint)?;
        // The engine trades these slots between the two decks at the exchange;
        // a write now would race it. The caller holds it until the answer.
        if exchange_fences(entry.exchange.as_ref(), &node, &kind.type_name, &endpoint.addr.param) {
            return Err(WriteRefusal::Busy);
        }
        match value {
            EndpointValue::Number(_) | EndpointValue::Toggle(_) => {
                let (min, max, toggle) = control_range(&kind.type_name, &endpoint.addr.param).ok_or(WriteRefusal::UnknownEndpoint)?;
                if !origin.layout.controls.contains_key(&name) { return Err(WriteRefusal::UnknownEndpoint); }
                if let Some(by) = origin.layout.drivers.get(&name) { return Err(WriteRefusal::Driven { by: by.clone() }); }
                let number = match value {
                    EndpointValue::Number(value) => value,
                    EndpointValue::Toggle(on) if toggle => if on { 1.0 } else { 0.0 },
                    _ => return Err(WriteRefusal::WrongKind { expected: "number" }),
                };
                if !number.is_finite() { return Err(WriteRefusal::NotFinite); }
                if toggle && number != 0.0 && number != 1.0 { return Err(WriteRefusal::WrongKind { expected: "0/1 toggle" }); }
                let number = number.clamp(min, max) as f32;
                for gen in entry.gens.iter().chain(entry.staged.iter().map(|(gen, _)| gen)) {
                    if compatible_control(&origin.layout, &gen.layout, &name) {
                        if let Some((index, _)) = gen.layout.controls.get(&name) { gen.bank.set_manual(*index, number); }
                    }
                }
                Ok(Some(number as f64))
            }
            // No audio node plays notes written to it: scores play theirs.
            EndpointValue::Note { .. } => Err(WriteRefusal::UnknownEndpoint),
            EndpointValue::Pulse => {
                if kind.type_name != "AudioDeck" || endpoint.addr.param != "cue" { return Err(WriteRefusal::WrongKind { expected: "pulse" }); }
                if self.active(session) != Some(generation) { return Err(WriteRefusal::Busy); }
                let (target, continuity) = *origin.layout.triggers.get(&node).ok_or(WriteRefusal::UnknownEndpoint)?;
                let (index, _) = *origin.layout.controls.get(&format!("{node}.cue_at")).ok_or(WriteRefusal::UnknownEndpoint)?;
                let seconds = origin.bank.get(index) as f64;
                // Pinned to the accepted source: a cue never retargets a
                // load that is still on its way.
                let sequence = match entry.decks.get(&(node.clone(), continuity)).map(|host| (host.active, &host.active_source)) {
                    Some((Some(sequence), SourceMirror::Clip)) => sequence,
                    Some((Some(_), SourceMirror::Paged(mirror))) => {
                        // A paged cue is a credited seek of exactly the
                        // accepted source, from the serial it was last seen at.
                        let (key, rate) = (mirror.key, mirror.sample_rate);
                        if !seconds.is_finite() || seconds < 0.0 { return Err(WriteRefusal::NotFinite); }
                        let index = *origin.layout.decks.get(&node).ok_or(WriteRefusal::UnknownEndpoint)? as usize;
                        let telemetry = &self.latest.slots[entry.slot as usize];
                        let observed = (telemetry.incarnation == session.incarnation && telemetry.generation == generation.0)
                            .then(|| origin.board.try_borrow_mut().ok().and_then(|mut board| {
                                let board = board.latest();
                                (board.publication != 0).then(|| board.decks.get(index).copied()).flatten()
                            }))
                            .flatten();
                        let seen = observed.filter(|deck| deck.paged == Some(key)).ok_or(WriteRefusal::Busy)?;
                        let mut ledger = self.pages.borrow_mut();
                        let route = ledger.free_route().ok_or(WriteRefusal::Busy)?;
                        if self.page_commands.free() == 0 { return Err(WriteRefusal::Busy); }
                        let ticket = ledger.cue_ticket.checked_add(1).ok_or(WriteRefusal::WrongKind { expected: "a cue ticket (exhausted)" })?;
                        ledger.cue_ticket = ticket;
                        let page_origin = PageOrigin { route: route as u16, slot: entry.slot, incarnation: session.incarnation, target, continuity, origin_generation: generation.0, source: key, ticket };
                        ledger.routes[route] = Some(PageRoute { session: session.clone(), generation: generation.0, node: node.clone(), continuity, source: key, ticket, kind: PageOpKind::Cue, origin: page_origin, ready: None });
                        let frame = (seconds * rate as f64).round() as u64;
                        let packet = PagePacket { origin: page_origin, kind: PageOpKind::Cue, op: PageOp::Seek { expected_serial: seen.seek_serial, frame } };
                        if self.page_commands.push(packet).is_err() {
                            ledger.routes[route] = None;
                            return Err(WriteRefusal::Busy);
                        }
                        return Ok(None);
                    }
                    // Nothing accepted to cue: as before, nothing happens.
                    _ => return Ok(None),
                };
                let packet = Packet::Seek { slot: entry.slot, incarnation: session.incarnation, target, continuity, sequence, seconds };
                self.packets.push(packet).map(|()| None).map_err(|_| WriteRefusal::Busy)
            }
        }
    }

    fn command(
        &mut self,
        session: &SessionKey,
        generation: Generation,
        endpoint: &Endpoint,
        command: OwnerCommand,
    ) -> Result<(), (WriteRefusal, OwnerCommand)> {
        // Every check that can refuse runs before the payload is taken, so
        // a refusal hands back the command exactly as it came.
        macro_rules! refuse {
            ($refusal:expr) => {
                return Err(($refusal, command))
            };
        }
        let paged = command.is::<PagedCommand>();
        let exchange = command.is::<DeckExchange>();
        let cancel = command.is::<DeckExchangeCancel>();
        if endpoint.owner != OWNER || !(command.is::<DeckCommand>() || paged || exchange || cancel) {
            refuse!(WriteRefusal::WrongKind { expected: "audio deck command" });
        }
        match command.payload.downcast_ref::<DeckCommand>() {
            Some(DeckCommand::Load { track, .. }) if !track.leased_from(&self.shared.budget) => {
                refuse!(WriteRefusal::WrongKind { expected: "a track leased from this audio device's budget" });
            }
            Some(DeckCommand::LoadPaged { sequence, source }) => {
                if !source.leased_from(&self.shared.budget) {
                    refuse!(WriteRefusal::WrongKind { expected: "a paged source prepared on this audio device" });
                }
                if source.key().source_sequence != *sequence {
                    refuse!(WriteRefusal::WrongKind { expected: "a paged load whose sequence is its source key's" });
                }
            }
            _ => {}
        }
        let Some(entry) = self.sessions.get(session) else { refuse!(WriteRefusal::NotRunning) };
        if entry.stop != Stop::No { refuse!(WriteRefusal::NotRunning); }
        let (slot, incarnation) = (entry.slot, session.incarnation);
        let Some(origin) = entry.gens.iter().chain(entry.staged.iter().map(|(gen, _)| gen)).find(|gen| gen.generation == generation.0) else {
            refuse!(WriteRefusal::NotRunning);
        };
        let node = makepad_flow::live::plan::qualified(&endpoint.addr.node);
        let Some(kind) = origin.layout.nodes.iter().find(|item| item.id == node) else { refuse!(WriteRefusal::UnknownEndpoint) };
        if kind.type_name != "AudioDeck" || endpoint.addr.param != "source" {
            refuse!(WriteRefusal::WrongKind { expected: "the `source` endpoint of an AudioDeck" });
        }
        let Some(&(target, continuity)) = origin.layout.triggers.get(&node) else { refuse!(WriteRefusal::UnknownEndpoint) };
        // The command addresses the running deck; a generation on its way
        // is not there yet. The format the generation was prepared for
        // must still be the device's.
        if self.active(session) != Some(generation) || origin.layout.rate != self.shared.format.load(Ordering::Acquire) as u32 {
            refuse!(WriteRefusal::Busy);
        }
        if paged {
            let node = node.clone();
            return self.admit_page(session, generation, slot, incarnation, target, continuity, node, command);
        }
        if exchange {
            return self.admit_exchange(session, generation, slot, incarnation, (node, target, continuity), command);
        }
        if cancel {
            let serial = command.payload.downcast_ref::<DeckExchangeCancel>().map_or(0, |cancel| cancel.serial);
            let outstanding = self.sessions.get(session).and_then(|entry| entry.exchange.as_ref()).is_some_and(|(pending, _, _)| *pending == serial);
            if !outstanding {
                refuse!(WriteRefusal::WrongKind { expected: "the unanswered exchange's serial" });
            }
            if self.commands.free().saturating_sub(self.outbox.len()) <= self.reserved {
                refuse!(WriteRefusal::Busy);
            }
            drop(command);
            self.send(Command::CancelExchange { slot, incarnation, serial });
            return Ok(());
        }
        // A deck taking part in an exchange takes no new source until it is answered.
        let exchanging = self.sessions.get(session).and_then(|entry| entry.exchange.as_ref())
            .is_some_and(|(_, a, b)| (a.0 == node && a.1 == continuity) || (b.0 == node && b.1 == continuity));
        if exchanging {
            refuse!(WriteRefusal::Busy);
        }
        if self.commands.free().saturating_sub(self.outbox.len()) <= self.reserved {
            refuse!(WriteRefusal::Busy);
        }
        let deck = match command.take::<DeckCommand>() {
            Ok(deck) => *deck,
            Err(command) => return Err((WriteRefusal::WrongKind { expected: "audio deck command" }, command)),
        };
        let sequence = deck.sequence();
        let host = self.sessions.get_mut(session).expect("checked above").decks.entry((node, continuity)).or_default();
        if sequence <= host.highest_sent {
            let back = OwnerCommand::new(OWNER, deck);
            return Err((WriteRefusal::WrongKind { expected: "a sequence newer than every sent one" }, back));
        }
        host.highest_sent = sequence;
        let (mirror, track) = match deck {
            DeckCommand::Load { track, .. } => (SourceMirror::Clip, Some(DeckSource::Clip(track))),
            DeckCommand::LoadPaged { source, .. } => (SourceMirror::Paged(PagedMirror::of(&source)), Some(DeckSource::Paged(source))),
            DeckCommand::LoadFeed { ring, ramp, .. } => (SourceMirror::Feed, Some(DeckSource::Feed(Box::new(crate::feed::FeedSource::new(ring, ramp))))),
            DeckCommand::Eject { .. } => (SourceMirror::None, None),
        };
        host.pending.push((sequence, mirror));
        self.send(Command::Track { slot, incarnation, target, continuity, sequence, track });
        Ok(())
    }

    fn resource(&self, session: &SessionKey, generation: Generation, endpoint: &Endpoint) -> Option<ResourceState> {
        if endpoint.owner != OWNER || endpoint.addr.param != "source" { return None; }
        let entry = self.sessions.get(session)?;
        let origin = entry.gens.iter().chain(entry.staged.iter().map(|(gen, _)| gen)).find(|gen| gen.generation == generation.0)?;
        let node = makepad_flow::live::plan::qualified(&endpoint.addr.node);
        origin.layout.nodes.iter().find(|item| item.id == node && item.type_name == "AudioDeck")?;
        let (_, continuity) = *origin.layout.triggers.get(&node)?;
        Some(entry.decks.get(&(node, continuity)).map(DeckHost::state).unwrap_or_default())
    }

    fn probe(&self, session: &SessionKey, generation: Generation, endpoint: &Endpoint) -> Option<ProbeValue> {
        if endpoint.owner != OWNER || self.active(session) != Some(generation) { return None; }
        let entry = self.sessions.get(session)?;
        let gen = Self::gen_for(entry, generation.0)?;
        let name = endpoint.qualified();
        let Some(index) = gen.layout.probes.get(&name) else { return self.audio_probe(session, generation, &name); };
        let mut board = gen.board.try_borrow_mut().ok()?;
        Some(ProbeValue::Number(*board.latest().probes.get(*index as usize)? as f64))
    }

    fn probe_node(&self, session: &SessionKey, generation: Generation, node: &str, name: &str) -> Option<ProbeValue> {
        self.audio_probe(session, generation, &format!("{node}.{name}"))
    }

    fn control(&self, session: &SessionKey, generation: Generation, endpoint: &Endpoint) -> Option<f64> {
        if endpoint.owner != OWNER { return None; }
        let entry = self.sessions.get(session)?;
        let gen = entry.gens.iter().chain(entry.staged.iter().map(|(gen, _)| gen))
            .find(|gen| gen.generation == generation.0)?;
        let (index, _) = gen.layout.controls.get(&endpoint.qualified())?;
        Some(gen.bank.get(*index) as f64)
    }

    fn rejected(&mut self, session: &SessionKey) -> Option<(Generation, String)> {
        self.sessions.get_mut(session)?.rejected.take()
    }

    fn stage_stop(&mut self, session: &SessionKey, generation: Generation) -> Result<(), StageRefusal> {
        let Some(entry) = self.sessions.get_mut(session) else { return Ok(()); };
        if entry.staged_stop == Some(generation) || entry.stop != Stop::No { return Ok(()); }
        if entry.staged_stop.is_some() || entry.staged.is_some()
            || self.commands.free().saturating_sub(self.outbox.len()) <= self.reserved
        { return Err(StageRefusal::Busy); }
        entry.staged_stop = Some(generation);
        self.reserved += 1;
        Ok(())
    }

    fn commit_stop(&mut self, session: &SessionKey, generation: Generation) {
        let Some(entry) = self.sessions.get_mut(session) else { return; };
        if entry.staged_stop != Some(generation) { return; }
        entry.staged_stop = None;
        self.reserved -= 1;
        entry.stop = Stop::Sent;
        let slot = entry.slot;
        self.send(Command::Remove { slot, incarnation: session.incarnation });
    }

    fn abort_stop(&mut self, session: &SessionKey, generation: Generation) {
        if let Some(entry) = self.sessions.get_mut(session) {
            if entry.staged_stop == Some(generation) { entry.staged_stop = None; self.reserved -= 1; }
        }
    }

    fn stop(&mut self, session: &SessionKey) {
        let Some(entry) = self.sessions.get_mut(session) else { return };
        if entry.staged.take().is_some() {
            self.reserved -= 1;
        }
        if entry.staged_stop.take().is_some() { self.reserved -= 1; }
        if entry.stop == Stop::No {
            entry.stop = Stop::Wanted;
        }
    }

    fn stopped(&self, session: &SessionKey) -> bool {
        !self.sessions.contains_key(session)
    }

    fn pump(&mut self) -> bool {
        while let Some(command) = self.outbox.pop_front() {
            if let Err(command) = self.commands.push(command) {
                self.outbox.push_front(command);
                break;
            }
        }
        let wanted: Vec<SessionKey> = self.sessions.iter().filter(|(_, entry)| entry.stop == Stop::Wanted).map(|(key, _)| key.clone()).collect();
        for key in wanted {
            if self.commands.free().saturating_sub(self.outbox.len()) <= self.reserved { break; }
            let entry = self.sessions.get_mut(&key).expect("listed above");
            entry.stop = Stop::Sent;
            let slot = entry.slot;
            self.send(Command::Remove { slot, incarnation: key.incarnation });
        }
        // This holder cannot be sent to a worker: until the one-time device
        // handoff the host thread alone may drive its engine.
        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            if let Some(engine) = engine.as_mut() { engine.idle(); }
        }
        self.latest = self.telemetry.latest().clone();
        self.settle_exchanges();
        if let Ok(mut view) = self.clocks.try_borrow_mut() {
            view.callbacks = self.latest.callbacks;
            view.frames = self.latest.frames;
            view.rate = self.latest.rate;
            view.slots.clear();
            view.slots.extend_from_slice(&self.latest.slots);
            view.sessions.clear();
            for (key, entry) in &self.sessions {
                let gens = entry.gens.iter().map(|gen| (gen.generation, gen.layout.clone(), gen.board.clone())).collect();
                view.sessions.push((key.clone(), entry.slot, gens));
            }
        }
        // A bounded drain: never wait for the callback, and never starve UI work
        // if a device continues publishing while this pump runs.
        for _ in 0..AUDIO_PROBE_QUEUE {
            let Some(packet) = self.audio_probes.pop() else { break };
            self.latest_audio.insert((packet.slot, packet.port), packet);
        }
        // Every page receipt answers a route of the ledger, so taking them
        // all never exceeds it; the route stays until its receipt is taken.
        if let Ok(mut ledger) = self.pages.try_borrow_mut() {
            let PageLedger { routes, ready, .. } = &mut *ledger;
            while let Some(back) = self.page_returns.pop() {
                let index = back.origin.route as usize;
                if let Some(route) = routes.get_mut(index).and_then(Option::as_mut)
                    .filter(|route| route.origin == back.origin && route.kind == back.kind && route.ready.is_none())
                {
                    route.ready = Some(back);
                    ready.push_back(index as u16);
                }
            }
        }
        // A `Grow`'s answer carries the engine's old slots and writer:
        // dropped here with the rest.
        while let Some(Retired { slot, incarnation, schedule, removed, rejected, track, .. }) = self.retired.pop() {
            let key = self.sessions.iter().find(|(key, entry)| entry.slot == slot && key.incarnation == incarnation).map(|(key, _)| key.clone());
            if let (Some(key), Some(TrackReturn { target, continuity, sequence, .. })) = (&key, &track) {
                if let Some(entry) = self.sessions.get_mut(key) {
                    if let Some(host) = deck_name(entry, *target, *continuity).and_then(|node| entry.decks.get_mut(&(node, *continuity))) {
                        host.acknowledge(*sequence, match rejected {
                            None => Ok(()),
                            Some(reason) => Err(format!("audio deck refused the track: {reason:?}")),
                        });
                    }
                }
            }
            if let (Some(key), Some(schedule)) = (&key, &schedule) {
                if let Some(entry) = self.sessions.get_mut(key) {
                    // A swap that never happened: its command is refused
                    // now, on the deck the schedule's own target table
                    // names, by target id and continuity.
                    for (target, state, continuity) in schedule.triggers.iter() {
                        let Some(NodeState::Ext(deck)) = schedule.states.get(*state as usize) else { continue };
                        let Some((sequence, _)) = &deck.next else { continue };
                        if let Some(host) = deck_name(entry, *target, *continuity).and_then(|node| entry.decks.get_mut(&(node, *continuity))) {
                            host.acknowledge(*sequence, Err("the deck was retired before the track was taken".to_string()));
                        }
                    }
                    entry.gens.retain(|gen| gen.generation != schedule.generation);
                    if let Some(reason) = rejected {
                        entry.rejected = Some((Generation(schedule.generation), format!("audio activation refused: {reason:?}")));
                    }
                    // A deck no held or staged generation names any more,
                    // with nothing in flight, is gone with its state.
                    let layouts: Vec<&Arc<Layout>> = entry.gens.iter().map(|gen| &gen.layout).chain(entry.staged.iter().map(|(gen, _)| &gen.layout)).collect();
                    entry.decks.retain(|(node, continuity), host| {
                        !host.pending.is_empty()
                            || layouts.iter().any(|layout| layout.triggers.get(node).is_some_and(|(_, held)| held == continuity))
                    });
                }
            }
            // The schedule, its synths, its buffers and any returned clip
            // are freed here, on the host's thread.
            drop(track);
            drop(schedule);
            if removed {
                if let Some(key) = key {
                    self.sessions.remove(&key);
                }
            }
        }
        let format = self.shared.format.load(Ordering::Acquire);
        let mut changed = false;
        if format as u32 != 0 && format != self.context_format {
            changed = self.context_format != 0;
            self.context_format = format;
        }
        for (key, entry) in &mut self.sessions {
            let slot = &self.latest.slots[entry.slot as usize];
            if entry.stop != Stop::No || key.incarnation != slot.incarnation { continue; }
            if let Some(gen) = entry.gens.iter_mut().find(|g| g.generation == slot.generation) {
                if let Some(line) = gen.layout.take_report() { eprintln!("{line}"); }
                // Another context change would cancel the prepare this one requested.
                let stale = gen.layout.nested_changed();
                changed |= stale && !gen.nested_change_reported;
                gen.nested_change_reported = stale;
            }
        }
        changed
    }
}

/// The qualified node a target id and continuity name in any generation
/// the session holds or has staged.
fn deck_name(entry: &AudioSession, target: u32, continuity: u64) -> Option<String> {
    entry.gens.iter().map(|gen| &gen.layout).chain(entry.staged.iter().map(|(gen, _)| &gen.layout))
        .flat_map(|layout| layout.triggers.iter())
        .find(|(_, (id, held))| *id == target && *held == continuity)
        .map(|(node, _)| node.clone())
}

/// Immutable node kind plus the registered parameter name defines the units
/// and range. A reused text address alone is not endpoint compatibility.
fn compatible_control(a: &Layout, b: &Layout, name: &str) -> bool {
    same_control_meaning(a, b, name) && a.drivers.get(name) == b.drivers.get(name)
        && a.controls.get(name).zip(b.controls.get(name))
            .is_some_and(|((_, left), (_, right))| left.to_bits() == right.to_bits())
}

/// Driver-independent identity for remembered manual values. Effective
/// values and writes use `compatible_control` and also require the same wire.
fn same_control_meaning(a: &Layout, b: &Layout, name: &str) -> bool {
    let Some((node, param)) = name.rsplit_once('.') else { return false; };
    fn kind<'a>(layout: &'a Layout, node: &str) -> Option<&'a str> {
        layout.nodes.iter().find(|entry| entry.id == node).map(|entry| entry.type_name.as_str())
    }
    match (kind(a, node), kind(b, node)) {
        (Some(kind_a), Some(kind_b)) => kind_a == kind_b && control_range(kind_a, param).is_some(),
        _ => false,
    }
}

#[cfg(test)]
mod exchange_host_tests {
    use super::*;

    fn exchange() -> (u64, (String, u64), (String, u64)) {
        (3, ("deck_a".into(), 1), ("deck_b".into(), 1))
    }

    #[test]
    fn transport_writes_to_either_exchanging_deck_wait_and_channel_writes_do_not() {
        let pending = exchange();
        for param in EXCHANGED_CONTROLS {
            assert!(exchange_fences(Some(&pending), "deck_a", "AudioDeck", param), "{param}");
            assert!(exchange_fences(Some(&pending), "deck_b", "AudioDeck", param), "{param}");
        }
        // The channel (gain, crossfade, stems) stays with its deck and is not fenced.
        assert!(!exchange_fences(Some(&pending), "deck_a_gain", "Gain", "gain"));
        assert!(!exchange_fences(Some(&pending), "deck_a_fade", "Gain", "mute"));
        assert!(!exchange_fences(Some(&pending), "deck_c", "AudioDeck", "play"));
        assert!(!exchange_fences(None, "deck_a", "AudioDeck", "play"));
    }

    #[test]
    fn an_imported_sequence_can_never_be_sent_again_on_its_new_deck() {
        // A played sequence 2 (its highest sent); B only ever sent 1.
        let mut a = DeckHost { highest_sent: 2, accepted: 2, active: Some(2), ..Default::default() };
        let mut b = DeckHost { highest_sent: 1, accepted: 1, active: Some(1), ..Default::default() };
        let (sent, accepted) = (a.highest_sent.max(b.highest_sent), a.accepted.max(b.accepted));
        let (from_a, from_b) = ((a.active.take(), std::mem::take(&mut a.active_source)), (b.active.take(), std::mem::take(&mut b.active_source)));
        a.import(from_b.0, from_b.1, sent, accepted);
        b.import(from_a.0, from_a.1, sent, accepted);
        assert_eq!(b.active, Some(2));
        assert_eq!(a.active, Some(1));
        // The admission rule is `sequence > highest_sent`: B refuses 2 now.
        assert!(2 <= b.highest_sent && 2 <= a.highest_sent);
        assert_eq!(b.accepted, 2);
    }
}
