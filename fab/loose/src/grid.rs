//! A part's distance on a grid, sampled once when a check starts: a sim step reads hundreds to
//! thousands of distances at 240 Hz, and a way-out search millions. The checks and the sim read it
//! through [`DistGrid`] alone; how its nodes are laid out is the grid's own.
//!
//! Two layouts, chosen by the person ([`Contact`]): an even lattice of the distance found from the
//! field's values ([`Contact::Fast`]), and bricks of the true distance, split until they read
//! within a tolerance of it or their cells are half the printer's layer ([`Contact::Exact`]).

mod bricks;
mod even;

use crate::solid::Solid;
use bricks::Bricks;
use even::Even;
use makepad_csg_mech::GAP_TOL;
use makepad_fab_toolpath::par::{Cancelled, Ctl};

/// The grid runs this far past the part's box (mm), for contacts made just off it.
const PAST_BOX: f64 = 2.0;
/// An even grid's word on a ball, taken only past what a sweep can overstate and this many cells.
const GRID_TRUST: f32 = 0.85;
const GRID_CELLS: f32 = 3.0;
/// Most cells along the part's longest side in a loose check's even grid (past it the cells
/// grow, so a big part's grid stays a few tens of MB), none finer than the least (mm). The
/// way-out search's lattice is this one's whatever the layout.
const MOST_CELLS: f64 = 192.0;
const LEAST_CELL: f64 = 0.15;

/// How a part's distance is found for its contacts and checks: the person's choice, kept with the
/// view's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Contact {
    /// An even lattice of the field's values over their gradient, swept out from the surface:
    /// built in a fraction of a second, off by up to a few cells at edges and in narrow gaps.
    #[default]
    Fast,
    /// Bricks of the true distance, read within [`Tol`] of it on faces, and within a quarter of
    /// the printer's layer at edges, corners and in narrow gaps, where cells stop at half a
    /// layer: slower to build. A sim's parts rest a few microns into each other (about 1 um,
    /// 6 um at most on a bearing's rollers), inside the gap tolerance, and a part pushed hard into
    /// another goes up to about 0.03 mm in: see [`Contact::layer`].
    Exact,
}

impl Contact {
    /// The setting's name as kept, and back.
    pub fn name(self) -> &'static str {
        match self {
            Contact::Fast => "fast",
            Contact::Exact => "exact",
        }
    }

    pub fn named(name: &str) -> Option<Contact> {
        [Contact::Fast, Contact::Exact].into_iter().find(|c| c.name() == name)
    }

    /// The soft layer (mm) a sim's contacts between this setting's grids take: Exact's reads are
    /// known to [`GAP_TOL`], and its parts rest inside it; Fast keeps the engine's stiffness.
    pub fn layer(self) -> f64 {
        match self {
            Contact::Fast => 0.0,
            Contact::Exact => GAP_TOL,
        }
    }
}

/// The setting in force for this process's checks and sims: set where the person chooses it.
static CONTACT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The contact setting in force.
pub fn contact() -> Contact {
    if CONTACT.load(std::sync::atomic::Ordering::Relaxed) { Contact::Exact } else { Contact::Fast }
}

/// Puts `c` in force for the grids built from now on.
pub fn set_contact(c: Contact) {
    CONTACT.store(c == Contact::Exact, std::sync::atomic::Ordering::Relaxed);
}

/// How near an exact grid reads to the true distance `d`: within max(GAP_TOL, slope x |d|), the
/// slope outside the part [`Tol::LOOSE`]'s or [`Tol::SIM`]'s, inside half of `d` for both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tol {
    outside: f64,
}

impl Tol {
    /// A loose check's. Its bar: a 0.24 mm ball in a 0.30 mm neck has 0.04 to spare past
    /// KEEP_CLEAR; a sixteenth of 0.30 (under GAP_TOL) leaves it half of that.
    pub const LOOSE: Tol = Tol { outside: 1.0 / 16.0 };
    /// A sim's: values far off only steer the engine's search for contacts.
    pub const SIM: Tol = Tol { outside: 0.25 };
    /// Inside, for both: overlaps are under 0.15 mm, and a deep value needs only its sign and a
    /// rough size.
    const INSIDE: f64 = 0.5;

    /// The tolerance at the true distance `d` (mm).
    pub fn at(self, d: f64) -> f64 {
        (d.abs() * if d < 0.0 { Self::INSIDE } else { self.outside }).max(GAP_TOL)
    }
}

#[derive(Clone, Debug)]
enum Kind {
    Even(Even),
    Bricks(Bricks),
}

#[derive(Clone, Debug)]
pub struct DistGrid {
    kind: Kind,
    /// The part's box.
    pub box_lo: [f32; 3],
    pub box_hi: [f32; 3],
}

impl DistGrid {
    /// A loose check's grid of `solid`. Fast: nodes along the part's longest side up to a count,
    /// cells never finer than a least size. Exact: bricks within [`Tol::LOOSE`] of the truth, that
    /// see down to `finest` (mm), the printer's resolution.
    pub fn check(ctl: Ctl, solid: &Solid, finest: f64, contact: Contact) -> Result<DistGrid, Cancelled> {
        let kind = match contact {
            Contact::Fast => Kind::Even(Even::build(ctl, solid)?),
            Contact::Exact => Kind::Bricks(Bricks::build(ctl, solid, finest, Tol::LOOSE)?),
        };
        Ok(Self::of(kind, solid))
    }

    /// A sim's grid of `solid`. Fast: nodes over the grid's box up to a count, cells no finer
    /// than `finest` (mm), the printer's resolution, finer than which a printed part has no
    /// shape. Exact: bricks within [`Tol::SIM`] of the truth, that see down to `finest`.
    pub fn sim(ctl: Ctl, solid: &Solid, finest: f64, contact: Contact) -> Result<DistGrid, Cancelled> {
        let kind = match contact {
            Contact::Fast => Kind::Even(Even::build_at(ctl, solid, finest)?),
            Contact::Exact => Kind::Bricks(Bricks::build(ctl, solid, finest, Tol::SIM)?),
        };
        Ok(Self::of(kind, solid))
    }

    fn of(kind: Kind, solid: &Solid) -> DistGrid {
        DistGrid { kind, box_lo: solid.lo.map(|v| v as f32), box_hi: solid.hi.map(|v| v as f32) }
    }

    /// The distance at `p` (negative inside) and the unit direction it grows in.
    #[inline]
    pub fn sample(&self, p: [f32; 3]) -> (f32, [f32; 3]) {
        match &self.kind {
            Kind::Even(g) => g.sample(p),
            Kind::Bricks(g) => g.sample(p),
        }
    }

    #[inline]
    pub fn distance(&self, p: [f32; 3]) -> f32 {
        self.sample(p).0
    }

    /// The side of the grid's cells at `p` (mm): a step that long or shorter passes over nothing
    /// the grid holds.
    pub fn cell_at(&self, p: [f32; 3]) -> f32 {
        match &self.kind {
            Kind::Even(g) => g.cell,
            Kind::Bricks(g) => g.cell_at(p),
        }
    }

    /// The least the true distance can be where the grid reads `d` at `p`.
    pub fn least_at(&self, p: [f32; 3], d: f32) -> f32 {
        match &self.kind {
            Kind::Even(g) => d * GRID_TRUST - GRID_CELLS * g.cell,
            Kind::Bricks(g) => g.least_at(p, d),
        }
    }

    /// A coarser lattice over the grid, every `every`-th node of a loose check's even lattice
    /// over the part: its least corner, its step (mm) and its nodes along each axis.
    pub fn coarse(&self, every: usize) -> ([f32; 3], f32, [usize; 3]) {
        let (lo, cell, n) = match &self.kind {
            Kind::Even(g) => (g.lo, g.cell, g.n),
            Kind::Bricks(_) => {
                let size: [f64; 3] = std::array::from_fn(|k| (self.box_hi[k] - self.box_lo[k]) as f64);
                let cell = (size.iter().fold(0.0, |a: f64, &b| a.max(b)) / MOST_CELLS).max(LEAST_CELL);
                let n = size.map(|s| ((s + 2.0 * PAST_BOX) / cell).ceil() as usize + 1);
                (self.box_lo.map(|v| (v as f64 - PAST_BOX) as f32), cell as f32, n)
            }
        };
        (lo, cell * every as f32, n.map(|v| (v - 1) / every + 1))
    }

    /// How many bricks an exact grid holds and the bytes they take (none for a fast one).
    pub fn bricks(&self) -> (usize, usize) {
        match &self.kind {
            Kind::Even(_) => (0, 0),
            Kind::Bricks(g) => (g.bricks(), g.bytes()),
        }
    }

    /// How many of an exact grid's bricks failed their test at the finest cells, and the worst
    /// error at a cell's centre among them (mm; none for a fast one).
    pub fn flagged(&self) -> (usize, f32) {
        match &self.kind {
            Kind::Even(_) => (0, 0.0),
            Kind::Bricks(g) => g.flagged(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_csg_sdf::{SdfNode, SdfOp, SdfShape};

    /// The setting, on a box as a mesh. Fast is the even lattice, bit for bit, for a check and a
    /// sim. Exact: no cell finer than half the layer; no read under the least `least_at` gives
    /// past the truth, anywhere; the flagged bricks' worst error at a cell's centre within a
    /// quarter layer; and every read within its tolerance of the truth, or, within a layer of two
    /// faces, within the tolerance or a quarter layer.
    #[test]
    fn fast_is_the_even_lattice_and_exact_reads_within_its_tolerance() {
        let half = [1.5, 1.0, 0.75];
        let corner = |c: usize| -> [f64; 3] { std::array::from_fn(|k| if c & (1 << k) == 0 { -half[k] } else { half[k] }) };
        let triangles = vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        let solid = Solid::of_mesh((0..8).map(corner).collect(), triangles);
        let finest = 0.2;
        let truth = |p: [f64; 3]| {
            let q: [f64; 3] = std::array::from_fn(|k| p[k].abs() - half[k]);
            let out = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2) + q[2].max(0.0).powi(2)).sqrt();
            out + q[0].max(q[1]).max(q[2]).min(0.0)
        };
        let mut points: Vec<[f64; 3]> = (0..2000).map(|s| {
            let t = s as f64 * 0.618_033_988_75;
            std::array::from_fn(|k| -half[k] - 0.6 + (2.0 * half[k] + 1.2) * ((t * (k as f64 * 2.0 + 1.0)) % 1.0))
        }).collect();
        // along each of the 12 edges, on it and off it along its bisector, out and in
        for axis in 0..3 {
            for corner in 0..4 {
                let (i, j) = ((axis + 1) % 3, (axis + 2) % 3);
                let s = [if corner & 1 == 0 { -1.0 } else { 1.0 }, if corner & 2 == 0 { -1.0 } else { 1.0 }];
                for a in [-0.8, -0.3, 0.1, 0.55] {
                    for off in [-0.3, -0.1, -0.02, 0.0, 0.02, 0.1, 0.3] {
                        let mut p = [0.0; 3];
                        p[axis] = a * half[axis];
                        p[i] = s[0] * (half[i] + off * std::f64::consts::FRAC_1_SQRT_2);
                        p[j] = s[1] * (half[j] + off * std::f64::consts::FRAC_1_SQRT_2);
                        points.push(p);
                    }
                }
            }
        }
        let fast = [
            ("check", DistGrid::check(Ctl::default(), &solid, finest, Contact::Fast).unwrap(), Even::build(Ctl::default(), &solid).unwrap()),
            ("sim", DistGrid::sim(Ctl::default(), &solid, finest, Contact::Fast).unwrap(), Even::build_at(Ctl::default(), &solid, finest).unwrap()),
        ];
        for (what, grid, even) in &fast {
            for p in &points {
                let p = p.map(|v| v as f32);
                let ((a, ga), (b, gb)) = (grid.sample(p), even.sample(p));
                assert!(a.to_bits() == b.to_bits() && ga.map(f32::to_bits) == gb.map(f32::to_bits), "fast {what} at {p:?}: {a} {ga:?}, the even lattice {b} {gb:?}");
            }
        }
        let exact = [
            ("check", DistGrid::check(Ctl::default(), &solid, finest, Contact::Exact).unwrap(), Tol::LOOSE),
            ("sim", DistGrid::sim(Ctl::default(), &solid, finest, Contact::Exact).unwrap(), Tol::SIM),
        ];
        for (what, grid, tol) in &exact {
            let (_, worst) = grid.flagged();
            assert!(worst as f64 <= 0.25 * finest + 0.005, "exact {what}: a flagged brick's centre {worst:.4} off");
            for p in &points {
                let q = p.map(|v| v as f32);
                let (read, want, cell) = (grid.distance(q) as f64, truth(*p), grid.cell_at(q) as f64);
                let least = grid.least_at(q, read as f32) as f64;
                // f32 coordinates near 2 mm: a few 1e-7 mm
                let err = |e: f64| e + 1e-5;
                assert!(cell >= 0.5 * finest, "exact {what} at {p:?}: a cell of {cell}");
                assert!(least <= err(want), "exact {what} at {p:?}: the least {least:.5} past the truth {want:.5}");
                // A box's distance is convex, and a trilinear read over-reads a convex function by
                // up to half a cell where it kinks: on a crease inside an edge (the faces' bisector)
                // and off an edge or corner. Cells there stop at half a layer, so within a layer
                // of two faces a read may be over by a quarter layer, and `least_at` covers it.
                let mut faces: Vec<f64> = (0..3).map(|k| p[k].abs() - half[k]).collect();
                faces.sort_by(|a, b| b.total_cmp(a));
                let room = if faces[1] > -finest { tol.at(want).max(0.25 * finest) } else { tol.at(want) };
                assert!((read - want).abs() <= err(room), "exact {what} at {p:?}: read {read:.5}, truth {want:.5}");
            }
        }
    }

    /// Exact on the end of a slot through a wall (an ellipsoid cut out of a box, its hollow
    /// narrowing to a 0.3 neck where it breaks the wall's end face), at a 0.2 layer: the neck
    /// reads its 0.3 no less than a quarter layer under and the tolerance over; no cell is finer
    /// than half the layer. Behind the end face the hollow leaves a blade of the wall thinner than
    /// a cell, and a flagged brick's worst error at a cell's centre is within what a blade leaves
    /// in a cell of half the layer, 0.87 of it; the face there reads no more than 0 by `least_at`.
    /// Points about the wall's end are never past the least `least_at` gives: over the truth by no
    /// more than the tolerance, or in a flagged brick (a blade) by no more than 0.87 of a cell and
    /// its share.
    #[test]
    fn exact_reads_a_slots_neck_within_a_quarter_layer() {
        let (neck, c, finest) = (0.3f64, 1.0f64, 0.2f64);
        let wall = 3.0 * (1.0 - (neck / c) * (neck / c)).sqrt();
        // the wall's last 0.6 at the end the neck breaks; its corner is the whole wall's, so the
        // lattices meet the end face where they do on the whole wall
        let (lo, hi) = ([-wall, -1.0, -1.5], [0.6 - wall, 1.0, 1.5]);
        let mid: [f64; 3] = std::array::from_fn(|k| 0.5 * (lo[k] + hi[k]));
        let size: [f64; 3] = std::array::from_fn(|k| hi[k] - lo[k]);
        let end = SdfNode::Transform { at: mid, rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size, radius: 0.0 }) };
        let root = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![end, SdfNode::Ellipsoid { radii: [3.0, 3.0, c] }] };
        let axes = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let solid = Solid::of_field(&SdfShape { root, min: lo, max: hi, materials: vec![] }, axes, None);
        let points: Vec<[f64; 3]> = (0..500).map(|s| {
            let t = s as f64 * 0.618_033_988_75;
            std::array::from_fn(|k| lo[k] - 0.5 + (size[k] + 1.0) * ((t * (k as f64 * 2.0 + 1.0)) % 1.0))
        }).collect();
        let truth = crate::truth::Truth::of(Ctl::default(), &solid, 0.2).unwrap();
        let mut want = vec![0.0; points.len()];
        truth.at(&points, &|_| 0.004, &mut want);
        let tol = Tol::LOOSE;
        let grid = DistGrid::check(Ctl::default(), &solid, finest, Contact::Exact).unwrap();
        let read = grid.distance([-wall as f32, 0.0, 0.0]) as f64;
        assert!(read >= neck - 0.25 * finest - 0.005 && read <= neck + tol.at(neck), "the neck reads {read:.4}");
        let (_, worst) = grid.flagged();
        assert!(worst as f64 <= 0.87 * 0.5 * finest + 0.005, "a flagged brick's centre {worst:.4} off");
        for p in [[-wall, 0.9, 0.1], [-wall, 1.0, 0.1], [-wall, 0.9, 0.2], [-wall, 1.0, 0.2]] {
            let q = p.map(|v| v as f32);
            // f32 coordinates near 3 mm: a few 1e-7 mm off the face
            let least = grid.least_at(q, grid.distance(q)) as f64;
            assert!(least <= 1e-5, "the end face over the hollow at {p:?}, the least {least:.4} past 0");
        }
        for (p, &want) in points.iter().zip(&want) {
            let q = p.map(|v| v as f32);
            let (read, cell, least) = (grid.distance(q) as f64, grid.cell_at(q) as f64, grid.least_at(q, grid.distance(q)) as f64);
            // the reference is within 0.004 of the truth
            assert!(cell >= 0.5 * finest, "{p:?}: a cell of {cell}");
            assert!(least <= want + 0.004, "{p:?}: read {read:.4}, the least {least:.4} past the truth {want:.4}");
        }
    }
}
