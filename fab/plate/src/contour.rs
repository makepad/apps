//! The slicer's input contract: closed layer contours per part and filament.
//!
//! The slicer never sees the model document. It reads a [`ContourSource`],
//! which answers "what is solid at height z" as integer loops. The first
//! source is the evaluated [`Plate`](crate::Plate) mesh; exact sources
//! (SDF fields and CSG trees sectioned directly) implement the same trait.
//!
//! Coordinates are integers on a fixed grid of [`UNITS_PER_MM`] per
//! millimetre (0.1 µm), so offsets and booleans in the slicer are exact and
//! a 350 mm bed uses 3.5e9 units: ample i64 headroom for products.

use crate::Bounds;

/// Grid units per millimetre for contour points (0.1 µm per unit).
pub const UNITS_PER_MM: i64 = 10_000;

/// A point on the contour grid.
pub type Point = [i64; 2];

pub fn to_units(mm: f64) -> i64 {
    (mm * UNITS_PER_MM as f64).round() as i64
}

pub fn to_mm(units: i64) -> f64 {
    units as f64 / UNITS_PER_MM as f64
}

/// One closed loop, without a repeated closing point. Outer boundaries wind
/// counter-clockwise seen from above (+Z), holes clockwise.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loop {
    pub points: Vec<Point>,
}

impl Loop {
    /// Twice the signed area in grid units² (positive = counter-clockwise).
    pub fn area2(&self) -> i128 {
        let n = self.points.len();
        let mut a = 0i128;
        for i in 0..n {
            let p = self.points[i];
            let q = self.points[(i + 1) % n];
            a += p[0] as i128 * q[1] as i128 - q[0] as i128 * p[1] as i128;
        }
        a
    }
    pub fn is_hole(&self) -> bool {
        self.area2() < 0
    }
}

/// What one part contributes to one layer: its loops, in one filament.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Region {
    /// Index into the plate's parts.
    pub part: usize,
    /// Filament slot, 1-based (as on [`Part`](crate::Part)).
    pub filament: u32,
    /// The part's nozzle restriction, if any (as on [`Part`](crate::Part)).
    pub nozzle: Option<usize>,
    pub loops: Vec<Loop>,
}

/// A source of layer cross-sections.
pub trait ContourSource: Sync {
    /// The solid's extent in millimetres (Z from the plate up).
    fn bounds(&self) -> Bounds;
    /// The cross-section at height `z` (mm). Regions of different parts may
    /// overlap; within one region the loops are disjoint and nest by winding.
    fn layer(&self, z: f64) -> Vec<Region>;
}
