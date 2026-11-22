//! The body: each stem of the plan printed as one open turn a layer, following its branch.
//!
//! A stem's centre follows its branch at most the line's lim a layer, and its turn's middle runs
//! at the planned radius. Every point stands within lim of the stem's own printed layer below,
//! wherever along it: one that would not is pulled in, the line's front stays, and a point that
//! makes no headway is dropped, walking a pulled run from both its sides; a span whose middle
//! would hang is cut. So a C closes from both its ends, a step a layer. A forking stem keeps its
//! turn's gap facing its fork axis from birth and pinches into a peanut over its last layers; the
//! cut is across the bottom of the waist, and each child stands only on what the parent printed
//! of its own lobe. A short piece the part cuts off, apart from its stem's main line, is dropped.
//! The crowns grow from the leaf stems' last layers, given as seeds.

use super::material::{Role, SupportPath};
use super::plan::{Plan, P2, FOLLOW};
use super::stem::N;
use crate::par::{Cancelled, Ctl};
use crate::support_rules::{closer, meet, Legal, BEAD_OVERLAP_TOL as TOUCH, R_MIN};
use makepad_fab_slicer::geom::{self, *};
use makepad_fab_slicer::grid::SegGrid;
use makepad_fab_slicer::offset::{offset_ex, Join};
use std::f64::consts::{PI, TAU};

/// How far (mm) past lim a span's middle may hang before it is cut.
const HANG: f64 = 0.02;
/// How finely (mm) a span is looked along for hanging.
const HANG_STEP: f64 = 0.05;
/// How far (rad) a stem's lobes turn a layer.
const TWIST: f64 = 0.03;
/// How much of its offset from its branch a stem keeps a layer.
const KEEP_OFF: f64 = 0.985;
/// A forking stem's turn stops this far (rad) short of closing: its gap, facing the fork axis.
const FORK_GAP: f64 = -0.3;
/// How far (rad) each lobe of a peanut stops short of the fork axis at the top of its waist.
const WAIST: f64 = 0.35;
/// The radius (mm) the bottom of a peanut's waist turns round from one lobe to the other: wider
/// than r_min, so its bead lies by itself as it bends (C5), and wider than a line, so it meets
/// both lobes.
const NECK: f64 = 0.6;
/// How far (rad) a lobe's swell fades out before it meets the neck, so the two meet smoothly.
const FADE: f64 = 0.6;
/// The points of a peanut's neck.
const BRIDGE: usize = 6;
/// A printed point nearer (mm) than this to the one before it says nothing but where a bend
/// tighter than the bead hooks: it goes, the line's last point taking its place at the end.
const STEP_MIN: f64 = 0.1;
/// A line bends no tighter than this (mm): r_min, with room for rounding (C5).
pub(super) const BEND: f64 = R_MIN * 1.1;
/// Rounds of straightening a line's tight bends.
const UNHOOK: usize = 4;
/// A body piece shorter than this share of its stem's main line, and farther than `APART` (mm)
/// from it, is a scrap.
const SCRAP: f64 = 0.5;
const APART: f64 = 1.2;
/// Rounds of pushing a layer's stems apart, and out of what keeps them off.
const PACK: usize = 12;
const CLEAR: usize = 4;
/// Rounds of packing the stems that share a footing.
const START: usize = 240;
/// Where (mm up) a root's heading is taken from its branch.
const AHEAD: f64 = 8.0;
/// Stems this far (mm) past touching on some layer are grown together.
const NEAR: f64 = 4.0;
/// A cell (mm) of a layer's lines, for nearest-point questions.
const NEAR_CELL: f64 = 0.5;

/// The body of a part's supports.
pub struct Body {
    /// Per layer, its paths.
    pub layers: Vec<Vec<SupportPath>>,
    /// Where each crown grows from.
    pub seeds: Vec<Seed>,
    /// The farthest (mm) any stem fell behind its branch.
    pub lag: f64,
}

/// A leaf stem's last layer: its crown grows from it from `layer` on.
#[derive(Clone, Debug)]
pub struct Seed {
    pub stem: usize,
    pub support: usize,
    pub layer: usize,
    pub lines: Vec<Vec<P2>>,
    /// The stem's centre and radius (mm) there.
    pub at: P2,
    pub r: f64,
}

/// A printed stretch of a stem's turn, and for each of its points the turn's point it is.
#[derive(Clone, Debug, Default)]
pub struct Line {
    pub pts: Vec<P2>,
    pub idx: Vec<usize>,
}

impl Line {
    /// The turn's points `from..` as a line.
    pub fn of(pts: &[P2], from: usize) -> Line {
        Line { pts: pts.to_vec(), idx: (from..from + pts.len()).collect() }
    }
}

/// A stretch of a stem's line, as [`apart`] looks at it: its ends, its line and its place on it,
/// how far along it it runs (mm), and the least turn index it holds.
struct Stretch {
    a: Pt,
    b: Pt,
    line: usize,
    j: usize,
    at: (f64, f64),
    k: usize,
}

/// Lines with every point dropped that ends a segment within a bead of an earlier stretch of
/// them, earlier by the turn index each point holds: where the run past a turn's start, or a
/// pulled stretch, would fuse with what it lies beside (C5), judged as the contracts judge it.
/// Stretches of one line less than π r_min apart along it, over no bend tighter than r_min, are
/// one bead, and two ends meeting a seam.
pub(super) fn apart(lines: Vec<Line>, w: f64) -> Vec<Line> {
    let r = ((w - TOUCH) * SCALE).round() as i64;
    let lens: Vec<f64> = lines.iter().map(|l| len(&l.pts)).collect();
    // rough[l][v]: how many of line l's vertices before v bend tighter than r_min.
    let rough: Vec<Vec<u32>> = lines.iter().map(|l| {
        let n = l.pts.len();
        let mut c = vec![0u32; n + 1];
        for v in 0..n {
            c[v + 1] = c[v] + (v > 0 && v + 1 < n && radius(l.pts[v - 1], l.pts[v], l.pts[v + 1]) < R_MIN) as u32;
        }
        c
    }).collect();
    let mut segs: Vec<Stretch> = Vec::new();
    let mut first = Vec::with_capacity(lines.len());
    for (li, l) in lines.iter().enumerate() {
        first.push(segs.len());
        let mut s = 0.0;
        for j in 1..l.pts.len() {
            let d = dist(l.pts[j - 1], l.pts[j]);
            segs.push(Stretch { a: pt(l.pts[j - 1]), b: pt(l.pts[j]), line: li, j: j - 1, at: (s, s + d), k: l.idx[j - 1].min(l.idx[j]) });
            s += d;
        }
    }
    let grid = SegGrid::new(segs.iter().enumerate().map(|(i, g)| (g.a, g.b, i as u32)).collect());
    // The part of a stretch farther than a bead from both ends of its line.
    let middle = |g: &Stretch| {
        let (lo, hi) = (g.at.0.max(w), g.at.1.min(lens[g.line] - w));
        let t = |s: f64| (s - g.at.0) / (g.at.1 - g.at.0);
        (hi > lo).then(|| (geom::lerp(g.a, g.b, t(lo)), geom::lerp(g.a, g.b, t(hi))))
    };
    let off = |g: &Stretch, h: &Stretch| middle(g).is_some_and(|(p, q)| closer(p, q, h.a, h.b, r));
    // Whether `g` fuses with `h`, earlier along the turn.
    let fuses = |g: &Stretch, h: &Stretch| {
        let one = g.line == h.line && {
            let (lo, hi) = (g.j.min(h.j), g.j.max(h.j));
            hi == lo + 1 || ((h.at.0 - g.at.1).max(g.at.0 - h.at.1) < PI * R_MIN && rough[g.line][hi + 1] == rough[g.line][lo + 1])
        };
        h.k < g.k && !one && closer(g.a, g.b, h.a, h.b, r) && (meet(g.a, g.b, h.a, h.b) || off(g, h) || off(h, g))
    };
    let mut out = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        let mut cur = Line::default();
        for j in 0..l.pts.len() {
            let gone = j > 0 && {
                let g = &segs[first[li] + j - 1];
                let bb = BBox { min: Pt::new(g.a.x.min(g.b.x) - r, g.a.y.min(g.b.y) - r), max: Pt::new(g.a.x.max(g.b.x) + r, g.a.y.max(g.b.y) + r) };
                let mut hit = false;
                grid.visit_box(bb, |_, _, h| hit = hit || fuses(g, &segs[h as usize]));
                hit
            };
            if gone {
                if cur.pts.len() >= 2 {
                    out.push(std::mem::take(&mut cur));
                }
                cur = Line::default();
            } else {
                cur.pts.push(l.pts[j]);
                cur.idx.push(l.idx[j]);
            }
        }
        if cur.pts.len() >= 2 {
            out.push(cur);
        }
    }
    out
}

/// How tightly (mm) a line bends at `b`, from `a` to `c`, as the contracts measure it: the lesser
/// of the circle through the three and the round that fits the shorter side.
pub(super) fn radius(a: P2, b: P2, c: P2) -> f64 {
    let (u, v) = ((b.0 - a.0, b.1 - a.1), (c.0 - b.0, c.1 - b.1));
    let (x, d) = (u.0 * v.1 - u.1 * v.0, u.0 * v.0 + u.1 * v.1);
    if x.abs() < 1e-15 && d >= 0.0 {
        return f64::INFINITY;
    }
    let (la, lb) = (dist(a, b), dist(b, c));
    let circle = if x.abs() < 1e-15 { 0.0 } else { dist(a, c) * la * lb / (2.0 * x.abs()) };
    circle.min(la.min(lb) / (x.atan2(d).abs() / 2.0).tan())
}

/// A line with its bends tighter than BEND straightened where `ok` lets a point be: a vertex
/// moves towards between its neighbours, an end point round onto the line's own heading; where
/// neither may, the line ends there and goes on after it. What is left, as lines.
pub(super) fn unhook(mut line: Line, ok: &impl Fn(P2, Option<P2>, Option<P2>) -> bool) -> Vec<Line> {
    let mut out = Vec::new();
    for _ in 0..UNHOOK {
        let mut changed = false;
        let mut i = 1;
        while i + 1 < line.pts.len() {
            let (a, b, c) = (line.pts[i - 1], line.pts[i], line.pts[i + 1]);
            if radius(a, b, c) >= BEND {
                i += 1;
                continue;
            }
            changed = true;
            let n = line.pts.len();
            // An end swings round its neighbour onto the heading the line comes in on.
            let end = if i + 2 == n { Some((i + 1, a)) } else if i == 1 { Some((0, c)) } else { None };
            if let Some((e, from)) = end {
                let l = dist(b, line.pts[e]);
                let ahead = (b.0 + (b.0 - from.0) * l / dist(from, b).max(1e-12), b.1 + (b.1 - from.1) * l / dist(from, b).max(1e-12));
                let swung = [1.0, 0.75, 0.5, 0.25].into_iter().map(|t| {
                    let q = lerp(line.pts[e], ahead, t);
                    let d = dist(b, q).max(1e-12);
                    (b.0 + (q.0 - b.0) * l / d, b.1 + (q.1 - b.1) * l / d)
                }).find(|&q| radius(from, b, q) >= BEND && ok(q, Some(b), None));
                match swung {
                    Some(q) => line.pts[e] = q,
                    None => {
                        line.pts.remove(e);
                        line.idx.remove(e);
                    }
                }
                continue;
            }
            let m = lerp(a, c, 0.5);
            if let Some(q) = [1.0, 0.5].into_iter().map(|t| lerp(b, m, t)).find(|&q| radius(a, q, c) >= BEND && ok(q, Some(a), Some(c))) {
                line.pts[i] = q;
                i += 1;
                continue;
            }
            // Neither: the line ends at the bend and goes on from the point after it, a seam.
            let rest = Line { pts: line.pts.split_off(i + 1), idx: line.idx.split_off(i + 1) };
            out.push(std::mem::replace(&mut line, rest));
            i = 1;
        }
        if !changed {
            break;
        }
    }
    // What the rounds left tight, each piece ends at and goes on after.
    out.push(line);
    let mut done = Vec::with_capacity(out.len());
    for line in out {
        let mut cur = Line::default();
        for (&p, &k) in line.pts.iter().zip(&line.idx) {
            let m = cur.pts.len();
            if m >= 2 && radius(cur.pts[m - 2], cur.pts[m - 1], p) < BEND {
                done.push(std::mem::take(&mut cur));
            }
            cur.pts.push(p);
            cur.idx.push(k);
        }
        done.push(cur);
    }
    done
}

/// A stem being printed.
struct Live {
    stem: usize,
    support: usize,
    /// Its turn last layer, every point where it was asked to be after the reach rule.
    pts: Vec<P2>,
    c: P2,
    th: f64,
    ph: f64,
    off: P2,
    /// What it printed last layer; none for a stem on its footing.
    last: Option<Vec<Line>>,
    lag: f64,
}

pub(super) fn dist(a: P2, b: P2) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

pub(super) fn pt(p: P2) -> Pt {
    Pt::from_mm(p.0, p.1)
}

pub(super) fn len(pts: &[P2]) -> f64 {
    pts.windows(2).map(|w| dist(w[0], w[1])).sum()
}

pub(super) fn lerp(a: P2, b: P2, t: f64) -> P2 {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// The nearest point of segment `a`-`b` to `p`.
pub(super) fn foot(a: P2, b: P2, p: P2) -> P2 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (a.0 + dx * t, a.1 + dy * t)
}

/// A layer's printed lines, for nearest-point questions: their segments in cells.
pub struct Near {
    segs: Vec<(P2, P2)>,
    x0: f64,
    y0: f64,
    nx: usize,
    ny: usize,
    at: Vec<u32>,
    items: Vec<u32>,
}

impl Near {
    pub fn new<'a>(lines: impl Iterator<Item = &'a [P2]>) -> Near {
        let segs: Vec<(P2, P2)> = lines.flat_map(|l| l.windows(2).map(|w| (w[0], w[1]))).collect();
        if segs.is_empty() {
            return Near { segs, x0: 0.0, y0: 0.0, nx: 0, ny: 0, at: vec![0], items: Vec::new() };
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(a, b) in &segs {
            x0 = x0.min(a.0.min(b.0));
            y0 = y0.min(a.1.min(b.1));
            x1 = x1.max(a.0.max(b.0));
            y1 = y1.max(a.1.max(b.1));
        }
        let nx = ((x1 - x0) / NEAR_CELL).floor() as usize + 1;
        let ny = ((y1 - y0) / NEAR_CELL).floor() as usize + 1;
        let cells = |&(a, b): &(P2, P2)| {
            let i0 = ((a.0.min(b.0) - x0) / NEAR_CELL).floor() as usize;
            let j0 = ((a.1.min(b.1) - y0) / NEAR_CELL).floor() as usize;
            let i1 = (((a.0.max(b.0) - x0) / NEAR_CELL).floor() as usize).min(nx - 1);
            let j1 = (((a.1.max(b.1) - y0) / NEAR_CELL).floor() as usize).min(ny - 1);
            (j0..=j1).flat_map(move |j| (i0..=i1).map(move |i| j * nx + i))
        };
        let mut at = vec![0u32; nx * ny + 1];
        for s in &segs {
            for c in cells(s) {
                at[c + 1] += 1;
            }
        }
        for c in 0..nx * ny {
            at[c + 1] += at[c];
        }
        let mut fill = at.clone();
        let mut items = vec![0u32; at[nx * ny] as usize];
        for (k, s) in segs.iter().enumerate() {
            for c in cells(s) {
                items[fill[c] as usize] = k as u32;
                fill[c] += 1;
            }
        }
        Near { segs, x0, y0, nx, ny, at, items }
    }

    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// The nearest point of the lines to `p` and how far it is: looked for within `within` mm
    /// first, and over all of them only when nothing is that near.
    pub fn nearest(&self, p: P2, within: f64) -> (f64, P2) {
        let mut best = (f64::INFINITY, p);
        if self.segs.is_empty() {
            return best;
        }
        let r = (within / NEAR_CELL).ceil() as i64 + 1;
        let (ci, cj) = (((p.0 - self.x0) / NEAR_CELL).floor() as i64, ((p.1 - self.y0) / NEAR_CELL).floor() as i64);
        for j in (cj - r).max(0)..=(cj + r).min(self.ny as i64 - 1) {
            for i in (ci - r).max(0)..=(ci + r).min(self.nx as i64 - 1) {
                let c = j as usize * self.nx + i as usize;
                for &k in &self.items[self.at[c] as usize..self.at[c + 1] as usize] {
                    let (a, b) = self.segs[k as usize];
                    let q = foot(a, b, p);
                    let d = dist(p, q);
                    if d < best.0 {
                        best = (d, q);
                    }
                }
            }
        }
        if best.0 > within {
            for &(a, b) in &self.segs {
                let q = foot(a, b, p);
                let d = dist(p, q);
                if d < best.0 {
                    best = (d, q);
                }
            }
        }
        best
    }
}

/// The lines printed so far on a layer, in cells, to keep the next ones a bead off them.
#[derive(Default)]
pub struct Beside {
    cells: std::collections::HashMap<(i64, i64), Vec<(P2, P2)>>,
}

impl Beside {
    fn cell(p: P2) -> (i64, i64) {
        ((p.0 / NEAR_CELL).floor() as i64, (p.1 / NEAR_CELL).floor() as i64)
    }

    pub fn add(&mut self, pts: &[P2]) {
        for s in pts.windows(2) {
            let (a, b) = (Self::cell(s[0]), Self::cell(s[1]));
            for j in a.1.min(b.1)..=a.1.max(b.1) {
                for i in a.0.min(b.0)..=a.0.max(b.0) {
                    self.cells.entry((i, j)).or_default().push((s[0], s[1]));
                }
            }
        }
    }

    /// Whether some line lies within `d` (mm, at most a cell) of `p`.
    pub fn within(&self, p: P2, d: f64) -> bool {
        let (ci, cj) = Self::cell(p);
        (cj - 1..=cj + 1).any(|j| (ci - 1..=ci + 1).any(|i| self.cells.get(&(i, j)).is_some_and(|v| v.iter().any(|&(a, b)| dist(foot(a, b, p), p) < d))))
    }
}

/// What keeps a bead's centre off, per layer: K_l, by its segments.
pub struct Keep {
    grids: Vec<SegGrid<u32>>,
}

/// How much (units) what keeps a bead off is grown: its round edges' chords lie inside their
/// arcs, twice over (the part's side gap, then half the bead), by at most this less a chord's.
const KEEP_MARGIN: f64 = 3000.0;

impl Keep {
    /// From K_l on each layer, a hair grown so that out of it is out of the true K_l.
    pub fn new(per_layer: impl Iterator<Item = Vec<ExPolygon>>) -> Keep {
        Keep { grids: per_layer.map(|k| {
            let k = if k.is_empty() { k } else { offset_ex(&k, KEEP_MARGIN, Join::Miter(2.0)) };
            SegGrid::of_regions(&k, |i| i as u32)
        }).collect() }
    }

    /// How far `p` lies outside what keeps a bead's centre off on layer `l` (mm, less than 0
    /// inside), and the way out.
    pub(super) fn clearance(&self, p: P2, l: usize) -> (f64, P2) {
        let g = &self.grids[l];
        if g.is_empty() {
            return (f64::INFINITY, (0.0, 0.0));
        }
        let q = pt(p);
        let mut best = (f64::INFINITY, q);
        g.nearest_with(q, |a, b, _| {
            let d = dist_point_segment(q, a, b);
            if d < best.0 {
                let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
                let l2 = dx * dx + dy * dy;
                let t = if l2 > 0.0 { (((q.x - a.x) as f64 * dx + (q.y - a.y) as f64 * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                best = (d, Pt::new(a.x + (dx * t).round() as i64, a.y + (dy * t).round() as i64));
            }
            best.0
        });
        let (ux, uy) = (p.0 - best.1.x_mm(), p.1 - best.1.y_mm());
        let ul = ux.hypot(uy).max(1e-12);
        let d = best.0 / SCALE;
        if g.contains_any(q) { (-d, (-ux / ul, -uy / ul)) } else { (d, (ux / ul, uy / ul)) }
    }

    /// Whether a bead's centre may be at `p` on layer `l`.
    pub fn legal(&self, p: P2, l: usize) -> bool {
        let g = &self.grids[l];
        g.is_empty() || !g.contains_any(pt(p))
    }

    /// Whether a bead's centre may run from `a` to `b` on layer `l`: it starts out of what keeps
    /// it off and crosses none of its edges.
    pub fn clear(&self, a: P2, b: P2, l: usize) -> bool {
        let g = &self.grids[l];
        if g.is_empty() {
            return true;
        }
        let (p, q) = (pt(a), pt(b));
        let mut hit = false;
        g.visit_box(BBox { min: Pt::new(p.x.min(q.x), p.y.min(q.y)), max: Pt::new(p.x.max(q.x), p.y.max(q.y)) }, |c, d, _| hit = hit || meet(p, q, c, d));
        !hit && !g.contains_any(p)
    }
}

/// A peanut round `c` along axis `ax`: round lobe A of radius `ra` from the top of the waist,
/// across its bottom on a neck of radius NECK tangent to both lobes, and round lobe B of `rb`
/// back up. Its points, and the lobes' centres.
fn peanut(c: P2, ax: f64, ra: f64, rb: f64, ph: f64, w: f64, lobe: f64) -> (Vec<P2>, P2, P2) {
    let na = (N - BRIDGE) >> 1;
    let nb = N - na - BRIDGE;
    let (u, v) = ((ax.cos(), ax.sin()), (-ax.sin(), ax.cos()));
    let r = ra + rb + w;
    let ca = (c.0 - (r - ra) * u.0, c.1 - (r - ra) * u.1);
    let cb = (c.0 + (r - rb) * u.0, c.1 + (r - rb) * u.1);
    // The neck's centre, under the axis, NECK off both lobes.
    let d = ra + rb + 2.0 * w;
    let rho = NECK.max(w * 1.1);
    let xa = (d * d + (ra + rho).powi(2) - (rb + rho).powi(2)) / (2.0 * d);
    let h = ((ra + rho).powi(2) - xa * xa).max(0.0).sqrt();
    let at = |x: f64, y: f64| (ca.0 + x * u.0 + y * v.0, ca.1 + x * u.1 + y * v.1);
    let f = at(xa, -h);
    let (ea, sb) = (TAU + (-h).atan2(xa), PI + h.atan2(d - xa));
    let mut out = vec![(0.0, 0.0); N];
    let mut arc = |o: P2, r: f64, a0: f64, a1: f64, i0: usize, n: usize, neck: f64| {
        for i in 0..n {
            let a = a0 + (a1 - a0) * i as f64 / (n - 1) as f64;
            let fade = ((a - neck).abs() / FADE).min(1.0).powi(2);
            let rr = r * (1.0 + fade * lobe * (3.0 * (ax + a) + ph).sin());
            out[i0 + i] = (o.0 + rr * (ax + a).cos(), o.1 + rr * (ax + a).sin());
        }
    };
    arc(ca, ra, WAIST, ea, 0, na, ea);
    arc(cb, rb, sb, 3.0 * PI - WAIST, na + BRIDGE, nb, sb);
    // Round the neck from lobe A over to lobe B, through the top of its circle.
    let (ta, tb) = (h.atan2(-xa), h.atan2(d - xa));
    for i in 0..BRIDGE {
        let t = ta + (tb - ta) * (i + 1) as f64 / (BRIDGE + 1) as f64;
        out[na + i] = (f.0 + rho * (t.cos() * u.0 + t.sin() * v.0), f.1 + rho * (t.cos() * u.1 + t.sin() * v.1));
    }
    (out, ca, cb)
}

/// `pts[i0..=i1]` resampled to N points evenly along it.
fn resample(pts: &[P2], i0: usize, i1: usize) -> Vec<P2> {
    let mut cum = vec![0.0];
    for i in i0 + 1..=i1 {
        cum.push(cum[cum.len() - 1] + dist(pts[i - 1], pts[i]));
    }
    let tot = cum[cum.len() - 1];
    let mut j = 0;
    (0..N)
        .map(|q| {
            let s = tot * q as f64 / (N - 1) as f64;
            while j < cum.len() - 2 && cum[j + 1] < s {
                j += 1;
            }
            let t = if cum[j + 1] > cum[j] { (s - cum[j]) / (cum[j + 1] - cum[j]) } else { 0.0 };
            lerp(pts[i0 + j], pts[i0 + j + 1], t)
        })
        .collect()
}

/// The reach rule against a stem's own layer below: a point beyond `lim` of it is pulled in
/// under it; in a run of pulled points the one next to a standing point is the line's front and
/// stays, and going on into the run from that side, a point that gets less than half its own step
/// on from the last one kept is dropped. A pulled point that comes within a bead of another stretch
/// of its own turn, the way a turn's run past its start is pulled onto its start over a single
/// line, would fuse with it, and goes too. A span whose middle hangs past lim is cut. Which points
/// are dropped, and before which point a span is cut.
fn reach_rule(want: &mut [P2], below: &Near, lim: f64, w: f64) -> (Vec<bool>, Vec<bool>) {
    let was: Vec<P2> = want.to_vec();
    let n = want.len();
    let mut pulled = vec![false; n];
    for (i, p) in want.iter_mut().enumerate() {
        let (d, q) = below.nearest(*p, lim);
        if d > lim {
            if d.is_finite() {
                *p = lerp(q, *p, lim / d);
            }
            pulled[i] = true;
        }
    }
    let mut gone = vec![false; n];
    let walk = |gone: &mut Vec<bool>, from: usize, to: usize| {
        let mut j = from;
        let mut i = from;
        while i != to {
            i = if to > from { i + 1 } else { i - 1 };
            if dist(want[i], want[j]) < 0.5 * dist(was[i], was[j]) {
                gone[i] = true;
            } else {
                j = i;
            }
        }
    };
    let mut a = 0;
    while a < n {
        if !pulled[a] {
            a += 1;
            continue;
        }
        let mut b = a;
        while b + 1 < n && pulled[b + 1] {
            b += 1;
        }
        match (a > 0, b < n - 1) {
            (true, true) => {
                let m = (a + b) / 2;
                walk(&mut gone, a, m);
                if b > m {
                    walk(&mut gone, b, m + 1);
                }
            }
            (false, true) => walk(&mut gone, b, a),
            _ => walk(&mut gone, a, b),
        }
        a = b + 1;
    }
    // Of two pulled points on two stretches of the turn, the later goes.
    let (apart, touch) = (n / 3, w - TOUCH);
    for i in 0..n {
        if pulled[i] && !gone[i] && (0..n).any(|j| i.abs_diff(j) >= apart && !gone[j] && (!pulled[j] || j < i) && dist(want[i], want[j]) < touch) {
            gone[i] = true;
        }
    }
    let mut hang = vec![false; n];
    for i in 1..n {
        if gone[i] || gone[i - 1] {
            continue;
        }
        let steps = (dist(want[i], want[i - 1]) / HANG_STEP).ceil() as usize;
        if (1..steps).any(|q| below.nearest(lerp(want[i - 1], want[i], q as f64 / steps as f64), lim + HANG).0 > lim + HANG) {
            hang[i] = true;
        }
    }
    (gone, hang)
}

/// One stem's layer: `want` is its turn where it is asked to be, `below` what it printed on the
/// layer under it (none on its footing). The reach rule, then what keeps it off and the lines
/// already printed `beside` it on the layer, a bead off, and its own stretches a bead apart; then
/// its scraps. Its turn after the reach rule, and what it prints.
pub fn print(mut want: Vec<P2>, below: Option<&[Line]>, keep: &Keep, beside: &Beside, l: usize, lim: f64, w: f64) -> (Vec<P2>, Vec<Line>) {
    let n = want.len();
    let under = below.map(|lines| Near::new(lines.iter().map(|l| &l.pts[..])));
    let (gone, hang) = match &under {
        Some(near) => reach_rule(&mut want, near, lim, w),
        None => (vec![false; n], vec![false; n]),
    };
    // Where a point may stand: within lim of the layer below, its spans to its neighbours not
    // hanging, clear of what keeps it off and of what is printed beside it.
    let stands = |p: P2| under.as_ref().is_none_or(|u| u.nearest(p, lim).0 <= lim + 1e-9);
    let spans = |p: P2, q: Option<P2>| q.is_none_or(|q| under.as_ref().is_none_or(|u| u.nearest(lerp(p, q, 0.5), lim + HANG).0 <= lim + HANG));
    let ok = |p: P2, a: Option<P2>, b: Option<P2>| stands(p) && spans(p, a) && spans(p, b) && keep.legal(p, l) && !beside.within(p, w - TOUCH);
    let mut out: Vec<Line> = Vec::new();
    let mut cur = Line::default();
    let flush = |cur: &mut Line, out: &mut Vec<Line>| {
        let mut kept = Line::default();
        for (i, (&p, &k)) in cur.pts.iter().zip(&cur.idx).enumerate() {
            let m = kept.pts.len();
            if m > 0 && dist(kept.pts[m - 1], p) < STEP_MIN {
                if i + 1 == cur.pts.len() && m > 1 {
                    kept.pts[m - 1] = p;
                    kept.idx[m - 1] = k;
                }
            } else {
                kept.pts.push(p);
                kept.idx.push(k);
            }
        }
        *cur = Line::default();
        out.extend(unhook(kept, &ok).into_iter().filter(|l| l.pts.len() >= 2 && len(&l.pts) >= 2.0 * w));
    };
    for i in 0..n {
        if hang[i] {
            flush(&mut cur, &mut out);
        }
        if gone[i] || !keep.legal(want[i], l) || beside.within(want[i], w - TOUCH) {
            flush(&mut cur, &mut out);
            continue;
        }
        cur.pts.push(want[i]);
        cur.idx.push(i);
    }
    flush(&mut cur, &mut out);
    let out: Vec<Line> = apart(out, w).into_iter().filter(|l| l.pts.len() >= 2 && len(&l.pts) >= 2.0 * w).collect();
    // A scrap the part cut off, short and standing apart from the stem's main line, would be a
    // fin of its own: it goes.
    let ls: Vec<f64> = out.iter().map(|l| len(&l.pts)).collect();
    let big = ls.iter().copied().fold(0.0, f64::max);
    let main = Near::new(out.iter().zip(&ls).filter(|(_, &x)| x == big).map(|(l, _)| &l.pts[..]));
    let kept: Vec<bool> = out.iter().zip(&ls).map(|(l, &x)| x >= SCRAP * big || l.pts.iter().any(|&p| main.nearest(p, APART).0 < APART)).collect();
    let out = out.into_iter().zip(kept).filter(|(_, k)| *k).map(|(l, _)| l).collect();
    (want, out)
}

/// The body of `plan`'s supports, keeping `legal`, out of `keep` (K_l, a bead's centre).
pub fn body(ctl: Ctl, legal: &Legal, keep: &Keep, plan: &Plan) -> Result<Body, Cancelled> {
    let n = legal.layers();
    // Each stem's support: the root it stands on.
    let mut support = vec![usize::MAX; plan.stems.len()];
    for (si, &r) in plan.roots.iter().enumerate() {
        let mut stack = vec![r];
        while let Some(e) = stack.pop() {
            support[e] = si;
            stack.extend(plan.stems[e].kids.iter().copied());
        }
    }
    let p = legal.params();
    let grower = Grower { plan, keep, support: &support, w: p.line_width, tan: p.angle_max_deg.to_radians().tan(), z: legal.heights()[..n].iter().map(|h| h.0).collect() };
    let grown = crate::par::try_map_vec(ctl, conflicts(plan, &support), |_, g| grower.grow(&g))?;
    let mut out = Body { layers: vec![Vec::new(); n], seeds: Vec::new(), lag: 0.0 };
    for g in grown {
        for (l, paths) in g.layers {
            out.layers[l].extend(paths);
        }
        out.seeds.extend(g.seeds);
        out.lag = out.lag.max(g.lag);
    }
    for l in &mut out.layers {
        l.sort_by_key(|p| (p.support, p.frond));
    }
    out.seeds.sort_by_key(|s| s.stem);
    Ok(out)
}

/// Supports whose stems come near each other on some layer: grown together, one group at a time,
/// apart from the rest.
fn conflicts(plan: &Plan, support: &[usize]) -> Vec<Vec<usize>> {
    let k = plan.roots.len();
    let mut id: Vec<usize> = (0..k).collect();
    fn find(id: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while id[r] != r {
            r = id[r];
        }
        id[i] = r;
        r
    }
    let end = plan.stems.iter().map(|s| s.end()).max().unwrap_or(0);
    let mut rows: Vec<Vec<(usize, P2, f64)>> = vec![Vec::new(); end];
    for (e, s) in plan.stems.iter().enumerate() {
        if support[e] == usize::MAX {
            continue;
        }
        for (i, r) in s.rows.iter().enumerate() {
            rows[s.first + i].push((support[e], r.at, r.r));
        }
    }
    for layer in &rows {
        for (i, a) in layer.iter().enumerate() {
            for b in &layer[i + 1..] {
                if a.0 != b.0 && dist(a.1, b.1) < a.2 + b.2 + NEAR {
                    let (ra, rb) = (find(&mut id, a.0), find(&mut id, b.0));
                    if ra != rb {
                        id[ra.max(rb)] = ra.min(rb);
                    }
                }
            }
        }
    }
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut at: Vec<usize> = vec![usize::MAX; k];
    for s in 0..k {
        let r = find(&mut id, s);
        if at[r] == usize::MAX {
            at[r] = out.len();
            out.push(Vec::new());
        }
        out[at[r]].push(s);
    }
    out
}

struct Grower<'a> {
    plan: &'a Plan,
    keep: &'a Keep,
    support: &'a [usize],
    w: f64,
    tan: f64,
    z: Vec<f64>,
}

/// One group's body: per layer its paths, its seeds, its lag.
struct Grown {
    layers: Vec<(usize, Vec<SupportPath>)>,
    seeds: Vec<Seed>,
    lag: f64,
}

/// The stems being printed, and the seeds of those done.
struct State {
    live: Vec<Live>,
    seeds: Vec<Seed>,
}

impl Grower<'_> {
    /// lim on layer `l` (mm): how far a point may stand past the layer under it.
    fn lim(&self, l: usize) -> f64 {
        let h = if l == 0 { self.z[0] } else { self.z[l] - self.z[l - 1] };
        FOLLOW * self.tan * h
    }

    fn overlap(&self, stem: usize) -> f64 {
        if self.plan.stems[stem].kids.is_empty() { self.plan.turn.overlap } else { FORK_GAP }
    }

    /// A stem starts: printing from its first layer, or, with no layers of its own, straight into
    /// its kids. A forking stem's turn has its gap facing its fork axis from birth.
    fn spawn(&self, st: &mut State, mut s: Live, k: usize) {
        let stem = &self.plan.stems[s.stem];
        if !stem.kids.is_empty() {
            s.th = stem.axis + PI / 2.0;
        }
        if let Some(row) = stem.rows.first() {
            s.off = (s.c.0 - row.at.0, s.c.1 - row.at.1);
            st.live.push(s);
        } else {
            self.split(st, s, k);
        }
    }

    /// A stem whose layers are done: a leaf gives its crown's seed; a fork's bridge across the
    /// bottom of its waist is cut, and each lobe's arc goes on as its kid, standing on what the
    /// parent printed of that lobe.
    fn split(&self, st: &mut State, s: Live, k: usize) {
        let stem = &self.plan.stems[s.stem];
        if stem.kids.is_empty() {
            let r = stem.rows.last().map_or(stem.r0, |r| r.r);
            st.seeds.push(Seed { stem: s.stem, support: s.support, layer: k, lines: s.last.unwrap_or_default().into_iter().map(|l| l.pts).collect(), at: s.c, r });
            return;
        }
        let (_, ca, cb) = peanut(s.c, stem.axis, stem.lobes.0, stem.lobes.1, s.ph, self.w, self.plan.turn.lobe);
        let na = (N - BRIDGE) >> 1;
        for (i0, i1, c, kid) in [(0, na - 1, ca, stem.kids[0]), (na + BRIDGE, N - 1, cb, stem.kids[1])] {
            let pts = resample(&s.pts, i0, i1);
            let own = s.last.as_ref().map(|last| {
                last.iter()
                    .filter_map(|l| {
                        let (a, b) = (l.idx.partition_point(|&k| k < i0), l.idx.partition_point(|&k| k <= i1));
                        (b > a + 1).then(|| Line { pts: l.pts[a..b].to_vec(), idx: l.idx[a..b].to_vec() })
                    })
                    .collect()
            });
            let th = (pts[0].1 - c.1).atan2(pts[0].0 - c.0);
            self.spawn(st, Live { stem: kid, support: s.support, pts, c, th, ph: s.ph, off: (0.0, 0.0), last: own, lag: 0.0 }, k);
        }
    }

    /// The stems that stand on one footing start packed round it, touching, clear of what keeps
    /// them off; each turn starts facing away from where it was pushed from.
    fn start(&self, st: &mut State, roots: &[usize], k: usize) {
        struct At {
            stem: usize,
            row: P2,
            c: P2,
            /// Its turn's outer bead's centre (mm from its own).
            r: f64,
        }
        let mut at: Vec<At> = roots
            .iter()
            .map(|&e| {
                let s = &self.plan.stems[e];
                let (row, r) = s.rows.first().map_or(((0.0, 0.0), s.r0), |r| (r.at, r.r));
                let i = s.rows.iter().enumerate().take_while(|(i, _)| self.z[s.first + i] <= AHEAD).count().max(1) - 1;
                let ahead = s.rows.get(i).map_or(row, |r| r.at);
                let d = dist(ahead, row);
                let c = if d > 1e-6 { lerp(row, ahead, 0.01 / d) } else { row };
                At { stem: e, row, c, r: r + self.w / 2.0 }
            })
            .collect();
        for it in 0..START {
            for a in at.iter_mut() {
                let (c, u) = self.keep.clearance(a.c, k);
                if c < a.r {
                    a.c = (a.c.0 + (a.r - c) * u.0, a.c.1 + (a.r - c) * u.1);
                }
            }
            for i in 0..at.len() {
                for j in i + 1..at.len() {
                    let (dx, dy) = (at[j].c.0 - at[i].c.0, at[j].c.1 - at[i].c.1);
                    let d = dx.hypot(dy).max(1e-6);
                    let w = at[i].r + at[j].r;
                    let m = if d < w { (w - d) / 2.0 + 1e-4 } else if it > START / 2 && d < w + 0.5 { (w - d) / 2.0 } else { 0.0 };
                    at[i].c = (at[i].c.0 - m * dx / d, at[i].c.1 - m * dy / d);
                    at[j].c = (at[j].c.0 + m * dx / d, at[j].c.1 + m * dy / d);
                }
            }
        }
        for a in at {
            let s = &self.plan.stems[a.stem];
            let th = if s.kids.is_empty() { (a.c.1 - a.row.1).atan2(a.c.0 - a.row.0) + PI } else { s.axis + PI / 2.0 };
            let pts = self.plan.turn.points(a.c, s.rows.first().map_or(s.r0, |r| r.r), th, 0.0, self.overlap(a.stem));
            self.spawn(st, Live { stem: a.stem, support: self.support[a.stem], pts, c: a.c, th, ph: 0.0, off: (0.0, 0.0), last: None, lag: 0.0 }, k);
        }
    }

    /// One group of supports, layer by layer.
    fn grow(&self, group: &[usize]) -> Grown {
        let roots: Vec<usize> = group.iter().map(|&s| self.plan.roots[s]).collect();
        let mut firsts: Vec<usize> = roots.iter().map(|&r| self.plan.stems[r].first).collect();
        firsts.sort_unstable();
        firsts.dedup();
        let mut st = State { live: Vec::new(), seeds: Vec::new() };
        let mut out = Grown { layers: Vec::new(), seeds: Vec::new(), lag: 0.0 };
        let n = self.z.len();
        let mut k = firsts.first().copied().unwrap_or(n);
        while k < n && (!st.live.is_empty() || firsts.iter().any(|&f| f >= k)) {
            if firsts.contains(&k) {
                let here: Vec<usize> = roots.iter().copied().filter(|&r| self.plan.stems[r].first == k).collect();
                self.start(&mut st, &here, k);
            }
            let mut i = 0;
            while i < st.live.len() {
                if k >= self.plan.stems[st.live[i].stem].end() {
                    let s = st.live.remove(i);
                    out.lag = out.lag.max(s.lag);
                    self.split(&mut st, s, k);
                } else {
                    i += 1;
                }
            }
            if !st.live.is_empty() {
                let paths = self.layer(&mut st.live, k);
                out.layers.push((k, paths));
            }
            k += 1;
        }
        // Stems still standing at the last layer give their seeds there.
        for s in std::mem::take(&mut st.live) {
            out.lag = out.lag.max(s.lag);
            if self.plan.stems[s.stem].kids.is_empty() {
                self.split(&mut st, s, n);
            }
        }
        out.seeds = st.seeds;
        out
    }

    /// One layer of the live stems: where each goes, and what it prints.
    fn layer(&self, live: &mut [Live], k: usize) -> Vec<SupportPath> {
        let lim = self.lim(k);
        // Where each stem's centre is asked to be: its branch and its offset, apart from the
        // others, its turn's outer bead clear of what keeps a bead's centre off.
        let mut t: Vec<(P2, f64)> = live
            .iter_mut()
            .map(|s| {
                let stem = &self.plan.stems[s.stem];
                let row = stem.rows[k - stem.first];
                s.off = (s.off.0 * KEEP_OFF, s.off.1 * KEEP_OFF);
                ((row.at.0 + s.off.0, row.at.1 + s.off.1), row.r + self.w / 2.0)
            })
            .collect();
        for _ in 0..PACK {
            for i in 0..t.len() {
                for j in i + 1..t.len() {
                    let (mut dx, mut dy) = (t[j].0 .0 - t[i].0 .0, t[j].0 .1 - t[i].0 .1);
                    let d = dx.hypot(dy);
                    let w = t[i].1 + t[j].1;
                    if d >= w {
                        continue;
                    }
                    if d < 1e-6 {
                        dx = live[j].c.0 - live[i].c.0;
                        dy = live[j].c.1 - live[i].c.1;
                    }
                    let l = dx.hypot(dy).max(1e-9);
                    let m = (w - d) / 2.0;
                    t[i].0 = (t[i].0 .0 - m * dx / l, t[i].0 .1 - m * dy / l);
                    t[j].0 = (t[j].0 .0 + m * dx / l, t[j].0 .1 + m * dy / l);
                }
            }
        }
        for (p, r) in t.iter_mut() {
            for _ in 0..CLEAR {
                let (c, u) = self.keep.clearance(*p, k);
                if c >= *r {
                    break;
                }
                *p = (p.0 + (*r - c) * u.0, p.1 + (*r - c) * u.1);
            }
        }
        let mut paths = Vec::new();
        let mut beside = Beside::default();
        for (s, &(target, _)) in live.iter_mut().zip(&t) {
            let stem = &self.plan.stems[s.stem];
            let row = stem.rows[k - stem.first];
            s.off = (target.0 - row.at.0, target.1 - row.at.1);
            // The centre follows at most lim; every point then heads for its place on the turn.
            let d = dist(target, s.c);
            let mv = d.min(lim);
            s.lag = s.lag.max(d - mv);
            if d > 1e-12 {
                s.c = lerp(s.c, target, mv / d);
            }
            s.ph += TWIST;
            let mut want = self.plan.turn.points(s.c, row.r, s.th, s.ph, self.overlap(s.stem));
            if stem.pinch > 0 {
                let b = ((k as f64 - (stem.end() as f64 - 1.0 - stem.pinch as f64)) / stem.pinch as f64).clamp(0.0, 1.0);
                if b > 0.0 {
                    let (pea, _, _) = peanut(s.c, stem.axis, stem.lobes.0, stem.lobes.1, s.ph, self.w, self.plan.turn.lobe);
                    for (p, q) in want.iter_mut().zip(&pea) {
                        *p = lerp(*p, *q, b);
                    }
                }
            }
            let (pts, out) = print(want, s.last.as_deref(), self.keep, &beside, k, lim, self.w);
            s.pts = pts;
            for l in &out {
                beside.add(&l.pts);
                paths.push(SupportPath { pts: l.pts.iter().map(|&p| pt(p)).collect(), width: self.w, role: if k == 0 { Role::Pad } else { Role::Body }, support: s.support, frond: s.stem });
            }
            s.last = Some(out);
        }
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::super::plan::{plan, Coral};
    use super::super::scenes::{rect, Scene, LH, LW};
    use super::super::stem::{imin, Turn};
    use super::*;
    use crate::support_rules::{prove, Contract};

    const SIX: [&str; 6] = ["shelf", "bar", "branches", "tower", "pads", "mushroom"];
    const TURN: Turn = Turn { width: LW, overlap: 0.9, lobe: 0.06 };
    /// lim at 45 degrees and the scenes' layers.
    const LIM: f64 = FOLLOW * LH;

    struct Run {
        scene: Scene,
        plan: Plan,
        body: Body,
    }

    fn run(name: &str) -> Run {
        let scene = Scene::named(name);
        let legal = scene.legal();
        let reach = crate::coral::reach_of(Ctl::default(), &legal, &scene.demand()).unwrap();
        let plan = plan(Ctl::default(), &legal, &reach, &scene.demand(), &Coral::default()).unwrap();
        let keep = Keep::new((0..legal.layers()).map(|l| reach.keep(l).to_vec()));
        let body = body(Ctl::default(), &legal, &keep, &plan).unwrap();
        Run { scene, plan, body }
    }

    fn mm(p: &SupportPath) -> Vec<P2> {
        p.pts.iter().map(|q| (q.x_mm(), q.y_mm())).collect()
    }

    /// The stem `e` stands on below its first layer, if it is a kid: its parent, or the nearest
    /// ancestor with layers of its own.
    fn under(plan: &Plan, e: usize) -> Option<usize> {
        let mut p = plan.stems.iter().position(|s| s.kids.contains(&e))?;
        while plan.stems[p].rows.is_empty() {
            p = plan.stems.iter().position(|s| s.kids.contains(&p))?;
        }
        Some(p)
    }

    /// On the six scenes, every point a stem prints, and every bit of every span between, lies
    /// within lim of what that stem (or, on a kid's first layer, its parent) printed on the layer
    /// below.
    #[test]
    fn every_point_is_within_reach_of_the_layer_below() {
        for name in SIX {
            let r = run(name);
            let mut worst: f64 = f64::NEG_INFINITY;
            for l in 1..r.body.layers.len() {
                for p in &r.body.layers[l] {
                    let s = &r.plan.stems[p.frond];
                    let Some(on) = (if l > s.first { Some(p.frond) } else { under(&r.plan, p.frond) }) else { continue };
                    let below: Vec<Vec<P2>> = r.body.layers[l - 1].iter().filter(|q| q.frond == on).map(mm).collect();
                    assert!(!below.is_empty(), "{name}: layer {l} stem {} stands on nothing", p.frond);
                    let near = Near::new(below.iter().map(|v| &v[..]));
                    let pts = mm(p);
                    // Between the rule's looks along a span, a step apart, a point may stand half
                    // a step further off.
                    for (i, &q) in pts.iter().enumerate() {
                        let d = near.nearest(q, LIM).0 - LIM;
                        assert!(d < 1e-5, "{name}: layer {l} stem {}: point {i} of {} stands {d} mm past lim", p.frond, pts.len());
                        if i > 0 {
                            for t in 1..8 {
                                worst = worst.max(near.nearest(lerp(pts[i - 1], q, t as f64 / 8.0), LIM).0 - LIM - HANG - HANG_STEP / 2.0);
                            }
                        }
                    }
                }
            }
            assert!(worst < 1e-5, "{name}: a span stands {worst} mm past lim");
        }
    }

    /// A C, a turn with its first quarter missing, under a stem that wants the whole turn: each
    /// layer its open end grows on towards the turn's start, by at most lim, until it closes. A C
    /// open in its middle closes from both ends, each a step a layer.
    #[test]
    fn a_c_closes_from_both_ends_one_step_a_layer() {
        let keep = Keep::new(std::iter::once(Vec::new()));
        let full = TURN.points((0.0, 0.0), 3.0, 0.0, 0.0, 0.9);
        let mut below = vec![Line::of(&full[24..], 24)];
        let mut left = dist(below[0].pts[0], full[0]);
        let mut layers = 0;
        while left > 1e-9 {
            let (_, out) = print(full.clone(), Some(&below), &keep, &Beside::default(), 0, LIM, LW);
            let main = out.iter().max_by_key(|l| l.pts.len()).unwrap();
            assert!(out.iter().all(|l| std::ptr::eq(l, main) || len(&l.pts) < LW * 3.0), "after {layers} layers it broke in {}", out.len());
            assert_eq!(main.pts[main.pts.len() - 1], full[N - 1], "the far end is the turn's own end");
            let step = dist(below[0].pts[0], main.pts[0]);
            let now = dist(main.pts[0], full[0]);
            assert!(step <= LIM + 1e-9 && (now < 1e-9 || left - now > 0.5 * LIM), "layer {layers}: the end stepped {step} mm, from {left} to {now} short");
            left = now;
            layers += 1;
            assert!(layers < 60, "the C never closes");
            below = vec![main.clone()];
        }
        let arc = len(&full[..=24]);
        assert!(layers as f64 >= arc / LIM && (layers as f64) < 1.5 * arc / LIM + 1.0, "{layers} layers for {arc} mm");
        let mut below = vec![Line::of(&full[..30], 0), Line::of(&full[54..], 54)];
        let ends = |b: &[Line]| (b[0].pts[b[0].pts.len() - 1], b[1].pts[0]);
        for _ in 0..60 {
            let (_, out) = print(full.clone(), Some(&below), &keep, &Beside::default(), 0, LIM, LW);
            if out.len() == 1 {
                assert_eq!(out[0].pts.len(), N, "closed into the whole turn");
                return;
            }
            assert_eq!(out.len(), 2);
            let ((a0, b0), (a1, b1)) = (ends(&below), ends(&out));
            // Each end steps in by about lim, until the two meet.
            for (from, to) in [(a0, a1), (b0, b1)] {
                assert!(dist(from, to) <= LIM + 1e-9 && (dist(from, to) > 0.5 * LIM || dist(a0, b0) < 4.0 * LIM), "an end stepped {} mm with {} mm to go", dist(from, to), dist(a0, b0));
            }
            below = out;
        }
        panic!("the gap never closed");
    }

    /// At every fork, each kid's first layer stands only on what its parent printed of its own
    /// lobe: every point's nearest point of the parent lies on the kid's side of the waist.
    #[test]
    fn a_fork_child_stands_on_its_own_lobe() {
        let mut forks = 0;
        for name in ["branches", "pads", "shelf", "bar"] {
            let r = run(name);
            for (e, s) in r.plan.stems.iter().enumerate() {
                if s.kids.len() != 2 || s.rows.is_empty() {
                    continue;
                }
                let l = s.end();
                let u = (s.axis.cos(), s.axis.sin());
                let parent: Vec<P2> = r.body.layers[l - 1].iter().filter(|p| p.frond == e).flat_map(mm).collect();
                if parent.is_empty() {
                    continue;
                }
                // Along the axis, lobe A runs from the parent's far end over 2 ra, and the waist
                // is a bead wide past it.
                let t0 = parent.iter().map(|p| p.0 * u.0 + p.1 * u.1).fold(f64::INFINITY, f64::min);
                let waist = t0 + 2.0 * s.lobes.0 * (1.0 + TURN.lobe) + LW;
                let near = Near::new(std::iter::once(&parent[..]));
                for (side, &kid) in s.kids.iter().enumerate() {
                    for p in r.body.layers[l].iter().filter(|p| p.frond == kid) {
                        for q in mm(p) {
                            let (_, f) = near.nearest(q, 1.0);
                            let t = f.0 * u.0 + f.1 * u.1;
                            assert!(if side == 0 { t < waist } else { t > waist - 2.0 * LW }, "{name}: stem {e} kid {kid} stands across the waist ({t} vs {waist})");
                        }
                    }
                }
                forks += 1;
            }
        }
        assert!(forks >= 3, "only {forks} forks looked at");
    }

    /// A turn the part cuts in two: a short piece far from the main line goes; one as short but
    /// near it stays.
    #[test]
    fn a_scrap_the_part_cuts_off_is_dropped() {
        let full = TURN.points((0.0, 0.0), 3.0, 0.0, 0.0, 0.9);
        let far = Keep::new(std::iter::once(vec![rect(1.6, -10.0, 2.7, 10.0)]));
        let (_, out) = print(full.clone(), None, &far, &Beside::default(), 0, LIM, LW);
        assert!(!out.is_empty() && out.iter().all(|l| l.pts.iter().all(|p| p.0 < 1.6)), "only the main line stays: {:?}", out.iter().map(|l| l.pts.len()).collect::<Vec<_>>());
        let near = Keep::new(std::iter::once(vec![rect(2.75, -10.0, 2.85, 10.0)]));
        let (_, out) = print(full, None, &near, &Beside::default(), 0, LIM, LW);
        assert!(out.iter().any(|l| l.pts.iter().all(|p| p.0 > 2.85)), "the piece just past the band stays");
    }

    fn crosses(a: Pt, b: Pt, c: Pt, d: Pt) -> bool {
        let o = |p: Pt, q: Pt, r: Pt| ((q.x - p.x) as i128 * (r.y - p.y) as i128 - (q.y - p.y) as i128 * (r.x - p.x) as i128).signum();
        o(a, b, c) * o(a, b, d) < 0 && o(c, d, a) * o(c, d, b) < 0
    }

    /// No body line crosses itself or another on the six scenes, and none breaks what carries
    /// it, what keeps it off, its footing, or the bead clearance (C5).
    #[test]
    fn no_body_line_crosses_itself_or_another() {
        for name in SIX {
            let r = run(name);
            let faults = prove(Ctl::default(), &r.scene.legal(), &r.body.layers).unwrap();
            let bad: Vec<_> = faults.iter().filter(|f| matches!(f.contract, Contract::Carried | Contract::Legal | Contract::Footing | Contract::Clear)).collect();
            let told: Vec<String> = bad.iter().take(6).map(|f| format!("{} L{} p{}/{:?} '{}' by {:.3} at ({:.2},{:.2}) fronds {}/{}", f.contract.code(), f.layer, f.path, f.other, f.why, f.by, f.at.0, f.at.1, r.body.layers[f.layer][f.path].frond, f.other.map_or(usize::MAX, |o| r.body.layers[f.layer][o].frond))).collect();
            assert!(bad.is_empty(), "{name}: {} faults: {}", bad.len(), told.join("; "));
            for (l, layer) in r.body.layers.iter().enumerate() {
                let segs: Vec<(usize, usize, Pt, Pt)> = layer.iter().enumerate().flat_map(|(i, p)| p.pts.windows(2).enumerate().map(move |(j, w)| (i, j, w[0], w[1]))).collect();
                for (x, a) in segs.iter().enumerate() {
                    for b in &segs[x + 1..] {
                        if !(a.0 == b.0 && a.1.abs_diff(b.1) <= 1) {
                            assert!(!crosses(a.2, a.3, b.2, b.3), "{name}: layer {l}: paths {} and {} cross (stems {} and {}, segments {} and {} of {} and {}, at {:?})", a.0, b.0, layer[a.0].frond, layer[b.0].frond, a.1, b.1, layer[a.0].pts.len(), layer[b.0].pts.len(), (a.2.x_mm(), a.2.y_mm()));
                        }
                    }
                }
            }
        }
    }

    /// Every support's printed body, as a column from its footing to its highest contact, bends
    /// less under the nozzle's push than the wobble allows: drag / E · ∫ (top - z)² / I(z) dz,
    /// I from what the support prints on each layer.
    #[test]
    fn the_body_bends_less_than_the_wobble_allows() {
        let c = Coral::default();
        for name in SIX {
            let r = run(name);
            for (si, &root) in r.plan.roots.iter().enumerate() {
                let top = r.plan.stems[root].top;
                let mut sum = 0.0;
                for (l, layer) in r.body.layers.iter().enumerate() {
                    let (z, h) = r.scene.heights[l];
                    if z >= top {
                        break;
                    }
                    let lines: Vec<Vec<P2>> = layer.iter().filter(|p| p.support == si).map(mm).collect();
                    let i = imin(lines.iter().map(|v| &v[..]), LW);
                    if i > 0.0 {
                        sum += (top - z).powi(2) / i * h;
                    }
                }
                let wobble = c.drag / c.modulus * sum;
                assert!(wobble <= c.wobble * 1.05, "{name}: support {si} wobbles {wobble} mm");
            }
        }
    }
}
