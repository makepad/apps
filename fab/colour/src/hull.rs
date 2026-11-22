//! The convex hull of a few hundred points in 3D, built one point at a time; points that are
//! flat, on one line or all one point make a hull of that many dimensions (a set of greys mixes
//! only greys).
//!
//! An ink set is full of points on one line or plane and of repeats (a layer opaque in a channel
//! holds that channel whatever lies under it), where a test in floating point says a point is
//! outside one face and inside its neighbour in the same plane. The solid is built on a grid
//! of [`GRID`] steps a unit with exact integer tests instead: which side of a face a point lies
//! on is never in doubt.

use std::collections::HashMap;

pub type P = [f64; 3];

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: P, b: P) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: P, b: P) -> P {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: P) -> f64 {
    dot(a, a).sqrt()
}
fn scale(a: P, k: f64) -> P {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// How far (in the points' units) a point may lie outside and still count as in.
const TOLERANCE: f64 = 1e-7;
/// Grid steps a unit the solid is built on: a point moves under a hundredth of [`TOLERANCE`].
const GRID: f64 = (1u64 << 30) as f64;

#[derive(Clone, Debug)]
enum Shape {
    Point(P),
    /// From one end to the other.
    Segment(P, P),
    /// In the plane through `origin` spanned by `u` and `v` (unit, square), the ring
    /// counter-clockwise in (u, v).
    Flat { origin: P, u: P, v: P, normal: P, ring: Vec<[f64; 2]> },
    /// Each face's outward unit normal and offset (a point p is in where n·p ≤ d).
    Solid { planes: Vec<(P, f64)> },
}

#[derive(Clone, Debug)]
pub struct Hull {
    shape: Shape,
}

impl Hull {
    pub fn new(points: &[P]) -> Hull {
        assert!(!points.is_empty(), "a hull of no points");
        let span = points.iter().fold(0.0f64, |m, p| m.max(norm(sub(*p, points[0]))));
        let eps = 1e-9 * span.max(1e-12);
        let far = |from: &dyn Fn(P) -> f64| (0..points.len()).max_by(|&a, &b| from(points[a]).total_cmp(&from(points[b]))).unwrap();
        let i0 = (0..points.len()).min_by(|&a, &b| points[a][0].total_cmp(&points[b][0])).unwrap();
        let p0 = points[i0];
        let i1 = far(&|p| norm(sub(p, p0)));
        let p1 = points[i1];
        if norm(sub(p1, p0)) <= eps {
            return Hull { shape: Shape::Point(p0) };
        }
        let dir = scale(sub(p1, p0), 1.0 / norm(sub(p1, p0)));
        let off_line = |p: P| norm(cross(sub(p, p0), dir));
        let i2 = far(&off_line);
        if off_line(points[i2]) <= eps {
            let t = |p: P| dot(sub(p, p0), dir);
            let lo = (0..points.len()).min_by(|&a, &b| t(points[a]).total_cmp(&t(points[b]))).unwrap();
            let hi = (0..points.len()).max_by(|&a, &b| t(points[a]).total_cmp(&t(points[b]))).unwrap();
            return Hull { shape: Shape::Segment(points[lo], points[hi]) };
        }
        let n = cross(sub(p1, p0), sub(points[i2], p0));
        let normal = scale(n, 1.0 / norm(n));
        let off_plane = |p: P| dot(sub(p, p0), normal).abs();
        let i3 = far(&off_plane);
        if off_plane(points[i3]) <= eps {
            let u = dir;
            let v = cross(normal, u);
            let mut uv: Vec<[f64; 2]> = points.iter().map(|p| [dot(sub(*p, p0), u), dot(sub(*p, p0), v)]).collect();
            return Hull { shape: Shape::Flat { origin: p0, u, v, normal, ring: ring_2d(&mut uv) } };
        }
        let grid: Vec<Q> = points.iter().map(|p| p.map(|x| (x * GRID).round() as i64)).collect();
        match solid(&grid, [i0, i1, i2, i3]) {
            Some(planes) => Hull { shape: Shape::Solid { planes } },
            // flat on the grid, past what the floating point test told
            None => {
                let u = dir;
                let v = cross(normal, u);
                let mut uv: Vec<[f64; 2]> = points.iter().map(|p| [dot(sub(*p, p0), u), dot(sub(*p, p0), v)]).collect();
                Hull { shape: Shape::Flat { origin: p0, u, v, normal, ring: ring_2d(&mut uv) } }
            }
        }
    }

    /// 0 for a point, 1 a segment, 2 a flat polygon, 3 a solid.
    pub fn dimension(&self) -> usize {
        match self.shape {
            Shape::Point(_) => 0,
            Shape::Segment(..) => 1,
            Shape::Flat { .. } => 2,
            Shape::Solid { .. } => 3,
        }
    }

    /// The point nearest `p` on the hull's span (its line, its plane); `p` for a solid.
    pub fn onto_span(&self, p: P) -> P {
        match &self.shape {
            Shape::Point(q) => *q,
            Shape::Segment(a, b) => {
                let d = sub(*b, *a);
                add(*a, scale(d, dot(sub(p, *a), d) / dot(d, d)))
            }
            Shape::Flat { origin, normal, .. } => sub(p, scale(*normal, dot(sub(p, *origin), *normal))),
            Shape::Solid { .. } => p,
        }
    }

    /// Whether `p` lies in the hull.
    pub fn contains(&self, p: P) -> bool {
        match &self.shape {
            Shape::Point(q) => norm(sub(p, *q)) <= TOLERANCE,
            Shape::Segment(a, b) => {
                let d = sub(*b, *a);
                let t = dot(sub(p, *a), d) / dot(d, d);
                (-TOLERANCE..=1.0 + TOLERANCE).contains(&t) && norm(sub(p, add(*a, scale(d, t)))) <= TOLERANCE
            }
            Shape::Flat { origin, u, v, normal, ring } => {
                let q = sub(p, *origin);
                if dot(q, *normal).abs() > TOLERANCE {
                    return false;
                }
                let (x, y) = (dot(q, *u), dot(q, *v));
                (0..ring.len()).all(|i| {
                    let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                    let len = (b[0] - a[0]).hypot(b[1] - a[1]);
                    (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]) >= -TOLERANCE * len
                })
            }
            Shape::Solid { planes } => planes.iter().all(|(n, d)| dot(*n, p) - d <= TOLERANCE),
        }
    }
}

/// The convex ring of points in a plane, counter-clockwise (monotone chain).
fn ring_2d(pts: &mut Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup();
    let turn = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut lower: Vec<[f64; 2]> = Vec::new();
    for &p in pts.iter() {
        while lower.len() >= 2 && turn(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<[f64; 2]> = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && turn(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// A point on the grid.
type Q = [i64; 3];

/// Which side of the plane through `a`, `b`, `c` the point `d` lies on, exactly: over 0 where
/// `a b c` turn counter-clockwise seen from `d`.
fn orient(a: Q, b: Q, c: Q, d: Q) -> i128 {
    let v = |p: Q| [(p[0] - a[0]) as i128, (p[1] - a[1]) as i128, (p[2] - a[2]) as i128];
    let (x, y, z) = (v(b), v(c), v(d));
    x[0] * (y[1] * z[2] - y[2] * z[1]) - x[1] * (y[0] * z[2] - y[2] * z[0]) + x[2] * (y[0] * z[1] - y[1] * z[0])
}

/// The faces' planes (outward unit normal, offset) of the hull of grid points round the
/// tetrahedron `start`; none when its corners lie in one plane. A face is seen from a point
/// strictly in front of it, so a point in a face's plane never cuts it; a face of no area (its
/// corners on one line) bounds nothing and gives no plane.
fn solid(points: &[Q], start: [usize; 4]) -> Option<Vec<(P, f64)>> {
    let [a, b, c, d] = start;
    let o = orient(points[a], points[b], points[c], points[d]);
    if o == 0 {
        return None;
    }
    // outward: the tetrahedron's other corner behind each face
    let out = |f: [usize; 3], other: usize| if orient(points[f[0]], points[f[1]], points[f[2]], points[other]) > 0 { [f[0], f[2], f[1]] } else { f };
    let mut faces: Vec<Option<[usize; 3]>> = vec![Some(out([a, b, c], d)), Some(out([a, b, d], c)), Some(out([a, c, d], b)), Some(out([b, c, d], a))];
    let mut edges: HashMap<(usize, usize), usize> = HashMap::new();
    for (i, f) in faces.iter().enumerate() {
        let f = f.unwrap();
        for k in 0..3 {
            edges.insert((f[k], f[(k + 1) % 3]), i);
        }
    }
    let sees = |f: [usize; 3], p: usize| orient(points[f[0]], points[f[1]], points[f[2]], points[p]) > 0;
    for pi in 0..points.len() {
        if start.contains(&pi) {
            continue;
        }
        let visible: Vec<usize> = (0..faces.len()).filter(|&i| faces[i].is_some_and(|f| sees(f, pi))).collect();
        if visible.is_empty() {
            continue;
        }
        let seen = |i: usize| visible.binary_search(&i).is_ok();
        let mut horizon = Vec::new();
        for &i in &visible {
            let f = faces[i].unwrap();
            for k in 0..3 {
                let (u, w) = (f[k], f[(k + 1) % 3]);
                if !edges.get(&(w, u)).is_some_and(|&j| seen(j)) {
                    horizon.push((u, w));
                }
            }
        }
        for &i in &visible {
            let f = faces[i].take().unwrap();
            for k in 0..3 {
                edges.remove(&(f[k], f[(k + 1) % 3]));
            }
        }
        for (u, w) in horizon {
            let f = [u, w, pi];
            let i = faces.len();
            faces.push(Some(f));
            for k in 0..3 {
                edges.insert((f[k], f[(k + 1) % 3]), i);
            }
        }
    }
    let at = |q: Q| q.map(|x| x as f64 / GRID);
    Some(
        faces
            .iter()
            .flatten()
            .filter_map(|f| {
                let p = f.map(|i| at(points[i]));
                let n = cross(sub(p[1], p[0]), sub(p[2], p[0]));
                let len = norm(n);
                (len > 0.0).then(|| {
                    let n = scale(n, 1.0 / len);
                    (n, dot(n, p[0]))
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cube's corners and inner points make the cube: its centre and corners are in, a point
    /// past a face is out; flat, straight and single points make hulls of fewer dimensions that
    /// hold what lies between them.
    #[test]
    fn hulls_of_every_dimension_hold_what_lies_between() {
        let mut cube: Vec<P> = (0..8).map(|i| [(i & 1) as f64, ((i >> 1) & 1) as f64, ((i >> 2) & 1) as f64]).collect();
        cube.extend([[0.5, 0.5, 0.5], [0.2, 0.9, 0.4], [0.99, 0.01, 0.5]]);
        let h = Hull::new(&cube);
        assert_eq!(h.dimension(), 3);
        assert!(h.contains([0.5, 0.5, 0.5]) && h.contains([1.0, 1.0, 1.0]) && h.contains([0.0, 0.3, 1.0]));
        assert!(!h.contains([1.01, 0.5, 0.5]) && !h.contains([0.5, -0.01, 0.5]));
        let greys = Hull::new(&[[0.1; 3], [0.9; 3], [0.5; 3]]);
        assert_eq!(greys.dimension(), 1);
        assert!(greys.contains([0.3; 3]) && !greys.contains([0.95; 3]) && !greys.contains([0.3, 0.3, 0.31]));
        assert_eq!(greys.onto_span([0.2, 0.3, 0.4]).map(|x| (x * 1e6).round() / 1e6), [0.3; 3]);
        let flat = Hull::new(&[[0.0, 0.0, 0.5], [1.0, 0.0, 0.5], [0.0, 1.0, 0.5], [0.2, 0.2, 0.5]]);
        assert_eq!(flat.dimension(), 2);
        assert!(flat.contains([0.3, 0.3, 0.5]) && !flat.contains([0.6, 0.6, 0.5]) && !flat.contains([0.3, 0.3, 0.6]));
        let one = Hull::new(&[[0.4; 3], [0.4; 3]]);
        assert!(one.dimension() == 0 && one.contains([0.4; 3]) && !one.contains([0.41; 3]));
    }

    /// Points as an ink set gives them (a line of reds whose other channels an opaque layer
    /// holds, a plane of cyans with no red, one black six times over) make a solid that holds
    /// every one of them and what lies between, and nothing past them.
    #[test]
    fn a_solid_of_lines_planes_and_repeats_holds_them_all() {
        let mut pts: Vec<P> = [0.30, 0.72, 0.74, 0.31, 0.74, 0.67].iter().map(|r| [*r, 0.005, 0.027]).collect();
        pts.extend([[1e-4, 0.40, 0.79], [1e-4, 0.25, 0.44], [1e-4, 0.51, 0.32], [1e-4, 0.26, 0.32], [1e-4, 0.52, 0.81]]);
        pts.extend([[0.0103; 3]; 6]);
        pts.extend([[0.84, 0.82, 0.32], [0.84, 0.84, 0.84], [0.33, 0.33, 0.33], [0.82, 0.32, 0.45]]);
        let h = Hull::new(&pts);
        assert_eq!(h.dimension(), 3);
        let n = pts.len() as f64;
        let mid = pts.iter().fold([0.0; 3], |s, p| add(s, scale(*p, 1.0 / n)));
        assert!(pts.iter().all(|p| h.contains(*p)) && h.contains(mid), "the solid lost a point");
        assert!(!h.contains([0.9, 0.005, 0.027]) && !h.contains([0.0, 0.0, 0.0]) && !h.contains([0.9, 0.9, 0.9]));
    }
}
