//! Non-planar top finish (NONPLANAR-DESIGN): the top shells of a gentle SDF top follow
//! the surface instead of stepping in terraces.
//!
//! [`plan`] works from the part's distance field alone:
//! - `top(x, y)` is the first crossing of the field going down each column;
//! - a column is a candidate when its slope is within the cap;
//! - shell k (k = shells − 1 is the visible one) is the iso-surface
//!   f = −(shells − 1 − k)·layer_h: the surface's normal offset for a field that is exact inside.
//!
//! Connected candidate areas are regions, each printed as one block right after the
//! planar layer at its own highest top. Under a region the planar layers stop at the cap:
//! a planar layer with top z keeps a column only while z ≤ z₁(x, y) − t_min, where z₁ is
//! the lowest shell. [`NpPlan::cap_clip`] is the removed area per layer, traced from that
//! continuous field.
//!
//! Each shell is laid in rings:
//! - the region's outline offset inward by the line width, projected onto the shell
//!   (Z varies along a ring as the surface does);
//! - each ring starts at its lowest point, and the rings print from the lowest up;
//! - the lower shells stay a flat radius inside the outline, so the tip never meets the
//!   planar body beside the region.
//!
//! The head is a set of boxes in the active tip's frame, the same data as the validator's
//! envelope. Every ring is checked against the printed height field as it grows; the own
//! trail of the last [`TRAIL`] mm is ignored, because the tip shapes its own bead there.
//! The rise cap is a box too. A column where a check fails becomes planar, and the plan is
//! rebuilt until no check fails. Margins carry a [`PAD`] over the validator's, so a plan
//! the slicer accepts is not refused on rounding. A plan never contains a failing move:
//! regions that still fail after the iterations are dropped.

use crate::geom::{ExPolygon, Pt};
use crate::sdf::{contour_segments, SdfParams};

/// A part's distance field in printer mm, negative inside.
pub type Field3<'a> = &'a (dyn Fn(f64, f64, f64) -> f64 + Sync);

/// The slicer's extra margin over the validator's (mm). The slicer also stamps beads wider
/// ([`STAMP_HALF`]) and reads boxes with more slack ([`SLACK`]) than the validator, so the
/// validator never sees material the slicer did not.
pub const PAD: f64 = 0.05;
/// A bead marks the cells within its half width + this, a box reads the cells within this of
/// it: at least the validator's half diagonal of its 0.1 mm cell (0.0708).
const SLACK: f64 = 0.08;
/// The own trail the tip may touch (mm of path behind it).
pub const TRAIL: f64 = 3.0;
/// The v1 slope cap may not be set above this.
pub const MAX_SLOPE_CAP: f64 = 20.0;
/// Lower shells start this far inside the region's outline (the tip's flat radius + 0.1);
/// where no lower bead lies, the planar body rises to just under the top shell.
const EDGE: f64 = 0.6;
const CARVE_R: f64 = 1.5;
/// The shortest path segment (mm).
const MIN_SEG: f64 = 0.25;
const ITERATIONS: usize = 10;
/// Columns this far around the part are in the height field (the head's reach).
const MARGIN: f64 = 60.0;

#[derive(Clone, Debug, PartialEq)]
pub struct NpSettings {
    /// Non-planar shells (the top 1-3).
    pub shells: usize,
    pub layer_h: f64,
    pub line_w: f64,
    pub slope_cap_deg: f64,
    /// Most printed material above the tip within `rise_half` (mm).
    pub rise_cap: f64,
    pub rise_half: f64,
    /// Chord tolerance of the paths (mm, vertical).
    pub tol: f64,
    /// The lowest shell's least thickness over the planar body (mm, vertical).
    pub t_min: f64,
    /// Height-field cell (mm).
    pub cell: f64,
}

impl Default for NpSettings {
    fn default() -> Self {
        Self { shells: 2, layer_h: 0.2, line_w: 0.42, slope_cap_deg: 15.0, rise_cap: 3.0, rise_half: 15.0, tol: 0.005, t_min: 0.1, cell: 0.1 }
    }
}

impl NpSettings {
    pub fn check(&self) -> Result<(), String> {
        if !(1..=3).contains(&self.shells) { return Err(format!("non-planar shells {} (1-3)", self.shells)); }
        if !(self.slope_cap_deg > 0.0 && self.slope_cap_deg <= MAX_SLOPE_CAP) { return Err(format!("non-planar slope cap {} deg (0-{MAX_SLOPE_CAP})", self.slope_cap_deg)); }
        if !(self.layer_h > 0.0 && self.line_w > 0.0 && self.tol > 0.0 && self.cell > 0.0 && self.rise_cap >= 0.0 && self.rise_half > 0.0 && self.t_min > 0.0 && self.t_min < self.layer_h) {
            return Err("non-planar settings out of range".into());
        }
        Ok(())
    }
}

/// One part of the head in the active tip's frame: footprint centre and half extents,
/// lowest point above the tip, required clearance (all mm). `hole` (half extent) leaves a
/// square out of the footprint's centre: the cone is a stack of such rings, and the tip zone
/// inside the innermost hole is where the tip forms its bead (not checked).
#[derive(Clone, Debug, PartialEq)]
pub struct NpBox { pub name: String, pub ox: f64, pub oy: f64, pub hx: f64, pub hy: f64, pub hole: f64, pub e: f64, pub margin: f64 }

/// A region's paths: per shell (lowest first), rings in print order, each a closed 3D
/// polyline (first point = last point).
#[derive(Clone, Debug, PartialEq)]
pub struct NpRegion {
    pub id: usize,
    /// The block prints right after the planar layer whose top is the last at or below this.
    pub schedule_z: f64,
    pub lo: [f64; 2],
    pub hi: [f64; 2],
    pub shells: Vec<Vec<Vec<[f64; 3]>>>,
    /// The lowest shell's thickness over the planar body there: (min, max), vertical mm.
    pub first_shell: (f64, f64),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NpStats {
    /// Upward top area (slopes under 75 deg) and the part of it printed non-planar (mm²).
    pub top_area: f64,
    pub np_area: f64,
    pub max_slope_deg: f64,
    pub max_pitch_deg: f64,
    /// The least head clearance over the paths, beyond each box's own margin (mm).
    pub min_clearance: Option<f64>,
    /// Terrace height along the normal (mm): p95 and mean over the whole top, planar vs with non-planar.
    pub terrace_p95_planar: f64,
    pub terrace_p95_np: f64,
    pub terrace_mean_planar: f64,
    pub terrace_mean_np: f64,
    /// Columns turned planar by the head check, by the box that hit.
    pub carved: Vec<(String, usize)>,
    pub iterations: usize,
    pub path_mm: f64,
    pub moves: usize,
}

impl NpStats {
    pub fn coverage(&self) -> f64 { if self.top_area > 0.0 { self.np_area / self.top_area } else { 0.0 } }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NpPlan {
    pub settings: NpSettings,
    pub regions: Vec<NpRegion>,
    pub stats: NpStats,
    x0: f64,
    y0: f64,
    c: f64,
    nx: usize,
    ny: usize,
    /// Region id per column (−1: planar).
    mask: Vec<i32>,
    /// The planar cap z₁ − t_min per column, extended one cell outside the regions.
    cap: Vec<f32>,
    /// What the regions' tops would be printed planar (the before of a before/after): rows
    /// [`GHOST_STEP`] apart of runs at one terrace height, (x0, x1, y, z) in mm.
    pub ghost: Vec<[f32; 4]>,
}

/// Row pitch of [`NpPlan::ghost`] (mm).
pub const GHOST_STEP: f64 = 0.5;

#[derive(Clone)]
struct Grid { x0: f64, y0: f64, c: f64, nx: usize, ny: usize, v: Vec<f32>, coarse: Vec<f32>, coarse2: Vec<f32> }

/// Cells per coarse tile side: a tile holds the max of its cells, an upper bound that answers
/// most box reads without touching the cells.
const TILE: usize = 16;

impl Grid {
    fn new(x0: f64, y0: f64, c: f64, nx: usize, ny: usize, v: Vec<f32>) -> Self {
        let (tx, ty) = (nx.div_ceil(TILE), ny.div_ceil(TILE));
        let mut coarse = vec![f32::MIN; tx * ty];
        let mut coarse2 = vec![f32::MIN; tx.div_ceil(TILE) * ty.div_ceil(TILE)];
        for (k, &h) in v.iter().enumerate() {
            let (i, j) = (k % nx, k / nx);
            let t = (j / TILE) * tx + i / TILE;
            if h > coarse[t] { coarse[t] = h; }
            let t2 = (j / TILE / TILE) * tx.div_ceil(TILE) + i / TILE / TILE;
            if h > coarse2[t2] { coarse2[t2] = h; }
        }
        Grid { x0, y0, c, nx, ny, v, coarse, coarse2 }
    }
    fn set_max(&mut self, k: usize, z: f32) {
        if z > self.v[k] {
            self.v[k] = z;
            let (i, j, tx) = (k % self.nx, k / self.nx, self.nx.div_ceil(TILE));
            let t = (j / TILE) * tx + i / TILE;
            if z > self.coarse[t] { self.coarse[t] = z; }
            let t2 = (j / TILE / TILE) * tx.div_ceil(TILE) + i / TILE / TILE;
            if z > self.coarse2[t2] { self.coarse2[t2] = z; }
        }
    }
    fn ij(&self, x: f64, y: f64) -> Option<usize> {
        let i = ((x - self.x0) / self.c).floor();
        let j = ((y - self.y0) / self.c).floor();
        if i < 0.0 || j < 0.0 || i >= self.nx as f64 || j >= self.ny as f64 { return None; }
        Some(j as usize * self.nx + i as usize)
    }
    fn xy(&self, k: usize) -> (f64, f64) { (self.x0 + ((k % self.nx) as f64 + 0.5) * self.c, self.y0 + ((k / self.nx) as f64 + 0.5) * self.c) }
}

/// First crossing of level `lev` going down the column; sphere tracing is safe on a
/// Lipschitz-1 lower bound, the floor step only matters beside walls.
fn march(f: Field3, x: f64, y: f64, lev: f64, ztop: f64, zbot: f64) -> Option<f64> {
    let (mut z, mut prev) = (ztop, ztop);
    loop {
        let v = f(x, y, z) - lev;
        if v <= 0.0 { break; }
        prev = z;
        z -= v.max(0.005);
        if z < zbot { return None; }
    }
    if z == prev { return None; } // inside at the top: not a top surface
    let (mut lo, mut hi) = (z, prev);
    for _ in 0..60 {
        let m = 0.5 * (lo + hi);
        if f(x, y, m) - lev <= 0.0 { lo = m } else { hi = m }
        if hi - lo < 1e-9 { break; }
    }
    Some(0.5 * (lo + hi))
}

/// The shell at level `lev` over a column: the field's iso-level, or, at the part's edge where
/// the level does not reach the column, the surface offset along its normal.
fn shell_z(f: Field3, x: f64, y: f64, lev: f64, ztop: f64, zbot: f64) -> Option<f64> {
    if lev == 0.0 { return march(f, x, y, 0.0, ztop, zbot); }
    let s = march(f, x, y, 0.0, ztop, zbot)?;
    match march(f, x, y, lev, ztop, zbot) {
        // The iso-level of this top (not of a lower surface further down the column).
        Some(z) if s - z < 3.0 * -lev + 0.5 => Some(z),
        _ => Some(s + lev / normal_z(f, x, y, s).max(0.5)),
    }
}

/// The normal's z by central differences. The step is 10 µm, not less: compiled kernel fields
/// evaluate in f32 (about 8e-6 mm resolution at 80 mm), and a smaller step reads that rounding
/// as the gradient.
fn normal_z(f: Field3, x: f64, y: f64, z: f64) -> f64 {
    let h = 1e-2;
    let g = [f(x + h, y, z) - f(x - h, y, z), f(x, y + h, z) - f(x, y - h, z), f(x, y, z + h) - f(x, y, z - h)];
    g[2] / (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt().max(1e-30)
}

fn label(m: &[bool], nx: usize, ny: usize) -> (Vec<i32>, usize) {
    let mut c = vec![-1i32; m.len()];
    let mut n = 0;
    for k0 in 0..m.len() {
        if !m[k0] || c[k0] >= 0 { continue; }
        let mut stack = vec![k0];
        c[k0] = n as i32;
        while let Some(k) = stack.pop() {
            let (i, j) = ((k % nx) as i64, (k / nx) as i64);
            for dj in -1..=1i64 { for di in -1..=1i64 {
                let (a, b) = (i + di, j + dj);
                if a < 0 || b < 0 || a >= nx as i64 || b >= ny as i64 { continue; }
                let q = b as usize * nx + a as usize;
                if m[q] && c[q] < 0 { c[q] = n as i32; stack.push(q); }
            } }
        }
        n += 1;
    }
    (c, n)
}

/// A separable box blur of radius `r` cells (edges clamp).
fn box_blur(v: &[f32], nx: usize, ny: usize, r: usize) -> Vec<f32> {
    let pass = |src: &[f32], n: usize, stride: usize, lines: usize, lstride: usize| -> Vec<f32> {
        let mut out = src.to_vec();
        for l in 0..lines {
            let at = |i: usize| src[l * lstride + i.min(n - 1) * stride];
            let mut sum: f32 = (0..=2 * r).map(|i| at(i.saturating_sub(r))).sum();
            for i in 0..n {
                out[l * lstride + i * stride] = sum / (2 * r + 1) as f32;
                sum += at(i + r + 1) - at(i.saturating_sub(r));
            }
        }
        out
    };
    let a = pass(v, nx, 1, ny, nx);
    pass(&a, ny, nx, nx, 1)
}

/// Bilinear value at (x, y) of per-cell values (cell centres as nodes).
fn bilinear(x0: f64, y0: f64, c: f64, nx: usize, ny: usize, at: &dyn Fn(usize) -> f64, x: f64, y: f64) -> f64 {
    let fx = ((x - x0) / c - 0.5).clamp(0.0, (nx - 1) as f64);
    let fy = ((y - y0) / c - 0.5).clamp(0.0, (ny - 1) as f64);
    let (i, j) = ((fx.floor() as usize).min(nx.saturating_sub(2)), (fy.floor() as usize).min(ny.saturating_sub(2)));
    let (tx, ty) = (fx - i as f64, fy - j as f64);
    let k = j * nx + i;
    let (a, b, cc, d) = (at(k), at(k + 1), at(k + nx), at(k + nx + 1));
    (a * (1.0 - tx) + b * tx) * (1.0 - ty) + (cc * (1.0 - tx) + d * tx) * ty
}

fn trace(f: &(dyn Fn(f64, f64) -> f64 + Sync), lo: [f64; 2], hi: [f64; 2]) -> Vec<ExPolygon> {
    let p = SdfParams { coarse: 1.0, fine: 0.1, lipschitz: 1.5, window: f64::INFINITY, tolerance: 1e-5, sharp_deg: 90.0 };
    let segs = contour_segments(f, lo, hi, &p);
    crate::slice::segments_to_regions(segs, &crate::slice::SliceParams { closing_radius: 0.0, resolution: 0.002, max_gap: 0.5 })
}

/// The non-planar plan of one part. `head` is the active nozzle's head (boxes); the rise
/// cap is added as a box.
pub fn plan(field: Field3, bounds: ([f64; 3], [f64; 3]), s: &NpSettings, head: &[NpBox]) -> Result<NpPlan, String> {
    s.check()?;
    let (lo, hi) = bounds;
    let c = s.cell;
    let (x0, y0) = (lo[0] - MARGIN, lo[1] - MARGIN);
    let nx = ((hi[0] - lo[0] + 2.0 * MARGIN) / c).ceil() as usize;
    let ny = ((hi[1] - lo[1] + 2.0 * MARGIN) / c).ceil() as usize;
    let (ztop, zbot) = (hi[2] + 1.0, lo[2] - 1.0);
    let ns = s.shells;
    let cap_cos = s.slope_cap_deg.to_radians().cos();
    // Per row: the top surface, its normal's z, the shell levels.
    let rows: Vec<Vec<(f32, f32, Vec<f32>)>> = crate::par::map(ny, |j| {
        (0..nx).map(|i| {
            let (x, y) = (x0 + (i as f64 + 0.5) * c, y0 + (j as f64 + 0.5) * c);
            if x < lo[0] - c || x > hi[0] + c || y < lo[1] - c || y > hi[1] + c { return (f32::NAN, 0.0, vec![]); }
            let Some(z) = march(field, x, y, 0.0, ztop, zbot) else { return (f32::NAN, 0.0, vec![]) };
            let n = normal_z(field, x, y, z);
            let shells = if n >= cap_cos {
                (0..ns).map(|k| if k + 1 == ns { z as f32 } else { shell_z(field, x, y, -((ns - 1 - k) as f64) * s.layer_h, ztop, zbot).map(|v| v as f32).unwrap_or(f32::NAN) }).collect()
            } else { vec![] };
            (z as f32, n as f32, shells)
        }).collect()
    });
    let cells: Vec<(f32, f32, Vec<f32>)> = rows.into_iter().flatten().collect();
    let surf: Vec<f32> = cells.iter().map(|c| c.0).collect();
    let nz: Vec<f32> = cells.iter().map(|c| c.1).collect();
    let zk: Vec<Vec<f32>> = (0..ns).map(|k| cells.iter().map(|c| c.2.get(k).copied().unwrap_or(f32::NAN)).collect()).collect();
    let mut cand: Vec<bool> = (0..nx * ny).map(|k| !surf[k].is_nan() && (0..ns).all(|j| !zk[j][k].is_nan())).collect();
    let mut head: Vec<NpBox> = head.to_vec();
    head.push(NpBox { name: "rise cap".into(), ox: 0.0, oy: 0.0, hx: s.rise_half, hy: s.rise_half, hole: 0.0, e: s.rise_cap, margin: 0.0 });
    let mut carved: Vec<(String, usize)> = Vec::new();
    let mut iterations = 0;
    let (mut regions, mut mask, mut least);
    let mut last_band: Vec<bool>;
    loop {
        iterations += 1;
        let (comp, nc) = label(&cand, nx, ny);
        let mut zs = vec![0f64; nc];
        for k in 0..nx * ny { if comp[k] >= 0 { let r = comp[k] as usize; zs[r] = zs[r].max(surf[k] as f64); } }
        let mut order: Vec<usize> = (0..nc).collect();
        order.sort_by(|a, b| zs[*a].total_cmp(&zs[*b]).then(a.cmp(b)));
        let mut rank = vec![0usize; nc];
        for (r, &i) in order.iter().enumerate() { rank[i] = r; }
        // Later regions' planar body at its highest (an upper bound until they are planned).
        let pl_hi = |k: usize| s.layer_h * ((zk[ns - 1][k] as f64 - s.t_min) / s.layer_h).floor();
        let mut band = vec![false; nx * ny];
        regions = Vec::new();
        least = None::<f64>;
        let mut fails: Vec<(f64, f64, String)> = Vec::new();
        let half = s.line_w / 2.0 + SLACK;
        for &r in &order {
            let zsr = zs[r] + 0.5 * s.layer_h; // planar columns: up to the layer that holds the region's top
            let region_mask: Vec<bool> = comp.iter().map(|&x| x == r as i32).collect();
            let (rlo, rhi) = mask_bounds(&region_mask, x0, y0, c, nx);
            // The outline from the mask's signed values blurred over 7 cells: a smooth curve through
            // the cell edges, not their staircase (which the rings would carry as a zig-zag pitch).
            let sv: Vec<f32> = region_mask.iter().map(|&m| if m { -0.5 * c as f32 } else { 0.5 * c as f32 }).collect();
            let sv = box_blur(&sv, nx, ny, 3);
            let msd = |x: f64, y: f64| bilinear(x0, y0, c, nx, ny, &|k| sv[k] as f64, x, y);
            let outline = trace(&msd, [rlo[0] - 1.0, rlo[1] - 1.0], [rhi[0] + 1.0, rhi[1] + 1.0]);
            let top_xy = ring_polys(&outline, 0.0, s);
            let low_xy = if ns > 1 { ring_polys(&outline, EDGE, s) } else { Vec::new() };
            let top_rings = project(field, &top_xy, &vec![true; top_xy.len()], 0.0, s, ztop, zbot);
            // Every lower shell's projection of every lower polygon, once.
            let lower_all: Vec<Vec<(usize, Vec<[f64; 3]>)>> = (0..ns - 1).map(|sh| project(field, &low_xy, &vec![true; low_xy.len()], -((ns - 1 - sh) as f64) * s.layer_h, s, ztop, zbot)).collect();
            // Lower shells share one set of rings; a lower ring that fails is dropped from all
            // of them, and the planar body under its strip rises to just under the top shell.
            let mut keep = vec![true; low_xy.len()];
            let mut tries = 0;
            let (shells_out, region_band, t1) = loop {
                tries += 1;
                let lower: Vec<Vec<(usize, Vec<[f64; 3]>)>> = lower_all.iter().map(|v| v.iter().filter(|(i, _)| keep[*i]).cloned().collect()).collect();
                // Region columns no lower bead reaches (a cell's half diagonal included).
                let mut covered = Grid::new(x0, y0, c, nx, ny, vec![0.0; nx * ny]);
                if let Some(first) = lower.first() { for (_, ring) in first { stamp(&mut covered, &flat(ring), s.line_w / 2.0 + 0.71 * c); } }
                let rband: Vec<bool> = (0..nx * ny).map(|k| region_mask[k] && (ns == 1 || covered.v[k] <= 0.0)).collect();
                let pl = |k: usize| s.layer_h * (((if rband[k] || ns == 1 { zk[ns - 1][k] } else { zk[0][k] }) as f64 - s.t_min) / s.layer_h).floor();
                // The printed height field when the block starts.
                let mut base = Grid::new(x0, y0, c, nx, ny, vec![0.0; nx * ny]);
                let mut t1 = (f64::MAX, 0f64);
                for k in 0..nx * ny {
                    if surf[k].is_nan() { continue; }
                    let ck = comp[k];
                    base.v[k] = if ck == r as i32 {
                        let p = pl(k);
                        let t = (if rband[k] || ns == 1 { zk[ns - 1][k] } else { zk[0][k] }) as f64 - p;
                        t1 = (t1.0.min(t), t1.1.max(t));
                        p
                    }
                    else if ck >= 0 && rank[ck as usize] < rank[r] { surf[k] as f64 }
                    else if ck >= 0 { pl_hi(k).min(zsr) }
                    else { (surf[k] as f64 + 0.5 * s.layer_h).min(zsr) } as f32;
                }
                let base = Grid::new(x0, y0, c, nx, ny, base.v);
                let mut shells: Vec<Vec<(usize, Vec<[f64; 3]>)>> = lower;
                shells.push(top_rings.clone());
                let mut h_minus = base;
                let mut dropped = false;
                let mut top_fails: Vec<(f64, f64, String)> = Vec::new();
                let mut least_r = None::<f64>;
                for (sh, rings) in shells.iter().enumerate() {
                    let is_top = sh + 1 == ns;
                    // Big boxes see the whole shell (conservative); small ones see what is printed so far.
                    let mut h_plus = h_minus.clone();
                    for (_, ring) in rings { stamp(&mut h_plus, ring, half); }
                    let mut live = h_minus.clone();
                    for (idx, ring) in rings {
                        let mut trail: std::collections::VecDeque<([f64; 3], [f64; 3], f64)> = Default::default();
                        let mut trail_len = 0.0;
                        let mut ring_fails = Vec::new();
                        for w in ring.windows(2) {
                            let (a, b) = (w[0], w[1]);
                            let l = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
                            let n = (l / 0.25).ceil().max(1.0) as usize;
                            for i in 0..=n {
                                let t = i as f64 / n as f64;
                                let p = [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]), a[2] + t * (b[2] - a[2])];
                                for bx in head.iter() {
                                    let limit = p[2] + bx.e - bx.margin - if bx.margin > 0.0 { PAD } else { 0.0 };
                                    // Big boxes see the whole shell (conservative); small ones what is printed so far.
                                    let top = if bx.hx.max(bx.hy) > 5.0 { read_box(&h_plus, bx, p[0], p[1], limit) } else { read_box(&live, bx, p[0], p[1], limit) };
                                    let clear = p[2] + bx.e - top - bx.margin - if bx.margin > 0.0 { PAD } else { 0.0 };
                                    if bx.margin > 0.0 { least_r = Some(least_r.map_or(clear, |m: f64| m.min(clear))); }
                                    if clear < 0.0 {
                                        ring_fails.push((p[0], p[1], bx.name.clone()));
                                    }
                                }
                            }
                            // The own trail joins the field once the tip is TRAIL behind it.
                            trail.push_back((a, b, l));
                            trail_len += l;
                            while trail_len > TRAIL { let (ta, tb, tl) = trail.pop_front().unwrap(); stamp(&mut live, &[ta, tb], half); trail_len -= tl; }
                        }
                        for (ta, tb, _) in trail { stamp(&mut live, &[ta, tb], half); }
                        if !ring_fails.is_empty() {
                            if is_top { top_fails.extend(ring_fails); } else { keep[*idx] = false; dropped = true; }
                        }
                    }
                    h_minus = h_plus;
                }
                if !dropped || tries >= 40 {
                    if dropped { top_fails.push((rlo[0], rlo[1], "lower shell".into())); }
                    fails.extend(top_fails);
                    least = match (least, least_r) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) };
                    break (shells.into_iter().map(|v| v.into_iter().map(|x| x.1).collect()).collect::<Vec<Vec<Vec<[f64; 3]>>>>(), rband, t1);
                }
            };
            for k in 0..nx * ny { if region_band[k] { band[k] = true; } }
            regions.push(NpRegion { id: regions.len(), schedule_z: zs[r], lo: rlo, hi: rhi, shells: shells_out, first_shell: t1 });
        }
        mask = comp;
        last_band = band;
        if fails.is_empty() { break; }
        for (x, y, name) in &fails {
            match carved.iter_mut().find(|c| &c.0 == name) { Some(c) => c.1 += 1, None => carved.push((name.clone(), 1)) }
            let rr = (CARVE_R / c).ceil() as i64;
            let (ci, cj) = (((x - x0) / c).floor() as i64, ((y - y0) / c).floor() as i64);
            for dj in -rr..=rr { for di in -rr..=rr {
                let (i, j) = (ci + di, cj + dj);
                if i < 0 || j < 0 || i >= nx as i64 || j >= ny as i64 { continue; }
                if ((di * di + dj * dj) as f64).sqrt() * c <= CARVE_R { cand[j as usize * nx + i as usize] = false; }
            } }
        }
        if iterations >= ITERATIONS {
            // Never hand out a failing move: no non-planar region at all.
            regions.clear();
            mask = vec![-1; nx * ny];
            last_band = vec![false; nx * ny];
            break;
        }
    }
    // The planar cap per column, extended one cell out so the clip's edge stays inside the region's own values.
    let mut cap = vec![f32::NAN; nx * ny];
    for k in 0..nx * ny { if mask[k] >= 0 { cap[k] = ((if last_band[k] { zk[ns - 1][k] } else { zk[0][k] }) as f64 - s.t_min) as f32; } }
    let inner = cap.clone();
    for k in 0..nx * ny {
        if !inner[k].is_nan() { continue; }
        let (i, j) = ((k % nx) as i64, (k / nx) as i64);
        let mut v = f32::NAN;
        for dj in -1..=1i64 { for di in -1..=1i64 {
            let (a, b) = (i + di, j + dj);
            if a < 0 || b < 0 || a >= nx as i64 || b >= ny as i64 { continue; }
            let q = inner[b as usize * nx + a as usize];
            if !q.is_nan() && (v.is_nan() || q < v) { v = q; }
        } }
        cap[k] = v;
    }
    let stats = stats(field, s, &regions, &mask, &surf, &nz, c, carved, iterations, least);
    // The planar terraces over the regions, as the midplane rule prints them.
    let mut ghost = Vec::new();
    let step = ((GHOST_STEP / c).round() as usize).max(1);
    let stair = |z: f32| (s.layer_h * (z as f64 / s.layer_h + 0.5).floor()) as f32;
    for j in (0..ny).step_by(step) {
        let mut run: Option<(usize, f32)> = None;
        for i in 0..=nx {
            let k = j * nx + i;
            let z = (i < nx && mask[k] >= 0 && !surf[k].is_nan()).then(|| stair(surf[k]));
            match (run, z) {
                (Some((_, z0)), Some(z1)) if (z1 - z0).abs() < 1e-4 => {}
                (r, z1) => {
                    if let Some((i0, z0)) = r {
                        let x = |i: usize| (x0 + i as f64 * c) as f32;
                        ghost.push([x(i0), x(i), (y0 + (j as f64 + 0.5) * c) as f32, z0]);
                    }
                    run = z1.map(|z| (i, z));
                }
            }
        }
    }
    Ok(NpPlan { settings: s.clone(), regions, stats, x0, y0, c, nx, ny, mask, cap, ghost })
}

fn mask_bounds(m: &[bool], x0: f64, y0: f64, c: f64, nx: usize) -> ([f64; 2], [f64; 2]) {
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for (k, _) in m.iter().enumerate().filter(|(_, &b)| b) {
        let (x, y) = (x0 + ((k % nx) as f64 + 0.5) * c, y0 + ((k / nx) as f64 + 0.5) * c);
        lo = [lo[0].min(x - c), lo[1].min(y - c)];
        hi = [hi[0].max(x + c), hi[1].max(y + c)];
    }
    (lo, hi)
}

/// An upper bound of the highest printed point over a box at one tile level (1: tiles of
/// TILE cells, 2: of TILE² cells).
fn tile_max(g: &Grid, level: u32, xa: f64, ya: f64, xb: f64, yb: f64) -> f64 {
    let f = TILE.pow(level);
    let t = g.c * f as f64;
    let (tx, ty) = (g.nx.div_ceil(f), g.ny.div_ceil(f));
    let tiles = if level == 1 { &g.coarse } else { &g.coarse2 };
    let i0 = (((xa - SLACK - g.x0) / t).floor().max(0.0)) as usize;
    let j0 = (((ya - SLACK - g.y0) / t).floor().max(0.0)) as usize;
    let i1 = ((((xb + SLACK - g.x0) / t).floor()) as i64).min(tx as i64 - 1);
    let j1 = ((((yb + SLACK - g.y0) / t).floor()) as i64).min(ty as i64 - 1);
    if i1 < i0 as i64 || j1 < j0 as i64 { return 0.0; }
    let mut m = 0f32;
    for j in j0..=j1 as usize { for i in i0..=i1 as usize { m = m.max(tiles[j * tx + i]); } }
    m as f64
}

/// The highest printed point over a box less its hole, read as cheaply as `limit` allows: a
/// bound at or under `limit` is returned as is (it passes either way); big boxes stop at the
/// tile level (conservative), small ones go down to the cells.
fn read_box(g: &Grid, b: &NpBox, x: f64, y: f64, limit: f64) -> f64 {
    let (xa, ya, xb, yb) = (x + b.ox - b.hx, y + b.oy - b.hy, x + b.ox + b.hx, y + b.oy + b.hy);
    let l2 = tile_max(g, 2, xa, ya, xb, yb);
    if l2 <= limit { return l2; }
    let l1 = tile_max(g, 1, xa, ya, xb, yb);
    if l1 <= limit || b.hx.max(b.hy) > 5.0 { return l1; }
    ring_max(g, x + b.ox, y + b.oy, b.hx, b.hy, b.hole)
}

/// The highest printed point over a box less its central hole.
fn ring_max(g: &Grid, cx: f64, cy: f64, hx: f64, hy: f64, hole: f64) -> f64 {
    if hole <= 0.0 { return box_max(g, cx - hx, cy - hy, cx + hx, cy + hy); }
    // The four bands around the hole (the hole's edge itself counts as outside it).
    let h = hole.min(hx).min(hy);
    box_max(g, cx - hx, cy - hy, cx + hx, cy - h)
        .max(box_max(g, cx - hx, cy + h, cx + hx, cy + hy))
        .max(box_max(g, cx - hx, cy - h, cx - h, cy + h))
        .max(box_max(g, cx + h, cy - h, cx + hx, cy + h))
}

fn box_max(g: &Grid, xa: f64, ya: f64, xb: f64, yb: f64) -> f64 {
    let c = g.c;
    let i0 = (((xa - SLACK - g.x0) / c).floor().max(0.0)) as usize;
    let j0 = (((ya - SLACK - g.y0) / c).floor().max(0.0)) as usize;
    let i1 = ((((xb + SLACK - g.x0) / c).floor()) as i64).min(g.nx as i64 - 1);
    let j1 = ((((yb + SLACK - g.y0) / c).floor()) as i64).min(g.ny as i64 - 1);
    if i1 < i0 as i64 || j1 < j0 as i64 { return 0.0; }
    let mut m = 0f32;
    for j in j0..=j1 as usize { for i in i0..=i1 as usize { m = m.max(g.v[j * g.nx + i]); } }
    m as f64
}

/// Raises the columns under a 3D polyline (bead top = nozzle z) within `half` + a cell, at
/// the height a cell further along (never under-stated on a slope).
fn stamp(g: &mut Grid, line: &[[f64; 3]], half: f64) {
    let r = half;
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let ab = [b[0] - a[0], b[1] - a[1]];
        let ll = ab[0] * ab[0] + ab[1] * ab[1];
        let slope = if ll > 1e-12 { (b[2] - a[2]).abs() / ll.sqrt() } else { 0.0 };
        let i0 = (((a[0].min(b[0]) - r - g.x0) / g.c).floor().max(0.0)) as usize;
        let j0 = (((a[1].min(b[1]) - r - g.y0) / g.c).floor().max(0.0)) as usize;
        let i1 = ((((a[0].max(b[0]) + r - g.x0) / g.c).ceil()) as usize).min(g.nx - 1);
        let j1 = ((((a[1].max(b[1]) + r - g.y0) / g.c).ceil()) as usize).min(g.ny - 1);
        for j in j0..=j1 { for i in i0..=i1 {
            let k = j * g.nx + i;
            let (x, y) = g.xy(k);
            let t = if ll > 1e-12 { (((x - a[0]) * ab[0] + (y - a[1]) * ab[1]) / ll).clamp(0.0, 1.0) } else { 0.0 };
            if (x - a[0] - t * ab[0]).hypot(y - a[1] - t * ab[1]) > r { continue; }
            let z = (a[2] + t * (b[2] - a[2]) + slope * g.c).min(a[2].max(b[2])) as f32;
            g.set_max(k, z);
        } }
    }
}

/// The outline offset inward by `inset` + w/2, + w, ...: the rings' XY polygons.
fn ring_polys(outline: &[ExPolygon], inset: f64, s: &NpSettings) -> Vec<Vec<Pt>> {
    // Each ring offset from the one before (a small offset of a small polygon), simplified at
    // 0.01 mm (the outline is already smooth).
    let tol = crate::geom::scaled(0.01) as f64;
    let simplify = |v: Vec<ExPolygon>| -> Vec<ExPolygon> {
        v.into_iter().map(|e| ExPolygon {
            contour: crate::geom::Polygon::new(crate::geom::simplify_ring(&e.contour.pts, tol)),
            holes: e.holes.into_iter().map(|h| crate::geom::Polygon::new(crate::geom::simplify_ring(&h.pts, tol))).collect(),
        }).filter(|e| e.contour.pts.len() >= 3).collect()
    };
    let mut polys: Vec<Vec<Pt>> = Vec::new();
    let mut cur = simplify(crate::offset::offset_mm(&simplify(outline.to_vec()), -(inset + s.line_w / 2.0)));
    let mut n = 0;
    while !cur.is_empty() && n < 10_000 {
        for ex in &cur { for ring in ex.rings() { if ring.pts.len() >= 3 { polys.push(ring.pts.clone()); } } }
        cur = simplify(crate::offset::offset_mm(&cur, -s.line_w));
        n += 1;
    }
    polys
}

/// The kept polygons projected onto level `lev`, resampled to the chord tolerance, each
/// starting at its lowest point, ordered from the lowest ring up: (polygon index, ring).
fn project(field: Field3, polys: &[Vec<Pt>], keep: &[bool], lev: f64, s: &NpSettings, ztop: f64, zbot: f64) -> Vec<(usize, Vec<[f64; 3]>)> {
    let zt = |x: f64, y: f64| shell_z(field, x, y, lev, ztop, zbot);
    let mut out: Vec<(usize, Vec<[f64; 3]>)> = Vec::new();
    for (idx, pts) in polys.iter().enumerate() {
        if !keep[idx] { continue; }
        // Vertices at least MIN_SEG apart: the G-code's 3-decimal words on shorter segments turn
        // into a jittering pitch (Z jerk).
        let mut xy: Vec<(f64, f64)> = Vec::with_capacity(pts.len() + 1);
        for p in pts {
            let q = (p.x_mm(), p.y_mm());
            if xy.last().is_none_or(|l: &(f64, f64)| (q.0 - l.0).hypot(q.1 - l.1) >= MIN_SEG) { xy.push(q); }
        }
        while xy.len() > 3 && (xy[xy.len() - 1].0 - xy[0].0).hypot(xy[xy.len() - 1].1 - xy[0].1) < MIN_SEG { xy.pop(); }
        if xy.len() < 3 { continue; }
        xy.push(xy[0]);
        let mut ring: Vec<[f64; 3]> = Vec::new();
        let mut ok = true;
        for w in xy.windows(2) {
            let (a, b) = (w[0], w[1]);
            let l = (b.0 - a.0).hypot(b.1 - a.1);
            let n = l.ceil().max(1.0) as usize;
            let mut prev: Option<[f64; 3]> = None;
            for i in 0..=n {
                let t = i as f64 / n as f64;
                let (x, y) = (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
                let Some(z) = zt(x, y) else { ok = false; break };
                let p = [x, y, z];
                if let Some(q) = prev { refine(&zt, q, p, &mut ring, s.tol, 0); } else if ring.is_empty() { ring.push(p); }
                prev = Some(p);
            }
            if !ok { break; }
        }
        if !ok || ring.len() < 4 { continue; }
        ring.pop();
        let low = (0..ring.len()).min_by(|&a, &b| ring[a][2].total_cmp(&ring[b][2])).unwrap_or(0);
        ring.rotate_left(low);
        let first = ring[0];
        ring.push(first);
        out.push((idx, ring));
    }
    out.sort_by(|a, b| {
        let ma = a.1.iter().map(|p| p[2]).fold(f64::MAX, f64::min);
        let mb = b.1.iter().map(|p| p[2]).fold(f64::MAX, f64::min);
        ma.total_cmp(&mb).then(a.0.cmp(&b.0))
    });
    out
}

/// A ring at z = 1 everywhere (for coverage stamping).
fn flat(ring: &[[f64; 3]]) -> Vec<[f64; 3]> { ring.iter().map(|p| [p[0], p[1], 1.0]).collect() }

fn refine(zt: &dyn Fn(f64, f64) -> Option<f64>, a: [f64; 3], b: [f64; 3], out: &mut Vec<[f64; 3]>, tol: f64, depth: u32) {
    let len = (b[0] - a[0]).hypot(b[1] - a[1]);
    if depth < 16 && len > 2.0 * MIN_SEG {
        let (mx, my) = (0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]));
        if let Some(zm) = zt(mx, my) {
            if (zm - 0.5 * (a[2] + b[2])).abs() > tol || len > 2.0 {
                let m = [mx, my, zm];
                refine(zt, a, m, out, tol, depth + 1);
                refine(zt, m, b, out, tol, depth + 1);
                return;
            }
        }
    }
    out.push(b);
}

#[allow(clippy::too_many_arguments)]
fn stats(field: Field3, s: &NpSettings, regions: &[NpRegion], mask: &[i32], surf: &[f32], nz: &[f32], c: f64,
    carved: Vec<(String, usize)>, iterations: usize, least: Option<f64>) -> NpStats {
    // The visible shell's chord error, measured at segment midpoints; path length, moves, pitch.
    let (mut dev_sum, mut dev_n) = (0.0, 0usize);
    let (mut path, mut moves, mut pitch) = (0.0, 0usize, 0f64);
    for r in regions { for (sh, rings) in r.shells.iter().enumerate() { for ring in rings { for w in ring.windows(2) {
        let (a, b) = (w[0], w[1]);
        let lxy = (b[0] - a[0]).hypot(b[1] - a[1]);
        path += (lxy * lxy + (b[2] - a[2]).powi(2)).sqrt();
        moves += 1;
        pitch = pitch.max((b[2] - a[2]).abs().atan2(lxy).to_degrees());
        if sh + 1 == r.shells.len() {
            let (x, y, zl) = (0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]), 0.5 * (a[2] + b[2]));
            if let Some(z) = march(field, x, y, 0.0, zl + 2.0, zl - 2.0) { dev_sum += (zl - z).abs(); dev_n += 1; }
        }
    } } } }
    let np_err = if dev_n > 0 { dev_sum / dev_n as f64 } else { 0.0 };
    // Terrace height along the normal per top column: planar (midplane layers) vs with the shells.
    let h = s.layer_h;
    let stair = |z: f64| h * (z / h + 0.5).floor();
    let (mut top, mut np, mut maxs) = (0.0, 0.0, 0f64);
    let (mut pl, mut nq): (Vec<(f64, f64)>, Vec<(f64, f64)>) = (vec![], vec![]);
    for k in 0..surf.len() {
        if surf[k].is_nan() { continue; }
        let n = nz[k] as f64;
        if n < 75f64.to_radians().cos() { continue; }
        let a = c * c / n;
        top += a;
        let e = (surf[k] as f64 - stair(surf[k] as f64)).abs() * n;
        pl.push((e, a));
        if mask[k] >= 0 { np += a; maxs = maxs.max(n.acos().to_degrees()); nq.push((np_err, a)); } else { nq.push((e, a)); }
    }
    let p95 = |mut v: Vec<(f64, f64)>| -> f64 {
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        let tot: f64 = v.iter().map(|x| x.1).sum();
        let mut acc = 0.0;
        for x in &v { acc += x.1; if acc >= 0.95 * tot { return x.0; } }
        0.0
    };
    let mean = |v: &[(f64, f64)]| { let tot: f64 = v.iter().map(|x| x.1).sum(); if tot > 0.0 { v.iter().map(|x| x.0 * x.1).sum::<f64>() / tot } else { 0.0 } };
    NpStats {
        top_area: top,
        np_area: np,
        max_slope_deg: maxs,
        max_pitch_deg: pitch,
        min_clearance: least,
        terrace_mean_planar: mean(&pl),
        terrace_mean_np: mean(&nq),
        terrace_p95_planar: p95(pl),
        terrace_p95_np: p95(nq),
        carved,
        iterations,
        path_mm: path,
        moves,
    }
}

impl NpPlan {
    /// The area a planar layer with top `z` must leave out (its columns are under the shells).
    /// It depends only on the plan and `z`.
    pub fn cap_clip(&self, z: f64) -> Vec<ExPolygon> {
        if self.regions.is_empty() { return Vec::new(); }
        let (x0, y0, c, nx, ny) = (self.x0, self.y0, self.c, self.nx, self.ny);
        let mask = &self.mask;
        let cap = &self.cap;
        let g = |x: f64, y: f64| {
            let m = bilinear(x0, y0, c, nx, ny, &|k| if mask[k] >= 0 { -0.5 * c } else { 0.5 * c }, x, y);
            let h = bilinear(x0, y0, c, nx, ny, &|k| if cap[k].is_nan() { -1e3 } else { cap[k] as f64 }, x, y);
            m.max(h - z)
        };
        let (lo, hi) = self.regions.iter().fold(([f64::MAX; 2], [f64::MIN; 2]), |a, r| ([a.0[0].min(r.lo[0]), a.0[1].min(r.lo[1])], [a.1[0].max(r.hi[0]), a.1[1].max(r.hi[1])]));
        trace(&g, [lo[0] - 1.0, lo[1] - 1.0], [hi[0] + 1.0, hi[1] + 1.0])
    }

    /// The regions whose head reach (60 mm) meets the box: what an edit there re-plans.
    pub fn regions_meeting(&self, lo: [f64; 2], hi: [f64; 2]) -> Vec<usize> {
        self.regions.iter().filter(|r| r.lo[0] - MARGIN <= hi[0] && r.hi[0] + MARGIN >= lo[0] && r.lo[1] - MARGIN <= hi[1] && r.hi[1] + MARGIN >= lo[1]).map(|r| r.id).collect()
    }

    /// The planar body's cap under a column (the highest planar layer top it may keep), or None
    /// outside the regions.
    pub fn cap_at(&self, x: f64, y: f64) -> Option<f64> {
        let k = Grid { x0: self.x0, y0: self.y0, c: self.c, nx: self.nx, ny: self.ny, v: vec![], coarse: vec![], coarse2: vec![] }.ij(x, y)?;
        if self.mask[k] < 0 { return None; }
        let v = self.cap[k];
        (!v.is_nan()).then_some(v as f64)
    }

    /// The region id of a column (None: planar).
    pub fn region_at(&self, x: f64, y: f64) -> Option<usize> {
        let g = Grid { x0: self.x0, y0: self.y0, c: self.c, nx: self.nx, ny: self.ny, v: vec![], coarse: vec![], coarse2: vec![] };
        g.ij(x, y).and_then(|k| usize::try_from(self.mask[k]).ok())
    }
}

#[cfg(test)]
mod tests;
