//! The part's surface as the Force render mode shows and picks it, and the regions a click grows on
//! it: a face (normals that turn slowly and stay near the clicked one) or a hole (a cylinder:
//! normals square to one axis, at one distance from it).
//!
//! The surface is the distance lattice's dual contour (T. Ju, F. Losasso, S. Schaefer and
//! J. Warren, "Dual contouring of Hermite data", SIGGRAPH 2002): one vertex per lattice cube
//! the surface crosses, where the tangent planes at its crossings meet (their least-squares
//! point, solved about the crossings' mean with the planes' weak directions left out, as
//! P. Lindstrom, "Out-of-core simplification of large polygonal models", SIGGRAPH 2000), kept
//! in its cube; one quad per crossed lattice edge. The planes come from the part itself (its
//! field's gradient, a mesh's triangles), so a sharp edge or corner lands on its vertices at
//! every cell size and a face reaches its edges: a pick covers the same face, of the same
//! area, on a coarse model as on a fine one.

use crate::element::V3;
use crate::grid::Lattice;
use std::collections::HashMap;

/// A cube's tangent planes whose normals spread less than this (an eigenvalue of their sum
/// of n n^T against the largest) are one smooth surface there: a crease turning more than
/// about 26 degrees stays sharp, a curved wall (a few degrees a cube) stays round.
const SHARP: f64 = 0.05;

/// A face's normal turns less than this from one quad to the next: a sharper turn is an edge.
const CREASE_DEG: f64 = 30.0;

/// A6 fixes this turn in degrees by geometry, independently of any fit. F37 tests it.
const SHARP_EDGE_DEG: f64 = 45.0;

/// The faces a bolt clamps are square to its hole's axis within about 25 degrees.
const CLAMP_COS: f64 = 0.9;

/// A hole's wall turns less than this from one quad to the next (a small hole on a coarse
/// model turns about 26 degrees a quad); its rims turn a right angle.
const WALL_DEG: f64 = 45.0;

#[derive(Clone, Debug, Default)]
pub struct Surface {
    pub pos: Vec<[f32; 3]>,
    pub normal: Vec<[f32; 3]>,
    /// Counter-clockwise seen from outside.
    pub quads: Vec<[u32; 4]>,
    pub quad_normal: Vec<[f32; 3]>,
    pub quad_area: Vec<f32>,
    pub quad_center: Vec<[f32; 3]>,
    /// Per quad edge (vertices k, k+1): the quad across it, `u32::MAX` for none.
    pub across: Vec<[u32; 4]>,
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub(crate) fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}
pub(crate) fn norm(a: V3) -> V3 {
    let l = len(a);
    if l > 0.0 {
        a.map(|v| v / l)
    } else {
        [0.0, 0.0, 1.0]
    }
}
fn f64s(p: [f32; 3]) -> V3 {
    p.map(|v| v as f64)
}
fn f32s(p: V3) -> [f32; 3] {
    p.map(|v| v as f32)
}

/// The normals of a distance field at points on its surface: its gradient by four samples on
/// a tetrahedron `eps` across (each point's four in one batch of `distance`), unit length.
pub fn field_normals(distance: &mut dyn FnMut(&[V3], &mut [f64]), eps: f64) -> impl FnMut(&[V3], &mut [V3]) + '_ {
    const K: [V3; 4] = [[1.0, -1.0, -1.0], [-1.0, -1.0, 1.0], [-1.0, 1.0, -1.0], [1.0, 1.0, 1.0]];
    let (mut at, mut d) = (Vec::new(), Vec::new());
    move |ps: &[V3], out: &mut [V3]| {
        at.clear();
        at.extend(ps.iter().flat_map(|p| K.map(|k| std::array::from_fn(|a| p[a] + eps * k[a]))));
        d.resize(at.len(), 0.0);
        distance(&at, &mut d);
        for (i, o) in out.iter_mut().enumerate() {
            let g: V3 = std::array::from_fn(|a| (0..4).map(|t| K[t][a] * d[4 * i + t]).sum());
            *o = if len(g) > 0.0 { norm(g) } else { [0.0; 3] };
        }
    }
}

impl Surface {
    /// The dual contour of `l`; `normals` gives the part's surface normal at points on it
    /// (batched, either sign).
    pub fn contour(l: &Lattice, normals: &mut dyn FnMut(&[V3], &mut [V3])) -> Surface {
        let n = l.n;
        if n.iter().any(|&n| n < 2) {
            return Surface::default();
        }
        // the crossings: per lattice edge whose ends' signs differ, its low end and axis
        // (and whether the low end is the inside), where along it (linear in the distances)
        let stride = [1, n[0], n[0] * n[1]];
        let mut edges: Vec<(u32, u8)> = Vec::new();
        let mut at: Vec<V3> = Vec::new();
        for k in 0..n[2] {
            for j in 0..n[1] {
                for i in 0..n[0] {
                    let s = l.index(i, j, k);
                    let d0 = l.d[s];
                    for (a, room) in [i + 1 < n[0], j + 1 < n[1], k + 1 < n[2]].into_iter().enumerate() {
                        if !room {
                            continue;
                        }
                        let d1 = l.d[s + stride[a]];
                        if (d0 < 0.0) == (d1 < 0.0) {
                            continue;
                        }
                        let t = d0 as f64 / (d0 as f64 - d1 as f64);
                        let mut p = l.pos(i, j, k);
                        p[a] += t * l.spacing;
                        edges.push((s as u32, a as u8 | (u8::from(d0 < 0.0) << 2)));
                        at.push(p);
                    }
                }
            }
        }
        let mut planes = vec![[0.0; 3]; at.len()];
        normals(&at, &mut planes);
        // each crossing's plane into the four cubes round its edge: a cube's vertex where its
        // planes meet; one quad per crossing over those four vertices
        let cn = [n[0] - 1, n[1] - 1, n[2] - 1];
        let mut vert = vec![u32::MAX; cn[0] * cn[1] * cn[2]];
        let mut cubes: Vec<[usize; 3]> = Vec::new();
        let mut qef: Vec<Qef> = Vec::new();
        let mut s = Surface::default();
        for (x, &(e, bits)) in edges.iter().enumerate() {
            let a = (bits & 3) as usize;
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            let e = e as usize;
            let p = [e % n[0], (e / n[0]) % n[1], e / (n[0] * n[1])];
            // the four cubes share the edge: at p less 0 or 1 along b and c
            let mut ids = [u32::MAX; 4];
            for (slot, (ob, oc)) in [(1, 1), (0, 1), (0, 0), (1, 0)].into_iter().enumerate() {
                let (Some(xb), Some(xc)) = (p[b].checked_sub(ob), p[c].checked_sub(oc)) else { continue };
                let mut q = p;
                q[b] = xb;
                q[c] = xc;
                if q[0] >= cn[0] || q[1] >= cn[1] || q[2] >= cn[2] {
                    continue;
                }
                let cube = q[0] + cn[0] * (q[1] + cn[1] * q[2]);
                if vert[cube] == u32::MAX {
                    vert[cube] = cubes.len() as u32;
                    cubes.push(q);
                    qef.push(Qef::default());
                }
                let v = vert[cube];
                qef[v as usize].add(at[x], planes[x]);
                ids[slot] = v;
            }
            if ids.contains(&u32::MAX) {
                continue;
            }
            // inside at the low end: the outside is +a, keep the order
            if bits & 4 == 0 {
                ids.reverse();
            }
            s.quads.push(ids);
        }
        s.pos = cubes.iter().zip(&qef).map(|(q, f)| f32s(f.meet(l.pos(q[0], q[1], q[2]), l.pos(q[0] + 1, q[1] + 1, q[2] + 1)))).collect();
        s.finish();
        s
    }

    fn finish(&mut self) {
        let mut vn = vec![[0.0f64; 3]; self.pos.len()];
        let mut edges: HashMap<(u32, u32), Vec<(u32, u8)>> = HashMap::new();
        for (qi, q) in self.quads.iter().enumerate() {
            let p = q.map(|v| f64s(self.pos[v as usize]));
            let nrm = cross(sub(p[2], p[0]), sub(p[3], p[1]));
            let area = 0.5 * len(nrm);
            let n = norm(nrm);
            self.quad_normal.push(f32s(n));
            self.quad_area.push(area as f32);
            self.quad_center.push(f32s(std::array::from_fn(|a| 0.25 * (p[0][a] + p[1][a] + p[2][a] + p[3][a]))));
            for &v in q {
                for a in 0..3 {
                    vn[v as usize][a] += nrm[a];
                }
            }
            for e in 0..4 {
                let (a, b) = (q[e], q[(e + 1) % 4]);
                edges.entry((a.min(b), a.max(b))).or_default().push((qi as u32, e as u8));
            }
        }
        self.normal = vn.into_iter().map(|n| f32s(norm(n))).collect();
        self.across = vec![[u32::MAX; 4]; self.quads.len()];
        for list in edges.values() {
            for &(q, e) in list {
                if let Some(&(o, _)) = list.iter().find(|(o, _)| *o != q) {
                    self.across[q as usize][e as usize] = o;
                }
            }
        }
    }

    /// The midpoint of each sharp inside edge, in printer mm: concave, its two quads' normals
    /// turning by more than [`SHARP_EDGE_DEG`], and at least `min_len` long. A sharp corner
    /// line runs a cell long an edge; the contour's small quads where three faces meet (a
    /// fillet at a part's end face) can turn as much over a much shorter edge, and are a
    /// corner of the contour, not a line.
    pub fn sharp_edges(&self, min_len: f64) -> Vec<V3> {
        let turn = SHARP_EDGE_DEG.to_radians();
        let mut points = Vec::new();
        for (q, quad) in self.quads.iter().enumerate() {
            let n = f64s(self.quad_normal[q]);
            let c = f64s(self.quad_center[q]);
            for (k, &a) in self.across[q].iter().enumerate() {
                if a == u32::MAX || a as usize <= q {
                    continue;
                }
                let a = a as usize;
                if dot(n, sub(f64s(self.quad_center[a]), c)) <= 0.0 {
                    continue;
                }
                let other = f64s(self.quad_normal[a]);
                if len(cross(n, other)).atan2(dot(n, other)) > turn {
                    let p = f64s(self.pos[quad[k] as usize]);
                    let r = f64s(self.pos[quad[(k + 1) % 4] as usize]);
                    if len(sub(r, p)) < min_len {
                        continue;
                    }
                    points.push(std::array::from_fn(|i| 0.5 * (p[i] + r[i])));
                }
            }
        }
        points
    }

    /// The quad nearest a point.
    pub fn nearest(&self, p: V3) -> Option<u32> {
        (0..self.quads.len()).min_by(|&a, &b| {
            let da = len(sub(f64s(self.quad_center[a]), p));
            let db = len(sub(f64s(self.quad_center[b]), p));
            da.total_cmp(&db)
        }).map(|q| q as u32)
    }

    /// The region a click on quad `seed` picks.
    pub fn grow(&self, seed: u32, pick: Pick) -> Region {
        let n0 = f64s(self.quad_normal[seed as usize]);
        match pick {
            Pick::Face => {
                let crease = CREASE_DEG.to_radians().cos();
                let wide = 40f64.to_radians().cos();
                let faces = self.flood(seed, |_, from, to| dot(from, to) > crease && dot(n0, to) > wide, |_| false);
                self.region(faces, None)
            }
            Pick::Hole => {
                let wall = WALL_DEG.to_radians().cos();
                // the wall around the click, as far as its normals turn through about a right
                // angle (or the whole smooth patch): enough of it to find the axis at any cell
                // size
                let mut spread = [[0.0; 3]; 3];
                let mut taken = 0;
                let near = self.flood(seed, |_, from, to| dot(from, to) > wall, |faces| {
                    for &q in &faces[taken..] {
                        let n = f64s(self.quad_normal[q as usize]);
                        for a in 0..3 {
                            for b in 0..3 {
                                spread[a][b] += n[a] * n[b];
                            }
                        }
                    }
                    taken = faces.len();
                    let (l, _) = eigen3(spread);
                    faces.len() >= 8 && l[1] >= 0.15 * (l[0] + l[1] + l[2])
                });
                let Some(mut fit) = self.fit_cylinder(&near) else {
                    return self.region(vec![seed], None);
                };
                // the whole wall on that cylinder, then fitted to all of it and taken again
                let mut faces = vec![seed];
                for _ in 0..2 {
                    let (centre, axis, radius) = fit;
                    let tol = 0.25 * radius;
                    faces = self.flood(seed, |q, from, to| {
                        let r = sub(f64s(self.quad_center[q as usize]), centre);
                        let off = sub(r, axis.map(|a| a * dot(r, axis)));
                        dot(from, to) > wall && dot(to, axis).abs() < 0.35 && (len(off) - radius).abs() < tol
                    }, |_| false);
                    match self.fit_cylinder(&faces) {
                        Some(f) => fit = f,
                        None => break,
                    }
                }
                self.region(faces, Some(fit))
            }
        }
    }

    /// What a click on quad `seed` means when nobody says: the wall of a hole when a cylinder
    /// fits the surface round it, its normals go all the way round the axis and point at it
    /// (a concave wall, not a boss or a fillet), else a face. The region with it.
    pub fn reading(&self, seed: u32) -> (Pick, Region) {
        let wall = self.grow(seed, Pick::Hole);
        let hole = wall.cylinder.is_some_and(|(centre, axis, _)| self.round(&wall.faces, centre, axis) && self.inward(&wall.faces, centre, axis));
        match hole {
            true => (Pick::Hole, wall),
            false => (Pick::Face, self.grow(seed, Pick::Face)),
        }
    }

    /// Three quarters of the quads' outward normals point at the axis through `centre` (the
    /// solid is outside the cylinder: a hole).
    fn inward(&self, faces: &[u32], centre: V3, axis: V3) -> bool {
        let at = faces.iter().filter(|&&q| {
            let r = sub(f64s(self.quad_center[q as usize]), centre);
            let off = sub(r, axis.map(|a| a * dot(r, axis)));
            dot(f64s(self.quad_normal[q as usize]), off) < 0.0
        });
        4 * at.count() >= 3 * faces.len()
    }

    /// The quads a test keeps (`keep(centre, outward normal)`, printer mm): a region the
    /// design names. A wall round one axis (a feature hole's) has its cylinder fitted, for a
    /// torque's axis.
    pub fn select(&self, keep: impl Fn(V3, V3) -> bool) -> Region {
        let faces: Vec<u32> = (0..self.quads.len() as u32).filter(|&q| keep(f64s(self.quad_center[q as usize]), f64s(self.quad_normal[q as usize]))).collect();
        let mut spread = [[0.0; 3]; 3];
        for &q in &faces {
            let n = f64s(self.quad_normal[q as usize]);
            for a in 0..3 {
                for b in 0..3 {
                    spread[a][b] += n[a] * n[b];
                }
            }
        }
        let (l, _) = eigen3(spread);
        let sum = l[0] + l[1] + l[2];
        // normals square to one axis and spread round it: a wall
        let wall = sum > 0.0 && l[2] < 0.05 * sum && l[1] >= 0.15 * sum;
        let cylinder = if wall { self.fit_cylinder(&faces) } else { None };
        self.region(faces, cylinder)
    }

    /// The quads reached from `seed` across edges where `ok(quad, its normal from, to)`,
    /// nearest first; `done` (asked as the region doubles) ends it early.
    fn flood(&self, seed: u32, ok: impl Fn(u32, V3, V3) -> bool, mut done: impl FnMut(&[u32]) -> bool) -> Vec<u32> {
        let mut seen = vec![false; self.quads.len()];
        let mut out = vec![seed];
        seen[seed as usize] = true;
        let mut at = 0;
        let mut ask = 8;
        while at < out.len() {
            let q = out[at];
            at += 1;
            let from = f64s(self.quad_normal[q as usize]);
            for &o in &self.across[q as usize] {
                if o == u32::MAX || seen[o as usize] {
                    continue;
                }
                if ok(o, from, f64s(self.quad_normal[o as usize])) {
                    seen[o as usize] = true;
                    out.push(o);
                }
            }
            if out.len() >= ask {
                ask = 2 * out.len();
                if done(&out) {
                    break;
                }
            }
        }
        out
    }

    /// What a bolt through each hole of `region` clamps: a fixture on a hole holds the bolt's
    /// head and the face the part is bolted against, not the hole's wall (a rigid wall is a
    /// singular support: the stress at its rim grows without bound as the cells shrink). Per
    /// hole (a connected piece of the region round one axis, its wall all the way round), the
    /// faces square to its axis (within 25 degrees) out to the head's radius (twice the
    /// hole's) at its two ends (within a cell `h` of the wall's span along the axis); a piece
    /// that is no hole stays as it is. The region keeps the first hole's cylinder. None: no
    /// piece of `region` is a hole.
    pub fn clamp(&self, region: &Region, h: f64) -> Option<Region> {
        let mut faces = Vec::new();
        let mut first = None;
        for piece in self.pieces(&region.faces) {
            let Some((centre, axis, r)) = self.fit_cylinder(&piece).filter(|&(c, a, _)| self.round(&piece, c, a)) else {
                faces.extend(piece);
                continue;
            };
            first.get_or_insert((centre, axis, r));
            let along = |p: V3| dot(sub(p, centre), axis);
            let (lo, hi) = piece.iter().map(|&q| along(f64s(self.quad_center[q as usize]))).fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), t| (lo.min(t), hi.max(t)));
            let head = 2.0 * r + 0.25 * h;
            for q in 0..self.quads.len() as u32 {
                let n = f64s(self.quad_normal[q as usize]);
                if dot(n, axis).abs() < CLAMP_COS {
                    continue;
                }
                let p = sub(f64s(self.quad_center[q as usize]), centre);
                let t = dot(p, axis);
                if t < lo - h || t > hi + h {
                    continue;
                }
                if len(sub(p, axis.map(|a| a * t))) <= head {
                    faces.push(q);
                }
            }
        }
        let cylinder = first?;
        faces.sort_unstable();
        faces.dedup();
        Some(self.region(faces, Some(cylinder)))
    }

    /// The connected pieces of a set of quads (across their edges).
    fn pieces(&self, faces: &[u32]) -> Vec<Vec<u32>> {
        // 1: in the set, not yet in a piece
        let mut mark = vec![0u8; self.quads.len()];
        for &q in faces {
            mark[q as usize] = 1;
        }
        let mut out = Vec::new();
        for &seed in faces {
            if mark[seed as usize] != 1 {
                continue;
            }
            mark[seed as usize] = 2;
            let mut piece = vec![seed];
            let mut at = 0;
            while at < piece.len() {
                for &o in &self.across[piece[at] as usize] {
                    if o != u32::MAX && mark[o as usize] == 1 {
                        mark[o as usize] = 2;
                        piece.push(o);
                    }
                }
                at += 1;
            }
            out.push(piece);
        }
        out
    }

    /// The quads' normals go all the way round the axis through `centre` (a hole's wall, not
    /// a concave fillet's quarter turn): three quarters of twelve directions at least.
    fn round(&self, faces: &[u32], centre: V3, axis: V3) -> bool {
        let t = norm(cross(axis, if axis[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] }));
        let b = cross(axis, t);
        let mut seen = [false; 12];
        for &q in faces {
            let p = sub(f64s(self.quad_center[q as usize]), centre);
            let a = dot(p, b).atan2(dot(p, t));
            seen[(((a / std::f64::consts::TAU + 1.0).fract() * 12.0) as usize).min(11)] = true;
        }
        seen.iter().filter(|&&s| s).count() >= 9
    }

    /// A cylinder through quads' centres and normals: the axis is the direction their
    /// normals are all square to (the smallest eigenvector of sum n n^T); the centre and
    /// radius from `p + r n = c` in the plane across it, by least squares. Its centre sits at
    /// the quads' mean along the axis.
    fn fit_cylinder(&self, faces: &[u32]) -> Option<(V3, V3, f64)> {
        if faces.len() < 6 {
            return None;
        }
        let mut m = [[0.0; 3]; 3];
        for &q in faces {
            let n = f64s(self.quad_normal[q as usize]);
            for a in 0..3 {
                for b in 0..3 {
                    m[a][b] += n[a] * n[b];
                }
            }
        }
        let (l, v) = eigen3(m);
        // normals all along one direction (a flat face): no axis
        if l[1] < 1e-3 * l[0] {
            return None;
        }
        let axis = v[2];
        // a plane basis across the axis
        let t = norm(cross(axis, if axis[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] }));
        let b = cross(axis, t);
        // unknowns cx, cy, r: p_k + r n_k = c, for k in the plane's two axes
        let mut ata = [[0.0; 3]; 3];
        let mut atb = [0.0; 3];
        let mut along = 0.0;
        for &q in faces {
            let p = f64s(self.quad_center[q as usize]);
            let n = f64s(self.quad_normal[q as usize]);
            along += dot(p, axis);
            for (row, e) in [(0usize, t), (1, b)] {
                // c_row - r n.e = p.e
                let mut a = [0.0; 3];
                a[row] = 1.0;
                a[2] = -dot(n, e);
                let rhs = dot(p, e);
                for i in 0..3 {
                    for j in 0..3 {
                        ata[i][j] += a[i] * a[j];
                    }
                    atb[i] += a[i] * rhs;
                }
            }
        }
        let inv = crate::element::invert3(&ata)?;
        let x: V3 = std::array::from_fn(|i| (0..3).map(|j| inv[i][j] * atb[j]).sum());
        let r = x[2];
        if !(r > 0.05) {
            return None;
        }
        let along = along / faces.len() as f64;
        let centre = std::array::from_fn(|a| x[0] * t[a] + x[1] * b[a] + along * axis[a]);
        Some((centre, axis, r))
    }

    fn region(&self, faces: Vec<u32>, cylinder: Option<(V3, V3, f64)>) -> Region {
        let mut area = 0.0;
        let mut c = [0.0; 3];
        let mut n = [0.0; 3];
        for &q in &faces {
            let a = self.quad_area[q as usize] as f64;
            area += a;
            let p = f64s(self.quad_center[q as usize]);
            let qn = f64s(self.quad_normal[q as usize]);
            for k in 0..3 {
                c[k] += a * p[k];
                n[k] += a * qn[k];
            }
        }
        let centroid = if area > 0.0 { c.map(|v| v / area) } else { c };
        Region { faces, area, centroid, normal: norm(n), cylinder }
    }
}

/// How a click grows its region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Pick {
    #[default]
    Face,
    Hole,
}

/// A picked region of the surface.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Region {
    pub faces: Vec<u32>,
    pub area: f64,
    pub centroid: V3,
    /// The area-weighted mean outward normal.
    pub normal: V3,
    /// A hole's axis: a point on it, its direction, the radius.
    pub cylinder: Option<(V3, V3, f64)>,
}

/// Where along `o + t d` a ray crosses a triangle (Moller and Trumbore 1997), ahead of `o`.
pub(crate) fn ray_triangle(o: V3, d: V3, t: [V3; 3]) -> Option<f64> {
    let e1 = sub(t[1], t[0]);
    let e2 = sub(t[2], t[0]);
    let p = cross(d, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-12 {
        return None;
    }
    let s = sub(o, t[0]);
    let u = dot(s, p) / det;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(d, q) / det;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let tt = dot(e2, q) / det;
    (tt > 0.0).then_some(tt)
}

/// A cube's tangent planes (points on them and their normals), summed: where they meet is
/// their least-squares point about the points' mean, the planes' weak directions (relative
/// eigenvalue under [`SHARP`]) left at the mean.
#[derive(Clone, Copy, Debug, Default)]
struct Qef {
    /// sum n n^T, sum n (n . p), sum p, how many.
    ata: [[f64; 3]; 3],
    atb: V3,
    sum: V3,
    count: f64,
}

impl Qef {
    fn add(&mut self, p: V3, n: V3) {
        let r = dot(n, p);
        for a in 0..3 {
            for b in 0..3 {
                self.ata[a][b] += n[a] * n[b];
            }
            self.atb[a] += n[a] * r;
            self.sum[a] += p[a];
        }
        self.count += 1.0;
    }

    /// The planes' meeting point, kept inside the cube `lo..hi`.
    fn meet(&self, lo: V3, hi: V3) -> V3 {
        let mass = self.sum.map(|v| v / self.count);
        // about the mean: A (x - m) = b - A m
        let rhs: V3 = std::array::from_fn(|a| self.atb[a] - dot(self.ata[a], mass));
        let (l, v) = eigen3(self.ata);
        let mut p = mass;
        for i in 0..3 {
            if l[i] > SHARP * l[0] && l[i] > 0.0 {
                let w = dot(v[i], rhs) / l[i];
                for a in 0..3 {
                    p[a] += w * v[i][a];
                }
            }
        }
        std::array::from_fn(|a| p[a].clamp(lo[a], hi[a]))
    }
}

/// A symmetric 3x3 matrix's eigenvalues, largest first, and their unit eigenvectors (cyclic
/// Jacobi rotations).
pub fn eigen3(m: [[f64; 3]; 3]) -> ([f64; 3], [V3; 3]) {
    let mut a = m;
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let scale: f64 = (0..3).map(|i| (0..3).map(|j| m[i][j] * m[i][j]).sum::<f64>()).sum();
    for _ in 0..32 {
        let off = a[0][1] * a[0][1] + a[0][2] * a[0][2] + a[1][2] * a[1][2];
        if off <= 1e-30 * scale {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q] == 0.0 {
                continue;
            }
            // the rotation that zeroes a[p][q] (Numerical Recipes, 11.1)
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            for k in 0..3 {
                let (kp, kq) = (a[k][p], a[k][q]);
                a[k][p] = c * kp - s * kq;
                a[k][q] = s * kp + c * kq;
            }
            for k in 0..3 {
                let (pk, qk) = (a[p][k], a[q][k]);
                a[p][k] = c * pk - s * qk;
                a[q][k] = s * pk + c * qk;
            }
            for k in 0..3 {
                let (kp, kq) = (v[k][p], v[k][q]);
                v[k][p] = c * kp - s * kq;
                v[k][q] = s * kp + c * kq;
            }
        }
    }
    let mut order = [0, 1, 2];
    order.sort_by(|&x, &y| a[y][y].total_cmp(&a[x][x]));
    (order.map(|i| a[i][i]), order.map(|i| [v[0][i], v[1][i], v[2][i]]))
}
