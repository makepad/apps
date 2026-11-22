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
use crate::live::sim::{LiveScreen, SimLive};
use fab_elec_core::{BoardId, CompRef, Diag, DocId, NetId, Sel};
use fab_elec_design::{By, Design, DesignOp, Focus, Place, SystemNet, TextEdit, TimeCursor};
use fab_elec_lang::sys_sim::SimSettings;
use fab_elec_lang::{SourceMap, UnitState};
use fab_elec_route::{GestureId, GestureSample, JobId, JobStatus, Method, Reason, RouteProposal, RouteScope};
use makepad_widgets_core::Texture;
use std::sync::Arc;
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

/// What every view reads.
#[derive(Clone, Debug)]
pub struct DesignSnapshot {
    /// The committed design (the ghost while a preview is live).
    pub design: Arc<Design>,
    /// The live preview, drawn over the ghost.
    pub preview: Option<Arc<Design>>,
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
    /// Ids of `violations` that await a zone's refill: they name a zone whose fill is still pending in the shown
    /// results. Views show them as pending, not as violations. Sorted.
    pub awaiting: Arc<Vec<u64>>,
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

/// What a view emits; the host applies it (views never touch the model or the text).
#[derive(Clone, PartialEq, Debug)]
pub enum ViewAction {
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
        ElecLive { snap, gesture: None, sim: SimLive::default(), states: Vec::new(), title: String::new(), chosen_bench: None, check: None, opened: None, traces: Arc::default(), shown_unit: None }
    }

    pub fn run_bench(&self) -> Option<&fab_elec_lang::BenchView> {
        run_bench(&self.snap.benches, self.chosen_bench.as_deref())
    }

    /// The sheet and its Library follow the run's circuit. A bench with its own circuit has
    /// no design sheet, so keep the previous unit when it still belongs to this design.
    pub(crate) fn sheet_unit(&self, previous: Option<(BoardId, &str)>) -> Option<usize> {
        let units = &self.snap.live().units;
        self.run_bench().and_then(|bench| units.iter().position(|u| u.id == bench.out.scope_id() && !u.circuit.comps.is_empty()))
            .or_else(|| previous.and_then(|(id, name)| units.iter().position(|u| u.id == id && u.name == name)))
            .or_else(|| units.iter().position(|u| !u.circuit.comps.is_empty()))
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
