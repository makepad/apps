//! Cloud volumes (design §2 and §12): what the marched cloud layers stand on.
//!
//! - **The footprint grid** (design §2.2, §6.2 and §12.1 item 1). A layer's 2D recipe, where cloud stands over the
//!   ground and how tall (its column channel), and three channels the recipe carries along (a Cumulus' domain warp,
//!   which its 3D read takes, and its cloud's strength, whose width its billows follow: design §2.4, §12.7 item 5,
//!   §12.9 items 1 and 3), sampled once on a square grid round the viewer, so a march step reads it in a few
//!   nanoseconds instead of evaluating the recipe's octaves. It keeps a mip chain, read at the level of a
//!   texel's footprint on the ray, so a far texel or an 8K one does not alias. Its reads are a quadratic B-spline over
//!   3 x 3 nodes (continuous slope, weights never negative) through the recipe's values at the nodes, so a cloud's
//!   edge does not run in straight stretches with kinks at the cell borders. A max pyramid holds per-cell bounds (the
//!   most footprint, the lowest base, the highest top), widened by the caller's margin; a march crosses an empty cell
//!   of it in one move.
//! - Grids are built in parallel over the bake pool's rows, polling the caller's cancel, and shared process-wide by
//!   key (`shared_footprint_grid`).
//!
//! Lengths are metres; ground positions are x east and z south of the viewer.

use makepad_draw::*;
use crate::caster::{serial_rows, RowRunner};
use crate::volumetric::Cancelled;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Rows a call of the row runner takes while a grid builds: the cancel is polled between calls.
const ROWS_PER_CALL: usize = 64;
/// Grids no live map holds that the cache keeps, the newest ones.
const KEEP_UNHELD: usize = 8;
/// The max pyramid starts at this level of the mip chain (cells of 4 x 4 nodes, 250 m on a 1024 grid over 64 km): a
/// skip needs no finer cell, and finer ones would cost more than the nodes themselves.
const PYRAMID_FIRST: usize = 2;

/// What a cell of the max pyramid holds: the most footprint any read inside it can give, and the lowest base and the
/// highest top its cloud can reach, in metres above the ground. A cell with no footprint is empty whatever its base
/// and top; EMPTY (no footprint, an infinite base, a top at minus infinity) changes nothing it is joined with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellBounds {
    pub footprint: f32,
    pub base_m: f32,
    pub top_m: f32,
}

impl CellBounds {
    pub const EMPTY: CellBounds = CellBounds { footprint: 0.0, base_m: f32::INFINITY, top_m: f32::NEG_INFINITY };

    /// No cloud can stand in it.
    pub fn is_empty(&self) -> bool {
        !(self.footprint > 0.0)
    }
}

/// A grid node's channels: the footprint, the column and three carried channels that no bound reads (a Cumulus'
/// domain warp, metres east and south, design §2.4, and the strength of its cloud, whose width its billows follow,
/// §12.9 items 1 and 3).
pub type Node = [f32; CHANNELS];
/// The channels a node holds.
pub const CHANNELS: usize = 5;

/// The footprint and column ranges, [low, high] each, that the reads inside a pyramid cell can take; EMPTY changes
/// nothing it is joined with.
#[derive(Clone, Copy)]
struct Ranges {
    f: [f32; 2],
    c: [f32; 2],
}

impl Ranges {
    const EMPTY: Ranges = Ranges { f: [f32::INFINITY, f32::NEG_INFINITY], c: [f32::INFINITY, f32::NEG_INFINITY] };

    fn join(self, other: Ranges) -> Ranges {
        Ranges { f: [self.f[0].min(other.f[0]), self.f[1].max(other.f[1])], c: [self.c[0].min(other.c[0]), self.c[1].max(other.c[1])] }
    }
}

/// How far the drawn cloud can stand outside what its footprint and bounds say (design §12.1 item 1): `across_m`
/// sideways (a lean's shift, a domain warp), `below_m` under the bounds' base (a ragged base's drop) and `above_m` over
/// their top. The max pyramid is widened by it, so a skip never cuts an edge.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SkipMargin {
    pub across_m: f32,
    pub below_m: f32,
    pub above_m: f32,
}

/// One level of the mip chain.
struct Level {
    /// Nodes along an edge.
    size: usize,
    cell_m: f32,
    /// (size + 2)^2 nodes (footprint, column, the three carried channels), x fastest, then z: the grid in a ring of zeros, so a read near the edge
    /// takes zeros outside it without a test.
    nodes: Vec<Node>,
}

/// A layer's footprint and column channels (and three carried ones) on a grid round the viewer, with their mip chain and
/// max pyramid (design §12.1 item 1). Immutable once built; bake threads share it by reference.
pub struct FootprintGrid {
    half_m: f32,
    /// Level 0 first, each next one half as many nodes along an edge, down to one.
    levels: Vec<Level>,
    /// The mip level the max pyramid starts at: PYRAMID_FIRST, or the last level of a smaller grid.
    first: usize,
    /// The max pyramid: bounds[k] is level first + k's, (size >> (first + k))^2 cells, x fastest, cell (i, j) the square
    /// of that level's node (i, j). A cell bounds every read at its level or finer at a point inside it.
    bounds: Vec<Vec<CellBounds>>,
}

impl FootprintGrid {
    /// The grid of `recipe` (footprint, column and the three carried channels at a ground point) at `size` x `size` nodes over `span_m` metres
    /// centred on the viewer, node (i, j) at the centre of its cell. `bounds` gives the lowest base and the highest top
    /// cloud can reach for any footprint and column in the given [low, high] ranges, together (it is asked once a
    /// pyramid cell, empty ones too, with the ranges its reads can take); the pyramid is widened by `margin`. The rows of each stage are spread by `run`, and `cancelled` is
    /// polled between its calls: Err(Cancelled) when it says so or when the runner skips a row. Any runner builds the
    /// same grid bit for bit. `size` is a power of two, at least 2.
    pub fn build(
        size: usize,
        span_m: f32,
        recipe: &(dyn Fn(f32, f32) -> Node + Sync),
        bounds: &(dyn Fn([f32; 2], [f32; 2]) -> (f32, f32) + Sync),
        margin: SkipMargin,
        cancelled: &dyn Fn() -> bool,
        run: RowRunner,
    ) -> Result<FootprintGrid, Cancelled> {
        assert!(size >= 2 && size.is_power_of_two(), "a footprint grid's edge is a power of two, not {size}");
        let half_m = 0.5 * span_m;
        let cell_m = span_m / size as f32;
        // Level 0: the recipe at every node. A NaN reads as no cloud.
        let at = |i: usize| (i as f32 + 0.5) * cell_m - half_m;
        let clean = |v: f32| if v.is_nan() { 0.0 } else { v };
        let rows = rows_by(size, cancelled, run, &|j| {
            let z = at(j);
            (0..size)
                .map(|i| {
                    recipe(at(i), z).map(clean)
                })
                .collect()
        })?;
        // The nodes the read weighs are the quadratic B-spline's coefficients of the recipe's values, along x and then
        // along z, so a read passes through the recipe at every node: the spline alone blurs like a 1-2-1 filter and
        // drew a cloud's outline 1.7 m inside its recipe's on average. The coarser levels are means of these, so every
        // read stays inside the ranges of the level-0 nodes round it (the pyramid's bounds).
        let rows = rows_by(size, cancelled, run, &|j| {
            let mut row = rows[j].clone();
            interpolating(&mut row);
            row
        })?;
        let columns = rows_by(size, cancelled, run, &|i| {
            let mut column: Vec<Node> = rows.iter().map(|row| row[i]).collect();
            interpolating(&mut column);
            column
        })?;
        let rows: Vec<Vec<Node>> = (0..size).map(|j| columns.iter().map(|column| column[j]).collect()).collect();
        let mut levels = vec![Level { size, cell_m, nodes: padded(size, &rows) }];
        // The mip chain: each node the mean of the four under it.
        while levels.last().is_some_and(|level| level.size > 1) {
            let finer = levels.last().unwrap();
            let (n, w) = (finer.size / 2, finer.size + 2);
            let rows = rows_by(n, cancelled, run, &|j| {
                (0..n)
                    .map(|i| {
                        let p = |di: usize, dj: usize| finer.nodes[(2 * j + dj + 1) * w + 2 * i + di + 1];
                        let (a, b, c, d) = (p(0, 0), p(1, 0), p(0, 1), p(1, 1));
                        std::array::from_fn(|ch| 0.25 * (a[ch] + b[ch] + c[ch] + d[ch]))
                    })
                    .collect()
            })?;
            levels.push(Level { size: n, cell_m: span_m / n as f32, nodes: padded(n, &rows) });
        }
        // The pyramid's finest level. Its cell (i, j) spans k x k level-0 nodes; a level-0 read inside it weighs those
        // and the ring round them, with weights that are never negative, so its footprint and column lie within their
        // ranges there. A read at a coarser level up to this one reaches at most a cell further, which the widening by
        // one cell below takes in.
        let first = PYRAMID_FIRST.min(levels.len() - 1);
        let (n, k) = (size >> first, 1usize << first);
        let base = &levels[0];
        let w = size + 2;
        // The pyramid joins and widens the channels' ranges, not bounds: a read mixing two cells can pair one cell's
        // footprint with the other's column, which only `bounds` over the joined ranges covers.
        let rows = rows_by(n, cancelled, run, &|j| {
            (0..n)
                .map(|i| {
                    let mut ranges = Ranges::EMPTY;
                    // Padded nodes i k ..= (i + 1) k + 1 are level-0 nodes i k - 1 ..= (i + 1) k.
                    for row in j * k..(j + 1) * k + 2 {
                        for &[f, c, ..] in &base.nodes[row * w + i * k..row * w + (i + 1) * k + 2] {
                            ranges = ranges.join(Ranges { f: [f, f], c: [c, c] });
                        }
                    }
                    ranges
                })
                .collect()
        })?;
        // One call of `bounds` a cell, empty cells too, on the ranges its reads can take.
        let to_bounds = |n: usize, cells: &[Ranges]| -> Result<Vec<CellBounds>, Cancelled> {
            let rows = rows_by(n, cancelled, run, &|j| {
                (0..n)
                    .map(|i| {
                        let Ranges { f, c } = cells[j * n + i];
                        let (low, high) = bounds(f, c);
                        CellBounds { footprint: f[1].max(0.0), base_m: low - margin.below_m, top_m: high + margin.above_m }
                    })
                    .collect()
            })?;
            Ok(rows.concat())
        };
        let reach = (margin.across_m.max(0.0) / levels[first].cell_m).ceil() as usize;
        let mut ranges = widened(n, rows.concat(), reach + 1, cancelled, run)?;
        let mut pyramid = vec![to_bounds(n, &ranges)?];
        // Each coarser level joins the four cells under it and is widened by one of its own cells: a read at that level
        // weighs nodes a cell to either side.
        for l in first + 1..levels.len() {
            let n = levels[l].size;
            let rows = rows_by(n, cancelled, run, &|j| {
                (0..n)
                    .map(|i| {
                        let p = |di: usize, dj: usize| ranges[(2 * j + dj) * 2 * n + 2 * i + di];
                        p(0, 0).join(p(1, 0)).join(p(0, 1)).join(p(1, 1))
                    })
                    .collect()
            })?;
            ranges = widened(n, rows.concat(), 1, cancelled, run)?;
            pyramid.push(to_bounds(n, &ranges)?);
        }
        Ok(FootprintGrid { half_m, levels, first, bounds: pyramid })
    }

    /// Footprint, column and the carried channels at a ground point, read at the mip level of `footprint_m` (the width
    /// a texel covers there): level 0 up to one cell, then between the two levels around log2(footprint_m / cell).
    /// Outside the span (and at a NaN point) all 0.
    pub fn sample(&self, x_m: f32, z_m: f32, footprint_m: f32) -> Node {
        if !(x_m.abs() < self.half_m && z_m.abs() < self.half_m) {
            return [0.0; CHANNELS];
        }
        let (low, t) = self.levels_of(footprint_m);
        if !(t > 0.0) {
            return self.read(low, x_m, z_m);
        }
        let (a, b) = (self.read(low, x_m, z_m), self.read(low + 1, x_m, z_m));
        std::array::from_fn(|ch| a[ch] + (b[ch] - a[ch]) * t)
    }

    /// The finer of the two levels `sample` reads at `footprint_m`, and the weight of the coarser one (0: the finer
    /// alone).
    #[inline]
    fn levels_of(&self, footprint_m: f32) -> (usize, f32) {
        let cells = footprint_m / self.levels[0].cell_m;
        if !(cells > 1.0) {
            return (0, 0.0);
        }
        let level = cells.log2();
        let last = self.levels.len() - 1;
        let low = (level.floor() as usize).min(last);
        let t = level - low as f32;
        if low >= last || !(t > 0.0) {
            (low, 0.0)
        } else {
            (low, t)
        }
    }

    /// The coarsest level `sample` reads at `footprint_m`: a pyramid cell at this level or a coarser one bounds the
    /// read (design §12.1 item 1).
    pub fn level_for(&self, footprint_m: f32) -> usize {
        let (low, t) = self.levels_of(footprint_m);
        if t > 0.0 {
            low + 1
        } else {
            low
        }
    }

    /// Half the span, metres: the grid covers |x|, |z| under it round the viewer.
    pub fn half_m(&self) -> f32 {
        self.half_m
    }

    /// Where a straight track x = o + v t (ground metres, t rising) leaves the max pyramid's cell at `level` that holds
    /// its point at `t`: the t of the nearer of the cell's far edges (infinite along an axis it does not move on).
    #[inline]
    pub fn cell_exit(&self, level: usize, o: Vec2f, v: Vec2f, t: f32) -> f32 {
        let level = level.clamp(self.first, self.levels.len() - 1);
        let lv = &self.levels[level];
        let edge = |o: f32, v: f32| {
            if v.abs() < 1.0e-12 {
                return f32::INFINITY;
            }
            let g = (o + v * t + self.half_m) / lv.cell_m;
            let next = if v > 0.0 { g.floor() + 1.0 } else { g.ceil() - 1.0 };
            (next * lv.cell_m - self.half_m - o) / v
        };
        edge(o.x, v.x).min(edge(o.y, v.y))
    }

    /// The read at `level` (inside the span): a quadratic B-spline over the 3 x 3 nodes round the node whose cell holds
    /// the point. Its weights are never negative and its slope is continuous, so the footprint's contours have no flat
    /// spots or kinks at the node lines (the smooth bilinear weights' slope was 0 all along them, a flat spot every 66
    /// m of an outline). The node is the cell the pyramid's own index finds at this level (the same truncation, and
    /// cells a power of two apart), so the nodes a read weighs are those round its pyramid cell, as the bilinear read's
    /// were (design §12.1 item 1).
    fn read(&self, level: usize, x_m: f32, z_m: f32) -> Node {
        let lv = &self.levels[level];
        let top = (lv.size - 1) as f32;
        // The node whose cell holds the point, and where the point sits in it (-0.5 to 0.5 from its centre); held so
        // rounding at the span's edge stays inside the ring of zeros.
        let gx = (x_m + self.half_m) / lv.cell_m;
        let gz = (z_m + self.half_m) / lv.cell_m;
        let (nx, nz) = (gx.floor().clamp(0.0, top), gz.floor().clamp(0.0, top));
        let (wx, wz) = (quadratic_weights(gx - nx - 0.5), quadratic_weights(gz - nz - 0.5));
        let w = lv.size + 2;
        // Padded node (nx - 1, nz - 1), the first of the 3 x 3.
        let k = nz as usize * w + nx as usize;
        let mut out: Node = [0.0; CHANNELS];
        for (j, &weight) in wz.iter().enumerate() {
            let row = &lv.nodes[k + j * w..k + j * w + 3];
            for c in 0..CHANNELS {
                out[c] += weight * (wx[0] * row[0][c] + wx[1] * row[1][c] + wx[2] * row[2][c]);
            }
        }
        out
    }

    /// The max pyramid's cell at `level` holding a ground point (its first level's for a finer one); EMPTY outside the
    /// span.
    pub fn bounds_at(&self, level: usize, x_m: f32, z_m: f32) -> CellBounds {
        let level = level.clamp(self.first, self.levels.len() - 1);
        if !(x_m.abs() < self.half_m && z_m.abs() < self.half_m) {
            return CellBounds::EMPTY;
        }
        let lv = &self.levels[level];
        let n = lv.size;
        let i = (((x_m + self.half_m) / lv.cell_m) as usize).min(n - 1);
        let j = (((z_m + self.half_m) / lv.cell_m) as usize).min(n - 1);
        self.bounds[level - self.first][j * n + i]
    }

    /// Every max-pyramid cell at `level` that the straight track x = o + v t (ground metres) crosses for t in t0..t1, in
    /// order, with the stretch of t inside it (an integer walk from cell to cell, so no cell is missed where the track
    /// meets a corner; EMPTY outside the span). `visit` returns false to stop: then so does this.
    pub fn cells_along(&self, level: usize, o: Vec2f, v: Vec2f, t0: f32, t1: f32, visit: &mut dyn FnMut(CellBounds, f32, f32) -> bool) -> bool {
        let level = level.clamp(self.first, self.levels.len() - 1);
        let lv = &self.levels[level];
        let (n, cell) = (lv.size as i64, lv.cell_m);
        let cells = &self.bounds[level - self.first];
        let index = |p: f32| ((p + self.half_m) / cell).floor() as i64;
        let (mut i, mut j) = (index(o.x + v.x * t0), index(o.y + v.y * t0));
        // The t of the next edge on an axis, and between edges.
        let axis = |o: f32, v: f32, i: i64| -> (i64, f32, f32) {
            if v > 0.0 {
                (1, ((i + 1) as f32 * cell - self.half_m - o) / v, cell / v)
            } else if v < 0.0 {
                (-1, (i as f32 * cell - self.half_m - o) / v, -cell / v)
            } else {
                (0, f32::INFINITY, f32::INFINITY)
            }
        };
        let (step_i, mut next_i, every_i) = axis(o.x, v.x, i);
        let (step_j, mut next_j, every_j) = axis(o.y, v.y, j);
        let mut t = t0;
        loop {
            let end = next_i.min(next_j).min(t1);
            let bounds = if (0..n).contains(&i) && (0..n).contains(&j) { cells[(j * n + i) as usize] } else { CellBounds::EMPTY };
            if !visit(bounds, t, end) {
                return false;
            }
            if !(end < t1) {
                return true;
            }
            if next_i <= next_j {
                i += step_i;
                next_i += every_i;
            } else {
                j += step_j;
                next_j += every_j;
            }
            t = end;
        }
    }

    /// The finest level the max pyramid has.
    pub fn first_bounds_level(&self) -> usize {
        self.first
    }

    /// The mip chain's levels (level 0 the finest).
    pub fn levels(&self) -> usize {
        self.levels.len()
    }

    /// The width of a cell at `level`, metres.
    pub fn cell_m(&self, level: usize) -> f32 {
        self.levels[level.min(self.levels.len() - 1)].cell_m
    }

    /// Nodes along an edge at level 0.
    pub fn size(&self) -> usize {
        self.levels[0].size
    }

    /// The grid as the GPU export reads it (design §12.3): every level's padded nodes in `out`'s vectors (footprint,
    /// column, warp east, warp south) and scalars (strength), the max pyramid's cells in its vectors (footprint, base,
    /// top, 0; an empty cell's base and top at +-EMPTY_FAR, so a float texture holds them), and two tables in its vectors:
    /// per level (its nodes' vector offset, their strengths' scalar offset, nodes along an edge, cell metres) from the
    /// first's offset, per pyramid level from `first` (its cells' offset, cells along an edge, cell metres, 0) from the
    /// second's. The shader's reads mirror `read`, `levels_of`, `bounds_at` and `cell_exit`.
    pub(crate) fn gpu_layout(&self, out: &mut crate::cloud_volume_gpu::UploadBuilder) -> (f32, f32) {
        use crate::cloud_volume_gpu::index;
        const EMPTY_FAR: f32 = 1.0e30;
        let mut levels = Vec::with_capacity(self.levels.len());
        for level in &self.levels {
            let vectors = out.vectors(level.nodes.iter().map(|n| [n[0], n[1], n[2], n[3]]));
            let scalars = out.scalars(level.nodes.iter().map(|n| n[4]));
            levels.push([index(vectors), index(scalars), level.size as f32, level.cell_m]);
        }
        let mut bounds = Vec::with_capacity(self.bounds.len());
        for (k, cells) in self.bounds.iter().enumerate() {
            let lv = &self.levels[self.first + k];
            let offset = out.vectors(cells.iter().map(|c| if c.is_empty() { [0.0, EMPTY_FAR, -EMPTY_FAR, 0.0] } else { [c.footprint, c.base_m.min(EMPTY_FAR), c.top_m.max(-EMPTY_FAR), 0.0] }));
            bounds.push([index(offset), lv.size as f32, lv.cell_m, 0.0]);
        }
        (index(out.vectors(levels)), index(out.vectors(bounds)))
    }

    /// The memory it holds, bytes.
    pub fn bytes(&self) -> usize {
        let nodes: usize = self.levels.iter().map(|level| level.nodes.len() * std::mem::size_of::<Node>()).sum();
        let cells: usize = self.bounds.iter().map(|level| level.len() * std::mem::size_of::<CellBounds>()).sum();
        nodes + cells
    }
}

/// The quadratic B-spline's pole (Unser 1993): its coefficients of values sampled at the nodes come from a causal and
/// an anticausal pass of this pole, gain (1 - z)(1 - 1/z) = 8.
const QUADRATIC_POLE: f32 = -0.171_572_87;
/// The causal pass starts from this many values (|z|^10 is under 3e-8).
const POLE_HORIZON: usize = 10;

/// `values` (one line of nodes, every channel) replaced by the quadratic B-spline's coefficients that pass through them
/// (c[k - 1] + 6 c[k] + c[k + 1]) / 8 = value[k], the line mirrored at its ends (Unser, Aldroubi and Eden 1993;
/// Thevenaz, Blu and Unser 2000).
fn interpolating(values: &mut [Node]) {
    let n = values.len();
    if n < 2 {
        return;
    }
    let z = QUADRATIC_POLE;
    for v in values.iter_mut() {
        *v = v.map(|x| 8.0 * x);
    }
    for c in 0..CHANNELS {
        // Causal: the first coefficient from the mirrored line, then forward.
        let (mut sum, mut zk) = (0.0f32, 1.0f32);
        for v in values.iter().take(POLE_HORIZON.min(n)) {
            sum += zk * v[c];
            zk *= z;
        }
        values[0][c] = sum;
        for k in 1..n {
            values[k][c] += z * values[k - 1][c];
        }
        // Anticausal: the last from the mirrored line, then backward.
        values[n - 1][c] = (z / (z * z - 1.0)) * (z * values[n - 2][c] + values[n - 1][c]);
        for k in (0..n - 1).rev() {
            values[k][c] = z * (values[k + 1][c] - values[k][c]);
        }
    }
}

/// The quadratic B-spline's weights of the nodes before, at and after the one a point sits `u` (-0.5 to 0.5) from:
/// never negative, summing to 1.
#[inline]
fn quadratic_weights(u: f32) -> [f32; 3] {
    let (a, b) = (0.5 - u, 0.5 + u);
    [0.5 * a * a, 0.75 - u * u, 0.5 * b * b]
}

/// `rows` (n rows of n nodes) in a ring of zeros: (n + 2)^2 nodes.
fn padded(n: usize, rows: &[Vec<Node>]) -> Vec<Node> {
    let w = n + 2;
    let mut nodes = vec![[0.0f32; CHANNELS]; w * w];
    for (j, row) in rows.iter().enumerate() {
        nodes[(j + 1) * w + 1..(j + 1) * w + 1 + n].copy_from_slice(row);
    }
    nodes
}

/// `cells` (n x n) with each cell joined with its neighbours up to `reach` cells away on either axis: a pass along x,
/// then one along z.
fn widened(n: usize, cells: Vec<Ranges>, reach: usize, cancelled: &dyn Fn() -> bool, run: RowRunner) -> Result<Vec<Ranges>, Cancelled> {
    if reach == 0 {
        return Ok(cells);
    }
    let span = |i: usize| i.saturating_sub(reach)..(i + reach + 1).min(n);
    let along_x = rows_by(n, cancelled, run, &|j| (0..n).map(|i| span(i).fold(Ranges::EMPTY, |b, k| b.join(cells[j * n + k]))).collect())?.concat();
    let along_z = rows_by(n, cancelled, run, &|j| (0..n).map(|i| span(j).fold(Ranges::EMPTY, |b, k| b.join(along_x[k * n + i]))).collect())?.concat();
    Ok(along_z)
}

/// `row(0..rows)` through `run`, at most ROWS_PER_CALL rows a call and as many at each, `cancelled` polled before each
/// call: Err(Cancelled) when it says so, or when the runner skipped a row. Under 32 rows (the coarse levels) the rows
/// run serially: a pool round trip would cost more than their work.
pub(crate) fn rows_by<T: Send + Sync>(rows: usize, cancelled: &dyn Fn() -> bool, run: RowRunner, row: &(dyn Fn(usize) -> Vec<T> + Sync)) -> Result<Vec<Vec<T>>, Cancelled> {
    let run: RowRunner = if rows < 32 { &serial_rows } else { run };
    let out: Vec<OnceLock<Vec<T>>> = (0..rows).map(|_| OnceLock::new()).collect();
    let per_call = rows.div_ceil(rows.div_ceil(ROWS_PER_CALL).max(1)).max(1);
    for start in (0..rows).step_by(per_call) {
        if cancelled() {
            return Err(Cancelled);
        }
        let n = per_call.min(rows - start);
        run(n, &|k| {
            let _ = out[start + k].set(row(start + k));
        });
    }
    out.into_iter().map(|done| done.into_inner().ok_or(Cancelled)).collect()
}

/// The blue-noise tile's edge (design §12.1 item 8).
const BLUE_SIZE: usize = 64;

/// A march's start offset for the texel (`ix`, `iy`) of a map, in [0, 1) (design §12.1 item 8): a 64 x 64 blue-noise
/// tile, so the grain the offsets leave has no low frequencies (no blotches), turned or mirrored and shifted per tile
/// by a hash of the tile, so the map does not repeat it every 64 texels. The tile is generated once per process
/// (`blue_noise_ranks`, about 20 ms) and never allocates afterwards.
pub fn blue_jitter(ix: u32, iy: u32) -> f32 {
    let ranks = blue_ranks();
    let n = BLUE_SIZE as u32;
    let h = crate::noise::hash_u32((ix / n).wrapping_mul(0x9e37_79b9) ^ crate::noise::hash_u32(iy / n ^ 0x2c1b_3c6d));
    let (x, y) = ((ix % n + (h >> 8)) % n, (iy % n + (h >> 16)) % n);
    // One of the tile's eight turns and mirrors.
    let (x, y) = if h & 1 != 0 { (y, x) } else { (x, y) };
    let x = if h & 2 != 0 { n - 1 - x } else { x };
    let y = if h & 4 != 0 { n - 1 - y } else { y };
    (ranks[(y * n + x) as usize] as f32 + 0.5) / (n * n) as f32
}

/// The blue-noise tile's ranks (`blue_jitter`'s), BLUE_SIZE x BLUE_SIZE row by row, generated once per process: the GPU
/// export uploads them (design §12.3).
pub(crate) fn blue_ranks() -> &'static [u16] {
    static RANKS: OnceLock<Vec<u16>> = OnceLock::new();
    RANKS.get_or_init(blue_noise_ranks)
}

/// The ranks 0..4096 of a 64 x 64 blue-noise tile, by void and cluster (Ulichney 1993, our own code): a Gaussian
/// energy (sigma 1.5 texels, wrapping) over the placed points; a starting pattern of a tenth of the texels is relaxed
/// by moving its tightest point into its largest void until that changes nothing; then its points are ranked by
/// taking out the tightest one after another (ranks downward), and the rest by filling the largest void after
/// another (ranks upward). Deterministic: integer hashes, a serial loop, ties to the lowest index.
fn blue_noise_ranks() -> Vec<u16> {
    const N: usize = BLUE_SIZE * BLUE_SIZE;
    const REACH: i32 = 6;
    let kernel: Vec<f32> = (-REACH..=REACH).flat_map(|dy| (-REACH..=REACH).map(move |dx| (-((dx * dx + dy * dy) as f32) / (2.0 * 1.5 * 1.5)).exp())).collect();
    // Adds `sign` x the kernel round texel i into `energy`.
    let spread = |energy: &mut [f32], i: usize, sign: f32| {
        let (x, y) = ((i % BLUE_SIZE) as i32, (i / BLUE_SIZE) as i32);
        let mut k = 0;
        for dy in -REACH..=REACH {
            let row = (y + dy).rem_euclid(BLUE_SIZE as i32) as usize * BLUE_SIZE;
            for dx in -REACH..=REACH {
                energy[row + (x + dx).rem_euclid(BLUE_SIZE as i32) as usize] += sign * kernel[k];
                k += 1;
            }
        }
    };
    // The tightest placed point (most energy) or the largest void (least energy among the empty).
    let pick = |energy: &[f32], placed: &[bool], want: bool| {
        let mut best = usize::MAX;
        for i in 0..N {
            if placed[i] == want && (best == usize::MAX || (want && energy[i] > energy[best]) || (!want && energy[i] < energy[best])) {
                best = i;
            }
        }
        best
    };
    let mut placed = vec![false; N];
    let mut energy = vec![0.0f32; N];
    let mut count = 0usize;
    let mut k = 0u32;
    while count < N / 10 {
        let i = crate::noise::hash_u32(k ^ 0x5bd1_e995) as usize % N;
        k += 1;
        if !placed[i] {
            placed[i] = true;
            spread(&mut energy, i, 1.0);
            count += 1;
        }
    }
    for _ in 0..N {
        let tight = pick(&energy, &placed, true);
        placed[tight] = false;
        spread(&mut energy, tight, -1.0);
        let void = pick(&energy, &placed, false);
        placed[void] = true;
        spread(&mut energy, void, 1.0);
        if void == tight {
            break;
        }
    }
    let mut ranks = vec![0u16; N];
    let (start_placed, start_energy) = (placed.clone(), energy.clone());
    for rank in (0..count).rev() {
        let tight = pick(&energy, &placed, true);
        placed[tight] = false;
        spread(&mut energy, tight, -1.0);
        ranks[tight] = rank as u16;
    }
    let (mut placed, mut energy) = (start_placed, start_energy);
    for rank in count..N {
        let void = pick(&energy, &placed, false);
        placed[void] = true;
        spread(&mut energy, void, 1.0);
        ranks[void] = rank as u16;
    }
    ranks
}

/// The grids in use, oldest first (`shared_footprint_grid`).
static GRIDS: GridCache<FootprintGrid> = Mutex::new(Vec::new());

/// The key of a grid: the bits of every field its recipe, bounds and margin read (a type's shape fields, never Cover
/// or the light rows), the seed, the size and the span. FNV-1a over them.
pub fn footprint_grid_key(fields: &[f32], seed: u32, size: usize, span_m: f32) -> u64 {
    let fnv = |h: u64, bytes: [u8; 4]| bytes.iter().fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3));
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for field in fields {
        h = fnv(h, field.to_bits().to_le_bytes());
    }
    for word in [seed, size as u32, span_m.to_bits()] {
        h = fnv(h, word.to_le_bytes());
    }
    h
}

/// The grid of `key`, shared process-wide: a lookup makes it the newest; a key not kept is built by `build` outside
/// the lock (it may be stopped: then nothing is kept), and when two threads built one key the first one's stays. The
/// cache keeps every grid a live map holds (an Arc beside its own) and the KEEP_UNHELD newest of the rest; it drops
/// the oldest past that, never fails for a count, and logs each build and each drop.
pub fn shared_footprint_grid(key: u64, build: &dyn Fn() -> Result<FootprintGrid, Cancelled>) -> Result<Arc<FootprintGrid>, Cancelled> {
    shared_by_key(&GRIDS, key, build, "footprint grid", &|grid, ms| format!("{}^2 built in {:.0} ms ({:.1} MB)", grid.size(), ms, grid.bytes() as f64 / 1.0e6))
}

/// A process-wide cache of built grids by key, oldest first ([`shared_by_key`]).
pub(crate) type GridCache<T> = Mutex<Vec<(u64, Arc<T>)>>;

/// The item of `key` in `cache`: a lookup makes it the newest; a key not kept is built by `build` outside the lock (it
/// may be stopped: then nothing is kept), and when two threads built one key the first one's stays. The cache keeps
/// every item a live map holds (an Arc beside its own) and the KEEP_UNHELD newest of the rest; it drops the oldest past
/// that, never fails for a count, and logs each build (`what` and `built`'s text, given the item and the build's
/// milliseconds) and each drop.
pub(crate) fn shared_by_key<T>(cache: &GridCache<T>, key: u64, build: &dyn Fn() -> Result<T, Cancelled>, what: &str, built: &dyn Fn(&T, f64) -> String) -> Result<Arc<T>, Cancelled> {
    if let Some(item) = kept_by_key(cache, key) {
        return Ok(item);
    }
    let start = std::time::Instant::now();
    let item = Arc::new(build()?);
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let (item, dropped, kept) = {
        let mut items = cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, first)) = items.iter().find(|(kept, _)| *kept == key) {
            return Ok(first.clone());
        }
        items.push((key, item.clone()));
        let mut unheld = items.iter().filter(|(_, item)| Arc::strong_count(item) == 1).count();
        let mut dropped = 0usize;
        items.retain(|(_, item)| {
            let goes = unheld > KEEP_UNHELD && Arc::strong_count(item) == 1;
            if goes {
                unheld -= 1;
                dropped += 1;
            }
            !goes
        });
        (item, dropped, items.len())
    };
    log!("hdri clouds: {} {}", what, built(&item, ms));
    if dropped > 0 {
        log!("hdri clouds: {}s trimmed to {} ({} dropped)", what, kept, dropped);
    }
    Ok(item)
}

/// The item of `key` when `cache` keeps it (made the newest), without building one.
pub(crate) fn kept_by_key<T>(cache: &GridCache<T>, key: u64) -> Option<Arc<T>> {
    let mut items = cache.lock().unwrap_or_else(PoisonError::into_inner);
    let i = items.iter().position(|(kept, _)| *kept == key)?;
    let found = items.remove(i);
    let item = found.1.clone();
    items.push(found);
    Some(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caster::serial_rows;
    use crate::cloud_classic::{downwind, stand_height_m};
    use crate::cloud_sheets::Sheet;
    use crate::noise::{fbm2, hash01};
    use crate::{dir_from_az_el, CloudGenus, CloudLayerParams};

    /// Runs every index on four scoped threads, interleaved, as the bake pool's fan_out would.
    fn four_threads(n: usize, f: &(dyn Fn(usize) + Sync)) {
        std::thread::scope(|scope| {
            for t in 0..4 {
                scope.spawn(move || {
                    let mut i = t;
                    while i < n {
                        f(i);
                        i += 4;
                    }
                });
            }
        });
    }

    /// Every node and every pyramid cell, as bits.
    fn bits(grid: &FootprintGrid) -> (Vec<u32>, Vec<u32>) {
        let nodes = grid.levels.iter().flat_map(|level| level.nodes.iter().flat_map(|n| n.map(f32::to_bits))).collect();
        let cells = grid.bounds.iter().flat_map(|level| level.iter().flat_map(|b| [b.footprint.to_bits(), b.base_m.to_bits(), b.top_m.to_bits()])).collect();
        (nodes, cells)
    }

    /// The grid of the phase-1 Cumulus footprint (its sheet's density at its defaults and cover 0.5, design §2.2's
    /// starting point) with a broad column channel, against the recipe itself: at 1000 random points inside the span its
    /// level-0 read is the recipe within 0.02, both channels, on 7.8 m cells, six to the recipe's finest octave (47 m
    /// at Size 1.5 km). A grid's spacing must resolve its recipe: on the 62.5 m cells of 1024 over 64 km this recipe is
    /// off by up to 0.2 (`building_and_marching_are_fast_enough` prints it), so a volume's footprint recipe leaves its
    /// finest detail to the 3D noise (design §2.2, §12.1 item 1). Built on four threads it is the serial grid bit for bit,
    /// and a cancel or a runner that skips a row stops it. Its max pyramid bounds every finer cell, and every read at
    /// its level or finer inside a cell: the footprint and the column's top (the bounds of what was read) lie within the
    /// cell's, also for bounds that read both channels where a coarse read mixes cells holding one each. Outside the
    /// span it reads nothing. The cache shares one grid per key and builds it once. The volume's own Cumulus recipe,
    /// band-limited, is read off its 1024 grid over the real span within 0.02 too.
    #[test]
    fn the_footprint_grid_is_its_recipe() {
        let genus = CloudGenus::Cumulus;
        let layer = CloudLayerParams::new(genus, &[], 1);
        let block = layer.cumulus_params();
        let cell = block.size_km * 1000.0;
        let sheet = Sheet::build(&layer, genus, 0.5, 1, downwind(30.0), dir_from_az_el(200.0, 30.0), stand_height_m(&layer, genus)).expect("a Cumulus sheet");
        let recipe = |x: f32, z: f32| [sheet.density_at(vec2f(x / cell, z / cell)), fbm2(x / (4.0 * cell), z / (4.0 * cell), 2, 7), 0.0, 0.0, 0.0];
        // A column's top rises with its channel (design §2.2's top = mix(0.4, 1, column)); the bounds take the highest.
        let (base, tops) = (block.base_m, block.tops_m);
        let top_of = |column: f32| base + (tops - base) * (0.4 + 0.6 * column.clamp(0.0, 1.0));
        let bounds = |_: [f32; 2], column: [f32; 2]| (base, top_of(column[1]));
        let margin = SkipMargin { across_m: 300.0, below_m: 100.0, above_m: 50.0 };
        let (size, span) = (512usize, 4000.0f32);
        let grid = FootprintGrid::build(size, span, &recipe, &bounds, margin, &|| false, &serial_rows).expect("nothing cancels");
        let spread = FootprintGrid::build(size, span, &recipe, &bounds, margin, &|| false, &four_threads).expect("nothing cancels");
        assert!(bits(&grid) == bits(&spread), "a parallel build is the serial one bit for bit");
        assert_eq!(grid.levels(), size.trailing_zeros() as usize + 1);
        assert!(FootprintGrid::build(size, span, &recipe, &bounds, margin, &|| true, &serial_rows).is_err());
        let skipping = |n: usize, row: &(dyn Fn(usize) + Sync)| (0..n).filter(|j| j % 2 == 0).for_each(row);
        assert!(FootprintGrid::build(size, span, &recipe, &bounds, margin, &|| false, &skipping).is_err());
        // The recipe, at random points.
        let point = |k: i32| {
            let r = |salt: u32| (hash01(k, 0, 0, 0x51ed_270b ^ salt) - 0.5) * 0.98 * span;
            (r(1), r(2))
        };
        let (mut worst, mut sum) = (0.0f32, 0.0f32);
        for k in 0..1000 {
            let (x, z) = point(k);
            let (read, want) = (grid.sample(x, z, 0.0), recipe(x, z));
            let off = (read[0] - want[0]).abs().max((read[1] - want[1]).abs());
            worst = worst.max(off);
            sum += off;
        }
        assert!(worst <= 0.02, "the grid is off the recipe by {worst} ({} on average)", sum / 1000.0);
        // The pyramid bounds every finer cell.
        let first = grid.first_bounds_level();
        assert_eq!(first, PYRAMID_FIRST);
        for l in first + 1..grid.levels() {
            let n = grid.levels[l].size;
            for j in 0..n {
                for i in 0..n {
                    let cell = grid.bounds[l - first][j * n + i];
                    for (di, dj) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let child = grid.bounds[l - first - 1][(2 * j + dj) * 2 * n + 2 * i + di];
                        assert!(child.footprint <= cell.footprint && child.base_m >= cell.base_m && child.top_m <= cell.top_m, "level {l} ({i}, {j}): {cell:?} under {child:?}");
                    }
                }
            }
        }
        // And every read inside a cell at its level or finer, the margin included.
        let mut clouded = 0;
        for k in 0..1000 {
            let (x, z) = point(k + 1000);
            for l in first..grid.levels() {
                let cell = grid.bounds_at(l, x, z);
                for finer in 0..=l {
                    let [f, column, ..] = grid.sample(x, z, grid.cell_m(finer));
                    assert!(f <= cell.footprint, "level {l}, read at {finer}, ({x}, {z}): {f} over {cell:?}");
                    if f > 0.0 {
                        assert!(cell.base_m <= base - margin.below_m && top_of(column) + margin.above_m <= cell.top_m, "level {l}, read at {finer}, ({x}, {z}): {column} outside {cell:?}");
                    }
                }
            }
            clouded += (grid.sample(x, z, 0.0)[0] > 0.0) as usize;
        }
        assert!(clouded > 100 && clouded < 900, "cover 0.5 has cloud and sky: {clouded} of 1000 points under cloud");
        // The read's 3 x 3 nodes are found by the pyramid's own index, so a point on a pyramid cell's edge, or an ulp
        // either side of it, reads only nodes its cell bounds.
        let c0 = grid.cell_m(first);
        for k in 0..1000 {
            let (_, z) = point(k + 4000);
            let edge = ((hash01(k, 5, 0, 0x0ed9_e5ee) * ((size >> first) - 1) as f32).floor() + 1.0) * c0 - 0.5 * span;
            for x in [edge, edge.next_up(), edge.next_down()] {
                for l in first..grid.levels() {
                    let cell = grid.bounds_at(l, x, z);
                    for finer in 0..=l {
                        let [f, ..] = grid.sample(x, z, grid.cell_m(finer));
                        assert!(f <= cell.footprint, "on an edge: level {l}, read at {finer}, ({x}, {z}): {f} over {cell:?}");
                    }
                }
            }
        }
        // The margin widens the finest cells: a cell up to its reach from cloud is not empty.
        let c0 = grid.cell_m(first);
        for k in 0..1000 {
            let (x, z) = point(k + 2000);
            if grid.sample(x, z, 0.0)[0] > 0.0 {
                for (dx, dz) in [(margin.across_m, 0.0), (-margin.across_m, 0.0), (0.0, margin.across_m), (0.0, -margin.across_m)] {
                    let (px, pz) = (x + dx, z + dz);
                    let inside = px.abs() < 0.5 * span - c0 && pz.abs() < 0.5 * span - c0;
                    assert!(!inside || !grid.bounds_at(0, px, pz).is_empty(), "({px}, {pz}) is {} m from cloud at ({x}, {z})", margin.across_m);
                }
            }
        }
        // Bounds that read both channels (a top from footprint times column) hold too. Stripes 240 m apart put the
        // footprint and the column in different cells, so no cell holds both, but a coarse read mixes them.
        let depth = tops - base;
        let both = |f: f32, column: f32| base + depth * f.clamp(0.0, 1.0) * column.clamp(0.0, 1.0);
        let striped = |x: f32, _: f32| {
            let m = ((x + 640.0) / 10.0).floor().rem_euclid(24.0);
            [if m < 4.0 { 1.0 } else { 0.0 }, if (12.0..16.0).contains(&m) { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0]
        };
        let bounds_both = |f: [f32; 2], column: [f32; 2]| (base, both(f[1], column[1]));
        let stripes = FootprintGrid::build(128, 1280.0, &striped, &bounds_both, SkipMargin::default(), &|| false, &serial_rows).expect("nothing cancels");
        let mut mixed = 0;
        for k in 0..1000 {
            let (x, z) = point(k + 3000);
            let (x, z) = (x * 0.32, z * 0.32);
            for l in stripes.first_bounds_level()..stripes.levels() {
                let cell = stripes.bounds_at(l, x, z);
                for finer in 0..=l {
                    let [f, column, ..] = stripes.sample(x, z, stripes.cell_m(finer));
                    if f > 0.0 {
                        assert!(both(f, column) <= cell.top_m, "striped, level {l}, read at {finer}, ({x}, {z}): {f} x {column} over {cell:?}");
                        mixed += (both(f, column) > base) as usize;
                    }
                }
            }
        }
        assert!(mixed > 100, "coarse reads mix footprint and column: {mixed}");
        // Outside the span, nothing.
        for (x, z) in [(0.5 * span, 0.0), (0.0, -0.5 * span), (f32::NAN, 0.0), (2.0 * span, span)] {
            assert_eq!(grid.sample(x, z, 0.0), [0.0; CHANNELS]);
            assert!(grid.bounds_at(0, x, z).is_empty());
        }
        // The cache: one grid per key, built once, the same Arc to every caller.
        let key = footprint_grid_key(&[block.puffiness, block.clustering, block.streets], 1, 64, span);
        let builds = std::sync::atomic::AtomicUsize::new(0);
        let make = || {
            builds.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            FootprintGrid::build(64, span, &recipe, &bounds, margin, &|| false, &serial_rows)
        };
        assert!(shared_footprint_grid(key, &|| Err(Cancelled)).is_err(), "a stopped build keeps nothing");
        let (a, b) = (shared_footprint_grid(key, &make).unwrap(), shared_footprint_grid(key, &make).unwrap());
        assert!(Arc::ptr_eq(&a, &b) && builds.into_inner() == 1);
        assert_ne!(key, footprint_grid_key(&[block.puffiness, block.clustering, block.streets], 2, 64, span), "the seed is in the key");
        // The volume's own recipe (design §2.4, §12.1 item 1): a Cumulus at its defaults and at the ends of its Size row,
        // its grid at 1024 over the real span (66 m cells), read at level 0 against the recipe at 1000 random points
        // (inside the outermost two cells, which the grid's ring of zeros fades), all four channels (the warp in cells of
        // Size): band-limited to the wavelengths its
        // cells hold, as the phase-1 recipe above is not, within 0.02 from Size 0.75 up (0.0031 at 1.5: the read passes
        // through every node). The recipe's finest features shrink with Size and the grid's cells do not, so a smaller
        // Size is held less closely (0.0054 at 1.0, 0.010 at 0.75, 0.022 at 0.5, 0.094 at 0.3: tiny clouds draw rounder
        // than their recipe); Size 0.3 is held to what is reached, so it cannot slip, its margin to 0.02 printed.
        // Grown with the strength channel its cloud's balls and depth across follow (design §12.9 items 1 and 3): read
        // against the recipe's where the recipe stands cloud (over the threshold), held at what is reached (0.0017 at
        // Size 1.5, 0.084 at 0.3, where a node just past a small cloud's rim holds a full strength).
        {
            use crate::cloud_volume_cumulus::{VolumeShape, VolumeSpec, GRID_SIZE, GRID_SPAN_M};
            for (size, held_to, strength_held) in [(None, 0.02f32, 0.002f32), (Some(30.0f32), 0.02, 0.002), (Some(0.3), 0.1, 0.09)] {
                let mut layer = CloudLayerParams::new(genus, &[], 1);
                if let Some(size_km) = size {
                    layer.cumulus = Some(crate::CumulusParams { size_km, ..layer.cumulus_params() });
                }
                let spec = VolumeSpec { layer: &layer, genus, cover: genus.default_cover(), map_seed: 1, downwind: downwind(30.0), sun_dir: dir_from_az_el(200.0, 30.0), march_steps: 48 };
                let shape = VolumeShape::build(&spec, &|| false, &four_threads).unwrap().expect("a Cumulus volume");
                let grid = shape.grid();
                assert_eq!((grid.size(), grid.half_m()), (GRID_SIZE, 0.5 * GRID_SPAN_M));
                let inner = 0.5 * GRID_SPAN_M - 2.0 * grid.cell_m(0);
                let (mut worst, mut sum) = (0.0f32, 0.0f32);
                let (mut strength_worst, mut strength_sum, mut inside) = (0.0f32, 0.0f32, 0usize);
                for k in 0..1000 {
                    let (x, z) = ((2.0 * hash01(k, 0, 0, 0x7a11_5eed) - 1.0) * inner, (2.0 * hash01(k, 1, 0, 0x7a11_5eed) - 1.0) * inner);
                    let (read, want) = (grid.sample(x, z, 0.0), shape.footprint_recipe(x, z));
                    // The warp (metres) in cells of Size.
                    let cell = shape.cell_m();
                    let off = (read[0] - want[0]).abs().max((read[1] - want[1]).abs()).max((read[2] - want[2]).abs() / cell).max((read[3] - want[3]).abs() / cell);
                    worst = worst.max(off);
                    sum += off;
                    if shape.footprint_at(x, z) > 0.0 {
                        let off = (read[4] - want[4]).abs();
                        strength_worst = strength_worst.max(off);
                        strength_sum += off;
                        inside += 1;
                    }
                }
                let size_km = layer.cumulus_params().size_km;
                println!("the volume's Cumulus recipe at Size {size_km} on its grid ({} m cells): off by {worst:.4} at most ({} 0.02), {:.5} on average", grid.cell_m(0), if worst <= 0.02 { "within" } else { "over" }, sum / 1000.0);
                assert!(worst <= held_to, "the volume's grid at Size {size_km} is off its recipe by {worst}");
                println!("  its strength where the recipe stands cloud ({inside} points): off by {strength_worst:.4} at most (held to {strength_held}), {:.5} on average", strength_sum / inside.max(1) as f32);
                assert!(inside > 100 && strength_worst <= strength_held, "the volume's grid at Size {size_km} reads its clouds' strength off by {strength_worst} ({inside} points in cloud)");
            }
        }
    }
}
