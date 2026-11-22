//! Commands to the route worker (E6 8): the bounded command queue's contents. Gesture samples
//! do not go through it; they go to the latest-value slot (`RouteHandle::sample`).

use crate::gesture::{GestureId, Style};
use crate::proposal::JobId;
use fab_elec_board::{Board, BoardDelta, Connectivity, RuleMatrix, ZoneFill};
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

/// How hard to try: a deterministic number (candidate transactions per open net, via-reduction
/// retries), never a wall-clock limit (E6 5.4).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Effort(pub u32);

impl Default for Effort {
    fn default() -> Self {
        Effort(4)
    }
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

/// `elec.place`.
#[derive(Clone, PartialEq, Debug)]
pub struct PlaceJob {
    pub job: JobId,
    pub unit: BoardId,
    /// The parts to place or legalise; empty is every unlocked part.
    pub scope: Vec<CompId>,
    pub mode: PlaceMode,
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
    /// `elec.route.explain`.
    Explain { unit: BoardId, net: NetId },
    /// Stop a job at its next transaction boundary; its last proposal is legal.
    Cancel(JobId),
}
