//! A triangle mesh's distance at a point: the nearest triangle from a tree of boxes, the closest
//! point on it by its regions (Ericson, "Real-Time Collision Detection", 2005, 5.1.5), and the
//! sign from the angle-weighted normal of the face, edge or corner it lands on (Bærentzen and
//! Aanæs, "Signed distance computation using the angle weighted pseudonormal", IEEE TVCG 2005):
//! outside where the point lies on the normal's side.

use std::collections::HashMap;

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn unit(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l > 0.0 { scale(a, 1.0 / l) } else { [0.0; 3] }
}

/// Where on a triangle its closest point to a point lies.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Feature {
    Face,
    /// The edge from corner `k` to corner `(k + 1) % 3`.
    Edge(usize),
    Corner(usize),
}

/// The point of triangle `t` closest to `p`, and where it lies.
fn closest(p: V3, t: [V3; 3]) -> (V3, Feature) {
    let [a, b, c] = t;
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return (a, Feature::Corner(0));
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return (b, Feature::Corner(1));
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return (add(a, scale(ab, d1 / (d1 - d3))), Feature::Edge(0));
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return (c, Feature::Corner(2));
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return (add(a, scale(ac, d2 / (d2 - d6))), Feature::Edge(2));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return (add(b, scale(sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6)))), Feature::Edge(1));
    }
    let denom = 1.0 / (va + vb + vc);
    (add(a, add(scale(ab, vb * denom), scale(ac, vc * denom))), Feature::Face)
}

#[derive(Clone, Copy, Debug)]
struct Node {
    lo: V3,
    hi: V3,
    /// A leaf: triangles `order[start..start + count]`; else children `start` and `start + 1`.
    start: u32,
    count: u32,
}

/// A closed mesh's triangles in a tree of boxes, with the normals that sign a distance.
pub struct Triangles {
    verts: Vec<V3>,
    tris: Vec<[u32; 3]>,
    nodes: Vec<Node>,
    order: Vec<u32>,
    face: Vec<V3>,
    /// Per corner (welded by position) and per edge (by its welded corners): the angle-weighted
    /// normal of the faces around it.
    corner: Vec<V3>,
    edge: HashMap<(u32, u32), V3>,
    /// Each vertex's welded corner.
    weld: Vec<u32>,
}

const LEAF: usize = 4;

impl Triangles {
    pub fn new(verts: &[V3], tris: &[[u32; 3]]) -> Triangles {
        // corners at one place are one corner
        let mut at: HashMap<[u64; 3], u32> = HashMap::new();
        let weld: Vec<u32> = verts
            .iter()
            .map(|v| {
                let next = at.len() as u32;
                *at.entry(v.map(f64::to_bits)).or_insert(next)
            })
            .collect();
        let tris: Vec<[u32; 3]> = tris.iter().copied().filter(|t| weld[t[0] as usize] != weld[t[1] as usize] && weld[t[1] as usize] != weld[t[2] as usize] && weld[t[2] as usize] != weld[t[0] as usize]).collect();
        let face: Vec<V3> = tris.iter().map(|t| unit(cross(sub(verts[t[1] as usize], verts[t[0] as usize]), sub(verts[t[2] as usize], verts[t[0] as usize])))).collect();
        let mut corner = vec![[0.0; 3]; at.len()];
        let mut edge: HashMap<(u32, u32), V3> = HashMap::new();
        for (t, n) in tris.iter().zip(&face) {
            for k in 0..3 {
                let (a, b, c) = (verts[t[k] as usize], verts[t[(k + 1) % 3] as usize], verts[t[(k + 2) % 3] as usize]);
                let angle = dot(unit(sub(b, a)), unit(sub(c, a))).clamp(-1.0, 1.0).acos();
                let w = weld[t[k] as usize] as usize;
                corner[w] = add(corner[w], scale(*n, angle));
                let (e0, e1) = (weld[t[k] as usize], weld[t[(k + 1) % 3] as usize]);
                let e = edge.entry((e0.min(e1), e0.max(e1))).or_insert([0.0; 3]);
                *e = add(*e, *n);
            }
        }
        let mut m = Triangles { verts: verts.to_vec(), tris, nodes: Vec::new(), order: Vec::new(), face, corner, edge, weld };
        m.order = (0..m.tris.len() as u32).collect();
        if !m.tris.is_empty() {
            m.nodes.push(Node { lo: [0.0; 3], hi: [0.0; 3], start: 0, count: m.tris.len() as u32 });
            m.split(0);
        }
        m
    }

    fn bounds_of(&self, t: u32) -> (V3, V3) {
        let tri = self.tris[t as usize].map(|v| self.verts[v as usize]);
        let lo = std::array::from_fn(|k| tri[0][k].min(tri[1][k]).min(tri[2][k]));
        let hi = std::array::from_fn(|k| tri[0][k].max(tri[1][k]).max(tri[2][k]));
        (lo, hi)
    }

    /// Bounds node `i`, and splits it in two at the middle of its longest side while it holds
    /// more than a leaf's worth.
    fn split(&mut self, i: usize) {
        let (start, count) = (self.nodes[i].start as usize, self.nodes[i].count as usize);
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for &t in &self.order[start..start + count] {
            let (a, b) = self.bounds_of(t);
            for k in 0..3 {
                lo[k] = lo[k].min(a[k]);
                hi[k] = hi[k].max(b[k]);
            }
        }
        self.nodes[i].lo = lo;
        self.nodes[i].hi = hi;
        if count <= LEAF {
            return;
        }
        let axis = (0..3).max_by(|a, b| (hi[*a] - lo[*a]).total_cmp(&(hi[*b] - lo[*b]))).unwrap();
        let centre = |t: u32, m: &Triangles| {
            let (a, b) = m.bounds_of(t);
            a[axis] + b[axis]
        };
        let mut items: Vec<u32> = self.order[start..start + count].to_vec();
        items.sort_unstable_by(|a, b| centre(*a, self).total_cmp(&centre(*b, self)));
        self.order[start..start + count].copy_from_slice(&items);
        let half = count / 2;
        let first = self.nodes.len() as u32;
        self.nodes.push(Node { lo, hi, start: start as u32, count: half as u32 });
        self.nodes.push(Node { lo, hi, start: (start + half) as u32, count: (count - half) as u32 });
        self.nodes[i].start = first;
        self.nodes[i].count = 0;
        self.split(first as usize);
        self.split(first as usize + 1);
    }

    /// The signed distance at `p` (below 0 inside) and the direction from the nearest surface
    /// point to `p` (outward), or the surface's normal there when `p` lies on it.
    pub fn near(&self, p: V3) -> (f64, V3) {
        if self.nodes.is_empty() {
            return (f64::INFINITY, [0.0, 0.0, 1.0]);
        }
        let mut best = (f64::INFINITY, 0u32, [0.0; 3], Feature::Face);
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let n = self.nodes[i];
            let gap: f64 = (0..3).map(|k| (n.lo[k] - p[k]).max(p[k] - n.hi[k]).max(0.0).powi(2)).sum();
            if gap >= best.0 {
                continue;
            }
            if n.count > 0 {
                for &t in &self.order[n.start as usize..(n.start + n.count) as usize] {
                    let (q, f) = closest(p, self.tris[t as usize].map(|v| self.verts[v as usize]));
                    let d = dot(sub(p, q), sub(p, q));
                    if d < best.0 {
                        best = (d, t, q, f);
                    }
                }
            } else {
                // the nearer child first
                let (a, b) = (n.start as usize, n.start as usize + 1);
                let near = |c: usize| -> f64 { (0..3).map(|k| (self.nodes[c].lo[k] - p[k]).max(p[k] - self.nodes[c].hi[k]).max(0.0).powi(2)).sum() };
                if near(a) < near(b) {
                    stack.extend([b, a]);
                } else {
                    stack.extend([a, b]);
                }
            }
        }
        let (d2, t, q, f) = best;
        let tri = self.tris[t as usize];
        let normal = match f {
            Feature::Face => self.face[t as usize],
            Feature::Corner(k) => self.corner[self.weld[tri[k] as usize] as usize],
            Feature::Edge(k) => {
                let (a, b) = (self.weld[tri[k] as usize], self.weld[tri[(k + 1) % 3] as usize]);
                self.edge[&(a.min(b), a.max(b))]
            }
        };
        let away = sub(p, q);
        let d = d2.sqrt();
        let outward = dot(away, normal) >= 0.0;
        let dir = if d > 1e-12 { scale(away, if outward { 1.0 / d } else { -1.0 / d }) } else { unit(normal) };
        (if outward { d } else { -d }, dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit cube's distance: inside negative, outside positive, signed right over its faces,
    /// edges and corners, the direction out of it.
    #[test]
    fn cube_distance_is_signed() {
        let verts: Vec<V3> = (0..8).map(|i| [(i & 1) as f64, ((i >> 1) & 1) as f64, ((i >> 2) & 1) as f64]).collect();
        // two triangles a face, counter-clockwise from outside
        let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        let tris: Vec<[u32; 3]> = quads.iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect();
        let m = Triangles::new(&verts, &tris);
        let (d, n) = m.near([0.5, 0.5, 0.5]);
        assert!((d + 0.5).abs() < 1e-12, "{d}");
        assert!(n.iter().map(|v| v.abs()).sum::<f64>() > 0.99);
        let (d, n) = m.near([0.5, 0.5, -0.25]);
        assert!((d - 0.25).abs() < 1e-12 && (n[2] + 1.0).abs() < 1e-12, "{d} {n:?}");
        // past an edge and a corner
        let (d, _) = m.near([1.5, 0.5, 1.5]);
        assert!((d - 0.5f64.sqrt()).abs() < 1e-12);
        let (d, n) = m.near([-1.0, -1.0, -1.0]);
        assert!((d - 3f64.sqrt()).abs() < 1e-12 && n.iter().all(|v| (*v + 1.0 / 3f64.sqrt()).abs() < 1e-12));
        // just inside, near an edge
        let (d, _) = m.near([0.95, 0.5, 0.97]);
        assert!((d + 0.03).abs() < 1e-12, "{d}");
        // corners shared by index or only by place sign alike
        let soup: Vec<V3> = tris.iter().flat_map(|t| t.map(|v| verts[v as usize])).collect();
        let soup_tris: Vec<[u32; 3]> = (0..tris.len() as u32).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect();
        let s = Triangles::new(&soup, &soup_tris);
        for p in [[1.2, 1.1, 1.3], [0.99, 0.99, 0.99], [0.5, 1.0001, 0.5], [0.5, 0.5, 0.0]] {
            assert!((s.near(p).0 - m.near(p).0).abs() < 1e-12, "{p:?}");
        }
    }
}
