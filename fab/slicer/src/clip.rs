//! Exact polygon booleans on the integer grid.
//!
//! The method is an arrangement, not a scanline clipper:
//! 1. every ring edge of both operands becomes a directed segment carrying
//!    a winding contribution per operand;
//! 2. segments are split at every crossing, touch and collinear overlap
//!    (crossings are rounded to the grid; the split repeats until no new
//!    crossing appears, so the result is a planar graph);
//! 3. coincident pieces merge and their contributions add up, pieces whose
//!    contributions cancel disappear;
//! 4. half-edges sorted by angle around each vertex give the faces; the
//!    winding of every face follows from one point-location per connected
//!    component (a leftward ray from its leftmost vertex) and is propagated
//!    across edges;
//! 5. the fill rule and the operation decide which faces are inside; edges
//!    with inside on exactly one side are linked into rings, inside on the
//!    left, and holes are grouped with the contour of their region.
//!
//! All predicates are exact (`i128`). Output contours are counter-clockwise,
//! holes clockwise, without collinear vertices.

use crate::geom::*;

/// Temporary arrays retain their allocation in their thread's pools while plates slice. A buffer
/// is checked out for its whole lifetime; pool borrows cover only checkout and return, so nested
/// calls use independent buffers. Clearing on return preserves the same initial contents and
/// iteration order as a fresh Vec. A returned buffer keeps its capacity, up to
/// [`SCRATCH_KEEP_BYTES`], until [`release_scratch`] empties every thread's pools once no plate
/// slices: nothing is held at idle. Output rings own their storage separately.
struct ScratchVec<T: ScratchItem>(Vec<T>);

/// The largest buffer a pool takes back; a bigger one goes back to the allocator.
const SCRATCH_KEEP_BYTES: usize = 1 << 20;

trait ScratchItem: Sized {
    fn pool<R>(f: impl FnOnce(&mut Vec<Vec<Self>>) -> R) -> R;
}

macro_rules! scratch_items {
    ($($ty:ty => $field:ident),* $(,)?) => {
        /// One thread's pools, one per item type.
        #[derive(Default)]
        struct Pools {
            $($field: Vec<Vec<$ty>>,)*
        }
        $(
            impl ScratchItem for $ty {
                fn pool<R>(f: impl FnOnce(&mut Vec<Vec<Self>>) -> R) -> R {
                    POOLS.with(|pools| f(&mut pools.lock().unwrap().$field))
                }
            }
        )*
    };
}

scratch_items! {
    Seg => seg,
    CellSeg => cell_seg,
    Pt => point,
    u32 => index,
    bool => flag,
    [i32; 2] => winding,
    (u32, Pt) => hit,
    ((Pt, Pt, bool), Vec<Pt>, u32) => ring,
    Option<(Vec<Polygon>, Vec<Polygon>)> => region,
}

thread_local! {
    /// This thread's pools. Only this thread and [`release_scratch`] take their lock, each for a
    /// push, a pop or a swap.
    static POOLS: std::sync::Arc<std::sync::Mutex<Pools>> = {
        let pools = std::sync::Arc::new(std::sync::Mutex::new(Pools::default()));
        ALL_POOLS.lock().unwrap().push(std::sync::Arc::downgrade(&pools));
        pools
    };
}

/// Every thread's pools, for [`release_scratch`] (a thread that ended leaves a dead entry, dropped
/// by the next release).
static ALL_POOLS: std::sync::Mutex<Vec<std::sync::Weak<std::sync::Mutex<Pools>>>> = std::sync::Mutex::new(Vec::new());

/// Gives every thread's pooled buffers back to the allocator. Call it when no plate slices: the
/// pools fill again from the next slice's first booleans. A buffer checked out meanwhile stays
/// with its boolean and returns to the emptied pool.
pub fn release_scratch() {
    let mut all = ALL_POOLS.lock().unwrap();
    all.retain(|pools| pools.strong_count() > 0);
    let live: Vec<_> = all.iter().filter_map(std::sync::Weak::upgrade).collect();
    drop(all);
    for pools in live {
        // freed outside the lock
        let held = std::mem::take(&mut *pools.lock().unwrap());
        drop(held);
    }
}

impl<T: ScratchItem> ScratchVec<T> {
    fn new() -> Self {
        Self(T::pool(|pool| pool.pop().unwrap_or_default()))
    }

    fn with_capacity(n: usize) -> Self {
        let mut out = Self::new();
        out.reserve(n);
        out
    }

    fn filled(n: usize, value: T) -> Self where T: Clone {
        let mut out = Self::new();
        out.resize(n, value);
        out
    }
}

impl<T: ScratchItem> std::iter::FromIterator<T> for ScratchVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut out = Self::new();
        out.extend(iter);
        out
    }
}

impl<T: ScratchItem> std::ops::Deref for ScratchVec<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl<T: ScratchItem> std::ops::DerefMut for ScratchVec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}

impl<'a, T: ScratchItem> IntoIterator for &'a ScratchVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter { self.iter() }
}

impl<'a, T: ScratchItem> IntoIterator for &'a mut ScratchVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter { self.iter_mut() }
}

impl<T: ScratchItem> Drop for ScratchVec<T> {
    fn drop(&mut self) {
        self.0.clear();
        let buf = std::mem::take(&mut self.0);
        if buf.capacity() * std::mem::size_of::<T>() <= SCRATCH_KEEP_BYTES {
            T::pool(|pool| pool.push(buf));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FillRule {
    EvenOdd,
    NonZero,
    Positive,
    Negative,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BoolOp {
    Union,
    Intersection,
    Difference,
    Xor,
}

#[inline]
fn fills(rule: FillRule, w: i32) -> bool {
    match rule {
        FillRule::EvenOdd => w & 1 != 0,
        FillRule::NonZero => w != 0,
        FillRule::Positive => w > 0,
        FillRule::Negative => w < 0,
    }
}

#[derive(Clone, Copy, Debug)]
struct Seg {
    a: Pt,
    b: Pt,
    w: [i32; 2],
}

/// Union of rings under `fill`.
pub fn union(polys: &[Polygon], fill: FillRule) -> Vec<ExPolygon> {
    boolean(polys, &[], BoolOp::Union, fill)
}

pub fn union_ex(ex: &[ExPolygon]) -> Vec<ExPolygon> {
    union(&to_polygons(ex), FillRule::NonZero)
}

pub fn difference_ex(a: &[ExPolygon], b: &[ExPolygon]) -> Vec<ExPolygon> {
    if a.is_empty() {
        return Vec::new();
    }
    if b.is_empty() {
        return a.to_vec();
    }
    // Regions of `b` clear of `a`'s bounds change nothing (see `near`).
    let ab = full_bbox(a);
    boolean(&to_polygons(a), &near(b, &ab), BoolOp::Difference, FillRule::NonZero)
}

pub fn intersection_ex(a: &[ExPolygon], b: &[ExPolygon]) -> Vec<ExPolygon> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let (ab, bb) = (full_bbox(a), full_bbox(b));
    if !grown(&ab).overlaps(&bb) {
        return Vec::new();
    }
    let (pa, pb) = (near(a, &bb), near(b, &ab));
    if pa.is_empty() || pb.is_empty() {
        return Vec::new();
    }
    boolean(&pa, &pb, BoolOp::Intersection, FillRule::NonZero)
}

fn full_bbox(set: &[ExPolygon]) -> BBox {
    let mut b = BBox::empty();
    for e in set {
        for r in e.rings() {
            b.merge(&r.bbox());
        }
    }
    b
}

/// `b` grown by two units: regions whose bounds miss it are at least two
/// units from every point of the box.
fn grown(b: &BBox) -> BBox {
    BBox { min: Pt::new(b.min.x - 2, b.min.y - 2), max: Pt::new(b.max.x + 2, b.max.y + 2) }
}

/// The rings of the regions of `set` whose bounds come within two units of
/// `bounds`. A region farther away cannot cross, touch or snap onto any
/// segment inside `bounds` (hot pixels reach half a unit), adds zero
/// winding there, and its faces lie outside every face the operation keeps
/// (those are inside the other operand, within `bounds`), so dropping it
/// leaves the kept part of the arrangement, its windings, its regions and
/// the order its rings are found in unchanged: the output is identical.
fn near(set: &[ExPolygon], bounds: &BBox) -> Vec<Polygon> {
    let g = grown(bounds);
    let mut out = Vec::new();
    for e in set {
        // Holes are inside the contour in well-formed regions; their bounds
        // count too, so a malformed region is never dropped wrongly.
        let mut b = e.bbox();
        for h in &e.holes {
            b.merge(&h.bbox());
        }
        if g.overlaps(&b) {
            out.push(e.contour.clone());
            out.extend(e.holes.iter().cloned());
        }
    }
    out
}

pub fn union2_ex(a: &[ExPolygon], b: &[ExPolygon]) -> Vec<ExPolygon> {
    if b.is_empty() {
        return a.to_vec();
    }
    if a.is_empty() {
        return b.to_vec();
    }
    boolean(&to_polygons(a), &to_polygons(b), BoolOp::Union, FillRule::NonZero)
}

pub fn boolean(subject: &[Polygon], clip: &[Polygon], op: BoolOp, fill: FillRule) -> Vec<ExPolygon> {
    let total: usize = subject.iter().chain(clip).map(|r| r.pts.len()).sum();
    let mut segs = ScratchVec::with_capacity(total);
    for (tag, set) in [(0usize, subject), (1usize, clip)] {
        let mut w = [0, 0];
        w[tag] = 1;
        for ring in set {
            let pts = &ring.pts;
            if pts.len() < 3 {
                continue;
            }
            // Each point to the next, then the last back to the first.
            for (&a, &b) in pts.iter().zip(pts[1..].iter().chain([&pts[0]])) {
                if a != b {
                    segs.push(Seg { a, b, w });
                }
            }
        }
    }
    if segs.is_empty() {
        return Vec::new();
    }
    let n = segs.len();
    let pieces = split_until_planar(segs);
    let inside = |w: [i32; 2]| {
        let s = fills(fill, w[0]);
        let c = fills(fill, w[1]);
        match op {
            BoolOp::Union => s || c,
            BoolOp::Intersection => s && c,
            BoolOp::Difference => s && !c,
            BoolOp::Xor => s ^ c,
        }
    };
    // Nothing split: a simple ring needs no graph.
    if pieces.len() == n {
        if let Some(out) = lone_ring(&pieces, &inside) {
            return out;
        }
    }
    let Some(graph) = Graph::build(&pieces) else {
        return Vec::new();
    };
    graph.extract(inside)
}

/// What [`Graph::extract`] makes of `pieces`, none split by the snap rounding, when they run round
/// one cycle through distinct vertices: a simple ring, whose graph has two faces, its inside and
/// the outside (winding zero, which no fill rule fills). When `inside` takes the inside's winding,
/// that is the one region: the ring anticlockwise from its least half-edge, cleaned as every ring
/// is. `None` for any other set: that takes the graph.
fn lone_ring(pieces: &[Seg], inside: &impl Fn([i32; 2]) -> bool) -> Option<Vec<ExPolygon>> {
    let n = pieces.len();
    let w = pieces[0].w;
    if (0..n).any(|i| pieces[i].b != pieces[(i + 1) % n].a || pieces[i].w != w) {
        return None;
    }
    let mut seen: ScratchVec<Pt> = pieces.iter().map(|s| s.a).collect();
    seen.sort_unstable();
    if seen.windows(2).any(|p| p[0] == p[1]) {
        return None;
    }
    // Inside, the winding is the pieces' own where they run anticlockwise, negated where they run
    // clockwise.
    let (ccw, wind) = match pieces.iter().map(|s| cross(s.a, s.b)).sum::<i128>().signum() {
        1 => (true, w),
        -1 => (false, [-w[0], -w[1]]),
        _ => return None,
    };
    if !inside(wind) {
        return Some(Vec::new());
    }
    // Anticlockwise, from the half-edge least by (lower end, upper end, upper end first).
    let mut pts: Vec<Pt> = if ccw { pieces.iter().map(|s| s.a).collect() } else { pieces.iter().rev().map(|s| s.b).collect() };
    let key = |o: Pt, d: Pt| if o < d { (o, d, false) } else { (d, o, true) };
    let least = (0..n).min_by_key(|&j| key(pts[j], pts[(j + 1) % n])).unwrap_or(0);
    pts.rotate_left(least);
    clean_ring(&mut pts);
    let ring = Polygon::new(pts);
    Some(if ring.pts.len() >= 3 && ring.area2() > 0 { vec![ExPolygon::new(ring)] } else { Vec::new() })
}

// ---------------------------------------------------------------- snap rounding

/// Snap rounding: every segment endpoint and every proper crossing (rounded
/// to the grid) is a hot pixel; each segment is rerouted through the centres
/// of all hot pixels its path touches. The rerouted pieces never cross
/// except at shared vertices, so one pass gives a planar arrangement. When no
/// segment touches a pixel but its own ends, the segments are the pieces.
fn split_until_planar(segs: ScratchVec<Seg>) -> ScratchVec<Seg> {
    let grid = Grid::new(&segs);
    // Every coordinate within ±2^30: products of differences fit in i64.
    let small = grid.small;
    // Each segment in every cell its path comes within a unit of: the pairs
    // that can cross share a cell, and so do a segment and every hot pixel
    // it can pass through (pixels reach half a unit).
    let cells = grid.bin(&segs, 1);
    // The segments copied into cell order: each cell's list is contiguous,
    // and put in order of least x below. Two segments cross, and a segment
    // passes a pixel, only within its x-range: each scan stops at the first
    // entry past it.
    let mut by_cell: ScratchVec<CellSeg> = cells
        .items
        .iter()
        .map(|&i| {
            let s = &segs[i as usize];
            CellSeg { a: s.a, b: s.b, i }
        })
        .collect();
    // Each pair in a cell whose ranges overlap: its proper crossing (recorded
    // in the cell that holds the rounded point, so pairs sharing several
    // cells count once), in cell order; and each one's pass through the pixel
    // of the other's start (recorded in the cell that holds the start). The
    // segments run along closed rings, so their starts are every endpoint,
    // and a segment through an endpoint's pixel meets the segment starting
    // there in that pixel's cell.
    let mut crossings: ScratchVec<(u32, Pt)> = ScratchVec::new();
    let mut hits: ScratchVec<(u32, Pt)> = ScratchVec::new();
    for &c in &cells.occ {
        let c = c as usize;
        let list = &mut by_cell[cells.start[c] as usize..cells.start[c + 1] as usize];
        if list.len() < 2 {
            continue;
        }
        let corner = grid.corner(c);
        list.sort_unstable_by_key(|e| e.x0());
        for x in 0..list.len() {
            let si = &list[x];
            let x1 = si.x1();
            for sj in &list[x + 1..] {
                if sj.x0() > x1 {
                    break;
                }
                if si.a.y.max(si.b.y) < sj.a.y.min(sj.b.y) || sj.a.y.max(sj.b.y) < si.a.y.min(si.b.y) {
                    continue;
                }
                if crosses(si.a, si.b, sj.a, sj.b, small) {
                    // From the lower-index segment, as the rounding depends
                    // on which one the point is computed from.
                    let (s, t) = if si.i < sj.i { (si, sj) } else { (sj, si) };
                    let q = crossing_point(s.a, s.b, t.a, t.b);
                    if grid.holds(corner, q) {
                        crossings.push((c as u32, q));
                    }
                }
                if passes(&grid, corner, si, sj.a) {
                    hits.push((si.i, sj.a));
                }
                if passes(&grid, corner, sj, si.a) {
                    hits.push((sj.i, si.a));
                }
            }
        }
    }
    if crossings.is_empty() && hits.is_empty() {
        return segs;
    }
    // The crossings' pixels. A point listed twice routes no differently:
    // each segment's route is sorted and deduplicated on its own.
    if !crossings.is_empty() {
        let hot_grid = grid.for_points(crossings.len());
        if hot_grid.cell == grid.cell {
            // Each cell's segments against its crossings.
            let mut pts: ScratchVec<Pt> = ScratchVec::new();
            let mut next = 0;
            while next < crossings.len() {
                let c = crossings[next].0 as usize;
                pts.clear();
                while next < crossings.len() && crossings[next].0 as usize == c {
                    pts.push(crossings[next].1);
                    next += 1;
                }
                // The pixels by x; the segments come by least x, so the first
                // pixel in reach only moves forward.
                pts.sort_unstable_by_key(|p| p.x);
                let mut lo = 0;
                for s in &by_cell[cells.start[c] as usize..cells.start[c + 1] as usize] {
                    let (x0, x1) = (s.x0(), s.x1());
                    while lo < pts.len() && pts[lo].x < x0 {
                        lo += 1;
                    }
                    for &p in &pts[lo..] {
                        if p.x > x1 {
                            break;
                        }
                        if p != s.a && p != s.b && segment_hits_pixel(s.a, s.b, p, small) {
                            hits.push((s.i, p));
                        }
                    }
                }
            }
        } else {
            // Dense crossings: on a finer grid of their own, each segment
            // against the crossings of the cells it reaches.
            let hot: ScratchVec<Pt> = crossings.iter().map(|c| c.1).collect();
            let hot_cells = hot_grid.bin_points(&hot);
            // Hot points in cell order (same order within a cell as the index lists).
            let hot_by_cell: ScratchVec<Pt> = hot_cells.items.iter().map(|&h| hot[h as usize]).collect();
            let mut visit: ScratchVec<u32> = ScratchVec::new();
            for (i, s) in segs.iter().enumerate() {
                visit.clear();
                hot_grid.cells_of_segment(s.a, s.b, 1, &mut visit);
                let (x0, x1) = (s.a.x.min(s.b.x), s.a.x.max(s.b.x));
                for &c in &visit {
                    let c = c as usize;
                    for &p in &hot_by_cell[hot_cells.start[c] as usize..hot_cells.start[c + 1] as usize] {
                        if x0 <= p.x && p.x <= x1 && p != s.a && p != s.b && segment_hits_pixel(s.a, s.b, p, small) {
                            hits.push((i as u32, p));
                        }
                    }
                }
            }
        }
    }
    // The hits by segment: a counting sort (most segments have none).
    let mut first = ScratchVec::filled(segs.len() + 1, 0u32);
    for h in &hits {
        first[h.0 as usize] += 1;
    }
    let mut sum = 0;
    for f in first.iter_mut() {
        sum += *f;
        *f = sum;
    }
    let mut by_seg = ScratchVec::filled(hits.len(), Pt::new(0, 0));
    for &(i, p) in &hits {
        first[i as usize] -= 1;
        by_seg[first[i as usize] as usize] = p;
    }
    let mut out = ScratchVec::with_capacity(segs.len() + hits.len());
    for (i, s) in segs.iter().enumerate() {
        route(s, &mut by_seg[first[i] as usize..first[i + 1] as usize], &mut out);
    }
    out
}

/// Does `s` pass through the pixel of `p`, the start of another segment in
/// the cell whose least corner is `k`? Each pixel counts in the cell that
/// holds it.
#[inline]
fn passes(grid: &Grid, k: Pt, s: &CellSeg, p: Pt) -> bool {
    p != s.a && p != s.b && s.x0() <= p.x && p.x <= s.x1() && grid.holds(k, p) && segment_hits_pixel(s.a, s.b, p, grid.small)
}

/// A segment in a cell's list: its ends and its index.
#[derive(Clone, Copy)]
struct CellSeg {
    a: Pt,
    b: Pt,
    i: u32,
}

impl CellSeg {
    #[inline]
    fn x0(&self) -> i64 {
        self.a.x.min(self.b.x)
    }

    #[inline]
    fn x1(&self) -> i64 {
        self.a.x.max(self.b.x)
    }
}

/// `s` rerouted through the hot pixels `through` (in any order, repeats
/// allowed), as pieces appended to `out`.
#[inline]
fn route(s: &Seg, through: &mut [Pt], out: &mut Vec<Seg>) {
    if through.is_empty() {
        out.push(*s);
        return;
    }
    let d = s.b.sub(s.a);
    // Ties in the projection (two pixels across the line) by point, so
    // the route never depends on the order cells are visited in.
    through.sort_unstable_by_key(|p| (dot(p.sub(s.a), d), *p));
    let mut prev = s.a;
    for &p in through.iter() {
        if p != prev {
            out.push(Seg { a: prev, b: p, w: s.w });
            prev = p;
        }
    }
    if prev != s.b {
        out.push(Seg { a: prev, b: s.b, w: s.w });
    }
}

/// Does segment ab pass through the closed unit pixel centred on p, a point
/// within ab's x-range (the callers check that)?
///
/// In doubled coordinates the pixel corners are integers and their
/// orientations against ab are `4o ± 2dx ± 2dy`, with `o` the orientation
/// of the centre: the line meets the pixel exactly when `2|o| <= |dx| + |dy|`.
#[inline]
fn segment_hits_pixel(a: Pt, b: Pt, p: Pt, small: bool) -> bool {
    if a.y.max(b.y) < p.y || a.y.min(b.y) > p.y {
        return false;
    }
    if small {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let o = dx * (p.y - a.y) - dy * (p.x - a.x);
        o.unsigned_abs() <= (dx.unsigned_abs() + dy.unsigned_abs()) / 2
    } else {
        let (dx, dy) = ((b.x - a.x) as i128, (b.y - a.y) as i128);
        let o = dx * (p.y - a.y) as i128 - dy * (p.x - a.x) as i128;
        2 * o.abs() <= dx.abs() + dy.abs()
    }
}

/// `orient` for points whose coordinates lie within ±2^30.
#[inline]
fn orient64(a: Pt, b: Pt, c: Pt) -> i64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// Do p1p2 and p3p4, whose x- and y-ranges overlap (the caller's sweep
/// checks that), cross properly (each one's ends strictly on opposite sides
/// of the other)? The same answer either way round.
#[inline]
fn crosses(p1: Pt, p2: Pt, p3: Pt, p4: Pt, small: bool) -> bool {
    let opposite = |a: Pt, b: Pt, c: Pt, d: Pt| -> bool {
        if small {
            let (o1, o2) = (orient64(a, b, c).signum(), orient64(a, b, d).signum());
            o1 * o2 < 0
        } else {
            let (o1, o2) = (orient(a, b, c).signum(), orient(a, b, d).signum());
            o1 * o2 < 0
        }
    };
    opposite(p3, p4, p1, p2) && opposite(p1, p2, p3, p4)
}

/// Where p1p2 crosses p3p4, rounded to the grid (computed along p1p2).
fn crossing_point(p1: Pt, p2: Pt, p3: Pt, p4: Pt) -> Pt {
    let d1 = p2.sub(p1);
    let d2 = p4.sub(p3);
    let den = cross(d1, d2);
    let num = cross(p3.sub(p1), d2);
    let x = p1.x as i128 + div_round(d1.x as i128 * num, den);
    let y = p1.y as i128 + div_round(d1.y as i128 * num, den);
    Pt::new(x as i64, y as i64)
}

#[inline]
fn div_round(n: i128, d: i128) -> i128 {
    let (n, d) = if d < 0 { (-n, -d) } else { (n, d) };
    if n >= 0 {
        (2 * n + d) / (2 * d)
    } else {
        -((-2 * n + d) / (2 * d))
    }
}

/// A uniform grid over the segments' bounds; segments are binned by the
/// cells their path crosses (not their bounding box), so long diagonals stay
/// cheap.
struct Grid {
    x0: i64,
    y0: i64,
    cell: i64,
    nx: usize,
    ny: usize,
    /// Every coordinate lies within ±2^30.
    small: bool,
}

/// Compressed cell lists.
struct Bins {
    start: ScratchVec<u32>,
    items: ScratchVec<u32>,
    /// The cells that hold anything, in order (most cells are empty).
    occ: ScratchVec<u32>,
}


impl Grid {
    fn new(segs: &[Seg]) -> Self {
        let mut b = BBox::empty();
        let mut total_len = 0.0;
        for s in segs {
            b.add(s.a);
            b.add(s.b);
            total_len += s.a.dist(s.b);
        }
        let w = (b.max.x - b.min.x).max(1) as f64;
        let h = (b.max.y - b.min.y).max(1) as f64;
        let n = segs.len().max(1) as f64;
        let avg = total_len / n;
        // About one segment length per cell, but never more than ~4n cells.
        let min_cell = (w * h / (4.0 * n)).sqrt();
        let cell = avg.max(min_cell).max(16.0).ceil() as i64;
        let nx = ((w as i64) / cell + 1) as usize;
        let ny = ((h as i64) / cell + 1) as usize;
        const LIM: i64 = 1 << 30;
        let small = b.min.x > -LIM && b.min.y > -LIM && b.max.x < LIM && b.max.y < LIM;
        Self { x0: b.min.x, y0: b.min.y, cell, nx, ny, small }
    }

    /// The same bounds with cells for `n` points: about two points per cell
    /// on average, never coarser than `self`. Dense crossings (outward
    /// offsets of finely curved concave parts cross pairwise) would
    /// otherwise put thousands of hot pixels in the cells a segment visits.
    fn for_points(&self, n: usize) -> Self {
        let (w, h) = ((self.nx as i64 * self.cell) as f64, (self.ny as i64 * self.cell) as f64);
        let cell = ((w * h / (n.max(1) as f64 / 2.0)).sqrt().max(16.0).ceil() as i64).min(self.cell);
        if cell >= self.cell {
            return Self { ..*self };
        }
        let nx = ((self.nx as i64 * self.cell) / cell + 1) as usize;
        let ny = ((self.ny as i64 * self.cell) / cell + 1) as usize;
        Self { x0: self.x0, y0: self.y0, cell, nx, ny, small: self.small }
    }

    #[inline]
    fn ncells(&self) -> usize {
        self.nx * self.ny
    }

    #[inline]
    fn cx(&self, x: i64) -> i64 {
        ((x - self.x0).div_euclid(self.cell)).clamp(0, self.nx as i64 - 1)
    }

    #[inline]
    fn cy(&self, y: i64) -> i64 {
        ((y - self.y0).div_euclid(self.cell)).clamp(0, self.ny as i64 - 1)
    }

    #[inline]
    fn cell_of(&self, p: Pt) -> usize {
        self.cy(p.y) as usize * self.nx + self.cx(p.x) as usize
    }

    /// The least corner of cell `c`.
    #[inline]
    fn corner(&self, c: usize) -> Pt {
        Pt::new(self.x0 + (c % self.nx) as i64 * self.cell, self.y0 + (c / self.nx) as i64 * self.cell)
    }

    /// Is `p` in the cell whose least corner is `k`? For points within the
    /// segments' bounds (ends, and crossings rounded between them) the same
    /// as `cell_of(p) == c`, by two compares instead of two divisions.
    #[inline]
    fn holds(&self, k: Pt, p: Pt) -> bool {
        ((p.x - k.x) as u64) < self.cell as u64 && ((p.y - k.y) as u64) < self.cell as u64
    }

    /// Every cell the segment, grown by `pad` units, passes through: in each
    /// column, the rows of the segment's y-range over the column's x-range
    /// (both grown by `pad`).
    fn cells_of_segment(&self, a: Pt, b: Pt, pad: i64, out: &mut ScratchVec<u32>) {
        let (a, b) = if a.x <= b.x { (a, b) } else { (b, a) };
        let c0 = self.cx(a.x - pad);
        let c1 = self.cx(b.x + pad);
        let r0 = self.cy(a.y.min(b.y) - pad);
        let r1 = self.cy(a.y.max(b.y) + pad);
        if c0 == c1 || r0 == r1 || a.x == b.x || a.y == b.y {
            // One column, one row or an axis-parallel segment: every column
            // reaches every row.
            for c in c0..=c1 {
                for r in r0..=r1 {
                    out.push((r as usize * self.nx + c as usize) as u32);
                }
            }
        } else if self.small {
            self.walk::<i64>(a, b, pad, [c0, c1, r0, r1], out);
        } else {
            self.walk::<i128>(a, b, pad, [c0, c1, r0, r1], out);
        }
    }

    /// `cells_of_segment` for a sloped segment (`a.x < b.x`) over columns
    /// `c0..=c1` and rows `r0..=r1`. Along the segment a column's least and
    /// greatest row only move one way, so two row counters step through them
    /// by exact comparisons: a row edge `k` lies below the segment at `x`
    /// when `(k - a.y) dx < dy (x - a.x)`, products in `T` (`i64` when every
    /// coordinate is small).
    #[inline]
    fn walk<T>(&self, a: Pt, b: Pt, pad: i64, [c0, c1, r0, r1]: [i64; 4], out: &mut ScratchVec<u32>)
    where
        T: Copy + PartialOrd + From<i64> + std::ops::Mul<Output = T>,
    {
        let (dx, dy) = (T::from(b.x - a.x), b.y - a.y);
        let at = |x: i64| T::from(dy) * T::from(x - a.x);
        let edge = |k: i64| T::from(k - a.y) * dx;
        let (y0, cell) = (self.y0, self.cell);
        // Both counters start at a's end of the row range and move away.
        let (mut lo, mut hi) = if dy > 0 { (r0, r0) } else { (r1, r1) };
        let mut l = self.x0 + c0 * cell;
        for c in c0..=c1 {
            let yl = at((l - pad).max(a.x));
            let yr = at((l + cell - 1 + pad).min(b.x));
            l += cell;
            if dy > 0 {
                // Rising: the least row from the y at the column's left
                // (floored, less pad), the greatest from the y at its right.
                while lo < r1 && edge(y0 + (lo + 1) * cell + pad) <= yl {
                    lo += 1;
                }
                while hi < r1 && edge(y0 + (hi + 1) * cell - pad - 1) < yr {
                    hi += 1;
                }
            } else {
                // Falling: the greatest row from the left, the least from the
                // right.
                while hi > r0 && edge(y0 + hi * cell - pad - 1) >= yl {
                    hi -= 1;
                }
                while lo > r0 && edge(y0 + lo * cell + pad) > yr {
                    lo -= 1;
                }
            }
            for r in lo..=hi {
                out.push((r as usize * self.nx + c as usize) as u32);
            }
        }
    }

    fn bin(&self, segs: &[Seg], pad: i64) -> Bins {
        let nc = self.ncells();
        // One cell walk per segment: (cell, segment) pairs, then a counting
        // sort by cell (segments stay in index order within a cell).
        let mut pairs: ScratchVec<u32> = ScratchVec::with_capacity(segs.len() * 3);
        let mut owner: ScratchVec<u32> = ScratchVec::with_capacity(segs.len() * 3);
        for (i, s) in segs.iter().enumerate() {
            let k = pairs.len();
            self.cells_of_segment(s.a, s.b, pad, &mut pairs);
            let end = owner.len() + (pairs.len() - k);
            owner.resize(end, i as u32);
        }
        let mut start = ScratchVec::filled(nc + 1, 0u32);
        for &c in &pairs {
            start[c as usize] += 1;
        }
        // start[c] = where cell c ends, then filled back to front (so each
        // cell keeps index order) down to where it starts.
        let mut occ = ScratchVec::new();
        let mut sum = 0;
        for (c, n) in start[..nc].iter_mut().enumerate() {
            if *n != 0 {
                occ.push(c as u32);
            }
            sum += *n;
            *n = sum;
        }
        start[nc] = sum;
        let mut items = ScratchVec::filled(pairs.len(), 0u32);
        for (&c, &i) in pairs.iter().zip(&owner).rev() {
            start[c as usize] -= 1;
            items[start[c as usize] as usize] = i;
        }
        Bins { start, items, occ }
    }

    fn bin_points(&self, pts: &[Pt]) -> Bins {
        let nc = self.ncells();
        let mut start = ScratchVec::filled(nc + 1, 0u32);
        for p in pts {
            start[self.cell_of(*p) + 1] += 1;
        }
        for c in 0..nc {
            start[c + 1] += start[c];
        }
        let mut fill: ScratchVec<u32> = start.iter().copied().collect();
        let mut items = ScratchVec::filled(pts.len(), 0u32);
        for (i, p) in pts.iter().enumerate() {
            let c = self.cell_of(*p);
            items[fill[c] as usize] = i as u32;
            fill[c] += 1;
        }
        Bins { start, items, occ: ScratchVec::new() }
    }
}

// ---------------------------------------------------------------- graph

const NONE: u32 = u32::MAX;

struct Graph {
    verts: ScratchVec<Pt>,
    /// Half-edge h: origin vertex; twin is h ^ 1.
    origin: ScratchVec<u32>,
    /// Contribution of edge e (half-edge 2e's direction; 2e + 1 counts it
    /// negated).
    w: ScratchVec<[i32; 2]>,
    /// Outgoing half-edges of each vertex, counter-clockwise by angle:
    /// `adj[adj_start[v]..adj_start[v + 1]]`.
    adj: ScratchVec<u32>,
    adj_start: ScratchVec<u32>,
    /// The half-edge after h around the face on its left: at h's
    /// destination, the outgoing half-edge just clockwise of h's twin.
    next: ScratchVec<u32>,
    /// Each vertex's connected component and the number of components, as
    /// found while numbering; `None` when an edge cancelled (that can cut a
    /// component in two: `extract` searches instead).
    comp: Option<(ScratchVec<u32>, usize)>,
}

#[inline]
fn half(v: Pt) -> u8 {
    // 0 for angles in [0, π), 1 for [π, 2π).
    if v.y > 0 || (v.y == 0 && v.x > 0) {
        0
    } else {
        1
    }
}

#[inline]
fn angle_cmp(a: Pt, b: Pt) -> std::cmp::Ordering {
    let (ha, hb) = (half(a), half(b));
    if ha != hb {
        return ha.cmp(&hb);
    }
    let c = cross(a, b);
    0.cmp(&c)
}

impl Graph {
    /// The planar graph of the routed pieces: coincident pieces summed per
    /// edge, cancelled ones dropped (`None` when all cancel). Vertices and
    /// edges are numbered as the pieces come; the faces, windings and rings
    /// do not depend on the numbering, and `extract` puts the rings in a
    /// canonical order.
    fn build(pieces: &[Seg]) -> Option<Self> {
        let np = pieces.len();
        // Vertices: the distinct endpoints (every one starts a piece), ids in
        // first-seen order through an open-addressing table.
        let mut verts: ScratchVec<Pt> = ScratchVec::with_capacity(np);
        // The table holds ids only (a quarter full at most; a slot's point
        // is `verts[id]`): 4 bytes a slot, not 20, so it stays in cache.
        let cap = (np * 4).next_power_of_two().max(16);
        let shift = 64 - cap.trailing_zeros();
        let mut slot: ScratchVec<u32> = ScratchVec::filled(cap, NONE);
        let mut id = |p: Pt, verts: &mut Vec<Pt>| -> u32 {
            let h = ((p.x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (p.y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F).rotate_left(31)).wrapping_mul(0x1656_67B1_9E37_79F9);
            let mut i = (h >> shift) as usize;
            loop {
                let v = slot[i];
                if v == NONE {
                    let v = verts.len() as u32;
                    slot[i] = v;
                    verts.push(p);
                    return v;
                }
                if verts[v as usize] == p {
                    return v;
                }
                i = (i + 1) & (cap - 1);
            }
        };
        // Piece i: half-edges 2i (a -> b) and 2i + 1 (b -> a). The pieces
        // run along their rings, so a piece mostly starts where the last one
        // ended: that end's id without a lookup.
        //
        // Components on the way: a chain of pieces (each starting where the
        // last ended) is connected; vertex v is labelled with the chain it
        // was first seen on, and an end found already numbered joins its
        // chain to that label's.
        let mut origin: ScratchVec<u32> = ScratchVec::with_capacity(2 * np);
        let mut label: ScratchVec<u32> = ScratchVec::with_capacity(np);
        let mut chains = UnionFind { p: ScratchVec::new() };
        let mut cur = 0u32;
        let mut last = (Pt::new(i64::MIN, i64::MIN), 0);
        for s in pieces {
            let a = if s.a == last.0 {
                last.1
            } else {
                cur = chains.p.len() as u32;
                chains.p.push(cur);
                let n0 = verts.len();
                let a = id(s.a, &mut verts);
                if verts.len() > n0 {
                    label.push(cur);
                } else {
                    chains.union(cur, label[a as usize]);
                }
                a
            };
            let n0 = verts.len();
            let b = id(s.b, &mut verts);
            if verts.len() > n0 {
                label.push(cur);
            } else {
                chains.union(cur, label[b as usize]);
            }
            origin.push(a);
            origin.push(b);
            last = (s.b, b);
        }
        let nv = verts.len();
        let mut adj_start = ScratchVec::filled(nv + 1, 0u32);
        for &o in &origin {
            adj_start[o as usize + 1] += 1;
        }
        for v in 0..nv {
            adj_start[v + 1] += adj_start[v];
        }
        let mut fill: ScratchVec<u32> = adj_start[..nv].iter().copied().collect();
        let mut adj = ScratchVec::filled(origin.len(), 0u32);
        for (h, &o) in origin.iter().enumerate() {
            adj[fill[o as usize] as usize] = h as u32;
            fill[o as usize] += 1;
        }
        // Around each vertex by angle; pieces joining the same two vertices
        // leave in the same direction, so they end up next to each other:
        // summed into the first of them (at the lower vertex id), the others
        // dropped, and the first too when the sum cancels.
        let mut w: ScratchVec<[i32; 2]> = pieces.iter().map(|s| s.w).collect();
        let mut dead: ScratchVec<bool> = ScratchVec::new();
        let mut cancelled = false;
        for v in 0..nv {
            let p = verts[v];
            let list = &mut adj[adj_start[v] as usize..adj_start[v + 1] as usize];
            // Two half-edges are in counter-clockwise order either way round
            // (most vertices lie on one ring).
            if list.len() > 2 {
                list.sort_by(|&x, &y| {
                    let dx = verts[origin[(x ^ 1) as usize] as usize].sub(p);
                    let dy = verts[origin[(y ^ 1) as usize] as usize].sub(p);
                    angle_cmp(dx, dy)
                });
            }
            let mut i = 0;
            while i + 1 < list.len() {
                let d = origin[(list[i] ^ 1) as usize];
                let mut j = i + 1;
                while j < list.len() && origin[(list[j] ^ 1) as usize] == d {
                    j += 1;
                }
                if j - i > 1 && (v as u32) < d {
                    if dead.is_empty() {
                        dead = ScratchVec::filled(np, false);
                    }
                    let mut sum = [0, 0];
                    for &h in &list[i..j] {
                        let c = w[(h >> 1) as usize];
                        let c = if h & 1 == 0 { c } else { [-c[0], -c[1]] };
                        sum = [sum[0] + c[0], sum[1] + c[1]];
                        dead[(h >> 1) as usize] = true;
                    }
                    let h0 = list[i];
                    w[(h0 >> 1) as usize] = if h0 & 1 == 0 { sum } else { [-sum[0], -sum[1]] };
                    dead[(h0 >> 1) as usize] = sum == [0, 0];
                    cancelled |= sum == [0, 0];
                }
                i = j;
            }
        }
        if !dead.is_empty() {
            // Renumber the kept pieces, in order; the lists keep their order.
            let mut new_id = ScratchVec::filled(np, NONE);
            let mut ne = 0u32;
            for i in 0..np {
                if !dead[i] {
                    new_id[i] = ne;
                    origin[2 * ne as usize] = origin[2 * i];
                    origin[2 * ne as usize + 1] = origin[2 * i + 1];
                    w[ne as usize] = w[i];
                    ne += 1;
                }
            }
            if ne == 0 {
                return None;
            }
            origin.truncate(2 * ne as usize);
            w.truncate(ne as usize);
            let mut k = 0;
            let mut from = 0;
            for v in 0..nv {
                let to = adj_start[v + 1] as usize;
                for x in from..to {
                    let h = adj[x];
                    let n = new_id[(h >> 1) as usize];
                    if n != NONE {
                        adj[k] = 2 * n | (h & 1);
                        k += 1;
                    }
                }
                from = to;
                adj_start[v + 1] = k as u32;
            }
            adj.truncate(k);
        }
        let mut next = ScratchVec::filled(origin.len(), 0u32);
        for v in 0..nv {
            let list = &adj[adj_start[v] as usize..adj_start[v + 1] as usize];
            if let Some(&last) = list.last() {
                let mut cw = last;
                for &h in list {
                    next[(h ^ 1) as usize] = cw;
                    cw = h;
                }
            }
        }
        let comp = (!cancelled).then(|| {
            let mut dense = ScratchVec::filled(chains.p.len(), NONE);
            let mut k = 0;
            let of: ScratchVec<u32> = label
                .iter()
                .map(|&c| {
                    let r = chains.find(c) as usize;
                    if dense[r] == NONE {
                        dense[r] = k;
                        k += 1;
                    }
                    dense[r]
                })
                .collect();
            (of, k as usize)
        });
        Some(Self { verts, origin, w, adj, adj_start, next, comp })
    }

    #[inline]
    fn out(&self, v: usize) -> &[u32] {
        &self.adj[self.adj_start[v] as usize..self.adj_start[v + 1] as usize]
    }

    #[inline]
    fn dest(&self, h: u32) -> u32 {
        self.origin[(h ^ 1) as usize]
    }

    /// Next half-edge around the face on the left of `h`.
    #[inline]
    fn face_next(&self, h: u32) -> u32 {
        self.next[h as usize]
    }

    /// The outgoing half-edge that is next clockwise from `h` around its
    /// origin.
    #[inline]
    fn rot_cw(&self, h: u32) -> u32 {
        self.next[(h ^ 1) as usize]
    }

    fn extract(&self, inside: impl Fn([i32; 2]) -> bool) -> Vec<ExPolygon> {
        let nh = self.origin.len();
        // Faces: face f's half-edges in order around it are
        // `cycles[face_start[f]..face_start[f + 1]]`.
        let mut face = ScratchVec::filled(nh, NONE);
        let mut cycles: ScratchVec<u32> = ScratchVec::with_capacity(nh);
        let mut face_start: ScratchVec<u32> = ScratchVec::new();
        for h in 0..nh as u32 {
            if face[h as usize] != NONE {
                continue;
            }
            let f = face_start.len() as u32;
            face_start.push(cycles.len() as u32);
            let mut e = h;
            loop {
                face[e as usize] = f;
                cycles.push(e);
                e = self.face_next(e);
                if e == h {
                    break;
                }
            }
        }
        let nf = face_start.len();
        face_start.push(cycles.len() as u32);

        // Connected components over vertices: the least vertex of each.
        let nv = self.verts.len();
        let comps: ScratchVec<u32> = match &self.comp {
            Some((of, k)) => {
                let mut least = ScratchVec::filled(*k, NONE);
                for (v, &c) in of.iter().enumerate() {
                    let m = &mut least[c as usize];
                    if *m == NONE || self.verts[v] < self.verts[*m as usize] {
                        *m = v as u32;
                    }
                }
                least
            }
            None => {
                let mut comp = ScratchVec::filled(nv, NONE);
                let mut comps: ScratchVec<u32> = ScratchVec::new();
                let mut stack = ScratchVec::new();
                for v0 in 0..nv {
                    // (A vertex whose edges all cancelled has none left.)
                    if comp[v0] != NONE || self.out(v0).is_empty() {
                        continue;
                    }
                    let c = comps.len() as u32;
                    let mut vmin = v0 as u32;
                    comp[v0] = c;
                    stack.push(v0 as u32);
                    while let Some(v) = stack.pop() {
                        if self.verts[v as usize] < self.verts[vmin as usize] {
                            vmin = v;
                        }
                        for &h in self.out(v as usize) {
                            let d = self.dest(h);
                            if comp[d as usize] == NONE {
                                comp[d as usize] = c;
                                stack.push(d);
                            }
                        }
                    }
                    comps.push(vmin);
                }
                comps
            }
        };
        let mut comp_order: ScratchVec<u32> = (0..comps.len() as u32).collect();
        comp_order.sort_by_key(|&c| self.verts[comps[c as usize] as usize]);

        // Leftward rays: a few components scan every edge, more get strips.
        // A strip holds every edge spanning its rows, in edge order, so both
        // find the same edge.
        let index = (comps.len() > SCAN_COMPONENTS).then(|| EdgeStrips::new(self));

        let mut wind: ScratchVec<[i32; 2]> = ScratchVec::filled(nf, [0, 0]);
        let mut known = ScratchVec::filled(nf, false);
        // Region union-find over faces (merges faces that share an inside
        // area, including a component's exterior with its container).
        let mut uf = UnionFind::new(nf);
        let mut fqueue = ScratchVec::new();
        for (i, &c) in comp_order.iter().enumerate() {
            let v = comps[c as usize];
            let p = self.verts[v as usize];
            // Exterior face of this component: left of the outgoing edge with
            // the largest angle (all outgoing edges point into x >= p.x).
            let list = self.out(v as usize);
            let mut hmax = list[0];
            for &h in list.iter().skip(1) {
                let dh = self.verts[self.dest(h) as usize].sub(p);
                let dm = self.verts[self.dest(hmax) as usize].sub(p);
                if cross(dm, dh) > 0 {
                    hmax = h;
                }
            }
            let outer = face[hmax as usize];
            // The first component holds the least vertex: no edge reaches
            // left of it.
            let found = match &index {
                _ if i == 0 => None,
                Some(strips) => strips.locate_left(self, p),
                None => nearest_left(self, p, (0..nh as u32).step_by(2)),
            };
            let container = found.map(|h| face[h as usize]);
            let w0 = container.map(|f| wind[f as usize]).unwrap_or([0, 0]);
            if let Some(cf) = container {
                if inside(w0) {
                    uf.union(cf, outer);
                }
            }
            wind[outer as usize] = w0;
            known[outer as usize] = true;
            fqueue.clear();
            fqueue.push(outer);
            while let Some(f) = fqueue.pop() {
                let wf = wind[f as usize];
                for &e in &cycles[face_start[f as usize] as usize..face_start[f as usize + 1] as usize] {
                    let g = face[(e ^ 1) as usize];
                    if !known[g as usize] {
                        let c = self.w[(e >> 1) as usize];
                        wind[g as usize] = if e & 1 == 0 { [wf[0] - c[0], wf[1] - c[1]] } else { [wf[0] + c[0], wf[1] + c[1]] };
                        known[g as usize] = true;
                        fqueue.push(g);
                    }
                }
            }
        }
        let filled: ScratchVec<bool> = wind.iter().map(|w| inside(*w)).collect();
        let boundary = |h: u32| filled[face[h as usize] as usize] && !filled[face[(h ^ 1) as usize] as usize];
        for h in (0..nh as u32).step_by(2) {
            let f = face[h as usize];
            let g = face[(h ^ 1) as usize];
            if filled[f as usize] && filled[g as usize] {
                uf.union(f, g);
            }
        }

        // Link boundary half-edges into rings. Each ring is a cycle; its
        // canonical form starts at its least half-edge by (lower end, upper
        // end, upper end first), and the rings go in that order: as if the
        // edges were numbered by their ends.
        let key = |h: u32| {
            let o = self.verts[self.origin[h as usize] as usize];
            let d = self.verts[self.origin[(h ^ 1) as usize] as usize];
            if o < d {
                (o, d, false)
            } else {
                (d, o, true)
            }
        };
        let mut used = ScratchVec::filled(nh, false);
        let mut rings: ScratchVec<((Pt, Pt, bool), Vec<Pt>, u32)> = ScratchVec::new();
        for h0 in 0..nh as u32 {
            if used[h0 as usize] || !boundary(h0) {
                continue;
            }
            let mut pts = Vec::new();
            let mut h = h0;
            let mut least = (key(h0), 0);
            loop {
                used[h as usize] = true;
                let k = key(h);
                if k < least.0 {
                    least = (k, pts.len());
                }
                pts.push(self.verts[self.origin[h as usize] as usize]);
                let mut c = self.face_next(h);
                let mut guard = 0;
                while !boundary(c) {
                    c = self.rot_cw(c);
                    guard += 1;
                    if guard > self.out(self.origin[c as usize] as usize).len() + 1 {
                        break;
                    }
                }
                h = c;
                if h == h0 || used[h as usize] {
                    break;
                }
            }
            pts.rotate_left(least.1);
            let region = uf.find(face[h0 as usize]);
            rings.push((least.0, pts, region));
        }
        rings.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        // Group by region.
        let mut by_region: ScratchVec<Option<(Vec<Polygon>, Vec<Polygon>)>> = ScratchVec::new();
        by_region.resize_with(nf, || None);
        let mut region_order: ScratchVec<u32> = ScratchVec::new();
        for (_, mut pts, region) in rings.drain(..) {
            clean_ring(&mut pts);
            if pts.len() < 3 {
                continue;
            }
            let poly = Polygon::new(pts);
            let a = poly.area2();
            if a == 0 {
                continue;
            }
            let entry = by_region[region as usize].get_or_insert_with(|| {
                region_order.push(region);
                (Vec::new(), Vec::new())
            });
            if a > 0 {
                entry.0.push(poly);
            } else {
                entry.1.push(poly);
            }
        }
        let mut out = Vec::new();
        for r in region_order.drain(..) {
            let (outers, holes) = by_region[r as usize].take().unwrap();
            if outers.len() == 1 {
                out.push(ExPolygon { contour: outers.into_iter().next().unwrap(), holes });
                continue;
            }
            let base = out.len();
            for o in outers {
                out.push(ExPolygon::new(o));
            }
            for h in holes {
                let probe = hole_probe(&h);
                let mut best: Option<usize> = None;
                for (i, e) in out.iter().enumerate().skip(base) {
                    if e.contour.winding(probe) != 0 {
                        let better = match best {
                            None => true,
                            Some(b) => e.contour.area2() < out[b].contour.area2(),
                        };
                        if better {
                            best = Some(i);
                        }
                    }
                }
                if let Some(b) = best {
                    out[b].holes.push(h);
                }
            }
        }
        out
    }
}

/// A grid point inside the area enclosed by a hole ring (close to its first
/// edge), for containment tests against contours.
fn hole_probe(h: &Polygon) -> Pt {
    // The hole is clockwise: its enclosed area is on the right of each edge.
    let a = h.pts[0];
    let b = h.pts[1];
    let mid = Pt::new((a.x + b.x) / 2, (a.y + b.y) / 2);
    let d = b.sub(a);
    let len = ((d.x as f64).powi(2) + (d.y as f64).powi(2)).sqrt().max(1.0);
    let step = 2.0;
    Pt::new(mid.x + ((d.y as f64) / len * step).round() as i64, mid.y - ((d.x as f64) / len * step).round() as i64)
}

struct UnionFind {
    p: ScratchVec<u32>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self { p: (0..n as u32).collect() }
    }
    fn find(&mut self, mut x: u32) -> u32 {
        while self.p[x as usize] != x {
            let gp = self.p[self.p[x as usize] as usize];
            self.p[x as usize] = gp;
            x = gp;
        }
        x
    }
    fn union(&mut self, a: u32, b: u32) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.p[ra.max(rb) as usize] = ra.min(rb);
        }
    }
}

/// Edges bucketed into horizontal strips for point location.
struct EdgeStrips {
    y0: i64,
    /// Strips are `1 << shift` tall: a point's strip is a shift.
    shift: u32,
    /// Strip s holds `items[start[s]..start[s + 1]]`, in edge order.
    start: ScratchVec<u32>,
    items: ScratchVec<u32>,
}

impl EdgeStrips {
    fn new(g: &Graph) -> Self {
        let ne = g.origin.len() / 2;
        let mut ymin = i64::MAX;
        let mut ymax = i64::MIN;
        for v in &g.verts {
            ymin = ymin.min(v.y);
            ymax = ymax.max(v.y);
        }
        // About sqrt(ne) strips, each a power of two tall (at most twice as
        // many strips, none taller): `n` is the last strip.
        let n = ((ne as f64).sqrt().ceil() as i64).clamp(1, 4096);
        let shift = (((ymax - ymin) / n).max(1) + 1).ilog2();
        let n = ((ymax - ymin) >> shift) as usize;
        let span = |e: usize| -> Option<(usize, usize)> {
            let a = g.verts[g.origin[2 * e] as usize];
            let b = g.verts[g.origin[2 * e + 1] as usize];
            if a.y == b.y {
                return None;
            }
            Some((((a.y.min(b.y) - ymin) >> shift) as usize, ((a.y.max(b.y) - ymin) >> shift) as usize))
        };
        let mut start = ScratchVec::filled(n + 2, 0u32);
        for e in 0..ne {
            if let Some((s0, s1)) = span(e) {
                for c in &mut start[s0 + 1..s1 + 2] {
                    *c += 1;
                }
            }
        }
        for s in 0..=n {
            start[s + 1] += start[s];
        }
        let mut fill: ScratchVec<u32> = start[..=n].iter().copied().collect();
        let mut items = ScratchVec::filled(start[n + 1] as usize, 0u32);
        for e in 0..ne {
            if let Some((s0, s1)) = span(e) {
                for f in &mut fill[s0..=s1] {
                    items[*f as usize] = (2 * e) as u32;
                    *f += 1;
                }
            }
        }
        Self { y0: ymin, shift, start, items }
    }

    /// The half-edge whose left face contains the point just left of `p`,
    /// i.e. the nearest edge crossed by a leftward ray from `p`.
    fn locate_left(&self, g: &Graph, p: Pt) -> Option<u32> {
        if p.y < self.y0 {
            return None;
        }
        let s = ((p.y - self.y0) >> self.shift) as usize;
        if s + 1 >= self.start.len() {
            return None;
        }
        nearest_left(g, p, self.items[self.start[s] as usize..self.start[s + 1] as usize].iter().copied())
    }
}

/// Up to this many components scan all edges for their containing face;
/// building strips costs more than a few scans.
const SCAN_COMPONENTS: usize = 8;

/// Of the half-edges `hs` (one per edge, in edge order), the one whose left
/// face holds the point just left of `p`: the nearest edge crossed by a
/// leftward ray from `p`, as its downward half-edge.
fn nearest_left(g: &Graph, p: Pt, hs: impl Iterator<Item = u32>) -> Option<u32> {
    // Best crossing as a rational x = num/den (den > 0), with the edge's
    // slope for ties.
    let mut best: Option<(i128, i128, u32)> = None;
    for h in hs {
        let o = g.verts[g.origin[h as usize] as usize];
        let d = g.verts[g.origin[(h ^ 1) as usize] as usize];
        // `down` runs from higher to lower y; (a, b) is the edge bottom-up.
        let (a, b, down) = if o.y > d.y { (d, o, h) } else { (o, d, h ^ 1) };
        if !(a.y <= p.y && p.y < b.y) {
            continue;
        }
        let den = (b.y - a.y) as i128;
        let num = a.x as i128 * den + (p.y - a.y) as i128 * (b.x - a.x) as i128;
        // x < p.x strictly
        if num >= p.x as i128 * den {
            continue;
        }
        let better = match best {
            None => true,
            Some((bn, bd, bh)) => {
                let l = num * bd;
                let r = bn * den;
                if l != r {
                    l > r
                } else {
                    // Tie at the same crossing: the edge further right just
                    // above the ray wins (larger dx/dy).
                    let ea = g.verts[g.origin[bh as usize] as usize];
                    let eb = g.verts[g.origin[(bh ^ 1) as usize] as usize];
                    let (ea, eb) = if ea.y > eb.y { (eb, ea) } else { (ea, eb) };
                    let s_new = (b.x - a.x) as i128 * (eb.y - ea.y) as i128;
                    let s_old = (eb.x - ea.x) as i128 * (b.y - a.y) as i128;
                    s_new > s_old
                }
            }
        };
        if better {
            best = Some((num, den, down));
        }
    }
    // The downward half-edge has +x on its left: that face holds p.
    best.map(|(_, _, h)| h)
}
