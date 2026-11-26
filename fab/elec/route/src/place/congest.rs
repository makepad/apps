//! Routing demand and capacity at a proposed placement, in nm of track per cell, over all
//! copper layers. A positive-weight net spreads its original weighted half-perimeter over its
//! pin box (at least one pitch per axis), and charges one cell edge for every pin escape, spread
//! over the half cell around the pin. Overflow uses the calibrated fraction of capacity; peak uses
//! the unscaled capacity.
//!
//! Capacity on each layer is the area inside the outline left free by all obstacles, divided
//! by pitch. Clearance-grown pad and hole boxes and track-keepout boxes approximate their shapes.
//! Courtyards and existing routes do not consume placement capacity. Outline arcs use chords within
//! 1 µm and polygon clipping rounds intersections to the board's integer nm grid.
//!
//! For a fixed board outline and copper stack a build costs O(pins + sum of cells in net
//! boxes + pads/keepouts + cells × layers), over the quarter-cell raster `congestion` reads its
//! lattices from (see there for the rest). A sweep visits each obstacle face once and queries
//! each grid cell twice. Range operations descend at most 32 levels (nm coordinates). At an
//! irregular outline only boundary cells subdivide at obstacle coordinates: each straight
//! outline edge crosses each distinct x or y coordinate at most once. Boundary work also
//! scales with the outline's own complexity. Memory is linear in these inputs and the nonzero
//! net/cell contributions retained for hot-cell attribution. Nothing runs per frame.

use super::{PinRef, PlaceProblem, Pose};
use fab_elec_board::{drc::reaches_copper, resolve_rules, Board, RuleItem, RuleKey};
use fab_elec_core::{poly::ring_area2, BBox, ClassId, LayerId, NetId, Nm, PadIx, PolySet, Shape, P};
use fab_elec_parts::Forbid;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

/// The fraction of a cell's capacity a router fills: the highest clipped hand-route length over
/// positive capacity on the fully routed handheld (204 tracks) and ledavr (71) boards, over every
/// cell of the `PHASES` x `PHASES` lattices `congestion` reads at `default_cell`. Handheld's cell
/// centred at (24.0, 23.4) mm, on the lattice shifted by (1.2, 1.8) mm, holds 19,716,113 nm of
/// track at 34,558,666 nm capacity, 0.571 (ledavr's highest is 0.456). Ledctl is partly routed
/// (8 tracks) and drum16-placed has none; both are left out. It holds at that cell size only:
/// smaller cells reach higher fractions (0.766 at half the size on one lattice), so another size
/// needs its own measurement.
pub const ETA: (i64, i64) = (19_716_113, 34_558_666);

/// Lattices per axis that `congestion` averages, each shifted from the last by `cell / PHASES`.
pub const PHASES: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub struct CongestionMap {
    /// The cell demand and capacity are measured over (`ETA` holds at `default_cell`). The map's
    /// own cells are the pieces its edges cut: quarter cells on `congestion`'s map.
    pub cell: Nm,
    pub w: usize,
    pub h: usize,
    /// Column and row edges, `w + 1` and `h + 1` of them.
    xs: Vec<Nm>,
    ys: Vec<Nm>,
    pub demand: Vec<i64>,
    pub capacity: Vec<i64>,
    overflow: Vec<i64>,
    over: i64,
    /// Wire demand by net, cell after cell: cell `i`'s is `wire[wire_at[i]..wire_at[i + 1]]`.
    wire: Vec<(NetId, i64)>,
    wire_at: Vec<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct HotCell {
    pub at: P,
    pub over: i64,
    /// All contributing nets, greatest wire demand first, then lowest net ID.
    pub nets: Vec<(NetId, i64)>,
}

fn pitch_and_clearance(board: &Board) -> (Nm, Nm) {
    let rules = resolve_rules(board, None);
    let key = RuleKey { class: ClassId(0), item: RuleItem::Track };
    let clearance = rules.clearance(key, key, LayerId::F_CU);
    (rules.track_min(key.class) + clearance, clearance)
}

/// The track pitch: the default class's track width plus its clearance.
pub fn pitch(board: &Board) -> Nm {
    pitch_and_clearance(board).0
}

/// The map's cell, the size `ETA` was measured at: eight track pitches, at least 2 mm. At half
/// that, the clearance-grown boxes of fine-pitch pads merge, and cells that hold routed track show
/// no capacity (two on the hand-routed handheld, five on ledavr); at this size none do.
pub fn default_cell(board: &Board) -> Nm {
    pitch_and_clearance(board).0.saturating_mul(8).max(2_000_000)
}

impl CongestionMap {
    pub fn overflow(&self, i: usize) -> i64 {
        self.overflow[i]
    }

    /// The total overflow: on a phase-free map, the mean of its lattices' totals.
    pub fn over(&self) -> i64 {
        self.over
    }

    /// Exact demand/capacity at the peak, or 0/1 if no cell has positive capacity.
    pub fn peak(&self) -> (i64, i64) {
        self.demand.iter().copied().zip(self.capacity.iter().copied()).filter(|&(_, c)| c > 0).fold(
            (0, 1),
            |best, (d, c)| {
                if i128::from(d) * i128::from(best.1) > i128::from(best.0) * i128::from(c) {
                    (d, c)
                } else {
                    best
                }
            },
        )
    }

    pub fn center(&self, i: usize) -> P {
        self.cell_box(i).center()
    }

    /// The k greatest overflows (or all cells on a smaller grid), ties by row, then column.
    pub fn hot(&self, k: usize) -> Vec<HotCell> {
        let mut worst: Vec<_> = (0..self.demand.len()).map(|i| (Reverse(self.overflow(i)), i)).collect();
        if k < worst.len() {
            worst.select_nth_unstable(k);
            worst.truncate(k);
        }
        worst.sort_unstable();
        worst
            .into_iter()
            .map(|(over, i)| {
                let mut nets = self.wire_in(i).to_vec();
                nets.sort_unstable_by_key(|&(net, amount)| (Reverse(amount), net));
                HotCell { at: self.center(i), over: over.0, nets }
            })
            .collect()
    }

    /// Row-major cell `i`'s box.
    pub fn cell_box(&self, i: usize) -> BBox {
        let (c, r) = (i % self.w, i / self.w);
        BBox::new(P::new(self.xs[c], self.ys[r]), P::new(self.xs[c + 1], self.ys[r + 1]))
    }

    /// The row-major cell holding `p`; a point beyond the grid is in its edge cell.
    pub fn cell_at(&self, p: P) -> usize {
        let (c, r) = (index(&self.xs, p.x), index(&self.ys, p.y));
        r * self.w + c
    }

    /// The cells box `b` touches, row-major; a box beyond the grid touches its edge cells.
    pub fn cells_under(&self, b: BBox) -> impl Iterator<Item = usize> + '_ {
        let ((c0, c1), (r0, r1)) = if b.is_empty() || self.w * self.h == 0 {
            ((1, 0), (1, 0))
        } else {
            ((index(&self.xs, b.min.x), index(&self.xs, b.max.x)), (index(&self.ys, b.min.y), index(&self.ys, b.max.y)))
        };
        (r0..=r1).flat_map(move |r| (c0..=c1).map(move |c| r * self.w + c))
    }

    /// Wire demand by net in row-major cell i, excluding pin escapes.
    pub fn wire_in(&self, i: usize) -> &[(NetId, i64)] {
        &self.wire[self.wire_at[i]..self.wire_at[i + 1]]
    }
}

/// The column (or row) of `edges` holding `v`, clamped to the first and last.
fn index(edges: &[Nm], v: Nm) -> usize {
    edges.get(1..edges.len().saturating_sub(1)).unwrap_or(&[]).partition_point(|&e| e <= v)
}

fn obstacles(board: &Board, problem: &PlaceProblem, poses: &[Pose], clearance: Nm) -> Vec<Vec<BBox>> {
    let layers = board.stackup.copper();
    let mut boxes = vec![Vec::new(); layers.len()];
    let mut parts = board.placed.clone();
    let moved: BTreeMap<_, _> = problem.movables.iter().zip(poses).map(|(m, p)| (m.item, p)).collect();
    for part in &mut parts {
        if let Some(pose) = moved.get(&part.id) {
            (part.at, part.rot, part.side) = (pose.at, pose.rot, pose.side);
        }
    }
    for (&layer, out) in layers.iter().zip(&mut boxes) {
        let count = layers.len() as u8;
        for p in &parts {
            for i in 0..p.footprint.pads.len() {
                let pad = PadIx(i as u16);
                if let Some(shape) = p.pad_shape(pad, layer, count) {
                    out.push(shape.bbox().inflate(clearance));
                }
                if let Some(hole) = p.drill_hole(pad) {
                    out.push(hole.shape().bbox().inflate(clearance));
                }
            }
            for k in &p.footprint.keepouts {
                if k.forbid.has(Forbid::TRACKS) && k.layers.has(p.fp_layer(layer, count)) {
                    out.push(p.shape_to_board(&Shape::Poly(k.area.clone())).bbox());
                }
            }
        }
        for k in &board.keepouts {
            if k.forbid.has(Forbid::TRACKS) && reaches_copper(k.z) && k.layers.has(layer) {
                out.push(k.area.bbox());
            }
        }
        for h in &board.holes {
            out.push(BBox::point(h.at).inflate(h.drill.max(h.pad.unwrap_or(0)) / 2).inflate(clearance));
        }
    }
    boxes
}

// Sweep in x. A node's clock advances only while every ancestor is uncovered. Its children
// therefore need no visit when a large rectangle temporarily covers the whole interval.
#[derive(Clone, Copy, Default)]
struct Span {
    covers: i64,
    free: i64,
    last: i64,
    clock: i64,
    area: i128,
}

struct Sweep<'a> {
    ys: &'a [Nm],
    tree: Vec<Span>,
}

impl Sweep<'_> {
    fn init(&mut self, n: usize, a: usize, b: usize) {
        self.tree[n].free = i64::from(self.ys[b]) - i64::from(self.ys[a]);
        if b - a > 1 {
            let m = (a + b) / 2;
            self.init(2 * n, a, m);
            self.init(2 * n + 1, m, b);
        }
    }

    // A zero delta queries the cumulative free area without changing the mask.
    fn visit(&mut self, n: usize, a: usize, b: usize, lo: usize, hi: usize, now: i64, delta: i64) -> i128 {
        let span = &mut self.tree[n];
        let dt = now - span.last;
        span.area += i128::from(dt) * i128::from(span.free);
        if span.covers == 0 {
            span.clock += dt;
        }
        span.last = now;
        let mut area = 0;
        if lo <= a && b <= hi {
            if delta == 0 {
                return span.area;
            }
            span.covers += delta;
        } else {
            let clock = span.clock;
            let m = (a + b) / 2;
            if lo < m {
                area += self.visit(2 * n, a, m, lo, hi, clock, delta);
            }
            if m < hi {
                area += self.visit(2 * n + 1, m, b, lo, hi, clock, delta);
            }
        }
        if delta != 0 {
            self.tree[n].free = if self.tree[n].covers > 0 {
                0
            } else if b - a == 1 {
                i64::from(self.ys[b]) - i64::from(self.ys[a])
            } else {
                self.tree[2 * n].free + self.tree[2 * n + 1].free
            };
        }
        area
    }
}

#[derive(Clone, Copy)]
struct Tile {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

struct AreaQuery {
    tile: Tile,
    cell: usize,
    // A partial tile has no obstacle edge through its interior. Its free area is all or zero.
    partial_area2: Option<i128>,
    free: i128,
}

fn area2(p: &PolySet) -> i128 {
    p.rings().map(ring_area2).sum()
}

fn outline_queries(xs: &[Nm], ys: &[Nm], tile: Tile, cell: usize, outline: &PolySet, out: &mut Vec<AreaQuery>) {
    let Tile { x0, x1, y0, y1 } = tile;
    let bb = BBox::new(P::new(xs[x0], ys[y0]), P::new(xs[x1], ys[y1]));
    let clipped = outline.intersection(&PolySet::rect(bb));
    let area = area2(&clipped);
    if area == 0 {
        return;
    }
    let full = area == 2 * i128::from(bb.width()) * i128::from(bb.height());
    if full || (x1 - x0 == 1 && y1 - y0 == 1) {
        out.push(AreaQuery { tile, cell, partial_area2: (!full).then_some(area), free: 0 });
        return;
    }
    let (left, right) = if x1 - x0 >= y1 - y0 {
        let mid = (x0 + x1) / 2;
        (Tile { x1: mid, ..tile }, Tile { x0: mid, ..tile })
    } else {
        let mid = (y0 + y1) / 2;
        (Tile { y1: mid, ..tile }, Tile { y0: mid, ..tile })
    };
    outline_queries(xs, ys, left, cell, &clipped, out);
    outline_queries(xs, ys, right, cell, &clipped, out);
}

fn cuts(bounds: &[Nm], faces: impl Iterator<Item = Nm>) -> Vec<Nm> {
    let axis: BTreeSet<_> = bounds.iter().copied().chain(faces).collect();
    axis.into_iter().collect()
}

/// Twice the free area of each cell of the grid `gx` x `gy` on one layer, added to `area2`.
fn layer_free_area2(gx: &[Nm], gy: &[Nm], boxes: Vec<BBox>, outline: &PolySet, rect: bool, area2: &mut [i128]) {
    let mut boxes = boxes;
    boxes.retain_mut(|b| {
        b.min.x = b.min.x.max(gx[0]);
        b.max.x = b.max.x.min(gx[gx.len() - 1]);
        b.min.y = b.min.y.max(gy[0]);
        b.max.y = b.max.y.min(gy[gy.len() - 1]);
        b.min.x < b.max.x && b.min.y < b.max.y
    });
    let xs = cuts(gx, boxes.iter().flat_map(|b| [b.min.x, b.max.x]));
    let ys = cuts(gy, boxes.iter().flat_map(|b| [b.min.y, b.max.y]));
    let xi = |x| xs.binary_search(&x).unwrap();
    let yi = |y| ys.binary_search(&y).unwrap();
    let mut faces = vec![Vec::new(); xs.len()];
    for b in boxes {
        let range = (yi(b.min.y), yi(b.max.y));
        faces[xi(b.min.x)].push((range, 1));
        faces[xi(b.max.x)].push((range, -1));
    }
    let mut queries = Vec::new();
    let xx: Vec<_> = gx.iter().copied().map(xi).collect();
    let yy: Vec<_> = gy.iter().copied().map(yi).collect();
    for (row, y) in yy.windows(2).enumerate() {
        for (col, x) in xx.windows(2).enumerate() {
            let cell = row * (gx.len() - 1) + col;
            let tile = Tile { x0: x[0], x1: x[1], y0: y[0], y1: y[1] };
            if rect {
                queries.push(AreaQuery { tile, cell, partial_area2: None, free: 0 });
            } else {
                outline_queries(&xs, &ys, tile, cell, outline, &mut queries);
            }
        }
    }
    let mut ends = vec![Vec::new(); xs.len()];
    for (i, q) in queries.iter().enumerate() {
        ends[q.tile.x0].push((i, -1));
        ends[q.tile.x1].push((i, 1));
    }
    let last = ys.len() - 1;
    let mut sweep = Sweep { ys: &ys, tree: vec![Span::default(); 4 * ys.len()] };
    sweep.init(1, 0, last);
    for (i, &x) in xs.iter().enumerate() {
        let now = i64::from(x) - i64::from(xs[0]);
        for &((lo, hi), delta) in &faces[i] {
            sweep.visit(1, 0, last, lo, hi, now, delta);
        }
        for &(id, sign) in &ends[i] {
            let q = &mut queries[id];
            q.free += sign * sweep.visit(1, 0, last, q.tile.y0, q.tile.y1, now, 0);
        }
    }
    for q in queries {
        if q.free > 0 {
            area2[q.cell] += q.partial_area2.unwrap_or(2 * q.free);
        }
    }
}

/// The edges of a lattice of `cell` through `at`, from `lo` to `hi`: the first and last cells
/// are cut at the ends.
fn grid(at: Nm, lo: Nm, hi: Nm, cell: Nm) -> Vec<Nm> {
    if hi <= lo {
        return vec![lo];
    }
    let (cell, hi) = (i64::from(cell), i64::from(hi));
    let mut edges = vec![lo];
    let mut k = i64::from(lo) - (i64::from(lo) - i64::from(at)).rem_euclid(cell) + cell;
    while k < hi {
        edges.push(k as Nm);
        k += cell;
    }
    edges.push(hi as Nm);
    edges
}

/// The map of `poses`, free of where its grid falls. `PHASES` x `PHASES` lattices of square cells
/// of `cell` run through the outline box's lower left, each shifted from the last by
/// `cell / PHASES` in x, in y or both. Each of the quarter cells their edges cut holds the mean of
/// the cells covering it (overflow included, each lattice's from its own cells), and `over` is the
/// mean of the lattices' totals. On one lattice a dense part reads two to four times as crowded
/// when it straddles a corner as when it sits in a cell's middle. Overflow is calibrated for
/// `default_cell` (`ETA`), which `PHASES` must divide.
///
/// Demand and free area are integrals over area, so every lattice's cell is a window over one
/// raster of quarter cells: the raster is built once, and each window is read from summed-area
/// tables. That costs one build at the quarter cell plus O(quarter cells x `PHASES`^2) for the
/// windows and, for each net, O(its window's quarter cells x `PHASES`) for its wire. Free area
/// sums exactly where the outline's edges cross the raster's lines at whole nm (a rectangle
/// does); elsewhere clipping rounds each crossing to the nm grid, so a cell's free area and its
/// quarters' sum can differ by that rounding.
pub fn congestion(board: &Board, problem: &PlaceProblem, poses: &[Pose], cell: Nm) -> CongestionMap {
    congestion_through(board, problem, poses, cell, board.outline_bbox().min)
}

/// `congestion` with its lattices through `at` instead of the box's lower left.
pub(super) fn congestion_through(board: &Board, problem: &PlaceProblem, poses: &[Pose], cell: Nm, at: P) -> CongestionMap {
    let n = PHASES;
    assert!(cell > 0 && cell % n as Nm == 0, "the cell {cell} must split into {n} raster steps");
    let bb = board.outline_bbox();
    let step = cell / n as Nm;
    let raster = rasterise(board, problem, poses, cell, grid(at.x, bb.min.x, bb.max.x, step), grid(at.y, bb.min.y, bb.max.y, step));
    let Raster { xs, ys, w, h, demand, area2, wire: nets } = raster;
    let mean = |sum: i128| (sum + (n * n / 2) as i128).div_euclid((n * n) as i128) as i64;
    let mut map = CongestionMap {
        cell,
        w,
        h,
        xs,
        ys,
        demand: Vec::with_capacity(w * h),
        capacity: Vec::with_capacity(w * h),
        overflow: Vec::with_capacity(w * h),
        over: 0,
        wire: Vec::new(),
        wire_at: vec![0; w * h + 1],
    };
    if w * h == 0 {
        return map;
    }

    // Window (a, b) is the lattice cell over raster columns a + 1 - n .. a + 1 and rows
    // b + 1 - n .. b + 1, cut to the raster: every lattice's cells, each once.
    let (wn, hn) = (w + n - 1, h + n - 1);
    let span = |a: usize, len: usize| ((a + 1).saturating_sub(n), (a + 1).min(len));
    let pitch2 = 2 * i128::from(pitch(board));
    let demand = summed(w, h, |i| i128::from(demand[i]));
    let free: Vec<Vec<i128>> = area2.iter().map(|a| summed(w, h, |i| a[i])).collect();
    let (mut win_demand, mut win_capacity, mut win_overflow) = (vec![0i128; wn * hn], vec![0i128; wn * hn], vec![0i128; wn * hn]);
    for b in 0..hn {
        let rows = span(b, h);
        for a in 0..wn {
            let cols = span(a, w);
            let d = window(&demand, w, cols, rows);
            let c: i128 = free.iter().map(|f| window(f, w, cols, rows) / pitch2).sum();
            let allowed = c * i128::from(ETA.0) / i128::from(ETA.1);
            let k = b * wn + a;
            (win_demand[k], win_capacity[k], win_overflow[k]) = (d, c, (d - allowed).max(0));
        }
    }
    map.over = mean(win_overflow.iter().sum());
    // Raster cell (x, y) lies in windows x .. x + n by y .. y + n.
    let (win_demand, win_capacity, win_overflow) =
        (summed(wn, hn, |k| win_demand[k]), summed(wn, hn, |k| win_capacity[k]), summed(wn, hn, |k| win_overflow[k]));
    for y in 0..h {
        for x in 0..w {
            let (cols, rows) = ((x, x + n), (y, y + n));
            map.demand.push(mean(window(&win_demand, wn, cols, rows)));
            map.capacity.push(mean(window(&win_capacity, wn, cols, rows)));
            map.overflow.push(mean(window(&win_overflow, wn, cols, rows)));
        }
    }

    // A net's wire in the windows over cell j, summed: each raster cell k counts once per window
    // holding both, (n - |jx - kx|) (n - |jy - ky|) times.
    let tent = |d: usize| n - d;
    let mut spread = Vec::with_capacity(nets.len());
    for net in nets {
        let (c0, c1) = (net.cols.0.saturating_sub(n - 1), (net.cols.1 + n - 1).min(w));
        let (r0, r1) = (net.rows.0.saturating_sub(n - 1), (net.rows.1 + n - 1).min(h));
        let (nw, ow) = (net.cols.1 - net.cols.0, c1 - c0);
        let mut across = vec![0i128; (net.rows.1 - net.rows.0) * ow];
        for (row, values) in net.nm.chunks(nw).enumerate() {
            for (k, &v) in values.iter().enumerate().filter(|e| *e.1 != 0) {
                let kx = net.cols.0 + k;
                for jx in kx.saturating_sub(n - 1).max(c0)..(kx + n).min(c1) {
                    across[row * ow + jx - c0] += i128::from(v) * tent(jx.abs_diff(kx)) as i128;
                }
            }
        }
        let mut both = vec![0i128; (r1 - r0) * ow];
        for (row, values) in across.chunks(ow).enumerate() {
            let ky = net.rows.0 + row;
            for jy in ky.saturating_sub(n - 1).max(r0)..(ky + n).min(r1) {
                let t = tent(jy.abs_diff(ky)) as i128;
                for (o, &v) in both[(jy - r0) * ow..(jy - r0 + 1) * ow].iter_mut().zip(values) {
                    *o += v * t;
                }
            }
        }
        let nm: Vec<i64> = both.into_iter().map(mean).collect();
        for (row, values) in nm.chunks(ow).enumerate() {
            for (k, &v) in values.iter().enumerate() {
                map.wire_at[(r0 + row) * w + c0 + k + 1] += usize::from(v > 0);
            }
        }
        spread.push((net.net, (c0, ow), r0, nm));
    }
    for i in 0..w * h {
        map.wire_at[i + 1] += map.wire_at[i];
    }
    let mut next = map.wire_at.clone();
    map.wire = vec![(NetId(0), 0); map.wire_at[w * h]];
    for (net, (c0, ow), r0, nm) in spread {
        for (row, values) in nm.chunks(ow).enumerate() {
            for (k, &v) in values.iter().enumerate().filter(|e| *e.1 > 0) {
                let i = (r0 + row) * w + c0 + k;
                map.wire[next[i]] = (net, v);
                next[i] += 1;
            }
        }
    }
    map
}

/// The summed-area table of `value` over a `w` x `h` grid: entry (x, y) of `w + 1` columns holds
/// the sum over the cells left of x and below y.
fn summed(w: usize, h: usize, value: impl Fn(usize) -> i128) -> Vec<i128> {
    let mut table = vec![0i128; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0;
        for x in 0..w {
            row += value(y * w + x);
            table[(y + 1) * (w + 1) + x + 1] = table[y * (w + 1) + x + 1] + row;
        }
    }
    table
}

/// The sum over columns `cols` and rows `rows` (half open) of a summed-area table `w` wide.
fn window(table: &[i128], w: usize, cols: (usize, usize), rows: (usize, usize)) -> i128 {
    let at = |x: usize, y: usize| table[y * (w + 1) + x];
    at(cols.1, rows.1) - at(cols.0, rows.1) - at(cols.1, rows.0) + at(cols.0, rows.0)
}

/// The map on one lattice of square cells of `cell` through `at`, cut to the outline box: what
/// `congestion` averages, built directly.
#[cfg(test)]
pub(super) fn lattice(board: &Board, problem: &PlaceProblem, poses: &[Pose], cell: Nm, at: P) -> CongestionMap {
    let bb = board.outline_bbox();
    let raster = rasterise(board, problem, poses, cell, grid(at.x, bb.min.x, bb.max.x, cell), grid(at.y, bb.min.y, bb.max.y, cell));
    let Raster { xs, ys, w, h, demand, area2, wire: nets } = raster;
    let pitch2 = 2 * i128::from(pitch(board));
    let capacity: Vec<i64> = (0..w * h).map(|i| area2.iter().map(|a| (a[i] / pitch2) as i64).sum()).collect();
    let overflow: Vec<i64> = demand.iter().zip(&capacity).map(|(&d, &c)| {
        let allowed = i128::from(c) * i128::from(ETA.0) / i128::from(ETA.1);
        (i128::from(d) - allowed).max(0) as i64
    }).collect();
    let mut lists = vec![Vec::new(); w * h];
    for net in &nets {
        let nw = net.cols.1 - net.cols.0;
        for (k, &v) in net.nm.iter().enumerate().filter(|e| *e.1 > 0) {
            lists[(net.rows.0 + k / nw) * w + net.cols.0 + k % nw].push((net.net, v));
        }
    }
    let mut wire_at = vec![0];
    for list in &lists {
        wire_at.push(wire_at[wire_at.len() - 1] + list.len());
    }
    CongestionMap { cell, w, h, xs, ys, over: overflow.iter().sum(), demand, capacity, overflow, wire: lists.concat(), wire_at }
}

/// Pin and wire demand and free area per cell of the grid `xs` x `ys`, for a map of `cell`.
struct Raster {
    xs: Vec<Nm>,
    ys: Vec<Nm>,
    w: usize,
    h: usize,
    demand: Vec<i64>,
    /// Twice the free area of each cell, by copper layer.
    area2: Vec<Vec<i128>>,
    /// Each positive-weight net's wire, in the order of the problem's nets.
    wire: Vec<NetWire>,
}

/// A net's wire over columns `cols` and rows `rows` (half open), row-major.
struct NetWire {
    net: NetId,
    cols: (usize, usize),
    rows: (usize, usize),
    nm: Vec<i64>,
}

/// A positive-weight net spreads its original weighted half-perimeter evenly over its pin box (at
/// least one pitch per axis). Each pin escape charges one cell edge, spread evenly over the square
/// half a cell wide centred on the pin: charged to the one cell holding the pin, a row of
/// fine-pitch pins jumps cells whenever a grid line crosses it. Both are cut to the outline box,
/// and the whole amount is kept inside it.
fn rasterise(board: &Board, problem: &PlaceProblem, poses: &[Pose], cell: Nm, xs: Vec<Nm>, ys: Vec<Nm>) -> Raster {
    assert!(cell > 0 && poses.len() == problem.movables.len());
    let (pitch, clearance) = pitch_and_clearance(board);
    assert!(pitch > 0, "track pitch must be positive");
    let bb = board.outline_bbox();
    let (w, h) = (xs.len() - 1, ys.len() - 1);
    let layers = board.stackup.copper().len();
    let mut raster = Raster { w, h, demand: vec![0; w * h], area2: vec![vec![0; w * h]; layers], wire: Vec::new(), xs, ys };
    if w * h == 0 {
        return raster;
    }
    let outline = board.outline_polyset(1000);
    let rect = area2(&outline) == 2 * i128::from(bb.width()) * i128::from(bb.height());
    for (boxes, free) in obstacles(board, problem, poses, clearance).into_iter().zip(&mut raster.area2) {
        layer_free_area2(&raster.xs, &raster.ys, boxes, &outline, rect, free);
    }

    let (xs, ys) = (&raster.xs, &raster.ys);
    // Spread `amount` evenly over `area`, grown to at least `least` per axis and cut to the box:
    // the cells it covers, and its share in each, the remainder carried so the shares sum exactly.
    let spread = |area: BBox, least: Nm, amount: i64| -> Option<((usize, usize), (usize, usize), Vec<i64>)> {
        let window = |a: Nm, b: Nm, lo: Nm, hi: Nm| {
            let extra = (i64::from(least) - (i64::from(b) - i64::from(a))).max(0);
            ((i64::from(a) - extra / 2).max(i64::from(lo)), (i64::from(b) + extra - extra / 2).min(i64::from(hi)))
        };
        let (x0, x1) = window(area.min.x, area.max.x, bb.min.x, bb.max.x);
        let (y0, y1) = window(area.min.y, area.max.y, bb.min.y, bb.max.y);
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let total = i128::from(x1 - x0) * i128::from(y1 - y0);
        let cols = (index(xs, x0 as Nm), index(xs, (x1 - 1) as Nm) + 1);
        let rows = (index(ys, y0 as Nm), index(ys, (y1 - 1) as Nm) + 1);
        let mut nm = Vec::with_capacity((cols.1 - cols.0) * (rows.1 - rows.0));
        let mut remainder = 0;
        for y in rows.0..rows.1 {
            let dy = y1.min(i64::from(ys[y + 1])) - y0.max(i64::from(ys[y]));
            for x in cols.0..cols.1 {
                let dx = x1.min(i64::from(xs[x + 1])) - x0.max(i64::from(xs[x]));
                let share = i128::from(amount) * i128::from(dx) * i128::from(dy) + remainder;
                remainder = share % total;
                nm.push((share / total) as i64);
            }
        }
        Some((cols, rows, nm))
    };
    let mut demand = vec![0i64; w * h];
    let mut add = |(cols, rows): ((usize, usize), (usize, usize)), nm: &[i64]| {
        for (k, &v) in nm.iter().enumerate() {
            demand[(rows.0 + k / (cols.1 - cols.0)) * w + cols.0 + k % (cols.1 - cols.0)] += v;
        }
    };
    let mut wire = Vec::new();
    for net in problem.nets.iter().filter(|n| n.weight > 0) {
        let mut pins = BBox::EMPTY;
        for pin in &net.pins {
            let p = match *pin {
                PinRef::Fixed(p) => p,
                PinRef::Mov(i, pad) => problem.movables[i].pad_at(pad, poses[i]),
            };
            pins = pins.with(p);
            if bb.contains(p) {
                if let Some((cols, rows, nm)) = spread(BBox::point(p), cell / 2, i64::from(cell)) {
                    add((cols, rows), &nm);
                }
            }
        }
        let length = net.wirelength(pins);
        if length == 0 {
            continue;
        }
        if let Some((cols, rows, nm)) = spread(pins, pitch, length) {
            add((cols, rows), &nm);
            wire.push(NetWire { net: net.net, cols, rows, nm });
        }
    }
    raster.demand = demand;
    raster.wire = wire;
    raster
}
