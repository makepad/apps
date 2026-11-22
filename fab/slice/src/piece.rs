//! A test print's piece: what lies inside a test cube, printed on a plate of its own.
//!
//! A piece is derived, never a copy. [`PieceSource`] asks the whole part's source, with its paint
//! and its regions of print settings, for the print's own cut planes and clips what it says to
//! the cube's section at each plane, so a piece's layers are the print's on fields and meshes
//! alike. Everything else about a point of the piece (its paint, the settings box over it, a
//! support channel) is read from the original at the same model point: only `z` is mapped, by
//! the layers ([`Stack`]). Where a piece stands on the bed is its slot's offset, so moving it
//! there cuts and plans nothing.
//!
//! The plate of a test print ([`test_input`]) holds one part per part a cube reaches. They stay
//! one block: one shift, one [`Piece::test`], whatever the cube reaches.
//!
//! A piece prints planar: a part's non-planar top is planned on its whole field, and the cube's
//! cuts are no part of it. Turning the cube changes what it cuts, never the part's orientation.
//! An explicit pose carries the part and cube together, then uses that whole part's layers;
//! leading cuts without material are dropped before the piece prints on the bed.

use crate::live::{Affine, LiveInput, Regions};
use fab_plate::machine::Rect;
use fab_plate::{Bounds, Part, PartMesh, Plate};
use makepad_fab_gcode::{record::{flags, MoveKind, MoveRecord}, writer::Stats};
use makepad_csg_cad::region::Region;
use makepad_fab_slicer::clip::{intersection_ex, union, FillRule};
use makepad_fab_slicer::geom::{ExPolygon, Polygon, Pt, SCALE};
use makepad_fab_slicer::paint::{LayerPaint, PaintParams};
use makepad_fab_slicer::source::{ContourSource, PaintAt, SettingsZone, SupportPaint, SupportRegion};
use makepad_fab_toolpath::{cut_z, layers, Params, Role, DROPPED_TIPS};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A test cube on the plate (mm), free to turn about all three axes. Its sections cut the
/// print's layers without turning the part or changing its overhangs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cube {
    pub center: [f64; 3],
    /// Per axis, before its turn.
    pub size: [f64; 3],
    /// About the plate's X, then Y, then Z through its centre (degrees).
    pub turn: [f64; 3],
}

impl Cube {
    /// Row-major rotation; its columns are the cube's axes.
    pub fn axes(&self) -> [[f64; 3]; 3] {
        fab_model::eval::rotation_xyz(self.turn)
    }

    pub fn bottom(&self) -> f64 {
        if self.turn[0] == 0.0 && self.turn[1] == 0.0 {
            return self.center[2] - self.size[2] / 2.0;
        }
        self.corners3().iter().map(|q| q[2]).fold(f64::INFINITY, f64::min)
    }

    pub fn top(&self) -> f64 {
        if self.turn[0] == 0.0 && self.turn[1] == 0.0 {
            return self.center[2] + self.size[2] / 2.0;
        }
        self.corners3().iter().map(|q| q[2]).fold(f64::NEG_INFINITY, f64::max)
    }

    /// Its upright corners in X and Y, counter-clockwise.
    pub fn corners(&self) -> [[f64; 2]; 4] {
        let (s, c) = self.turn[2].to_radians().sin_cos();
        let (hx, hy) = (self.size[0] / 2.0, self.size[1] / 2.0);
        [[-hx, -hy], [hx, -hy], [hx, hy], [-hx, hy]].map(|q| [self.center[0] + c * q[0] - s * q[1], self.center[1] + s * q[0] + c * q[1]])
    }

    /// Its eight corners; each index bit chooses a side of one local axis.
    pub fn corners3(&self) -> [[f64; 3]; 8] {
        if self.turn[0] == 0.0 && self.turn[1] == 0.0 {
            let xy = self.corners();
            return std::array::from_fn(|i| {
                let q = xy[[0, 1, 3, 2][i & 3]];
                [q[0], q[1], if i < 4 { self.bottom() } else { self.top() }]
            });
        }
        let r = self.axes();
        std::array::from_fn(|i| {
            let q: [f64; 3] = std::array::from_fn(|k| if i & (1 << k) == 0 { -self.size[k] / 2.0 } else { self.size[k] / 2.0 });
            std::array::from_fn(|k| self.center[k] + r[k][0] * q[0] + r[k][1] * q[1] + r[k][2] * q[2])
        })
    }

    /// The box of all its corners in X and Y.
    pub fn footprint(&self) -> ([f64; 2], [f64; 2]) {
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for q in self.corners3() {
            for k in 0..2 {
                lo[k] = lo[k].min(q[k]);
                hi[k] = hi[k].max(q[k]);
            }
        }
        (lo, hi)
    }

    /// Its convex section at a print plane, counter-clockwise, empty without area.
    pub fn section(&self, z: f64) -> Vec<[f64; 2]> {
        if self.turn[0] == 0.0 && self.turn[1] == 0.0 {
            return if z < self.bottom() || z > self.top() || self.size[0] <= 0.0 || self.size[1] <= 0.0 { Vec::new() } else { self.corners().to_vec() };
        }
        let corners = self.corners3();
        if corners.iter().all(|q| q[2] < z) || corners.iter().all(|q| q[2] > z) {
            return Vec::new();
        }
        let mut points: Vec<[f64; 2]> = Vec::new();
        // Shared corners and plane-aligned edges contribute each point only once.
        let tolerance = corners.iter().flatten().fold(1.0f64, |m, x| m.max(x.abs())) * 32.0 * f64::EPSILON;
        let mut add = |q: [f64; 2]| {
            if !points.iter().any(|p| (p[0] - q[0]).abs() <= tolerance && (p[1] - q[1]).abs() <= tolerance) {
                points.push(q);
            }
        };
        for i in 0..8 {
            for k in 0..3 {
                let j = i ^ (1 << k);
                if j <= i {
                    continue;
                }
                let (a, b) = (corners[i], corners[j]);
                if a[2] == z {
                    add([a[0], a[1]]);
                }
                if b[2] == z {
                    add([b[0], b[1]]);
                }
                if (a[2] < z && z < b[2]) || (b[2] < z && z < a[2]) {
                    let t = (z - a[2]) / (b[2] - a[2]);
                    add([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
                }
            }
        }
        if points.len() < 3 {
            return Vec::new();
        }
        let center: [f64; 2] = std::array::from_fn(|k| points.iter().map(|q| q[k]).sum::<f64>() / points.len() as f64);
        points.sort_by(|a, b| (a[1] - center[1]).atan2(a[0] - center[0]).total_cmp(&(b[1] - center[1]).atan2(b[0] - center[0])));
        let area: f64 = points.iter().zip(points.iter().cycle().skip(1)).map(|(a, b)| (a[0] - center[0]) * (b[1] - center[1]) - (a[1] - center[1]) * (b[0] - center[0])).sum();
        if area <= 0.0 {
            points.clear();
        }
        points
    }

    /// Its section in a frame that sits at `off` on the plate.
    pub fn outline_at(&self, z: f64, off: Pt) -> Vec<ExPolygon> {
        let section = self.section(z);
        if section.is_empty() {
            return Vec::new();
        }
        vec![ExPolygon::new(Polygon::new(section.iter().map(|q| Pt::from_mm(q[0], q[1]).sub(off)).collect()))]
    }

    /// Whether its body reaches into the box, tested on all separating axes.
    pub(crate) fn reaches(&self, b: &Bounds) -> bool {
        let r = self.axes();
        let axes: [[f64; 3]; 3] = std::array::from_fn(|k| [r[0][k], r[1][k], r[2][k]]);
        let world = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let half: [f64; 3] = std::array::from_fn(|k| (b.max[k] - b.min[k]) / 2.0);
        let delta: [f64; 3] = std::array::from_fn(|k| (b.max[k] + b.min[k]) / 2.0 - self.center[k]);
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let overlaps = |n: [f64; 3]| {
            n == [0.0; 3] || dot(n, delta).abs() < (0..3).map(|k| half[k] * n[k].abs() + self.size[k] / 2.0 * dot(n, axes[k]).abs()).sum::<f64>()
        };
        world.iter().chain(&axes).copied().all(overlaps) && world.iter().all(|a| axes.iter().all(|b| overlaps([a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]])))
    }

    /// Its six sides as half-spaces it keeps: (outward normal, `d`), `n · p <= d` inside.
    pub fn sides(&self) -> [([f64; 3], f64); 6] {
        let r = self.axes();
        std::array::from_fn(|i| {
            let k = i / 2;
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            let n = [sign * r[0][k], sign * r[1][k], sign * r[2][k]];
            (n, n[0] * self.center[0] + n[1] * self.center[1] + n[2] * self.center[2] + self.size[k] / 2.0)
        })
    }
}

/// A piece's layers: the print's whose cut plane lies inside the cube, the lowest on the bed at
/// the profile's first layer height, the others as thick as they print in the full print.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stack {
    /// As the piece prints them: (top z, height), bottom up, before a raft's lift.
    heights: Arc<[(f64, f64)]>,
    /// Each layer's cut plane in the piece's frame, ascending.
    own: Vec<f64>,
    /// The plane the full print cuts that layer at.
    print: Vec<f64>,
    /// The layer boundaries, in the piece and in the full print: `z` maps by them.
    from: Vec<f64>,
    to: Vec<f64>,
}

impl Stack {
    /// A piece prints from its lowest material, wherever the cube's bottom lies: only the empty
    /// leading cuts go. The planes stay cached for its contour source, so finding the first
    /// material never slices the whole part twice.
    pub(crate) fn grounded(&self, print: &[(f64, f64)], p: &Params, src: &(dyn ContourSource + Send + Sync), planes: &Planes, cube: &Cube, off: Pt) -> Stack {
        let cuts = planes.cut(src, &self.print);
        match cuts.iter().zip(&self.print).find(|(regions, z)| !intersection_ex(regions, &cube.outline_at(**z, off)).is_empty()) {
            Some((_, z)) => Stack::of(print, p, *z, cube.top()),
            None => Stack::default(),
        }
    }

    /// From the full print's layers (`print`, as [`layers::object_heights`] gives them) and the
    /// cube's bottom and top.
    pub fn of(print: &[(f64, f64)], p: &Params, bottom: f64, top: f64) -> Stack {
        let lift = if p.raft_layers > 0 { p.raft_lift } else { 0.0 };
        let plane = |l: &(f64, f64)| cut_z(p, l.0, l.1);
        let lowest = print.partition_point(|l| plane(l) < bottom);
        let kept = &print[lowest..lowest + print[lowest..].partition_point(|l| plane(l) < top)];
        let mut s = Stack::default();
        let Some(first) = kept.first() else { return s };
        s.from.push(0.0);
        s.to.push(first.0 - first.1 - lift);
        let mut heights = Vec::with_capacity(kept.len());
        let mut z = 0.0;
        for (i, l) in kept.iter().enumerate() {
            let h = if i == 0 { layers::first_height(p) } else { l.1 };
            z += h;
            heights.push((z, h));
            s.own.push(cut_z(p, z + lift, h));
            s.print.push(plane(l));
            s.from.push(z);
            s.to.push(l.0 - lift);
        }
        s.heights = heights.into();
        s
    }

    /// The layers it prints.
    pub fn heights(&self) -> &[(f64, f64)] {
        &self.heights
    }

    /// The piece's top.
    pub fn top(&self) -> f64 {
        self.from.last().copied().unwrap_or(0.0)
    }

    /// How far the full print's layers above the first sit over the piece's (mm).
    pub fn fall(&self) -> f64 {
        match (self.to.last(), self.from.last()) {
            (Some(to), Some(from)) => to - from,
            _ => 0.0,
        }
    }

    /// A height of the piece as the full print's: through the layer it lies in, past either end
    /// as far beyond.
    pub fn to_print(&self, z: f64) -> f64 {
        Self::through(&self.from, &self.to, z)
    }

    /// A height of the full print as the piece's.
    pub fn from_print(&self, z: f64) -> f64 {
        Self::through(&self.to, &self.from, z)
    }

    fn through(a: &[f64], b: &[f64], z: f64) -> f64 {
        let (Some(first), Some(last)) = (a.first(), a.last()) else { return z };
        if z <= *first {
            return b[0] + (z - first);
        }
        if z >= *last {
            return b[b.len() - 1] + (z - last);
        }
        let k = a.partition_point(|x| *x <= z);
        b[k - 1] + (z - a[k - 1]) / (a[k] - a[k - 1]) * (b[k] - b[k - 1])
    }

    /// The full print's cut plane of a plane of the piece: its layer's exactly, so the contours
    /// are the print's to the bit.
    fn plane(&self, z: f64) -> f64 {
        let k = self.own.partition_point(|x| *x < z);
        let near = [k.wrapping_sub(1), k].into_iter().filter(|&i| i < self.own.len()).find(|&i| (self.own[i] - z).abs() < 1e-6);
        near.map_or_else(|| self.to_print(z), |i| self.print[i])
    }
}

/// What has been cut of a whole part for its pieces, by plane: another cube over the same layers
/// cuts nothing of it again.
pub struct Planes {
    by_z: Mutex<HashMap<u64, Regions>>,
    /// Planes cut of whole parts, over the engine.
    cut: Arc<AtomicU64>,
}

impl Planes {
    pub(crate) fn new(cut: Arc<AtomicU64>) -> Planes {
        Planes { by_z: Mutex::default(), cut }
    }

    /// What the whole part's own slice cut of the planes `stack` reads: `layers` by plane.
    pub(crate) fn seed(&self, layers: impl Iterator<Item = (f64, Regions)>, stack: &Stack) {
        let mut by_z = self.by_z.lock().unwrap();
        for (z, regions) in layers {
            if stack.print.binary_search_by(|p| p.total_cmp(&z)).is_ok() {
                by_z.entry(z.to_bits()).or_insert(regions);
            }
        }
    }

    fn cut(&self, src: &(dyn ContourSource + Send + Sync), zs: &[f64]) -> Vec<Regions> {
        let missing: Vec<f64> = {
            let by_z = self.by_z.lock().unwrap();
            zs.iter().copied().filter(|z| !by_z.contains_key(&z.to_bits())).collect()
        };
        // Not under the lock: the parallel chunks of a layer's trace share it.
        let made = if missing.is_empty() { Vec::new() } else { src.contours(&missing) };
        self.cut.fetch_add(made.len() as u64, Ordering::Relaxed);
        let mut by_z = self.by_z.lock().unwrap();
        for (z, regions) in missing.iter().zip(made) {
            by_z.insert(z.to_bits(), regions.into());
        }
        zs.iter().map(|z| by_z[&z.to_bits()].clone()).collect()
    }
}

/// A whole part's source cut to a cube, in the part's frame in X and Y and on the bed in Z.
pub struct PieceSource {
    inner: Arc<dyn ContourSource + Send + Sync>,
    planes: Arc<Planes>,
    cube: Cube,
    off: Pt,
    stack: Arc<Stack>,
    bounds: ([f64; 3], [f64; 3]),
    xf: Option<Affine>,
}

impl PieceSource {
    pub fn new(inner: Arc<dyn ContourSource + Send + Sync>, planes: Arc<Planes>, cube: Cube, off: Pt, stack: Arc<Stack>, xf: Option<Affine>) -> PieceSource {
        let (lo, hi) = inner.bounds();
        let (mut min, mut max) = ([lo[0], lo[1], 0.0], [hi[0], hi[1], stack.top()]);
        let (c0, c1) = cube.footprint();
        let (c0, c1) = (Pt::from_mm(c0[0], c0[1]).sub(off), Pt::from_mm(c1[0], c1[1]).sub(off));
        for k in 0..2 {
            min[k] = min[k].max([c0.x, c0.y][k] as f64 / SCALE);
            max[k] = max[k].min([c1.x, c1.y][k] as f64 / SCALE);
        }
        PieceSource { inner, planes, cube, off, stack, bounds: (min, max), xf }
    }

    /// Its content key from the whole part's: the cube in the part's frame, and its layers. Where
    /// the piece stands on the bed is no part of it.
    pub fn key(&self, whole: u64) -> u64 {
        let mut h = whole ^ 0x7069_6563_6521_u64;
        let mut eat = |x: u64| h = (h ^ x).wrapping_mul(0x100_0000_01b3);
        for q in self.cube.corners3() {
            let p = Pt::from_mm(q[0], q[1]).sub(self.off);
            eat(p.x as u64);
            eat(p.y as u64);
            eat(q[2].to_bits());
        }
        for (z, height) in self.stack.heights.iter() {
            eat(z.to_bits());
            eat(height.to_bits());
        }
        for z in &self.stack.print {
            eat(z.to_bits());
        }
        if let Some(xf) = &self.xf {
            for v in xf.iter().flatten() {
                eat(v.to_bits());
            }
        }
        h
    }

    /// A region of the original as the piece reads it, if it reaches into the cube: none lets a
    /// region clear of the cube change nothing of the piece, to the byte.
    fn region(&self, r: &SupportRegion) -> Option<SupportRegion> {
        let (min, max) = self.bounds;
        let (z0, z1) = (self.stack.to_print(0.0), self.stack.to_print(max[2]));
        if let Some((lo, hi)) = r.bounds {
            if !(lo[0] < max[0] && min[0] < hi[0] && lo[1] < max[1] && min[1] < hi[1] && lo[2] < z1 && z0 < hi[2]) {
                return None;
            }
        }
        let (at, stack) = (r.at.clone(), self.stack.clone());
        let bounds = r.bounds.map(|(lo, hi)| ([lo[0], lo[1], self.stack.from_print(lo[2])], [hi[0], hi[1], self.stack.from_print(hi[2])]));
        Some(SupportRegion::new(Arc::new(move |x, y, z| at(x, y, stack.to_print(z))), bounds))
    }
}

impl ContourSource for PieceSource {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.bounds
    }
    /// The whole part's: a piece's infill lies on the grid of the full print's.
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.inner.pivot_box()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        let planes: Vec<f64> = zs.iter().map(|z| self.stack.plane(*z)).collect();
        self.planes.cut(&*self.inner, &planes).iter().zip(&planes).map(|(regions, z)| intersection_ex(regions, &self.cube.outline_at(*z, self.off))).collect()
    }
    fn painted(&self) -> bool {
        self.inner.painted()
    }
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
        let planes: Vec<f64> = zs.iter().map(|z| self.stack.plane(*z)).collect();
        self.inner.paint_regions(&planes, slices, p)
    }
    fn paint_at(&self) -> Option<PaintAt> {
        let (at, stack) = (self.inner.paint_at()?, self.stack.clone());
        Some(Arc::new(move |x, y, z| at(x, y, stack.to_print(z))))
    }
    fn support_paint(&self) -> Option<SupportPaint> {
        let p = self.inner.support_paint()?;
        let of = |r: &Option<SupportRegion>| r.as_ref().and_then(|r| self.region(r));
        Some(SupportPaint { block: of(&p.block), enforce: of(&p.enforce), no_trees: of(&p.no_trees) })
    }
    fn settings_zones(&self) -> Vec<SettingsZone> {
        self.inner.settings_zones().into_iter().filter_map(|z| Some(SettingsZone { region: self.region(&z.region)?, values: z.values })).collect()
    }
    fn layer_stack(&self) -> Option<Arc<[(f64, f64)]>> {
        Some(self.stack.heights.clone())
    }
}

/// One test print to slice.
#[derive(Clone, Debug, PartialEq)]
pub struct TestPrint {
    pub name: String,
    pub cube: Cube,
    /// Row-major rotation in the plate's frame, from as printed to posed.
    pub pose: Option<[[f64; 3]; 3]>,
    /// A brim of its own: outside its first layer, at the profile's brim width (Fab's where the
    /// profile's brim takes none).
    pub brim: bool,
}

impl TestPrint {
    /// The cube on the bed and the rigid map carrying the original plate into that pose.
    fn posed(&self) -> (Cube, Option<Affine>) {
        let Some(r) = self.pose else { return (self.cube, None) };
        let axes = self.cube.axes();
        let turned = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| r[i][k] * axes[k][j]).sum()));
        let mut cube = Cube { turn: fab_model::eval::euler_xyz(&turned), ..self.cube };
        cube.center[2] = 0.0;
        cube.center[2] = -cube.bottom();
        let xf = std::array::from_fn(|i| [r[i][0], r[i][1], r[i][2], cube.center[i] - (0..3).map(|k| r[i][k] * self.cube.center[k]).sum::<f64>()]);
        (cube, Some(xf))
    }
}

/// One part of a test plate.
#[derive(Clone)]
pub struct Piece {
    /// Which test print it is of: every part a cube reaches is one piece to the person.
    pub test: usize,
    pub cube: Cube,
    /// Plate to posed coordinates; absent for an as-printed piece.
    pub xf: Option<Affine>,
    /// Where its cube moved to print (plate mm): the same for all of the cube's parts.
    pub shift: [f64; 2],
    /// The piece as the plate's checks take it, where it prints.
    pub part: Part,
}

/// A test plate's parts, in the input's part order.
#[derive(Clone)]
pub struct Pieces {
    /// The id of the plate they are cut from.
    pub whole_plate: u64,
    pub parts: Vec<Piece>,
}

/// What the slice does for one test print. Times in seconds, weights in grams.
pub struct PieceRead {
    pub test: usize,
    pub name: String,
    /// Its own moves' time: what it prints, the travels and retractions that belong to it.
    pub seconds: f64,
    pub grams: f64,
    /// Of `grams`, what goes into supports and brim.
    pub support_grams: f64,
    /// Of `support_grams`, what goes into its brim: the profile's or its own.
    pub brim_grams: f64,
    /// Filament length per slot (mm), the signed sum of its records' `e`.
    pub filament_mm: Vec<f64>,
    /// The printed extent on the bed: the box of its extrusions.
    /// Empty prints have zero bounds, layers and footprint.
    pub bed_min: [f64; 3],
    pub bed_max: [f64; 3],
    /// First and last layer index it prints on, of the job's layers (zero-based).
    pub layers: (u32, u32),
    /// Width and depth of its first printed layer's extrusion box (mm).
    pub footprint: [f64; 2],
    /// Height above the bed divided by that box's shorter side; zero without a footprint.
    pub height_over_footprint: f64,
}

pub struct PieceReads {
    pub pieces: Vec<PieceRead>,
    /// What belongs to none of them (the start sequence, wipes, purge, the moves between them).
    pub shared_seconds: f64,
    pub shared_grams: f64,
    pub total_seconds: f64,
    pub total_grams: f64,
}

/// Read the slice's records (in print order) without planning or cutting. Object lookup and slot
/// conversion are prepared once; the move scan holds one layer's records at a time. Only
/// unlabelled support/brim/skirt extrusions need a nearest-footprint search. The writer normally
/// labels supports and brim with their part.
pub fn read_pieces(records: impl Iterator<Item = MoveRecord>, stats: &Stats, pieces: &Pieces) -> PieceReads {
    let mut order: Vec<usize> = (0..pieces.parts.len()).collect();
    order.sort_by_key(|&i| pieces.parts[i].test);
    let mut objects = vec![0; pieces.parts.len()];
    let mut reads: Vec<PieceRead> = Vec::new();
    let mut boxes = Vec::new();
    for i in order {
        let p = &pieces.parts[i];
        if reads.last().is_none_or(|r| r.test != p.test) {
            let (lo, hi) = p.cube.footprint();
            boxes.push((std::array::from_fn::<_, 2, _>(|k| lo[k] + p.shift[k]), std::array::from_fn::<_, 2, _>(|k| hi[k] + p.shift[k])));
            reads.push(PieceRead {
                test: p.test, name: p.part.name.clone(), seconds: 0.0, grams: 0.0, support_grams: 0.0, brim_grams: 0.0,
                filament_mm: vec![0.0; stats.filament_mm.len()],
                bed_min: [f64::INFINITY; 3], bed_max: [f64::NEG_INFINITY; 3], layers: (u32::MAX, 0),
                footprint: [0.0; 2], height_over_footprint: 0.0,
            });
        }
        objects[i] = reads.len() - 1;
    }
    let grams_per_mm: Vec<f64> = stats.filament_mm.iter().zip(&stats.filament_g).map(|(&mm, &g)| if mm == 0.0 { 0.0 } else { g / mm }).collect();
    let mut first_boxes = vec![([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]); reads.len()];
    let (mut records, mut chunk) = (records.peekable(), Vec::new());
    while let Some(first) = records.next() {
        // A layer's records.
        chunk.clear();
        chunk.push(first);
        while let Some(m) = records.next_if(|m| m.layer == first.layer) {
            chunk.push(m);
        }
        // Nothing before the first or after the last labelled object record is object time.
        let object_record = |m: &MoveRecord| m.object != u16::MAX && (m.object as usize) < objects.len();
        let window = chunk.iter().position(object_record).zip(chunk.iter().rposition(object_record));
        for (index, m) in chunk.iter().enumerate() {
            let extrudes = m.kind == MoveKind::Extrude && m.e > 0.0;
            let support = matches!(m.role, Role::Support | Role::SupportInterface | Role::SupportTransition | Role::Brim);
            let owner = if m.object != u16::MAX {
                objects.get(m.object as usize).copied()
            } else if extrudes && support {
                let mut nearest = None;
                let mut distance = f64::INFINITY;
                for (i, (lo, hi)) in boxes.iter().enumerate() {
                    let dx = (lo[0] - m.p0[0] as f64).max(m.p0[0] as f64 - hi[0]).max(0.0);
                    let dy = (lo[1] - m.p0[1] as f64).max(m.p0[1] as f64 - hi[1]).max(0.0);
                    let d = dx * dx + dy * dy;
                    if d < distance {
                        distance = d;
                        nearest = Some(i);
                    }
                }
                nearest
            } else {
                None
            };
            let Some(owner) = owner else { continue };
            let r = &mut reads[owner];
            if window.is_some_and(|(first, last)| first <= index && index <= last) {
                r.seconds += m.t_end - m.t_start;
            }
            if m.filament != 0 {
                let slot = m.filament as usize - 1;
                r.filament_mm[slot] += m.e as f64;
                if extrudes && support {
                    let grams = m.e as f64 * grams_per_mm[slot];
                    r.support_grams += grams;
                    if m.role == Role::Brim {
                        r.brim_grams += grams;
                    }
                }
            }
            if !extrudes { continue; }
            let layer = m.layer;
            if layer < r.layers.0 {
                r.layers.0 = layer;
                first_boxes[owner] = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
            }
            r.layers.1 = r.layers.1.max(layer);
            let mut add_point = |q: [f64; 3]| {
                for k in 0..3 {
                    r.bed_min[k] = r.bed_min[k].min(q[k]);
                    r.bed_max[k] = r.bed_max[k].max(q[k]);
                }
                if layer == r.layers.0 {
                    for k in 0..2 {
                        first_boxes[owner].0[k] = first_boxes[owner].0[k].min(q[k]);
                        first_boxes[owner].1[k] = first_boxes[owner].1[k].max(q[k]);
                    }
                }
            };
            add_point(m.p0.map(f64::from));
            add_point(m.p1.map(f64::from));
            // An arc can reach beyond both endpoints. Only its swept cardinal points can
            // enlarge the box; Z is linear along the arc and already bounded by its ends.
            if m.flags & flags::ARC != 0 {
                let center = m.center.map(f64::from);
                let dx = m.p0[0] as f64 - center[0];
                let dy = m.p0[1] as f64 - center[1];
                let radius = dx.hypot(dy);
                let angle = dy.atan2(dx);
                for (i, axis) in [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]].iter().enumerate() {
                    let delta = i as f64 * std::f64::consts::FRAC_PI_2 - angle;
                    let swept = (delta * (m.sweep as f64).signum()).rem_euclid(std::f64::consts::TAU);
                    if swept <= (m.sweep as f64).abs() {
                        add_point([center[0] + radius * axis[0], center[1] + radius * axis[1], m.p0[2] as f64]);
                    }
                }
            }
        }
    }
    let total_grams = stats.filament_g.iter().sum();
    let mut out = PieceReads { pieces: reads, shared_seconds: stats.total_time, shared_grams: total_grams, total_seconds: stats.total_time, total_grams };
    for (r, (lo, hi)) in out.pieces.iter_mut().zip(first_boxes) {
        r.grams = r.filament_mm.iter().zip(&grams_per_mm).map(|(mm, g)| mm * g).sum();
        // Stats count deposited filament and macro purge, whereas records also carry signed
        // retractions. Leave that balance, float rounding and time between records shared.
        out.shared_seconds -= r.seconds;
        out.shared_grams -= r.grams;
        if r.layers.0 == u32::MAX {
            r.bed_min = [0.0; 3];
            r.bed_max = [0.0; 3];
            r.layers = (0, 0);
        } else {
            r.footprint = [hi[0] - lo[0], hi[1] - lo[1]];
            let short = r.footprint[0].min(r.footprint[1]);
            if short > 0.0 {
                r.height_over_footprint = r.bed_max[2] / short;
            }
        }
    }
    out
}

impl Piece {
    /// Inside the cube where the piece prints: `shift` over, from the bed up to the stack's top
    /// (negative inside, the distance's lower bound outside).
    pub(crate) fn solid(&self, stack: &Stack) -> impl Fn([f64; 3]) -> f64 + Send + Sync + 'static {
        let (s, c) = self.cube.turn[2].to_radians().sin_cos();
        let at = [self.cube.center[0] + self.shift[0], self.cube.center[1] + self.shift[1]];
        let (hx, hy, top) = (self.cube.size[0] / 2.0, self.cube.size[1] / 2.0, stack.top());
        let tilted = (self.cube.turn[0] != 0.0 || self.cube.turn[1] != 0.0).then(|| (self.cube.sides(), stack.clone()));
        let shift = self.shift;
        move |q| {
            if let Some((sides, stack)) = &tilted {
                let p = [q[0] - shift[0], q[1] - shift[1], stack.to_print(q[2])];
                return sides.iter().map(|(n, d)| n[0] * p[0] + n[1] * p[1] + n[2] * p[2] - d).fold(f64::NEG_INFINITY, f64::max).max(-q[2]).max(q[2] - top);
            }
            let (dx, dy) = (q[0] - at[0], q[1] - at[1]);
            let (x, y) = (c * dx + s * dy, c * dy - s * dx);
            (x.abs() - hx).max(y.abs() - hy).max(-q[2]).max(q[2] - top)
        }
    }
}

/// Where each test print's block stands: the plate's normal spacing between their footprints, by
/// the same packing the plate's own free objects take. `blocks`: per test, its footprint's box
/// and height, and the bed it may stand on. Each one's shift from where it is, or why they can't
/// all stand.
pub fn arrange(machine: &fab_plate::machine::Machine, names: &[String], blocks: &[(([f64; 2], [f64; 2]), f64, Rect)]) -> Result<Vec<[f64; 2]>, String> {
    let ids: Vec<String> = (0..blocks.len()).map(|i| i.to_string()).collect();
    let mut items: Vec<(&str, [f64; 2], f64, Rect)> = blocks.iter().zip(&ids).map(|((b, h, r), id)| (id.as_str(), [b.1[0] - b.0[0], b.1[1] - b.0[1]], *h, *r)).collect();
    let (slots, no_room) = fab_model::eval::arrange(machine, &mut items, false);
    if !no_room.is_empty() {
        let list: Vec<&str> = no_room.iter().filter_map(|id| id.parse::<usize>().ok().and_then(|i| names.get(i)).map(String::as_str)).collect();
        return Err(format!("The test prints do not fit on the bed: no room for {}. Tick fewer, or make the cubes smaller.", list.join(", ")));
    }
    let mut shifts = vec![[0.0; 2]; blocks.len()];
    for (id, at) in slots {
        let i: usize = id.parse().map_err(|_| "a test print's slot".to_string())?;
        let b = &blocks[i].0;
        shifts[i] = [at[0] - (b.0[0] + b.1[0]) / 2.0, at[1] - (b.0[1] + b.1[1]) / 2.0];
    }
    Ok(shifts)
}

/// Where these test prints stand on the bed, and what of the plate each cuts: all a test plate
/// asks of the whole plate but its contents, so the thread that asks for one can say what's wrong
/// with it, and its id, without copying the plate.
#[derive(Clone, Debug, PartialEq)]
pub struct TestPlan {
    /// Per test, the indices of the plate's parts it reaches: its parts, in the test plate's order.
    pub reached: Vec<Vec<usize>>,
    /// Per test, where its block moved to print (plate mm).
    pub shifts: Vec<[f64; 2]>,
}

impl TestPlan {
    /// How many parts the test plate has.
    pub fn parts(&self) -> usize {
        self.reached.iter().map(Vec::len).sum()
    }

    /// Each part of the test plate's id, as its beads name it, `of` being the whole plate's ids.
    pub fn objects(&self, of: &[&str]) -> Vec<String> {
        self.reached.iter().enumerate().flat_map(|(test, parts)| parts.iter().map(move |&i| format!("{}#{test}", of[i]))).collect()
    }
}

/// The plan of these test prints on `plate`, or what's wrong with them, in words that say which and
/// what would fix it.
pub fn test_plan(plate: &Plate, machine: &fab_plate::machine::Machine, tests: &[TestPrint]) -> Result<TestPlan, String> {
    // Each test's parts, its block and what it can reach.
    let mut reached: Vec<Vec<usize>> = Vec::with_capacity(tests.len());
    let mut blocks = Vec::with_capacity(tests.len());
    for t in tests {
        let parts: Vec<usize> = (0..plate.parts.len()).filter(|&i| t.cube.reaches(&plate.parts[i].bounds())).collect();
        if parts.is_empty() {
            return Err(format!("{}: nothing of the plate is inside this cube. Move it onto the model.", t.name));
        }
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        let (mut bottom, mut top) = (f64::INFINITY, f64::NEG_INFINITY);
        let (c0, c1) = t.cube.footprint();
        let mut nozzles = Vec::new();
        for &i in &parts {
            let b = plate.parts[i].bounds();
            for k in 0..2 {
                lo[k] = lo[k].min(b.min[k].max(c0[k]));
                hi[k] = hi[k].max(b.max[k].min(c1[k]));
            }
            bottom = bottom.min(b.min[2].max(t.cube.bottom()));
            top = top.max(b.max[2].min(t.cube.top()));
            nozzles.extend(plate.parts[i].nozzles());
        }
        let (cube, xf) = t.posed();
        let (footprint, height) = if xf.is_some() { (cube.footprint(), cube.top() - cube.bottom()) } else { ((lo, hi), top - bottom) };
        blocks.push((footprint, height, machine.reach(&nozzles)));
        reached.push(parts);
    }
    let names: Vec<String> = tests.iter().map(|t| t.name.clone()).collect();
    Ok(TestPlan { reached, shifts: arrange(machine, &names, &blocks)? })
}

/// The id of the plate of these test prints, cut from the plate of id `whole_plate`.
pub fn test_plate_id(whole_plate: u64, tests: &[TestPrint], plan: &TestPlan) -> u64 {
    let mut id = whole_plate ^ 0x7465_7374_7321_u64;
    let mut eat = |x: u64| id = (id ^ x).wrapping_mul(0x100_0000_01b3);
    for (t, s) in tests.iter().zip(&plan.shifts) {
        for v in t.cube.center.iter().chain(&t.cube.size).chain(&t.cube.turn).chain(s) {
            eat(v.to_bits());
        }
        if let Some(pose) = &t.pose {
            for v in pose.iter().flatten() {
                eat(v.to_bits());
            }
        }
        if t.brim {
            eat(1);
        }
    }
    id
}

/// The plate of these test prints cut from the whole plate `whole`, for the same engine, job and
/// checks: the job is the whole plate's, so a test plate and the plate it is cut from keep each
/// other's cuts and plans. All its pieces print in one job, layer by layer, whatever the plate's
/// own sequence.
pub fn test_input(whole: &LiveInput, tests: &[TestPrint]) -> Result<LiveInput, String> {
    let plan = test_plan(&whole.plate, &whole.machine, tests)?;
    Ok(planned_input(whole, tests, &plan))
}

/// [`test_input`], its plan made.
pub fn planned_input(whole: &LiveInput, tests: &[TestPrint], plan: &TestPlan) -> LiveInput {
    let plate = &whole.plate;
    let (reached, shifts) = (&plan.reached, &plan.shifts);
    let mut parts = Vec::new();
    let mut pieces = Vec::new();
    let mut frames = whole.frames.as_ref().map(|_| Vec::new());
    let mut paint = Vec::new();
    for (test, (t, of)) in tests.iter().zip(reached).enumerate() {
        let (cube, xf) = t.posed();
        for &i in of {
            let p = &plate.parts[i];
            let named = named_part(p, t, test);
            let (part, _) = shown_part(&named, &t.cube, Some((&cube, xf.as_ref(), shifts[test])));
            pieces.push(Piece { test, cube, xf, shift: shifts[test], part });
            parts.push(named);
            if let (Some(out), Some(from)) = (frames.as_mut(), whole.frames.as_ref()) {
                out.push(from[i].clone());
            }
            paint.push(whole.paint.get(i).cloned().unwrap_or_default());
        }
    }
    LiveInput {
        plate_id: test_plate_id(whole.plate_id, tests, plan),
        settings_id: whole.settings_id,
        setup: {
            let setup = whole.setup.clone();
            let own_brims = tests.iter().any(|t| t.brim);
            Arc::new(move |kept| {
                let mut job = setup(kept)?;
                let values = &mut job.process.values;
                values.set_str("print_sequence", "by layer");
                // A brim of its own is the profile's brim width; where the profile's brim takes no
                // width (auto, or none) and sets none, Fab's. No other piece's brim reads it then.
                let takes_none = values.f64("brim_width") <= 0.0 && !matches!(values.str("brim_type"), "outer_only" | "inner_only" | "outer_and_inner");
                if let Some(fabs) = fab_plate::print_setting("brim_width").filter(|_| own_brims && takes_none) {
                    values.set_str("brim_width", fabs.default);
                }
                Ok(job)
            })
        },
        plate: Arc::new(Plate { excluded: Vec::new(), parts, settings: plate.settings.clone() }),
        paint: Arc::new(paint),
        frames: frames.map(Arc::new),
        machine: whole.machine.clone(),
        np_head: None,
        pieces: Some(Arc::new(Pieces { whole_plate: whole.plate_id, parts: pieces })),
    }
}

/// A plate part as a part of test `test`: its own by the test, so a cube's parts glue and fuse as
/// the part's do, each cube apart.
fn named_part(p: &Part, t: &TestPrint, test: usize) -> Part {
    let mut print: Vec<(String, String)> = p.print.iter().filter(|(k, _)| k != DROPPED_TIPS && !(t.brim && k == "brim_type")).cloned().collect();
    if t.brim {
        print.push(("brim_type".into(), "outer_only".into()));
    }
    Part { object: format!("{}#{test}", p.object), name: t.name.clone(), body_of: p.body_of.as_ref().map(|b| format!("{b}#{test}")), fused: p.fused.iter().map(|f| format!("{f}#{test}")).collect(), print, ..p.clone() }
}

/// The part a piece is for the checks and the views: a field part's box, a mesh part's mesh clipped
/// and closed. `at` is where it prints (its posed cube, its pose, its shift); with none it stays
/// where it is cut. With what takes the plate to where it stands.
fn shown_part(p: &Part, original: &Cube, at: Option<(&Cube, Option<&Affine>, [f64; 2])>) -> (Part, Affine) {
    let (cube, xf, shift) = at.unwrap_or((original, None, [0.0; 2]));
    let moved = |m: [f64; 3], drop: f64| [m[0] + shift[0], m[1] + shift[1], m[2] - drop];
    let posed = |q: [f64; 3]| match xf {
        Some(m) => std::array::from_fn(|i| m[i][0] * q[0] + m[i][1] * q[1] + m[i][2] * q[2] + m[i][3]),
        None => q,
    };
    let to_bed = |drop: f64| -> Affine {
        let turn = xf.copied().unwrap_or([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]);
        let by = [shift[0], shift[1], -drop];
        std::array::from_fn(|r| {
            let mut row = turn[r];
            row[3] += by[r];
            row
        })
    };
    match p.field_bounds {
        Some(b) => {
            let b = if xf.is_some() {
                let mut out = Bounds { min: [f64::INFINITY; 3], max: [f64::NEG_INFINITY; 3] };
                for i in 0..8 {
                    let q = posed(std::array::from_fn(|k| if i & (1 << k) == 0 { b.min[k] } else { b.max[k] }));
                    for k in 0..3 {
                        out.min[k] = out.min[k].min(q[k]);
                        out.max[k] = out.max[k].max(q[k]);
                    }
                }
                out
            } else { b };
            let (c0, c1) = cube.footprint();
            let bottom = b.min[2].max(cube.bottom());
            let drop = if at.is_some() { bottom } else { 0.0 };
            let lo = [b.min[0].max(c0[0]), b.min[1].max(c0[1]), bottom];
            let hi = [b.max[0].min(c1[0]), b.max[1].min(c1[1]), b.max[2].min(cube.top())];
            (Part { field_bounds: Some(Bounds { min: moved(lo, drop), max: moved(hi, drop) }), field_floor: None, print_mesh: None, mesh: PartMesh { positions: Vec::new(), triangles: Vec::new() }, ..p.clone() }, to_bed(drop))
        }
        None => {
            let mut mesh = clipped_mesh(&p.mesh, original);
            if xf.is_some() {
                for q in &mut mesh.positions {
                    *q = posed(*q);
                }
            }
            let drop = if at.is_some() { mesh.positions.iter().map(|q| q[2]).fold(f64::INFINITY, f64::min) } else { 0.0 };
            for q in &mut mesh.positions {
                *q = moved(*q, drop);
            }
            (Part { mesh, field_bounds: None, field_floor: None, print_mesh: None, ..p.clone() }, to_bed(drop))
        }
    }
}

/// One part of a test print's piece as the views show it, with no slice asked.
pub struct ShownPart {
    /// The plate part it is cut from.
    pub of: usize,
    /// Its id and name, a mesh part's mesh cut and closed, a field part's box.
    pub part: Part,
    /// From the plate to where it stands (row-major, rigid).
    pub to_bed: Affine,
    /// Its cube, on the plate.
    pub cube: Cube,
}

/// Each part of these pieces, standing where it prints, or `on_bed` false, where it is on the plate.
pub fn shown_parts(plate: &Plate, tests: &[TestPrint], plan: &TestPlan, on_bed: bool) -> Vec<ShownPart> {
    let mut out = Vec::new();
    for (test, (t, of)) in tests.iter().zip(&plan.reached).enumerate() {
        let (cube, xf) = t.posed();
        for &i in of {
            let at = on_bed.then_some((&cube, xf.as_ref(), plan.shifts[test]));
            let (part, to_bed) = shown_part(&named_part(&plate.parts[i], t, test), &t.cube, at);
            out.push(ShownPart { of: i, part, to_bed, cube: t.cube });
        }
    }
    out
}

/// A plate-frame mesh cut to a cube and closed, standing where `to_bed` takes it.
pub fn shown_mesh(mesh: &PartMesh, cube: &Cube, to_bed: &Affine) -> PartMesh {
    let mut out = clipped_mesh(mesh, cube);
    for q in &mut out.positions {
        *q = std::array::from_fn(|i| to_bed[i][0] * q[0] + to_bed[i][1] * q[1] + to_bed[i][2] * q[2] + to_bed[i][3]);
    }
    out
}

impl Cube {
    /// Its six sides as half-spaces to keep (`n . q + d >= 0`, unit normals) in a frame `to_plate`
    /// maps onto the plate.
    pub fn planes_in(&self, to_plate: &Affine) -> [[f64; 4]; 6] {
        self.sides().map(|(n, d)| {
            // n . (M q + t) <= d
            let m: [f64; 3] = std::array::from_fn(|c| -(n[0] * to_plate[0][c] + n[1] * to_plate[1][c] + n[2] * to_plate[2][c]));
            let k = (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt().max(1e-30);
            let t = n[0] * to_plate[0][3] + n[1] * to_plate[1][3] + n[2] * to_plate[2][3];
            [m[0] / k, m[1] / k, m[2] / k, (d - t) / k]
        })
    }
}

/// A closed mesh cut to a cube, its cuts closed with flat faces.
pub fn clipped_mesh(mesh: &PartMesh, cube: &Cube) -> PartMesh {
    let mut tris: Vec<[[f64; 3]; 3]> = mesh.triangles.iter().map(|t| t.map(|v| mesh.positions[v as usize])).collect();
    for (n, d) in cube.sides() {
        let mut kept = Vec::with_capacity(tris.len());
        let mut edges = Vec::new();
        for t in &tris {
            clip_triangle(t, n, d, &mut kept, &mut edges);
        }
        cap(&edges, n, d, &mut kept);
        tris = kept;
    }
    let mut at: HashMap<[u64; 3], u32> = HashMap::new();
    let mut out = PartMesh { positions: Vec::new(), triangles: Vec::with_capacity(tris.len()) };
    for t in &tris {
        let tri = t.map(|q| *at.entry(q.map(f64::to_bits)).or_insert_with(|| {
            out.positions.push(q);
            out.positions.len() as u32 - 1
        }));
        if tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
            out.triangles.push(tri);
        }
    }
    out
}

/// Triangle `t` on the kept side of `n · p <= d`, and its cut edge on the plane, running as the
/// kept side's boundary does. A corner on the plane counts as outside, so neighbours agree.
fn clip_triangle(t: &[[f64; 3]; 3], n: [f64; 3], d: f64, kept: &mut Vec<[[f64; 3]; 3]>, edges: &mut Vec<[[f64; 3]; 2]>) {
    let side = t.map(|q| n[0] * q[0] + n[1] * q[1] + n[2] * q[2] - d);
    let inside = side.map(|s| s < 0.0);
    match inside.iter().filter(|i| **i).count() {
        3 => kept.push(*t),
        0 => {}
        count => {
            // from an inside corner to an outside one, both triangles of the edge alike
            let cross = |a: usize, b: usize| {
                let (i, o) = if inside[a] { (a, b) } else { (b, a) };
                let f = side[i] / (side[i] - side[o]);
                std::array::from_fn(|k| t[i][k] + f * (t[o][k] - t[i][k]))
            };
            let k = (0..3).find(|&k| if count == 1 { inside[k] } else { !inside[k] }).expect("a corner on its own");
            let (a, b, c) = (k, (k + 1) % 3, (k + 2) % 3);
            let (ab, ca): ([f64; 3], [f64; 3]) = (cross(a, b), cross(c, a));
            if count == 1 {
                kept.push([t[a], ab, ca]);
                edges.push([ab, ca]);
            } else {
                kept.push([ab, t[b], t[c]]);
                kept.push([ab, t[c], ca]);
                edges.push([ca, ab]);
            }
        }
    }
}

/// The flat face closing a cut: its edges chained into rings, filled, facing out along `n`.
fn cap(edges: &[[[f64; 3]; 2]], n: [f64; 3], d: f64, kept: &mut Vec<[[f64; 3]; 3]>) {
    if edges.is_empty() {
        return;
    }
    // The plane's axes, u x v = n.
    let helper = if n[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let unit = |a: [f64; 3]| {
        let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
        a.map(|x| x / l)
    };
    let u = unit(cross(helper, n));
    let v = cross(n, u);
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let key = |q: [f64; 3]| q.map(|x| (x * 1e6).round() as i64);
    // The cap runs against the kept side's boundary: each edge from its end back to its start.
    let mut from: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        from.entry(key(e[1])).or_default().push(i);
    }
    let mut used = vec![false; edges.len()];
    let mut rings = Vec::new();
    for start in 0..edges.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut ring = vec![edges[start][1]];
        let mut at = edges[start][0];
        let end = key(edges[start][1]);
        while key(at) != end {
            let Some(next) = from.get(&key(at)).and_then(|list| list.iter().copied().find(|&i| !used[i])) else { break };
            used[next] = true;
            ring.push(edges[next][1]);
            at = edges[next][0];
        }
        if ring.len() >= 3 {
            rings.push(Polygon::new(ring.iter().map(|q| Pt::from_mm(dot(*q, u), dot(*q, v))).collect()));
        }
    }
    for e in union(&rings, FillRule::EvenOdd) {
        let ring = |p: &Polygon| p.pts.iter().map(|q| [q.x as f64 / SCALE, q.y as f64 / SCALE]).collect::<Vec<[f64; 2]>>();
        let region = Region { outer: ring(&e.contour), holes: e.holes.iter().map(ring).collect() };
        let (points, tris) = region.triangulate();
        let back = |p: [f64; 2]| std::array::from_fn(|k| d * n[k] + p[0] * u[k] + p[1] * v[k]);
        kept.extend(tris.iter().map(|t| t.map(|i| back(points[i as usize]))));
    }
}
