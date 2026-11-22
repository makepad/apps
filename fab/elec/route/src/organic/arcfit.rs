// Ported from TiNRS Psychedelic Router 0.1.0: src/core/ArcFit.h and ArcFit.cpp (36-967).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Exported polylines fitted to circular arcs, octilinear legs or simplified segments.
//! Width changes and T-junctions split runs; arc ends avoid same-net pad and via copper.
//!
//! ADAPTED: board_primitives takes a per-trace violation counter in place of
//! perTrace(Drc::run(geomBoard(...), dopt)); None disables the DRC guard. The geometry
//! board, Segments baseline, level loop and statistics are unchanged. Layer membership
//! uses copper::pad_on_layer / via_on_layer and the port's OBoard fields.
//! CLI trackStyleName / parseTrackStyle are intentionally omitted. C++ default arguments
//! are explicit; the second fitOctilinear overload is fit_octilinear_legs. Run callbacks
//! take the output vector explicitly to avoid overlapping mutable Rust borrows.

use super::copper::{pad_on_layer, via_on_layer};
use super::geom::{std_clamp, std_max, std_min, Segment2, Vec2};
use super::model::{OBoard, Pad, Trace, Via};
use std::collections::BTreeMap;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TrackStyle {
    Arcs,
    Octilinear,
    Segments,
}

#[derive(Copy, Clone, Debug, Default)]
pub struct TrackPrim {
    pub is_arc: bool,
    pub a: Vec2,
    /// A point halfway along the arc; only used for arcs.
    pub mid: Vec2,
    pub b: Vec2,
    pub width: f32,
}

#[derive(Clone, Debug)]
pub struct ArcFitOptions {
    /// Arcs: maximum two-way deviation in mm.
    pub tol: f32,
    pub oct_tol: f32,
    pub oct_leg: f32,
    pub oct_staircase: bool,
    pub oct_coarse: bool,
    pub oct_coarse_dp: f32,
    pub oct_coarse_leg: f32,
    pub max_sweep_deg: f32,
    pub max_radius: f32,
    pub min_arc_sagitta: f32,
    pub drc_guard: bool,
}

impl Default for ArcFitOptions {
    fn default() -> Self {
        Self {
            tol: 0.005f32,
            oct_tol: 0.02f32,
            oct_leg: 0.1f32,
            oct_staircase: false,
            oct_coarse: true,
            oct_coarse_dp: 0.05f32,
            oct_coarse_leg: 0.5f32,
            max_sweep_deg: 170.0f32,
            max_radius: 100.0f32,
            min_arc_sagitta: 5e-4f32,
            drc_guard: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ArcFitStats {
    pub traces: i32,
    pub segments: i32,
    pub arcs: i32,
    pub guard_refit: i32,
    pub guard_fallback: i32,
}

pub type PointPredicate<'a> = dyn Fn(&Vec2) -> bool + 'a;

const K_PI_D: f64 = 3.14159265358979323846;

#[derive(Copy, Clone, Default)]
struct D2 {
    x: f64,
    y: f64,
}

fn d2(v: Vec2) -> D2 { D2 { x: v.x as f64, y: v.y as f64 } }
fn dot(a: D2, b: D2) -> f64 { a.x * b.x + a.y * b.y }
fn cross(a: D2, b: D2) -> f64 { a.x * b.y - a.y * b.x }
fn sub(a: D2, b: D2) -> D2 { D2 { x: a.x - b.x, y: a.y - b.y } }
fn len(a: D2) -> f64 { dot(a, a).sqrt() }

fn point_seg(p: D2, a: D2, b: D2) -> f64 {
    let ab = sub(b, a);
    let ap = sub(p, a);
    let l2 = dot(ab, ab);
    let t = if l2 > 1e-24 { std_clamp(dot(ap, ab) / l2, 0.0, 1.0) } else { 0.0 };
    let q = D2 { x: a.x + ab.x * t, y: a.y + ab.y * t };
    len(sub(p, q))
}

fn wrap_pi(mut a: f64) -> f64 {
    while a > K_PI_D { a -= 2.0 * K_PI_D; }
    while a <= -K_PI_D { a += 2.0 * K_PI_D; }
    a
}

fn simplify_collinear(in_pts: &[Vec2], eps: f32) -> Vec<Vec2> {
    if in_pts.len() <= 2 { return in_pts.to_vec(); }
    let mut out = Vec::new();
    out.push(in_pts[0]);
    for i in 1..in_pts.len() - 1 {
        let prev = *out.last().unwrap();
        let curr = in_pts[i];
        let next = in_pts[i + 1];
        let d1 = curr - prev;
        let d2v = next - curr;
        let l1 = d1.length();
        let l2 = d2v.length();
        if l1 < 1e-5f32 || l2 < 1e-5f32 { continue; }
        let cr = d1.cross(d2v).abs() / (l1 * l2);
        let dt = d1.dot(d2v) / (l1 * l2);
        if cr < eps && dt > 0.999f32 { continue; }
        out.push(curr);
    }
    out.push(*in_pts.last().unwrap());
    out
}

fn dp_rec(p: &[Vec2], a: usize, b: usize, tol: f32, keep: &mut [u8]) {
    if b <= a + 1 { return; }
    let mut worst = -1.0f32;
    let mut wi = a;
    let s = Segment2::new(p[a], p[b]);
    for i in a + 1..b {
        let d = s.distance_to_point(p[i]);
        if d > worst { worst = d; wi = i; }
    }
    if worst > tol {
        keep[wi] = 1;
        dp_rec(p, a, wi, tol, keep);
        dp_rec(p, wi, b, tol, keep);
    }
}

fn straighten_stubs(tr: &Trace, pts: &mut Vec<Vec2>, widths: &mut Vec<f32>) {
    let tol2 = 2e-3f32 * 2e-3f32;
    let mut cursor = 0;
    for k in 1..tr.points.len() {
        if !tr.points[k].straight_before { continue; }
        let a = tr.points[k - 1].pos;
        let b = tr.points[k].pos;
        let mut ia = cursor;
        while ia < pts.len() && pts[ia].distance_squared_to(a) > tol2 { ia += 1; }
        if ia >= pts.len() { continue; }
        let mut ib = ia + 1;
        while ib < pts.len() && pts[ib].distance_squared_to(b) > tol2 { ib += 1; }
        if ib >= pts.len() { continue; }
        if ib > ia + 1 {
            drop(pts.drain(ia + 1..ib));
            if widths.len() > ia + 1 && widths.len() >= ib { drop(widths.drain(ia + 1..ib)); }
        }
        pts[ia] = a;
        pts[ia + 1] = b;
        cursor = ia + 1;
    }
}

fn decimate(pts: &mut Vec<Vec2>, widths: &mut Vec<f32>, tol: f32) {
    if pts.len() <= 2 { return; }
    let mut keep = vec![0u8; pts.len()];
    keep[0] = 1;
    keep[pts.len() - 1] = 1;
    let mut start = 0;
    for i in 1..pts.len() {
        let brk = i + 1 == pts.len() || (!widths.is_empty() && (widths[i] - widths[start]).abs() > 1e-4f32);
        if !brk { continue; }
        keep[i] = 1;
        dp_rec(pts, start, i, tol, &mut keep);
        start = i;
    }
    let mut np = Vec::new();
    let mut nw = Vec::new();
    for i in 0..pts.len() {
        if keep[i] == 0 { continue; }
        np.push(pts[i]);
        if !widths.is_empty() { nw.push(widths[i]); }
    }
    std::mem::swap(pts, &mut np);
    if !widths.is_empty() { std::mem::swap(widths, &mut nw); }
}

struct Fitter<'a> {
    p: &'a [Vec2],
    tol: f64,
    opt: &'a ArcFitOptions,
    no_arc_end: Option<&'a PointPredicate<'a>>,
}

impl Fitter<'_> {
    /// Primitive through p[i]..p[j] (i < j) within tol, or false.
    fn fit(&self, i: usize, j: usize, out: &mut TrackPrim) -> bool {
        let p = self.p;
        let tol = self.tol;
        let opt = self.opt;
        let a = d2(p[i]);
        let b = d2(p[j]);
        let ab = sub(b, a);
        let l = len(ab);
        if l < 1e-9 { return false; }
        out.a = p[i];
        out.b = p[j];
        {
            let mut ok = true;
            let mut prev_t = 0.0;
            let mut k = i + 1;
            while k < j && ok {
                let q = sub(d2(p[k]), a);
                let t = dot(q, ab) / l;
                let d = cross(ab, q).abs() / l;
                if d > tol || t < prev_t - tol || t > l + tol { ok = false; }
                prev_t = std_max(prev_t, t);
                k += 1;
            }
            if ok { out.is_arc = false; return true; }
        }
        if j < i + 2 { return false; }
        if let Some(no_arc_end) = self.no_arc_end {
            if no_arc_end(&p[i]) || no_arc_end(&p[j]) { return false; }
        }
        // Circle through a and b; algebraic least squares on the chord bisector.
        let m = D2 { x: (a.x + b.x) * 0.5, y: (a.y + b.y) * 0.5 };
        let n = D2 { x: -ab.y / l, y: ab.x / l };
        let mut num = 0.0;
        let mut den = 0.0;
        let am2 = dot(sub(a, m), sub(a, m));
        for k in i + 1..j {
            let q = d2(p[k]);
            let ak = dot(sub(q, m), sub(q, m)) - am2;
            let bk = 2.0 * dot(n, sub(q, a));
            num += ak * bk;
            den += bk * bk;
        }
        if den < 1e-24 { return false; }
        let t = num / den;
        let c = D2 { x: m.x + n.x * t, y: m.y + n.y * t };
        let r = len(sub(a, c));
        if !(r > 1e-6) || r > opt.max_radius as f64 { return false; }
        let th0 = (a.y - c.y).atan2(a.x - c.x);
        let qm = d2(p[(i + j) / 2]);
        let aq = wrap_pi((qm.y - c.y).atan2(qm.x - c.x) - th0);
        let a_b = wrap_pi((b.y - c.y).atan2(b.x - c.x) - th0);
        let mut sweep = a_b;
        if !(aq * a_b > 0.0 && aq.abs() < a_b.abs()) {
            sweep = if a_b > 0.0 { a_b - 2.0 * K_PI_D } else { a_b + 2.0 * K_PI_D };
        }
        if sweep.abs() > opt.max_sweep_deg as f64 * K_PI_D / 180.0 { return false; }
        let sg = if sweep > 0.0 { 1.0 } else { -1.0 };
        let sw = sweep.abs();
        let mut prev_a = 0.0;
        let mut prev_d: f64 = 0.0;
        let ang_tol = tol / r;
        for k in i + 1..=j {
            let q = d2(p[k]);
            let cq = sub(q, c);
            let dk = len(cq) - r;
            let ak = if k == j { sw } else { wrap_pi(cq.y.atan2(cq.x) - th0) * sg };
            if dk.abs() > tol { return false; }
            if ak < prev_a - ang_tol || ak > sw + ang_tol { return false; }
            let da = std_max(0.0, ak - prev_a);
            let sag = r * (1.0 - (0.5 * da).cos());
            if std_max(dk.abs(), prev_d.abs()) + sag > tol { return false; }
            prev_a = std_max(prev_a, ak);
            prev_d = dk;
        }
        let sagitta = r * (1.0 - (0.5 * sw).cos());
        if sagitta < opt.min_arc_sagitta as f64 { return false; }
        let tm = th0 + sweep * 0.5;
        out.is_arc = true;
        out.mid = Vec2::new((c.x + r * tm.cos()) as f32, (c.y + r * tm.sin()) as f32);
        true
    }

    /// Greedy longest primitive, exponential + binary search, within [i, e].
    fn longest(&self, i: usize, e: usize, out: &mut TrackPrim) -> usize {
        let mut best = TrackPrim::default();
        self.fit(i, i + 1, &mut best);
        let mut good = i + 1;
        let mut step = 1;
        let mut bad = 0;
        while good < e {
            let j = std_min(e, i + step * 2);
            let mut tp = TrackPrim::default();
            if self.fit(i, j, &mut tp) {
                good = j;
                best = tp;
                step *= 2;
            } else {
                bad = j;
                break;
            }
        }
        if bad != 0 {
            let mut lo = good;
            let mut hi = bad;
            while hi > lo + 1 {
                let mid = (lo + hi) / 2;
                let mut tp = TrackPrim::default();
                if self.fit(i, mid, &mut tp) { lo = mid; best = tp; } else { hi = mid; }
            }
            good = lo;
            // The bound follows good as it advances, just as in the C++ for loop.
            let mut j = good + 2;
            while j <= std_min(e, good + 6) {
                let mut tp = TrackPrim::default();
                if self.fit(i, j, &mut tp) { good = j; best = tp; }
                j += 1;
            }
        }
        *out = best;
        good
    }
}

#[derive(Copy, Clone)]
struct Ranged {
    pr: TrackPrim,
    i: usize,
    j: usize,
}

fn fit_run(p: &[Vec2], s: usize, e: usize, width: f32, tol: f64, opt: &ArcFitOptions,
    no_arc_end: Option<&PointPredicate<'_>>, out: &mut Vec<TrackPrim>)
{
    if e <= s { return; }
    let f = Fitter { p, tol, opt, no_arc_end };
    let mut rs = Vec::new();
    let mut i = s;
    while i < e {
        let mut tp = TrackPrim::default();
        let j = f.longest(i, e, &mut tp);
        tp.width = width;
        rs.push(Ranged { pr: tp, i, j });
        i = j;
    }
    let mut changed = true;
    while changed && rs.len() > 1 {
        changed = false;
        let mut m: Vec<Ranged> = Vec::new();
        for k in 0..rs.len() {
            if let Some(last) = m.last_mut() {
                let mut tp = TrackPrim::default();
                if f.fit(last.i, rs[k].j, &mut tp) {
                    tp.width = width;
                    last.pr = tp;
                    last.j = rs[k].j;
                    changed = true;
                    continue;
                }
            }
            m.push(rs[k]);
        }
        std::mem::swap(&mut rs, &mut m);
    }
    for r in rs { out.push(r.pr); }
}

// A width break at i ends the run at i-1; i-1 -> i is a segment at min(w[i-1], w[i]).
fn for_runs(p: &[Vec2], w: &[f32], def_w: f32,
    mut run: impl FnMut(usize, usize, f32, &mut Vec<TrackPrim>), out: &mut Vec<TrackPrim>)
{
    let n = p.len();
    if n < 2 { return; }
    if w.len() != n { run(0, n - 1, def_w, out); return; }
    let min_w = |a: usize, b: usize| {
        let mut m = w[a];
        for k in a..=b { m = std_min(m, w[k]); }
        if m > 0.0f32 { m } else { def_w }
    };
    let mut start = 0;
    for i in 1..n {
        let wbrk = (w[i] - w[start]).abs() > 1e-4f32;
        if !wbrk && i + 1 < n { continue; }
        if !wbrk { run(start, i, min_w(start, i), out); break; }
        if i - 1 > start { run(start, i - 1, min_w(start, i - 1), out); }
        let mut sp = TrackPrim::default();
        sp.a = p[i - 1];
        sp.b = p[i];
        sp.width = min_w(i - 1, i);
        out.push(sp);
        start = i;
    }
}

fn dedupe(pts: &[Vec2], widths: &[f32], p: &mut Vec<Vec2>, w: &mut Vec<f32>) {
    p.clear();
    w.clear();
    let hw = widths.len() == pts.len();
    for k in 0..pts.len() {
        if !p.is_empty() && p.last().unwrap().distance_squared_to(pts[k]) < 1e-12f32 {
            if hw { let last = w.last_mut().unwrap(); *last = std_min(*last, widths[k]); }
            continue;
        }
        p.push(pts[k]);
        if hw { w.push(widths[k]); }
    }
}

const K_DIRS: [D2; 8] = [
    D2 { x: 1.0, y: 0.0 },
    D2 { x: 0.70710678118654752, y: 0.70710678118654752 },
    D2 { x: 0.0, y: 1.0 },
    D2 { x: -0.70710678118654752, y: 0.70710678118654752 },
    D2 { x: -1.0, y: 0.0 },
    D2 { x: -0.70710678118654752, y: -0.70710678118654752 },
    D2 { x: 0.0, y: -1.0 },
    D2 { x: 0.70710678118654752, y: -0.70710678118654752 },
];

// At most two octilinear legs. Order 0 is axis first, 1 diagonal first.
fn dogleg(a: D2, b: D2, order: i32, corner: &mut D2) -> f64 {
    let d = sub(b, a);
    let ax = d.x.abs();
    let ay = d.y.abs();
    let sx = if d.x >= 0.0 { 1.0 } else { -1.0 };
    let sy = if d.y >= 0.0 { 1.0 } else { -1.0 };
    let m = std_min(ax, ay);
    let diag = D2 { x: sx * m, y: sy * m };
    let straight = D2 {
        x: if ax > ay { sx * (ax - ay) } else { 0.0 },
        y: if ay > ax { sy * (ay - ax) } else { 0.0 },
    };
    let first = if order == 0 { straight } else { diag };
    *corner = D2 { x: a.x + first.x, y: a.y + first.y };
    let l = len(d);
    if l < 1e-12 { return 0.0; }
    cross(d, first).abs() / l
}

fn is_oct(d: D2, eps: f64) -> bool {
    let l = len(d);
    if l < 1e-12 { return true; }
    for u in K_DIRS {
        if cross(d, u).abs() <= eps && dot(d, u) > 0.0 { return true; }
    }
    false
}

fn oct_chord(p: &[Vec2], ia: usize, ib: usize, a: D2, b: D2, tol: f64, out: &mut Vec<D2>, depth: i32) {
    let d = sub(b, a);
    if len(d) < 1e-9 { return; }
    if is_oct(d, 1e-5) { out.push(b); return; }
    let mut c0 = D2::default();
    let mut c1 = D2::default();
    let h = dogleg(a, b, 0, &mut c0);
    dogleg(a, b, 1, &mut c1);
    if h <= tol || depth > 24 {
        let mut e0 = 0.0;
        let mut e1 = 0.0;
        for k in ia + 1..ib {
            let q = d2(p[k]);
            e0 = std_max(e0, std_min(point_seg(q, a, c0), point_seg(q, c0, b)));
            e1 = std_max(e1, std_min(point_seg(q, a, c1), point_seg(q, c1, b)));
        }
        let mut c = if e1 < e0 - 1e-9 { c1 } else { c0 };
        // Prefer an order continuing the previous leg, so they merge.
        if out.len() >= 2 {
            let pd = sub(*out.last().unwrap(), out[out.len() - 2]);
            let cont = |corner: D2| {
                let l = sub(corner, a);
                len(l) > 1e-9 && cross(pd, l).abs() <= 1e-9 * std_max(1.0, len(pd) * len(l)) && dot(pd, l) > 0.0
            };
            if cont(c0) { c = c0; } else if cont(c1) { c = c1; }
        }
        out.push(c);
        out.push(b);
        return;
    }
    let mut m;
    let im;
    if ib > ia + 1 {
        im = (ia + ib) / 2;
        m = d2(p[im]);
        let dn = D2 { x: d.x / len(d), y: d.y / len(d) };
        let t = dot(sub(m, a), dn);
        m = D2 { x: a.x + dn.x * t, y: a.y + dn.y * t };
    } else {
        im = ia;
        m = D2 { x: (a.x + b.x) * 0.5, y: (a.y + b.y) * 0.5 };
    }
    oct_chord(p, ia, im, a, m, tol, out, depth + 1);
    oct_chord(p, im, ib, m, b, tol, out, depth + 1);
}

fn oct_run(p: &[Vec2], s: usize, e: usize, width: f32, dp_tol: f64, leg_tol: f64, out: &mut Vec<TrackPrim>) {
    if e <= s { return; }
    let mut keep = vec![0u8; p.len()];
    keep[s] = 1;
    keep[e] = 1;
    dp_rec(p, s, e, dp_tol as f32, &mut keep);
    let mut v = vec![d2(p[s])];
    let mut prev = s;
    for k in s + 1..=e {
        if keep[k] == 0 { continue; }
        oct_chord(p, prev, k, d2(p[prev]), d2(p[k]), leg_tol, &mut v, 0);
        prev = k;
    }
    // Exact run endpoints; merge collinear legs.
    let mut m: Vec<D2> = Vec::new();
    for q in v {
        if !m.is_empty() && len(sub(q, *m.last().unwrap())) < 1e-7 { continue; }
        if m.len() >= 2 {
            let d1 = sub(*m.last().unwrap(), m[m.len() - 2]);
            let dd = sub(q, *m.last().unwrap());
            if cross(d1, dd).abs() < 1e-9 * std_max(1.0, len(d1) * len(dd)) && dot(d1, dd) > 0.0 {
                *m.last_mut().unwrap() = q;
                continue;
            }
        }
        m.push(q);
    }
    let mut k = 0;
    while k + 1 < m.len() {
        let mut tp = TrackPrim::default();
        tp.a = if k == 0 { p[s] } else { Vec2::new(m[k].x as f32, m[k].y as f32) };
        tp.b = if k + 2 == m.len() { p[e] } else { Vec2::new(m[k + 1].x as f32, m[k + 1].y as f32) };
        tp.width = width;
        k += 1;
        if tp.a.distance_squared_to(tp.b) < 1e-12f32 { continue; }
        out.push(tp);
    }
}

/// Smoothed or raw trace polyline; smoothed stubs are straightened. Widths may be empty.
pub fn export_polyline(tr: &Trace, pts: &mut Vec<Vec2>, widths: &mut Vec<f32>, smoothed: &mut bool) {
    pts.clear();
    widths.clear();
    *smoothed = tr.smoothed.len() >= 2;
    if *smoothed {
        *pts = tr.smoothed.clone();
        if tr.point_widths.len() == pts.len() { *widths = tr.point_widths.clone(); }
        straighten_stubs(tr, pts, widths);
    } else {
        for tp in &tr.points { pts.push(tp.pos); }
    }
}

/// Pre-P2 segments: straighten stubs, remove collinear samples, Douglas-Peucker at 2 um.
pub fn segments_of(tr: &Trace) -> Vec<TrackPrim> {
    let mut out = Vec::new();
    let mut pts = Vec::new();
    let mut widths = Vec::new();
    let mut sm = false;
    export_polyline(tr, &mut pts, &mut widths, &mut sm);
    if pts.len() < 2 { return out; }
    if widths.is_empty() { pts = simplify_collinear(&pts, 1e-4f32); }
    decimate(&mut pts, &mut widths, 0.002f32);
    for i in 0..pts.len() - 1 {
        if pts[i].distance_squared_to(pts[i + 1]) < 1e-8f32 { continue; }
        let mut w = if widths.is_empty() { tr.width } else { std_min(widths[i], widths[i + 1]) };
        if !(w > 0.0f32) { w = tr.width; }
        let mut tp = TrackPrim::default();
        tp.a = pts[i];
        tp.b = pts[i + 1];
        tp.width = w;
        out.push(tp);
    }
    out
}

// Project T-junctions onto the nearest chord, or snap within 0.1 um of a sample.
fn insert_breaks(p: &mut Vec<Vec2>, w: &mut Vec<f32>, breaks: &[Vec2], brk: &mut Vec<u8>) {
    brk.clear();
    brk.resize(p.len(), 0);
    if p.len() < 2 { return; }
    let hw = w.len() == p.len();
    for &q in breaks {
        let q = d2(q);
        let mut bk = 0;
        let mut bd = 1e300;
        let mut bt = 0.0;
        for k in 0..p.len() - 1 {
            let a = d2(p[k]);
            let ab = sub(d2(p[k + 1]), a);
            let l2 = dot(ab, ab);
            let t = if l2 > 1e-24 { std_clamp(dot(sub(q, a), ab) / l2, 0.0, 1.0) } else { 0.0 };
            let d = len(sub(q, D2 { x: a.x + ab.x * t, y: a.y + ab.y * t }));
            if d < bd { bd = d; bk = k; bt = t; }
        }
        let a = d2(p[bk]);
        let ab = sub(d2(p[bk + 1]), a);
        let pr = D2 { x: a.x + ab.x * bt, y: a.y + ab.y * bt };
        let snap = 1e-4;
        if len(sub(pr, a)) <= snap { brk[bk] = 1; }
        else if len(sub(pr, d2(p[bk + 1]))) <= snap { brk[bk + 1] = 1; }
        else {
            p.insert(bk + 1, Vec2::new(pr.x as f32, pr.y as f32));
            brk.insert(bk + 1, 1);
            if hw { w.insert(bk + 1, std_min(w[bk], w[bk + 1])); }
        }
    }
    brk[0] = 0;
    *brk.last_mut().unwrap() = 0;
}

fn split_run(brk: &[u8], s: usize, e: usize, wd: f32,
    mut run: impl FnMut(usize, usize, f32, &mut Vec<TrackPrim>), out: &mut Vec<TrackPrim>)
{
    let mut a = s;
    for k in s + 1..e {
        if k < brk.len() && brk[k] != 0 {
            run(a, k, wd, out);
            a = k;
        }
    }
    run(a, e, wd, out);
}

/// Fit variable-width samples; empty widths use default_width. A true no_arc_end
/// predicate forbids arc ends there. Each break_at projection becomes a primitive end.
pub fn fit_arcs(pts: &[Vec2], widths: &[f32], default_width: f32, tol: f32,
    opt: &ArcFitOptions, no_arc_end: Option<&PointPredicate<'_>>, break_at: &[Vec2]) -> Vec<TrackPrim>
{
    let mut p = Vec::new();
    let mut w = Vec::new();
    dedupe(pts, widths, &mut p, &mut w);
    let mut brk = Vec::new();
    insert_breaks(&mut p, &mut w, break_at, &mut brk);
    let mut out = Vec::new();
    for_runs(&p, &w, default_width, |s, e, wd, out| {
        split_run(&brk, s, e, wd, |a, b, x, out| {
            fit_run(&p, a, b, x, tol as f64, opt, no_arc_end, out);
        }, out);
    }, &mut out);
    out
}

/// Octilinear fit using half tol for simplification and half for dogleg offsets.
pub fn fit_octilinear(pts: &[Vec2], widths: &[f32], default_width: f32, tol: f32) -> Vec<TrackPrim> {
    fit_octilinear_legs(pts, widths, default_width, tol * 0.5f32, tol * 0.5f32, &[])
}

/// The second C++ fitOctilinear overload: separate DP and dogleg tolerances.
pub fn fit_octilinear_legs(pts: &[Vec2], widths: &[f32], default_width: f32,
    dp_tol: f32, leg_tol: f32, break_at: &[Vec2]) -> Vec<TrackPrim>
{
    let mut p = Vec::new();
    let mut w = Vec::new();
    dedupe(pts, widths, &mut p, &mut w);
    let mut brk = Vec::new();
    insert_breaks(&mut p, &mut w, break_at, &mut brk);
    let mut out = Vec::new();
    for_runs(&p, &w, default_width, |s, e, wd, out| {
        split_run(&brk, s, e, wd, |a, b, x, out| {
            oct_run(&p, a, b, x, dp_tol as f64, leg_tol as f64, out);
        }, out);
    }, &mut out);
    out
}

// Fit ladder; level >= levels(style) is Segments.
fn levels(style: TrackStyle, opt: &ArcFitOptions) -> i32 {
    if style == TrackStyle::Arcs { return 2; }
    if style == TrackStyle::Octilinear {
        return (if opt.oct_coarse { 1 } else { 0 }) + 1 + (if opt.oct_staircase { 1 } else { 0 });
    }
    0
}

fn styled(tr: &Trace, style: TrackStyle, mut level: i32, opt: &ArcFitOptions,
    no_arc_end: Option<&PointPredicate<'_>>, break_at: &[Vec2]) -> Vec<TrackPrim>
{
    if level >= levels(style, opt) || tr.smoothed.len() < 2 { return segments_of(tr); }
    let mut pts = Vec::new();
    let mut widths = Vec::new();
    let mut sm = false;
    export_polyline(tr, &mut pts, &mut widths, &mut sm);
    if pts.len() < 2 { return Vec::new(); }
    if style == TrackStyle::Arcs {
        return fit_arcs(&pts, &widths, tr.width, if level == 0 { opt.tol } else { opt.tol * 0.5f32 }, opt, no_arc_end, break_at);
    }
    if !opt.oct_coarse { level += 1; }
    if level == 0 { return fit_octilinear_legs(&pts, &widths, tr.width, opt.oct_coarse_dp, opt.oct_coarse_leg, break_at); }
    if level == 1 { return fit_octilinear_legs(&pts, &widths, tr.width, opt.oct_tol, opt.oct_leg, break_at); }
    fit_octilinear_legs(&pts, &widths, tr.width, opt.oct_tol * 0.5f32, opt.oct_tol * 0.5f32, break_at)
}

/// One trace in a style, without a DRC guard. Raw routes always use segments_of.
pub fn trace_primitives(tr: &Trace, style: TrackStyle, opt: &ArcFitOptions) -> Vec<TrackPrim> {
    styled(tr, style, 0, opt, None, &[])
}

/// Circle through three points; false when nearly collinear or the radius is not finite.
pub fn circle_from_3(a: Vec2, m: Vec2, b: Vec2, cx: &mut f64, cy: &mut f64, r: &mut f64) -> bool {
    let a = d2(a);
    let m = d2(m);
    let b = d2(b);
    let d = 2.0 * (a.x * (m.y - b.y) + m.x * (b.y - a.y) + b.x * (a.y - m.y));
    if d.abs() < 1e-14 { return false; }
    let a2 = dot(a, a);
    let m2 = dot(m, m);
    let b2 = dot(b, b);
    *cx = (a2 * (m.y - b.y) + m2 * (b.y - a.y) + b2 * (a.y - m.y)) / d;
    *cy = (a2 * (b.x - m.x) + m2 * (a.x - b.x) + b2 * (m.x - a.x)) / d;
    *r = (a.x - *cx).hypot(a.y - *cy);
    r.is_finite()
}

fn arc_params(pr: &TrackPrim, cx: &mut f64, cy: &mut f64, r: &mut f64, th0: &mut f64, sweep: &mut f64) -> bool {
    if !circle_from_3(pr.a, pr.mid, pr.b, cx, cy, r) { return false; }
    *th0 = (pr.a.y as f64 - *cy).atan2(pr.a.x as f64 - *cx);
    let thm = wrap_pi((pr.mid.y as f64 - *cy).atan2(pr.mid.x as f64 - *cx) - *th0);
    let mut thb = wrap_pi((pr.b.y as f64 - *cy).atan2(pr.b.x as f64 - *cx) - *th0);
    if thm > 0.0 && thb < thm { thb += 2.0 * K_PI_D; }
    else if thm < 0.0 && thb > thm { thb -= 2.0 * K_PI_D; }
    *sweep = thb;
    true
}

/// Distance from p to the primitive's centreline.
pub fn distance_to(p: Vec2, pr: &TrackPrim) -> f64 {
    let q = d2(p);
    let (mut cx, mut cy, mut r, mut th0, mut sw) = (0.0, 0.0, 0.0, 0.0, 0.0);
    if !pr.is_arc || !arc_params(pr, &mut cx, &mut cy, &mut r, &mut th0, &mut sw) {
        return point_seg(q, d2(pr.a), d2(pr.b));
    }
    let ang = wrap_pi((q.y - cy).atan2(q.x - cx) - th0);
    let mut s = if sw >= 0.0 { ang } else { -ang };
    if s < 0.0 { s += 2.0 * K_PI_D; }
    if s <= sw.abs() { return ((q.x - cx).hypot(q.y - cy) - r).abs(); }
    std_min(len(sub(q, d2(pr.a))), len(sub(q, d2(pr.b))))
}

/// Tessellate arcs with the reference's sagitta bound and 4096-piece cap. Optional
/// vertex widths follow the min(w[i], w[i+1]) convention for each piece.
pub fn tessellate(prims: &[TrackPrim], sagitta: f64, pts: &mut Vec<Vec2>, mut widths: Option<&mut Vec<f32>>) {
    pts.clear();
    if let Some(widths) = widths.as_deref_mut() { widths.clear(); }
    for pr in prims {
        let mut piece = Vec::new();
        let (mut cx, mut cy, mut r, mut th0, mut sw) = (0.0, 0.0, 0.0, 0.0, 0.0);
        if pr.is_arc && arc_params(pr, &mut cx, &mut cy, &mut r, &mut th0, &mut sw) {
            let max_step = if sagitta < r { 2.0 * (1.0 - sagitta / r).acos() } else { K_PI_D / 4.0 };
            let mut n = std_max(2, (sw.abs() / std_max(1e-4, max_step)).ceil() as i32);
            n = std_min(n, 4096);
            piece.push(pr.a);
            for k in 1..n {
                let t = th0 + sw * k as f64 / n as f64;
                piece.push(Vec2::new((cx + r * t.cos()) as f32, (cy + r * t.sin()) as f32));
            }
            piece.push(pr.b);
        } else {
            piece = vec![pr.a, pr.b];
        }
        let joined = !pts.is_empty() && pts.last().unwrap().distance_squared_to(piece[0]) < 1e-10f32;
        if let Some(widths) = widths.as_deref_mut() {
            if joined {
                let last = widths.last_mut().unwrap();
                *last = std_max(*last, pr.width);
            } else if !pts.is_empty() {
                // Gap (should not happen): bridge it at this width.
            }
        }
        for k in (if joined { 1 } else { 0 })..piece.len() {
            pts.push(piece[k]);
            if let Some(widths) = widths.as_deref_mut() { widths.push(pr.width); }
        }
    }
}

/// Two-way deviation: samples to primitives and a dense primitive tessellation to samples.
pub fn deviation(samples: &[Vec2], prims: &[TrackPrim]) -> f64 {
    let mut worst = 0.0;
    if samples.is_empty() || prims.is_empty() { return if samples.is_empty() { 0.0 } else { 1e9 }; }
    for &s in samples {
        let mut best = 1e300;
        for pr in prims { best = std_min(best, distance_to(s, pr)); }
        worst = std_max(worst, best);
    }
    let mut t = Vec::new();
    tessellate(prims, 1e-5, &mut t, None);
    // The reference comments "also midpoints" here, but iterates only vertices.
    for k in 0..t.len() {
        let q = d2(t[k]);
        let mut best = 1e300;
        for i in 0..samples.len() - 1 { best = std_min(best, point_seg(q, d2(samples[i]), d2(samples[i + 1]))); }
        if samples.len() == 1 { best = len(sub(q, d2(samples[0]))); }
        worst = std_max(worst, best);
    }
    worst
}

/// Every trace in board order; unrouted traces have empty primitive lists.
///
/// ADAPTED: count replaces perTrace(Drc::run(geomBoard(...), dopt)) at both call
/// sites. It must return one count per trace, in board order, using the supplied
/// smoothed geometry and widths. The caller supplies the reference's violation
/// policy (no connectivity; exclude Unconnected, DanglingVia, PlaneIntrusion and
/// HoleClearance; zero tolerance). None disables the guard, as does !opt.drc_guard.
/// C++ capped reports at 1,000,000 violations and counted both matching trace IDs;
/// that report policy and ID-to-index mapping now belong to the counter's caller.
/// The Segments c0 baseline, refit levels, fallback and statistics are unchanged.
pub fn board_primitives(
    b: &OBoard,
    style: TrackStyle,
    opt: &ArcFitOptions,
    stats: Option<&mut ArcFitStats>,
    count: Option<&dyn Fn(&OBoard) -> Vec<i32>>,
) -> Vec<Vec<TrackPrim>> {
    let t_count = b.traces.len();
    let mut out = vec![Vec::new(); t_count];
    let mut st = ArcFitStats::default();
    let mut level = vec![0; t_count];
    // Arc ends must avoid same-net pad / via copper.
    let mut pads_by_net: BTreeMap<i32, Vec<&Pad>> = BTreeMap::new();
    let mut vias_by_net: BTreeMap<i32, Vec<&Via>> = BTreeMap::new();
    for p in &b.pads {
        if p.net >= 0 { pads_by_net.entry(p.net).or_default().push(p); }
    }
    for v in &b.vias {
        if v.net >= 0 { vias_by_net.entry(v.net).or_default().push(v); }
    }
    let n_l = std_max(2, b.layers.len() as i32);
    // Other same-net trace ends on this layer, within half this trace's width,
    // must meet a primitive end (except at this trace's own endpoints).
    let mut breaks: Vec<Vec<Vec2>> = vec![Vec::new(); t_count];
    if style != TrackStyle::Segments {
        #[derive(Clone, Default)]
        struct Poly {
            p: Vec<Vec2>,
            lo: Vec2,
            hi: Vec2,
        }
        let mut polys = vec![Poly::default(); t_count];
        let mut groups: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
        for t in 0..t_count {
            let tr = &b.traces[t];
            if !tr.is_routed || tr.net < 0 { continue; }
            let mut wd = Vec::new();
            let mut sm = false;
            export_polyline(tr, &mut polys[t].p, &mut wd, &mut sm);
            if polys[t].p.len() < 2 { continue; }
            let mut lo = polys[t].p[0];
            let mut hi = lo;
            for q in &polys[t].p {
                lo = Vec2::new(std_min(lo.x, q.x), std_min(lo.y, q.y));
                hi = Vec2::new(std_max(hi.x, q.x), std_max(hi.y, q.y));
            }
            polys[t].lo = lo;
            polys[t].hi = hi;
            groups.entry((tr.net, tr.layer)).or_default().push(t);
        }
        for ids in groups.values() {
            if ids.len() < 2 { continue; }
            for &t in ids {
                let p = &polys[t];
                let r = 0.5f32 * b.traces[t].width;
                for &o in ids {
                    if o == t { continue; }
                    for q in [polys[o].p[0], *polys[o].p.last().unwrap()] {
                        if q.x < p.lo.x - r || q.y < p.lo.y - r || q.x > p.hi.x + r || q.y > p.hi.y + r { continue; }
                        if q.distance_to(p.p[0]) <= 1e-3f32 || q.distance_to(*p.p.last().unwrap()) <= 1e-3f32 { continue; }
                        let mut d = 1e300;
                        for k in 0..p.p.len() - 1 { d = std_min(d, point_seg(d2(q), d2(p.p[k]), d2(p.p[k + 1]))); }
                        if d > r as f64 { continue; }
                        let mut dup = false;
                        for x in &breaks[t] { dup = dup || x.distance_to(q) <= 1e-4f32; }
                        if !dup { breaks[t].push(q); }
                    }
                }
            }
        }
    }
    // Explicit level/out arguments replace the C++ lambda's mutable captures.
    let build = |t: usize, level: &[i32], out: &mut Vec<Vec<TrackPrim>>| {
        let tr = &b.traces[t];
        if !tr.is_routed { out[t].clear(); return; }
        let pads = pads_by_net.get(&tr.net);
        let vias = vias_by_net.get(&tr.net);
        let on_copper = |q: &Vec2| {
            let eps = 1e-3f32;
            if let Some(pads) = pads {
                for p in pads {
                    if !pad_on_layer(p, tr.layer, n_l) { continue; }
                    // Conservative board-frame bounding box, including rotated pads.
                    let mut hs = p.size * 0.5f32;
                    if p.local_size.x > 0.0 && p.local_size.y > 0.0 {
                        let r = 0.5f32 * p.local_size.x.hypot(p.local_size.y);
                        hs = Vec2::new(std_max(hs.x, r), std_max(hs.y, r));
                    }
                    if (q.x - p.center.x).abs() <= hs.x + eps && (q.y - p.center.y).abs() <= hs.y + eps { return true; }
                }
            }
            if let Some(vias) = vias {
                for v in vias {
                    if via_on_layer(v, tr.layer) && q.distance_to(v.pos) <= 0.5f32 * v.diameter + eps { return true; }
                }
            }
            false
        };
        out[t] = styled(tr, style, level[t], opt, Some(&on_copper), &breaks[t]);
    };
    for t in 0..t_count { build(t, &level, &mut out); }

    if style != TrackStyle::Segments && opt.drc_guard && t_count > 0 {
        if let Some(count) = count {
            // ADAPTED: DrcOptions and perTrace are the injected counter's policy.
            let geom_board = |prims: &[Vec<TrackPrim>]| {
                let mut g = b.clone();
                for t in 0..t_count {
                    let tr = &mut g.traces[t];
                    if !tr.is_routed || prims[t].is_empty() { continue; }
                    tessellate(&prims[t], 2e-4, &mut tr.smoothed, Some(&mut tr.point_widths));
                }
                g
            };
            let mut base = vec![Vec::new(); t_count];
            for t in 0..t_count {
                if b.traces[t].is_routed { base[t] = segments_of(&b.traces[t]); }
            }
            let c0 = count(&geom_board(&base));
            let seg_level = levels(style, opt);
            for iter in 0..=seg_level {
                let c = count(&geom_board(&out));
                let mut any = false;
                for t in 0..t_count {
                    if c[t] <= c0[t] || level[t] >= seg_level { continue; }
                    // Last round: straight to Segments (baseline geometry).
                    level[t] = if iter == seg_level { seg_level } else { level[t] + 1 };
                    if level[t] < seg_level { st.guard_refit += 1; }
                    else { st.guard_fallback += 1; }
                    build(t, &level, &mut out);
                    any = true;
                }
                if !any { break; }
            }
        }
    }
    for t in 0..t_count {
        if !b.traces[t].is_routed { continue; }
        st.traces += 1;
        for p in &out[t] {
            if p.is_arc { st.arcs += 1; } else { st.segments += 1; }
        }
    }
    if let Some(stats) = stats { *stats = st; }
    out
}
