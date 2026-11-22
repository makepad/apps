//! The plate's overhangs ranked by what no support holds: the list the Slice panel shows, made
//! (by fab-ai, from the plate's live slice) the way `fab.overhangs` makes its rows. What the
//! part's own settings keep the trees out from under is listed apart, after them, and is not
//! asked of the supports.

/// Why the plan leaves an overhang with nothing under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unheld {
    /// No support can reach it: none could stand under it, or the part is in the way.
    GivenUp,
    /// A support there would be locked in the part: it could not come out.
    Locked,
    /// An `m.support_block` covers most of it.
    Blocked,
    /// It lies inside the part's footprint, which the part keeps its trees out of by its
    /// setting, and no branch reaches in to it.
    Footprint,
    /// It lies over a no-trees area (`m.no_trees`).
    NoTrees,
}

impl Unheld {
    /// The person's own settings keep the trees out from under it: not asked of the supports.
    pub fn chosen(self) -> bool {
        matches!(self, Unheld::Footprint | Unheld::NoTrees)
    }
}

/// One line: an overhang patch, or patches near each other.
#[derive(Clone, Debug, PartialEq)]
pub struct OverhangLine {
    /// The plate part's name.
    pub part: String,
    /// A single patch's id over the plate (`p3`: the third largest); none for a cluster.
    pub id: Option<usize>,
    pub patches: usize,
    pub area_mm2: f64,
    /// What no support holds (none: the plate plans no supports).
    pub left_mm2: Option<f64>,
    /// How many of its patches hang over the part (the rest over the bed).
    pub over_part: usize,
    /// Why the plan leaves a single patch, when it leaves much of it. A line of what the part's
    /// own settings keep the trees out from under ([`OverhangLine::chosen`]) says which.
    pub unheld: Option<Unheld>,
    /// Its box, plate mm.
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// A cluster's patches, each as its own line, worst first (the most area left): the panel
    /// steps through them. Empty for a single patch.
    pub members: Vec<OverhangLine>,
}

impl OverhangLine {
    /// All of it is what the part's own settings keep the trees out from under: its area is left,
    /// and none of it is in the list's.
    pub fn chosen(&self) -> bool {
        self.unheld.is_some_and(Unheld::chosen)
    }

    /// The share of its area the supports hold (none: the plate plans no supports).
    pub fn held(&self) -> Option<f64> {
        self.left_mm2.map(|left| if self.area_mm2 > 0.0 { (1.0 - left / self.area_mm2).max(0.0) } else { 1.0 })
    }
}

/// Every overhang on the plate, in lines worst first (the most area left), then a line per part
/// and reason for what its own settings keep the trees out from under ([`OverhangLine::chosen`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OverhangList {
    pub patches: usize,
    /// The overhang the supports are asked to hold: what the parts' own settings keep the trees
    /// out from under is not in it.
    pub area_mm2: f64,
    /// What no support holds of that (none: the plate plans no supports).
    pub left_mm2: Option<f64>,
    pub lines: Vec<OverhangLine>,
}
