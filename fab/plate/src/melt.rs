//! Heat-set inserts' holes (`insert(.., melt: heat_insert(..))`): where one sits in its part, and
//! how much of the part is round its hole and under it.

use crate::PartMesh;

/// A heat-set insert's hole in its part: millimetres, in the frame its numbers were placed in
/// (the model's on an [`crate::Insert`], the plate's from [`crate::ExcludedPart::melt`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Melt {
    /// Its size as its table names it (`M3`).
    pub size: String,
    /// The middle of the hole's mouth, on the part's face, and the unit axis into the part.
    pub mouth: [f64; 3],
    pub axis: [f64; 3],
    /// Across the hole at its mouth, and the hole's depth and narrowing over that depth.
    pub hole: f64,
    pub depth: f64,
    pub taper: f64,
    /// What it wants of the part round the hole and under it.
    pub wall: f64,
    pub floor: f64,
}

/// How a part holds a hole: the least of it round the hole and under it (mm), or no hole there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seat {
    /// The part is solid where the hole should be: nothing cut it.
    NoHole,
    Held { wall: f64, floor: f64 },
}

/// How far into the part a wall or floor ray starts, past the hole's own face (mm).
const START: f64 = 0.02;
/// Round the hole: rays at these fractions of its depth, this many round.
const DEPTHS: [f64; 3] = [0.2, 0.5, 0.85];
const AROUND: usize = 12;

impl Melt {
    /// This hole moved by an affine map (rows of [rotation | translation]): its mouth and axis.
    pub fn placed(&self, m: &[[f64; 4]; 3]) -> Melt {
        let p = self.mouth;
        let a = self.axis;
        let mouth = std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
        let v: [f64; 3] = std::array::from_fn(|r| m[r][0] * a[0] + m[r][1] * a[1] + m[r][2] * a[2]);
        Melt { mouth, axis: unit(v).unwrap_or(a), ..self.clone() }
    }

    /// How `mesh` (its part, a closed mesh with outward faces, in this hole's frame) holds it:
    /// rays from the hole's side out at three depths and twelve ways round, and from its bottom
    /// down. A ray from inside the part meets a face it leaves through first; one that meets a
    /// face it enters through first, or none, started outside: no part there. Cost: 41 rays, each
    /// O(triangles); on the evaluator's worker, once per evaluation.
    pub fn seat(&self, mesh: &PartMesh) -> Seat {
        let Some(axis) = unit(self.axis) else { return Seat::NoHole };
        let (u, v) = across(axis);
        let at = |t: f64| add(self.mouth, scale(axis, t));
        // The hole's middle half way down: inside the part, nothing cut it.
        if matches!(first_hit(mesh, at(0.5 * self.depth), u), Some((_, true))) && matches!(first_hit(mesh, at(0.5 * self.depth), scale(u, -1.0)), Some((_, true))) {
            return Seat::NoHole;
        }
        let mut wall = f64::INFINITY;
        for f in DEPTHS {
            let t = f * self.depth;
            let r = 0.5 * (self.hole - self.taper * f);
            for k in 0..AROUND {
                let a = k as f64 * std::f64::consts::TAU / AROUND as f64;
                let w = add(scale(u, a.cos()), scale(v, a.sin()));
                let start = add(at(t), scale(w, r + START));
                wall = wall.min(through(mesh, start, w));
            }
        }
        let mut floor = f64::INFINITY;
        let r = 0.25 * (self.hole - self.taper);
        for (cu, cv) in [(0.0, 0.0), (1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
            let start = add(add(at(self.depth + START), scale(u, cu * r)), scale(v, cv * r));
            floor = floor.min(through(mesh, start, axis));
        }
        Seat::Held { wall, floor }
    }
}

/// How much of the part lies from `start` along `dir` before it ends: 0 when `start` is not in it.
fn through(mesh: &PartMesh, start: [f64; 3], dir: [f64; 3]) -> f64 {
    match first_hit(mesh, start, dir) {
        Some((t, true)) => t + START,
        _ => 0.0,
    }
}

/// The nearest face along the ray and whether the ray leaves the part through it.
fn first_hit(mesh: &PartMesh, o: [f64; 3], d: [f64; 3]) -> Option<(f64, bool)> {
    let mut best: Option<(f64, bool)> = None;
    for t in &mesh.triangles {
        let [a, b, c] = t.map(|i| mesh.positions[i as usize]);
        let e1 = sub(b, a);
        let e2 = sub(c, a);
        let h = cross(d, e2);
        let det = dot(e1, h);
        if det.abs() < 1e-12 {
            continue;
        }
        let s = sub(o, a);
        let bu = dot(s, h) / det;
        let q = cross(s, e1);
        let bv = dot(d, q) / det;
        let dist = dot(e2, q) / det;
        if bu < 0.0 || bv < 0.0 || bu + bv > 1.0 || dist <= 0.0 {
            continue;
        }
        if best.is_none_or(|(t, _)| dist < t) {
            best = Some((dist, dot(cross(e1, e2), d) > 0.0));
        }
    }
    best
}

/// Two unit vectors square to `a` and to each other.
fn across(a: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let pick = if a[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let u = unit(cross(a, pick)).unwrap_or([1.0, 0.0, 0.0]);
    (u, cross(a, u))
}

fn unit(a: [f64; 3]) -> Option<[f64; 3]> {
    let n = dot(a, a).sqrt();
    (n > 1e-12).then(|| scale(a, 1.0 / n))
}
fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { [a[0] + b[0], a[1] + b[1], a[2] + b[2]] }
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
fn scale(a: [f64; 3], k: f64) -> [f64; 3] { [a[0] * k, a[1] * k, a[2] * k] }
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]] }
