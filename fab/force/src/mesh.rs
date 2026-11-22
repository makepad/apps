//! A part given as triangles (a boundary model, an imported mesh) as the samples the grid
//! takes of a distance field: whether each sample is inside, by the parity of a ray's
//! crossings along each of the lattice's three axes with two of the three deciding, so a stray
//! gap in the mesh flips no whole column (Nooruddin and Turk, "Simplification and repair of
//! polygonal models using volumetric techniques", IEEE TVCG 2003); and how far it is from the
//! nearest triangle, signed by that: an AABB tree of the triangles, each sample's search
//! started from the triangle nearest the sample before it (neighbours share it mostly), and
//! the closest point on a triangle by its Voronoi regions (Ericson, "Real-Time Collision
//! Detection", 2005, 5.1.5).

use crate::element::V3;
use crate::grid::Lattice;

/// How deep inside distances are measured (mm): past any wall or skin (a few lines or layers
/// thick), so the material model sees every depth it tells apart; deeper samples read this deep.
pub const DEEP: f64 = 8.0;

/// How far outside, in lattice spacings: a sample this far out is in no cell's filled share,
/// and its neighbours' gradients still see the surface.
const OUT: f64 = 3.0;

/// A part's triangles, printer mm, in their bounding-box tree.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    /// Three per triangle.
    pub indices: Vec<u32>,
    tree: Tree,
}

impl Mesh {
    /// The triangles with two corners at one place left out: they bound nothing.
    pub fn new(positions: &[[f64; 3]], triangles: &[[u32; 3]]) -> Mesh {
        let positions: Vec<[f32; 3]> = positions.iter().map(|p| p.map(|v| v as f32)).collect();
        let at = |v: u32| positions[v as usize];
        let indices = triangles.iter().filter(|t| at(t[0]) != at(t[1]) && at(t[1]) != at(t[2]) && at(t[2]) != at(t[0])).flatten().copied().collect();
        let mut mesh = Mesh { positions, indices, tree: Tree::default() };
        mesh.tree = Tree::new(&mesh);
        mesh
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    /// The edges not closed off: used by the triangles on one side more often than on the
    /// other, corners matched by place. 0 for a closed, consistently wound surface; a mesh
    /// with holes still samples (the rays vote), but less surely near them.
    pub fn open_edges(&self) -> usize {
        let mut at = std::collections::HashMap::with_capacity(self.positions.len());
        let id: Vec<u32> = self.positions.iter().map(|p| {
            let n = at.len() as u32;
            *at.entry(p.map(f32::to_bits)).or_insert(n)
        }).collect();
        let mut uses: std::collections::HashMap<(u32, u32), i32> = std::collections::HashMap::with_capacity(self.indices.len());
        for t in self.indices.chunks_exact(3) {
            for e in 0..3 {
                let (a, b) = (id[t[e] as usize], id[t[(e + 1) % 3] as usize]);
                *uses.entry((a.min(b), a.max(b))).or_insert(0) += if a < b { 1 } else { -1 };
            }
        }
        uses.values().filter(|&&u| u != 0).count()
    }

    /// Writes the lattice's samples: minus the depth inside, the distance outside.
    pub fn sample(&self, lattice: &mut Lattice) {
        let inside = self.inside(lattice);
        let tree = &self.tree;
        let out = OUT * lattice.spacing;
        let mut near = 0;
        for k in 0..lattice.n[2] {
            for j in 0..lattice.n[1] {
                for i in 0..lattice.n[0] {
                    let s = lattice.index(i, j, k);
                    let reach = if inside[s] { DEEP } else { out };
                    let d = tree.nearest(self, lattice.pos(i, j, k), reach * reach, &mut near).sqrt();
                    lattice.d[s] = if inside[s] { -d } else { d } as f32;
                }
            }
        }
    }

    /// The surface's normal at points on it: their nearest triangle's (either sign).
    pub fn normals(&self, ps: &[V3], out: &mut [V3]) {
        if self.is_empty() {
            return out.fill([0.0; 3]);
        }
        let mut near = 0;
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            self.tree.nearest(self, *p, f64::INFINITY, &mut near);
            let [a, b, c] = self.triangle(near);
            let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
            let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            *o = if l > 0.0 { n.map(|x| x / l) } else { [0.0; 3] };
        }
    }

    /// How far each point is from the nearest triangle.
    pub fn distances(&self, ps: &[V3], out: &mut [f64]) {
        let mut near = 0;
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = self.tree.nearest(self, *p, f64::INFINITY, &mut near).sqrt();
        }
    }

    /// Each point moved to the nearest place on the triangles.
    pub fn onto(&self, ps: &mut [V3]) {
        let mut near = 0;
        for p in ps.iter_mut() {
            self.tree.nearest(self, *p, f64::INFINITY, &mut near);
            *p = closest_on_triangle(*p, self.triangle(near));
        }
    }

    fn triangle(&self, t: u32) -> [V3; 3] {
        let t = t as usize * 3;
        std::array::from_fn(|q| self.positions[self.indices[t + q] as usize].map(|v| v as f64))
    }

    /// Each sample inside or not: two of the three axes' ray parities agree.
    fn inside(&self, l: &Lattice) -> Vec<bool> {
        let mut votes = vec![0u8; l.d.len()];
        let mut hits: Vec<(u32, f64)> = Vec::new();
        for a in 0..3 {
            // columns along axis a, laid out over the other two (b fastest)
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            let (nb, nc) = (l.n[b], l.n[c]);
            let at = |axis: usize, i: usize| l.origin[axis] + i as f64 * l.spacing;
            let span = |axis: usize, lo: f64, hi: f64, n: usize| -> Option<(usize, usize)> {
                let i0 = ((lo - l.origin[axis]) / l.spacing).ceil().max(0.0);
                let i1 = ((hi - l.origin[axis]) / l.spacing).floor().min(n as f64 - 1.0);
                (i0 <= i1).then_some((i0 as usize, i1 as usize))
            };
            hits.clear();
            for t in self.indices.chunks_exact(3) {
                let v: [[f64; 3]; 3] = std::array::from_fn(|q| self.positions[t[q] as usize].map(|x| x as f64));
                let flat: [[f64; 2]; 3] = v.map(|p| [p[b], p[c]]);
                let (lo, hi) = flat.iter().fold(([f64::MAX; 2], [f64::MIN; 2]), |(lo, hi), p| ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]));
                let (Some((ib0, ib1)), Some((ic0, ic1))) = (span(b, lo[0], hi[0], nb), span(c, lo[1], hi[1], nc)) else { continue };
                for ic in ic0..=ic1 {
                    for ib in ib0..=ib1 {
                        if let Some(w) = covers(flat, [at(b, ib), at(c, ic)]) {
                            hits.push(((ib + nb * ic) as u32, w[0] * v[0][a] + w[1] * v[1][a] + w[2] * v[2][a]));
                        }
                    }
                }
            }
            hits.sort_unstable_by(|x, y| x.0.cmp(&y.0).then(x.1.total_cmp(&y.1)));
            let mut stride = [0usize; 3];
            stride[0] = 1;
            stride[1] = l.n[0];
            stride[2] = l.n[0] * l.n[1];
            for column in hits.chunk_by(|x, y| x.0 == y.0) {
                let col = column[0].0 as usize;
                let base = (col % nb) * stride[b] + (col / nb) * stride[c];
                let mut crossed = 0;
                for k in 0..l.n[a] {
                    let x = at(a, k);
                    while crossed < column.len() && column[crossed].1 < x {
                        crossed += 1;
                    }
                    if crossed % 2 == 1 {
                        votes[base + k * stride[a]] += 1;
                    }
                }
            }
        }
        votes.into_iter().map(|v| v >= 2).collect()
    }
}

/// Which side of the edge `p`-`r` the point `q` is on, seen from `p` to `r`: the same bits
/// for the two triangles sharing the edge (it is evaluated with its ends in one fixed order),
/// and never 0 for a point on it: `q` is taken an infinitesimal step off, the same step for
/// every edge (Edelsbrunner and Mücke, "Simulation of simplicity", ACM TOG 1990). So a ray
/// through a shared edge or vertex crosses exactly one of the triangles around it. 0 only for
/// an edge of no length.
fn side(p: [f64; 2], r: [f64; 2], q: [f64; 2]) -> i32 {
    let flip = r[0] < p[0] || (r[0] == p[0] && r[1] < p[1]);
    let (u, v) = if flip { (r, p) } else { (p, r) };
    let e = (v[0] - u[0]) * (q[1] - u[1]) - (v[1] - u[1]) * (q[0] - u[0]);
    let s = if e != 0.0 {
        if e > 0.0 { 1 } else { -1 }
    } else if v[1] != u[1] {
        // q + (eps, eps^2): the edge function moves by (v - u) x (eps, eps^2)
        if v[1] > u[1] { -1 } else { 1 }
    } else if v[0] != u[0] {
        if v[0] > u[0] { 1 } else { -1 }
    } else {
        0
    };
    if flip { -s } else { s }
}

/// The weights of `q` on the projected triangle when it covers `q` (exactly one of the
/// triangles round a shared edge or vertex does); a triangle seen edge on covers nothing.
fn covers(t: [[f64; 2]; 3], q: [f64; 2]) -> Option<[f64; 3]> {
    let s = [side(t[0], t[1], q), side(t[1], t[2], q), side(t[2], t[0], q)];
    if s[0] == 0 || s[0] != s[1] || s[1] != s[2] {
        return None;
    }
    let edge = |p: [f64; 2], r: [f64; 2]| ((r[0] - p[0]) * (q[1] - p[1]) - (r[1] - p[1]) * (q[0] - p[0])).abs();
    let w = [edge(t[1], t[2]), edge(t[2], t[0]), edge(t[0], t[1])];
    let total = w[0] + w[1] + w[2];
    Some(if total > 0.0 { w.map(|x| x / total) } else { [1.0 / 3.0; 3] })
}

/// A node of [`Tree`]: a leaf holds `order[first..first + count]`; an inner node (`count` 0)
/// has its children at `first` and `first + 1`.
#[derive(Clone, Copy, Debug, Default)]
struct Node {
    lo: [f32; 3],
    hi: [f32; 3],
    first: u32,
    count: u32,
}

/// Triangles in a bounding-box tree, split at the median of the longest axis of their centres.
#[derive(Clone, Debug, Default)]
struct Tree {
    nodes: Vec<Node>,
    order: Vec<u32>,
}

/// Triangles per leaf.
const LEAF: usize = 4;

impl Tree {
    fn new(mesh: &Mesh) -> Tree {
        let count = mesh.indices.len() / 3;
        let boxes: Vec<([f32; 3], [f32; 3])> = mesh
            .indices
            .chunks_exact(3)
            .map(|t| {
                let p: [[f32; 3]; 3] = std::array::from_fn(|q| mesh.positions[t[q] as usize]);
                (std::array::from_fn(|a| p[0][a].min(p[1][a]).min(p[2][a])), std::array::from_fn(|a| p[0][a].max(p[1][a]).max(p[2][a])))
            })
            .collect();
        let mut tree = Tree { nodes: Vec::with_capacity(2 * count / LEAF + 1), order: (0..count as u32).collect() };
        if count == 0 {
            return tree;
        }
        tree.nodes.push(Node { lo: [0.0; 3], hi: [0.0; 3], first: 0, count: 0 });
        let mut todo = vec![(0usize, 0usize, count)];
        while let Some((node, start, end)) = todo.pop() {
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            let (mut clo, mut chi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for &t in &tree.order[start..end] {
                let (blo, bhi) = boxes[t as usize];
                for a in 0..3 {
                    lo[a] = lo[a].min(blo[a]);
                    hi[a] = hi[a].max(bhi[a]);
                    let c = 0.5 * (blo[a] + bhi[a]);
                    clo[a] = clo[a].min(c);
                    chi[a] = chi[a].max(c);
                }
            }
            if end - start <= LEAF {
                tree.nodes[node] = Node { lo, hi, first: start as u32, count: (end - start) as u32 };
                continue;
            }
            let axis = (0..3).max_by(|&x, &y| (chi[x] - clo[x]).total_cmp(&(chi[y] - clo[y]))).unwrap();
            let mid = (start + end) / 2;
            let centre = |t: &u32| boxes[*t as usize].0[axis] + boxes[*t as usize].1[axis];
            tree.order[start..end].select_nth_unstable_by(mid - start, |x, y| centre(x).total_cmp(&centre(y)));
            let first = tree.nodes.len();
            tree.nodes.push(Node { lo: [0.0; 3], hi: [0.0; 3], first: 0, count: 0 });
            tree.nodes.push(Node { lo: [0.0; 3], hi: [0.0; 3], first: 0, count: 0 });
            tree.nodes[node] = Node { lo, hi, first: first as u32, count: 0 };
            todo.push((first, start, mid));
            todo.push((first + 1, mid, end));
        }
        tree
    }

    /// The squared distance from `p` to the nearest triangle, at most `reach2`; `near` is a
    /// triangle to try first (the last sample's nearest), and becomes this one's.
    fn nearest(&self, mesh: &Mesh, p: V3, reach2: f64, near: &mut u32) -> f64 {
        if self.nodes.is_empty() {
            return reach2;
        }
        let mut best = reach2.min(dist2(p, closest_on_triangle(p, mesh.triangle(*near))));
        let mut stack = [0u32; 64];
        let mut top = 1;
        while top > 0 {
            top -= 1;
            let node = self.nodes[stack[top] as usize];
            if box_dist2(p, &node) >= best {
                continue;
            }
            if node.count > 0 {
                for &t in &self.order[node.first as usize..(node.first + node.count) as usize] {
                    let d = dist2(p, closest_on_triangle(p, mesh.triangle(t)));
                    if d < best {
                        best = d;
                        *near = t;
                    }
                }
                continue;
            }
            // the nearer child is searched first (pushed last)
            let (l, r) = (node.first, node.first + 1);
            let (dl, dr) = (box_dist2(p, &self.nodes[l as usize]), box_dist2(p, &self.nodes[r as usize]));
            let (first, second) = if dl <= dr { (l, r) } else { (r, l) };
            stack[top] = second;
            stack[top + 1] = first;
            top += 2;
        }
        best
    }
}

fn dist2(p: V3, q: V3) -> f64 {
    (0..3).map(|a| (p[a] - q[a]) * (p[a] - q[a])).sum()
}

fn box_dist2(p: V3, n: &Node) -> f64 {
    (0..3).map(|a| {
        let d = (n.lo[a] as f64 - p[a]).max(p[a] - n.hi[a] as f64).max(0.0);
        d * d
    }).sum()
}

/// The point of triangle `t` nearest `p` (Ericson 2005, 5.1.5: which vertex, edge or the face
/// is nearest, by the dot products that bound their Voronoi regions).
fn closest_on_triangle(p: V3, [a, b, c]: [V3; 3]) -> V3 {
    let sub = |x: V3, y: V3| -> V3 { [x[0] - y[0], x[1] - y[1], x[2] - y[2]] };
    let dot = |x: V3, y: V3| x[0] * y[0] + x[1] * y[1] + x[2] * y[2];
    let along = |o: V3, d: V3, s: f64| -> V3 { [o[0] + s * d[0], o[1] + s * d[1], o[2] + s * d[2]] };
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return along(a, ab, d1 / (d1 - d3));
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return along(a, ac, d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        return along(b, sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let sum = va + vb + vc;
    if sum <= 0.0 {
        // a triangle of no area: its nearest corner
        return [a, b, c].into_iter().min_by(|x, y| dist2(p, *x).total_cmp(&dist2(p, *y))).unwrap();
    }
    along(along(a, ab, vb / sum), ac, vc / sum)
}
