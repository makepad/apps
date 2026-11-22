//! Convex body geometry for case fit checks in the board frame, in mm.
//! `new` runs once per body per board change, on a few hundred posed hull corners; nothing here runs per frame.
//! A closest-pair query is O(points + faces x 3 + edges x 3) per case triangle, with faces linear in points.
//! Containment visits the planes; overlap and clipping visit the current polygon at each plane (overlap
//! after a 3-dot rejection per plane). Only construction, overlap and clipping allocate, two buffers a call.

use crate::board_mech::dot;
use crate::checks::{along, clip_plane_into, closest_on_segments, closest_on_triangle, cross, sub};
use makepad_dynamics::hull::convex_hull;

// Keep the fit check's tolerance without widening the visibility of its constant.
const EPS: f64 = 1e-9;

/// A convex body in the board frame (mm): the hull of a part's posed mesh points.
#[derive(Clone, Debug)]
pub struct BodyHull {
    /// The hull's corner points.
    pub points: Vec<[f64; 3]>,
    /// Its triangles, indices into `points`, counter-clockwise seen from outside.
    pub faces: Vec<[u32; 3]>,
    /// Each edge once (lower index first), indices into `points`.
    pub edges: Vec<[u32; 2]>,
    /// Its face planes, each distinct plane once: outward unit normal `n` and offset `d`; a point p is
    /// inside when `dot(n, p) <= d` for every plane.
    pub planes: Vec<([f64; 3], f64)>,
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl BodyHull {
    /// The hull of `points`. None when there are fewer than 4 points, any coordinate is not finite, or
    /// the points span no volume (all coplanar or collinear): the caller then keeps the box.
    /// A failed hull build or planes that cannot be represented at the fit tolerance also keep the box.
    pub fn new(points: &[[f64; 3]]) -> Option<BodyHull> {
        if points.len() < 4 || points.len() - 1 > i32::MAX as usize
            || points.iter().flatten().any(|v| !v.is_finite())
        {
            return None;
        }
        let flat: Vec<f64> = points.iter().flatten().copied().collect();
        let hull = convex_hull(&flat, points.len()).ok()?;
        if hull.vertices.len() < 4 || hull.faces.len() < 4 {
            return None;
        }
        let mut corners = Vec::with_capacity(hull.vertices.len());
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for &vertex in &hull.vertices {
            let p = *points.get(usize::try_from(vertex).ok()?)?;
            for axis in 0..3 {
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
            corners.push(p);
        }
        let mut faces = Vec::with_capacity(hull.faces.len());
        let mut edges = Vec::new();
        let mut planes: Vec<([f64; 3], f64)> = Vec::new();
        for face in &hull.faces {
            let mut indices = [0; 3];
            for i in 0..3 {
                indices[i] = u32::try_from(hull.vertices.binary_search(&face[i]).ok()?).ok()?;
            }
            faces.push(indices);
            for i in 0..3 {
                let (a, b) = (indices[i], indices[(i + 1) % 3]);
                edges.push([a.min(b), a.max(b)]);
            }
            let [a, b, c] = indices.map(|i| corners[i as usize]);
            let normal = cross(sub(b, a), sub(c, a));
            let length = normal[0].hypot(normal[1]).hypot(normal[2]);
            if !length.is_finite() {
                return None;
            }
            if length < EPS {
                continue;
            }
            let normal = normal.map(|v| v / length);
            let offset = dot(normal, a);
            if !offset.is_finite() {
                return None;
            }
            if !planes.iter().any(|(n, d)| {
                (0..3).all(|i| (n[i] - normal[i]).abs() <= 1e-9) && (d - offset).abs() <= EPS
            }) {
                planes.push((normal, offset));
            }
        }
        if planes.len() < 4 {
            return None;
        }
        edges.sort_unstable();
        edges.dedup();
        Some(Self { points: corners, faces, edges, planes, min, max })
    }

    /// `p` inside or on the surface (within EPS).
    pub fn contains(&self, p: [f64; 3]) -> bool {
        !self.planes.is_empty() && p.iter().all(|v| v.is_finite())
            && self.planes.iter().all(|&(normal, offset)| dot(normal, p) <= offset + EPS)
    }

    /// The triangle and the hull share a point (touching counts, within EPS): the triangle clipped by
    /// every face plane, each moved out by EPS, keeps something. A plane with the whole triangle beyond
    /// it rejects first, at 3 dot products a plane. Exact for a convex hull, and O(planes x the clipped
    /// polygon): no projection of the hull's points on separating axes, whose edge crosses cost
    /// O(edges x 3 x points) per triangle.
    pub fn overlaps(&self, t: &[[f64; 3]; 3]) -> bool {
        if self.planes.is_empty() || t.iter().flatten().any(|v| !v.is_finite()) {
            return false;
        }
        if self.planes.iter().any(|&(normal, offset)| t.iter().all(|&p| dot(normal, p) > offset + EPS)) {
            return false;
        }
        let (mut polygon, mut next) = (t.to_vec(), Vec::with_capacity(8));
        for &(normal, offset) in &self.planes {
            clip_plane_into(&polygon, normal, offset + EPS, &mut next);
            if next.is_empty() {
                return false;
            }
            std::mem::swap(&mut polygon, &mut next);
        }
        true
    }

    /// The part of the triangle inside the hull, as a convex polygon: the triangle clipped by every plane
    /// (`clip_plane`). Empty when they do not overlap, and possibly empty when they only touch.
    pub fn clip(&self, t: &[[f64; 3]; 3]) -> Vec<[f64; 3]> {
        if self.planes.is_empty() || t.iter().flatten().any(|v| !v.is_finite()) {
            return Vec::new();
        }
        let (mut polygon, mut next) = (t.to_vec(), Vec::with_capacity(8));
        for &(normal, offset) in &self.planes {
            clip_plane_into(&polygon, normal, offset, &mut next);
            std::mem::swap(&mut polygon, &mut next);
            if polygon.is_empty() {
                break;
            }
        }
        polygon
    }

    /// The closest points (on the hull, on the triangle) of a hull and a triangle that do not overlap,
    /// exact: a vertex of one against a face of the other, or an edge of each. Visit triangle vertices
    /// against hull faces, hull points against the triangle, then hull edges against triangle edges.
    /// Keep the first pair with the smallest squared distance, so equal distances are deterministic.
    pub fn closest(&self, t: &[[f64; 3]; 3]) -> ([f64; 3], [f64; 3]) {
        let mut best = (f64::INFINITY, [0.0; 3], [0.0; 3]);
        let mut offer = |p: [f64; 3], q: [f64; 3]| {
            let delta = sub(q, p);
            let distance = dot(delta, delta);
            if distance < best.0 {
                best = (distance, p, q);
            }
        };
        for &p in t {
            for &[a, b, c] in &self.faces {
                let (Some(&a), Some(&b), Some(&c)) = (
                    self.points.get(a as usize), self.points.get(b as usize), self.points.get(c as usize),
                ) else { continue };
                offer(closest_on_triangle(p, &[a, b, c]), p);
            }
        }
        for &p in &self.points {
            offer(p, closest_on_triangle(p, t));
        }
        for &[a, b] in &self.edges {
            let (Some(&a), Some(&b)) = (self.points.get(a as usize), self.points.get(b as usize)) else { continue };
            for i in 0..3 {
                let (p, q) = closest_on_segments(a, b, t[i], t[(i + 1) % 3]);
                offer(p, q);
            }
        }
        (best.1, best.2)
    }

    /// For each unit direction in `dirs`, how far the hull must move along it to leave the convex
    /// `polygon` (its points in order, in a plane with normal `normal`; a contact's part inside the
    /// hull), exact: where the ray t·u leaves the Minkowski difference polygon - hull. That is the
    /// smallest h(m) / (m·u) over the difference's face normals m with m·u > 0: the hull's faces turned
    /// inward, the polygon's two sides, and each hull edge crossed with each polygon edge (both signs),
    /// where h(m) is the polygon's high end along m plus the hull's high end along -m. 0 for a touch.
    /// Cost: O(planes × k + edges × k × points) for a k-point polygon, shared by all directions.
    pub fn exits(&self, polygon: &[[f64; 3]], normal: [f64; 3], dirs: &[[f64; 3]]) -> Vec<f64> {
        let mut best = vec![f64::INFINITY; dirs.len()];
        if polygon.is_empty() || self.points.is_empty() {
            return vec![0.0; dirs.len()];
        }
        let range = |points: &[[f64; 3]], m: [f64; 3]| {
            points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &p| {
                let v = dot(m, p);
                (lo.min(v), hi.max(v))
            })
        };
        let mut offer = |m: [f64; 3], h: f64| {
            let length = dot(m, m).sqrt();
            for (best, &u) in best.iter_mut().zip(dirs) {
                let along = dot(m, u);
                if along > EPS * length {
                    *best = best.min(h / along);
                }
            }
        };
        // A face (n, d) turned inward: m = -n, the hull's high end along n is d.
        for &(n, d) in &self.planes {
            let (lo, _) = range(polygon, n);
            offer(n.map(|v| -v), d - lo);
        }
        let mut both = |m: [f64; 3]| {
            let (lo, hi) = range(polygon, m);
            let (hull_lo, hull_hi) = range(&self.points, m);
            offer(m, hi - hull_lo);
            offer(m.map(|v| -v), hull_hi - lo);
        };
        if dot(normal, normal).sqrt() > EPS {
            both(normal);
        }
        for i in 0..polygon.len() {
            let edge = sub(polygon[(i + 1) % polygon.len()], polygon[i]);
            if dot(edge, edge).sqrt() < EPS {
                continue;
            }
            for &[a, b] in &self.edges {
                let (Some(&a), Some(&b)) = (self.points.get(a as usize), self.points.get(b as usize)) else { continue };
                let axis = cross(sub(b, a), edge);
                if dot(axis, axis).sqrt() < EPS {
                    continue;
                }
                both(axis);
            }
        }
        best.into_iter().map(|t| if t.is_finite() { t.max(0.0) } else { 0.0 }).collect()
    }

    /// The mean of the hull's points: a point strictly inside it.
    pub fn centre(&self) -> [f64; 3] {
        let weight = 1.0 / self.points.len() as f64;
        self.points.iter().fold([0.0; 3], |sum, &p| along(sum, p, weight))
    }
}
