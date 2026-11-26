//! Commands to the route worker (E6 8): the bounded command queue's contents. Gesture samples
//! do not go through it; they go to the latest-value slot (`RouteHandle::sample`).

use crate::gesture::{GestureId, Style};
use crate::proposal::JobId;
use fab_elec_board::{Board, BoardDelta, Connectivity, Placed, PortRoom, RuleMatrix, ZoneFill};
use fab_elec_circuit::Circuit;
use fab_elec_core::{BBox, BoardId, Canon, ClassId, CompId, ContentHash, LayerSet, NetId};
use std::sync::Arc;

/// The committed state of one unit's board, as the host's stages published it. The worker
/// updates its world by `delta` (O(delta log n)) or rebuilds when it is global or the worker holds
/// no base of the unit; `None` is diffed against the held base on the worker.
#[derive(Clone, PartialEq, Debug)]
pub struct Base {
    pub unit: BoardId,
    pub board: Arc<Board>,
    pub rules: Arc<RuleMatrix>,
    pub conn: Arc<Connectivity>,
    pub fills: Arc<Vec<ZoneFill>>,
    /// From the previous `Base` of this unit to this one. `None`: the worker computes it.
    pub delta: Option<BoardDelta>,
    /// `board`'s content hash, as the host computed it: the worker copies it onto every proposal
    /// made against this base, and a gesture's samples expect it.
    pub hash: ContentHash,
    /// Whether `fills` are exact. A gesture's base never waits for fills and may carry fast ones;
    /// a job's base is always exact.
    pub exact: bool,
}

impl Base {
    /// A base without a delta, with exact fills, its hash computed from `board`.
    pub fn new(board: Arc<Board>, rules: Arc<RuleMatrix>, conn: Arc<Connectivity>, fills: Arc<Vec<ZoneFill>>) -> Base {
        let hash = board.content_hash();
        Base { unit: board.unit, board, rules, conn, fills, delta: None, hash, exact: true }
    }
}

/// Which nets a job works on.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum RouteScope {
    #[default]
    All,
    Nets(Vec<NetId>),
    Class(ClassId),
    /// A length-match, bundle or lane group by name.
    Group(String),
    /// The nets with a terminal or owned copper inside the box.
    Region(BBox),
    /// The open connections with an end on a pad of these parts (a block's parts, or the selection's):
    /// a net wholly among them is routed whole, a net that leaves them only on the connections that
    /// reach them. Nets a pour serves are skipped, as for every scope.
    Parts(Vec<CompId>),
}

/// The routing method: how a job shapes its copper. Every method answers through the same request,
/// proposal and worker; `Interactive` is the grid-snapped router.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Method {
    #[default]
    Interactive,
    /// `@organic`: the grid router's tracks relaxed into smooth, clearance-guarded curves
    /// (`organic::job`; ported, see CREDITS.md).
    Organic,
}

/// Which search engine (E6 0.7).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Engine {
    #[default]
    Auto,
    Grid,
    Sketch,
    /// A ported engine by name.
    Named(String),
}

/// How hard to try: a declared setting that ends at a step of the algorithm, never at a clock
/// (E6 5.4). `Full` negotiates (rips up and reroutes) until a whole pass opens no fewer nets;
/// `Fast` routes each net once and stops there: the nets one pass leaves open, with their
/// blockers, in seconds, for judging a placement that is about to change. A board that routes at
/// `Full` cannot fail because a caller asked for `Fast`; it shows what is left to negotiate.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Effort {
    #[default]
    Full,
    Fast,
}

/// `elec.route`.
#[derive(Clone, PartialEq, Debug)]
pub struct RouteJob {
    pub job: JobId,
    pub unit: BoardId,
    pub scope: RouteScope,
    pub method: Method,
    pub engine: Engine,
    /// `None`: each net's own style.
    pub style: Option<Style>,
    pub effort: Effort,
    /// `None`: each net's allowed layers.
    pub layers: Option<LayerSet>,
    /// Re-route: the board's solved, unlocked copper may be ripped and laid again, of the nets in
    /// scope and of any other net in their way (joined again before the job keeps the rip). Without it the job keeps every piece of copper the board has and routes
    /// only the open connections: a pin added to a routed net joins its tree as it is.
    pub reroute: bool,
}

/// `elec.route.restyle`: re-embed the tracks in scope from their own geometry (E6 7).
#[derive(Clone, PartialEq, Debug)]
pub struct RestyleJob {
    pub job: JobId,
    pub unit: BoardId,
    pub scope: RouteScope,
    pub style: Style,
    /// Authored copper too (an explicit, undoable action); otherwise only solved copper.
    pub authored: bool,
}

/// A published rule for checking a finished placement. Only its factory crosses threads.
pub trait PlacePlan: Send + Sync {
    fn open(&self) -> Result<Box<dyn PlacePlanner>, String>;
}

impl std::fmt::Debug for dyn PlacePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("PlacePlan") }
}

impl PartialEq for dyn PlacePlan {
    fn eq(&self, other: &Self) -> bool { std::ptr::addr_eq(self, other) }
}

/// One job's instance of the rule, opened and called on its placement worker.
pub trait PlacePlanner {
    fn plan(&mut self, board: &Board, circuit: &Circuit) -> Result<PlanVerdict, String>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanVerdict {
    pub rooms: Vec<PortRoom>,
    pub refused: Vec<String>,
    /// Counts of individual pipes, including every channel in a refused group.
    pub refused_pipes: u32,
    pub made_pipes: u32,
}

/// The final offer's guard result. An error before a verdict leaves its prediction absent.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct PlanReport {
    pub calls: u32,
    pub replaces: u32,
    pub made_pipes: Option<u32>,
    pub refused_pipes: Option<u32>,
    pub note: Option<String>,
}
fab_elec_core::canon_struct!(PlanReport { calls, replaces, made_pipes, refused_pipes, note });

/// One net-length edit on the committed route base.
#[derive(Clone, PartialEq, Debug)]
pub struct TuneJob {
    pub job: JobId,
    pub unit: BoardId,
    pub tune: crate::meander::Tune,
    pub circuit: Arc<Circuit>,
}

/// A single atomic differential route on the unit's worker base.
#[derive(Clone, PartialEq, Debug)]
pub struct PairJob {
    pub job: JobId,
    pub unit: BoardId,
    pub route: crate::pair::Route,
}

/// `elec.place`.
#[derive(Clone, PartialEq, Debug)]
pub struct PlaceJob {
    pub job: JobId,
    pub unit: BoardId,
    /// The parts to place or legalise, among the board's and `add`'s; empty is every unlocked part.
    pub scope: Vec<CompId>,
    pub mode: PlaceMode,
    /// Standing panel parts left out of the requested scope.
    pub held: u32,
    /// The unit's latest published port geometry, relative to each component's position in board axes.
    pub rooms: Vec<PortRoom>,
    pub plan: Option<Arc<dyn PlacePlan>>,
    /// Parts of the circuit the board does not place yet, made by the host as the tray's drop
    /// makes them (footprint, pad nets, body band; ids from the base's `next_item` on), each at
    /// its spot in the row under the board: the job places them, and its proposal adds each on the
    /// spot it found, or in that row when it found no room.
    pub add: Vec<Placed>,
    /// The unit's circuit: its blocks and intents steer the seed.
    pub circuit: Arc<Circuit>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum PlaceMode {
    /// Make the current placement legal with minimal movement.
    #[default]
    Legalise,
    /// Place from the block tree and intent, then legalise.
    Place,
}

/// A command to the route worker.
#[derive(Clone, PartialEq, Debug)]
pub enum RouteRequest {
    /// A new committed state of a unit's board.
    Base(Base),
    /// The gesture ended (committed or cancelled by the host): the worker drops its state.
    End(GestureId),
    Route(RouteJob),
    Restyle(RestyleJob),
    Place(PlaceJob),
    Tune(TuneJob),
    Pair(PairJob),
    /// `elec.route.explain`.
    Explain { unit: BoardId, net: NetId },
    /// Stop a job at its next transaction boundary; its last proposal is legal.
    Cancel(JobId),
}
