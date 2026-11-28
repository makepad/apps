//! Contract C6 (E4 "live feedback"): the snapshot every view reads and the actions views emit.
//!
//! The host (`ElecHost`) owns the design and the engines; it publishes one `DesignSnapshot` per
//! change (an `Arc` swap; views never wait). Views read it, draw, and answer input by emitting
//! `ViewAction`s; they never edit the model or touch source text.
//!
//! Previews: while a preview is live (a drag, a router proposal, an AI dry run), `design` is the
//! committed state shown as the ghost and `preview` the live state drawn on top. Every engine
//! result is tagged with the state and generation it was computed from, so a view can show a
//! result that is still catching up as such (`stale`) instead of mixing states.
//!
//! The source map (C5) joins every model item to its source text (`None` until the documents
//! evaluate).
//!
//! Cost: building a snapshot is a handful of `Arc` clones; nothing here copies the model.

use crate::states::{SheetIndex, StatesOf};
use fab_elec_board::{Connectivity, RuleMatrix, ZoneFill};
use fab_elec_cam::Export;
use fab_elec_circuit::Violation;
pub use fab_elec_circuit::CheckKind;
use crate::live::sim::{LiveScreen, SimLive};
use fab_elec_core::{BoardId, CompId, CompRef, Diag, DocId, ItemId, NetId, Sel};
use fab_elec_design::{By, Design, DesignOp, Focus, Place, SystemNet, TextEdit, TimeCursor};
use fab_elec_lang::sys_sim::SimSettings;
use fab_elec_lang::{SourceMap, UnitState};
use fab_elec_route::{GestureId, GestureSample, JobId, JobStatus, Method, Reason, RouteProposal, RouteScope};
use makepad_widgets_core::Texture;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// A preview tag unique across every pane of the process.
pub fn next_tag() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Relaxed)
}

/// Which state an engine result was computed from.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum StateOf {
    Committed,
    Preview,
}

/// An engine result with what it was computed from.
#[derive(Clone, PartialEq, Debug)]
pub struct Tagged<T> {
    /// The unit, for per-board results (`None` for design-wide ones).
    pub unit: Option<BoardId>,
    pub of: StateOf,
    /// The board's (or the design's) generation it was computed at.
    pub generation: u64,
    pub value: T,
}

/// The stages still catching up with the latest state.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct StageFlags(pub u16);

impl StageFlags {
    pub const CONNECT: StageFlags = StageFlags(1);
    pub const DRC: StageFlags = StageFlags(2);
    pub const FILLS: StageFlags = StageFlags(4);
    pub const MECH: StageFlags = StageFlags(8);
    pub const CABLES: StageFlags = StageFlags(16);
    pub const SYSNETS: StageFlags = StageFlags(32);
    /// The fabrication export (run while a proof pane wants it).
    pub const CAM: StageFlags = StageFlags(64);
    /// The engine strand has commands waiting longer than a frame (catching up).
    pub const ENGINE: StageFlags = StageFlags(128);
    /// The documents changed since the design was evaluated (an evaluation runs or waits): views show the units stale.
    pub const EVAL: StageFlags = StageFlags(256);
    pub const NONE: StageFlags = StageFlags(0);
    pub fn has(self, s: StageFlags) -> bool {
        self.0 & s.0 == s.0 && s.0 != 0
    }
    pub fn with(self, s: StageFlags) -> StageFlags {
        StageFlags(self.0 | s.0)
    }
    pub fn without(self, s: StageFlags) -> StageFlags {
        StageFlags(self.0 & !s.0)
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// The newest edit the engine refused, kept until the next accepted edit.
#[derive(Clone, PartialEq, Debug)]
pub struct Refusal {
    /// The engine's refusal count at this refusal: new with every refusal, so a view shows each
    /// one once. (The project step counter numbers undo steps only.)
    pub seq: u64,
    /// The refused action's label, as its view wrote it: the view finds its own refusal by it.
    pub label: String,
    /// The refused op's kind, short: "board", "circuit", "bench", "system" (the first op's
    /// DesignOp variant), "undo" or "redo".
    pub op: &'static str,
    /// The full diagnostic for the log, tickets and tools.
    pub text: String,
    /// The sentence the host prepared for the person editing the design.
    pub reason: String,
    /// The source range the refusal is about (a `Sel::Source`), when it names one: the refused
    /// statement, or where the language located its refusal.
    pub site: Option<fab_elec_core::Sel>,
    /// The refused action's tag: a preview sample's, an invoke's or a typed edit's
    /// (`ViewAction::Text`); other edits answer their caller's ticket.
    pub sample: Option<u64>,
}

/// The newest accepted commit, so a pane can focus what it made.
#[derive(Clone, PartialEq, Debug)]
pub struct Committed {
    /// The history entry's project step number.
    pub seq: u64,
    /// The committed sample's tag, or None for ordinary commits and tool calls.
    pub tag: Option<u64>,
    /// The author recorded on the history entry.
    pub by: By,
    /// New parts, nets or items written by the commit.
    pub created: Vec<Sel>,
}

/// The project's documents as the engine holds them (C5): what a code pane shows and the
/// Checks pane's code rows read. A new Arc with each text change or evaluation; O(documents +
/// units + notes) to build, nothing per frame.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DocsView {
    /// Every project document, in document order.
    pub texts: Vec<DocText>,
    /// Every unit's evaluation: built, kept at its last good lowering, or failed, with its
    /// located diags.
    pub units: Vec<(BoardId, String, UnitState)>,
    /// Warnings that do not stop the design; after a load where no unit built, its diags.
    pub notes: Vec<Diag>,
    /// The project's run settings, as its system document's `sim` statement says them
    /// (`fab_elec_lang::sys_sim`); none chosen without one.
    pub sim: SimSettings,
    /// The system document, where the run settings are written (None: the project has none).
    pub system: Option<DocId>,
}

/// One document's text as the engine holds it.
#[derive(Clone, PartialEq, Debug)]
pub struct DocText {
    pub doc: DocId,
    /// The file name, relative to the project: `ledctl.pcb.splash`.
    pub name: String,
    pub text: Arc<str>,
    /// One more with each change of the text since the documents loaded.
    pub rev: u64,
}

/// How a route job ended; the state stays shown until the next job starts or the engine reloads.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum JobEnd {
    /// The job finished; its last proposal is committed.
    Finished,
    /// The person stopped it; what it had routed is committed.
    Stopped,
}

/// A running or ended route job, for the board's status line.
/// Finished and stopped jobs commit what they routed; discarded jobs leave no state.
#[derive(Clone, PartialEq, Debug)]
pub struct RouteJobState {
    pub status: JobStatus,
    /// The board's name of `status.current`.
    pub current_name: Option<Arc<str>>,
    pub end: Option<JobEnd>,
    /// The nets the job was asked to route: the board's line names those a finished job left open.
    pub scope: Arc<RouteScope>,
    /// How the job shapes its copper: an organic job's line says so from its first moment.
    pub method: Method,
    /// Why each net the job's last proposal left open stays open (empty until it ends).
    pub open: Arc<Vec<(NetId, Reason)>>,
}

/// The case checks a snapshot's case findings come from, for one board (`Cmd::CaseFindings`):
/// the board generation they ran on, the app's run (one per plate build it checks) and whether
/// the plate had case parts for the board. A board without one has not been case checked.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CaseHeld {
    pub unit: BoardId,
    pub generation: u64,
    pub run: u64,
    pub cased: bool,
}

/// A decision names one proposal across all loads and engines in the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProposalId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProposalAuthor {
    Local(By),
    /// The person checking and accepting a peer's text owns the local history entry.
    /// The journal will record the peer label in slice 3.
    Peer { label: Arc<str> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalFinding {
    pub unit: String,
    /// The check's family as published (erc, drc, factory, case, system, route, custom).
    pub family: &'static str,
    pub kind: CheckKind,
    pub severity: fab_elec_core::Severity,
    pub title: String,
    pub message: String,
    pub items: String,
    pub fix: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProposalChecks {
    pub counts: std::collections::BTreeMap<(CheckKind, fab_elec_core::Severity), usize>,
    pub pending: bool,
    /// Every non-awaiting finding, errors first. Pages and display limits belong to the readers.
    pub findings: Vec<ProposalFinding>,
    pub pending_labels: Vec<String>,
}

/// Requirement verdicts for the whole proposed project. Detail entries are (name, reason)
/// in document order, so Review can explain each failed or unavailable measurement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReqSummary {
    pub pass: usize,
    pub fail: usize,
    pub unknown: usize,
    pub waived: usize,
    pub failures: Vec<(String, String)>,
    pub unjudged: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProposalState {
    Unchecked,
    Checking,
    Ready,
    Stale(String),
    Refused(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalDoc {
    pub name: String,
    pub before: Option<Arc<str>>,
    pub after: Option<Arc<str>>,
}

/// The pool start of one verification, shared by its job and all views of that run.
#[derive(Debug, Default)]
pub struct RunClock(AtomicU64);

static RUN_BASE: OnceLock<Instant> = OnceLock::new();

impl RunClock {
    pub fn started(&self) -> Option<Instant> {
        // Store offset + 1 so even a job starting exactly at the base has a timestamp.
        let nanos = self.0.load(Relaxed).checked_sub(1)?;
        Some(*RUN_BASE.get()? + Duration::from_nanos(nanos))
    }

    pub fn start(&self) {
        let base = RUN_BASE.get_or_init(Instant::now);
        let start = base.elapsed().as_nanos() as u64 + 1;
        if self.0.compare_exchange(0, start, Relaxed, Relaxed).is_ok() {
            makepad_widgets_core::SignalToUI::set_ui_signal();
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProposalBoard {
    pub unit: BoardId,
    pub fills: Arc<Vec<ZoneFill>>,
    /// Sorted zone ids whose published fills still owe a refill.
    pub pending_zones: Arc<[ItemId]>,
}

#[derive(Clone, Debug)]
pub struct ProposalView {
    pub id: ProposalId,
    pub shown: u64,
    pub slot: Arc<str>,
    pub label: Arc<str>,
    pub by: ProposalAuthor,
    pub state: ProposalState,
    pub run: Arc<RunClock>,
    pub docs: Arc<Vec<ProposalDoc>>,
    pub design: Option<Arc<Design>>,
    /// Ready geometry, shared with the held candidate. Both collections clear with the design.
    pub boards: Arc<Vec<ProposalBoard>>,
    pub sheets: Arc<std::collections::BTreeMap<BoardId, Arc<SheetIndex>>>,
    pub created: Arc<Vec<Sel>>,
    /// None until the first verification. While rechecking, the last summary is kept.
    pub checks: Option<Arc<ProposalChecks>>,
    /// Published with checks and retained through a recheck. None without a requirements document.
    pub requirements: Option<Arc<ReqSummary>>,
}

/// What every view reads.
#[derive(Clone, Debug)]
pub struct DesignSnapshot {
    /// The committed design (the ghost while a preview is live).
    pub design: Arc<Design>,
    /// The engine's factories in offer order, with the same capabilities used for the board rules.
    pub profiles: Arc<Vec<fab_elec_cam::profile_doc::ProfileDef>>,
    /// Located errors reading the current project factory documents.
    pub profile_diags: Arc<Vec<Diag>>,
    /// The live preview, drawn over the ghost.
    pub preview: Option<Arc<Design>>,
    /// Held document sets, independent of the live gesture.
    pub proposals: Arc<Vec<Arc<ProposalView>>>,
    /// The tag of the sample shown by the live preview.
    pub preview_tag: Option<u64>,
    /// The live preview's author, for its accent styling.
    pub preview_by: Option<By>,
    /// Items added by the live preview, for its accent outline.
    pub created: Vec<Sel>,
    /// The newest accepted commit, retained through undo and redo.
    pub last_commit: Option<Arc<Committed>>,
    /// Per unit, the committed sheet rows (`StateOf::Committed`, the design's generation), and for
    /// each unit whose preview circuit differs, the preview circuit's rows (`StateOf::Preview`,
    /// the preview's generation). Read a unit's shown rows with [`DesignSnapshot::of`].
    pub sheets: Vec<Tagged<Arc<SheetIndex>>>,
    /// The router proposal the live preview shows (its metrics, collisions, open nets and source),
    /// when the preview is one.
    pub proposal: Option<Arc<RouteProposal>>,
    /// The host's route job, overlaid by the Driver on every shown snapshot; the engine always publishes None.
    pub job: Option<Arc<RouteJobState>>,
    /// Per board unit.
    pub conn: Vec<Tagged<Arc<Connectivity>>>,
    pub rules: Vec<Tagged<Arc<RuleMatrix>>>,
    pub fills: Vec<Tagged<Arc<Vec<ZoneFill>>>>,
    pub sysnets: Option<Tagged<Arc<Vec<SystemNet>>>>,
    /// Per board unit, the fabrication export with its read-back, while a proof pane wants it.
    pub export: Vec<Tagged<Arc<Export>>>,
    /// Every check's findings (ERC, DRC, system ERC, the rest), sorted by id.
    pub violations: Tagged<Arc<Vec<Violation>>>,
    /// Per case-checked board, the case checks its case findings in `violations` come from, by unit.
    pub cases: Arc<Vec<CaseHeld>>,
    /// Ids of `violations` that await a zone's refill: they name a zone whose fill is still pending in the shown
    /// results. Views show them as pending, not as violations. Sorted.
    pub awaiting: Arc<Vec<u64>>,
    /// Board units whose connectivity and DRC wait for their whole-board checks (a design just
    /// loaded, a new unit): their `conn` is the board's without its pours, and `violations` holds
    /// none of their DRC. Views show such a unit as checking, not its ratsnest or unrouted count.
    /// Sorted; empty once every unit's checks merged.
    pub checking: Vec<BoardId>,
    /// The newest refused edit, kept until the next accepted edit.
    pub last_refusal: Option<Arc<Refusal>>,
    pub stale: StageFlags,
    /// The documents' revision the design was evaluated from.
    pub head: String,
    /// Every evaluated bench's view (`fab_elec_lang::bench_views`), in bench order: what each source
    /// card shows and may edit. Built on the pool beside each evaluation; the same Arc until an
    /// evaluation changes a view. Empty for a design without documents.
    pub benches: Arc<Vec<fab_elec_lang::BenchView>>,
    /// Where each item of `design` came from in the source text (C5), kept current through text edits.
    pub source_map: Option<Arc<SourceMap>>,
    /// The documents `design` is written in; None for a design without documents.
    pub docs: Option<Arc<DocsView>>,
    /// the Library panel's entries for the documents' definitions (G5): the same Arc until a
    /// definition document changes; None for a design without documents
    pub catalog: Option<Arc<fab_elec_lang::Catalog>>,
}

impl DesignSnapshot {
    /// The state views edit and the AI checks: the preview when one is live.
    pub fn live(&self) -> &Arc<Design> {
        self.preview.as_ref().unwrap_or(&self.design)
    }
    /// Whether `unit` waits for its whole-board checks (`checking`). O(log units).
    pub fn checking(&self, unit: BoardId) -> bool {
        self.checking.binary_search(&unit).is_ok()
    }
    /// The newest per-unit result of `list` for `unit`, preferring one computed from the live state.
    pub fn of<'a, T>(&self, list: &'a [Tagged<T>], unit: BoardId) -> Option<&'a Tagged<T>> {
        let want = if self.preview.is_some() { StateOf::Preview } else { StateOf::Committed };
        let mine = list.iter().filter(|t| t.unit == Some(unit));
        mine.clone().find(|t| t.of == want).or_else(|| mine.max_by_key(|t| t.generation))
    }
}

/// The chosen row, including a bench with a failure to show. A missing name follows the first
/// bench that built or kept its last good lowering.
pub(crate) fn bench_index(benches: &[fab_elec_lang::BenchView], choice: Option<&str>) -> Option<usize> {
    choice.and_then(|name| benches.iter().position(|bench| bench.out.name == name))
        .or_else(|| benches.iter().position(|bench| !matches!(bench.out.state, UnitState::Failed(_))))
}

/// The bench whose scope and stimulus the host runs. A chosen bench that failed runs nothing.
pub fn run_bench<'a>(benches: &'a [fab_elec_lang::BenchView], choice: Option<&str>) -> Option<&'a fab_elec_lang::BenchView> {
    let bench = &benches[bench_index(benches, choice)?];
    (!matches!(bench.out.state, UnitState::Failed(_))).then_some(bench)
}

/// The catalog spelling of a gesture, captured while its input still names the target.
#[derive(Clone, PartialEq, Debug)]
pub struct Invocation {
    pub name: &'static str,
    pub args: Vec<(String, String)>,
}

/// Standalone electronics canvases consume the original Ops and CommitPreview actions.
#[derive(Default)]
pub struct CapturePlannerGestures;

impl Invocation {
    pub fn new(name: &'static str, unit: &str, fields: impl IntoIterator<Item = (&'static str, String)>) -> Self {
        let mut args = vec![("unit".into(), unit.to_owned())];
        args.extend(fields.into_iter().map(|(k, v)| (k.to_owned(), v)));
        Self { name, args }
    }

    pub fn send(cx: &mut makepad_widgets_core::Cx, uid: makepad_widgets_core::WidgetUid, action: ViewAction) {
        use makepad_widgets_core::WidgetActionCxExt;
        let action = match action {
            ViewAction::Planned { action, .. } if !cx.has_global::<CapturePlannerGestures>() => match *action {
                ViewAction::PlanRefused(_) => return,
                ViewAction::Land(_) => ViewAction::CommitPreview,
                action => action,
            },
            ViewAction::PlanRefused(_) => return,
            action => action,
        };
        cx.widget_action(uid, action);
    }

    pub fn attach(self, action: ViewAction) -> ViewAction {
        ViewAction::Planned { call: self, action: Box::new(action) }
    }

    /// Use the same spelling the resolver can actually map back to this component.
    pub fn part(unit: &fab_elec_design::Unit, id: CompId) -> Option<&str> {
        let comp = unit.circuit.comps.get(id.ix())?;
        [comp.refdes.as_str(), comp.path.as_str()].into_iter().find(|name| {
            !name.is_empty() && !name.contains(|c: char| c == ',' || c.is_whitespace() || c == '"')
                && !(name.starts_with('[') && name.ends_with(']'))
                && unit.circuit.comps.iter().find(|c| c.refdes == *name || c.path == *name).is_some_and(|c| c.id == id)
        })
    }
}

/// What a view emits; the host applies it (views never touch the model or the text).
#[derive(Clone, PartialEq, Debug)]
pub enum ViewAction {
    ProposalDecision { id: ProposalId, shown: u64, accept: bool },
    CheckProposal(ProposalId),
    /// Keep the original pane action for hosts that operate without a journal.
    Planned { call: Invocation, action: Box<ViewAction> },
    /// The planner answered the press locally. Nothing is submitted to the engine.
    PlanRefused(String),
    /// A released schematic wire names the exact preview it is committing.
    Land(u64),
    /// Commit edits as one undo step.
    Ops { ops: Vec<DesignOp>, place: Vec<Place>, label: String },
    /// Show edits on a scratch copy (replacing any live preview); nothing is committed.
    Preview {
        tag: u64, ops: Vec<DesignOp>, place: Vec<Place>, label: String,
        /// The router's `Reason::SharpCorner` findings that outlive the answer as session
        /// notes on this edit. Empty for other previews.
        notes: Vec<(NetId, Reason)>,
    },
    /// A board drag's newest sample for the router (route and segment drags). The host relays
    /// it to the route worker's latest-value slot once the worker holds the unit's base; each
    /// answer is previewed under the gesture's tag and written to `ElecLive.gesture`.
    Gesture(GestureSample),
    /// The drag ended or was cancelled: the router forgets the gesture. On a release the pane
    /// sends it after `CommitPreview`.
    EndGesture(GestureId),
    /// The person routes `nets` of board `unit` (empty: every unrouted net) as a route job by
    /// `method`; a running job is stopped first. The job's progress and outcome show in the
    /// snapshot's `job`. A `reroute` may move the board's routed copper in their way.
    Route { unit: BoardId, nets: Vec<NetId>, method: fab_elec_route::Method, reroute: bool },
    /// The person places parts of board `unit` as a place job (`elec.place` with `@place`): the
    /// `parts` (empty: the circuit's parts the board does not place yet and those outside its
    /// outline), or with `all` every unlocked part. The panel parts standing on the board stay
    /// where they are (`plan::held_panel`) unless `free_panel`. A running job is stopped first;
    /// its progress and outcome show in the snapshot's `job`, as a route's do, and Stop and
    /// Discard work alike.
    Place { unit: BoardId, parts: Vec<CompId>, all: bool, free_panel: bool },
    /// Stop the running route job and keep what it has routed so far as one undo step.
    StopRoute(JobId),
    /// Stop the running route job and drop what it routed: nothing is committed.
    DiscardRoute(JobId),
    /// Commit the live preview as one undo step.
    CommitPreview,
    /// Drop the live preview.
    CancelPreview,
    /// Typed text of one document, addressed to the revision the editor read (`DocText::rev`):
    /// a document at another revision refuses it (a `Refusal` of op "text" whose `sample` is
    /// `tag`), and the editor reads the text again. Changes of one `group` that follow each other
    /// merge into one undo step, named `label`.
    Text { doc: DocId, base_rev: u64, edits: Vec<TextEdit>, group: Option<u64>, label: String, tag: u64 },
    /// The one broadcast every view answers.
    Focus(Focus),
    /// Nothing is selected any more (a click on an empty spot of the sheet): every view clears
    /// the selection a `Focus` made.
    Unfocus,
    Hover(Option<Sel>),
    /// The shared time cursor (simulation, motion, replay).
    Cursor(TimeCursor),
}

/// What every electronics view reads from its host at draw time: a Cx global the host updates
/// whenever the shown snapshot changes (`cx.get_global::<ElecLive>()`). Views clone what they keep
/// (an `Arc` or texture handle), never hold the global. The host writes both board and schematic
/// item states (`shared/item-state.md`); schematic rows come from ESCH's SheetRows.
pub struct ElecLive {
    pub snap: Arc<DesignSnapshot>,
    /// Same-plate reload notice, dismissed locally by the Review strip.
    pub ended_proposals: Option<Arc<str>>,
    /// The global still holds the previous engine's snapshot until the replacement is published.
    pub proposals_reloading: bool,
    /// The person's chosen design comparison, shared by Board panes.
    pub previewing: Option<ProposalId>,
    /// The router's newest answer to the live gesture, written by the host in the frame it
    /// arrives; replaced answers are retired to the engine strand. None outside a route drag.
    pub gesture: Option<Arc<RouteProposal>>,
    /// The live bench (`shared/live-rows.md`): the row textures, the numbering and the segment-row
    /// allocator, written by the simulation host each frame.
    pub sim: SimLive,
    /// The item-state texture of each geometry owner, written by the host once a frame.
    pub states: Vec<(StatesOf, Texture)>,
    /// The name of what is open (the project, as the app's caption names it), for a view that
    /// says what it shows; empty until the app names it.
    pub title: String,
    /// The bench chosen for this project. None follows the first usable bench.
    pub chosen_bench: Option<Arc<str>>,
    /// The bench's counted check as the app last put it up (EXPECTATIONS' Check bench,
    /// `elec.bench.run`); None before the first.
    pub check: Option<Arc<crate::instruments::bench_setup::BenchCheck>>,
    /// The saved trace the Scope tab shows in place of the live run (`elec.capture.open`, the
    /// Scope menu's saved traces); None shows the run.
    pub opened: Option<Arc<crate::instruments::tools::capture::OpenedTrace>>,
    /// The saved traces in the project's captures folder, project-relative, newest first: listed
    /// by the app as the Scope menu opens (`TraceAction::List`).
    pub traces: Arc<Vec<String>>,
    /// The unit the last Schematic or Board drew: what the Checks editor lists. Each pane writes
    /// it as it draws, only when it changes.
    pub shown_unit: Option<BoardId>,
    /// The project's requirements, judged on the pool by the app; None while the project has no
    /// requirements document. The same Arc until a judged input changes.
    pub requirements: Option<Arc<ReqShown>>,
    pub bom: Option<Arc<crate::bom::Shown>>,
    pub imported_catalog: Arc<Vec<fab_elec_lang::CatalogEntry>>,
    pub price_book: Option<Arc<fab_elec_bom::prices::Library>>,
}

/// The project's requirements as the Requirements editor and the bar's chip show them
/// (`fab_elec_req::judge` over what the design, its checks and the app's producers measure).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReqShown {
    pub report: fab_elec_req::Report,
    /// Per row of `report`, the document it is written in and its 1-based line.
    pub at: Vec<(Arc<str>, usize)>,
    /// Source positions for the unmeasured wishes and the project's brief.
    pub wish_at: Vec<(Arc<str>, usize)>,
    pub brief_at: Option<(Arc<str>, usize)>,
    /// The documents' mistakes and notes in words, each led by its document and line.
    pub mistakes: Vec<String>,
}

/// An electronics view asks its host to bring one of the electronics editors forward: a pane's
/// Check button the Checks, the Transport's controls header the Board controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElecPanel {
    Library,
    Checks,
    BoardControls,
}

impl ElecLive {
    pub fn new(snap: Arc<DesignSnapshot>) -> ElecLive {
        ElecLive { snap, ended_proposals: None, proposals_reloading: false, previewing: None, gesture: None, sim: SimLive::default(), states: Vec::new(), title: String::new(), chosen_bench: None, check: None, opened: None, traces: Arc::default(), shown_unit: None, requirements: None, bom: None, imported_catalog: Arc::default(), price_book: None }
    }

    fn preview_in<'a>(&self, snap: &'a DesignSnapshot, chosen: Option<ProposalId>) -> Option<&'a Arc<ProposalView>> {
        let id = chosen?;
        if self.proposals_reloading || snap.preview.is_some() { return None; }
        snap.proposals.iter().find(|p| p.id == id && p.state == ProposalState::Ready && p.design.is_some())
    }

    /// Select a comparison from the strip, or return from one of its panes. The old source's
    /// finished boards can be adopted even when its last Board pane is hidden.
    pub fn select_preview(cx: &mut makepad_widgets_core::Cx, next: Option<ProposalId>) {
        let live = cx.get_global::<ElecLive>();
        if live.previewing != next && (next.is_none() || live.preview_in(&live.snap, next).is_some()) {
            live.previewing = next;
            let snap = live.snap.clone();
            crate::board::BoardDraws::keep_preview(cx, next, &snap);
            cx.redraw_all();
        }
    }

    pub fn previewed(&self) -> Option<&Arc<ProposalView>> {
        self.preview_in(&self.snap, self.previewing)
    }

    /// Check every incoming publication, including intermediate pictures in one UI event.
    pub fn end_invalid_preview(&mut self, snap: &DesignSnapshot) -> bool {
        if self.previewing.is_some() && self.preview_in(snap, self.previewing).is_none() {
            self.previewing = None;
            return true;
        }
        false
    }

    pub fn run_bench(&self) -> Option<&fab_elec_lang::BenchView> {
        run_bench(&self.snap.benches, self.chosen_bench.as_deref())
    }

    /// The sheet and its Library follow the run's circuit. A bench with its own circuit has
    /// no design sheet, so keep the previous unit when it still belongs to this design.
    pub(crate) fn sheet_unit(&self, previous: Option<(BoardId, &str)>) -> Option<usize> {
        let units = &self.snap.live().units;
        self.run_bench().and_then(|bench| units.iter().position(|u| u.id == bench.out.scope_id() && (!u.circuit.comps.is_empty() || u.circuit.tree.len() > 1)))
            .or_else(|| previous.and_then(|(id, name)| units.iter().position(|u| u.id == id && u.name == name)))
            .or_else(|| units.iter().position(|u| !u.circuit.comps.is_empty() || u.circuit.tree.len() > 1))
    }

    /// The host's item-state texture for one owner. Cost: O(owners), a handful per design.
    pub fn item_states(&self, of: StatesOf) -> Option<&Texture> {
        self.states.iter().find(|(owner, _)| *owner == of).map(|(_, texture)| texture)
    }

    /// A live knob's name as every view shows it: its pot's `label` field (`DECAY`), else its
    /// reference. Cost: O(parts of its board).
    pub fn knob_name(&self, comp: CompRef) -> String {
        let design = self.snap.live();
        let part = design.unit(comp.board).and_then(|unit| unit.circuit.comps.iter().find(|c| c.id == comp.comp));
        part.map_or_else(|| "pot".to_string(), |c| c.fields.get("label").map_or_else(|| c.refdes.clone(), str::to_string))
    }

    /// The live display panel of one part, None for a part that is not one. Cost: O(panels).
    pub fn screen(&self, comp: CompRef) -> Option<&LiveScreen> {
        self.sim.screens.iter().find(|screen| screen.comp == comp)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn snapshot(design: Arc<fab_elec_design::Design>) -> DesignSnapshot {
        DesignSnapshot {
            proposals: Default::default(),
            profiles: Arc::default(), profile_diags: Arc::default(),
            design, preview: None, preview_tag: None, preview_by: None, created: vec![], last_commit: None,
            sheets: vec![], proposal: None, job: None, conn: vec![], rules: vec![], fills: vec![], sysnets: None, export: vec![], benches: Arc::default(),
            violations: Tagged { unit: None, of: StateOf::Committed, generation: 0, value: Arc::new(vec![]) },
            awaiting: Arc::new(vec![]), cases: Arc::default(), checking: vec![], last_refusal: None, stale: StageFlags::default(), head: String::new(),
            source_map: None, docs: None, catalog: None,
        }
    }

    #[test]
    fn preview_requires_continuously_ready_work() {
        let design = Arc::new(Design::default());
        let mut snap = snapshot(design.clone());
        let id = ProposalId(1);
        let view = Arc::new(ProposalView { id, shown: 1, slot: "stage".into(), label: "Stage".into(),
            by: ProposalAuthor::Local(By::Person), state: ProposalState::Ready, design: Some(design.clone()),
            run: Arc::default(), docs: Arc::default(), boards: Arc::default(), sheets: Arc::default(),
            created: Arc::default(), checks: None, requirements: None });
        snap.proposals = Arc::new(vec![view.clone()]);
        let mut live = ElecLive::new(Arc::new(snap.clone()));
        live.previewing = Some(id);
        assert!(Arc::ptr_eq(live.previewed().unwrap(), &view) && !live.end_invalid_preview(&snap),
            "a ready proposal retains the chosen picture");
        let mut ends = |next: &DesignSnapshot, reloading: bool, why: &str| {
            live.previewing = Some(id);
            live.proposals_reloading = reloading;
            assert!(live.end_invalid_preview(next), "{why} ends the preview");
            live.proposals_reloading = false;
            assert!(live.previewed().is_none(), "{why} cannot resume on the old Ready view");
        };
        ends(&snap, true, "reload");
        snap.preview = Some(design);
        ends(&snap, false, "a gesture picture");
        snap.preview = None;
        for state in [ProposalState::Unchecked, ProposalState::Checking, ProposalState::Stale("changed".into()), ProposalState::Refused("error".into())] {
            Arc::make_mut(&mut snap.proposals)[0] = Arc::new(ProposalView { state: state.clone(), ..(*view).clone() });
            ends(&snap, false, &format!("{state:?}"));
        }
        Arc::make_mut(&mut snap.proposals)[0] = Arc::new(ProposalView { design: None, ..(*view).clone() });
        ends(&snap, false, "loss of the verified design");
        Arc::make_mut(&mut snap.proposals)[0] = Arc::new(ProposalView { id: ProposalId(2), ..(*view).clone() });
        ends(&snap, false, "replacement in the same slot");
        snap.proposals = Arc::default();
        ends(&snap, false, "accept or dismissal");
    }
}
