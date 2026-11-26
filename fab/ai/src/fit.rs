//! Fit two model-frame meshes at picked seating faces. All result points are in a's model mm.
use makepad_csg_graph::TriMesh;
use makepad_csg_math::{BBox3d, Vec3d};
use makepad_csg_mesh::bvh::closest_on_tri;
use std::collections::HashMap;

const CONTACT: f64 = 1e-6; // mm
const MIN_VOLUME: f64 = 1e-6; // mm³
const TIE: f64 = 1e-9; // mm, equal nearest witnesses at a corner
const FACE_MM: f64 = 0.01;
const FACE_DEGREES: f64 = 1.0;

/// The surface clicked in the view, in this part's model mm. The name is the view's part label.
pub struct PickPlane<'a> {
    pub part: &'a str,
    pub at: Vec3d,
    pub normal: Vec3d,
}

#[derive(Clone, Copy, Debug)]
pub struct Gap {
    pub distance: f64,
    pub a: Vec3d,
    pub b: Vec3d,
}

#[derive(Debug)]
pub struct Fit {
    /// Rotate b about its model origin by this axis and angle, then translate.
    pub axis: Vec3d,
    pub degrees: f64,
    pub translation: Vec3d,
    pub overlap_mm3: f64,
    pub overlap_bounds: Option<BBox3d>,
    /// Directions of b - a, in order +u, -u, +v, -v.
    pub directions: [Vec3d; 4],
    pub gaps: [Option<Gap>; 4],
    pub side_gap: Option<f64>,
    pub seat: bool,
}

fn triangle(mesh: &TriMesh, i: usize) -> [Vec3d; 3] {
    mesh.triangles[i].map(|j| mesh.vertices[j as usize])
}

// Bvh::closest returns a single triangle. A lip corner can be equally near two walls in
// different directions. Visit all tied leaves, using the same closest_on_tri as clearance.
// Face contact also needs to visit triangles in a box, even when no vertex touches.
struct Node {
    bounds: BBox3d,
    children: Option<Box<[Node; 2]>>,
}

impl Node {
    fn build(order: &mut [usize], boxes: &[BBox3d]) -> Self {
        let bounds = order.iter().fold(BBox3d::empty(), |b, &i| b.union(boxes[i]));
        let children = (order.len() > 4).then(|| {
            let axis = bounds.size().dominant_axis();
            let mid = order.len() / 2;
            order.select_nth_unstable_by(mid, |&a, &b| {
                boxes[a].center().component(axis).total_cmp(&boxes[b].center().component(axis))
            });
            let (l, r) = order.split_at_mut(mid);
            Box::new([Self::build(l, boxes), Self::build(r, boxes)])
        });
        Self { bounds, children }
    }

    fn dist(&self, p: Vec3d) -> f64 {
        p.distance(p.max(self.bounds.min).min(self.bounds.max))
    }

    fn nearest(&self, mesh: &TriMesh, order: &[usize], p: Vec3d, best: &mut f64, hits: &mut Vec<(f64, Vec3d, usize)>, keep: &impl Fn(usize) -> bool) {
        if let Some(children) = &self.children {
            let (l, r) = order.split_at(order.len() / 2);
            let mut visits = [(children[0].dist(p), &children[0], l), (children[1].dist(p), &children[1], r)];
            if visits[1].0 < visits[0].0 { visits.swap(0, 1); }
            for (bound, node, indices) in visits {
                if bound <= *best + TIE { node.nearest(mesh, indices, p, best, hits, keep); }
            }
        } else {
            for &i in order {
                let [a, b, c] = triangle(mesh, i);
                let q = closest_on_tri(p, a, b, c);
                let d = p.distance(q);
                if d <= *best + TIE && keep(i) {
                    if d < *best - TIE { hits.clear(); }
                    *best = (*best).min(d);
                    hits.push((d, q, i));
                }
            }
        }
    }

    fn any_in(&self, order: &[usize], area: BBox3d, test: &impl Fn(usize) -> bool) -> bool {
        if !self.bounds.intersects(area) { return false; }
        if let Some(children) = &self.children {
            let (l, r) = order.split_at(order.len() / 2);
            children[0].any_in(l, area, test) || children[1].any_in(r, area, test)
        } else {
            order.iter().any(|&i| test(i))
        }
    }
}

struct Surface<'a> {
    mesh: &'a TriMesh,
    order: Vec<usize>,
    root: Node,
}

impl<'a> Surface<'a> {
    fn new(mesh: &'a TriMesh, part: &str) -> Result<Self, String> {
        if mesh.triangles.is_empty() { return Err(format!("part {part} is empty")); }
        let boxes: Vec<_> = (0..mesh.triangles.len()).map(|i| BBox3d::from_points(&triangle(mesh, i))).collect();
        let mut order: Vec<_> = (0..boxes.len()).collect();
        let root = Node::build(&mut order, &boxes);
        Ok(Self { mesh, order, root })
    }

    fn nearest(&self, p: Vec3d, hits: &mut Vec<(f64, Vec3d, usize)>) {
        let mut best = f64::INFINITY;
        hits.clear();
        self.root.nearest(self.mesh, &self.order, p, &mut best, hits, &|_| true);
        hits.retain(|(d, _, _)| *d <= best + TIE);
    }

    fn face(&self, at: Vec3d, part: &str, plane: Option<Vec3d>) -> Result<Face, String> {
        let mut hits = Vec::new();
        let cos = FACE_DEGREES.to_radians().cos();
        let in_plane = |tri: &[Vec3d; 3], p: Vec3d, n: Vec3d| tri.iter().all(|q| (*q - p).dot(n).abs() <= FACE_MM);
        if let Some(n) = plane {
            let n = n.normalize();
            // Either direction names the same clicked plane: the view faces the normal toward
            // the ray. The seed's own outward normal still controls the flood and the fit.
            let mut reach = FACE_MM;
            self.root.nearest(self.mesh, &self.order, at, &mut reach, &mut hits, &|i| {
                self.mesh.triangle_normal(i).dot(n).abs() >= cos && in_plane(&triangle(self.mesh, i), at, n)
            });
            hits.retain(|(d, _, _)| *d <= FACE_MM);
            if hits.is_empty() { return Err(format!("Measure Fit seats flat faces: the surface picked on {part} is curved")); }
        } else {
            self.nearest(at, &mut hits);
        }
        let &(d, origin, seed) = hits.iter().min_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)))
            .ok_or_else(|| format!("part {part} has no surface at the pick"))?;
        if d > FACE_MM { return Err(format!("{part}_at is {d:.3} mm from part {part}'s surface; pick a point on its seating face")); }
        let mesh = self.mesh;
        let normal = mesh.triangle_normal(seed);
        if normal == Vec3d::ZERO { return Err(format!("{part}_at picked a degenerate triangle; pick inside a face")); }
        let [u, v] = plane_axes(normal);
        // Use the same shared-position tolerance as measure::shells, across index seams.
        let key = |p: Vec3d| [p.x, p.y, p.z].map(|x| (x * 1e6).round() as i64);
        let mut incident = HashMap::<_, Vec<usize>>::new();
        for i in 0..mesh.triangles.len() {
            let tri = triangle(mesh, i);
            if mesh.triangle_normal(i).dot(normal) >= cos && in_plane(&tri, origin, normal) {
                for p in tri { incident.entry(key(p)).or_default().push(i); }
            }
        }
        let mut members = vec![false; mesh.triangles.len()];
        let mut stack = vec![seed];
        let mut bounds = BBox3d::empty();
        while let Some(i) = stack.pop() {
            if std::mem::replace(&mut members[i], true) { continue; }
            for p in triangle(mesh, i) {
                let offset = p - origin;
                bounds.expand(Vec3d::new(offset.dot(u), offset.dot(v), 0.0));
                if let Some(next) = incident.remove(&key(p)) { stack.extend(next); }
            }
        }
        let mid = bounds.center();
        Ok(Face { center: origin + u * mid.x + v * mid.y, normal, u, v, members })
    }
}

struct Face {
    center: Vec3d,
    normal: Vec3d,
    u: Vec3d,
    v: Vec3d,
    members: Vec<bool>,
}

fn plane_axes(normal: Vec3d) -> [Vec3d; 2] {
    let axes = [Vec3d::X, Vec3d::Y, Vec3d::Z];
    let mut axis = axes[0];
    for other in &axes[1..] {
        if other.dot(normal).abs() + 1e-12 < axis.dot(normal).abs() { axis = *other; }
    }
    let u = (axis - normal * normal.dot(axis)).normalize();
    let mut v = normal.cross(u);
    if v.component(v.dominant_axis()) < 0.0 { v = -v; }
    [u, v]
}

// The vector and scalar parts of a unit quaternion. Compose the twist after the shortest
// turn before extracting the single axis and angle reported to the caller.
struct Rotation { v: Vec3d, w: f64 }

impl Rotation {
    fn fit(a: &Face, b: &Face, turn: f64) -> Self {
        let cross = b.normal.cross(-a.normal);
        let dot = b.normal.dot(-a.normal);
        let sin = cross.length();
        let (v, w) = if sin > 1e-14 {
            let (s, c) = (0.5 * sin.atan2(dot)).sin_cos();
            (cross * (s / sin), c)
        } else if dot < 0.0 {
            (a.u, 0.0)
        } else {
            (Vec3d::ZERO, 1.0)
        };
        let (s, c) = (0.5 * (turn % 360.0).to_radians()).sin_cos();
        let twist = a.normal * s;
        let (v, w) = (v * c + twist * w + twist.cross(v), w * c - twist.dot(v));
        if w < 0.0 { Self { v: -v, w: -w } } else { Self { v, w } }
    }

    fn apply(&self, p: Vec3d) -> Vec3d { p + self.v.cross(self.v.cross(p) + p * self.w) * 2.0 }
    fn inverse(&self, p: Vec3d) -> Vec3d { p + self.v.cross(self.v.cross(p) - p * self.w) * 2.0 }
}

// Separating-axis test between one triangle swept by less than CONTACT along the face
// normal (a thin triangular prism) and the other triangle. Also catches edge crossings
// over a hollow rim where neither triangle contains a vertex of the other.
fn faces_touch(a: [Vec3d; 3], b: [Vec3d; 3], normal: Vec3d) -> bool {
    let origin = a[0];
    let a = a.map(|p| p - origin);
    let b = b.map(|p| p - origin);
    let edges = |p: [Vec3d; 3]| [p[1] - p[0], p[2] - p[1], p[0] - p[2]];
    let (ea, eb) = (edges(a), edges(b));
    let separates = |axis: Vec3d| {
        let interval = |p: [Vec3d; 3]| {
            let [x, y, z] = p.map(|q| q.dot(axis));
            (x.min(y).min(z), x.max(y).max(z))
        };
        let ((al, ah), (bl, bh)) = (interval(a), interval(b));
        let gap = (al - bh).max(bl - ah);
        gap > 0.0 && gap >= CONTACT * axis.dot(normal).abs()
    };
    !separates(ea[0].cross(ea[1])) && !separates(eb[0].cross(eb[1]))
        && !ea.iter().any(|e| separates(e.cross(normal)))
        && !eb.iter().any(|f| separates(normal.cross(*f)) || ea.iter().any(|e| separates(e.cross(*f))))
}

/// Measure b seated against a. Each pick is in the corresponding mesh's own model frame.
/// The caller checks part IDs: distinct instances can share the same mesh.
pub fn measure(a: &TriMesh, b: &TriMesh, a_at: Vec3d, b_at: Vec3d, turn: f64) -> Result<Fit, String> {
    measure_with(a, b, turn, |sa, sb| Ok((sa.face(a_at, "a", None)?, sb.face(b_at, "b", None)?)))
}

/// As [`measure`], with the seating faces identified by the planes clicked in the view.
pub fn measure_planes(a: &TriMesh, b: &TriMesh, a_pick: PickPlane<'_>, b_pick: PickPlane<'_>, turn: f64) -> Result<Fit, String> {
    measure_with(a, b, turn, |sa, sb| {
        let fa = sa.face(a_pick.at, a_pick.part, Some(a_pick.normal))?;
        let fb = sb.face(b_pick.at, b_pick.part, Some(b_pick.normal))?;
        Ok((fa, fb))
    })
}

fn measure_with(a: &TriMesh, b: &TriMesh, turn: f64, faces: impl FnOnce(&Surface<'_>, &Surface<'_>) -> Result<(Face, Face), String>) -> Result<Fit, String> {
    if !turn.is_finite() { return Err("turn must be a finite angle in degrees".into()); }
    let (sa, sb) = (Surface::new(a, "a")?, Surface::new(b, "b")?);
    let (fa, fb) = faces(&sa, &sb)?;
    let spin = Rotation::fit(&fa, &fb, turn);
    let translation = fa.center - spin.apply(fb.center);
    let pose = |p: Vec3d| fa.center + spin.apply(p - fb.center);
    let unpose = |p: Vec3d| fb.center + spin.inverse(p - fa.center);
    let mut posed = b.clone();
    posed.clear_normals();
    for p in &mut posed.vertices { *p = pose(*p); }

    let overlap = makepad_csg_cad::intersection(a, &posed);
    let volume = makepad_csg_cad::signed_volume(&overlap);
    if !volume.is_finite() || volume < -MIN_VOLUME { return Err("the mesh intersection has an invalid signed volume; check that both parts are closed and outward-facing".into()); }
    let overlap_bounds = (volume > MIN_VOLUME).then(|| overlap.bounding_box());
    let seat = overlap_bounds.is_none() && fb.members.iter().enumerate().any(|(i, &member)| {
        if !member { return false; }
        let tri = triangle(&posed, i);
        let bb = BBox3d::from_points(&tri);
        let pad = Vec3d::all(CONTACT);
        sa.root.any_in(&sa.order, BBox3d::from_min_max(bb.min - pad, bb.max + pad), &|j| {
            fa.members[j] && faces_touch(triangle(a, j), tri, fa.normal)
        })
    });

    let mut gaps = [None::<Gap>; 4];
    let mut record = |distance, a, b| {
        if distance < CONTACT { return; }
        let delta: Vec3d = b - a;
        if delta.dot(fa.normal).abs() > distance * std::f64::consts::FRAC_1_SQRT_2 { return; }
        let (u, v) = (delta.dot(fa.u), delta.dot(fa.v));
        let side = if u.abs() >= v.abs() { usize::from(u < 0.0) } else { 2 + usize::from(v < 0.0) };
        if gaps[side].map_or(true, |g| distance < g.distance) { gaps[side] = Some(Gap { distance, a, b }); }
    };
    let mut hits = Vec::new();
    for &p in &a.vertices {
        sb.nearest(unpose(p), &mut hits);
        for &(d, q, _) in &hits { record(d, p, pose(q)); }
    }
    for &p in &posed.vertices {
        sa.nearest(p, &mut hits);
        for &(d, q, _) in &hits { record(d, q, p); }
    }
    Ok(Fit {
        axis: if spin.v == Vec3d::ZERO { fa.u } else { spin.v.normalize() },
        degrees: 2.0 * spin.v.length().atan2(spin.w).to_degrees(),
        translation, overlap_mm3: volume.max(0.0), overlap_bounds,
        directions: [fa.u, -fa.u, fa.v, -fa.v],
        side_gap: gaps.iter().flatten().map(|g| g.distance).reduce(f64::min), gaps, seat,
    })
}
