//! Layer contours straight from a distance field: the cross-section
//! `f(x, y, z) = 0` of a part at each layer height, with no triangle mesh.
//! (Prototype for the pipeline switch; the kernel moves to csg_sdf.)
//!
//! Per layer:
//! 1. a coarse grid over the part's footprint finds the cells the surface can
//!    pass through (|f| at a corner within the cell's reach, by the field's
//!    Lipschitz bound): no surface is missed that is wider than the fine cell;
//! 2. those cells are walked on one fine grid (global node indices, so
//!    neighbouring cells share every edge crossing exactly: watertight);
//! 3. each edge crossing is refined onto the zero set (bisection to
//!    `tolerance`), so the contour's points are on the surface whatever the
//!    grid;
//! 4. a cell whose two crossings meet at a crease (their gradients more than
//!    `sharp_deg` apart: a hard min/max boolean) gets the crease point: the
//!    intersection of the two tangent lines (a 2D QEF), so a box minus a
//!    cylinder keeps square corners;
//! 5. segments are oriented with the material on their left and chained into
//!    regions by the mesh path's own [`crate::slice::segments_to_regions`].

use crate::geom::Pt;

/// A field at one layer: `f(x, y)` in mm, negative inside.
pub type Field2<'a> = &'a (dyn Fn(f64, f64) -> f64 + Sync);

#[derive(Clone, Copy, Debug)]
pub struct SdfParams {
    /// Coarse search cell (mm), at most 63 fine cells.
    pub coarse: f64,
    /// Fine contouring cell (mm): features thinner than this may be missed.
    pub fine: f64,
    /// How steep the field can be at the values within `window` of 0 (1 for a true distance
    /// field): a value `v` proves the circle of `min(|v|, window) / lipschitz` about its point
    /// clear.
    pub lipschitz: f64,
    /// See `lipschitz` (infinite: at every value).
    pub window: f64,
    /// Crossing refinement tolerance (mm).
    pub tolerance: f64,
    /// Crossing gradients further apart than this make a crease (degrees).
    pub sharp_deg: f64,
}

impl Default for SdfParams {
    fn default() -> Self {
        Self { coarse: 1.0, fine: 0.1, lipschitz: 1.0, window: f64::INFINITY, tolerance: 1e-4, sharp_deg: 25.0 }
    }
}

impl SdfParams {
    /// The largest circle a skipped cell's corners must prove clear (the finer search's are
    /// smaller): a `lipschitz` and `window` that prove circles of this radius serve every skip.
    pub fn reach(&self) -> f64 {
        self.coarse * COARSE_REACH * std::f64::consts::SQRT_2
    }
}

/// The central-difference step for gradients. Compiled kernel fields evaluate in f32 (about
/// 8e-6 mm resolution at 80 mm): a step under ~1 µm reads that rounding as the gradient, which
/// flips the contour's orientation and makes islands out of nothing.
const GRAD_H: f64 = 1e-3;

/// Each point moved onto the zero set along the gradient (up to 4 Newton steps), all points in
/// lockstep: a batch per step for the values and one for the gradients.
fn project_many(many: Many2, mut ps: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    const H: f64 = GRAD_H;
    let mut active: Vec<usize> = (0..ps.len()).collect();
    let mut moving: Vec<(usize, f64)> = Vec::new();
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for _ in 0..4 {
        if active.is_empty() {
            break;
        }
        pts.clear();
        pts.extend(active.iter().map(|&i| ps[i]));
        let vs = sample_many(many, &pts, f64::INFINITY);
        moving.clear();
        moving.extend(active.iter().zip(vs).filter(|(_, v)| !(v.abs() < 1e-7)).map(|(&i, v)| (i, v)));
        pts.clear();
        pts.extend(moving.iter().flat_map(|&(i, _)| {
            let p = ps[i];
            [(p.0 + H, p.1), (p.0 - H, p.1), (p.0, p.1 + H), (p.0, p.1 - H)]
        }));
        let g = sample_many(many, &pts, f64::INFINITY);
        active.clear();
        for (n, &(i, v)) in moving.iter().enumerate() {
            let gx = (g[4 * n] - g[4 * n + 1]) / (2.0 * H);
            let gy = (g[4 * n + 2] - g[4 * n + 3]) / (2.0 * H);
            let g2 = gx * gx + gy * gy;
            if g2 < 1e-18 {
                continue;
            }
            let p = ps[i];
            ps[i] = (p.0 - v * gx / g2, p.1 - v * gy / g2);
            active.push(i);
        }
    }
    ps
}

/// Many samples of a layer's field at once: `out[k] = f(pts[k]).clamp(-reach, reach)`, bit for
/// bit (an infinite `reach`: the values). A field that runs as a kernel answers a whole batch in
/// one call, and a tree is read only as far as the reach needs.
pub type Many2<'a> = &'a (dyn Fn(&[(f64, f64)], f64, &mut [f64]) + Sync);

/// The contour segments of `f` over the box `lo`..`hi` (mm), oriented with
/// the inside (f < 0) on the left, on the slicer's integer grid.
pub fn contour_segments(f: Field2, lo: [f64; 2], hi: [f64; 2], p: &SdfParams) -> Vec<(Pt, Pt)> {
    let many = |pts: &[(f64, f64)], reach: f64, out: &mut [f64]| {
        for (o, q) in out.iter_mut().zip(pts) {
            *o = f(q.0, q.1).clamp(-reach, reach);
        }
    };
    contour_segments_many(f, &many, lo, hi, p)
}

/// [`contour_segments`] with the grid, the edge crossings and their
/// gradients sampled in batches through `many` (the same values `f` gives;
/// `f` itself only for the rare points a cell needs alone: saddles, creases).
pub fn contour_segments_many(f: Field2, many: Many2, lo: [f64; 2], hi: [f64; 2], p: &SdfParams) -> Vec<(Pt, Pt)> {
    contour_segments_mapped(f, many, lo, hi, p, None)
}

/// [`contour_segments_many`] with the plane's slope read square by square from `map` where
/// given ([`SlopeMap`]): each node's circle at the most slope and the least window of the
/// squares it meets, in place of `p.lipschitz` and `p.window` (values are still read as far as
/// `p.lipschitz` asks: a circle whose slope asks for more is not cleared). A field within the
/// map's slopes traces the segments it traces at `p.lipschitz` alone, bit for bit, where the map
/// asks no more of a circle: a circle the map clears holds no surface, so the cells it leaves out
/// hold no segment.
pub fn contour_segments_mapped(f: Field2, many: Many2, lo: [f64; 2], hi: [f64; 2], p: &SdfParams, map: Option<&SlopeMap>) -> Vec<(Pt, Pt)> {
    let fine = p.fine;
    let k = (p.coarse / fine).round().max(1.0) as i64;
    assert!(k < 64, "a coarse cell is at most 63 fine cells a side");
    let coarse = fine * k as f64;
    // The grid stands on the field's own lattice of coarse cells: fine node (i, j) is at
    // (oi + i, oj + j) * fine, from the coarse lattice line a cell or more below `lo` to one a
    // cell or more past `hi`. The box only decides which cells there are, never where a node sits.
    let below = |lo: f64| ((lo - coarse) / coarse).floor() as i64;
    let beyond = |hi: f64| ((hi + coarse) / coarse).ceil() as i64;
    let (oi, oj) = (below(lo[0]) * k, below(lo[1]) * k);
    let (nx, ny) = (beyond(hi[0]) - below(lo[0]), beyond(hi[1]) - below(lo[1]));
    let node = |i: i64, j: i64| ((oi + i) as f64 * fine, (oj + j) as f64 * fine);
    // (read only as far as the reach the coarse cells are kept by: past it a value tells no more)
    let sample = |pts: &[(f64, f64)]| sample_many(many, pts, coarse * COARSE_REACH * std::f64::consts::SQRT_2 * p.lipschitz);
    // 1. Coarse cells the surface can reach (the coarse nodes in one batch).
    let coarse_pts: Vec<(f64, f64)> = (0..=ny).flat_map(|j| (0..=nx).map(move |i| (i, j))).map(|(i, j)| node(i * k, j * k)).collect();
    let cval = sample(&coarse_pts);
    // a node's circle: this radius, cleared at the slope of the squares it meets
    let circle = coarse * COARSE_REACH * std::f64::consts::SQRT_2;
    let cslope: Vec<(f64, f64)> = match map {
        Some(m) => coarse_pts.iter().map(|q| m.most(q.0, q.1, circle)).map(|(s, w)| (s, w.min(p.window))).collect(),
        None => Vec::new(),
    };
    let slope = |n: usize| if map.is_some() { cslope[n] } else { (p.lipschitz, p.window) };
    let clear = |n: usize| {
        let (s, w) = slope(n);
        cval[n].abs().min(w) >= circle * s
    };
    let mut cells: Vec<(i64, i64)> = Vec::new();
    for j in 0..ny {
        for i in 0..nx {
            let n = [(j * (nx + 1) + i) as usize, (j * (nx + 1) + i + 1) as usize, ((j + 1) * (nx + 1) + i) as usize, ((j + 1) * (nx + 1) + i + 1) as usize];
            let c = n.map(|n| cval[n]);
            let sign_change = c.iter().any(|v| *v < 0.0) && c.iter().any(|v| *v >= 0.0);
            if sign_change || !n.iter().all(|&n| clear(n)) {
                cells.push((i, j));
            }
        }
    }
    // A big layer (the view's layer while a part is dragged) is traced in pieces at once: each
    // piece samples its own cells' nodes and crossings. A crossing on the border of two pieces
    // is computed alike from both sides (the same nodes, the same values), so the contour stays
    // watertight, and the pieces' segments joined in order are the whole layer's.
    let grid = Grid { oi, oj, fine, k };
    let nodes = cells.len() * ((k + 1) * (k + 1)) as usize;
    let pieces = crate::par::threads().min(nodes / PIECE_NODES).max(1);
    if pieces <= 1 {
        return trace_cells(f, many, &grid, &cells, p, map);
    }
    crate::par::map(pieces, |s| trace_cells(f, many, &grid, &cells[cells.len() * s / pieces..cells.len() * (s + 1) / pieces], p, map)).concat()
}

/// Fine nodes a piece of a layer samples at least (a smaller layer is traced whole).
const PIECE_NODES: usize = 8192;

/// A kept coarse cell's sub-cells are this many fine cells a side: their corners are sampled
/// first, and the fine nodes only of the sub-cells the surface can reach.
const SUB: i64 = 2;

/// A cell is kept when a corner's value is under this share of its diagonal times the field's
/// Lipschitz bound. A zero in a cell is within half the diagonal of its nearest corner, so for a
/// field within its bound one half would do. The bound is not always true: a field with no kernel
/// takes 1 unmeasured (an ellipsoid's estimate rises up to 31 times that over a coarse cell), and a
/// kernel's comes from a coarse lattice that a small steep feature can slip through.
///
/// These shares are measured, not proven. The check was sampled: at most 48 layers a part and 1000
/// contour points a layer, at grid alignments one eighth of a cell apart. On thin walls, struts,
/// creases, blends, the finest Voronoi and lattice, thin ellipsoids, warps, sculpts and closed
/// meshes the nearest corner asked at most 0.44 of a coarse diagonal and 0.33 of a sub-cell's, so
/// both shares leave about 1.5 times that. Not held: a kernel with a feature under a millimetre
/// far steeper than its bound (0.9 for one 16 times too steep), which can drop out of a layer at
/// some alignment, and an open mesh, whose value jumps across a hole at any share. A missed cell
/// leaves a gap in the contour with no sign of it.
const COARSE_REACH: f64 = 0.71;
const SUB_REACH: f64 = 0.5;

/// The fine grid of a layer: node (i, j) at (oi + i, oj + j) * fine, `k` fine cells a coarse one.
#[derive(Clone, Copy)]
struct Grid {
    oi: i64,
    oj: i64,
    fine: f64,
    k: i64,
}

fn sample_many(many: Many2, pts: &[(f64, f64)], reach: f64) -> Vec<f64> {
    let mut out = vec![0.0; pts.len()];
    if !pts.is_empty() {
        many(pts, reach, &mut out);
    }
    out
}

/// The set bits of `m`, lowest first.
fn bits(mut m: u64) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        (m != 0).then(|| {
            let b = m.trailing_zeros() as usize;
            m &= m - 1;
            b
        })
    })
}

/// The segments of the kept coarse `cells` (steps 2 to 6).
fn trace_cells(f: Field2, many: Many2, grid: &Grid, cells: &[(i64, i64)], p: &SdfParams, map: Option<&SlopeMap>) -> Vec<(Pt, Pt)> {
    let Grid { oi, oj, fine, k } = *grid;
    let node = |i: i64, j: i64| ((oi + i) as f64 * fine, (oj + j) as f64 * fine);
    let sample = |pts: &[(f64, f64)], reach: f64| sample_many(many, pts, reach);
    // The nodes are read only as far as the reach the sub-cells are kept by: past it a node tells
    // its sign alone, but at the end of an edge whose sign changes (read again in full below).
    let past = fine * SUB as f64 * SUB_REACH * std::f64::consts::SQRT_2 * p.lipschitz;
    if cells.is_empty() {
        return Vec::new();
    }
    // Each kept cell keeps its own block of (k + 1)^2 node values, row by row, and its edges'
    // crossings, indexed by its position in `cells` (row-major: a cell's left and lower
    // neighbours come before it). A node or edge on a border belongs to the first kept cell
    // that holds it; the others read it from there. Per row of a block, the nodes inside and the
    // nodes to sample are bit masks (bit li of row lj), so a row's sign-changing edges and its
    // fine cells with both signs come out of a few bit operations.
    const NONE: u32 = u32::MAX;
    let ku = k as usize;
    let side = ku + 1;
    let per = side * side;
    // Every node of a row (`side` is at most 64: a coarse cell is at most 63 fine cells a side).
    let full = u64::MAX >> (64 - side);
    let (lo_i, lo_j) = cells.iter().fold((i64::MAX, i64::MAX), |m, c| (m.0.min(c.0), m.1.min(c.1)));
    let (hi_i, hi_j) = cells.iter().fold((i64::MIN, i64::MIN), |m, c| (m.0.max(c.0), m.1.max(c.1)));
    let width = (hi_i - lo_i + 1).max(0) as usize;
    let mut slots = vec![NONE; width * (hi_j - lo_j + 1).max(0) as usize];
    for (s, &(ci, cj)) in cells.iter().enumerate() {
        slots[(cj - lo_j) as usize * width + (ci - lo_i) as usize] = s as u32;
    }
    let slot = |ci: i64, cj: i64| -> u32 {
        if ci < lo_i || ci > hi_i || cj < lo_j || cj > hi_j {
            return NONE;
        }
        slots[(cj - lo_j) as usize * width + (ci - lo_i) as usize]
    };
    // Each cell's kept neighbours that come before it (NONE: not kept): below left, below,
    // below right, left.
    let nbs: Vec<[u32; 4]> = cells.iter().map(|&(ci, cj)| [slot(ci - 1, cj - 1), slot(ci, cj - 1), slot(ci + 1, cj - 1), slot(ci - 1, cj)]).collect();
    let at = |s: usize, li: usize, lj: usize| s * per + lj * side + li;
    // The nodes of row lj a cell holds first: on its lowest row the ones no kept cell below
    // holds (nor below left or left at the corner, nor below right at the far end), up its left
    // column the ones no left cell holds.
    let owned = |nb: &[u32; 4], lj: usize| -> u64 {
        let [dl, d, dr, l] = *nb;
        if lj > 0 {
            return if l == NONE { full } else { full & !1 };
        }
        if d != NONE {
            return 0;
        }
        let mut m = full;
        if dl != NONE || l != NONE {
            m &= !1;
        }
        if dr != NONE {
            m &= !(1 << ku);
        }
        m
    };
    // Where the value of a node a cell does not own lives: in the block of the first kept cell
    // holding it, which owns it.
    let home = |nb: &[u32; 4], li: usize, lj: usize| -> usize {
        let [dl, d, dr, l] = *nb;
        if lj > 0 {
            at(l as usize, ku, lj)
        } else if li == 0 && dl != NONE {
            at(dl as usize, ku, ku)
        } else if d != NONE {
            at(d as usize, li, ku)
        } else if li == 0 {
            at(l as usize, ku, 0)
        } else {
            at(dr as usize, 0, ku)
        }
    };
    // The border nodes of cell s it does not own, in rows and columns `ls`, read from their homes.
    let copy = |vals: &mut [f64], s: usize, ls: u64| {
        let nb = &nbs[s];
        for li in bits(full & !owned(nb, 0) & ls) {
            vals[at(s, li, 0)] = vals[home(nb, li, 0)];
        }
        if nb[3] != NONE {
            for lj in bits(ls & !1) {
                vals[at(s, 0, lj)] = vals[home(nb, 0, lj)];
            }
        }
    };
    // 2. The kept cells' fine nodes, each sampled once (neighbouring cells share their border
    // nodes, and so every edge crossing exactly: watertight). First the corners of each cell's
    // sub-cells (SUB fine cells a side; the last one may be narrower), in one batch. A sub-cell
    // whose corners hold one sign and are all beyond its reach has no surface in it, by the rule
    // the coarse cells are kept by one level down. Then the nodes of the other sub-cells, in one
    // batch. A node no kept sub-cell holds takes its sub-cell's corner value unsampled: only its
    // sign is read, as no edge at it changes sign.
    let bl: Vec<usize> = (0..ku).step_by(SUB as usize).chain([ku]).collect();
    let nsub = bl.len() - 1;
    // The sub-cell corner rows (and columns), and the others.
    let subs = bl.iter().fold(0u64, |m, &l| m | (1 << l));
    let nonsub = full & !subs;
    // A sub-cell column (or row) holding fine node l (on a sub-cell border, the one starting there).
    let sub_of: Vec<usize> = (0..=ku).map(|l| l / SUB as usize - usize::from(l == ku && ku % SUB as usize == 0)).collect();
    let mut vals = vec![0.0f64; cells.len() * per];
    let mut own: Vec<usize> = Vec::new();
    let mut node_pts: Vec<(f64, f64)> = Vec::new();
    for (s, &(ci, cj)) in cells.iter().enumerate() {
        for &lj in &bl {
            for li in bits(owned(&nbs[s], lj) & subs) {
                own.push(at(s, li, lj));
                node_pts.push(node(ci * k + li as i64, cj * k + lj as i64));
            }
        }
    }
    for (&i, v) in own.iter().zip(sample(&node_pts, past)) {
        vals[i] = v;
    }
    for s in 0..cells.len() {
        copy(&mut vals, s, subs);
    }
    // a sub-cell corner's circle: this radius, cleared at the slope of the squares it meets
    let circle = fine * SUB as f64 * SUB_REACH * std::f64::consts::SQRT_2;
    let mut slopes = vec![(p.lipschitz, p.window); (nsub + 1) * (nsub + 1)];
    // The nodes each cell needs sampled, a mask per row.
    let mut need = vec![0u64; cells.len() * side];
    for (s, &(ci, cj)) in cells.iter().enumerate() {
        if let Some(m) = map {
            for (b, &lj) in bl.iter().enumerate() {
                for (a, &li) in bl.iter().enumerate() {
                    let q = node(ci * k + li as i64, cj * k + lj as i64);
                    let (s, w) = m.most(q.0, q.1, circle);
                    slopes[b * (nsub + 1) + a] = (s, w.min(p.window));
                }
            }
        }
        let clear = |v: f64, a: usize, b: usize| {
            let (s, w) = slopes[b * (nsub + 1) + a];
            v.abs().min(w) >= circle * s
        };
        for b in 0..nsub {
            for a in 0..nsub {
                let (i0, i1, j0, j1) = (bl[a], bl[a + 1], bl[b], bl[b + 1]);
                let c = [vals[at(s, i0, j0)], vals[at(s, i1, j0)], vals[at(s, i0, j1)], vals[at(s, i1, j1)]];
                let sign_change = c.iter().any(|v| *v < 0.0) && c.iter().any(|v| *v >= 0.0);
                if sign_change || !(clear(c[0], a, b) && clear(c[1], a + 1, b) && clear(c[2], a, b + 1) && clear(c[3], a + 1, b + 1)) {
                    let m = ((1u64 << (i1 - i0 + 1)) - 1) << i0;
                    for r in &mut need[s * side + j0..=s * side + j1] {
                        *r |= m;
                    }
                }
            }
        }
    }
    // A border node is sampled by its home cell when any cell holding it needs it (the
    // corners of sub-cells are sampled already: only the others are left).
    for s in 0..cells.len() {
        let [_, d, _, l] = nbs[s];
        if d != NONE {
            need[d as usize * side + ku] |= need[s * side] & nonsub;
        }
        if l != NONE {
            for lj in bits(nonsub) {
                need[l as usize * side + lj] |= (need[s * side + lj] & 1) << ku;
            }
        }
    }
    own.clear();
    node_pts.clear();
    for (s, &(ci, cj)) in cells.iter().enumerate() {
        for lj in 0..side {
            let rest = owned(&nbs[s], lj) & if (subs >> lj) & 1 != 0 { nonsub } else { full };
            let needed = rest & need[s * side + lj];
            for li in bits(needed) {
                own.push(at(s, li, lj));
                node_pts.push(node(ci * k + li as i64, cj * k + lj as i64));
            }
            for li in bits(rest & !needed) {
                vals[at(s, li, lj)] = vals[at(s, bl[sub_of[li]], bl[sub_of[lj]])];
            }
        }
    }
    for (&i, v) in own.iter().zip(sample(&node_pts, past)) {
        vals[i] = v;
    }
    for s in 0..cells.len() {
        copy(&mut vals, s, nonsub);
    }
    let val = |s: usize, li: usize, lj: usize| vals[at(s, li, lj)];
    // The nodes inside (f < 0), a mask per row.
    let inside_rows: Vec<u64> = vals.chunks_exact(side).map(|row| row.iter().enumerate().fold(0u64, |m, (li, v)| m | (u64::from(*v < 0.0) << li))).collect();
    let row = |s: usize, lj: usize| inside_rows[s * side + lj];
    // The nodes of a row a horizontal edge (and a fine cell) starts at: all but the last.
    let lefts = full >> 1;
    // 3. Every sign-changing fine edge, from its lower node: horizontal
    // (i, j)-(i+1, j) as dir 0, vertical (i, j)-(i, j+1) as dir 1. Per cell, the index of
    // each edge's crossing (none: no sign change): horizontal at lj * k + li (lj in 0..=k),
    // vertical at lj * (k + 1) + li (lj in 0..k). A cell's lowest row of horizontal edges is
    // its lower neighbour's top row; its left column of vertical edges its left neighbour's right.
    let (hn, vn) = (ku * side, side * ku);
    let mut hx = vec![NONE; cells.len() * hn];
    let mut vx = vec![NONE; cells.len() * vn];
    // 4. Each crossing refined onto the zero set (bisection with a secant
    // guess, always bracketed), all edges in lockstep: one batch per round.
    struct Refine {
        a: (f64, f64),
        b: (f64, f64),
        len: f64,
        lo: f64,
        hi: f64,
        fa: f64,
        fhi: f64,
        at: Option<(f64, f64)>,
    }
    // The edge from node (i, j) along dir, with the values at its ends.
    let refine = |i: i64, j: i64, dir: u8, fa: f64, fhi: f64| {
        let (bi, bj) = if dir == 0 { (i + 1, j) } else { (i, j + 1) };
        let (a, b) = (node(i, j), node(bi, bj));
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        Refine { a, b, len, lo: 0.0, hi: 1.0, fa, fhi, at: None }
    };
    let mut work: Vec<Refine> = Vec::new();
    for (s, &(ci, cj)) in cells.iter().enumerate() {
        let [_, below, _, left] = nbs[s];
        for lj in 0..side {
            let r = row(s, lj);
            for li in bits((r ^ (r >> 1)) & lefts) {
                hx[s * hn + lj * ku + li] = if lj == 0 && below != NONE {
                    hx[below as usize * hn + ku * ku + li]
                } else {
                    work.push(refine(ci * k + li as i64, cj * k + lj as i64, 0, val(s, li, lj), val(s, li + 1, lj)));
                    (work.len() - 1) as u32
                };
            }
        }
        for lj in 0..ku {
            for li in bits(row(s, lj) ^ row(s, lj + 1)) {
                vx[s * vn + lj * side + li] = if li == 0 && left != NONE {
                    vx[left as usize * vn + lj * side + ku]
                } else {
                    work.push(refine(ci * k + li as i64, cj * k + lj as i64, 1, val(s, li, lj), val(s, li, lj + 1)));
                    (work.len() - 1) as u32
                };
            }
        }
    }
    // An edge's end read at the reach or past it (a field steeper than its bound there) is read
    // again in full: the crossing's guesses take the values themselves.
    let ends: Vec<(usize, bool)> = work.iter().enumerate().flat_map(|(e, r)| [(e, false, r.fa), (e, true, r.fhi)]).filter(|&(_, _, v)| !(v.abs() < past)).map(|(e, b, _)| (e, b)).collect();
    let at_ends: Vec<(f64, f64)> = ends.iter().map(|&(e, b)| if b { work[e].b } else { work[e].a }).collect();
    for (&(e, b), v) in ends.iter().zip(sample(&at_ends, f64::INFINITY)) {
        if b {
            work[e].fhi = v;
        } else {
            work[e].fa = v;
        }
    }
    let point = |r: &Refine, t: f64| (r.a.0 + (r.b.0 - r.a.0) * t, r.a.1 + (r.b.1 - r.a.1) * t);
    let mut active: Vec<usize> = (0..work.len()).collect();
    let mut ts: Vec<f64> = Vec::new();
    let mut pts: Vec<(f64, f64)> = Vec::new();
    while !active.is_empty() {
        active.retain(|&e| {
            let r = &mut work[e];
            if (r.hi - r.lo) * r.len > p.tolerance {
                return true;
            }
            // The last bracket's secant, for no read more. Two bodies sharing a face on a grid
            // line read a tent that only touches zero at the node: its crossings from both sides
            // must land on that node, where a midpoint would leave a gap between two outlines.
            r.at = Some(point(r, r.lo + (r.hi - r.lo) * (r.fa / (r.fa - r.fhi))));
            false
        });
        ts.clear();
        pts.clear();
        for &e in &active {
            let r = &work[e];
            let t = (r.lo + (r.hi - r.lo) * (r.fa / (r.fa - r.fhi))).clamp(r.lo + (r.hi - r.lo) * 0.05, r.hi - (r.hi - r.lo) * 0.05);
            ts.push(t);
            pts.push(point(r, t));
        }
        let fs = sample(&pts, f64::INFINITY);
        let mut next = Vec::with_capacity(active.len());
        for (n, &e) in active.iter().enumerate() {
            let (t, q, ft) = (ts[n], pts[n], fs[n]);
            let r = &mut work[e];
            if ft == 0.0 {
                r.at = Some(q);
                continue;
            }
            if (ft < 0.0) == (r.fa < 0.0) {
                r.lo = t;
                r.fa = ft;
            } else {
                r.hi = t;
                r.fhi = ft;
            }
            next.push(e);
        }
        active = next;
    }
    // 5. The crossings' gradients (central differences), in one batch.
    const H: f64 = GRAD_H;
    let crossings: Vec<(f64, f64)> = work.iter().map(|r| r.at.expect("refined")).collect();
    let around: Vec<(f64, f64)> = crossings.iter().flat_map(|q| [(q.0 + H, q.1), (q.0 - H, q.1), (q.0, q.1 + H), (q.0, q.1 - H)]).collect();
    let g = sample(&around, f64::INFINITY);
    let grads: Vec<(f64, f64)> = (0..crossings.len()).map(|e| {
        let gx = (g[4 * e] - g[4 * e + 1]) / (2.0 * H);
        let gy = (g[4 * e + 2] - g[4 * e + 3]) / (2.0 * H);
        let l = (gx * gx + gy * gy).sqrt().max(1e-12);
        (gx / l, gy / l)
    }).collect();
    let crossing = |e: u32| {
        assert!(e != NONE, "a sign-changing edge");
        (crossings[e as usize], grads[e as usize])
    };
    // 6. Each cell's segments from its crossings: the fine cells with both signs at their corners.
    let cos_sharp = p.sharp_deg.to_radians().cos();
    // Spans turning more than this get their middle on the surface too.
    let cos_curve = 4.0f64.to_radians().cos();
    let to_pt = |q: (f64, f64)| Pt::from_mm(q.0, q.1);
    // Each span in order: its ends (inside on the left) and its middle.
    let mut spans: Vec<((f64, f64), (f64, f64), Mid)> = Vec::new();
    let mut mids: Vec<(f64, f64)> = Vec::new();
    for (s, &(ci, cj)) in cells.iter().enumerate() {
        let (bi, bj) = (ci * k, cj * k);
        for lj in 0..ku {
            let (r0, r1) = (row(s, lj), row(s, lj + 1));
            let any = r0 | (r0 >> 1) | r1 | (r1 >> 1);
            let all = r0 & (r0 >> 1) & r1 & (r1 >> 1);
            for li in bits(any & !all & lefts) {
                let (fi, fj) = (bi + li as i64, bj + lj as i64);
                // The cell's corners and edges in order: bottom, right, top, left.
                let inside: [bool; 4] = [(r0 >> li) & 1 != 0, (r0 >> (li + 1)) & 1 != 0, (r1 >> (li + 1)) & 1 != 0, (r1 >> li) & 1 != 0];
                let (h0, v0) = (s * hn + lj * ku + li, s * vn + lj * side + li);
                let edge = [hx[h0], vx[v0 + 1], hx[h0 + ku], vx[v0]];
                // The crossing on each sign-changing edge, with its gradient.
                let (mut xs, mut gs, mut found) = ([(0.0, 0.0); 4], [(0.0, 0.0); 4], 0);
                for e in 0..4 {
                    if inside[e] == inside[(e + 1) % 4] {
                        continue;
                    }
                    (xs[found], gs[found]) = crossing(edge[e]);
                    found += 1;
                }
                // Pairs of crossings that bound the same inside corner run;
                // a saddle (4 crossings) decided by the centre.
                let pairs: &[(usize, usize)] = if found == 2 {
                    &[(0, 1)]
                } else if found == 4 {
                    let (cx, cy) = node(fi, fj);
                    let centre_in = f(cx + fine * 0.5, cy + fine * 0.5) < 0.0;
                    // Crossings are in edge order 0..3; corner 0 is inside or not.
                    if centre_in == inside[0] { &[(0, 1), (2, 3)] } else { &[(3, 0), (1, 2)] }
                } else {
                    continue;
                };
                for &(ia, ib) in pairs {
                    let (a, b, na, nb) = (xs[ia], xs[ib], gs[ia], gs[ib]);
                    // Inside on the left: travel along (-n.y, n.x) of the mean normal.
                    let n = (na.0 + nb.0, na.1 + nb.1);
                    let d = (b.0 - a.0, b.1 - a.1);
                    let (a, b, na, nb) = if d.0 * -n.1 + d.1 * n.0 >= 0.0 { (a, b, na, nb) } else { (b, a, nb, na) };
                    // A crease: the two tangent lines meet inside the cell,
                    // on the surface (a tight curve is no crease).
                    let mut corner: Option<(f64, f64)> = None;
                    let turn = na.0 * nb.0 + na.1 * nb.1;
                    if turn < cos_sharp {
                        let det = na.0 * nb.1 - na.1 * nb.0;
                        if det.abs() > 1e-9 {
                            let (ca, cb) = (na.0 * a.0 + na.1 * a.1, nb.0 * b.0 + nb.1 * b.1);
                            let q = ((ca * nb.1 - na.1 * cb) / det, (na.0 * cb - ca * nb.0) / det);
                            let (cx, cy) = node(fi, fj);
                            let m = fine * 0.5;
                            if q.0 >= cx - m && q.0 <= cx + fine + m && q.1 >= cy - m && q.1 <= cy + fine + m && f(q.0, q.1).abs() <= p.tolerance.max(1e-4) * p.lipschitz {
                                corner = Some(q);
                            }
                        }
                    }
                    // A curved span: its middle projected onto the surface (all spans' together,
                    // below), so the chord error stays far under the tolerance.
                    let mid = match corner {
                        Some(q) => Mid::At(q),
                        None if turn < cos_curve => {
                            mids.push(((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5));
                            Mid::Curve(mids.len() - 1, node(fi, fj))
                        }
                        None => Mid::None,
                    };
                    spans.push((a, b, mid));
                }
            }
        }
    }
    let projected = project_many(many, mids);
    let mut segs: Vec<(Pt, Pt)> = Vec::with_capacity(spans.len());
    for (a, b, mid) in spans {
        let corner = match mid {
            Mid::At(q) => Some(q),
            Mid::Curve(m, (cx, cy)) => {
                let q = projected[m];
                (q.0 >= cx - fine && q.0 <= cx + 2.0 * fine && q.1 >= cy - fine && q.1 <= cy + 2.0 * fine).then_some(q)
            }
            Mid::None => None,
        };
        let (pa, pb) = (to_pt(a), to_pt(b));
        match corner {
            Some(q) => {
                let pq = to_pt(q);
                if pa != pq {
                    segs.push((pa, pq));
                }
                if pq != pb {
                    segs.push((pq, pb));
                }
            }
            None if pa != pb => segs.push((pa, pb)),
            None => {}
        }
    }
    segs
}

/// A span's middle point: none, a crease corner, or (index into the projected middles, the
/// fine cell's lower corner) for a curved span.
enum Mid {
    None,
    At((f64, f64)),
    Curve(usize, (f64, f64)),
}

/// A distance field `f(x, y, z)` in printer mm, negative inside.
pub type Field3 = std::sync::Arc<dyn Fn(f64, f64, f64) -> f64 + Send + Sync>;

/// A field and its material reduced to what can matter inside the slab
/// `z0..z1` (and the source's footprint): the same sign, zero set and
/// values near the surface there.
pub struct Slab {
    pub field: Field3,
    /// The same field at many points in one call (a kernel field's batch);
    /// `None`: sampled point by point.
    pub many: Option<Field3Many>,
    pub material: Option<crate::sdf_paint::MaterialFn>,
}

/// [`Field3`] at many points: `out[k] = f(pts[k]).clamp(-reach, reach)`, bit for bit ([`Many2`]).
pub type Field3Many = std::sync::Arc<dyn Fn(&[[f64; 3]], f64, &mut [f64]) + Send + Sync>;

/// `(z0, z1)` → the source's [`Slab`] there.
pub type SlabFn = std::sync::Arc<dyn Fn(f64, f64) -> Slab + Send + Sync>;

/// A plane's `z` → the slope its tracer reads it with ([`PlaneSlope`]).
pub type SlopeFn = std::sync::Arc<dyn Fn(f64) -> PlaneSlope + Send + Sync>;

/// The slope a plane is traced with: `(lipschitz, window)` ([`SdfParams`]) and, where the field
/// knows its slope piece by piece, the map of them ([`SlopeMap`]).
#[derive(Clone, Debug)]
pub struct PlaneSlope {
    pub lipschitz: f64,
    pub window: f64,
    pub map: Option<std::sync::Arc<SlopeMap>>,
}

/// How steep a plane's field is square by square: the squares of a grid on the plane, `size`
/// wide, from `(x0, y0)`, `nx` by `ny` row by row, each with a slope `s` and a window `w` its
/// values clear circles in it at (as [`SdfParams`]' `lipschitz` and `window`: a value `v`
/// proves its circle of `min(|v|, w) / s` clear; a slope of 0 where its values all lie past the
/// window, infinite where none is known). A circle reads the most slope and the least window of
/// the squares it meets ([`Self::most`]); a square past the grid knows none.
pub struct SlopeMap {
    pub x0: f64,
    pub y0: f64,
    pub size: f64,
    pub nx: usize,
    pub ny: usize,
    pub of: Vec<(f64, f64)>,
}

impl PlaneSlope {
    /// Whether `other` traces a plane as this does: the same slopes, and maps of the same
    /// squares and slopes, bit for bit.
    pub fn same(&self, other: &PlaneSlope) -> bool {
        let maps = match (&self.map, &other.map) {
            (Some(a), Some(b)) => [a.x0, a.y0, a.size].map(f64::to_bits) == [b.x0, b.y0, b.size].map(f64::to_bits) && (a.nx, a.ny) == (b.nx, b.ny) && a.same_over_both(b),
            (a, b) => a.is_none() && b.is_none(),
        };
        maps && self.lipschitz.to_bits() == other.lipschitz.to_bits() && self.window.to_bits() == other.window.to_bits()
    }
}

impl SlopeMap {
    /// The most slope and the least window of the squares the square of half side `r` about
    /// `(x, y)` meets (a circle of radius `r` there meets no other): `(infinity, 0)` where it
    /// reaches past the grid.
    pub fn most(&self, x: f64, y: f64, r: f64) -> (f64, f64) {
        let index = |v: f64, o: f64| ((v - o) / self.size).floor();
        let (i0, i1, j0, j1) = (index(x - r, self.x0), index(x + r, self.x0), index(y - r, self.y0), index(y + r, self.y0));
        if !(i0 >= 0.0 && j0 >= 0.0 && i1 < self.nx as f64 && j1 < self.ny as f64) {
            return (f64::INFINITY, 0.0);
        }
        let (mut most, mut least) = (0.0f64, f64::INFINITY);
        for j in j0 as usize..=j1 as usize {
            for &(s, w) in &self.of[j * self.nx + i0 as usize..=j * self.nx + i1 as usize] {
                most = most.max(s);
                least = least.min(w);
            }
        }
        (most, least)
    }

    /// Whether `other` holds the same slopes, bit for bit, over the squares both hold (on the
    /// same grid).
    pub fn same_over_both(&self, other: &SlopeMap) -> bool {
        if self.size.to_bits() != other.size.to_bits() {
            return false;
        }
        // each grid's offset on the other's, in squares
        let (di, dj) = (((other.x0 - self.x0) / self.size).round(), ((other.y0 - self.y0) / self.size).round());
        if (self.x0 + di * self.size).to_bits() != other.x0.to_bits() || (self.y0 + dj * self.size).to_bits() != other.y0.to_bits() {
            return false;
        }
        let (di, dj) = (di as i64, dj as i64);
        let (i0, i1) = (di.max(0), (self.nx as i64).min(di + other.nx as i64));
        let (j0, j1) = (dj.max(0), (self.ny as i64).min(dj + other.ny as i64));
        let bits = |(s, w): (f64, f64)| (s.to_bits(), w.to_bits());
        (j0..j1).all(|j| (i0..i1).all(|i| bits(self.of[(j * self.nx as i64 + i) as usize]) == bits(other.of[((j - dj) * other.nx as i64 + i - di) as usize])))
    }
}

impl std::fmt::Debug for SlopeMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let most = self.of.iter().map(|q| q.0).filter(|s| s.is_finite()).fold(0.0f64, f64::max);
        let least = self.of.iter().map(|q| q.1).fold(f64::INFINITY, f64::min);
        let unknown = self.of.iter().filter(|q| q.0.is_infinite()).count();
        // a short hash of the slopes, to tell two maps apart
        let h = self.of.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, q| ((h ^ q.0.to_bits()).wrapping_mul(0x100_0000_01b3) ^ q.1.to_bits()).wrapping_mul(0x100_0000_01b3));
        write!(f, "SlopeMap {{ {} by {} of {} mm from ({}, {}), the most slope {most}, the least window {least}, {unknown} unknown, hash {h:016x} }}", self.nx, self.ny, self.size, self.x0, self.y0)
    }
}

/// A part given as a distance field `f(x, y, z)` in printer mm (negative
/// inside) and its bounds: a contour source that cuts no mesh.
pub struct SdfSource {
    pub field: std::sync::Arc<dyn Fn(f64, f64, f64) -> f64 + Send + Sync>,
    /// `field` in batches ([`Slab::many`]).
    pub many: Option<Field3Many>,
    pub bounds: ([f64; 3], [f64; 3]),
    /// The XY box the planner centres the part's patterns on ([`ContourSource::pivot_box`]):
    /// `bounds`' unless given ([`Self::with_pivot`]).
    ///
    /// [`ContourSource::pivot_box`]: crate::source::ContourSource::pivot_box
    pub pivot: ([f64; 2], [f64; 2]),
    pub params: SdfParams,
    /// The regions' cleanup: no closing (the field is closed), simplification
    /// well under the contour accuracy.
    pub slice: crate::slice::SliceParams,
    /// The state at a point (material slots mapped to filaments): a field
    /// with colour ([`crate::sdf_paint`]).
    pub material: Option<crate::sdf_paint::MaterialFn>,
    /// The field reduced per layer (a shape tree culled to the layer's
    /// slab); `None`: every layer reads `field` and `material` whole.
    pub slab: Option<SlabFn>,
    /// The slope each plane is traced with, where the field proves one there; `None`: `params`'
    /// at every plane.
    pub slopes: Option<SlopeFn>,
}

impl SdfSource {
    /// Shared cleanup for closed field contours, regardless of tracing backend.
    pub const SLICE_PARAMS: crate::slice::SliceParams = crate::slice::SliceParams { closing_radius: 0.0, resolution: 0.012, max_gap: 0.5 };
    /// How far below a layer's plane (mm) its regions are sampled: a flat face lying exactly
    /// on the plane belongs to the solid under it, as a mesh's cut ((min z, max z] of a
    /// triangle) has it, instead of a zero field across the face. The GPU tracer samples at the
    /// same height (`fab_sdf_engine::trace::BELOW_PLANE`).
    pub const BELOW_PLANE: f64 = 1e-4;
    pub fn new(field: std::sync::Arc<dyn Fn(f64, f64, f64) -> f64 + Send + Sync>, bounds: ([f64; 3], [f64; 3])) -> Self {
        let pivot = ([bounds.0[0], bounds.0[1]], [bounds.1[0], bounds.1[1]]);
        Self { field, many: None, bounds, pivot, params: SdfParams::default(), slice: Self::SLICE_PARAMS, material: None, slab: None, slopes: None }
    }

    /// This source tracing each plane with the slope `slopes` gives it.
    pub fn with_slopes(mut self, slopes: SlopeFn) -> Self {
        self.slopes = Some(slopes);
        self
    }

    /// The tracer's parameters at the plane `z`: `params` with the plane's slope.
    pub fn params_at(&self, z: f64) -> SdfParams {
        self.plane_at(z).0
    }

    /// [`Self::params_at`], and the plane's map of slopes where it has one ([`PlaneSlope`]).
    fn plane_at(&self, z: f64) -> (SdfParams, Option<std::sync::Arc<SlopeMap>>) {
        match &self.slopes {
            Some(f) => {
                let PlaneSlope { lipschitz, window, map } = f(z);
                (SdfParams { lipschitz, window, ..self.params }, map)
            }
            None => (self.params, None),
        }
    }

    /// This source centring the part's patterns on the XY box `pivot`.
    pub fn with_pivot(mut self, pivot: ([f64; 2], [f64; 2])) -> Self {
        self.pivot = pivot;
        self
    }

    /// This source sampling `field` in batches through `many`.
    pub fn with_many(mut self, many: Field3Many) -> Self {
        self.many = Some(many);
        self
    }

    /// This source reading each layer through `slab`.
    pub fn with_slab(mut self, slab: SlabFn) -> Self {
        self.slab = Some(slab);
        self
    }

    /// How far from a layer's plane the tracer's values must be exact: a
    /// coarse cell's diagonal times the bound (more than the coarse search
    /// reads), plus a fine cell and the gradient step for the points around a
    /// crossing.
    pub fn slab_margin(&self) -> f64 {
        Self::margin_of(&self.params)
    }

    /// [`Self::slab_margin`] for the plane `z`, at its own slope ([`Self::params_at`]).
    pub fn slab_margin_at(&self, z: f64) -> f64 {
        Self::margin_of(&self.params_at(z))
    }

    fn margin_of(p: &SdfParams) -> f64 {
        let k = (p.coarse / p.fine).round().max(1.0);
        p.fine * k * std::f64::consts::SQRT_2 * p.lipschitz + 2.0 * p.fine + 0.01
    }

    /// The field and material for the plane `z`.
    pub fn at(&self, z: f64) -> Slab {
        match &self.slab {
            Some(s) => {
                let m = self.slab_margin_at(z);
                s(z - m, z + m)
            }
            None => Slab { field: self.field.clone(), many: self.many.clone(), material: self.material.clone() },
        }
    }

    /// The segments of the slab's field at plane `z`.
    fn segments_of(&self, s: &Slab, z: f64) -> Vec<(Pt, Pt)> {
        let (params, map) = self.plane_at(z);
        let z = z - Self::BELOW_PLANE;
        let f = &s.field;
        let at = |x: f64, y: f64| f(x, y, z);
        let (lo, hi) = self.bounds;
        match &s.many {
            Some(m) => {
                let many = |pts: &[(f64, f64)], reach: f64, out: &mut [f64]| {
                    let p3: Vec<[f64; 3]> = pts.iter().map(|q| [q.0, q.1, z]).collect();
                    m(&p3, reach, out)
                };
                contour_segments_mapped(&at, &many, [lo[0], lo[1]], [hi[0], hi[1]], &params, map.as_deref())
            }
            None => {
                let many = |pts: &[(f64, f64)], reach: f64, out: &mut [f64]| {
                    for (o, q) in out.iter_mut().zip(pts) {
                        *o = at(q.0, q.1).clamp(-reach, reach);
                    }
                };
                contour_segments_mapped(&at, &many, [lo[0], lo[1]], [hi[0], hi[1]], &params, map.as_deref())
            }
        }
    }

    /// This source with colour: `m` gives the state at a point.
    pub fn with_material(mut self, m: crate::sdf_paint::MaterialFn) -> Self {
        self.material = Some(m);
        self
    }

    /// The segments of one layer (for measuring the kernel).
    pub fn segments(&self, z: f64) -> Vec<(Pt, Pt)> {
        self.segments_of(&self.at(z), z)
    }

    /// [`Self::segments`] with the layer's material.
    pub fn segments_with_material(&self, z: f64) -> (Vec<(Pt, Pt)>, Option<crate::sdf_paint::MaterialFn>) {
        let s = self.at(z);
        (self.segments_of(&s, z), s.material)
    }
}

impl crate::source::ContourSource for SdfSource {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.bounds
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.pivot
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<crate::geom::ExPolygon>> {
        zs.iter().map(|&z| crate::slice::segments_to_regions(self.segments(z), &self.slice)).collect()
    }
    /// From the field: |n_z| of the 3D gradient at every contour point of
    /// each band's middle plane; flat faces where the cross-section's area
    /// jumps between planes (found to 1 µm by bisection).
    fn slope_profile(&self, step: f64) -> Option<crate::source::SlopeProfile> {
        let (lo, hi) = self.bounds;
        let mut p = crate::source::SlopeProfile::empty(lo[2], hi[2], step);
        let n = p.nz.len();
        let nz_at = |f: &Field3, x: f64, y: f64, z: f64| -> f64 {
            const H: f64 = GRAD_H;
            let g = [
                f(x + H, y, z) - f(x - H, y, z),
                f(x, y + H, z) - f(x, y - H, z),
                f(x, y, z + H) - f(x, y, z - H),
            ];
            let l = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().max(1e-12);
            g[2] / l
        };
        let area_of = |segs: &[(Pt, Pt)]| -> (f64, f64) {
            let a: f64 = segs.iter().map(|(a, b)| 0.5 * (a.x_mm() * b.y_mm() - b.x_mm() * a.y_mm())).sum();
            let per: f64 = segs.iter().map(|(a, b)| (b.x_mm() - a.x_mm()).hypot(b.y_mm() - a.y_mm())).sum();
            (a.abs(), per)
        };
        let area = |z: f64| area_of(&self.segments(z));
        let mut prev: Option<(f64, f64, f64)> = None; // (z, area, perimeter)
        for k in 0..n {
            let z = (lo[2] + (k as f64 + 0.5) * step).min(hi[2] - 1e-6);
            let slab = self.at(z);
            let f = slab.field.clone();
            let segs = self.segments_of(&slab, z);
            let mut worst = 0.0f64;
            for (a, _) in &segs {
                let nz = nz_at(&f, a.x_mm(), a.y_mm(), z).abs();
                if nz < crate::source::SlopeProfile::FLAT {
                    worst = worst.max(nz);
                }
            }
            p.nz[k] = worst as f32;
            let (a, per) = area_of(&segs);
            if let Some((z0, a0, p0)) = prev {
                // A face flatter than ~14° changes the area by more than 4·P·dz.
                if (a - a0).abs() > 4.0 * per.max(p0) * (z - z0) + 1.0 {
                    let (mut za, mut zb, aa) = (z0, z, a0);
                    for _ in 0..14 {
                        let m = 0.5 * (za + zb);
                        if (area(m).0 - aa).abs() < 0.5 * (a - a0).abs() { za = m } else { zb = m }
                    }
                    p.flats.push(0.5 * (za + zb));
                }
            }
            prev = Some((z, a, per));
        }
        // The top and bottom of the part are flat where they are cut.
        p.finish();
        Some(p)
    }
    fn painted(&self) -> bool {
        self.material.is_some()
    }
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<crate::geom::ExPolygon>], p: &crate::paint::PaintParams) -> Vec<crate::paint::LayerPaint> {
        match &self.material {
            Some(m) => crate::sdf_paint::regions(self, m, zs, slices, p),
            None => slices.iter().map(|s| vec![(0, s.clone())]).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ContourSource;

    #[test]
    fn two_boxes_sharing_a_face_on_a_grid_line_are_one_outline() {
        let cube = |p: [f64; 3], c: [f64; 3]| (0..3).map(|k| (p[k] - c[k]).abs() - if k == 2 { 2.0 } else { 5.0 }).fold(f64::MIN, f64::max);
        let f = move |x: f64, y: f64, z: f64| cube([x, y, z], [5.0, 5.0, 2.0]).min(cube([x, y, z], [15.0, 5.0, 2.0]));
        let src = SdfSource::new(std::sync::Arc::new(f), ([0.0, 0.0, 0.0], [20.0, 10.0, 4.0]));
        let layers = src.contours(&[0.1, 2.0]);
        for layer in &layers {
            assert_eq!(layer.len(), 1, "the shared face leaves a gap between two outlines");
            assert_eq!(layer[0].contour.pts.len(), 4);
            assert!(layer[0].holes.is_empty());
        }
    }

    /// A 40 x 30 box minus a radius-8 hole: square corners and a true circle.
    fn box_minus_hole(x: f64, y: f64, _z: f64) -> f64 {
        let (dx, dy) = ((x - 50.0).abs() - 20.0, (y - 50.0).abs() - 15.0);
        let rect = dx.max(dy).min(0.0) + (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
        let hole = ((x - 50.0).powi(2) + (y - 50.0).powi(2)).sqrt() - 8.0;
        rect.max(-hole)
    }

    #[test]
    fn box_minus_hole_is_exact() {
        let src = SdfSource::new(std::sync::Arc::new(box_minus_hole), ([29.0, 34.0, 0.0], [71.0, 66.0, 10.0]));
        let regions = src.contours(&[5.0]).pop().unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].holes.len(), 1);
        let mut worst = 0.0f64;
        for r in regions[0].rings() {
            for q in &r.pts {
                worst = worst.max(box_minus_hole(q.x_mm(), q.y_mm(), 5.0).abs());
            }
        }
        assert!(worst < 0.005, "max contour error {worst} mm");
        // The four corners are vertices, within a micron.
        for c in [(30.0, 35.0), (70.0, 35.0), (70.0, 65.0), (30.0, 65.0)] {
            let d = regions[0].contour.pts.iter().map(|q| ((q.x_mm() - c.0).powi(2) + (q.y_mm() - c.1).powi(2)).sqrt()).fold(f64::MAX, f64::min);
            assert!(d < 0.001, "corner {c:?} off by {d} mm");
        }
        let area = regions[0].area_mm2();
        let exact = 40.0 * 30.0 - std::f64::consts::PI * 64.0;
        // The hole's chords, simplified to the contour's resolution, lie inside its circle and
        // leave a little more: at most two thirds of that resolution along its length.
        let chords = 2.0 / 3.0 * SdfSource::SLICE_PARAMS.resolution * std::f64::consts::TAU * 8.0;
        assert!(area > exact && area - exact < chords, "area {area} vs {exact}");
    }

    /// A spot steeper than the plane's slope, at a coarse cell's centre, holds a crossing its
    /// corners clear at that slope, which the tracer steps over. A map of slopes square by square
    /// that holds the spot's own finds it, and over a field within the map's slopes traces the
    /// segments the map's most slope traces alone, bit for bit, reading fewer values.
    #[test]
    fn a_map_of_slopes_finds_a_steep_spot_and_traces_as_its_most_slope() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (sx, sy) = (60.5, 47.5);
        let ball = |x: f64, y: f64| ((x - 52.0).powi(2) + (y - 47.0).powi(2)).sqrt() - 6.5;
        let spot = move |x: f64, y: f64| 10.0 * (((x - sx).powi(2) + (y - sy).powi(2)).sqrt() - 0.3);
        let f = move |x: f64, y: f64| ball(x, y).min(spot(x, y));
        let reads = AtomicUsize::new(0);
        let many = |pts: &[(f64, f64)], reach: f64, out: &mut [f64]| {
            reads.fetch_add(pts.len(), Ordering::Relaxed);
            for (o, q) in out.iter_mut().zip(pts) {
                *o = f(q.0, q.1).clamp(-reach, reach);
            }
        };
        let (lo, hi) = ([29.0, 34.0], [71.0, 66.0]);
        let trace = |lipschitz: f64, map: Option<&SlopeMap>| {
            reads.store(0, Ordering::Relaxed);
            let segs = contour_segments_mapped(&f, &many, lo, hi, &SdfParams { lipschitz, ..SdfParams::default() }, map);
            (segs, reads.load(Ordering::Relaxed))
        };
        let near_spot = |segs: &[(Pt, Pt)]| segs.iter().any(|(a, _)| (a.x_mm() - sx).hypot(a.y_mm() - sy) < 0.6);
        // the spot (slope 10 where it is the least) lies in the square 60..62 by 46..48 of a map
        // of 2 mm squares; the ball's distance (slope 1) everywhere else
        let (x0, y0, size, nx, ny) = (26.0, 32.0, 2.0, 24, 18);
        let of = (0..ny).flat_map(|j| (0..nx).map(move |i| (i, j))).map(|(i, j)| (if x0 + i as f64 * size == 60.0 && y0 + j as f64 * size == 46.0 { 10.0 } else { 1.0 }, f64::INFINITY)).collect();
        let map = SlopeMap { x0, y0, size, nx, ny, of };
        let (low, _) = trace(1.0, None);
        assert!(!low.is_empty() && !near_spot(&low), "the ball's slope steps over the spot");
        let (high, all) = trace(10.0, None);
        assert!(near_spot(&high), "the spot's slope finds it");
        let (mapped, fewer) = trace(10.0, Some(&map));
        assert!(mapped == high, "the map traces {} segments, its most slope {}", mapped.len(), high.len());
        assert!(fewer < all, "the map reads {fewer} values, its most slope {all}");
    }

    /// A tracer that reads its grids only as far as the reaches its cells are kept by (the
    /// coarse nodes, the sub-cells' corners and the fine nodes) traces the segments of one that
    /// reads every value in full, bit for bit: over creases and curves, and where a field is
    /// steeper than its bound (an edge's ends past the reach, read again in full for its
    /// crossing). The test sees values it clamped.
    #[test]
    fn reads_as_far_as_the_reaches_trace_the_segments_of_full_reads() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let ball = |x: f64, y: f64| ((x - 52.0).powi(2) + (y - 47.0).powi(2)).sqrt() - 6.5;
        let fields: [(&str, Box<dyn Fn(f64, f64) -> f64 + Sync>, f64); 4] = [
            ("box minus hole", Box::new(|x, y| box_minus_hole(x, y, 0.0)), 1.0),
            ("ball", Box::new(ball), 1.0),
            ("a ball three times as steep as its bound", Box::new(move |x, y| 3.0 * ball(x, y)), 1.0),
            ("waves", Box::new(|x: f64, y: f64| (x * 0.9).sin() + (y * 0.7).cos() - 0.3), 1.2),
        ];
        for (name, f, lipschitz) in &fields {
            let p = SdfParams { lipschitz: *lipschitz, ..SdfParams::default() };
            let clamped = AtomicUsize::new(0);
            let full = |pts: &[(f64, f64)], _: f64, out: &mut [f64]| {
                for (o, q) in out.iter_mut().zip(pts) {
                    *o = f(q.0, q.1);
                }
            };
            let reaching = |pts: &[(f64, f64)], reach: f64, out: &mut [f64]| {
                for (o, q) in out.iter_mut().zip(pts) {
                    let v = f(q.0, q.1);
                    if v.abs() > reach {
                        clamped.fetch_add(1, Ordering::Relaxed);
                    }
                    *o = v.clamp(-reach, reach);
                }
            };
            let (lo, hi) = ([29.0, 34.0], [71.0, 66.0]);
            let want = contour_segments_many(&|x, y| f(x, y), &full, lo, hi, &p);
            let got = contour_segments_many(&|x, y| f(x, y), &reaching, lo, hi, &p);
            assert!(!want.is_empty() && got == want, "{name}: {} segments read as far as the reaches, {} read in full", got.len(), want.len());
            assert!(clamped.load(Ordering::Relaxed) > 0, "{name}: no value was past its reach");
        }
    }
}
