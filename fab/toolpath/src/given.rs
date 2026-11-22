//! What a part's GPU stages already made of a layer, put with its trace
//! ([`crate::stages::PartPlan::put_trace`]). A stage that has its piece given skips its own
//! body for it: the walls (W), their overhang labels (S3), the classes (S1+S2) and the
//! concentric fills (S1b). What is given is exactly what the stage would make, so a plan from
//! given pieces equals the plan made on the cores ([`crate::stages::PartPlan::with_capture`]
//! records them).

use makepad_fab_slicer::geom::Pt;
use makepad_fab_slicer::source::{Classes, IslandWalls};
use std::sync::Arc;

/// A layer's given pieces.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GivenLayer {
    /// The layer's classes, every island's together. A part gives every layer's classes or
    /// none ([`crate::stages::PartPlan::with_given_classes`]): they reach across layers.
    pub classes: Option<Arc<Classes>>,
    /// One per island its walls are made from, in that order.
    pub islands: Vec<GivenIsland>,
    /// Its trace is the part's outline already: the XY compensation applied, and (on the first
    /// layer) the elephant-foot compensation in its walls. Its islands are the outline's.
    pub compensated: bool,
}

/// An island's given pieces.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GivenIsland {
    /// Its wall rings or beads and the fill region inside them.
    pub walls: Option<Arc<IslandWalls>>,
    /// The overhang labels of each line of its walls, numbered as
    /// [`crate::walls::Along`] numbers them.
    pub labels: Option<Vec<crate::walls::Labels>>,
    /// Its share of the layer's classes: each clipped to its fill. Read with the layer's.
    pub classes: Option<Classes>,
    /// One per piece of its solid share: whether the piece is narrow (filled by loops).
    pub narrow: Option<Vec<bool>>,
    /// The concentric fills of its pieces.
    pub concentric: Option<GivenConcentric>,
}

/// An island's concentric fills: the beads of each piece, in its share's order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GivenConcentric {
    /// One per piece of its vertical-shell share.
    pub vertical: Vec<Vec<Bead>>,
    /// One per narrow piece of its solid share.
    pub narrow: Vec<Vec<Bead>>,
}

/// One bead of a concentric fill ([`crate::concentric_beads`]); its path follows from the role
/// ([`crate::bead_paths`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Bead {
    /// A closed loop seamed at point `seam`.
    Loop { pts: Vec<Pt>, seam: usize, width: f64 },
    /// An open line.
    Line { pts: Vec<Pt>, width: f64 },
}

/// How many given pieces a part's stages took instead of making them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GivenUse {
    pub walls: usize,
    pub labels: usize,
    pub classes: usize,
    pub island_classes: usize,
    pub narrow: usize,
    pub concentric: usize,
}
