//! Shared requests and authorship for edits resolved by the host.

use crate::DesignOp;
use fab_elec_core::geom::{Angle, P};
use fab_elec_core::{BoardId, Side};

/// Who made an edit, set by the calling code path rather than a tool argument.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum By {
    /// Input from a person using a pane or editor.
    #[default]
    Person,
    /// An AI tool dispatch, identified by its conversation turn and call.
    Ai { turn: u64, call: u64 },
    /// A document changed on disk.
    Disk,
}

/// Put a part's footprint on a board in the same step that adds or names it.
/// The host resolves this into board ops; a block uses its placement macro anchored at `at`.
#[derive(Clone, PartialEq, Debug)]
pub struct Place {
    /// The board receiving the footprint.
    pub unit: BoardId,
    /// The existing or newly added part to place.
    pub part: PartRef,
    /// The placement anchor on the board.
    pub at: P,
    /// A requested rotation, otherwise the placement default.
    pub rot: Option<Angle>,
    /// A requested board side, otherwise the placement default.
    pub side: Option<Side>,
}

/// A placement's part before the host resolves names and newly added components.
#[derive(Clone, PartialEq, Debug)]
pub enum PartRef {
    /// The zero-based `CircuitOp::AddPart` in the same commit or sample.
    Added(u32),
    /// An existing part, by component path.
    Path(String),
}

/// The edits a lane's plan function returns for a host invocation.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Resolved {
    /// Model edits to commit or preview together.
    pub ops: Vec<DesignOp>,
    /// Footprints to place in the same step.
    pub place: Vec<Place>,
    /// The action's name in history and responses.
    pub label: String,
}
