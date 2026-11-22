//! Coplanar triangle overlap analysis and render-only conflict resolution.
//!
//! This module deliberately reads the canonical [`RenderBatch`] triangle
//! soup but never rewrites it. It finds near-coplanar pairs through adjacent
//! plane bins and a projected-AABB grid, then measures their actual projected
//! triangle intersection area. The resulting part priorities are metadata for
//! renderers; picking, snapping and measurement continue to use the source
//! geometry byte-for-byte.

use crate::model::batch::{ElementRange, RenderBatch};
use crate::model::ids::{ElementId, MaterialId};
use crate::model::scene::{Element, Material, ScenePart};
use makepad_math::Vec3f;
use crate::model::spread::{chunk_of, in_chunks, sorted};
use makepad_widgets_core::makepad_platform::thread::TaskPool;
use std::collections::{BTreeMap, HashMap, HashSet};

pub const DEFAULT_COPLANAR_TOL_M: f32 = 0.002;
/// The coplanar tolerance never exceeds this share of the scene's radius: 2 mm suits a building
/// (1e-4 of 20 m) but makes every facet of a 3 mm chainmail ring "coplanar" with its neighbours'.
const COPLANAR_TOL_REL: f64 = 1.0e-4;
const NORMAL_DOT_TOL: f64 = 1.0e-5;
// sqrt(2 * NORMAL_DOT_TOL), rounded upward: any normals that pass the dot
// test differ by at most one bin in each component.
const NORMAL_BIN: f64 = 0.0045;
// A near-tie can swap the dominant projection axis at both ends of the
// allowed normal delta. Inserting both axes keeps candidate generation whole.
const PROJECTION_TIE_TOL: f64 = NORMAL_BIN * 2.0;
/// The largest projected grid cell (building scale). A scene of small triangles (a 3D-print
/// plate in metres) gets cells sized to its own triangles: [`grid_cell`].
const PROJECTED_CELL_M: f64 = 2.0;
/// The smallest projected grid cell: candidates are found in the cells of a triangle's own projected
/// bounds, so a fine mesh (a chainmail link's fifth-of-a-millimetre facets) wants cells its size.
const MIN_CELL_M: f64 = 0.0001;

/// The projected grid's cell: twice the median triangle extent, within
/// [`MIN_CELL_M`]..[`PROJECTED_CELL_M`], so a candidate list holds the triangles near a triangle
/// rather than every coplanar triangle of a plate (a 0.5 mm mesh in a 2 m cell paired every flat
/// face of one part with every flat face of another: seconds per scene).
fn grid_cell(tris: &[TriRec]) -> f64 {
    let step = (tris.len() / 4096).max(1);
    let mut extents: Vec<f64> = tris
        .iter()
        .step_by(step)
        .map(|t| {
            let ext = |k: usize| t.p.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max) - t.p.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min);
            ext(0).max(ext(1)).max(ext(2))
        })
        .collect();
    if extents.is_empty() {
        return PROJECTED_CELL_M;
    }
    let mid = extents.len() / 2;
    let (_, median, _) = extents.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    (*median * 2.0).clamp(MIN_CELL_M, PROJECTED_CELL_M)
}

#[derive(Clone, Debug, PartialEq)]
pub struct OverlapRecord {
    pub element_a: ElementId,
    pub element_a_name: String,
    pub part_a: u32,
    pub element_part_a: u32,
    pub material_a: MaterialId,
    pub material_a_name: String,
    pub element_b: ElementId,
    pub element_b_name: String,
    pub part_b: u32,
    pub element_part_b: u32,
    pub material_b: MaterialId,
    pub material_b_name: String,
    pub triangle_pairs: u32,
    pub area_m2: f64,
    /// An interior point and normal from the largest contributing triangle
    /// intersection. Useful for deterministic diagnostics and render tests.
    pub sample_point_m: [f64; 3],
    pub sample_normal: [f64; 3],
    pub draw_priority_a: u16,
    pub draw_priority_b: u16,
    pub coplanar_group: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OverlapReport {
    pub tolerance_m: f32,
    pub triangle_pairs: u64,
    pub area_m2: f64,
    pub pairs: Vec<OverlapRecord>,
}

impl OverlapReport {
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// A deterministic, human-readable element/part overlap table.
    pub fn table(&self) -> String {
        let mut out = String::from(
            "element A / part / material | element B / part / material | tri pairs | area m2 | priority A/B | group\n",
        );
        for r in &self.pairs {
            out.push_str(&format!(
                "{} [{}] / {} / {} | {} [{}] / {} / {} | {} | {:.6} | {}/{} | {}\n",
                r.element_a_name,
                r.element_a.0,
                r.element_part_a,
                r.material_a_name,
                r.element_b_name,
                r.element_b.0,
                r.element_part_b,
                r.material_b_name,
                r.triangle_pairs,
                r.area_m2,
                r.draw_priority_a,
                r.draw_priority_b,
                r.coplanar_group,
            ));
        }
        out
    }
}

#[derive(Clone, Copy, Debug)]
struct TriRec {
    p: [[f64; 3]; 3],
    n: [f64; 3],
    d: f64,
    part: u32,
}

/// A triangle's plane in a projection: its height bin, and its normal's two in-plane
/// components (signed so the dominant one is positive) in [`NORMAL_BIN`] steps. Near-parallel,
/// near-coplanar triangles have keys at most one step apart in each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PlaneKey {
    d: i64,
    drop: u8,
    nu: i32,
    nv: i32,
}

/// A multiply-xor hasher for the overlap grid's small integer keys (the std SipHash costs more
/// than the work it indexes).
#[derive(Default, Clone, Copy)]
struct GridHasher(u64);

impl std::hash::Hasher for GridHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }
    fn write_u8(&mut self, v: u8) {
        self.write_u64(v as u64);
    }
    fn write_i32(&mut self, v: i32) {
        self.write_u64(v as u32 as u64);
    }
    fn write_i64(&mut self, v: i64) {
        self.write_u64(v as u64);
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

type GridMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<GridHasher>>;

type Cell = (i64, i64);

fn v3(p: Vec3f) -> [f64; 3] {
    [p.x as f64, p.y as f64, p.z as f64]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn canonical_normal(p: [[f64; 3]; 3]) -> Option<[f64; 3]> {
    let mut n = cross(sub(p[1], p[0]), sub(p[2], p[0]));
    let len = dot(n, n).sqrt();
    if len <= 1.0e-12 {
        return None;
    }
    for x in &mut n {
        *x /= len;
    }
    // A lexicographic hemisphere rule is stable when two components tie for
    // dominance (45° faces are common). Using the dominant component's
    // sign can flip otherwise identical normals across that tie boundary.
    let sign_component = n.iter().copied().find(|v| v.abs() > 1.0e-12).unwrap_or(1.0);
    if sign_component < 0.0 {
        n = [-n[0], -n[1], -n[2]];
    }
    Some(n)
}

fn projection_axes(drop: usize) -> (usize, usize) {
    match drop {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

fn eligible_projections(n: [f64; 3]) -> impl Iterator<Item = usize> {
    let m = n[0].abs().max(n[1].abs()).max(n[2].abs());
    (0..3).filter(move |&i| m - n[i].abs() <= PROJECTION_TIE_TOL)
}

fn projected_aabb(t: &TriRec, drop: usize, expand: f64) -> ([f64; 2], [f64; 2]) {
    let (u, v) = projection_axes(drop);
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in t.p {
        lo[0] = lo[0].min(p[u]);
        lo[1] = lo[1].min(p[v]);
        hi[0] = hi[0].max(p[u]);
        hi[1] = hi[1].max(p[v]);
    }
    ([lo[0] - expand, lo[1] - expand], [hi[0] + expand, hi[1] + expand])
}

/// The grid cells a projected bounds touches: `lo` to `hi`, inclusive.
#[derive(Clone, Copy, Debug)]
struct Span {
    lo: Cell,
    hi: Cell,
}

impl Span {
    fn of(aabb: ([f64; 2], [f64; 2]), cell_m: f64) -> Span {
        let at = |v: f64| (v / cell_m).floor() as i64;
        Span { lo: (at(aabb.0[0]), at(aabb.0[1])), hi: (at(aabb.1[0]), at(aabb.1[1])) }
    }

    /// Its cells, column by column.
    fn cells(&self) -> impl Iterator<Item = Cell> {
        let (lo, hi) = (self.lo, self.hi);
        (lo.0..=hi.0).flat_map(move |x| (lo.1..=hi.1).map(move |y| (x, y)))
    }

    /// Its four corner cells.
    fn corners(&self) -> [Cell; 4] {
        [self.lo, (self.lo.0, self.hi.1), (self.hi.0, self.lo.1), self.hi]
    }

    fn meet(&self, o: &Span) -> Option<Span> {
        let lo = (self.lo.0.max(o.lo.0), self.lo.1.max(o.lo.1));
        let hi = (self.hi.0.min(o.hi.0), self.hi.1.min(o.hi.1));
        (lo.0 <= hi.0 && lo.1 <= hi.1).then_some(Span { lo, hi })
    }

    /// The span in the cells of `level`, each `2^level` cells to a side.
    fn at_level(&self, level: u32) -> Span {
        Span { lo: (self.lo.0 >> level, self.lo.1 >> level), hi: (self.hi.0 >> level, self.hi.1 >> level) }
    }

    /// The finest level whose cells it touches at most two of to a side.
    fn level(&self) -> u32 {
        (0..63).find(|&l| (self.hi.0 >> l) - (self.lo.0 >> l) <= 1 && (self.hi.1 >> l) - (self.lo.1 >> l) <= 1).unwrap_or(63)
    }
}

fn quantize(x: f64, cell: f64) -> i64 {
    (x / cell).floor() as i64
}

fn plane_height_bin(tol: f64, cell_m: f64) -> f64 {
    // Both triangles touch the same expanded 2D cell. Re-evaluating the plane
    // at that cell's lower corner bounds the normal-delta term by the cell
    // size instead of the radius of the entire scene. Eligible projection axes have |n| >= ~0.568; 0.55 is a
    // conservative divisor including the projection-tie allowance.
    (tol + 2.0 * NORMAL_BIN * (cell_m + tol)) / 0.55
}

fn plane_key(
    t: &TriRec,
    drop: usize,
    cell: Cell,
    origin: [f64; 3],
    height_bin: f64,
    cell_m: f64,
) -> PlaneKey {
    let (u, v) = projection_axes(drop);
    let q_u = cell.0 as f64 * cell_m;
    let q_v = cell.1 as f64 * cell_m;
    let height = -(t.d
        + t.n[u] * (q_u - origin[u])
        + t.n[v] * (q_v - origin[v]))
        / t.n[drop];
    let s = if t.n[drop] < 0.0 { -1.0 } else { 1.0 };
    PlaneKey {
        d: quantize(height, height_bin),
        drop: drop as u8,
        nu: quantize(s * t.n[u], NORMAL_BIN) as i32,
        nv: quantize(s * t.n[v], NORMAL_BIN) as i32,
    }
}

/// Two keys of one projection at most one step apart in height and in each normal component
/// (what [`GridEntry::pairs_with`] tests, packed).
#[cfg(test)]
fn keys_neighbour(a: PlaneKey, b: PlaneKey) -> bool {
    a.drop == b.drop && (a.d - b.d).abs() <= 1 && (a.nu - b.nu).abs() <= 1 && (a.nv - b.nv).abs() <= 1
}

/// A triangle in one grid cell, packed small (the grid scans these by the million): its plane
/// key (height, normal) and its part.
#[derive(Clone, Copy)]
struct GridEntry {
    d: i32,
    nu: i16,
    nv: i16,
    part: u32,
}

impl GridEntry {
    fn new(k: PlaneKey, part: u32) -> GridEntry {
        GridEntry { d: k.d as i32, nu: k.nu as i16, nv: k.nv as i16, part }
    }

    /// Another part's triangle at most one key step away.
    fn pairs_with(&self, o: &GridEntry) -> bool {
        self.part != o.part
            && (self.d - o.d).abs() <= 1
            && (self.nu - o.nu).abs() <= 1
            && (self.nv - o.nv).abs() <= 1
    }
}

fn max_plane_separation(a: &TriRec, b: &TriRec) -> (f64, f64) {
    let ab = b
        .p
        .iter()
        .map(|&p| dot(a.n, sub(p, a.p[0])).abs())
        .fold(0.0, f64::max);
    let ba = a
        .p
        .iter()
        .map(|&p| dot(b.n, sub(p, b.p[0])).abs())
        .fold(0.0, f64::max);
    (ab, ba)
}

fn signed_area(poly: &[[f64; 2]]) -> f64 {
    let mut a = 0.0;
    for i in 0..poly.len() {
        let p = poly[i];
        let q = poly[(i + 1) % poly.len()];
        a += p[0] * q[1] - p[1] * q[0];
    }
    a * 0.5
}

fn cross2(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

/// Sutherland-Hodgman clipping of one projected triangle by the other.
fn intersection_polygon_2d(a: [[f64; 2]; 3], b: [[f64; 2]; 3]) -> Vec<[f64; 2]> {
    let winding = if signed_area(&b) >= 0.0 { 1.0 } else { -1.0 };
    let mut poly = a.to_vec();
    for i in 0..3 {
        let c0 = b[i];
        let c1 = b[(i + 1) % 3];
        let edge = [c1[0] - c0[0], c1[1] - c0[1]];
        let side = |p: [f64; 2]| {
            winding * cross2(edge, [p[0] - c0[0], p[1] - c0[1]])
        };
        let input = std::mem::take(&mut poly);
        if input.is_empty() {
            break;
        }
        let mut prev = *input.last().unwrap();
        let mut prev_side = side(prev);
        for cur in input {
            let cur_side = side(cur);
            let prev_in = prev_side >= -1.0e-12;
            let cur_in = cur_side >= -1.0e-12;
            if prev_in != cur_in {
                let denom = prev_side - cur_side;
                if denom.abs() > 1.0e-20 {
                    let t = prev_side / denom;
                    poly.push([
                        prev[0] + (cur[0] - prev[0]) * t,
                        prev[1] + (cur[1] - prev[1]) * t,
                    ]);
                }
            }
            if cur_in {
                poly.push(cur);
            }
            prev = cur;
            prev_side = cur_side;
        }
    }
    poly
}

fn polygon_centroid(poly: &[[f64; 2]]) -> [f64; 2] {
    let mut twice_area = 0.0;
    let mut c = [0.0; 2];
    for i in 0..poly.len() {
        let p = poly[i];
        let q = poly[(i + 1) % poly.len()];
        let cross = p[0] * q[1] - q[0] * p[1];
        twice_area += cross;
        c[0] += (p[0] + q[0]) * cross;
        c[1] += (p[1] + q[1]) * cross;
    }
    if twice_area.abs() <= 1.0e-20 {
        return poly.iter().fold([0.0; 2], |a, p| [a[0] + p[0], a[1] + p[1]])
            .map(|v| v / poly.len().max(1) as f64);
    }
    [c[0] / (3.0 * twice_area), c[1] / (3.0 * twice_area)]
}

fn projected_intersection(a: &TriRec, b: &TriRec, drop: usize) -> (f64, [f64; 3]) {
    let (u, v) = projection_axes(drop);
    let pa = a.p.map(|p| [p[u], p[v]]);
    let pb = b.p.map(|p| [p[u], p[v]]);
    let poly = intersection_polygon_2d(pa, pb);
    if poly.len() < 3 {
        return (0.0, [0.0; 3]);
    }
    let projected = signed_area(&poly).abs();
    let c = polygon_centroid(&poly);
    let mut point = [0.0; 3];
    point[u] = c[0];
    point[v] = c[1];
    point[drop] = a.p[0][drop]
        - (a.n[u] * (point[u] - a.p[0][u]) + a.n[v] * (point[v] - a.p[0][v]))
            / a.n[drop];
    (projected / a.n[drop].abs().max(1.0e-12), point)
}

#[cfg(test)]
fn projected_intersection_area(a: &TriRec, b: &TriRec, drop: usize) -> f64 {
    projected_intersection(a, b, drop).0
}

/// Each batch's element ranges of the parts that take part (`apart` marks the others).
fn taking_part<'a>(batches: &'a [RenderBatch], apart: &'a [bool]) -> impl Iterator<Item = (&'a RenderBatch, &'a ElementRange)> {
    batches.iter().flat_map(move |batch| batch.element_ranges.iter().filter(|r| apart.get(r.part as usize) != Some(&true)).map(move |r| (batch, r)))
}

fn triangles_from_batches(batches: &[RenderBatch], apart: &[bool], origin: [f64; 3], pool: Option<&TaskPool>) -> Vec<TriRec> {
    // each part's triangles in pieces, the pieces in order
    let count: u32 = taking_part(batches, apart).map(|(_, r)| r.index_count / 3).sum();
    let piece = chunk_of(count as usize) as u32;
    let pieces: Vec<(&RenderBatch, u32, u32, u32)> = taking_part(batches, apart)
        .flat_map(|(batch, range)| {
            let first = range.first_index / 3;
            let end = (range.first_index + range.index_count) / 3;
            (first..end).step_by(piece as usize).map(move |lo| (batch, range.part, lo, (lo + piece).min(end)))
        })
        .collect();
    in_chunks(pool, pieces.len(), 1, |which| {
        let mut out = Vec::new();
        for &(batch, part, first, end) in &pieces[which] {
            for triangle in first..end {
                let (a, b, c) = batch.triangle(triangle);
                let p = [v3(a), v3(b), v3(c)];
                let Some(n) = canonical_normal(p) else {
                    continue;
                };
                let d = -dot(n, sub(p[0], origin));
                out.push(TriRec { p, n, d, part });
            }
        }
        out
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Aggregate {
    triangle_pairs: u32,
    area_m2: f64,
    sample_area_m2: f64,
    sample_point_m: [f64; 3],
    sample_normal: [f64; 3],
}

/// Slack on a triangle's projected bounds: two triangles whose projections only touch have no
/// overlap area, so candidates need overlapping bounds, not `tol`-expanded ones.
const IN_PLANE_EPS_M: f64 = 1.0e-7;

/// Per triangle: whether its bounds, grown by `tol`, reach another part's bounds. Parts are
/// paired by a sweep over their x extents, so a plate of many parts costs its neighbours only.
fn triangles_near_other_parts(tris: &[TriRec], tol: f64, pool: Option<&TaskPool>) -> Vec<bool> {
    let bounds_of = |t: &TriRec| {
        let mut lo = t.p[0];
        let mut hi = t.p[0];
        for p in &t.p[1..] {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        (lo, hi)
    };
    let mut parts: HashMap<u32, ([f64; 3], [f64; 3])> = HashMap::new();
    for t in tris {
        let (lo, hi) = bounds_of(t);
        let b = parts.entry(t.part).or_insert((lo, hi));
        for k in 0..3 {
            b.0[k] = b.0[k].min(lo[k]);
            b.1[k] = b.1[k].max(hi[k]);
        }
    }
    let overlaps = |a: &([f64; 3], [f64; 3]), b: &([f64; 3], [f64; 3]), grow: f64| (0..3).all(|k| a.0[k] - grow <= b.1[k] && b.0[k] <= a.1[k] + grow);
    let mut order: Vec<(u32, ([f64; 3], [f64; 3]))> = parts.into_iter().collect();
    order.sort_by(|a, b| a.1 .0[0].total_cmp(&b.1 .0[0]).then(a.0.cmp(&b.0)));
    let mut neighbours: HashMap<u32, Vec<([f64; 3], [f64; 3])>> = HashMap::new();
    for i in 0..order.len() {
        let (pa, ba) = order[i];
        for &(pb, bb) in &order[i + 1..] {
            if bb.0[0] > ba.1[0] + 2.0 * tol {
                break;
            }
            if overlaps(&ba, &bb, 2.0 * tol) {
                neighbours.entry(pa).or_default().push(bb);
                neighbours.entry(pb).or_default().push(ba);
            }
        }
    }
    in_chunks(pool, tris.len(), chunk_of(tris.len()), |range| {
        tris[range]
            .iter()
            .map(|t| {
                let b = bounds_of(t);
                neighbours.get(&t.part).is_some_and(|list| list.iter().any(|o| overlaps(&b, o, tol)))
            })
            .collect()
    })
}

/// The part pairs a scene's near triangles overlap in, and what finding them took.
#[derive(Debug, Default)]
struct Found {
    aggregate: BTreeMap<(u32, u32), Aggregate>,
    /// pairs of triangles (of different parts) whose projected cells overlap and whose planes
    /// come within a few height steps there
    spans_met: u64,
    candidates: u64,
    area_tests: u64,
    /// how long its phases took (ms): the scan of the grid, the pairs' first cells, the areas
    phases_ms: [f64; 3],
}

/// A near triangle in one projection: its index, part, its plane key's normal (the same in
/// every cell), its cells and the level of the grid it is in.
#[derive(Clone, Copy)]
struct Entry {
    tri: u32,
    part: u32,
    drop: u8,
    nu: i16,
    nv: i16,
    level: u32,
    span: Span,
    /// Its plane's height over its cells, in height steps ([`height_steps`]): the least and the
    /// most, at the span's corners (a plane's height is affine in the cell), and whether all of
    /// them are small enough that the grid's keys do not wrap ([`keys_apart`]).
    steps: (f64, f64),
    small: bool,
}

/// An entry as the grid's cells hold it: what the scan tests before it reads the entry (the
/// scan passes over cells' entries by the hundred million).
#[derive(Clone, Copy)]
struct Slot {
    part: u32,
    e: u32,
    nu: i16,
    nv: i16,
    level: u32,
    small: bool,
    steps: (f64, f64),
}

/// The overlapping pairs of the `near` triangles of different parts.
///
/// Two triangles are candidates where they meet in a cell of the projected grid (one of
/// [`grid_cell`], in which both their bounds lie) with plane keys at most one step apart there
/// ([`GridEntry::pairs_with`]); each later triangle tests each earlier one once, in the order of
/// the cells it meets them in, projection by projection, then column by column, then in index
/// order. A part is never compared with itself (its triangles draw in one call, so a priority
/// cannot separate them).
///
/// The pairs whose cells overlap come from a grid of levels, each triangle in the finest whose
/// cells (twice the last level's to a side) it touches at most two of, so a big face (a bed, a
/// pad) is a few cells, not its thousands, and a small triangle finds it in the levels above its
/// own. A pair then meets in the first cell of their overlap whose keys pair: the same cells,
/// keys and order the cells of the grid alone give, without filling them.
fn pairs(tris: &[TriRec], near: &[bool], origin: [f64; 3], tol: f64, pool: Option<&TaskPool>) -> Found {
    let t0 = std::time::Instant::now();
    let cell_m = grid_cell(tris);
    let height_bin = plane_height_bin(tol, cell_m);
    // The passes over triangles, entries and pairs spread over the pool, their results in order.
    let entries: Vec<Entry> = in_chunks(pool, tris.len(), chunk_of(tris.len()), |range| {
        let mut entries = Vec::new();
        for i in range.filter(|i| near[*i]) {
            let t = &tris[i];
            for drop in eligible_projections(t.n) {
                // In the projection plane a pair must overlap (positive area), so the cells are those
                // of the triangle's own projected bounds; `tol` is the distance out of plane.
                let span = Span::of(projected_aabb(t, drop, IN_PLANE_EPS_M), cell_m);
                let k = GridEntry::new(plane_key(t, drop, span.lo, origin, height_bin, cell_m), t.part);
                let corners = span.corners().map(|c| height_steps(t, drop, c, origin, height_bin, cell_m));
                let steps = (corners.iter().copied().fold(f64::INFINITY, f64::min), corners.iter().copied().fold(f64::NEG_INFINITY, f64::max));
                let small = corners.iter().all(|s| s.abs() < 1.0e9);
                entries.push(Entry { tri: i as u32, part: t.part, drop: drop as u8, nu: k.nu, nv: k.nv, level: span.level(), span, steps, small });
            }
        }
        entries
    });
    // The grid: each entry in the cells of its level, a cell's entries by part, then by normal
    // key, so a scan passes over its own part's run and reads only the normal keys that pair.
    let mut placed: Vec<((u8, u32, Cell), Slot)> = Vec::new();
    let mut levels_of: [Vec<u32>; 3] = Default::default();
    for (e, entry) in entries.iter().enumerate() {
        levels_of[entry.drop as usize].push(entry.level);
        let slot = Slot { part: entry.part, e: e as u32, nu: entry.nu, nv: entry.nv, level: entry.level, small: entry.small, steps: entry.steps };
        for cell in entry.span.at_level(entry.level).cells() {
            placed.push(((entry.drop, entry.level, cell), slot));
        }
    }
    for l in &mut levels_of {
        l.sort_unstable();
        l.dedup();
    }
    // an entry is in a cell once: no two placed are alike
    let placed = sorted(pool, placed, |(k, a), (l, b)| (k, a.part, a.nu, a.nv, a.e).cmp(&(l, b.part, b.nu, b.nv, b.e)));
    let slots: Vec<Slot> = placed.iter().map(|(_, s)| *s).collect();
    // a cell's runs (one a part) in `runs`, and each run's slots in `slots`
    let mut cells: GridMap<(u8, u32, Cell), (u32, u32)> = GridMap::default();
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    let mut i = 0;
    while i < placed.len() {
        let key = placed[i].0;
        let first_run = runs.len() as u32;
        while i < placed.len() && placed[i].0 == key {
            let part = placed[i].1.part;
            let lo = i;
            while i < placed.len() && placed[i].0 == key && placed[i].1.part == part {
                i += 1;
            }
            runs.push((part, lo as u32, i as u32));
        }
        cells.insert(key, (first_run, runs.len() as u32));
    }
    std::mem::drop(placed);
    // every pair of entries of different parts whose cells overlap, whose normals' keys pair and
    // whose planes come within a few steps: from the finer of the two (of one level, from the
    // first), as (later triangle, projection, earlier triangle, their entries)
    let met: Vec<(u32, u8, u32, u32, u32)> = in_chunks(pool, entries.len(), chunk_of(entries.len()), |range| {
        let mut met = Vec::new();
        for e in range {
            let a = &entries[e];
            for &level in levels_of[a.drop as usize].iter().filter(|l| **l >= a.level) {
                for cell in a.span.at_level(level).cells() {
                    let Some(&(r0, r1)) = cells.get(&(a.drop, level, cell)) else { continue };
                    for &(part, lo, hi) in &runs[r0 as usize..r1 as usize] {
                        // a part is never compared with itself
                        if part == a.part {
                            continue;
                        }
                        let run = &slots[lo as usize..hi as usize];
                        let from = run.partition_point(|b| b.nu < a.nu - 1);
                        let to = run.partition_point(|b| b.nu <= a.nu + 1);
                        for b in &run[from..to] {
                            let f = b.e;
                            if (b.level == a.level && f as usize <= e) || (a.nv - b.nv).abs() > 1 {
                                continue;
                            }
                            // Planes more than three steps apart all over both spans pair in none of
                            // their cells (keys a step apart are under two steps apart), and the
                            // cells they meet in are in both spans: no first cell below would be found.
                            if a.small && b.small && (a.steps.0 - b.steps.1 > 3.0 || b.steps.0 - a.steps.1 > 3.0) {
                                continue;
                            }
                            let Some(both) = a.span.meet(&entries[f as usize].span) else { continue };
                            // The pair is scanned at max(a.level, b.level). Emit it only in
                            // the first shared cell at that level, regardless of which entry
                            // is finer. Every overlapping pair has exactly one such cell.
                            if cell != both.at_level(level).lo {
                                continue;
                            }
                            let b_tri = entries[f as usize].tri;
                            let (later, earlier) = if a.tri > b_tri { ((a.tri, e as u32), (b_tri, f)) } else { ((b_tri, f), (a.tri, e as u32)) };
                            met.push((later.0, a.drop, earlier.0, later.1, earlier.1));
                        }
                    }
                }
            }
        }
        met
    });
    let t_scan = t0.elapsed();
    // No duplicate entry pairs remain. Only the final meets need sorting: their
    // order determines accumulation and which tied sample point wins.
    // where each pair first meets with keys that pair: (later, projection, cell, earlier)
    let key = |t: u32, drop: usize, cell: Cell| {
        let tri = &tris[t as usize];
        GridEntry::new(plane_key(tri, drop, cell, origin, height_bin, cell_m), tri.part)
    };
    let mut meets: Vec<(u32, u8, Cell, u32)> = in_chunks(pool, met.len(), chunk_of(met.len()), |range| {
        met[range]
            .iter()
            .filter_map(|&(i, drop, j, ei, ej)| {
                let d = drop as usize;
                let both = entries[ei as usize].span.meet(&entries[ej as usize].span)?;
                if keys_apart(tris, (i, j), d, both, (origin, height_bin, cell_m)) {
                    return None;
                }
                both.cells().find(|&c| key(i, d, c).pairs_with(&key(j, d, c))).map(|c| (i, drop, c, j))
            })
            .collect()
    });
    meets.sort_unstable();
    let t_meets = t0.elapsed();
    let mut found = Found { spans_met: met.len() as u64, ..Found::default() };
    // Which triangle last met each candidate (once per triangle, whichever key found it).
    let mut seen_by = vec![u32::MAX; tris.len()];
    for &(i, drop, _, j) in &meets {
        let (tri, other) = (&tris[i as usize], &tris[j as usize]);
        if seen_by[j as usize] == i {
            continue;
        }
        seen_by[j as usize] = i;
        found.candidates += 1;
        if 1.0 - dot(tri.n, other.n).abs() > NORMAL_DOT_TOL {
            continue;
        }
        let (ab, ba) = max_plane_separation(tri, other);
        if ab > tol || ba > tol {
            continue;
        }
        found.area_tests += 1;
        let (area, sample_point_m) = projected_intersection(tri, other, drop as usize);
        // Contacts smaller than the square of the positional
        // tolerance are indistinguishable from a shared edge
        // after projection.  Keeping them would connect large
        // models into one conflict group through numerical
        // slivers, even though they cannot produce a visible
        // coplanar patch.
        if area <= (tol * tol).max(1.0e-12) {
            continue;
        }
        let key = if tri.part <= other.part { (tri.part, other.part) } else { (other.part, tri.part) };
        let a = found.aggregate.entry(key).or_default();
        a.triangle_pairs += 1;
        a.area_m2 += area;
        if area > a.sample_area_m2 {
            a.sample_area_m2 = area;
            a.sample_point_m = sample_point_m;
            a.sample_normal = tri.n;
        }
    }
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
    found.phases_ms = [ms(t_scan), ms(t_meets - t_scan), ms(t0.elapsed() - t_meets)];
    found
}

/// A triangle's plane height at a cell's lower corner, in height steps: the value whose floor is
/// its key's height there ([`plane_key`]).
fn height_steps(t: &TriRec, drop: usize, c: Cell, origin: [f64; 3], height_bin: f64, cell_m: f64) -> f64 {
    let (u, v) = projection_axes(drop);
    -(t.d + t.n[u] * (c.0 as f64 * cell_m - origin[u]) + t.n[v] * (c.1 as f64 * cell_m - origin[v])) / t.n[drop] / height_bin
}

/// Whether two triangles' planes are more than a few height steps apart over all of `span`: a
/// plane's height is affine in the cell, so their difference is, and is that far apart all over
/// when it is at the corners (a pair of keys a step apart is less than two steps apart).
fn keys_apart(tris: &[TriRec], (i, j): (u32, u32), drop: usize, span: Span, (origin, height_bin, cell_m): ([f64; 3], f64, f64)) -> bool {
    let steps = |t: u32, c: Cell| height_steps(&tris[t as usize], drop, c, origin, height_bin, cell_m);
    let corners = span.corners();
    // keys past an i32 wrap are compared as the grid stores them: walk those cells
    let small = corners.iter().all(|&c| steps(i, c).abs() < 1.0e9 && steps(j, c).abs() < 1.0e9);
    let gaps = corners.map(|c| steps(i, c) - steps(j, c));
    small && (gaps.iter().all(|g| *g > 3.0) || gaps.iter().all(|g| *g < -3.0))
}

/// Compare the analysis inputs with an already-owned scene. Normals, colours and resolved
/// priorities are irrelevant; positions (including unused vertices), ordering and names are not.
/// The old scene owns the only geometry copy, and fresh conflict resolution uses current materials.
pub(crate) fn same_analysis_inputs(old: &crate::model::Scene, batches: &[RenderBatch], parts: &[ScenePart], elements: &[Element], materials: &[Material]) -> bool {
    old.batches.len() == batches.len() && old.parts.len() == parts.len()
        && old.elements.len() == elements.len() && old.materials.len() == materials.len()
        && old.batches.iter().zip(batches).all(|(a, b)| {
            a.vertex_count() == b.vertex_count() && a.indices == b.indices
                && (0..a.vertex_count() as u32).all(|i| {
                    let (a, b) = (a.position(i), b.position(i));
                    [a.x.to_bits(), a.y.to_bits(), a.z.to_bits()] == [b.x.to_bits(), b.y.to_bits(), b.z.to_bits()]
                })
                && a.element_ranges.len() == b.element_ranges.len()
                && a.element_ranges.iter().zip(&b.element_ranges).all(|(a, b)|
                    (a.part, a.first_index, a.index_count) == (b.part, b.first_index, b.index_count))
        })
        && old.parts.iter().zip(parts).all(|(a, b)| (a.element, a.element_part, a.material) == (b.element, b.element_part, b.material))
        && old.elements.iter().zip(elements).all(|(a, b)| a.name == b.name)
        && old.materials.iter().zip(materials).all(|(a, b)| a.name == b.name)
}

/// Analyze the canonical scene triangles of every part but those `apart` marks (by part id): a
/// part drawn in a pass of its own never meets the others in a depth buffer, and its bounds
/// leave the tolerance as it is. Only triangles within `tol` of another part's bounds take part;
/// candidate generation uses adjacent plane bins plus every cell touched by each projected AABB
/// (grown by [`IN_PLANE_EPS_M`]); exact duplicates and edge-sharing triangles therefore reach
/// the area test, where only positive-area intersections survive.
pub fn analyze(
    batches: &[RenderBatch],
    parts: &[ScenePart],
    elements: &[Element],
    materials: &[Material],
    tolerance_m: f32,
    apart: &[bool],
    pool: Option<&TaskPool>,
) -> OverlapReport {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (batch, range) in taking_part(batches, apart) {
        for triangle in range.first_index / 3..(range.first_index + range.index_count) / 3 {
            let (a, b, c) = batch.triangle(triangle);
            for p in [v3(a), v3(b), v3(c)] {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
        }
    }
    let origin = if lo[0].is_finite() {
        [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5]
    } else {
        [0.0; 3]
    };
    let radius = if lo[0].is_finite() { (0..3).map(|k| (hi[k] - lo[k]) * (hi[k] - lo[k])).sum::<f64>().sqrt() * 0.5 } else { 0.0 };
    let tol = (tolerance_m as f64).min(radius * COPLANAR_TOL_REL).max(1.0e-7);
    let t0 = std::time::Instant::now();
    let tris = triangles_from_batches(batches, apart, origin, pool);
    let t_tris = t0.elapsed();
    // Broadphase: a triangle can only pair with another part's triangle when it comes within
    // `tol` of that part's bounds. Triangles near no other part (a part alone on the plate, the
    // far side of a chainmail link) neither look up nor go into the grid.
    let near = triangles_near_other_parts(&tris, tol, pool);
    let t_near = t0.elapsed();
    let found = pairs(&tris, &near, origin, tol, pool);

    if std::env::var_os("FAB_SCENE_TIMING").is_some() {
        println!(
            "overlap: {} triangles ({:.0} ms), {} near another part ({:.0} ms), {} pairs of spans, {} candidates, {} area tests ({:.0} ms: scan {:.0}, first cells {:.0}, areas {:.0}), {} part pairs",
            tris.len(),
            t_tris.as_secs_f64() * 1e3,
            near.iter().filter(|n| **n).count(),
            (t_near - t_tris).as_secs_f64() * 1e3,
            found.spans_met,
            found.candidates,
            found.area_tests,
            (t0.elapsed() - t_near).as_secs_f64() * 1e3,
            found.phases_ms[0],
            found.phases_ms[1],
            found.phases_ms[2],
            found.aggregate.len()
        );
    }
    let mut report = OverlapReport {
        tolerance_m: tol as f32,
        ..Default::default()
    };
    for ((part_a, part_b), a) in found.aggregate {
        let Some(pa) = parts.get(part_a as usize) else { continue };
        let Some(pb) = parts.get(part_b as usize) else { continue };
        let ea = elements.get(pa.element.index());
        let eb = elements.get(pb.element.index());
        let ma = materials.get(pa.material.index());
        let mb = materials.get(pb.material.index());
        report.triangle_pairs += a.triangle_pairs as u64;
        report.area_m2 += a.area_m2;
        report.pairs.push(OverlapRecord {
            element_a: pa.element,
            element_a_name: ea.map(|e| e.name.clone()).unwrap_or_default(),
            part_a,
            element_part_a: pa.element_part,
            material_a: pa.material,
            material_a_name: ma.map(|m| m.name.clone()).unwrap_or_default(),
            element_b: pb.element,
            element_b_name: eb.map(|e| e.name.clone()).unwrap_or_default(),
            part_b,
            element_part_b: pb.element_part,
            material_b: pb.material,
            material_b_name: mb.map(|m| m.name.clone()).unwrap_or_default(),
            triangle_pairs: a.triangle_pairs,
            area_m2: a.area_m2,
            sample_point_m: a.sample_point_m,
            sample_normal: a.sample_normal,
            draw_priority_a: 0,
            draw_priority_b: 0,
            coplanar_group: 0,
        });
    }
    report
}

struct UnionFind {
    parent: Vec<u32>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self { parent: (0..n as u32).collect() }
    }
    fn root(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            let p = self.parent[x as usize];
            self.parent[x as usize] = self.parent[p as usize];
            x = self.parent[x as usize];
        }
        x
    }
    fn join(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.root(a), self.root(b));
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.parent[hi as usize] = lo;
        }
    }
}

/// Assign a unique priority within each connected coplanar part group:
/// opaque over transparent, then the source element/part ids as the stable
/// final key.
pub(crate) fn resolve(report: &mut OverlapReport, parts: &mut [ScenePart], materials: &[Material]) {
    let mut uf = UnionFind::new(parts.len());
    let mut active = HashSet::new();
    let mut neighbours: HashMap<u32, Vec<u32>> = HashMap::new();
    for r in &report.pairs {
        if (r.part_a as usize) < parts.len() && (r.part_b as usize) < parts.len() {
            uf.join(r.part_a, r.part_b);
            active.insert(r.part_a);
            active.insert(r.part_b);
            if r.part_a != r.part_b {
                neighbours.entry(r.part_a).or_default().push(r.part_b);
                neighbours.entry(r.part_b).or_default().push(r.part_a);
            }
        }
    }
    let mut groups: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for p in active {
        groups.entry(uf.root(p)).or_default().push(p);
    }
    for (group_index, members) in groups.values_mut().enumerate() {
        members.sort_by_key(|&p| {
            let part = &parts[p as usize];
            let opaque = materials
                .get(part.material.index())
                .map(|m| !m.transparent)
                .unwrap_or(true);
            (opaque as u8, part.element.0, part.element_part, p)
        });
        let group = group_index as u32 + 1;
        for &p in members.iter() {
            // The total semantic order orients every conflict edge. A part
            // only needs to outrank its directly overlapping predecessors,
            // not every unrelated member of a large connected component.
            // Longest-path ranks keep the GPU value compact while preserving
            // a strict, deterministic winner for every reported pair.
            let rank = neighbours
                .get(&p)
                .into_iter()
                .flatten()
                .filter_map(|&q| parts.get(q as usize).map(|part| part.draw_priority))
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            parts[p as usize].coplanar_group = group;
            parts[p as usize].draw_priority = rank;
        }
    }
    for r in &mut report.pairs {
        let pa = parts.get(r.part_a as usize);
        let pb = parts.get(r.part_b as usize);
        r.draw_priority_a = pa.map(|p| p.draw_priority).unwrap_or(0);
        r.draw_priority_b = pb.map(|p| p.draw_priority).unwrap_or(0);
        r.coplanar_group = pa.map(|p| p.coplanar_group).unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::batch::{ElementRange, Vertex};
    use crate::model::ids::MeshId;

    fn tri(p: [[f64; 3]; 3], part: u32) -> TriRec {
        let n = canonical_normal(p).unwrap();
        TriRec {
            p,
            n,
            d: -dot(n, p[0]),
            part,
        }
    }

    fn overlap(a: &TriRec, b: &TriRec, tol: f64) -> f64 {
        let (ab, ba) = max_plane_separation(a, b);
        if 1.0 - dot(a.n, b.n).abs() > NORMAL_DOT_TOL || ab > tol || ba > tol {
            return 0.0;
        }
        let drop = eligible_projections(a.n).next().unwrap();
        projected_intersection_area(a, b, drop)
    }

    fn analyze_pair(a: [[f64; 3]; 3], b: [[f64; 3]; 3], shared_vertices: bool) -> OverlapReport {
        let mut batch = RenderBatch { material: MaterialId(0), ..Default::default() };
        for p in a {
            batch.push_vertex(&Vertex {
                position: p.map(|v| v as f32),
                normal: [0.0, 0.0, 1.0],
                ..Default::default()
            });
        }
        batch.indices.extend([0, 1, 2]);
        if shared_vertices {
            batch.indices.extend([2, 1, 0]);
        } else {
            for p in b {
                batch.push_vertex(&Vertex {
                    position: p.map(|v| v as f32),
                    normal: [0.0, 0.0, 1.0],
                    ..Default::default()
                });
            }
            batch.indices.extend([3, 4, 5]);
        }
        batch.element_ranges = vec![
            ElementRange {
                element: ElementId(0),
                part: 0,
                first_index: 0,
                index_count: 3,
                draw_priority: 0,
                coplanar_group: 0,
            },
            ElementRange {
                element: ElementId(1),
                part: 1,
                first_index: 3,
                index_count: 3,
                draw_priority: 0,
                coplanar_group: 0,
            },
        ];
        let parts = vec![
            ScenePart {
                id: 0,
                element: ElementId(0),
                element_part: 0,
                mesh: MeshId(0),
                material: MaterialId(0),
                batch: 0,
                first_index: 0,
                index_count: 3,
                draw_priority: 0,
                coplanar_group: 0,
            },
            ScenePart {
                id: 1,
                element: ElementId(1),
                element_part: 0,
                mesh: MeshId(1),
                material: MaterialId(0),
                batch: 0,
                first_index: 3,
                index_count: 3,
                draw_priority: 0,
                coplanar_group: 0,
            },
        ];
        analyze(&[batch], &parts, &[], &[], DEFAULT_COPLANAR_TOL_M, &[], None)
    }

    #[test]
    fn exact_duplicates_are_not_discarded_for_shared_vertices() {
        let a = tri([[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0]], 0);
        let b = tri([[0.0, 2.0, 0.0], [2.0, 0.0, 0.0], [0.0, 0.0, 0.0]], 1);
        assert!((overlap(&a, &b, 0.002) - 2.0).abs() < 1.0e-10);
        let report = analyze_pair(a.p, b.p, true);
        assert_eq!(report.pairs.len(), 1);
        assert!((report.area_m2 - 2.0).abs() < 1.0e-9);
    }

    #[test]
    fn partial_overlap_measures_intersection_area() {
        let a = tri([[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0]], 0);
        let b = tri([[0.5, 0.5, 0.0], [1.5, 0.5, 0.0], [0.5, 1.5, 0.0]], 1);
        assert!((overlap(&a, &b, 0.002) - 0.5).abs() < 1.0e-10);
        assert!((analyze_pair(a.p, b.p, false).area_m2 - 0.5).abs() < 1.0e-9);
    }

    #[test]
    fn adjacent_shared_edge_has_zero_overlap_area() {
        let a = tri([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], 0);
        let b = tri([[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]], 1);
        assert!(overlap(&a, &b, 0.002) < 1.0e-12);
        assert!(analyze_pair(a.p, b.p, false).is_empty());
    }

    #[test]
    fn both_directional_plane_separations_must_pass() {
        let a = tri([[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 100.0, 0.0]], 0);
        let b = tri([[0.0, 0.0, 0.001], [100.0, 0.0, 0.003], [0.0, 100.0, 0.001]], 1);
        let (ab, ba) = max_plane_separation(&a, &b);
        assert!(ab > 0.002 && ba > 0.002);
        assert_eq!(overlap(&a, &b, 0.002), 0.0);
    }

    #[test]
    fn neighbouring_plane_bins_cover_a_quantization_boundary() {
        let height_bin = plane_height_bin(0.002, PROJECTED_CELL_M);
        let za = height_bin - 1.0e-5;
        let zb = height_bin + 1.0e-5;
        let a = tri([[0.0, 0.0, za], [1.0, 0.0, za], [0.0, 1.0, za]], 0);
        let b = tri([[0.0, 0.0, zb], [1.0, 0.0, zb], [0.0, 1.0, zb]], 1);
        let cell = (0, 0);
        let ka = plane_key(&a, 2, cell, [0.0; 3], height_bin, PROJECTED_CELL_M);
        let kb = plane_key(&b, 2, cell, [0.0; 3], height_bin, PROJECTED_CELL_M);
        assert_ne!(ka.d, kb.d, "fixture must straddle a plane bucket");
        assert!(keys_neighbour(ka, kb));
        assert!((overlap(&a, &b, 0.002) - 0.5).abs() < 1.0e-10);
        assert_eq!(analyze_pair(a.p, b.p, false).pairs.len(), 1);
    }

    /// The search as the grid of cells alone did it: every triangle into every cell of its
    /// bounds, meeting there the triangles before it.
    fn pairs_by_cells(tris: &[TriRec], near: &[bool], origin: [f64; 3], tol: f64) -> (BTreeMap<(u32, u32), Aggregate>, u64, u64) {
        let cell_m = grid_cell(tris);
        let height_bin = plane_height_bin(tol, cell_m);
        let mut grid: GridMap<(u8, Cell), Vec<(GridEntry, u32)>> = GridMap::default();
        let mut seen_by = vec![u32::MAX; tris.len()];
        let mut aggregate: BTreeMap<(u32, u32), Aggregate> = BTreeMap::new();
        let (mut candidates, mut area_tests) = (0, 0);
        for (i, tri) in tris.iter().enumerate().filter(|(i, _)| near[*i]) {
            for drop in eligible_projections(tri.n) {
                for cell in Span::of(projected_aabb(tri, drop, IN_PLANE_EPS_M), cell_m).cells() {
                    let entry = GridEntry::new(plane_key(tri, drop, cell, origin, height_bin, cell_m), tri.part);
                    let slot = grid.entry((drop as u8, cell)).or_default();
                    for &(_, j) in slot.iter().filter(|(e, _)| e.pairs_with(&entry)) {
                        let j = j as usize;
                        if seen_by[j] == i as u32 {
                            continue;
                        }
                        seen_by[j] = i as u32;
                        candidates += 1;
                        let other = &tris[j];
                        let (ab, ba) = max_plane_separation(tri, other);
                        if 1.0 - dot(tri.n, other.n).abs() > NORMAL_DOT_TOL || ab > tol || ba > tol {
                            continue;
                        }
                        area_tests += 1;
                        let (area, at) = projected_intersection(tri, other, drop);
                        if area <= (tol * tol).max(1.0e-12) {
                            continue;
                        }
                        let a = aggregate.entry(if tri.part <= other.part { (tri.part, other.part) } else { (other.part, tri.part) }).or_default();
                        a.triangle_pairs += 1;
                        a.area_m2 += area;
                        if area > a.sample_area_m2 {
                            (a.sample_area_m2, a.sample_point_m, a.sample_normal) = (area, at, tri.n);
                        }
                    }
                    slot.push((entry, i as u32));
                }
            }
        }
        (aggregate, candidates, area_tests)
    }

    /// The pairs, their order and every sum the same as the grid of cells finds, to the last bit.
    fn same_as_by_cells(tris: &[TriRec], tol: f64) -> Found {
        let near = triangles_near_other_parts(tris, tol, None);
        let found = pairs(tris, &near, [0.0; 3], tol, None);
        let (aggregate, candidates, area_tests) = pairs_by_cells(tris, &near, [0.0; 3], tol);
        assert_eq!(found.aggregate, aggregate);
        assert_eq!((found.candidates, found.area_tests), (candidates, area_tests));
        // spread over a pool's workers: the same, to the last bit
        let cx = makepad_widgets_core::Cx::new(Box::new(|_, _| {}));
        let pool = cx.task_pool();
        let (pooled_near, pooled) = std::thread::scope(|s| {
            s.spawn(|| {
                let near = triangles_near_other_parts(tris, tol, Some(&pool));
                let found = pairs(tris, &near, [0.0; 3], tol, Some(&pool));
                (near, found)
            })
            .join()
            .unwrap()
        });
        assert_eq!(pooled_near, near);
        let sums = |f: &Found| format!("{:?}", (&f.aggregate, f.spans_met, f.candidates, f.area_tests));
        assert_eq!(sums(&pooled), sums(&found));
        found
    }

    /// A box's six faces as twelve triangles.
    fn cuboid(lo: [f64; 3], hi: [f64; 3], part: u32, out: &mut Vec<TriRec>) {
        let c = |i: usize| [if i & 1 == 0 { lo[0] } else { hi[0] }, if i & 2 == 0 { lo[1] } else { hi[1] }, if i & 4 == 0 { lo[2] } else { hi[2] }];
        for [a, b, d, e] in [[0, 1, 3, 2], [4, 6, 7, 5], [0, 4, 5, 1], [2, 3, 7, 6], [0, 2, 6, 4], [1, 5, 7, 3]] {
            out.push(tri([c(a), c(b), c(d)], part));
            out.push(tri([c(a), c(d), c(e)], part));
        }
    }

    /// A resin plate's worst case for the grid: a bed of big triangles (a fan-triangulated disc
    /// and a long quad) under a dense support mesh of square pillars, their feet on the bed or a
    /// hair above it (in and out of the tolerance), a wall on one row of their sides, and two
    /// parts in a 45° plane whose normal ties two projections.
    #[test]
    fn big_bed_triangles_under_dense_supports_pair_as_the_grid_of_cells_does() {
        let z = 0.01;
        let mut tris = Vec::new();
        let (c, r) = ([0.05, 0.05], 0.03);
        let rim = |k: usize| {
            let a = k as f64 * std::f64::consts::TAU / 96.0;
            [c[0] + r * a.cos(), c[1] + r * a.sin(), z]
        };
        for k in 0..96 {
            tris.push(tri([[c[0], c[1], z], rim(k), rim(k + 1)], 0));
        }
        tris.push(tri([[0.085, 0.04, z], [0.11, 0.04, z], [0.11, 0.06, z]], 0));
        tris.push(tri([[0.085, 0.04, z], [0.11, 0.06, z], [0.085, 0.06, z]], 0));
        // pillars on the disc and on the quad, each eight boxes high (a support mesh's
        // triangles are small: they set the grid's cell)
        let (w, high) = (0.0004, 0.003);
        let lift = |k: usize| if k % 7 == 3 { 2.0e-5 } else if k % 5 == 1 { 2.0e-6 } else { 0.0 };
        let mut feet: Vec<[f64; 2]> = (0..100).map(|k| [0.03 + 0.0015 * (k % 10) as f64, 0.03 + 0.0015 * (k / 10) as f64]).collect();
        feet.extend((0..6).map(|k| [0.088 + 0.003 * k as f64, 0.05]));
        for (k, f) in feet.iter().enumerate() {
            for s in 0..8 {
                let lo = if s == 0 { z + lift(k) } else { z + high * s as f64 / 8.0 };
                cuboid([f[0] - w, f[1] - w, lo], [f[0] + w, f[1] + w, z + high * (s + 1) as f64 / 8.0], 1, &mut tris);
            }
        }
        // a wall on the first pillar row's -y sides
        let y = 0.03 - w;
        tris.push(tri([[0.029, y, z], [0.045, y, z], [0.045, y, z + high]], 2));
        tris.push(tri([[0.029, y, z], [0.045, y, z + high], [0.029, y, z + high]], 2));
        // a 45° plane (x + y = 0.2): one big triangle, and a dense patch of small ones
        let on = |s: f64, h: f64| [0.1 + s, 0.1 - s, z + h];
        tris.push(tri([on(-0.01, 0.0), on(0.01, 0.0), on(0.0, 0.01)], 3));
        for k in 0..40 {
            let (s, h) = (-0.004 + 0.0002 * (k % 20) as f64, 0.001 + 0.0003 * (k / 20) as f64);
            tris.push(tri([on(s, h), on(s + 0.0002, h), on(s, h + 0.0002)], 4));
        }
        let tol = 5.0e-6;
        let found = same_as_by_cells(&tris, tol);
        let keys: Vec<(u32, u32)> = found.aggregate.keys().copied().collect();
        assert_eq!(keys, vec![(0, 1), (1, 2), (3, 4)]);
        // the feet on the bed and within the tolerance of it, not those above it; the wall on the
        // first row's sides
        let feet_on = (0..feet.len()).filter(|k| lift(*k) < tol).count() as u32;
        assert!(found.aggregate[&(0, 1)].triangle_pairs >= feet_on, "{:?}", found.aggregate[&(0, 1)]);
        let sides: f64 = (0..10).map(|k| 2.0 * w * (high - lift(k))).sum();
        assert!((found.aggregate[&(1, 2)].area_m2 - sides).abs() < 1.0e-12, "{:?} against {sides}", found.aggregate[&(1, 2)]);
    }

    /// Triangles of every size from a tenth of a millimetre to five centimetres, of four parts,
    /// on a level plane, a tilted one crossing it and a 45° one, each in, at or past the
    /// tolerance off its plane: the pairs and every sum as the grid of cells finds them.
    #[test]
    fn a_soup_of_sizes_and_planes_pairs_as_the_grid_of_cells_does() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let tol = 5.0e-6;
        let planes: [([f64; 3], f64); 3] = [([0.0, 0.0, 1.0], 0.01), ([0.3, 0.1, 0.948_683_298_050_513_8], 0.012), ([std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::FRAC_1_SQRT_2, 0.0], 0.05)];
        let mut tris = Vec::new();
        while tris.len() < 1500 {
            let (n, d) = planes[(next() * 3.0) as usize];
            // two directions in the plane
            let a = if n[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
            let u = cross(n, a);
            let lu = dot(u, u).sqrt();
            let u = [u[0] / lu, u[1] / lu, u[2] / lu];
            let v = cross(n, u);
            let size = 1.0e-4 * 500f64.powf(next());
            let off = [0.0, 0.5, 1.0, 3.0][(next() * 4.0) as usize] * tol * if next() < 0.5 { -1.0 } else { 1.0 };
            let (cu, cv) = (0.06 * next(), 0.06 * next());
            let p = |du: f64, dv: f64| std::array::from_fn(|k| n[k] * (d + off) + u[k] * (cu + du) + v[k] * (cv + dv));
            let (a1, a2) = (next() * std::f64::consts::TAU, next() * std::f64::consts::TAU);
            let t = [p(0.0, 0.0), p(size * a1.cos(), size * a1.sin()), p(size * a2.cos(), size * a2.sin())];
            if canonical_normal(t).is_some() {
                tris.push(tri(t, (next() * 4.0) as u32));
            }
        }
        let found = same_as_by_cells(&tris, tol);
        assert!(found.aggregate.len() >= 4 && found.candidates > 1000, "{} part pairs, {} candidates", found.aggregate.len(), found.candidates);
    }

    #[test]
    fn projected_aabb_index_finds_pairs_with_distant_centroids() {
        let a = [[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 100.0, 0.0]];
        let b = [[98.0, 0.5, 0.0], [99.0, 0.5, 0.0], [98.0, 1.5, 0.0]];
        let report = analyze_pair(a, b, false);
        assert_eq!(report.pairs.len(), 1);
        assert!((report.area_m2 - 0.5).abs() < 1.0e-9);
    }
}
