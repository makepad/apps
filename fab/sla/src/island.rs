//! Where an island is held: the points an island's shape gets, after R7 §2.3, measured on a grid
//! of the island's cells instead of its outline's polygon.
//!
//! The grid's distances to the outline (an exact distance transform) give the island's widths
//! and its middle, and walks over its cells (shortest paths) its lengths, with no skeleton of the
//! outline and so none of its degenerate cases:
//! 1. a speck no larger than the tip gets one point at its middle;
//! 2. an island shorter (along its longest walk, end to end) than [`Pattern::one`] gets one, at
//!    the walk's middle;
//! 3. a short narrow one, two, near its ends where it is as wide as the tip;
//! 4. otherwise its thick part (what discs of [`Pattern::thick`] fill, where one of
//!    [`Pattern::thin`] fits) gets a row of points along its outline, inset by the tip, and a
//!    triangular grid inside; its thin part, points along its middle, from the longest walk
//!    out along every branch;
//! 5. the points then even out (a few rounds of moving each to the middle of the cells nearest
//!    it), each along its own track: the middle, the outline row, or the inside.

use crate::grid::{PixelGrid, PixelRect};
use crate::parts::Run;
use makepad_fab_slicer::par;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// The cells a mask holds at most: a larger island is measured on coarser cells.
pub const CELLS: usize = 1 << 18;

/// An island's numbers for a tip, after R7 §2.3: lengths and widths measured on test prints of
/// single islands, all scaled from the tip's area. Every length is in mm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pattern {
    /// The tip's radius.
    pub tip: f64,
    /// Shorter than this: one point.
    pub one: f64,
    /// Shorter than this (and narrower than `thin`): two points.
    pub two: f64,
    /// Wider than this somewhere: a thick part, reaching down to where it is `thick` wide.
    pub thin: f64,
    pub thick: f64,
    /// The points along a thin part's middle.
    pub step: f64,
    /// The points along a thick part's outline.
    pub outline: f64,
    /// The side of the grid inside a thick part.
    pub grid: f64,
    /// How far in from an end a point near it may sit.
    pub inset: f64,
    /// How far a point on the middle may move as the points even out.
    pub track: f64,
    /// The grid's own reach: how far the middle of one of its triangles is from its corners.
    pub reach: f64,
}

impl Pattern {
    /// For a tip of `tip_radius`, the points `density` times as dense along lines (floored at a
    /// tenth), `√density` across areas. The one- and two-point limits keep to the tip.
    pub fn new(tip_radius: f64, density: f64) -> Pattern {
        let rho = density.max(0.1);
        let area = std::f64::consts::PI * tip_radius * tip_radius;
        let one = 2.9 * area + 1.3;
        let two = 3.9 * one;
        let grid = two / rho.sqrt();
        Pattern {
            tip: tip_radius,
            one,
            two,
            thin: 2.5 * one,
            thick: 2.15 * one,
            step: 0.8 * two / rho,
            outline: 0.75 * two / rho,
            grid,
            inset: 0.8 * two / 3.0,
            track: two / 2.0,
            reach: grid / 3f64.sqrt(),
        }
    }
}

/// A point an island gets, on the bed (mm), and what it may move along.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub at: [f64; 2],
    pub track: Track,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Track {
    /// One of a one- or two-point island's: it stays.
    Fixed,
    /// On a thin part's middle.
    Middle,
    /// On a thick part's outline row.
    Outline,
    /// Inside, anywhere at least the tip in from the outline.
    Inside,
}

/// A shape on a grid of cells on the bed: cell `(i, j)`'s centre at `origin + (i, j) * cell`
/// (mm), row-major. The cells along the grid's sides are never on.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub cell: [f64; 2],
    pub origin: [f64; 2],
    pub on: Vec<bool>,
}

impl Mask {
    /// `runs` (pixels of `grid` within `rect`) as cells: each a square of pixels, as few to a
    /// side as keeps the mask within `cap` cells, on where at least half its pixels are; with a
    /// ring of cells around them.
    pub fn of_runs(runs: &[Run], rect: PixelRect, grid: &PixelGrid, cap: usize) -> Mask {
        let (pw, ph) = (rect.width() as usize, rect.height() as usize);
        let mut f = 1;
        while (pw.div_ceil(f) + 2) * (ph.div_ceil(f) + 2) > cap.max(9) {
            f += 1;
        }
        let (w, h) = (pw.div_ceil(f) + 2, ph.div_ceil(f) + 2);
        let mut count = vec![0u32; w * h];
        for r in runs {
            let j = (r.y - rect.y0) as usize / f + 1;
            let mut x = r.x0;
            while x < r.x1 {
                let i = (x - rect.x0) as usize / f;
                let end = (rect.x0 + ((i + 1) * f) as u32).min(r.x1);
                count[j * w + i + 1] += end - x;
                x = end;
            }
        }
        let half = (f * f).div_ceil(2) as u32;
        let cell = [grid.pitch[0] * f as f64, grid.pitch[1] * f as f64];
        // cell 1's pixels start at the rect's corner
        let origin = [(rect.x0 as f64 + (0.5 - 1.0) * f as f64) * grid.pitch[0], (rect.y0 as f64 + (0.5 - 1.0) * f as f64) * grid.pitch[1]];
        Mask { w, h, cell, origin, on: count.iter().map(|c| *c >= half).collect() }
    }

    pub fn center(&self, k: usize) -> [f64; 2] {
        [self.origin[0] + (k % self.w) as f64 * self.cell[0], self.origin[1] + (k / self.w) as f64 * self.cell[1]]
    }

    /// The cell holding bed point `p` (clamped to the grid).
    pub fn cell_at(&self, p: [f64; 2]) -> usize {
        let i = ((p[0] - self.origin[0]) / self.cell[0]).round().clamp(0.0, (self.w - 1) as f64) as usize;
        let j = ((p[1] - self.origin[1]) / self.cell[1]).round().clamp(0.0, (self.h - 1) as f64) as usize;
        j * self.w + i
    }

    fn small(&self) -> f64 {
        self.cell[0].min(self.cell[1])
    }

    /// The length of a diagonal step between cells.
    fn diagonal(&self) -> f64 {
        self.cell[0].hypot(self.cell[1])
    }

    /// The on cells' 8 neighbours (each with the step's length), a diagonal only where both cells
    /// beside it are on, so no walk slips between two cells that meet at a corner.
    /// (`d` is the diagonal step's length, [`Mask::diagonal`].)
    fn neighbours(&self, k: usize, d: f64, mut f: impl FnMut(usize, f64)) {
        let (w, cx, cy) = (self.w, self.cell[0], self.cell[1]);
        let (l, r, u, b) = (k - 1, k + 1, k - w, k + w);
        // a cell on is never on the grid's sides, so its neighbours are in the grid
        for (n, len) in [(l, cx), (r, cx), (u, cy), (b, cy)] {
            if self.on[n] {
                f(n, len);
            }
        }
        for (n, s1, s2) in [(u - 1, u, l), (u + 1, u, r), (b - 1, b, l), (b + 1, b, r)] {
            if self.on[n] && self.on[s1] && self.on[s2] {
                f(n, d);
            }
        }
    }
}

/// The squared distances (mm²) from each cell's centre to the nearest site's (`site[k]`), on a
/// grid of `w` by `h` cells of `cell` (Felzenszwalb and Huttenlocher, "Distance transforms of
/// sampled functions", 2012): rows of parabolas' lower envelopes, down the columns then along the
/// rows.
pub(crate) fn distance_squared(w: usize, h: usize, cell: [f64; 2], site: impl Fn(usize) -> bool) -> Vec<f64> {
    const FAR: f64 = 1e30;
    let mut d: Vec<f64> = (0..w * h).map(|k| if site(k) { 0.0 } else { FAR }).collect();
    let n = w.max(h);
    let (mut f, mut out, mut v, mut z) = (vec![0.0; n], vec![0.0; n], vec![0usize; n], vec![0.0; n + 1]);
    let mut pass = |d: &mut [f64], len: usize, count: usize, at: &dyn Fn(usize, usize) -> usize, w2: f64| {
        for line in 0..count {
            for q in 0..len {
                f[q] = d[at(line, q)];
            }
            envelope(&f[..len], w2, &mut out[..len], &mut v, &mut z);
            for q in 0..len {
                d[at(line, q)] = out[q];
            }
        }
    };
    pass(&mut d, h, w, &|i, j| j * w + i, cell[1] * cell[1]);
    pass(&mut d, w, h, &|j, i| j * w + i, cell[0] * cell[0]);
    d
}

/// `out[q] = min over p of w2 (q - p)² + f[p]`.
fn envelope(f: &[f64], w2: f64, out: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let meet = |q: usize, p: usize| ((f[q] + w2 * (q * q) as f64) - (f[p] + w2 * (p * p) as f64)) / (2.0 * w2 * (q - p) as f64);
    let mut k = 0;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..n {
        let mut s = meet(q, v[k]);
        while s <= z[k] {
            k -= 1;
            s = meet(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate() {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let d = q as f64 - v[k] as f64;
        *o = w2 * d * d + f[v[k]];
    }
}

/// Each on cell's distance (mm) in from the outline: to the nearest off cell's centre, less half
/// a cell (the outline runs between the two). Off cells: 0.
pub fn depth(m: &Mask) -> Vec<f64> {
    let half = 0.5 * m.small();
    distance_squared(m.w, m.h, m.cell, |k| !m.on[k]).iter().zip(&m.on).map(|(d, on)| if *on { (d.sqrt() - half).max(0.0) } else { 0.0 }).collect()
}

/// Shortest walks over the on cells of `within` from `from`, each step's length times `weight`
/// of the cell it enters: each cell's cost and the cell it was reached from.
/// Walks over a mask's cells (shortest paths, Dijkstra's), their costs and steps kept between
/// walks: a walk from more cells lowers what the walks before left, and clearing resets only the
/// cells walks reached, so walks within a small part of a big mask cost that part, not the mask.
struct Walks {
    cost: Vec<f64>,
    came: Vec<u32>,
    reached: Vec<usize>,
    heap: BinaryHeap<Reverse<(u64, u32)>>,
}

impl Walks {
    fn new(m: &Mask) -> Walks {
        Walks { cost: vec![f64::INFINITY; m.w * m.h], came: vec![u32::MAX; m.w * m.h], reached: Vec::new(), heap: BinaryHeap::new() }
    }

    fn clear(&mut self) {
        for k in self.reached.drain(..) {
            self.cost[k] = f64::INFINITY;
            self.came[k] = u32::MAX;
        }
    }

    fn lower(&mut self, k: usize, cost: f64, came: usize) {
        if self.cost[k] == f64::INFINITY {
            self.reached.push(k);
        }
        self.cost[k] = cost;
        self.came[k] = came as u32;
        self.heap.push(Reverse((cost.to_bits(), k as u32)));
    }

    /// Walks through `within` from `from` (each at cost 0), a step costing its length times the
    /// `weight` of the cell it steps onto; up to `to` when given (whose walk is then the
    /// shortest: every cell on it was reached before it).
    fn walk(&mut self, m: &Mask, within: &[bool], from: &[usize], to: Option<usize>, weight: impl Fn(usize) -> f64) {
        for &s in from {
            self.lower(s, 0.0, s);
        }
        let d = m.diagonal();
        while let Some(Reverse((c, k))) = self.heap.pop() {
            let (c, k) = (f64::from_bits(c), k as usize);
            if c > self.cost[k] {
                continue;
            }
            if Some(k) == to {
                break;
            }
            m.neighbours(k, d, |n, len| {
                if within[n] {
                    let next = c + len * weight(n);
                    if next < self.cost[n] {
                        self.lower(n, next, k);
                    }
                }
            });
        }
        self.heap.clear();
    }

    /// The walk from where the walks began to `to` (reached), from `to` back: each cell's step
    /// the one a walk from scratch takes (of the cells it is reached from at its cost, the one it
    /// reaches first: the least cost, then the first in the mask's order), so walks from more
    /// cells find the walks a single walk from all of them does.
    fn back(&self, m: &Mask, to: usize, weight: impl Fn(usize) -> f64) -> Vec<usize> {
        let mut path = vec![to];
        let mut n = to;
        let d = m.diagonal();
        while self.cost[n] > 0.0 {
            let (cost, w) = (self.cost[n], weight(n));
            let mut from: Option<(f64, usize)> = None;
            // neighbours are mutual, at the same length
            m.neighbours(n, d, |k, len| {
                let c = self.cost[k];
                if c.is_finite() && c + len * w == cost && from.is_none_or(|f| (c, k) < f) {
                    from = Some((c, k));
                }
            });
            n = from.expect("a reached cell is reached from a cell before it").1;
            path.push(n);
        }
        path
    }

    /// The cell farthest along the walks (the first in the mask's order of those as far), and
    /// how far; `from` when none is farther than 0.
    fn farthest(&self, from: usize) -> (usize, f64) {
        self.reached.iter().fold((from, 0.0), |b, &k| {
            let c = self.cost[k];
            if c > b.1 || (c == b.1 && c > 0.0 && k < b.0) {
                (k, c)
            } else {
                b
            }
        })
    }
}

/// The cell of `within` farthest from `from` along walks, and how far.
fn farthest(m: &Mask, walks: &mut Walks, within: &[bool], from: &[usize]) -> (usize, f64) {
    walks.clear();
    walks.walk(m, within, from, None, |_| 1.0);
    walks.farthest(from[0])
}

/// A walk through the middle of `within` from `from` to `to`: each step costs more the nearer
/// its cell is to the outline (`depth`; `deepest` is the deepest of `within`). Its cells from
/// `to` back to `from`, and their lengths along it from `to`.
fn middle_walk(m: &Mask, walks: &mut Walks, within: &[bool], depth: &[f64], deepest: f64, from: &[usize], to: usize) -> (Vec<usize>, Vec<f64>) {
    walks.clear();
    walks.walk(m, within, from, Some(to), middle_weight(m, depth, deepest));
    let came = &walks.came;
    let mut path = vec![to];
    let mut k = to;
    while came[k] as usize != k && came[k] != u32::MAX {
        k = came[k] as usize;
        path.push(k);
    }
    let s = lengths(m, &path);
    (path, s)
}

/// A middle walk's step onto a cell costs its length times this: more the nearer the cell is to
/// the outline (`depth`; `deepest` is the deepest the walk goes through).
fn middle_weight<'a>(m: &Mask, depth: &'a [f64], deepest: f64) -> impl Fn(usize) -> f64 + 'a {
    let c = m.small();
    move |k| ((deepest + c) / (depth[k] + c)).powi(2)
}

/// The lengths along `path` from its start.
fn lengths(m: &Mask, path: &[usize]) -> Vec<f64> {
    let mut s = vec![0.0];
    for p in path.windows(2) {
        let (a, b) = (m.center(p[0]), m.center(p[1]));
        s.push(s.last().unwrap() + (a[0] - b[0]).hypot(a[1] - b[1]));
    }
    s
}

/// The first of `cells` nearest `to`, as `min_by` of the distances finds it: a cell no nearer
/// along either axis than the nearest so far is passed over unmeasured.
fn nearest_to(cells: &[[f64; 2]], to: [f64; 2]) -> Option<[f64; 2]> {
    let mut best: Option<([f64; 2], f64)> = None;
    for &c in cells {
        if let Some((_, d)) = best {
            if (c[0] - to[0]).abs() >= d || (c[1] - to[1]).abs() >= d {
                continue;
            }
        }
        let d = dist(c, to);
        if best.is_none_or(|b| d < b.1) {
            best = Some((c, d));
        }
    }
    best.map(|b| b.0)
}

/// The cell of `path` at length `at` along it.
fn along(path: &[usize], s: &[f64], at: f64) -> usize {
    path[s.partition_point(|v| *v < at).min(path.len() - 1)]
}

/// How far in from the start of `path` its tip fits: the first cell as wide as the tip, but no
/// farther than `most`.
fn inset_from_start(path: &[usize], s: &[f64], depth: &[f64], tip: f64, most: f64) -> f64 {
    let k = path.iter().position(|c| depth[*c] >= tip).unwrap_or(path.len() - 1);
    s[k].min(most)
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// The points an island (`m`'s on cells) gets.
pub fn island_points(m: &Mask, p: &Pattern) -> Vec<Placed> {
    let cells: Vec<usize> = (0..m.on.len()).filter(|k| m.on[*k]).collect();
    let Some(&first) = cells.first() else { return Vec::new() };
    // 1. a speck: its box no larger than the tip
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for &k in &cells {
        let c = m.center(k);
        for a in 0..2 {
            lo[a] = lo[a].min(c[a] - 0.5 * m.cell[a]);
            hi[a] = hi[a].max(c[a] + 0.5 * m.cell[a]);
        }
    }
    if hi[0] - lo[0] <= p.tip && hi[1] - lo[1] <= p.tip {
        return vec![Placed { at: [0.5 * (lo[0] + hi[0]), 0.5 * (lo[1] + hi[1])], track: Track::Fixed }];
    }
    let depth = depth(m);
    let (mut walks, mut spread, mut mid) = (Walks::new(m), Walks::new(m), Walks::new(m));
    // its longest walk: from the cell farthest from any one, to the cell farthest from that
    let (a, _) = farthest(m, &mut walks, &m.on, &[first]);
    let (b, _) = farthest(m, &mut walks, &m.on, &[a]);
    let deepest = cells.iter().map(|k| depth[*k]).fold(0.0, f64::max);
    let (path, s) = middle_walk(m, &mut walks, &m.on, &depth, deepest, &[a], b);
    let length = *s.last().unwrap();
    let widest = 2.0 * path.iter().map(|k| depth[*k]).fold(0.0, f64::max);
    // 2. short: one point at the middle
    if length < p.one {
        return vec![Placed { at: m.center(along(&path, &s, 0.5 * length)), track: Track::Fixed }];
    }
    // 3. short and narrow: two, near the ends
    let two = || {
        let most = p.inset.min(0.25 * length);
        let start = inset_from_start(&path, &s, &depth, p.tip, most);
        let rev: Vec<usize> = path.iter().rev().copied().collect();
        let rs: Vec<f64> = s.iter().rev().map(|v| length - v).collect();
        let end = inset_from_start(&rev, &rs, &depth, p.tip, most);
        // the walk leaves its ends from the outline: each point to the deepest cell near it
        let middle = |k: usize| {
            let c = m.center(k);
            m.center(window(m, c, 0.5 * widest).filter(|n| m.on[*n]).fold(k, |b, n| if depth[n] > depth[b] { n } else { b }))
        };
        vec![Placed { at: middle(along(&path, &s, start)), track: Track::Fixed }, Placed { at: middle(along(&path, &s, length - end)), track: Track::Fixed }]
    };
    if widest < p.thin && length < p.two {
        return two();
    }
    // 4. the thick part and the thin part
    let thick = thick_part(m, &depth, p);
    let thin: Vec<bool> = (0..m.on.len()).map(|k| m.on[k] && !thick[k]).collect();
    let mut points: Vec<Placed> = Vec::new();
    let mut tracks: Vec<Vec<usize>> = Vec::new();
    let mut track_of: Vec<usize> = Vec::new();
    for component in components(m, &thin) {
        // the whole island thin: its longest walk is the island's, from the same first cell
        let known = (component.len() == cells.len()).then(|| (path.clone(), s.clone()));
        middle_points(m, (&mut walks, &mut spread, &mut mid), &component, known, &depth, p, &mut points, &mut tracks);
        track_of.resize(points.len(), tracks.len() - 1);
    }
    // the thick part, the tip in from the outline: a row along the outline, a grid inside
    let inside: Vec<bool> = (0..m.on.len()).map(|k| thick[k] && depth[k] >= p.tip).collect();
    let row = outline_row(m, &inside, &depth, p);
    let row_cells: Vec<usize> = (0..m.on.len()).filter(|k| row.1[*k]).collect();
    tracks.push(row_cells);
    let row_track = tracks.len() - 1;
    for at in &row.0 {
        points.push(Placed { at: *at, track: Track::Outline });
        track_of.push(row_track);
    }
    for at in inner_grid(m, &inside, p) {
        if row.0.iter().all(|r| dist(*r, at) >= 0.5 * p.grid) {
            points.push(Placed { at, track: Track::Inside });
            track_of.push(usize::MAX);
        }
    }
    let deep: Vec<bool> = (0..m.on.len()).map(|k| m.on[k] && depth[k] >= p.tip).collect();
    // too few for its shape: two near its ends
    if points.len() <= 2 {
        return two();
    }
    // 5. even out
    relax(m, &mut points, &track_of, &tracks, &deep, p);
    points
}

/// The cells of `within` in parts joined across sides, each part's cells in order.
fn components(m: &Mask, within: &[bool]) -> Vec<Vec<usize>> {
    let mut seen = vec![false; within.len()];
    let mut out = Vec::new();
    for k in 0..within.len() {
        if !within[k] || seen[k] {
            continue;
        }
        let mut part = vec![k];
        seen[k] = true;
        let mut at = 0;
        while at < part.len() {
            let c = part[at];
            at += 1;
            for n in [c - 1, c + 1, c - m.w, c + m.w] {
                if within[n] && !seen[n] {
                    seen[n] = true;
                    part.push(n);
                }
            }
        }
        part.sort_unstable();
        out.push(part);
    }
    out
}

/// What of the island is thick: where discs `thick` wide fill it, in the parts of that with
/// room for a disc `thin` wide somewhere; thin pieces too short for a point along their middle
/// (narrower than a step either way) that touch it are taken in.
fn thick_part(m: &Mask, depth: &[f64], p: &Pattern) -> Vec<bool> {
    let n = m.on.len();
    let r0 = 0.5 * p.thick;
    // the island opened by r0: within r0 of where it is r0 deep (whose outline runs half a cell
    // beyond the centres of the cells that deep)
    let near = distance_squared(m.w, m.h, m.cell, |k| depth[k] >= r0);
    let reach = r0 + 0.5 * m.small();
    let opened: Vec<bool> = (0..n).map(|k| m.on[k] && near[k] <= reach * reach).collect();
    let mut thick = vec![false; n];
    for part in components(m, &opened) {
        if part.iter().any(|k| depth[*k] >= 0.5 * p.thin) {
            for k in part {
                thick[k] = true;
            }
        }
    }
    let thin: Vec<bool> = (0..n).map(|k| m.on[k] && !thick[k]).collect();
    for part in components(m, &thin) {
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        let mut touches = false;
        for &k in &part {
            let c = m.center(k);
            for a in 0..2 {
                lo[a] = lo[a].min(c[a]);
                hi[a] = hi[a].max(c[a]);
            }
            touches |= [k - 1, k + 1, k - m.w, k + m.w].iter().any(|n| thick[*n]);
        }
        if touches && (hi[0] - lo[0]).max(hi[1] - lo[1]) < p.step {
            for k in part {
                thick[k] = true;
            }
        }
    }
    thick
}

/// Points along the middle of a thin part (`cells`): along its longest walk at the step, from its
/// own middle out (`known` when it is the island's own, as the whole island), a point at each end
/// that has none within half a step; then along each branch
/// still more than half a step from the walks so far. The points pushed onto `points`, the walks'
/// cells (their track) onto `tracks`.
fn middle_points(m: &Mask, (walks, spread, mid): (&mut Walks, &mut Walks, &mut Walks), cells: &[usize], known: Option<(Vec<usize>, Vec<f64>)>, depth: &[f64], p: &Pattern, points: &mut Vec<Placed>, tracks: &mut Vec<Vec<usize>>) {
    let start = points.len();
    let mut within = vec![false; m.on.len()];
    for &k in cells {
        within[k] = true;
    }
    let deepest = cells.iter().map(|k| depth[*k]).fold(0.0, f64::max);
    let (path, s) = known.unwrap_or_else(|| {
        let (a, _) = farthest(m, walks, &within, &[cells[0]]);
        let (b, _) = farthest(m, walks, &within, &[a]);
        middle_walk(m, walks, &within, depth, deepest, &[a], b)
    });
    let length = *s.last().unwrap();
    let mut track = path.clone();
    let rev: Vec<usize> = path.iter().rev().copied().collect();
    let rs: Vec<f64> = s.iter().rev().map(|v| length - v).collect();
    let (s0, s1) = (inset_from_start(&path, &s, depth, p.tip, p.inset), length - inset_from_start(&rev, &rs, depth, p.tip, p.inset));
    let put = |at: f64, points: &mut Vec<Placed>, path: &[usize], s: &[f64]| points.push(Placed { at: m.center(along(path, s, at)), track: Track::Middle });
    if s1 > s0 {
        let mid = 0.5 * (s0 + s1);
        let mut k = 0;
        loop {
            let off = (k as f64 + 0.5) * p.step;
            if mid + off > s1 {
                break;
            }
            put(mid - off, points, &path, &s);
            put(mid + off, points, &path, &s);
            k += 1;
        }
        // the ends: a point where the nearest is more than half a step away
        let reach = if k == 0 { f64::INFINITY } else { 0.5 * (s1 - s0) - (k as f64 - 0.5) * p.step };
        if reach > 0.5 * p.step {
            put(s0, points, &path, &s);
            put(s1, points, &path, &s);
        }
    } else {
        put(0.5 * length, points, &path, &s);
    }
    // the branches: from the cell farthest from the walks, a walk back to them, its points from
    // the tip in, each at least half a step from every other (the walks from the walks so far
    // spread on from each branch)
    spread.clear();
    spread.walk(m, &within, &track, None, |_| 1.0);
    let weight = middle_weight(m, depth, deepest);
    mid.clear();
    mid.walk(m, &within, &track, None, &weight);
    for _ in 0..64 {
        let (far, how) = spread.farthest(track[0]);
        if how <= 0.5 * p.step {
            break;
        }
        let branch = mid.back(m, far, &weight);
        let bs = lengths(m, &branch);
        let blen = *bs.last().unwrap();
        let mut at = inset_from_start(&branch, &bs, depth, p.tip, p.inset);
        while at < blen {
            let c = m.center(along(&branch, &bs, at));
            if points[start..].iter().all(|q| dist(q.at, c) >= 0.5 * p.step) {
                points.push(Placed { at: c, track: Track::Middle });
            }
            at += p.step;
        }
        spread.walk(m, &within, &branch, None, |_| 1.0);
        mid.walk(m, &within, &branch, None, &weight);
        track.extend(branch);
    }
    track.sort_unstable();
    track.dedup();
    tracks.push(track);
}

/// The row of points along the outline of `inside` (the thick part, the tip in from the
/// outline): its cells next to the island's own outline (not those where the thick part meets
/// the thin), walked in chains and a point every outline step along each. The points, and which
/// cells are the row.
fn outline_row(m: &Mask, inside: &[bool], depth: &[f64], p: &Pattern) -> (Vec<[f64; 2]>, Vec<bool>) {
    let n = m.on.len();
    let row: Vec<bool> = (0..n).map(|k| inside[k] && [k - 1, k + 1, k - m.w, k + m.w].iter().any(|c| depth[*c] < p.tip)).collect();
    let mut seen = vec![false; n];
    let mut out = Vec::new();
    for start in 0..n {
        if !row[start] || seen[start] {
            continue;
        }
        // a chain: on to an unseen neighbour, those across a side first
        let mut chain = vec![start];
        seen[start] = true;
        let mut k = start;
        loop {
            let (w, mut next) = (m.w, None);
            for c in [k - 1, k + 1, k - w, k + w, k - w - 1, k - w + 1, k + w - 1, k + w + 1] {
                if row[c] && !seen[c] {
                    next = Some(c);
                    break;
                }
            }
            let Some(c) = next else { break };
            seen[c] = true;
            chain.push(c);
            k = c;
        }
        let closed = chain.len() > 2 && dist(m.center(chain[0]), m.center(*chain.last().unwrap())) <= 1.5 * m.cell[0].hypot(m.cell[1]);
        let mut since = f64::INFINITY;
        let mut placed = Vec::new();
        for (i, c) in chain.iter().enumerate() {
            if i > 0 {
                since += dist(m.center(chain[i - 1]), m.center(*c));
            }
            if since >= p.outline {
                placed.push(m.center(*c));
                since = 0.0;
            }
        }
        // a closed chain's last point too near its first
        if closed && placed.len() > 1 && dist(placed[0], *placed.last().unwrap()) < 0.5 * p.outline {
            placed.pop();
        }
        out.extend(placed);
    }
    (out, row)
}

/// A triangular grid of the pattern's side over `inside`, turned to the region's own frame (its
/// middle toward its farthest cell), so a turned copy of the island gets a turned copy of the grid.
fn inner_grid(m: &Mask, inside: &[bool], p: &Pattern) -> Vec<[f64; 2]> {
    let cells: Vec<usize> = (0..inside.len()).filter(|k| inside[*k]).collect();
    if cells.is_empty() {
        return Vec::new();
    }
    let mut mid = [0.0; 2];
    for &k in &cells {
        let c = m.center(k);
        mid = [mid[0] + c[0], mid[1] + c[1]];
    }
    mid = [mid[0] / cells.len() as f64, mid[1] / cells.len() as f64];
    let far = cells.iter().map(|k| m.center(*k)).fold((mid, 0.0), |b, c| if dist(c, mid) > b.1 { (c, dist(c, mid)) } else { b });
    let (ux, uy) = if far.1 > 1e-9 { ((far.0[0] - mid[0]) / far.1, (far.0[1] - mid[1]) / far.1) } else { (1.0, 0.0) };
    let g = p.grid;
    let rows = (far.1 / (g * 0.5 * 3f64.sqrt())).ceil() as i64 + 1;
    let cols = (far.1 / g).ceil() as i64 + 1;
    let mut out = Vec::new();
    for j in -rows..=rows {
        for i in -cols..=cols {
            let (a, b) = ((i as f64 + 0.5 * (j.rem_euclid(2)) as f64) * g, j as f64 * g * 0.5 * 3f64.sqrt());
            let at = [mid[0] + a * ux - b * uy, mid[1] + a * uy + b * ux];
            let k = m.cell_at(at);
            if inside[k] && dist(m.center(k), at) <= 0.75 * m.cell[0].hypot(m.cell[1]) {
                out.push(at);
            }
        }
    }
    out
}

/// The cells of `m` within `r` of `c`, in a box around it.
fn window(m: &Mask, c: [f64; 2], r: f64) -> impl Iterator<Item = usize> + '_ {
    let at = |v: f64, o: f64, cell: f64, n: usize| (((v - o) / cell).floor().max(0.0) as usize).min(n - 1);
    let (i0, i1) = (at(c[0] - r, m.origin[0], m.cell[0], m.w), at(c[0] + r, m.origin[0], m.cell[0], m.w) + 1);
    let (j0, j1) = (at(c[1] - r, m.origin[1], m.cell[1], m.h), at(c[1] + r, m.origin[1], m.cell[1], m.h) + 1);
    (j0..=j1.min(m.h - 1)).flat_map(move |j| (i0..=i1.min(m.w - 1)).map(move |i| j * m.w + i)).filter(move |k| dist(m.center(*k), c) <= r)
}

/// Points for a shelf: the cells of `region` of a part's mask `m` (its own outline, so its
/// depth is from that), sampled as a thick part of an island is: a row along the outline the
/// tip in from it and a grid inside.
pub fn shelf_points(m: &Mask, region: &[bool], p: &Pattern) -> Vec<[f64; 2]> {
    let depth = depth(m);
    let inside: Vec<bool> = (0..m.on.len()).map(|k| region[k] && depth[k] >= p.tip).collect();
    let (row, _) = outline_row(m, &inside, &depth, p);
    let mut points = row.clone();
    points.extend(inner_grid(m, &inside, p).into_iter().filter(|at| row.iter().all(|r| dist(*r, *at) >= 0.5 * p.grid)));
    points
}

/// The cells [`relax`] gives a thread at a time.
const RELAX_CHUNK: usize = 4096;

/// A few rounds of moving each point to the middle of the cells nearest it (those within the
/// grid's side), then back onto its track: a point on the middle to the nearest of its walk's
/// cells within the track's reach of where it began, one of the outline row to the nearest of the
/// row's, one inside to the nearest cell at least the tip deep. Until no point moves a tenth of a
/// millimetre. Then of two points within the tip's width, the later goes.
fn relax(m: &Mask, points: &mut Vec<Placed>, track_of: &[usize], tracks: &[Vec<usize>], deep: &[bool], p: &Pattern) {
    let n = m.on.len();
    let bound = p.grid;
    let began: Vec<[f64; 2]> = points.iter().map(|q| q.at).collect();
    let deep_cells: Vec<usize> = (0..n).filter(|k| deep[*k]).collect();
    // where each point on a track may go, in its track's order: an outline row's cells, a
    // middle walk's within the track's reach of where the point began
    let centres: Vec<Vec<[f64; 2]>> = tracks.iter().map(|t| t.iter().map(|k| m.center(*k)).collect()).collect();
    let onto_of: Vec<Vec<[f64; 2]>> = points
        .iter()
        .enumerate()
        .map(|(i, q)| match q.track {
            Track::Middle => centres[track_of[i]].iter().copied().filter(|c| dist(*c, began[i]) <= p.track).collect(),
            _ => Vec::new(),
        })
        .collect();
    // the points in buckets of the bound's size, so each cell reads the nine around it
    let (bx, by) = ((bound / m.cell[0]).ceil().max(1.0) as usize, (bound / m.cell[1]).ceil().max(1.0) as usize);
    let (nbx, nby) = (m.w.div_ceil(bx), m.h.div_ceil(by));
    // the on cells: their centres and buckets
    let on_cells: Vec<([f64; 2], usize, usize)> = (0..n).filter(|k| m.on[*k]).map(|k| (m.center(k), (k % m.w) / bx, k / m.w / by)).collect();
    let deep_centres: Vec<[f64; 2]> = deep_cells.iter().map(|k| m.center(*k)).collect();
    for _ in 0..30 {
        let mut buckets = vec![Vec::new(); nbx * nby];
        for (i, q) in points.iter().enumerate() {
            let k = m.cell_at(q.at);
            buckets[(k / m.w / by) * nbx + (k % m.w) / bx].push(i);
        }
        // each cell's nearest point within the bound, the cells in chunks in parallel; then the
        // sums in the cells' order
        let nearest_of: Vec<Vec<usize>> = par::map(on_cells.len().div_ceil(RELAX_CHUNK), |b| {
            on_cells[b * RELAX_CHUNK..((b + 1) * RELAX_CHUNK).min(on_cells.len())]
                .iter()
                .map(|&(c, ci, cj)| {
                    let mut best = (usize::MAX, bound);
                    for j in cj.saturating_sub(1)..(cj + 2).min(nby) {
                        for i in ci.saturating_sub(1)..(ci + 2).min(nbx) {
                            for &q in &buckets[j * nbx + i] {
                                // no nearer than along either axis
                                let a = points[q].at;
                                if (a[0] - c[0]).abs() >= best.1 || (a[1] - c[1]).abs() >= best.1 {
                                    continue;
                                }
                                let d = dist(a, c);
                                if d < best.1 {
                                    best = (q, d);
                                }
                            }
                        }
                    }
                    best.0
                })
                .collect()
        });
        let mut sum = vec![[0.0f64; 3]; points.len()];
        for (&(c, _, _), &q) in on_cells.iter().zip(nearest_of.iter().flatten()) {
            if q != usize::MAX {
                let s = &mut sum[q];
                *s = [s[0] + c[0], s[1] + c[1], s[2] + 1.0];
            }
        }
        let mut moved: f64 = 0.0;
        for (i, q) in points.iter_mut().enumerate() {
            if q.track == Track::Fixed || sum[i][2] == 0.0 {
                continue;
            }
            let to = [sum[i][0] / sum[i][2], sum[i][1] / sum[i][2]];
            let onto = match q.track {
                Track::Middle => nearest_to(&onto_of[i], to),
                Track::Outline => nearest_to(&centres[track_of[i]], to),
                // inside: where it went, or the nearest cell deep enough, if that is near where
                // it began (a point in a part too narrow for the tip stays)
                _ => {
                    let k = m.cell_at(to);
                    if deep[k] {
                        Some(to)
                    } else {
                        nearest_to(&deep_centres, to).filter(|c| dist(*c, began[i]) <= p.track)
                    }
                }
            };
            if let Some(at) = onto {
                moved = moved.max(dist(at, q.at));
                q.at = at;
            }
        }
        if moved <= 0.1 {
            break;
        }
    }
    let mut kept: Vec<Placed> = Vec::with_capacity(points.len());
    for q in points.drain(..) {
        if kept.iter().all(|k| dist(k.at, q.at) >= 2.0 * p.tip) {
            kept.push(q);
        }
    }
    *points = kept;
}
