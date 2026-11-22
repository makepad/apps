//! The schematic connection grid.
//!
//! Symbol pins and sheet connection points sit on a grid of 1.27 mm (50 mil), the grid every
//! sheet and symbol library shares; a `SchPt` counts grid steps, so two pins connect exactly
//! when their `SchPt`s are equal. Symbol graphics are free and stay in `P` (nm). Y is up, as in
//! `P`.

use crate::geom::{Nm, P};

/// A point on the connection grid, in grid steps of `SchPt::GRID`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct SchPt {
    pub x: i32,
    pub y: i32,
}

impl SchPt {
    /// One grid step: 1.27 mm.
    pub const GRID: Nm = 1_270_000;
    pub const ZERO: SchPt = SchPt { x: 0, y: 0 };
    pub const fn new(x: i32, y: i32) -> SchPt {
        SchPt { x, y }
    }
    pub fn add(self, o: SchPt) -> SchPt {
        SchPt::new(self.x + o.x, self.y + o.y)
    }
    pub fn sub(self, o: SchPt) -> SchPt {
        SchPt::new(self.x - o.x, self.y - o.y)
    }
    /// The point in nm (symbol graphics space).
    pub fn to_p(self) -> P {
        P::new(self.x * Self::GRID, self.y * Self::GRID)
    }
    /// The nearest grid point to `p`.
    pub fn snap(p: P) -> SchPt {
        let r = |v: Nm| (v as f64 / Self::GRID as f64).round() as i32;
        SchPt::new(r(p.x), r(p.y))
    }
    /// Rotated by a quarter turns counter-clockwise about the origin (exact).
    pub fn rotated_quarters(self, q: u8) -> SchPt {
        match q % 4 {
            0 => self,
            1 => SchPt::new(-self.y, self.x),
            2 => SchPt::new(-self.x, -self.y),
            _ => SchPt::new(self.y, -self.x),
        }
    }
}

crate::canon_struct!(SchPt { x, y });
