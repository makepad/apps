//! Infill pattern geometry, one generator per pattern.
//!
//! Density convention (checked against reference slices of every
//! pattern): a pattern at density d with bead spacing s lays d / s mm of
//! line per mm² of region, so `base = s / d` is the spacing of a single
//! line family. Patterns with k families use k·base per family; curve
//! patterns scale their cell so their length per area matches. Honeycomb
//! sizes its hexagons as if each edge were printed once and then doubles
//! the shared flats (4/3 of d / s), as the reference does.
//!
//! All generators work in the region's frame around `pivot` (the object's
//! centre), so the pattern is stable from layer to layer, and add their
//! output to caller-provided buffers.

use crate::clipper::{Clipper, Pieces};
use crate::fill::{self, Align, RawSeg};
use makepad_fab_slicer::geom::*;
use std::f64::consts::{FRAC_PI_2, PI, SQRT_2, TAU};

/// Per-layer inputs every generator may read.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    /// Layer index (direction alternation).
    pub li: usize,
    /// Print height of the layer (mm).
    pub z: f64,
    pub pivot: Pt,
    /// `infill_direction` (radians).
    pub dir: f64,
}

/// One family of parallel lines at `angle`, spacing `sp` mm, shifted
/// `offset` mm along its normal from the lines through the pivot.
pub fn family(region: &[ExPolygon], angle: f64, sp: f64, offset: f64, pivot: Pt) -> Vec<RawSeg> {
    let (sin, cos) = angle.sin_cos();
    let off = offset.rem_euclid(sp);
    let pv = Pt::new(pivot.x + scaled(-sin * off), pivot.y + scaled(cos * off));
    fill::family(region, angle, sp, pv, Align::Grid)
}

/// Rectilinear-family angle of layer `li`: the direction turns 90° per layer.
pub fn alternating(ctx: &Ctx) -> f64 {
    ctx.dir + if ctx.li % 2 == 1 { FRAC_PI_2 } else { 0.0 }
}

/// The offset after `li` layers of a value moving `step` per layer and
/// turning back at ±`half` (a triangle wave).
pub fn bounce(li: usize, step: f64, half: f64) -> f64 {
    if step <= 0.0 || half <= 0.0 {
        return 0.0;
    }
    let period = 4.0 * half;
    let t = (li as f64 * step).rem_euclid(period);
    if t < half {
        t
    } else if t < 3.0 * half {
        2.0 * half - t
    } else {
        t - period
    }
}

/// Three families 120° apart (cubic: offsets z/√2 along each normal;
/// triangles: through the pivot; tri-hexagon: one family half a spacing off).
pub fn three(region: &[ExPolygon], angle0: f64, sp: f64, offsets: [f64; 3], pivot: Pt) -> Vec<RawSeg> {
    let mut segs = Vec::new();
    for (i, off) in offsets.iter().enumerate() {
        segs.extend(family(region, angle0 + i as f64 * TAU / 3.0, sp, *off, pivot));
    }
    segs
}

/// Cubic: three families at `angle0` + 0°/120°/240°, spacing L = 3·base;
/// the planes of cubes standing on a corner cut the layer at z, each
/// family shifted z/√2 along its own normal.
pub fn cubic_offsets(z: f64) -> [f64; 3] {
    let o = z / SQRT_2;
    [o, o, o]
}

/// Rotate points from a pattern frame (angle `a` about `pivot`) to the bed.
struct Frame {
    cos: f64,
    sin: f64,
    pivot: Pt,
}

impl Frame {
    fn new(angle: f64, pivot: Pt) -> Frame {
        let (sin, cos) = angle.sin_cos();
        Frame { cos, sin, pivot }
    }
    fn to_bed(&self, u: f64, v: f64) -> Pt {
        Pt::new(self.pivot.x + (u * self.cos - v * self.sin).round() as i64, self.pivot.y + (u * self.sin + v * self.cos).round() as i64)
    }
    /// The region's bounding box in the frame (units).
    fn bounds(&self, bb: &BBox) -> (f64, f64, f64, f64) {
        let mut r = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for p in [bb.min, bb.max, Pt::new(bb.min.x, bb.max.y), Pt::new(bb.max.x, bb.min.y)] {
            let (dx, dy) = ((p.x - self.pivot.x) as f64, (p.y - self.pivot.y) as f64);
            let u = dx * self.cos + dy * self.sin;
            let v = -dx * self.sin + dy * self.cos;
            r = (r.0.min(u), r.1.max(u), r.2.min(v), r.3.max(v));
        }
        r
    }
}

/// Honeycomb: hexagon prisms of side a = 2·base/√3 (as if each edge were
/// printed once). Each layer is a set of trapezoid waves, neighbours
/// mirrored so they meet along the flats, where the two beads lie side by
/// side half a spacing off the edge. The waves turn 120° about a hexagon
/// centre every layer (the hexagons stay put), so the doubled edges take
/// turns over the three edge directions.
pub fn honeycomb(clip: &mut Clipper, base: f64, s: f64, ctx: &Ctx, out: &mut Pieces) {
    let a = 2.0 * base / 3f64.sqrt() * SCALE;
    let h = a * 3f64.sqrt() / 2.0;
    let inset = (s / 2.0 * SCALE).min(h * 0.45);
    let frame = Frame::new(ctx.dir + (ctx.li % 3) as f64 * TAU / 3.0, ctx.pivot);
    let (u0, u1, v0, v1) = frame.bounds(&clip.bbox);
    // A hexagon centre sits at the pivot: frame origin = wave (0.5a, h).
    let (cu, cv) = (0.5 * a, h);
    let r0 = ((v0 + cv) / h).floor() as i64 - 1;
    let r1 = ((v1 + cv) / h).ceil() as i64 + 1;
    let m0 = ((u0 + cu) / (3.0 * a)).floor() as i64 - 1;
    let m1 = ((u1 + cu) / (3.0 * a)).ceil() as i64 + 1;
    let mut pts = Vec::with_capacity(((m1 - m0) * 4 + 2) as usize);
    for r in r0..=r1 {
        let phase = if r.rem_euclid(2) == 1 { 1.5 * a } else { 0.0 };
        let lo = r as f64 * h + inset - cv;
        let hi = (r + 1) as f64 * h - inset - cv;
        pts.clear();
        for m in m0..=m1 {
            let x = m as f64 * 3.0 * a + phase - cu;
            pts.push(frame.to_bed(x, lo));
            pts.push(frame.to_bed(x + a, lo));
            pts.push(frame.to_bed(x + 1.5 * a, hi));
            pts.push(frame.to_bed(x + 2.5 * a, hi));
        }
        clip.clip(&pts, out);
    }
}

/// 3D honeycomb: one family of trapezoid waves per layer (period 2P, 45°
/// risers, flats f), the flats growing and shrinking with z and the
/// direction turning 90° every block of P mm, so stacked layers close into
/// cells like truncated octahedra. Mean length per area is (1 + √2) / (2P);
/// with the joins along the walls, P = 1.1·(1 + √2)/2 · base matches the
/// reference's length per layer.
pub fn honeycomb_3d(clip: &mut Clipper, base: f64, ctx: &Ctx, out: &mut Pieces) {
    let p = 1.1 * (1.0 + SQRT_2) / 2.0 * base;
    let block = (ctx.z / p).floor();
    let phase = ctx.z / p - block;
    let f = p * (1.0 - (2.0 * phase - 1.0).abs()); // 0 → P → 0 over a block
    let amp = (p - f) / 2.0;
    let turn = if (block as i64).rem_euclid(2) == 1 { FRAC_PI_2 } else { 0.0 };
    let frame = Frame::new(ctx.dir + turn, ctx.pivot);
    let (u0, u1, v0, v1) = frame.bounds(&clip.bbox);
    let (ps, fs, as_) = (p * SCALE, f * SCALE, amp * SCALE);
    let k0 = (v0 / ps).floor() as i64 - 1;
    let k1 = (v1 / ps).ceil() as i64 + 1;
    let m0 = (u0 / (2.0 * ps)).floor() as i64 - 1;
    let m1 = (u1 / (2.0 * ps)).ceil() as i64 + 1;
    let mut pts = Vec::new();
    for k in k0..=k1 {
        let v = k as f64 * ps;
        pts.clear();
        for m in m0..=m1 {
            let x = m as f64 * 2.0 * ps;
            // Low flat, riser up, high flat, riser down.
            pts.push(frame.to_bed(x, v - as_));
            pts.push(frame.to_bed(x + fs, v - as_));
            pts.push(frame.to_bed(x + fs + 2.0 * as_, v + as_));
            pts.push(frame.to_bed(x + 2.0 * fs + 2.0 * as_, v + as_));
        }
        pts.dedup();
        clip.clip(&pts, out);
    }
}

/// Crosshatch: one family per layer whose direction switches every block of
/// `base` mm. Near each switch the lines fold into zig-zags until, at the
/// switch, neighbours meet in a diamond lattice that both directions share,
/// so no layer starts mid-air.
pub fn crosshatch(clip: &mut Clipper, base: f64, ctx: &Ctx, out: &mut Pieces) {
    let half = base;
    let q = (ctx.z / half).rem_euclid(2.0);
    let yfam = q >= 1.0;
    let u = q.fract();
    // The fold takes about a layer each side of a switch.
    let tau = (0.25 / half).min(0.25);
    let t = if u < tau { 1.0 - u / tau } else if u > 1.0 - tau { (u - (1.0 - tau)) / tau } else { 0.0 };
    let b = base * SCALE;
    let frame = Frame::new(ctx.dir + if yfam { FRAC_PI_2 } else { 0.0 }, ctx.pivot);
    // The Y family sits half a spacing over with its fold half a period
    // along, which puts both diamond lattices on the same points.
    let (shift, along) = if yfam { (0.5 * b, 0.5 * b) } else { (0.0, 0.0) };
    let (u0, u1, v0, v1) = frame.bounds(&clip.bbox);
    let k0 = ((v0 - shift) / b).floor() as i64 - 1;
    let k1 = ((v1 - shift) / b).ceil() as i64 + 1;
    let m0 = ((u0 + along) / b).floor() as i64 - 1;
    let m1 = ((u1 + along) / b).ceil() as i64 + 1;
    let mut pts = Vec::new();
    for k in k0..=k1 {
        let v = k as f64 * b + shift;
        pts.clear();
        if t <= 1e-9 {
            pts.push(frame.to_bed(m0 as f64 * b, v));
            pts.push(frame.to_bed(m1 as f64 * b, v));
        } else {
            for m in m0..=m1 {
                let sign = if (k + m).rem_euclid(2) == 0 { 1.0 } else { -1.0 };
                pts.push(frame.to_bed(m as f64 * b - along, v + sign * t * b / 2.0));
            }
        }
        clip.clip(&pts, out);
    }
}

/// Hilbert curve on a grid of `sp` mm cells (rotated by `angle`), over a
/// power-of-two square centred on the pivot; blocks outside the region's
/// box are skipped (the curve breaks there).
pub fn hilbert(clip: &mut Clipper, sp: f64, angle: f64, pivot: Pt, out: &mut Pieces) {
    let frame = Frame::new(angle, pivot);
    let (u0, u1, v0, v1) = frame.bounds(&clip.bbox);
    let c = sp * SCALE;
    let reach = u0.abs().max(u1.abs()).max(v0.abs()).max(v1.abs()) / c + 1.0;
    let mut n = 1u32;
    while ((1u64 << n) as f64) < 2.0 * reach && n < 24 {
        n += 1;
    }
    let side = (1u64 << n) as f64 * c;
    let target = (u0 - c, u1 + c, v0 - c, v1 + c);
    let mut run: Vec<Pt> = Vec::new();
    struct H<'a> {
        frame: &'a Frame,
        target: (f64, f64, f64, f64),
        run: &'a mut Vec<Pt>,
        clip: &'a mut Clipper,
        out: &'a mut Pieces,
    }
    impl H<'_> {
        fn flush(&mut self) {
            if self.run.len() >= 2 {
                self.clip.clip(&self.run[..], self.out);
            }
            self.run.clear();
        }
        #[allow(clippy::too_many_arguments)]
        fn go(&mut self, x0: f64, y0: f64, xi: f64, xj: f64, yi: f64, yj: f64, n: u32) {
            // This block's box.
            let xs = [x0, x0 + xi, x0 + yi, x0 + xi + yi];
            let ys = [y0, y0 + xj, y0 + yj, y0 + xj + yj];
            let (bx0, bx1) = (xs.iter().cloned().fold(f64::INFINITY, f64::min), xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
            let (by0, by1) = (ys.iter().cloned().fold(f64::INFINITY, f64::min), ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
            let t = self.target;
            if bx1 < t.0 || bx0 > t.1 || by1 < t.2 || by0 > t.3 {
                self.flush();
                return;
            }
            if n == 0 {
                self.run.push(self.frame.to_bed(x0 + (xi + yi) / 2.0, y0 + (xj + yj) / 2.0));
                return;
            }
            let (xi2, xj2, yi2, yj2) = (xi / 2.0, xj / 2.0, yi / 2.0, yj / 2.0);
            self.go(x0, y0, yi2, yj2, xi2, xj2, n - 1);
            self.go(x0 + xi2, y0 + xj2, xi2, xj2, yi2, yj2, n - 1);
            self.go(x0 + xi2 + yi2, y0 + xj2 + yj2, xi2, xj2, yi2, yj2, n - 1);
            self.go(x0 + xi2 + yi, y0 + xj2 + yj, -yi2, -yj2, -xi2, -xj2, n - 1);
        }
    }
    let mut hh = H { frame: &frame, target, run: &mut run, clip, out };
    hh.go(-side / 2.0, -side / 2.0, side, 0.0, 0.0, side, n);
    hh.flush();
}

/// The region box's distance range from the pivot (units).
fn radii(bb: &BBox, pivot: Pt) -> (f64, f64) {
    let dx = if pivot.x < bb.min.x { bb.min.x - pivot.x } else if pivot.x > bb.max.x { pivot.x - bb.max.x } else { 0 } as f64;
    let dy = if pivot.y < bb.min.y { bb.min.y - pivot.y } else if pivot.y > bb.max.y { pivot.y - bb.max.y } else { 0 } as f64;
    let far = [bb.min, bb.max, Pt::new(bb.min.x, bb.max.y), Pt::new(bb.max.x, bb.min.y)].iter().map(|p| p.dist(pivot)).fold(0.0, f64::max);
    ((dx * dx + dy * dy).sqrt(), far)
}

/// Archimedean spiral r = sp·θ/2π from the pivot, as chords at most `sp`
/// long (so its arms keep the line spacing), over the radii the region
/// spans.
pub fn archimedean(clip: &mut Clipper, sp: f64, angle: f64, pivot: Pt, out: &mut Pieces) {
    let (r_min, r_max) = radii(&clip.bbox, pivot);
    let k = sp * SCALE / TAU;
    let mut th = ((r_min - sp * SCALE) / k).max(0.0);
    let th_end = (r_max + sp * SCALE) / k;
    let mut pts = Vec::new();
    let chord = sp * SCALE;
    while th <= th_end {
        let r = k * th;
        let (s, c) = (th + angle).sin_cos();
        pts.push(Pt::new(pivot.x + (r * c).round() as i64, pivot.y + (r * s).round() as i64));
        th += (chord / r.max(chord)).min(0.5);
    }
    clip.clip(&pts, out);
}

/// Octagram spiral: a spiral through the points of eight-pointed stars
/// (16 corners a turn, the inner ones at Q of the outer radius), the
/// radius growing P a turn. Length per area of such a spiral is
/// √(1 + Q² − 2Q·cos(π/8)) / (P·Q·sin(π/8)); P is chosen to make it 1/sp.
pub fn octagram(clip: &mut Clipper, sp: f64, angle: f64, pivot: Pt, out: &mut Pieces) {
    const Q: f64 = 0.6;
    let c8 = (PI / 8.0).cos();
    let s8 = (PI / 8.0).sin();
    let pitch = sp * (1.0 + Q * Q - 2.0 * Q * c8).sqrt() / (Q * s8) * SCALE;
    let (r_min, r_max) = radii(&clip.bbox, pivot);
    // The inner corners sit at Q·R: start where they reach the region.
    let k0 = (((r_min - pitch) / pitch).max(0.0) * 16.0).floor() as i64;
    let k1 = ((r_max / Q / pitch + 1.0) * 16.0).ceil() as i64;
    let mut pts = Vec::with_capacity((k1 - k0 + 1).max(0) as usize);
    for k in k0..=k1 {
        let r_out = pitch * k as f64 / 16.0;
        let r = if k % 2 == 0 { r_out } else { Q * r_out };
        let (s, c) = (k as f64 * PI / 8.0 + angle).sin_cos();
        pts.push(Pt::new(pivot.x + (r * c).round() as i64, pivot.y + (r * s).round() as i64));
    }
    pts.dedup();
    clip.clip(&pts, out);
}

/// Concentric loops `sp` apart, the first on the region's outline.
pub fn concentric(region: &[ExPolygon], sp: f64) -> Vec<Vec<Pt>> {
    use makepad_fab_slicer::offset::{offset_ex, DEFAULT_MITER};
    let mut out = Vec::new();
    let mut cur: Vec<ExPolygon> = region.to_vec();
    while !cur.is_empty() {
        for e in &cur {
            for r in e.rings() {
                if r.pts.len() >= 3 {
                    out.push(r.pts.clone());
                }
            }
        }
        cur = offset_ex(&cur, -sp * SCALE, DEFAULT_MITER);
    }
    out
}

/// Half-period of the gyroid in units of `sp` (s / d), the reference's: its
/// curves are sampled an eighth of this apart, 0.4552 mm at 0.12 mm layers
/// and 0.4367 mm at 0.2 mm (s / d 2.828 and 2.714 mm), so P = 1.2874·sp.
pub const GYROID_PERIOD: f64 = 1.2874;

/// Gyroid sections: the zero set of sin x·cos y + sin y·cos z + sin z·cos x
/// (x, y, z scaled by π / P), with z a quarter period on, the phase at which
/// the reference's layers turn from lines along x to lines along y.
///
/// Where |cos z| ≥ |sin z| the section is a set of wave lines along x: for
/// each x, cos z·sin y + sin x·cos y = −sin z·cos x has the two solutions
/// y = −ψ + asin(c / R) and y = −ψ + π − asin(c / R) (R sin(y + ψ) the left
/// side; ψ is continuous because cos z keeps its sign), which never meet
/// inside the band. Elsewhere it is the same along y. Every line is one
/// continuous curve from wall to wall, so neighbouring lines join along the
/// walls like rectilinear lines and a layer prints as one run.
pub fn gyroid(clip: &mut Clipper, z: f64, sp: f64, pivot: Pt, out: &mut Pieces) {
    let p = GYROID_PERIOD * sp;
    let k = PI / p;
    let (sz, cz) = (z * k - FRAC_PI_2).sin_cos();
    let along_x = cz.abs() >= sz.abs();
    // Along x: the constant coefficient (of sin y) is cos z, the term in the
    // sampled axis sin x·cos y, the rest −sin z·cos x. Along y the roles of
    // x and y swap with sin z ↔ cos z: sin z·cos x + cos y·sin x = −cos z·sin y.
    let (konst, other) = if along_x { (cz, sz) } else { (sz, cz) };
    let sign = konst.signum();
    let bb = clip.bbox;
    let (lo_u, hi_u, lo_v, hi_v, pu, pv) = if along_x {
        (bb.min.x, bb.max.x, bb.min.y, bb.max.y, pivot.x, pivot.y)
    } else {
        (bb.min.y, bb.max.y, bb.min.x, bb.max.x, pivot.y, pivot.x)
    };
    // Samples a sixteenth of the period apart along the line, on a grid fixed to the pivot.
    let step = p / 16.0 * SCALE;
    let i0 = (((lo_u - pu) as f64) / step).floor() as i64 - 1;
    let i1 = (((hi_u - pu) as f64) / step).ceil() as i64 + 1;
    // Per sample: u, then the two branch phases (radians, before the 2πn shift).
    let mut samples: Vec<(f64, f64, f64)> = Vec::with_capacity((i1 - i0 + 1) as usize);
    for i in i0..=i1 {
        let u = i as f64 * step;
        let (su, cu) = (u / SCALE * k).sin_cos();
        // Along x: cos z·sin v + sin u·cos v = −sin z·cos u (v = y).
        // Along y: sin z·cos v + cos u·sin v = −cos z·sin u (v = x): the same
        // shape with sin v ↔ cos v, solved below by v' = π/2 − v.
        let (q, c) = if along_x { (su, -other * cu) } else { (cu, -other * su) };
        let (q, c, kc) = (q * sign, c * sign, konst * sign);
        let r = (kc * kc + q * q).sqrt();
        let psi = (q / kc).atan();
        let a = (c / r).clamp(-1.0, 1.0).asin();
        samples.push((u, -psi + a, -psi + PI - a));
    }
    let to_v = |t: f64| if along_x { t } else { FRAC_PI_2 - t };
    let (vmin, vmax) = ((lo_v - pv) as f64 / SCALE * k, (hi_v - pv) as f64 / SCALE * k);
    let tol = scaled(0.01) as f64;
    let mut line: Vec<Pt> = Vec::with_capacity(samples.len());
    for branch in 0..2 {
        let at = |s: &(f64, f64, f64)| to_v(if branch == 0 { s.1 } else { s.2 });
        let (lo, hi) = samples.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), s| (lo.min(at(s)), hi.max(at(s))));
        let n0 = ((vmin - hi) / TAU).floor() as i64;
        let n1 = ((vmax - lo) / TAU).ceil() as i64;
        for n in n0..=n1 {
            line.clear();
            for s in &samples {
                let v = (at(s) + n as f64 * TAU) / k * SCALE;
                let (u, v) = (pu as f64 + s.0, pv as f64 + v);
                line.push(if along_x { Pt::new(u.round() as i64, v.round() as i64) } else { Pt::new(v.round() as i64, u.round() as i64) });
            }
            clip.clip(&simplify_polyline(&line, tol), out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounce_stays_within_half() {
        for li in 0..200 {
            let v = bounce(li, 0.4, 1.3);
            assert!(v.abs() <= 1.3 + 1e-9, "{li} {v}");
        }
        assert_eq!(bounce(0, 0.4, 1.0), 0.0);
        assert!((bounce(1, 0.4, 1.0) - 0.4).abs() < 1e-9);
        assert!((bounce(3, 0.4, 1.0) - 0.8).abs() < 1e-9);
    }
}
