//! Clearance: each arm as a chain of capsules (joint centre to joint centre, then flange and
//! tool), the distance between two arms' capsules, and a capsule's distance to a part given as
//! a distance field. The tool's tip, which works at the part, is left out of the part check by
//! a working allowance.

use crate::arm::*;
use crate::kin::Frames;
use crate::math::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Capsule {
    pub a: V3,
    pub b: V3,
    pub r: f64,
}

/// The closest points of segments `p0 p1` and `q0 q1`: (distance, s, t).
pub fn segment_distance(p0: V3, p1: V3, q0: V3, q1: V3) -> (f64, f64, f64) {
    let d1 = sub(p1, p0);
    let d2 = sub(q1, q0);
    let r = sub(p0, q0);
    let (a, e, f) = (dot(d1, d1), dot(d2, d2), dot(d2, r));
    let (s, t);
    if a <= 1e-18 && e <= 1e-18 {
        return (len(r), 0.0, 0.0);
    }
    if a <= 1e-18 {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = dot(d1, r);
        if e <= 1e-18 {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = dot(d1, d2);
            let denom = a * e - b * b;
            let mut ss = if denom > 1e-18 { ((b * f - c * e) / denom).clamp(0.0, 1.0) } else { 0.0 };
            let mut tt = (b * ss + f) / e;
            if tt < 0.0 {
                tt = 0.0;
                ss = (-c / a).clamp(0.0, 1.0);
            } else if tt > 1.0 {
                tt = 1.0;
                ss = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = ss;
            t = tt;
        }
    }
    let c1 = add(p0, scale(d1, s));
    let c2 = add(q0, scale(d2, t));
    (dist(c1, c2), s, t)
}

/// The gap between two capsules (negative: they overlap).
pub fn capsule_gap(a: &Capsule, b: &Capsule) -> f64 {
    segment_distance(a.a, a.b, b.a, b.b).0 - a.r - b.r
}

/// An arm's capsules: its links, then its heads with the one at work last.
#[derive(Clone, Copy, Debug)]
pub struct Capsules {
    c: [Capsule; DOF + MAX_HEADS],
    n: usize,
}

impl std::ops::Deref for Capsules {
    type Target = [Capsule];
    fn deref(&self) -> &[Capsule] {
        &self.c[..self.n]
    }
}

/// An arm's capsules at `f`: one per link (its joint centre to the next), then the wrist to
/// the flange, then each head from the flange toward its TCP, the one at work last. A head's
/// round end reaches its TCP and no further (its nozzle ends there; a capsule to the TCP
/// itself would bulge its radius past it).
pub fn capsules(arm: &ArmModel, f: &Frames) -> Capsules {
    let mut c = [Capsule { a: ZERO, b: ZERO, r: 0.0 }; DOF + MAX_HEADS];
    for i in 0..DOF {
        let b = if i + 1 < DOF { f.point[i + 1] } else { f.flange.t };
        c[i] = Capsule { a: f.point[i], b, r: arm.radius[i] };
    }
    let head = |tcp: V3, r: f64| {
        let a = f.flange.t;
        let l = dist(a, tcp);
        Capsule { a, b: if l > r { lerp(tcp, a, r / l) } else { a }, r }
    };
    let mut n = DOF;
    for (i, h) in arm.heads.iter().enumerate() {
        if i != arm.head && h.radius > 0.0 {
            c[n] = head(f.flange.apply(h.tcp.t), h.radius);
            n += 1;
        }
    }
    c[n] = head(f.tcp.t, arm.tool().radius);
    Capsules { c, n: n + 1 }
}

/// The least gap between two arms' capsules, and which pair (link of a, link of b).
pub fn arms_gap(a: &[Capsule], b: &[Capsule]) -> (f64, usize, usize) {
    let mut best = (f64::MAX, 0, 0);
    for (i, ca) in a.iter().enumerate() {
        for (j, cb) in b.iter().enumerate() {
            let g = capsule_gap(ca, cb);
            if g < best.0 {
                best = (g, i, j);
            }
        }
    }
    best
}

/// The least gap from `c` to the solid `field` describes (distance, negative inside), sampled
/// along its axis at `step` mm, and where along it (0..1). A 1-Lipschitz field never hides a
/// closer point between samples by more than half a step.
pub fn field_gap(c: &Capsule, field: &dyn Fn(V3) -> f64, step: f64) -> (f64, f64) {
    let l = dist(c.a, c.b);
    let n = ((l / step.max(1e-3)).ceil() as usize).max(1);
    let mut best = (f64::MAX, 0.0);
    for k in 0..=n {
        let t = k as f64 / n as f64;
        let g = field(lerp(c.a, c.b, t)) - c.r;
        if g < best.0 {
            best = (g, t);
        }
    }
    best
}

/// The arm's least gap to the part, the working head's last `allowance` mm (its working end)
/// left out: (gap, capsule index). `caps` as [`capsules`] orders them.
pub fn part_gap(caps: &[Capsule], field: &dyn Fn(V3) -> f64, allowance: f64) -> (f64, usize) {
    let mut best = (f64::MAX, 0);
    for (i, c) in caps.iter().enumerate() {
        let mut c = *c;
        if i == caps.len() - 1 {
            let l = dist(c.a, c.b);
            if l <= allowance {
                continue;
            }
            c.b = lerp(c.a, c.b, (l - allowance) / l);
        }
        let (g, _) = field_gap(&c, field, (c.r * 0.5).max(2.0));
        if g < best.0 {
            best = (g, i);
        }
    }
    best
}
