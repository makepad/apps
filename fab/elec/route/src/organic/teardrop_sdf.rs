// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/TeardropSdf.h and TeardropSdf.cpp.
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Teardrops from distance fields, for pads and vias. Each flank blends the junction's landing
//! feature and a calmed track edge: (1 - a/ka)^2 + (1 - b/kb)^2 = 1. A predictor/corrector
//! walks the contour; a smooth maximum with foreign copper bends it along the clearance line.
//! The landing follows the outline up to the wanted height or the tip's visibility limit.
//! Short track pieces are smoothed away; a taut edge keeps the real track inside the flank.
//! A side that cannot land tries a lower or shorter flank. Other teardrops are excluded from
//! the foreign field; the caller checks polygon simplicity and clearance afterwards.

use std::cell::Cell;
use std::ops::{Add, Mul, Sub};

use super::geom::{std_clamp, std_max, std_min, Vec2};
use super::padgeom::{self, Frame};

#[derive(Clone, Debug, Default)]
pub struct Conn<'a> {
    /// From the junction outwards.
    pub path: Option<&'a [Vec2]>,
    pub width: Option<&'a [f32]>,
    /// Arc length where the path leaves the junction.
    pub s_x: f64,
    /// Arc length of the polygon's closing point inside the junction.
    pub s_root: f64,
    /// Tip station = s_x + length.
    pub length: f64,
    /// Bridge tip station = s_x + bridge_length (0: length).
    pub bridge_length: f64,
    /// Full width of the teardrop at the outline.
    pub width2: f64,
}

pub struct Input<'a> {
    /// None selects the via below.
    pub pad: Option<&'a Frame<'a>>,
    pub via_centre: Vec2,
    pub via_radius: f64,
    /// From the junction outwards.
    pub path: Option<&'a [Vec2]>,
    pub width: Option<&'a [f32]>,
    /// Arc length where the path leaves the junction.
    pub s_x: f64,
    /// Arc length of the polygon's closing point inside the junction.
    pub s_root: f64,
    /// Tip station = s_x + length.
    pub length: f64,
    /// Full width of the teardrop at the outline.
    pub width2: f64,
    /// Multiple connections to this junction (when >= 2, all run at once).
    pub conns: Vec<Conn<'a>>,
    pub this_conn: i32,
    /// Distance left to foreign copper's clearance (< 0: too close); None: nothing near.
    pub foreign: Option<&'a dyn Fn(Vec2) -> f64>,
    /// Close through the pad's middle if the path's root is shallow.
    pub deep_root: bool,
    /// Extend a flank that stops in the air past a receding corner to the outline.
    pub bridge: bool,
}

impl Default for Input<'_> {
    fn default() -> Self {
        Self {
            pad: None, via_centre: Vec2::default(), via_radius: 0.0, path: None, width: None,
            s_x: 0.0, s_root: 0.0, length: 0.0, width2: 0.0, conns: Vec::new(), this_conn: 0,
            foreign: None, deep_root: true, bridge: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Result {
    /// Implicitly closed ring: the last vertex connects to the first.
    pub poly: Vec<Vec2>,
    /// Largest half width at the outline, from the path.
    pub flare: f64,
}

/// Fraction of full height and length; height 0 follows the path's centreline.
#[derive(Copy, Clone, Debug)]
pub struct Scale {
    pub height: f64,
    pub length: f64,
}

impl Default for Scale {
    fn default() -> Self { Self { height: 1.0, length: 1.0 } }
}

#[derive(Copy, Clone, Default)]
struct P {
    x: f64,
    y: f64,
}
impl Add for P {
    type Output = P;
    #[inline]
    fn add(self, b: P) -> P { P { x: self.x + b.x, y: self.y + b.y } }
}
impl Sub for P {
    type Output = P;
    #[inline]
    fn sub(self, b: P) -> P { P { x: self.x - b.x, y: self.y - b.y } }
}
impl Mul<f64> for P {
    type Output = P;
    #[inline]
    fn mul(self, k: f64) -> P { P { x: self.x * k, y: self.y * k } }
}
#[inline]
fn dot(a: P, b: P) -> f64 { a.x * b.x + a.y * b.y }
#[inline]
fn cross(a: P, b: P) -> f64 { a.x * b.y - a.y * b.x }
#[inline]
fn len(a: P) -> f64 { dot(a, a).sqrt() }
#[inline]
fn unit(a: P) -> P {
    let l = len(a);
    if l > 1e-12 { a * (1.0 / l) } else { P { x: 1.0, y: 0.0 } }
}
#[inline]
fn to_p(v: Vec2) -> P { P { x: v.x as f64, y: v.y as f64 } }

// Signed distance to the junction's outline.
fn junction_dist(input: &Input<'_>, q: P) -> f64 {
    let Some(f) = input.pad else { return len(q - to_p(input.via_centre)) - input.via_radius; };
    let (lx, ly) = padgeom::to_local(f, q.x, q.y);
    if let Some(poly) = f.poly { return padgeom::poly_signed(poly, lx + f.ox, ly + f.oy); }
    let dx = lx.abs() - (f.hx - f.r);
    let dy = ly.abs() - (f.hy - f.r);
    let ox = std_max(dx, 0.0);
    let oy = std_max(dy, 0.0);
    (ox * ox + oy * oy).sqrt() + std_min(std_max(dx, dy), 0.0) - f.r
}

fn junction_grad(input: &Input<'_>, q: P) -> P {
    if input.pad.is_none() { return unit(q - to_p(input.via_centre)); }
    let h = 1e-4;
    unit(P {
        x: junction_dist(input, q + P { x: h, y: 0.0 }) - junction_dist(input, q - P { x: h, y: 0.0 }),
        y: junction_dist(input, q + P { x: 0.0, y: h }) - junction_dist(input, q - P { x: 0.0, y: h }),
    })
}

// The outline as a dense ring (the walk that finds how far a side may land).
fn outline(input: &Input<'_>, r_out: &mut Vec<P>) {
    r_out.clear();
    let Some(f) = input.pad else {
        let n = ((6.2832 * input.via_radius / 0.05).ceil() as i32).clamp(24, 96);
        for i in 0..n {
            let t = 6.283185307179586 * i as f64 / n as f64;
            r_out.push(to_p(input.via_centre) + P { x: t.cos(), y: t.sin() } * input.via_radius);
        }
        return;
    };
    let mut v = Vec::new(); // Corners / arc points in the local frame, then subdivided.
    if let Some(poly) = f.poly {
        for q in poly { v.push(P { x: q.x as f64 - f.ox, y: q.y as f64 - f.oy }); }
    } else {
        let r = std_min(std_min(f.r, f.hx), f.hy);
        let ix = f.hx - r;
        let iy = f.hy - r;
        let cx = [ix, -ix, -ix, ix];
        let cy = [iy, iy, -iy, -iy];
        let m = if r > 1e-6 { ((r / 0.04).ceil() as i32).clamp(3, 16) } else { 0 };
        for c in 0..4 {
            if m == 0 {
                v.push(P { x: cx[c], y: cy[c] });
            } else {
                for k in 0..=m {
                    let t = (c as f64 + k as f64 / m as f64) * 1.5707963267948966;
                    v.push(P { x: cx[c] + r * t.cos(), y: cy[c] + r * t.sin() });
                }
            }
        }
    }
    for i in 0..v.len() {
        let a = v[i];
        let b = v[(i + 1) % v.len()];
        if len(b - a) < 1e-9 { continue; } // A full circle's quarters meet in one point.
        let n = std_max(1, (len(b - a) / 0.1).ceil() as i32);
        for k in 0..n {
            let q = a + (b - a) * (k as f64 / n as f64);
            r_out.push(P { x: f.cx + q.x * f.c + q.y * f.s, y: f.cy - q.x * f.s + q.y * f.c });
        }
    }
}

// Blocks of segments with a bounding circle rule out most of a long track at once.
#[derive(Default)]
struct Block {
    c: P,
    r: f64,
    w: f64,
    a: usize,
    b: usize,
}

#[derive(Default)]
struct Track {
    p: Vec<P>,
    cum: Vec<f64>,
    w: Vec<f64>,
    i0: usize,
    i1: usize,
    blocks: Vec<Block>,
    // ADAPTED: Cell preserves the C++ mutable search hint through shared borrows.
    hint: Cell<usize>,
}
impl Track {
    fn init(&mut self, q: &[Vec2], w: &[f32]) {
        self.p.resize(q.len(), P::default());
        self.w.resize(q.len(), 0.0);
        self.cum = vec![0.0; q.len()];
        for i in 0..q.len() {
            self.p[i] = to_p(q[i]);
            self.w[i] = w[i] as f64;
            if i != 0 { self.cum[i] = self.cum[i - 1] + len(self.p[i] - self.p[i - 1]); }
        }
        self.i0 = 0;
        self.i1 = self.p.len() - 1;
    }
    // Rust has no overloads; build validates the slices before calling this overload.
    fn init_input(&mut self, input: &Input<'_>) { self.init(input.path.unwrap(), input.width.unwrap()); }
    fn length(&self) -> f64 { *self.cum.last().unwrap() }
    fn seg(&self, s: f64) -> usize {
        // upper_bound uses !(s < element), including its unordered-float behaviour.
        let k = self.cum.partition_point(|&v| !(s < v));
        std_min(if k > 0 { k - 1 } else { 0 }, self.p.len() - 2)
    }
    fn at(&self, s: f64) -> P {
        let k = self.seg(s);
        let l = self.cum[k + 1] - self.cum[k];
        let t = if l > 1e-12 { std_clamp((s - self.cum[k]) / l, 0.0, 1.0) } else { 0.0 };
        self.p[k] + (self.p[k + 1] - self.p[k]) * t
    }
    fn width_at(&self, s: f64) -> f64 {
        let k = self.seg(s);
        let l = self.cum[k + 1] - self.cum[k];
        let t = if l > 1e-12 { std_clamp((s - self.cum[k]) / l, 0.0, 1.0) } else { 0.0 };
        self.w[k] + (self.w[k + 1] - self.w[k]) * t
    }
    fn tangent(&self, s: f64) -> P {
        unit(self.at(std_min(s + 0.02, self.length())) - self.at(std_max(s - 0.02, 0.0)))
    }
    fn index(&mut self) {
        self.blocks.clear();
        self.hint.set(0);
        for a in (self.i0..self.i1).step_by(8) {
            let mut k = Block { a, b: std_min(a + 8, self.i1), ..Block::default() };
            for i in a..=k.b { k.w = std_max(k.w, self.w[i]); }
            k.c = (self.p[a] + self.p[k.b]) * 0.5;
            for i in a..=k.b { k.r = std_max(k.r, len(self.p[i] - k.c)); }
            self.blocks.push(k);
        }
    }
    // Signed distance to the track's edge, gradient g.
    fn dist(&self, q: P, g: Option<&mut P>) -> f64 {
        let mut best = 1e300;
        let mut bd = 0.0;
        let mut bc = P::default();
        let mut bi = self.hint.get();
        let mut scan = |k: usize| {
            let b = &self.blocks[k];
            if len(q - b.c) - b.r - 0.5 * b.w >= best { return; }
            for i in b.a..b.b {
                let e = self.p[i + 1] - self.p[i];
                let r = q - self.p[i];
                let l2 = dot(e, e);
                let t = if l2 > 1e-18 { std_clamp(dot(r, e) / l2, 0.0, 1.0) } else { 0.0 };
                let c = self.p[i] + e * t;
                let d = len(q - c);
                let v = d - 0.5 * (self.w[i] + (self.w[i + 1] - self.w[i]) * t);
                if v < best {
                    best = v;
                    bc = c;
                    bd = d;
                    bi = k;
                }
            }
        };
        if self.hint.get() < self.blocks.len() { scan(self.hint.get()); }
        for k in 0..self.blocks.len() {
            if k != self.hint.get() { scan(k); }
        }
        self.hint.set(bi);
        if let Some(g) = g { *g = if bd > 1e-9 { (q - bc) * (1.0 / bd) } else { P { x: 0.0, y: 0.0 } }; }
        best
    }
}

// The landing's circle or tangent line, not the whole outline (which would bulge at every corner).
#[inline]
fn land_dist(a: P, n: P, rad: f64, q: P) -> f64 {
    if rad > 0.0 { len(q - (a - n * rad)) - rad } else { dot(q - a, n) }
}
#[inline]
fn land_grad(a: P, n: P, rad: f64, q: P) -> P {
    if rad > 0.0 { unit(q - (a - n * rad)) } else { n }
}

struct Blend<'a, 'b> {
    input: &'b Input<'a>,
    t: &'b Track,
    a: P,
    n: P,
    rad: f64,
    ka: f64,
    kb: f64,
    // ADAPTED: Cell is the C++ mutable Lipschitz cache; query order is unchanged.
    fq: Cell<P>,
    fc0: Cell<f64>,
}
impl Blend<'_, '_> {
    const K_SOFT: f64 = 0.10;
    const K_MARGIN: f64 = 0.004;

    // C++ A and a become a Rust field and method with the same name.
    fn a(&self, q: P) -> f64 { land_dist(self.a, self.n, self.rad, q) }
    fn value(&self, q: P, grad: Option<&mut P>) -> f64 {
        let mut gb = P::default();
        let mut gf = P::default();
        let av = self.a(q);
        let bv = self.t.dist(q, if grad.is_some() { Some(&mut gb) } else { None });
        let u = std_max(0.0, 1.0 - av / self.ka);
        let v = std_max(0.0, 1.0 - bv / self.kb);
        let scale = std_min(self.ka, self.kb);
        let n = (u * u + v * v).sqrt();
        let mut f = (1.0 - n) * scale;
        if grad.is_some() {
            gf = if n > 1e-9 {
                (land_grad(self.a, self.n, self.rad, q) * (u / self.ka) + gb * (v / self.kb)) * (scale / n)
            } else { gb };
        }
        if let Some(foreign) = self.input.foreign {
            if f + self.fc0.get() - len(q - self.fq.get()) - Self::K_MARGIN < Self::K_SOFT {
                let fc = |r: P| Self::K_MARGIN - foreign(Vec2::new(r.x as f32, r.y as f32));
                let g = fc(q);
                let ad = (f - g).abs();
                self.fq.set(q);
                self.fc0.set(Self::K_MARGIN - g);
                if ad < Self::K_SOFT || g > f {
                    let h = std_max(Self::K_SOFT - ad, 0.0) / Self::K_SOFT;
                    let sgn = if f > g { 1.0 } else { -1.0 };
                    if grad.is_some() {
                        let e = 2e-3;
                        let gg = P {
                            x: fc(q + P { x: e, y: 0.0 }) - fc(q - P { x: e, y: 0.0 }),
                            y: fc(q + P { x: 0.0, y: e }) - fc(q - P { x: 0.0, y: e }),
                        } * (0.5 / e);
                        gf = gf * ((if f > g { 1.0 } else { 0.0 }) - 0.5 * h * sgn)
                            + gg * ((if g > f { 1.0 } else { 0.0 }) + 0.5 * h * sgn);
                    }
                    f = std_max(f, g) + h * h * Self::K_SOFT * 0.25;
                }
            }
        }
        if let Some(grad) = grad { *grad = gf; }
        f
    }
    fn project(&self, q: &mut P, iters: i32, g: &mut P) -> bool {
        for _ in 0..iters {
            let f = self.value(*q, Some(&mut *g));
            let g2 = dot(*g, *g);
            if g2 < 1e-10 { return false; }
            if f.abs() < 2e-5 { return true; }
            let d = *g * (f / g2);
            let l = len(d);
            *q = *q - d * if l > 0.1 { 0.1 / l } else { 1.0 };
        }
        self.value(*q, Some(g)).abs() < 3e-4
    }
    // From the tip along the contour to the outline.
    fn walk(&self, tip: P, dir: P, out: &mut Vec<P>) -> bool {
        const K_MAX_STEP: f64 = 0.2;
        const K_MIN_STEP: f64 = 0.006;
        const K_TURN: f64 = 0.11;
        const K_LAND: f64 = 0.0015;
        out.clear();
        let mut x = tip;
        out.push(x);
        let mut ds = 0.03;
        let mut total = 0.0;
        let cap = 3.0 * (self.ka + self.kb) + 1.0;
        let mut g = P::default();
        self.value(x, Some(&mut g));
        if dot(g, g) < 1e-10 { return false; }
        let mut tau = unit(P { x: -g.y, y: g.x });
        if dot(tau, dir) < 0.0 { tau = tau * -1.0; }
        for _ in 0..400 {
            if !(total < cap) { break; }
            let mut y = x + tau * ds;
            let mut gy = P::default();
            if !self.project(&mut y, 5, &mut gy) {
                if ds <= K_MIN_STEP { return false; }
                ds = std_max(ds * 0.5, K_MIN_STEP);
                continue;
            }
            if dot(gy, gy) < 1e-10 { return false; }
            let mut t2 = unit(P { x: -gy.y, y: gy.x });
            if dot(t2, tau) < 0.0 { t2 = t2 * -1.0; }
            let turn = cross(tau, t2).abs().atan2(dot(tau, t2));
            if turn > K_TURN && ds > K_MIN_STEP {
                ds = std_max(ds * 0.5, K_MIN_STEP);
                continue;
            }
            total += len(y - x);
            if self.a(y) <= K_LAND {
                let mut lo = x;
                let mut hi = y;
                for _ in 0..12 {
                    let m = (lo + hi) * 0.5;
                    if self.a(m) > K_LAND { lo = m; } else { hi = m; }
                }
                out.push(hi);
                // A tip close to the outline's line gets there early: on along the line.
                if dot(self.a - hi, tau) > 0.0 { out.push(self.a - self.n * 0.02); }
                return true;
            }
            out.push(y);
            x = y;
            tau = t2;
            if turn < 0.5 * K_TURN { ds = std_min(ds * 1.6, K_MAX_STEP); }
        }
        false
    }
}

#[derive(Copy, Clone, Default)]
struct Landing {
    a: P,
    n: P,
    b: f64,
    rad: f64,
}

// Walk from the exit to the wanted height or where the tip can no longer see the outline.
fn landing(input: &Input<'_>, t: &Track, r: &[P], x: P, d: P, tip: P, tip_dir: P,
    flank: f64, aspect: f64, full: bool, side: i32, want: f64) -> Landing {
    let mut l = Landing::default();
    let n = r.len() as i32;
    if n < 3 { return l; }
    let mut area = 0.0;
    let mut bd = 1e300;
    let mut i_x = 0;
    for i in 0..n {
        area += cross(r[i as usize], r[((i + 1) % n) as usize]);
        let e = len(r[i as usize] - x);
        if e < bd { bd = e; i_x = i; }
    }
    let k = std_max(1, n / 16);
    let fw = cross(d, r[((i_x + k) % n) as usize] - x);
    let bw = cross(d, r[((i_x - k + n) % n) as usize] - x);
    let step = if side as f64 * fw >= side as f64 * bw { 1 } else { -1 };
    let out = (if area > 0.0 { 1.0 } else { -1.0 }) * step as f64;
    // A long track along the outline may stay close; a short one keeps a waist.
    let mut prev = P::default();
    let mut prev_n = P::default();
    let mut first = Landing::default();
    for i in 0..n {
        let q = r[(((i_x + step * i) % n + n) % n) as usize];
        let nx = r[(((i_x + step * (i + 1)) % n + n) % n) as usize];
        let b = t.dist(q, None);
        if i > 0 && b >= want {
            if first.b <= 0.0 {
                let mut lo = prev;
                let mut hi = q;
                for _ in 0..14 {
                    let m = (lo + hi) * 0.5;
                    if t.dist(m, None) < want { lo = m; } else { hi = m; }
                }
                first.a = hi;
                first.n = prev_n;
                first.b = want;
            }
            // The tip tangent must meet the outline's before the landing, avoiding an overhang.
            let dn = dot(tip_dir, prev_n);
            let k = if dn.abs() > 1e-3 { tip - tip_dir * (dot(tip - first.a, prev_n) / dn) } else { first.a };
            let e = q - prev;
            // A shrunk flank lands exactly; a full one may take the next ring point.
            if !full && (dn.abs() <= 1e-3 || dot(k - first.a, e) <= 0.0 || b > 1.4 * want) {
                l = first;
                break;
            }
            if dn.abs() <= 1e-3 || dot(k - q, e) <= 0.0 || b > 1.4 * want {
                l = if dot(k - q, e) <= 0.0 && b > want + 1e-9 { Landing { a: q, n: prev_n, b, rad: 0.0 } } else { first };
                break;
            }
        }
        if l.b > 0.0 && b < l.b - 0.02 { break; }
        if i > 0 && b > l.b { l.a = q; l.n = prev_n; l.b = b; }
        let e = nx - q;
        let nrm = unit(P { x: e.y, y: -e.x } * out);
        if b > 0.0 && dot(nrm, tip - q) <= aspect * b * if flank >= 4.5 * b { dot(nrm, tip_dir).abs() } else { 1.0 } { break; }
        prev = q;
        prev_n = nrm;
    }
    if first.b > 0.0 && l.b < first.b { l = first; }
    // Round outlines: exact point and normal, not the ring's chord.
    if l.b > 0.0 && (input.pad.is_none() || input.pad.is_some_and(|f| f.r > 1e-6 && f.poly.is_none())) {
        let h = 1e-3;
        let g = unit(P {
            x: junction_dist(input, l.a + P { x: h, y: 0.0 }) - junction_dist(input, l.a - P { x: h, y: 0.0 }),
            y: junction_dist(input, l.a + P { x: 0.0, y: h }) - junction_dist(input, l.a - P { x: 0.0, y: h }),
        });
        l.a = l.a - g * junction_dist(input, l.a);
        l.n = g;
        if let Some(f) = input.pad {
            let (lx, ly) = padgeom::to_local(f, l.a.x, l.a.y);
            if lx.abs() > f.hx - f.r - 1e-6 && ly.abs() > f.hy - f.r - 1e-6 { l.rad = f.r; }
        } else { l.rad = input.via_radius; }
    }
    l
}

// Douglas-Peucker: remove small walk steps where the clearance field bends the flank.
fn simplify(c: &mut Vec<P>, tol: f64) {
    if c.len() < 3 { return; }
    let mut keep = vec![0u8; c.len()];
    keep[0] = 1;
    *keep.last_mut().unwrap() = 1;
    let mut todo = vec![(0, c.len() - 1)];
    while let Some((i, j)) = todo.pop() {
        let e = unit(c[j] - c[i]);
        let mut worst = tol;
        let mut at = 0;
        for k in i + 1..j {
            let d = cross(e, c[k] - c[i]).abs();
            if d > worst { worst = d; at = k; }
        }
        if at == 0 { continue; }
        keep[at] = 1;
        todo.push((i, at));
        todo.push((at, j));
    }
    let mut n = 0;
    for k in 0..c.len() {
        if keep[k] != 0 { c[n] = c[k]; n += 1; }
    }
    c.truncate(n);
}

// Extend a flank that stops in the air past a receding corner to touch the outline.
fn bridge(input: &Input<'_>, r: &[P], inner: P, c: &mut Vec<P>) {
    if c.len() < 2 { return; }
    let e = *c.last().unwrap();
    if junction_dist(input, e) < 0.002 { return; }
    let d = unit(e - c[c.len() - 2]);
    let sg = if cross(d, inner - e) >= 0.0 { 1.0 } else { -1.0 };
    let mut best = 1e30;
    let mut at = P::default();
    for &q in r {
        let v = q - e;
        if dot(v, v) > 0.04 { continue; }
        let ang = (sg * cross(d, v)).atan2(dot(d, v));
        if ang >= 0.0 && ang < best { best = ang; at = q; }
    }
    if best < 2.0 { c.push(at); }
}

// The calmed path from s0 to s_tip: stations with short pieces smoothed away.
struct Calm {
    s0: f64,
    s_tip: f64,
    _l: f64, // ADAPTED: C++ L is stored but never read.
    win: f64,
    n: i32,
    c: Vec<P>,
    nr: Vec<P>,
    off: Vec<f64>,
    wid: Vec<f64>,
    // ADAPTED: the reference reserves sOf but never uses it.
    _s_of: Vec<f64>,
}
impl Default for Calm {
    fn default() -> Self {
        Self { s0: 0.0, s_tip: -1.0, _l: 0.0, win: 0.0, n: 0, c: Vec::new(), nr: Vec::new(),
            off: Vec::new(), wid: Vec::new(), _s_of: Vec::new() }
    }
}
fn calm_path(r: &Track, s0: f64, s_x: f64, s_tip: f64, l: f64, k: &mut Calm) {
    if k.s_tip == s_tip { return; }
    k.s0 = s0;
    k.s_tip = s_tip;
    k._l = l;
    let n = std_max(4, ((s_tip - s0) / 0.12).ceil() as i32);
    k.n = n;
    let mut raw = vec![P::default(); n as usize + 1];
    k.wid.resize(n as usize + 1, 0.0);
    let s_in = std_max(s0, s_x - 0.1);
    let p_in = r.at(s_in);
    let t_in = r.tangent(s_in);
    for i in 0..=n {
        let s = s0 + (s_tip - s0) * i as f64 / n as f64;
        // What the path does deep in the pad does not count: straight on from the exit.
        raw[i as usize] = if s >= s_in { r.at(s) } else { p_in + t_in * (s - s_in) };
        k.wid[i as usize] = r.width_at(std_max(s, s_in));
    }
    // Three box blurs, with the path mirrored through its ends so they stay put.
    let mut e = Vec::new();
    let mut acc = Vec::new();
    // ADAPTED: pass c explicitly so the closure need not hold a mutable borrow of all of K.
    let mut calm = |win: f64, c: &mut Vec<P>| {
        let r = ((win * n as f64 / (s_tip - s0)).round() as i32).clamp(1, n);
        c.clone_from(&raw);
        e.resize((n + 1 + 2 * r) as usize, P::default());
        acc.resize(e.len() + 1, P::default());
        for _ in 0..3 {
            for i in -r..=n + r {
                e[(i + r) as usize] = if i < 0 { c[0] * 2.0 - c[(-i) as usize] }
                    else if i > n { c[n as usize] * 2.0 - c[(2 * n - i) as usize] } else { c[i as usize] };
            }
            for i in 0..e.len() { acc[i + 1] = acc[i] + e[i]; }
            for i in 1..n { c[i as usize] = (acc[(i + 2 * r) as usize + 1] - acc[i as usize]) * (1.0 / (2 * r + 1) as f64); }
        }
    };
    k.nr.resize(n as usize + 1, P::default());
    k.off.resize(n as usize + 1, 0.0);
    k.win = std_clamp(0.3 * l, 0.05, 0.5);
    for pass in 0..2 {
        let mut dev = 0.0;
        calm(k.win, &mut k.c);
        for i in 0..=n {
            let t = unit(k.c[std_min(i + 1, n) as usize] - k.c[std_max(i - 1, 0) as usize]);
            k.nr[i as usize] = P { x: -t.y, y: t.x };
            k.off[i as usize] = dot(raw[i as usize] - k.c[i as usize], k.nr[i as usize]);
            dev = std_max(dev, k.off[i as usize].abs());
        }
        if dev <= 0.03 || pass == 1 { break; }
        k.win = std_clamp(0.6 * l, 0.1, 1.0); // Wiggles: a calmer path.
    }
}

// The field's track for one side: a taut edge over the calm path; past the tip, the real track.
fn calm_track(r_track: &Track, k: &Calm, side: i32, f: &mut Track) {
    let n = k.n;
    let s_tip = k.s_tip;
    let c = &k.c;
    let mut r = vec![0.0; n as usize + 1];
    for i in 0..=n { r[i as usize] = k.off[i as usize] * side as f64 + 0.5 * k.wid[i as usize]; }
    // Closing over twice the calm window fills short valleys but keeps a real bend.
    let wn = ((2.0 * k.win * n as f64 / (s_tip - k.s0)).round() as i32).clamp(1, n);
    let mut m = vec![0.0; n as usize + 1];
    let mut h = vec![0.0; n as usize + 1];
    for i in 0..=n {
        let mut v = -1e30;
        for j in std_max(0, i - wn)..=std_min(n, i + wn) { v = std_max(v, r[j as usize]); }
        m[i as usize] = v;
    }
    // Under the taut line over everything, holding it to the real edge at the ends.
    let mut hull: Vec<i32> = Vec::new(); // C++ H; h is the heights above.
    for i in 0..=n {
        while hull.len() >= 2 {
            let a = hull[hull.len() - 2];
            let b = *hull.last().unwrap();
            if (r[b as usize] - r[a as usize]) * (i - a) as f64 <= (r[i as usize] - r[a as usize]) * (b - a) as f64 {
                hull.pop();
            } else { break; }
        }
        hull.push(i);
    }
    for k in 0..hull.len() - 1 {
        for i in hull[k]..=hull[k + 1] {
            h[i as usize] = r[hull[k] as usize] + (r[hull[k + 1] as usize] - r[hull[k] as usize])
                * (i - hull[k]) as f64 / std_max(1, hull[k + 1] - hull[k]) as f64;
        }
    }
    for i in 0..=n {
        let mut v = 1e30;
        for j in std_max(0, i - wn)..=std_min(n, i + wn) { v = std_min(v, m[j as usize]); }
        h[i as usize] = std_min(h[i as usize], v);
    }
    // Round it.
    let mut g = h.clone();
    for _ in 0..6 {
        for i in 1..n { g[i as usize] = 0.25 * h[i as usize - 1] + 0.5 * h[i as usize] + 0.25 * h[i as usize + 1]; }
        h.clone_from(&g);
    }
    f.p.clear();
    f.w.clear();
    f.cum.clear();
    let mut acc = 0.0;
    let mut add = |q: P, w: f64| {
        if let Some(&last) = f.p.last() { acc += len(q - last); }
        f.p.push(q);
        f.w.push(w);
        f.cum.push(acc);
    };
    for i in 0..=n { add(c[i as usize], 2.0 * h[i as usize]); }
    let s_end = std_min(s_tip + 0.3, r_track.length());
    let mut s = s_tip + 0.12;
    while s < s_end { add(r_track.at(s), r_track.width_at(s)); s += 0.12; }
    f.i0 = 0;
    f.i1 = f.p.len() - 1;
    f.index();
}

struct BlendMulti<'a, 'b> {
    input: &'b Input<'a>,
    ta: &'b Track,
    tb: &'b Track,
    ka: f64,
    kb: f64,
    // Each tip's own length and height, blended by the smooth minimum of the track fields.
    ka_a: f64,
    kb_a: f64,
    smooth: f64,
    fq: Cell<P>,
    fc0: Cell<f64>,
}
impl BlendMulti<'_, '_> {
    const K_SOFT: f64 = 0.10;
    const K_MARGIN: f64 = 0.004;

    fn a(&self, q: P, grad: Option<&mut P>) -> f64 {
        if let Some(grad) = grad { *grad = junction_grad(self.input, q); }
        junction_dist(self.input, q)
    }
    fn b(&self, q: P, grad: Option<&mut P>, h_out: Option<&mut f64>) -> f64 {
        let mut ga = P::default();
        let mut gb = P::default();
        let da = self.ta.dist(q, if grad.is_some() { Some(&mut ga) } else { None });
        let db = self.tb.dist(q, if grad.is_some() { Some(&mut gb) } else { None });
        let eps = self.smooth;
        let h = std_clamp(0.5 + 0.5 * (db - da) / eps, 0.0, 1.0);
        if let Some(h_out) = h_out {
            const EPS_K: f64 = 0.25;
            let t = std_clamp(0.5 + 0.5 * (db - da) / EPS_K, 0.0, 1.0);
            *h_out = t * t * (3.0 - 2.0 * t);
        }
        if let Some(grad) = grad { *grad = gb + (ga - gb) * h; }
        db + (da - db) * h - eps * h * (1.0 - h)
    }
    fn value(&self, q: P, grad: Option<&mut P>) -> f64 {
        let mut ga = P::default();
        let mut gb = P::default();
        let mut gf = P::default();
        let av = self.a(q, if grad.is_some() { Some(&mut ga) } else { None });
        let mut hk = 0.0;
        let bv = self.b(q, if grad.is_some() { Some(&mut gb) } else { None }, Some(&mut hk));
        let ka = self.ka + (self.ka_a - self.ka) * hk;
        let kb = self.kb + (self.kb_a - self.kb) * hk;
        let u = std_max(0.0, 1.0 - av / ka);
        let v = std_max(0.0, 1.0 - bv / kb);
        let scale = std_min(ka, kb);
        let n = (u * u + v * v).sqrt();
        let mut f = (1.0 - n) * scale;
        if grad.is_some() {
            gf = if n > 1e-9 { (ga * (u / ka) + gb * (v / kb)) * (scale / n) } else { gb };
        }
        if let Some(foreign) = self.input.foreign {
            if f + self.fc0.get() - len(q - self.fq.get()) - Self::K_MARGIN < Self::K_SOFT {
                let fc = |r: P| Self::K_MARGIN - foreign(Vec2::new(r.x as f32, r.y as f32));
                let g = fc(q);
                let ad = (f - g).abs();
                self.fq.set(q);
                self.fc0.set(Self::K_MARGIN - g);
                if ad < Self::K_SOFT || g > f {
                    let h = std_max(Self::K_SOFT - ad, 0.0) / Self::K_SOFT;
                    let sgn = if f > g { 1.0 } else { -1.0 };
                    if grad.is_some() {
                        let e = 2e-3;
                        let gg = P {
                            x: fc(q + P { x: e, y: 0.0 }) - fc(q - P { x: e, y: 0.0 }),
                            y: fc(q + P { x: 0.0, y: e }) - fc(q - P { x: 0.0, y: e }),
                        } * (0.5 / e);
                        gf = gf * ((if f > g { 1.0 } else { 0.0 }) - 0.5 * h * sgn)
                            + gg * ((if g > f { 1.0 } else { 0.0 }) + 0.5 * h * sgn);
                    }
                    f = std_max(f, g) + h * h * Self::K_SOFT * 0.25;
                }
            }
        }
        if let Some(grad) = grad { *grad = gf; }
        f
    }
    fn project(&self, q: &mut P, iters: i32, g: &mut P) -> bool {
        for _ in 0..iters {
            let f = self.value(*q, Some(&mut *g));
            let g2 = dot(*g, *g);
            if g2 < 1e-10 { return false; }
            if f.abs() < 2e-5 { return true; }
            let d = *g * (f / g2);
            let l = len(d);
            *q = *q - d * if l > 0.1 { 0.1 / l } else { 1.0 };
        }
        self.value(*q, Some(g)).abs() < 3e-4
    }
    fn walk_bridge(&self, tip_a: P, dir_a: P, tip_b: P, out: &mut Vec<P>) -> bool {
        const K_MAX_STEP: f64 = 0.15;
        const K_MIN_STEP: f64 = 0.005;
        const K_TURN: f64 = 0.15;
        const K_LAND: f64 = 0.0015;
        out.clear();
        let mut x = tip_a;
        out.push(x);
        let mut ds = 0.025;
        let mut total = 0.0;
        let cap = 4.0 * (std_max(self.ka, self.ka_a) + std_max(self.kb, self.kb_a)) + 2.0;
        let mut g = P::default();
        self.value(x, Some(&mut g));
        if dot(g, g) < 1e-10 { return false; }
        let mut tau = unit(P { x: -g.y, y: g.x });
        if dot(tau, dir_a) < 0.0 { tau = tau * -1.0; }
        for it in 0..500 {
            if !(total < cap) { break; }
            if len(x - tip_b) < std_max(ds * 1.5, 0.04) && it > 2 {
                out.push(tip_b);
                return true;
            }
            if junction_dist(self.input, x) <= K_LAND && it > 2 { return false; }
            let mut y = x + tau * ds;
            let mut gy = P::default();
            if !self.project(&mut y, 6, &mut gy) {
                if ds <= K_MIN_STEP { return false; }
                ds = std_max(ds * 0.5, K_MIN_STEP);
                continue;
            }
            if dot(gy, gy) < 1e-10 { return false; }
            let mut t2 = unit(P { x: -gy.y, y: gy.x });
            if dot(t2, tau) < 0.0 { t2 = t2 * -1.0; }
            let turn = cross(tau, t2).abs().atan2(dot(tau, t2));
            if turn > K_TURN && ds > K_MIN_STEP {
                ds = std_max(ds * 0.5, K_MIN_STEP);
                continue;
            }
            total += len(y - x);
            out.push(y);
            x = y;
            tau = t2;
            if turn < 0.5 * K_TURN { ds = std_min(ds * 1.5, K_MAX_STEP); }
        }
        if len(x - tip_b) < 0.15 { out.push(tip_b); return true; }
        false
    }
}

fn build_multi(input: &Input<'_>, left: &Scale, right: &Scale, res: &mut Result) -> bool {
    res.poly.clear();
    res.flare = 0.0;
    if input.conns.len() < 2 { return false; }
    const K_MIN_FLARE: f64 = 0.008;
    const K_TALL: f64 = 0.8;
    let mut ring = Vec::new();
    outline(input, &mut ring);
    let center;
    // ADAPTED: junctionR is computed but never read by the reference.
    let _junction_r;
    if let Some(pad) = input.pad {
        center = P { x: pad.cx, y: pad.cy };
        _junction_r = std_min(pad.hx, pad.hy);
    } else {
        center = to_p(input.via_centre);
        _junction_r = input.via_radius;
    }
    let count = input.conns.len(); // C++ M; m below is the target connection.
    #[derive(Default)]
    struct ConnData {
        t: Track,
        s_tip: f64,
        s_br: f64,
        x: P,
        d_x: P,
        angle: f64,
        want: f64,
        w_x: f64,
        s_x: f64,
        s_root: f64,
        orig_idx: usize,
    }
    let mut cd: Vec<ConnData> = (0..count).map(|_| ConnData::default()).collect();
    for m in 0..count {
        let c = &input.conns[m];
        // ADAPTED: reject a missing width as well; C++ dereferences that null pointer.
        let (Some(path), Some(width)) = (c.path, c.width) else { return false; };
        if path.len() < 2 || width.len() != path.len() { return false; }
        cd[m].orig_idx = m;
        cd[m].t.init(path, width);
        cd[m].s_x = c.s_x;
        cd[m].s_root = c.s_root;
        cd[m].s_tip = std_min(c.s_x + c.length, cd[m].t.length());
        if cd[m].s_tip <= cd[m].s_x + 0.03 { return false; }
        cd[m].s_br = if c.bridge_length > 0.0 { std_min(c.s_x + c.bridge_length, cd[m].s_tip) } else { cd[m].s_tip };
        if cd[m].s_br <= cd[m].s_x + 0.03 { cd[m].s_br = cd[m].s_tip; }
        cd[m].t.i0 = cd[m].t.seg(c.s_root);
        cd[m].t.i1 = std_min(cd[m].t.seg(cd[m].s_tip + 0.3) + 1, cd[m].t.p.len() - 1);
        cd[m].t.index();
        cd[m].x = cd[m].t.at(c.s_x);
        cd[m].d_x = cd[m].t.tangent(c.s_x);
        cd[m].w_x = cd[m].t.width_at(c.s_x);
        cd[m].want = std_max(0.0, 0.5 * c.width2 - 0.5 * cd[m].w_x);
        cd[m].angle = (cd[m].x.y - center.y).atan2(cd[m].x.x - center.x);
    }
    cd.sort_unstable_by(|a, b| {
        use std::cmp::Ordering;
        if a.angle < b.angle || (a.angle == b.angle && a.orig_idx < b.orig_idx) { Ordering::Less }
        else if b.angle < a.angle || (b.angle == a.angle && b.orig_idx < a.orig_idx) { Ordering::Greater }
        else { Ordering::Equal }
    });
    for k in 0..count { res.flare = std_max(res.flare, cd[k].want + 0.5 * cd[k].w_x); }
    // Only this connection's flanks and its neighbours' facing tracks and bridges are needed.
    let target_orig = input.this_conn.clamp(0, count as i32 - 1) as usize;
    let mut m = 0;
    for i in 0..count {
        if cd[i].orig_idx == target_orig { m = i; break; }
    }
    let nx = (m + 1) % count;
    // C++ size_t subtraction wraps before addition when m == 0.
    let pr = m.wrapping_sub(1).wrapping_add(count) % count;
    let mut tip_l = vec![P::default(); count];
    let mut tip_r = vec![P::default(); count];
    let mut dir_l = vec![P::default(); count];
    let mut dir_r = vec![P::default(); count];
    let mut fl: Vec<Track> = (0..count).map(|_| Track::default()).collect();
    let mut fr: Vec<Track> = (0..count).map(|_| Track::default()).collect();
    let mut flank_track = |k: usize, side: i32| {
        let s_t = cd[k].s_br;
        let w_t = cd[k].t.width_at(s_t);
        let t_t = cd[k].t.tangent(s_t);
        let n_t = P { x: -t_t.y, y: t_t.x };
        let mut calm = Calm::default(); // C++ K; k is the connection index.
        calm_path(&cd[k].t, cd[k].s_root, cd[k].s_x, s_t, s_t - cd[k].s_x, &mut calm);
        if side == 1 {
            tip_l[k] = cd[k].t.at(s_t) + n_t * (0.5 * w_t);
            dir_l[k] = t_t;
            calm_track(&cd[k].t, &calm, 1, &mut fl[k]);
        } else {
            tip_r[k] = cd[k].t.at(s_t) - n_t * (0.5 * w_t);
            dir_r[k] = t_t;
            calm_track(&cd[k].t, &calm, -1, &mut fr[k]);
        }
    };
    flank_track(m, 1);
    flank_track(m, -1);
    if pr != m { flank_track(pr, 1); }
    if nx != m { flank_track(nx, -1); }

    // A flank without a bridge, scaled independently. Returns its tip station.
    let walk_single_flank = |m: usize, side: i32, s_t: f64, height: f64, curve: &mut Vec<P>| {
        curve.clear();
        let c = &cd[m];
        let mut f = Track::default();
        let mut k = Calm::default();
        let t_t = c.t.tangent(s_t);
        let n_t = P { x: -t_t.y, y: t_t.x };
        let tip = c.t.at(s_t) + n_t * (side as f64 * 0.5 * c.t.width_at(s_t));
        calm_path(&c.t, c.s_root, c.s_x, s_t, s_t - c.s_x, &mut k);
        calm_track(&c.t, &k, side, &mut f);
        let ld = landing(input, &f, &ring, c.x, c.d_x, tip, t_t, s_t - c.s_x, 0.0, height >= 1.0, side,
            std_min(c.want * height, K_TALL * (s_t - c.s_x)));
        let ka = land_dist(ld.a, ld.n, ld.rad, tip);
        if ld.b >= K_MIN_FLARE && ka >= K_MIN_FLARE {
            let b = Blend { input, t: &f, a: ld.a, n: ld.n, rad: ld.rad, ka, kb: ld.b,
                fq: Cell::new(P::default()), fc0: Cell::new(-1e30) };
            if b.walk(tip, t_t * -1.0, curve) {
                if input.bridge { bridge(input, &ring, c.t.at(c.s_root), curve); }
                simplify(curve, 0.0015);
                return s_t;
            }
        }
        *curve = vec![tip, ld.a];
        s_t
    };
    let mut is_bridge = vec![false; count];
    let mut bridge_curves = vec![Vec::new(); count];
    let mut flank_l = vec![Vec::new(); count];
    let mut flank_r = vec![Vec::new(); count];
    let mut build_bridge = |m: usize| {
        let nx = (m + 1) % count;
        let mut ang_diff = cd[nx].angle - cd[m].angle;
        if ang_diff < 0.0 { ang_diff += 6.283185307179586; }
        let exit_sep = len(cd[nx].x - cd[m].x);
        let want_sum = cd[m].want + cd[nx].want;
        let mut candidate = ang_diff < 2.0944 && exit_sep < std_max(want_sum * 1.5, 1.2);
        if count == 2 && ang_diff < 1e-9 && m > nx { candidate = false; }
        if candidate {
            let mut bm = BlendMulti { input, ta: &fl[m], tb: &fr[nx],
                ka: 0.5 * (cd[m].s_br - cd[m].s_x + cd[nx].s_br - cd[nx].s_x), kb: 0.5 * want_sum,
                ka_a: 1.0, kb_a: 1.0, smooth: 0.04, fq: Cell::new(P::default()), fc0: Cell::new(-1e30) };
            {
                let ka_m = junction_dist(input, tip_l[m]);
                let ka_n = junction_dist(input, tip_r[nx]);
                if ka_m > 0.02 && ka_n > 0.02 && cd[m].want > 0.0 && cd[nx].want > 0.0 {
                    bm.ka_a = ka_m;
                    bm.ka = ka_n;
                    bm.kb_a = std_min(cd[m].want, K_TALL * ka_m);
                    bm.kb = std_min(cd[nx].want, K_TALL * ka_n);
                    bm.smooth = std_clamp(0.5 * std_min(bm.kb, bm.kb_a), 0.04, 0.25);
                } else { bm.ka_a = bm.ka; bm.kb_a = bm.kb; }
            }
            // A bridge running into the junction must instead land as two single flanks.
            let clear_of = |c: &[P]| {
                for &q in c {
                    if junction_dist(input, q) < 0.03 { return false; }
                }
                true
            };
            let mut walked = bm.walk_bridge(tip_l[m], dir_l[m] * -1.0, tip_r[nx], &mut bridge_curves[m]);
            if walked {
                simplify(&mut bridge_curves[m], 0.0015);
                walked = clear_of(&bridge_curves[m]);
            }
            if walked { is_bridge[m] = true; }
            else if ang_diff < 1.05 && exit_sep < std_max(want_sum, 0.8) {
                let xmid = (cd[m].x + cd[nx].x) * 0.5;
                let dmid = unit(cd[m].d_x + cd[nx].d_x);
                let mut lo = 0.0;
                let mut hi = want_sum * 1.5;
                for _ in 0..20 {
                    let mid = 0.5 * (lo + hi);
                    if bm.value(xmid + dmid * mid, None) < 0.0 { lo = mid; } else { hi = mid; }
                }
                let saddle = xmid + dmid * hi;
                bridge_curves[m].clear();
                // FA16: cubic tangent to both tracks, with half-distance handles to the saddle.
                let a = tip_l[m];
                let b = tip_r[nx];
                let ta = dir_l[m] * -1.0;
                let tb = dir_r[nx];
                let c1 = a + ta * (0.5 * len(saddle - a));
                let c2 = b - tb * (0.5 * len(saddle - b));
                let n_s = 24;
                for i in 0..=n_s {
                    let t = i as f64 / n_s as f64;
                    let u = 1.0 - t;
                    bridge_curves[m].push(a * (u * u * u) + c1 * (3.0 * u * u * t) + c2 * (3.0 * u * t * t) + b * (t * t * t));
                }
                is_bridge[m] = clear_of(&bridge_curves[m]);
            }
        }
    };
    // Preserve index order: distance queries keep a search hint, bridge before its own flank.
    if pr < m { build_bridge(pr); }
    build_bridge(m);
    if pr > m { build_bridge(pr); }

    let len_m = cd[m].s_tip - cd[m].s_x;
    let mut flat_l = !is_bridge[m] && left.height <= 0.0;
    let mut flat_r = !is_bridge[pr] && right.height <= 0.0;
    let mut s_tl = if is_bridge[m] { cd[m].s_br } else { cd[m].s_x + len_m * std_clamp(left.length, 0.0, 1.0) };
    let mut s_tr = if is_bridge[pr] { cd[m].s_br } else { cd[m].s_x + len_m * std_clamp(right.length, 0.0, 1.0) };
    if !is_bridge[m] && !flat_l {
        s_tl = walk_single_flank(m, 1, s_tl, std_min(left.height, 1.0), &mut flank_l[m]);
        flat_l = s_tl <= 0.0;
    }
    if !is_bridge[pr] && !flat_r {
        s_tr = walk_single_flank(m, -1, s_tr, std_min(right.height, 1.0), &mut flank_r[m]);
        flat_r = s_tr <= 0.0;
    }
    if flat_l && flat_r { return false; }
    let s_end = std_max(if flat_l { 0.0 } else { s_tl }, if flat_r { 0.0 } else { s_tr });
    if flat_l { s_tl = s_end; }
    if flat_r { s_tr = s_end; }
    let centre = |s0: f64, s1: f64, out: &mut Vec<P>| {
        let n = std_max(1, ((s1 - s0).abs() / 0.1).ceil() as i32);
        for i in 0..=n { out.push(cd[m].t.at(s0 + (s1 - s0) * i as f64 / n as f64)); }
    };
    let mut root = cd[m].t.at(cd[m].s_root);
    if junction_dist(input, root) > -1e-3 { root = cd[m].t.p[0]; }
    if input.deep_root && input.pad.is_some() && junction_dist(input, root) > -0.1 {
        let pad = input.pad.unwrap();
        let c = P { x: pad.cx, y: pad.cy };
        if junction_dist(input, c) < junction_dist(input, root) { root = c; }
    }
    // FA16: split a shared bridge at its saddle along the same cut into the junction.
    // One share overlaps the cut slightly, avoiding a hairline seam between filled zones.
    const K_OVERLAP: f64 = 0.03;
    const K_DEEP: f64 = 0.05;
    let saddle_of = |bc: &[P]| {
        let mut si = 0;
        let mut min_d = 1e300;
        for i in 0..bc.len() {
            let d = junction_dist(input, bc[i]);
            if d < min_d { min_d = d; si = i; }
        }
        si
    };
    let dive = |q: P| q - junction_grad(input, q) * (std_max(junction_dist(input, q), 0.0) + K_DEEP);
    let mut poly = Vec::new();
    poly.push(root);
    if is_bridge[pr] {
        let bc = &bridge_curves[pr];
        let si = saddle_of(bc);
        poly.push(dive(bc[si]));
        for i in si..bc.len() { poly.push(bc[i]); }
    } else if flat_r { centre(cd[m].s_root, s_tr, &mut poly); }
    else {
        for i in (0..flank_r[m].len()).rev() { poly.push(flank_r[m][i]); }
    }
    if s_tl == s_tr && !flat_l && !flat_r {
        let t_t = cd[m].t.tangent(s_tr);
        let n_t = P { x: -t_t.y, y: t_t.x };
        let c = cd[m].t.at(s_tr);
        let hw = 0.5 * cd[m].t.width_at(s_tr);
        let tr = c - n_t * hw;
        let tl = c + n_t * hw;
        let n_across = std_max(2, (len(tl - tr) / 0.05).ceil() as i32);
        for i in 0..=n_across { poly.push(tr + (tl - tr) * (i as f64 / n_across as f64)); }
    } else {
        if !flat_r { poly.push(cd[m].t.at(s_tr)); }
        if s_end > s_tr + 1e-6 { centre(s_tr, s_end, &mut poly); }
        if s_end > s_tl + 1e-6 { centre(s_end, s_tl, &mut poly); }
        if !flat_l { poly.push(cd[m].t.at(s_tl)); }
    }
    let mut cut_at = 0;
    let mut cut_saddle = P::default();
    let mut cut_deep = P::default();
    let mut overlap = Vec::new();
    if is_bridge[m] {
        let bc = &bridge_curves[m];
        let si = saddle_of(bc);
        for i in 0..=si { poly.push(bc[i]); }
        cut_at = poly.len() - 1;
        cut_saddle = bc[si];
        cut_deep = dive(cut_saddle);
        poly.push(cut_deep);
        let cd = unit(cut_deep - cut_saddle);
        let mut off = 0.0;
        let mut run = 0.0;
        let mut prev = cut_saddle;
        let mut done = false;
        for i in si + 1..bc.len() {
            if done { break; }
            let l = len(bc[i] - prev);
            let n = std_max(1, (l / 0.005).ceil() as i32);
            for k in 1..=n {
                let q = prev + (bc[i] - prev) * (k as f64 / n as f64);
                off = cross(cd, q - cut_saddle).abs();
                if off >= K_OVERLAP || run + l * k as f64 / n as f64 >= 4.0 * K_OVERLAP {
                    overlap.push(q);
                    done = true;
                    break;
                }
            }
            if !done { overlap.push(bc[i]); }
            run += l;
            prev = bc[i];
        }
        if off < 0.5 * K_OVERLAP { overlap.clear(); }
        else {
            let side = *overlap.last().unwrap() - cut_saddle;
            overlap.push(cut_deep + (side - cd * dot(side, cd)));
        }
    } else if flat_l { centre(s_tl, cd[m].s_root, &mut poly); }
    else {
        for &q in &flank_l[m] { poly.push(q); }
    }
    poly.push(root);
    if cut_at != 0 {
        let mut area2 = 0.0;
        for i in 0..poly.len() - 1 { area2 += cross(poly[i], poly[i + 1]); }
        let cd = unit(cut_deep - cut_saddle);
        let far_side = if area2 > 0.0 { P { x: cd.y, y: -cd.x } } else { P { x: -cd.y, y: cd.x } };
        if overlap.is_empty() || dot(overlap[0] - cut_saddle, far_side) <= 0.0 {
            overlap = vec![cut_saddle + far_side * K_OVERLAP, cut_deep + far_side * K_OVERLAP];
        }
        poly.splice(cut_at + 1..cut_at + 1, overlap);
    }
    for q in poly {
        let v = Vec2::new(q.x as f32, q.y as f32);
        if res.poly.is_empty() || res.poly.last().unwrap().distance_squared_to(v) > 1e-7_f32 { res.poly.push(v); }
    }
    if res.poly.len() >= 2 && res.poly[0].distance_squared_to(*res.poly.last().unwrap()) < 1e-7_f32 { res.poly.pop(); }
    res.poly.len() >= 3
}

/// Build an implicitly closed copper polygon. The caller checks simplicity and clearance.
pub fn build(input: &Input<'_>, left: &Scale, right: &Scale, res: &mut Result) -> bool {
    if input.conns.len() >= 2 { return build_multi(input, left, right, res); }
    res.poly.clear();
    res.flare = 0.0;
    // ADAPTED: reject a missing width as well; C++ dereferences that null pointer.
    let (Some(path), Some(width)) = (input.path, input.width) else { return false; };
    if path.len() < 2 || width.len() != path.len() { return false; }
    const K_MIN_FLARE: f64 = 0.008;
    const K_TALL: f64 = 0.8;
    let mut t = Track::default();
    t.init_input(input);
    let s_tip = std_min(input.s_x + input.length, t.length());
    if s_tip <= input.s_x + 0.03 { return false; }
    // Copper further than a little past the tip does not shape this teardrop.
    t.i0 = t.seg(input.s_root);
    t.i1 = std_min(t.seg(s_tip + 0.3) + 1, t.p.len() - 1);
    t.index();
    let mut ring = Vec::new();
    outline(input, &mut ring);
    let x = t.at(input.s_x);
    let d_x = t.tangent(input.s_x);
    let w_x = t.width_at(input.s_x);
    let want = std_max(0.0, 0.5 * input.width2 - 0.5 * w_x);
    let mut curve = [Vec::new(), Vec::new()];
    let mut has = [false, false];
    let mut tip_at = [s_tip, s_tip];
    let sc = [left, right];
    // Each side tries lower or shorter flanks if a hard bend hides the tip behind the outline.
    const K_SIDE: [[f64; 2]; 18] = [
        [1.0, 1.0], [1.0, 0.75], [0.7, 1.0], [0.7, 0.75], [1.0, 0.55], [0.7, 0.55],
        [0.45, 1.0], [0.45, 0.75], [0.45, 0.55], [1.0, 0.4], [0.7, 0.4], [0.45, 0.4],
        [0.3, 0.75], [0.3, 0.4], [1.0, 0.28], [0.7, 0.28], [0.45, 0.28], [0.3, 0.28],
    ];
    let mut f = Track::default();
    let mut calm = Calm::default(); // C++ K; k below selects the side.
    for k in 0..2 {
        let side = if k == 0 { 1 } else { -1 };
        if sc[k].height <= 0.0 { continue; }
        // Last, without the wall limit: a plain flank is better than none.
        for pass in 0..2 {
            if has[k] { break; }
            for try_it in &K_SIDE {
                let s_t = input.s_x + (s_tip - input.s_x) * try_it[1] * sc[k].length;
                if s_t <= input.s_x + 0.02 { continue; }
                let w_t = t.width_at(s_t);
                let t_t = t.tangent(s_t);
                let n_t = P { x: -t_t.y, y: t_t.x };
                let tip = t.at(s_t) + n_t * (side as f64 * 0.5 * w_t);
                if calm.s_tip != s_t { calm.s_tip = -1.0; }
                calm_path(&t, input.s_root, input.s_x, s_t, s_t - input.s_x, &mut calm);
                calm_track(&t, &calm, side, &mut f);
                let ld = landing(input, &f, &ring, x, d_x, tip, t_t, s_t - input.s_x,
                    if pass != 0 { 0.0 } else { 0.5 }, sc[k].height >= 1.0, side,
                    std_min(want * try_it[0] * sc[k].height, K_TALL * (s_t - input.s_x)));
                let ka = land_dist(ld.a, ld.n, ld.rad, tip);
                if ld.b < K_MIN_FLARE || ka < K_MIN_FLARE { continue; }
                let b = Blend { input, t: &f, a: ld.a, n: ld.n, rad: ld.rad, ka, kb: ld.b,
                    fq: Cell::new(P::default()), fc0: Cell::new(-1e30) };
                if !b.walk(tip, t_t * -1.0, &mut curve[k]) {
                    if pass == 0 { continue; }
                    curve[k] = vec![tip, ld.a];
                }
                has[k] = true;
                tip_at[k] = s_t;
                if input.bridge { bridge(input, &ring, t.at(input.s_root), &mut curve[k]); }
                simplify(&mut curve[k], 0.0015);
                res.flare = std_max(res.flare, ld.b + 0.5 * w_x);
                break;
            }
        }
    }
    if !has[0] && !has[1] { return false; }
    let s_end = std_max(if has[0] { tip_at[0] } else { 0.0 }, if has[1] { tip_at[1] } else { 0.0 });
    let flat = |c: &mut Vec<P>, at: &mut f64| {
        c.clear();
        *at = s_end;
        let n = std_max(2, ((s_end - input.s_root) / 0.1).ceil() as i32);
        for i in (0..=n).rev() { c.push(t.at(input.s_root + (s_end - input.s_root) * i as f64 / n as f64)); }
    };
    if !has[0] { flat(&mut curve[0], &mut tip_at[0]); }
    if !has[1] { flat(&mut curve[1], &mut tip_at[1]); }
    // Root -> right flank -> along the path to the later tip -> left flank down to the outline.
    let mut root = t.at(input.s_root);
    if junction_dist(input, root) > -1e-3 { root = t.p[0]; }
    if input.deep_root && input.pad.is_some() && junction_dist(input, root) > -0.1 {
        let pad = input.pad.unwrap();
        let c = P { x: pad.cx, y: pad.cy };
        if junction_dist(input, c) < junction_dist(input, root) { root = c; }
    }
    let mut poly = Vec::new();
    poly.push(root);
    // ADAPTED: explicit output borrow avoids retaining the closure's borrow during assembly.
    let along = |s0: f64, s1: f64, poly: &mut Vec<P>| {
        if (s1 - s0).abs() < 1e-6 { return; }
        let n = std_max(1, ((s1 - s0).abs() / 0.1).ceil() as i32);
        for i in 0..=n { poly.push(t.at(s0 + (s1 - s0) * i as f64 / n as f64)); }
    };
    for i in (0..curve[1].len()).rev() { poly.push(curve[1][i]); }
    along(tip_at[1], s_end, &mut poly);
    along(s_end, tip_at[0], &mut poly);
    for &q in &curve[0] { poly.push(q); }
    for q in poly {
        let v = Vec2::new(q.x as f32, q.y as f32);
        if res.poly.is_empty() || res.poly.last().unwrap().distance_squared_to(v) > 1e-7_f32 { res.poly.push(v); }
    }
    if res.poly.len() >= 2 && res.poly[0].distance_squared_to(*res.poly.last().unwrap()) < 1e-7_f32 { res.poly.pop(); }
    res.poly.len() >= 3
}
