//! A bounding volume hierarchy for casting rays through a triangle list, shared by the studio light models
//! (light_model.rs) and the candle (light_model_candle.rs).

use makepad_draw::*;

/// One box of the hierarchy: a leaf holds `count` triangles of `order` from `first`; an inner box (count 0) has its
/// children at `first` and `first + 1`.
#[derive(Clone, Copy)]
struct Node {
    min: Vec3f,
    max: Vec3f,
    first: u32,
    count: u32,
}

/// The most triangles a leaf holds.
const LEAF_TRIANGLES: usize = 4;

/// Where a ray met a triangle of a `TriBvh`: how far along the ray (in the ray's own units), which triangle (its
/// index in the list the hierarchy was built from), the barycentric u and v there (toward its second and third
/// corners) and its geometric normal (unit, as its corners wind: (p1 - p0) x (p2 - p0), either side of the ray).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BvhHit {
    pub t: f32,
    pub tri: usize,
    pub u: f32,
    pub v: f32,
    pub face: Vec3f,
}

/// A bounding volume hierarchy for ray casts against a list of triangles.
#[derive(Clone)]
pub struct TriBvh {
    /// The triangles in the hierarchy's leaf order, and their corners in that order, side by side.
    order: Vec<u32>,
    corners: Vec<[Vec3f; 3]>,
    nodes: Vec<Node>,
}

impl TriBvh {
    /// The hierarchy over `corners`, one triangle each: the triangles split at the median of their centres along the widest axis, down to leaves
    /// of LEAF_TRIANGLES.
    pub fn new(corners: Vec<[Vec3f; 3]>) -> TriBvh {
        let nt = corners.len();
        let centres: Vec<Vec3f> = corners.iter().map(|c| (c[0] + c[1] + c[2]) * (1.0 / 3.0)).collect();
        let mut order: Vec<u32> = (0..nt as u32).collect();
        let mut nodes = Vec::with_capacity(2 * nt / LEAF_TRIANGLES + 1);
        nodes.push(Node { min: Vec3f::default(), max: Vec3f::default(), first: 0, count: 0 });
        let mut stack: Vec<(usize, usize, usize)> = vec![(0, 0, nt)];
        while let Some((node, from, to)) = stack.pop() {
            let (mut min, mut max) = (Vec3f::all(f32::INFINITY), Vec3f::all(f32::NEG_INFINITY));
            for &t in &order[from..to] {
                for &p in &corners[t as usize] {
                    min = Vec3f::min_componentwise(min, p);
                    max = Vec3f::max_componentwise(max, p);
                }
            }
            nodes[node].min = min;
            nodes[node].max = max;
            if to - from <= LEAF_TRIANGLES {
                nodes[node].first = from as u32;
                nodes[node].count = (to - from) as u32;
                continue;
            }
            let (mut cmin, mut cmax) = (Vec3f::all(f32::INFINITY), Vec3f::all(f32::NEG_INFINITY));
            for &t in &order[from..to] {
                cmin = Vec3f::min_componentwise(cmin, centres[t as usize]);
                cmax = Vec3f::max_componentwise(cmax, centres[t as usize]);
            }
            let span = cmax - cmin;
            let axis = if span.x >= span.y && span.x >= span.z { 0 } else if span.y >= span.z { 1 } else { 2 };
            let key = |t: u32| -> f32 {
                let c = centres[t as usize];
                match axis {
                    0 => c.x,
                    1 => c.y,
                    _ => c.z,
                }
            };
            let mid = from + (to - from) / 2;
            order[from..to].select_nth_unstable_by(mid - from, |&a, &b| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal));
            let first = nodes.len();
            nodes[node].first = first as u32;
            nodes[node].count = 0;
            nodes.push(Node { min: Vec3f::default(), max: Vec3f::default(), first: 0, count: 0 });
            nodes.push(Node { min: Vec3f::default(), max: Vec3f::default(), first: 0, count: 0 });
            stack.push((first, from, mid));
            stack.push((first + 1, mid, to));
        }
        let corners = order.iter().map(|&t| corners[t as usize]).collect();
        TriBvh { order, corners, nodes }
    }

    /// How many triangles it holds.
    pub fn triangles(&self) -> usize {
        self.order.len()
    }

    /// Every triangle's corners, in the hierarchy's leaf order.
    pub fn corners(&self) -> &[[Vec3f; 3]] {
        &self.corners
    }


    /// Where the ray from `ro` along `rd` first meets a triangle. None when it misses. `rd` need not be unit: `t`
    /// is in its units.
    pub fn hit(&self, ro: Vec3f, rd: Vec3f) -> Option<BvhHit> {
        if self.nodes.is_empty() {
            return None;
        }
        let inv = vec3f(1.0 / rd.x, 1.0 / rd.y, 1.0 / rd.z);
        let mut best = f32::INFINITY;
        let mut found: Option<(usize, f32, f32)> = None;
        let mut stack: [u32; 64] = [0; 64];
        let mut depth = 1;
        while depth > 0 {
            depth -= 1;
            let i = stack[depth] as usize;
            let node = self.nodes[i];
            if box_entry(node.min, node.max, ro, inv) >= best {
                continue;
            }
            if node.count == 0 {
                let (a, b) = (node.first as usize, node.first as usize + 1);
                let (ta, tb) = (box_entry(self.nodes[a].min, self.nodes[a].max, ro, inv), box_entry(self.nodes[b].min, self.nodes[b].max, ro, inv));
                // the nearer child last, so it is searched first
                let (near, far, t_far) = if ta <= tb { (a, b, tb) } else { (b, a, ta) };
                if t_far < best && depth < 63 {
                    stack[depth] = far as u32;
                    depth += 1;
                }
                if depth < 63 {
                    stack[depth] = near as u32;
                    depth += 1;
                }
                continue;
            }
            for k in node.first as usize..(node.first + node.count) as usize {
                let [p0, p1, p2] = self.corners[k];
                if let Some((t, u, v)) = ray_triangle(ro, rd, p0, p1, p2) {
                    if t < best {
                        best = t;
                        found = Some((k, u, v));
                    }
                }
            }
        }
        let (k, u, v) = found?;
        let [p0, p1, p2] = self.corners[k];
        Some(BvhHit { t: best, tri: self.order[k] as usize, u, v, face: Vec3f::cross(p1 - p0, p2 - p0).normalize() })
    }
}


impl TriBvh {
    /// Every triangle the ray from `ro` along `rd` meets before `t_max` (in `rd`'s units), in no particular order,
    /// handed to `each` (its index in the list the hierarchy was built from, and the hit's geometric normal), until
    /// `each` answers false: one walk for a shadow ray that passes glass and stops at the first solid.
    pub fn each_hit(&self, ro: Vec3f, rd: Vec3f, t_max: f32, mut each: impl FnMut(usize, Vec3f) -> bool) {
        if self.nodes.is_empty() {
            return;
        }
        let inv = vec3f(1.0 / rd.x, 1.0 / rd.y, 1.0 / rd.z);
        let mut stack: [u32; 64] = [0; 64];
        let mut depth = 1;
        while depth > 0 {
            depth -= 1;
            let node = self.nodes[stack[depth] as usize];
            if box_entry(node.min, node.max, ro, inv) >= t_max {
                continue;
            }
            if node.count == 0 {
                if depth < 62 {
                    stack[depth] = node.first;
                    stack[depth + 1] = node.first + 1;
                    depth += 2;
                }
                continue;
            }
            for k in node.first as usize..(node.first + node.count) as usize {
                let [p0, p1, p2] = self.corners[k];
                if let Some((t, _, _)) = ray_triangle(ro, rd, p0, p1, p2) {
                    if t < t_max && !each(self.order[k] as usize, Vec3f::cross(p1 - p0, p2 - p0).normalize()) {
                        return;
                    }
                }
            }
        }
    }
}

/// Where the ray enters the box (the slab test), f32::INFINITY when it misses it; 0 from inside.
fn box_entry(min: Vec3f, max: Vec3f, ro: Vec3f, inv: Vec3f) -> f32 {
    let (tx0, tx1) = ((min.x - ro.x) * inv.x, (max.x - ro.x) * inv.x);
    let (ty0, ty1) = ((min.y - ro.y) * inv.y, (max.y - ro.y) * inv.y);
    let (tz0, tz1) = ((min.z - ro.z) * inv.z, (max.z - ro.z) * inv.z);
    let near = tx0.min(tx1).max(ty0.min(ty1)).max(tz0.min(tz1)).max(0.0);
    let far = tx0.max(tx1).min(ty0.max(ty1)).min(tz0.max(tz1));
    // (NaN from a zero direction across a slab's face compares false: a miss)
    if near <= far { near } else { f32::INFINITY }
}

/// Möller–Trumbore: how far along the ray the triangle is met, and the barycentric u and v there. Both sides.
fn ray_triangle(ro: Vec3f, rd: Vec3f, p0: Vec3f, p1: Vec3f, p2: Vec3f) -> Option<(f32, f32, f32)> {
    let e1 = p1 - p0;
    let e2 = p2 - p0;
    let p = Vec3f::cross(rd, e2);
    let det = e1.dot(p);
    if det.abs() < 1.0e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = ro - p0;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = Vec3f::cross(s, e1);
    let v = rd.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    if t > 1.0e-6 { Some((t, u, v)) } else { None }
}
