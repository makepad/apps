//! Where a part may be lightened, and the cuts that do it, from one solved study.
//!
//! The rule is stress-driven removal: material whose smoothed utilisation is a small share of
//! the part's highest goes first. This is evolutionary structural optimisation's rejection
//! criterion (Xie & Steven 1993), taken here in one step to the cuts a person would make: holes
//! and pockets in the part's plates. Each plate gets a 2D mask of where it may be cut through,
//! or into from either face, and the cuts are fitted into the masks ([`super::fit`]).
//!
//! What may never be cut (hard rules):
//! - near the study's fixtures and loads (their surface, within [`Rules::keep`]): a cut keeps
//!   off a held or loaded face and the bolt holes a fixture holds;
//! - a web between cuts, and between a cut and the part's outline or an existing hole, of at
//!   least [`Rules::web`]: the nozzle rule (a multiple of its width) or two wall loops each side,
//!   whichever is more, so a web prints as solid lines;
//! - a pocket's floor at least as thick as the web;
//! - a pocket that opens down (towards the bed) is a bridge: no wider than [`Rules::bridge`],
//!   wider ones split into slots;
//! - a hole or pocket across the build direction gets a 45-degree roof ([`super::cuts`]).

use super::cuts::{needs_roof, Cut};
use super::fit::{self, Mask};
use super::plates::{plates, uv_of, Plate};
use crate::bc::Condition;
use crate::material::{Materials, Print};
use crate::study::Model;
use crate::V3;

/// The printability and safety rules a plan keeps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rules {
    /// The nozzle's width and the multiple of it a wall or web is at least (mm).
    pub nozzle: f64,
    pub wall_k: f64,
    /// How far a cut keeps from a fixture's or a load's surface (mm).
    pub keep: f64,
    /// A pocket's corner radius (mm).
    pub corner: f64,
    /// The widest a pocket that opens down may be (a bridge, mm).
    pub bridge: f64,
    /// The smallest and largest hole radius (mm).
    pub r_min: f64,
    pub r_max: f64,
    /// At most this many rows of holes in one plate.
    pub rows: usize,
}

impl Default for Rules {
    fn default() -> Self {
        Rules { nozzle: 0.4, wall_k: 2.0, keep: 2.0, corner: 2.0, bridge: 10.0, r_min: 1.5, r_max: 12.0, rows: 2 }
    }
}

impl Rules {
    /// The thinnest web or floor a cut leaves: the nozzle rule, or two wall loops each side.
    pub fn web(&self, print: &Print, as_printed: bool) -> f64 {
        let walls = if as_printed { 2.0 * print.wall_thickness() } else { 0.0 };
        (self.wall_k * self.nozzle).max(walls)
    }
}

/// What a plan is made from: a part's model at one cell size (with its samples: not shed), its
/// solved results, the study's conditions on the model's surface, and the materials its model
/// was graded with (the print settings and zones the masses follow).
pub struct Ground<'a> {
    pub model: &'a Model,
    pub results: &'a crate::results::Results,
    pub conditions: &'a [Condition],
    pub materials: &'a Materials,
}

/// One plate's masks: cells across it (from `lo`, `w` by `h`) it may be cut through, or into
/// from its face at the high or low end of its axis.
#[derive(Clone, Debug)]
pub struct PlateMasks {
    pub plate: Plate,
    pub lo: [usize; 2],
    /// Its faces along the axis (mm, the columns' medians).
    pub faces: [f64; 2],
    pub through: Mask,
    pub from_hi: Mask,
    pub from_lo: Mask,
    /// The plastic its median column holds per mm² across it (mm), for what a hole's walls cost.
    pub plastic: f64,
}

/// A part's lightening ground: its plates' masks and the rules they were cut with.
#[derive(Clone, Debug)]
pub struct Analysis {
    pub plates: Vec<PlateMasks>,
    pub cell: f64,
    pub origin: V3,
    pub rules: Rules,
    pub web: f64,
    /// The equivalent thickness of a hole's edge walls per mm² of plate (mm), see
    /// [`fit::best_row`].
    pub wall: f64,
    pub density: f64,
    pub as_printed: bool,
}

/// The cells the study's conditions touch, as points (mm).
fn condition_points(g: &Ground) -> Vec<V3> {
    let s = &g.model.surface;
    g.conditions.iter().flat_map(|c| c.region.faces.iter().map(|&q| s.quad_center[q as usize].map(|v| v as f64))).collect()
}

/// Analyses where the part may be cut: cells whose smoothed utilisation is under `share` of the
/// part's highest.
pub fn analyse(g: &Ground, rules: &Rules, share: f64) -> Analysis {
    let grid = &g.model.grid;
    let h = grid.h[0];
    let fill = super::field::fills(g.results);
    let util = super::field::utilisation(g.results);
    let smooth = super::field::smoothed(grid, &util, &fill, 2.5 * h);
    let top = smooth.iter().copied().fold(0.0f32, f32::max) as f64;
    let limit = share * top;
    let (print, as_printed) = (&g.materials.print, g.materials.as_printed);
    let web = rules.web(print, as_printed);
    let marks = condition_points(g);
    let mut out = Vec::new();
    let lattice = g.model.lattice.as_deref().expect("a plan's model keeps its samples (a shed one is the Force study's own)");
    for plate in plates(grid, lattice, 6) {
        let [ua, va] = plate.uv;
        let (lo, hi) = plate.extent();
        let (w, hgt) = (hi[0] - lo[0], hi[1] - lo[1]);
        let axis = plate.axis;
        let median = |k: usize| {
            let mut v: Vec<f64> = plate.columns.iter().map(|c| c.faces[k]).collect();
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let faces = [median(0), median(1)];
        let floor = web;
        // per cell across the plate: the lowest and highest condition point within `keep` of
        // it across the plate and of the plate's slab along it
        let mut lowest = vec![f64::INFINITY; w * hgt];
        let mut highest = vec![f64::NEG_INFINITY; w * hgt];
        let reach = (rules.keep / h).ceil() as isize + 1;
        for p in &marks {
            if p[axis] < faces[0] - rules.keep || p[axis] > faces[1] + rules.keep {
                continue;
            }
            let ci = ((p[ua] - grid.origin[ua]) / h).floor() as isize - lo[0] as isize;
            let cj = ((p[va] - grid.origin[va]) / h).floor() as isize - lo[1] as isize;
            for dj in -reach..=reach {
                for di in -reach..=reach {
                    let (i, j) = (ci + di, cj + dj);
                    if i < 0 || j < 0 || i >= w as isize || j >= hgt as isize {
                        continue;
                    }
                    let centre = [grid.origin[ua] + (lo[0] as f64 + i as f64 + 0.5) * h, grid.origin[va] + (lo[1] as f64 + j as f64 + 0.5) * h];
                    let d = ((centre[0] - p[ua]).powi(2) + (centre[1] - p[va]).powi(2)).sqrt() - 0.5 * h * std::f64::consts::SQRT_2;
                    if d > rules.keep {
                        continue;
                    }
                    let k = i as usize + w * j as usize;
                    lowest[k] = lowest[k].min(p[axis]);
                    highest[k] = highest[k].max(p[axis]);
                }
            }
        }
        let mut through = Mask::new(w, hgt);
        let mut from_hi = Mask::new(w, hgt);
        let mut from_lo = Mask::new(w, hgt);
        let mut plastic = Vec::new();
        for col in &plate.columns {
            // a column the plate's full thickness (no step, boss or existing hole in it)
            if ((col.faces[1] - col.faces[0]) - (faces[1] - faces[0])).abs() > 0.5 * h {
                continue;
            }
            let (i, j) = (col.uv[0] - lo[0], col.uv[1] - lo[1]);
            let k = i + w * j;
            let cell_at = |t: usize| {
                let mut p = [0; 3];
                p[axis] = t;
                p[ua] = col.uv[0];
                p[va] = col.uv[1];
                grid.cell(p[0], p[1], p[2])
            };
            let centre_t = |t: usize| grid.origin[axis] + (t as f64 + 0.5) * h;
            let busy = |t0: f64, t1: f64| (col.span[0]..col.span[1]).filter(|&t| centre_t(t) >= t0 - 0.5 * h && centre_t(t) <= t1 + 0.5 * h).any(|t| smooth[cell_at(t)] as f64 >= limit);
            plastic.push((col.span[0]..col.span[1]).map(|t| super::mass::cell_plastic(grid, cell_at(t), g.materials)).sum::<f64>() / (h * h));
            let (low, high) = (lowest[k], highest[k]);
            let clear = low.is_infinite();
            if clear && !busy(faces[0], faces[1]) {
                through.set(i, j, true);
            }
            // into the high face, a floor left at the low one: no condition above the floor (the
            // floor is the margin to a held or loaded face under it)
            let floor_hi = faces[0] + floor;
            if (clear || high < floor_hi) && !busy(floor_hi, faces[1]) {
                from_hi.set(i, j, true);
            }
            let floor_lo = faces[1] - floor;
            if (clear || low > floor_lo) && !busy(faces[0], floor_lo) {
                from_lo.set(i, j, true);
            }
        }
        plastic.sort_by(f64::total_cmp);
        let plastic = plastic.get(plastic.len() / 2).copied().unwrap_or(0.0);
        out.push(PlateMasks { plate, lo, faces, through, from_hi, from_lo, plastic });
    }
    Analysis { plates: out, cell: h, origin: grid.origin, rules: *rules, web, wall: if as_printed { print.wall_thickness() } else { 0.0 }, density: print.density, as_printed }
}

impl Analysis {
    /// A point across plate `p` (mm from its mask's corner) and along its axis at `t`, in the
    /// printer frame.
    fn point(&self, p: &PlateMasks, uv: [f64; 2], t: f64) -> V3 {
        let [ua, va] = uv_of(p.plate.axis);
        let mut out = [0.0; 3];
        out[ua] = self.origin[ua] + p.lo[0] as f64 * self.cell + uv[0];
        out[va] = self.origin[va] + p.lo[1] as f64 * self.cell + uv[1];
        out[p.plate.axis] = t;
        out
    }

    fn unit(axis: usize, sign: f64) -> V3 {
        let mut v = [0.0; 3];
        v[axis] = sign;
        v
    }

    /// Rows of through holes, their radii `scale` (0..1] of the most each row's band takes.
    pub fn holes(&self, scale: f64) -> Vec<Cut> {
        let mut out = Vec::new();
        for p in &self.plates {
            let thickness = p.faces[1] - p.faces[0];
            // a hole's walls cost their perimeter times the wall thickness through the plate,
            // against the plastic it takes away
            let edge = if self.as_printed && p.plastic > 0.0 { self.wall * thickness * (1.0 - self.density) / p.plastic } else { 0.0 };
            let axis = Self::unit(p.plate.axis, 1.0);
            let teardrop = needs_roof(axis);
            // a teardrop fits by its enclosing circle, (1 + sqrt 2) / 2 r about a centre
            // (sqrt 2 - 1) / 2 r above the hole's (up across a level axis is the plate's v)
            let (grow, lift) = if teardrop { (0.5 * (1.0 + std::f64::consts::SQRT_2), 0.5 * (std::f64::consts::SQRT_2 - 1.0)) } else { (1.0, 0.0) };
            let mut mask = p.through.clone();
            for _ in 0..self.rules.rows {
                let clear = fit::clearance(&mask, self.cell, self.web);
                let Some(band) = fit::bands(&mask, &clear, self.cell, self.rules.r_min * grow).into_iter().next() else { break };
                let Some(best) = fit::best_row(&mask, &clear, self.cell, &band, self.rules.r_min, self.rules.r_max, 0.25, self.web, edge, grow) else { break };
                let radius = (best.radius / grow * scale).max(self.rules.r_min);
                let Some(row) = fit::row_of(&mask, &clear, self.cell, &band, radius * grow, self.web) else { break };
                let mid = 0.5 * (p.faces[0] + p.faces[1]);
                let down = |q: [f64; 2]| [q[0], q[1] - lift * radius];
                out.push(Cut::Holes {
                    from: self.point(p, down(row.from), mid),
                    to: self.point(p, down(row.to), mid),
                    count: row.count,
                    diameter: 2.0 * radius,
                    length: thickness + 2.0,
                    axis,
                    teardrop,
                });
                // the row and its webs are taken: the next row fits beside it
                let keep = best.radius + self.web;
                for c in row.centres() {
                    for j in 0..mask.h {
                        for i in 0..mask.w {
                            let q = [(i as f64 + 0.5) * self.cell, (j as f64 + 0.5) * self.cell];
                            if ((q[0] - c[0]).powi(2) + (q[1] - c[1]).powi(2)).sqrt() < keep + 0.5 * self.cell {
                                mask.set(i, j, false);
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Pockets: the largest rectangle each plate may be cut into from one face, `scale` (0..1]
    /// of its size about its centre. A pocket that opens down splits into slots no wider than a
    /// bridge; one across the build direction keeps room above it for its roof.
    pub fn pockets(&self, scale: f64) -> Vec<Cut> {
        let mut out = Vec::new();
        for p in &self.plates {
            let thickness = p.faces[1] - p.faces[0];
            let depth = thickness - self.web;
            if depth < self.rules.nozzle * self.rules.wall_k {
                continue;
            }
            // the bigger of the two faces' rectangles
            let mut best: Option<(f64, f64, [f64; 2], [f64; 2])> = None;
            for (sign, mask) in [(-1.0, &p.from_hi), (1.0, &p.from_lo)] {
                let clear = fit::clearance(mask, self.cell, self.web);
                let Some((lo, hi)) = fit::largest_rect(mask, &clear) else { continue };
                let (centre, size) = fit::rect_mm(lo, hi, self.cell);
                let area = size[0] * size[1];
                if size[0] >= 2.0 * self.rules.corner && size[1] >= 2.0 * self.rules.corner && best.is_none_or(|b| area > b.1) {
                    best = Some((sign, area, centre, size));
                }
            }
            let Some((sign, _, centre, size)) = best else { continue };
            let axis = Self::unit(p.plate.axis, sign);
            let face = if sign < 0.0 { p.faces[1] } else { p.faces[0] };
            // across a plate whose thin axis is level, the second axis across is the build
            // direction ([`uv_of`]): a roof goes over the pocket's edge at its high end
            let [ua, _] = uv_of(p.plate.axis);
            let (mut centre, mut size) = (centre, size);
            let roof = needs_roof(axis);
            if roof {
                // the roof's tip reaches `depth` above the top edge: the rectangle gives it room
                let room = depth.min(size[1] - 2.0 * self.rules.corner).max(0.0);
                size[1] -= room;
                centre[1] -= 0.5 * room;
            }
            let size = size.map(|s| s * scale);
            if size[0] < 2.0 * self.rules.corner || size[1] < 2.0 * self.rules.corner {
                continue;
            }
            let along = Self::unit(ua, 1.0);
            let opens_down = axis[2] > 0.5;
            let corner = self.rules.corner.min(0.5 * size[0]).min(0.5 * size[1]);
            if opens_down && size[0].min(size[1]) > self.rules.bridge {
                // slots across the shorter side, each a bridge wide, a web apart
                let (short, long) = if size[0] <= size[1] { (0, 1) } else { (1, 0) };
                let n = ((size[short] + self.web) / (self.rules.bridge + self.web)).ceil() as usize;
                let width = (size[short] - (n - 1) as f64 * self.web) / n as f64;
                if width < 2.0 * self.rules.corner {
                    continue;
                }
                for s in 0..n {
                    let mut c = centre;
                    c[short] = centre[short] - 0.5 * size[short] + 0.5 * width + s as f64 * (width + self.web);
                    let mut sz = [0.0; 2];
                    sz[short] = width;
                    sz[long] = size[long];
                    out.push(Cut::Pocket { at: self.point(p, c, face), size: sz, along, axis, depth, corner: corner.min(0.5 * width), roof: false });
                }
                continue;
            }
            out.push(Cut::Pocket { at: self.point(p, centre, face), size, along, axis, depth, corner, roof });
        }
        out
    }
}
