//! The supports' solids, each with its exact distance (printer mm, below 0 inside) and a mesh
//! to draw it by:
//! - [`Solid::Round`]: two balls and every ball on the line between them, their radii between
//!   the two (the balls' hull: a capsule when the radii agree, a head when they don't);
//! - [`Solid::Cut`]: a cone cut flat at both ends (a pillar's base);
//! - [`Pad`]: the plate under the supports, an outline with a chamfered edge (its distance a
//!   proven bound, its layers drawn from its outline).

use crate::section::Box3;
use fab_plate::PartMesh;
use makepad_fab_slicer::geom::{ExPolygon, Polygon, Pt, SCALE};
use makepad_fab_slicer::offset::{self, Join};
use std::f64::consts::TAU;

pub type V3 = [f64; 3];

pub(crate) fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub(crate) fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub(crate) fn scale(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
pub(crate) fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub(crate) fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub(crate) fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}
pub(crate) fn lerp(a: V3, b: V3, t: f64) -> V3 {
    add(a, scale(sub(b, a), t))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Solid {
    /// The balls of radius `ra` at `a` and `rb` at `b`, and every ball on the line between them,
    /// its radius in proportion. The larger must not hold the smaller.
    Round { a: V3, ra: f64, b: V3, rb: f64 },
    /// A cone cut flat at both ends: radius `ra` at `a`, `rb` at `b`.
    Cut { a: V3, ra: f64, b: V3, rb: f64 },
}

impl Solid {
    pub fn distance(&self, p: V3) -> f64 {
        match *self {
            Solid::Round { a, ra, b, rb } => round(p, a, ra, b, rb),
            Solid::Cut { a, ra, b, rb } => cut(p, a, ra, b, rb),
        }
    }

    pub fn bounds(&self) -> Box3 {
        let (Solid::Round { a, ra, b, rb } | Solid::Cut { a, ra, b, rb }) = *self;
        (std::array::from_fn(|k| (a[k] - ra).min(b[k] - rb)), std::array::from_fn(|k| (a[k] + ra).max(b[k] + rb)))
    }

    /// Its surface as triangles, `around` sides about its axis.
    pub fn mesh(&self, around: u32, out: &mut PartMesh) {
        match *self {
            Solid::Round { a, ra, b, rb } => {
                // the profile: the ball at a from its pole to where the cone touches it, the
                // cone, then the ball at b on to its pole; the cone meets each ball at the
                // angle whose sine is (ra - rb) / length from the balls' equators
                let l = norm(sub(b, a)).max(1e-9);
                let s = ((ra - rb) / l).clamp(-1.0, 1.0);
                let tilt = s.asin();
                let steps = (around / 4).max(2);
                let mut ring: Vec<(f64, f64)> = Vec::new();
                // (along the axis from a, radius)
                for k in 0..=steps {
                    let ang = -std::f64::consts::FRAC_PI_2 + (std::f64::consts::FRAC_PI_2 + tilt) * k as f64 / steps as f64;
                    ring.push((ra * ang.sin(), ra * ang.cos()));
                }
                for k in 0..=steps {
                    let ang = tilt + (std::f64::consts::FRAC_PI_2 - tilt) * k as f64 / steps as f64;
                    ring.push((l + rb * ang.sin(), rb * ang.cos()));
                }
                lathe(a, b, &ring, around, out);
            }
            Solid::Cut { a, ra, b, rb } => {
                let l = norm(sub(b, a));
                lathe(a, b, &[(0.0, 0.0), (0.0, ra), (l, rb), (l, 0.0)], around, out);
            }
        }
    }
}

/// The balls' hull's distance: outside, the nearest ball's; inside, the depth into it.
fn round(p: V3, a: V3, ra: f64, b: V3, rb: f64) -> f64 {
    let ba = sub(b, a);
    let l2 = dot(ba, ba);
    if l2 < 1e-18 {
        return norm(sub(p, a)) - ra.max(rb);
    }
    let rr = ra - rb;
    let a2 = l2 - rr * rr;
    let pa = sub(p, a);
    let y = dot(pa, ba);
    let z = y - l2;
    let q = sub(scale(pa, l2), scale(ba, y));
    let x2 = dot(q, q);
    let (y2, z2) = (y * y * l2, z * z * l2);
    let k = rr.signum() * rr * rr * x2;
    if z.signum() * a2 * z2 > k {
        return (x2 + z2).sqrt() / l2 - rb;
    }
    if y.signum() * a2 * y2 < k {
        return (x2 + y2).sqrt() / l2 - ra;
    }
    ((x2 * a2 / l2).sqrt() + y * rr) / l2 - ra
}

/// The cut cone's distance.
fn cut(p: V3, a: V3, ra: f64, b: V3, rb: f64) -> f64 {
    let ba = sub(b, a);
    let baba = dot(ba, ba);
    let pa = sub(p, a);
    let papa = dot(pa, pa);
    let paba = dot(pa, ba) / baba;
    let x = (papa - paba * paba * baba).max(0.0).sqrt();
    let rba = rb - ra;
    let cax = (x - if paba < 0.5 { ra } else { rb }).max(0.0);
    let cay = (paba - 0.5).abs() - 0.5;
    let k = rba * rba + baba;
    let f = ((rba * (x - ra) + paba * baba) / k).clamp(0.0, 1.0);
    let cbx = x - ra - f * rba;
    let cby = paba - f;
    let s = if cbx < 0.0 && cay < 0.0 { -1.0 } else { 1.0 };
    s * (cax * cax + cay * cay * baba).min(cbx * cbx + cby * cby * baba).sqrt()
}

/// Two unit vectors square to `d` and to each other.
pub(crate) fn frame(d: V3) -> (V3, V3) {
    let n = scale(d, 1.0 / norm(d).max(1e-12));
    let o = if n[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
    let u = cross(n, o);
    let u = scale(u, 1.0 / norm(u));
    (u, cross(n, u))
}

/// The surface swept by `profile` ((along the axis from `a` toward `b`, radius), from one pole
/// to the other) about the line a b.
fn lathe(a: V3, b: V3, profile: &[(f64, f64)], around: u32, out: &mut PartMesh) {
    let d = sub(b, a);
    let l = norm(d).max(1e-12);
    let n = scale(d, 1.0 / l);
    let (u, v) = frame(d);
    let m = around.max(3);
    // each profile point's ring: its first vertex and whether it is one point (a pole)
    let mut rings: Vec<(u32, bool)> = Vec::with_capacity(profile.len());
    for &(t, r) in profile {
        let c = add(a, scale(n, t));
        let pole = r <= 1e-12;
        rings.push((out.positions.len() as u32, pole));
        if pole {
            out.positions.push(c);
            continue;
        }
        for k in 0..m {
            let ang = TAU * k as f64 / m as f64;
            out.positions.push(add(c, add(scale(u, ang.cos() * r), scale(v, ang.sin() * r))));
        }
    }
    // outward: the rings run counter-clockwise about the axis, from a toward b
    for w in rings.windows(2) {
        let ((r0, p0), (r1, p1)) = (w[0], w[1]);
        let at = |r: u32, pole: bool, k: u32| if pole { r } else { r + k % m };
        for k in 0..m {
            let (a0, a1, b0, b1) = (at(r0, p0, k), at(r0, p0, k + 1), at(r1, p1, k), at(r1, p1, k + 1));
            if !p0 {
                out.triangles.push([a0, a1, b1]);
            }
            if !p1 {
                out.triangles.push([a0, b1, b0]);
            }
        }
    }
}

/// How far (mm) the pad's arcs may fall inside the circles they follow, each offset's half and
/// its outline's simplifying the other.
pub(crate) const PAD_CHORD: f64 = 0.005;

/// The pad: a slab on the plate from height `z[0]` to `z[1]` under the outline `ring`
/// (counter-clockwise, no holes), its edge drawing in by `inset` from its top to the plate (a
/// chamfer, R7 §4.3). Its layers grow from its bottom, the outline drawn in by the inset, each by
/// the chamfer's run to its height ([`Pad::cut_at`]), so every layer holds on to the one under
/// it. Where the outline curves tighter than the inset or narrows below twice it (the cusps and
/// necks the closing leaves where two rows of bases nearly meet), drawing it in corner by corner
/// folded over and lit specks nothing under them held: those parts of the outline don't print.
/// A piece whose bottom cures no pixel prints on no layer. The mesh draws in corner by corner
/// ([`Pad::outline_at`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Pad {
    pub ring: Vec<[f64; 2]>,
    /// The outline drawn in by `inset`, each piece of it curing a pixel: the layers grow from it.
    pub bottom: Vec<ExPolygon>,
    pub z: [f64; 2],
    pub inset: f64,
}

impl Pad {
    /// The pad under `ring` from `z[0]` to `z[1]`, drawing in by `inset`.
    pub fn new(ring: Vec<[f64; 2]>, z: [f64; 2], inset: f64) -> Pad {
        let top = ExPolygon::new(Polygon::new(ring.iter().map(|p| Pt::from_mm(p[0], p[1])).collect()));
        let bottom = offset::offset_ex(&[top], -inset * SCALE, Join::Round(0.5 * PAD_CHORD * SCALE));
        Pad { ring, bottom, z, inset }
    }

    /// Its cut at height `z`: its bottom grown by the chamfer's run up to there, as outlines and
    /// their holes (bed mm, the holes winding the other way).
    pub fn cut_at(&self, z: f64) -> Vec<Vec<[f64; 2]>> {
        let run = self.inset * ((z - self.z[0]) / (self.z[1] - self.z[0])).clamp(0.0, 1.0);
        let grown = if run > 0.0 { offset::offset_ex(&self.bottom, run * SCALE, Join::Round(0.5 * PAD_CHORD * SCALE)) } else { self.bottom.clone() };
        grown.iter().flat_map(|e| e.rings()).map(|r| r.pts.iter().map(|q| [q.x_mm(), q.y_mm()]).collect()).collect()
    }

    /// Its top outline's distance on the bed (below 0 inside).
    pub fn outline_distance(&self, p: [f64; 2]) -> f64 {
        let r = &self.ring;
        let mut d = f64::INFINITY;
        let mut inside = false;
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            d = d.min((p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy));
            if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / dy * dx {
                inside = !inside;
            }
        }
        if inside { -d } else { d }
    }

    /// A distance it is proven not to be nearer than (below 0 inside): the most of its top's,
    /// its bottom's and its edge's, each a face's own distance near it.
    pub fn distance(&self, p: V3) -> f64 {
        let c = self.inset / (self.z[1] - self.z[0]);
        let edge = (self.outline_distance([p[0], p[1]]) + (self.z[1] - p[2]) * c) / (1.0 + c * c).sqrt();
        edge.max(p[2] - self.z[1]).max(self.z[0] - p[2])
    }

    pub fn bounds(&self) -> Box3 {
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in &self.ring {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        ([lo[0], lo[1], self.z[0]], [hi[0], hi[1], self.z[1]])
    }

    /// Its outline at height `z`: the top's drawn in by the chamfer's share there.
    pub fn outline_at(&self, z: f64) -> Vec<[f64; 2]> {
        let d = self.inset * ((self.z[1] - z) / (self.z[1] - self.z[0])).clamp(0.0, 1.0);
        let r = &self.ring;
        let n = r.len();
        let out = |a: [f64; 2], b: [f64; 2]| {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l = dx.hypot(dy);
            [dy / l, -dx / l]
        };
        // the corner that keeps `d` from both its edges: along their normals' sum, d over one
        // plus their cosine
        (0..n)
            .map(|i| {
                let (a, v, b) = (r[(i + n - 1) % n], r[i], r[(i + 1) % n]);
                let (n1, n2) = (out(a, v), out(v, b));
                let k = d / (1.0 + n1[0] * n2[0] + n1[1] * n2[1]);
                [v[0] - (n1[0] + n2[0]) * k, v[1] - (n1[1] + n2[1]) * k]
            })
            .collect()
    }

    /// Its surface as triangles: the top and the bottom cut ear by ear, the edge between.
    pub fn mesh(&self, out: &mut PartMesh) {
        let n = self.ring.len() as u32;
        if n < 3 {
            return;
        }
        let bottom = self.outline_at(self.z[0]);
        let bot = out.positions.len() as u32;
        let top = bot + n;
        out.positions.extend(bottom.iter().map(|p| [p[0], p[1], self.z[0]]));
        out.positions.extend(self.ring.iter().map(|p| [p[0], p[1], self.z[1]]));
        for i in 0..n {
            let j = (i + 1) % n;
            out.triangles.push([bot + i, bot + j, top + j]);
            out.triangles.push([bot + i, top + j, top + i]);
        }
        out.triangles.extend(ears(&self.ring).into_iter().map(|[a, b, c]| [top + a, top + b, top + c]));
        out.triangles.extend(ears(&bottom).into_iter().map(|[a, b, c]| [bot + a, bot + c, bot + b]));
    }
}

/// A simple counter-clockwise outline cut into counter-clockwise triangles, an ear at a time:
/// a convex corner whose triangle holds no other corner (only a reflex corner can be in one).
fn ears(ring: &[[f64; 2]]) -> Vec<[u32; 3]> {
    let turn = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let n = ring.len();
    let reflex: Vec<usize> = (0..n).filter(|&i| turn(ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]) <= 0.0).collect();
    let mut gone = vec![false; n];
    let mut left: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity(n.saturating_sub(2));
    let (mut i, mut since) = (0usize, 0usize);
    while left.len() > 3 {
        let m = left.len();
        i %= m;
        let (a, b, c) = (left[(i + m - 1) % m], left[i], left[(i + 1) % m]);
        let (pa, pb, pc) = (ring[a], ring[b], ring[c]);
        let holds = |k: usize| !gone[k] && k != a && k != b && k != c && turn(pa, pb, ring[k]) >= 0.0 && turn(pb, pc, ring[k]) >= 0.0 && turn(pc, pa, ring[k]) >= 0.0;
        // a full round with no ear is the rounding of a sliver: the next corner goes as one
        if (turn(pa, pb, pc) > 0.0 && !reflex.iter().any(|&k| holds(k))) || since > m {
            out.push([a as u32, b as u32, c as u32]);
            gone[b] = true;
            left.remove(i);
            i = (i + m - 2) % (m - 1);
            since = 0;
        } else {
            i += 1;
            since += 1;
        }
    }
    if left.len() == 3 {
        out.push([left[0] as u32, left[1] as u32, left[2] as u32]);
    }
    out
}
