//! Fab electronics routing (`fab-elec-route`; E6 in the design, ARCHITECTURE 5.7): the
//! routing world, problem and solution, the route worker, the legaliser and placer, the grid
//! autorouter and the round embedder.
//!
//! The seam the board canvas builds against:
//! - `gesture`: `Gesture` and `GestureSample`, what the canvas sends per pointer event;
//! - `request`: `RouteRequest`, the worker's commands (committed bases, jobs, gesture ends);
//! - `proposal`: `RouteProposal` and `RouteEvent`, what the canvas draws: board ops against a
//!   committed generation, metrics, `verified`, collisions, open nets;
//! - `solution`: `RouteSolution`, `Reason`, `RouteMetrics`, what every engine returns;
//! - `progress`: `Progress`, counted work, cancel and `offer` at transaction boundaries;
//! - `channel`: the worker's two ends, a latest-value sample slot and a bounded command queue
//!   on the way in, superseding answers on the way out.
//!
//! The engines' common parts:
//! - `world`: `RBase` (the board's pieces, indexed per layer, updated by delta) and `World`
//!   (a base plus a transaction's overlay);
//! - `verify`: the verifier every engine runs on what it adds;
//! - `check`: the check of a change against a checked board state (the worker's proposal check,
//!   the organic judge's);
//! - `problem`: `RouteProblem`, `RNet`, the plan order, the `Autorouter` and `Legaliser` traits;
//! - `grid`: the tiled occupancy grid, its A* and blockers searches, and `lift` to geometry;
//! - `embed`: the band embedder (E6 2): a drawn reference made legal on its own side of each
//!   obstacle (`make_legal`) and pulled taut (`tighten`), with octagonal hulls (`Octi`);
//! - `head`: the interactive head (E6 3), a route gesture's legs, vias and head laid by the
//!   embedder against the committed base, answered on the worker per sample;
//! - `source`: the world as the grid's obstacle source (rules through `required_to`);
//! - `organic`: the organic method's relaxer and kernels (ported; see CREDITS.md).
//!
//! Threading (E6 8): the canvas thread does O(1) per pointer event (a slot write) and one
//! `try_recv` drain per frame; it never computes a route and never waits. One long-lived
//! worker owns the world; jobs fan out on the pool over snapshots.
//!
//! One definition of legal (E6 0.8): every engine here verifies with the board's own
//! `required_to`, `check_to`, `check_drills` and `check_item`, the functions DRC calls.

pub mod auto;
pub mod channel;
pub mod check;
pub mod embed;
pub mod gesture;
pub mod grid;
pub mod head;
pub mod organic;
pub mod problem;
pub mod progress;
pub mod proposal;
pub mod request;
pub mod solution;
pub mod source;
pub mod verify;
pub mod worker;
pub mod world;

pub use channel::{route_channel, RouteHandle, RouteInbox, Work, QUEUE_DEPTH};
pub use gesture::{Gesture, GestureId, GestureKind, GestureSample, Leg, Mode, Posture, Style, WidthRule, Xform};
pub use problem::{problem, scope_nets, Autorouter, End, Legaliser, RNet, RStack, RouteProblem, Terminal};
pub use progress::{JobPhase, NetStatus, Progress, RetryWhy};
pub use proposal::{
    BlockCause, Blocked, Checked, Collision, Explanation, HeadAnswer, JobId, JobStatus, ProposalId, ProposalSource, RouteEvent,
    RouteProposal, SampleCause, SampleRefused, Tier,
};
pub use request::{Base, Effort, Engine, Method, PlaceJob, PlaceMode, RestyleJob, RouteJob, RouteRequest, RouteScope};
pub use solution::{Kept, NetCopper, Reason, RouteMetrics, RouteSolution, WorkCounts};
pub use source::{probe_copper, WorldSource};
pub use verify::{new_pieces, verify};
pub use worker::{replay, RouteWorker, Spawn};
pub use world::{Fixity, Overlay, RBase, RId, RItem, RPiece, WRef, World};

/// The engines that make `Solved` copper (`Origin::Solved { by }`), with their versions in the
/// low byte: a new engine version never changes a board by itself (E6 9.6).
pub mod solvers {
    use fab_elec_core::SolverId;
    /// The tiled grid autorouter (E6 5.2).
    pub const GRID: SolverId = SolverId(0x0101);
    /// The round embedder's restyle (E6 7).
    pub const RESTYLE: SolverId = SolverId(0x0201);
    /// "Finish this net" from the head.
    pub const FINISH: SolverId = SolverId(0x0301);
    /// The legaliser and placer.
    pub const PLACE: SolverId = SolverId(0x0401);
    /// The organic relaxer (`organic`; ported, see CREDITS.md).
    pub const ORGANIC: SolverId = SolverId(0x0501);
}
