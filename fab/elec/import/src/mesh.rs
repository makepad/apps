//! The part mesh: triangles grouped by material, with per-vertex normals, and the builder every
//! reader feeds with polygons.
//!
//! Units and axes are the caller's: a reader's mesh is in its file's coordinates; the converter
//! places it in the part's body frame (millimetres, Z up from the board surface, X and Y as the
//! footprint's) with [`Mesh::transform`] before it is written as `.glb`.
//!
//! Cost: [`MeshBuilder::polygon`] is linear in a convex polygon's corners and quadratic in a concave
//! one's; [`MeshBuilder::finish`] is linear in corners plus, per position, the square of the faces
//! around it (smooth cones and fans are the large case: a 1000-face apex is a million dot products).

use std::collections::HashMap;

/// Decodes an sRGB display value to linear light. Values outside 0..=1 are returned unchanged;
/// readers leave those for the builder to clamp, and standalone MTL factors remain unclamped.
pub fn srgb_to_linear(v: f32) -> f32 {
    if !(0.0..=1.0).contains(&v) {
        v
    } else if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Encodes linear light as an sRGB display value, the inverse of [`srgb_to_linear`].
/// Values outside 0..=1 are returned unchanged.
pub fn linear_to_srgb(v: f32) -> f32 {
    if !(0.0..=1.0).contains(&v) {
        v
    } else if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// A surface's look: glTF's metal-rough model, the factors only (no textures).
#[derive(Clone, PartialEq, Debug)]
pub struct Material {
    pub name: String,
    /// Linear RGBA, each in 0..=1; readers decode display-referred file colours to linear RGB.
    /// Alpha under 1 is see-through.
    pub base_color: [f32; 4],
    /// The file's Phong specular colour, decoded to linear RGB like the base colour, if stated.
    /// Source data only: generic readers never turn it into metalness.
    pub specular: Option<[f32; 3]>,
    /// 0 a dielectric (plastic, ceramic), 1 a metal.
    pub metallic: f32,
    /// 0 a mirror, 1 fully diffuse.
    pub roughness: f32,
    /// Linear RGB light the surface gives off, each in 0..=1.
    pub emissive: [f32; 3],
}

impl Material {
    /// A neutral grey dielectric: what a face gets when its file names no material, and what a
    /// reader starts a named material from before its statements set the factors.
    pub fn named(name: impl Into<String>) -> Self {
        Material { name: name.into(), base_color: [0.8, 0.8, 0.8, 1.0], specular: None,
            metallic: 0.0, roughness: 0.5, emissive: [0.0; 3] }
    }

    /// Every glTF factor in 0..=1; a non-finite one takes [`Material::named`]'s value.
    /// Phong specular source data is kept unchanged.
    pub fn clamped(self) -> Self {
        let d = Material::named("");
        let c = |v: f32, d: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { d };
        Material {
            name: self.name,
            base_color: [0, 1, 2, 3].map(|i| c(self.base_color[i], d.base_color[i])),
            specular: self.specular,
            metallic: c(self.metallic, d.metallic),
            roughness: c(self.roughness, d.roughness),
            emissive: [0, 1, 2].map(|i| c(self.emissive[i], d.emissive[i])),
        }
    }
}

/// One material's triangles: `indices` (three per triangle, counter-clockwise seen from the front)
/// into `positions` and `normals`, which have one entry per vertex.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Prim {
    /// Index into [`Mesh::materials`].
    pub material: usize,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

/// A body mesh: its materials and one or more primitives per material.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Mesh {
    pub materials: Vec<Material>,
    pub prims: Vec<Prim>,
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.prims.iter().map(|p| p.indices.len() / 3).sum()
    }

    pub fn vertex_count(&self) -> usize {
        self.prims.iter().map(|p| p.positions.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.triangle_count() == 0
    }

    /// Every vertex position (a convex hull or a bounding box needs nothing else).
    pub fn points(&self) -> impl Iterator<Item = [f32; 3]> + '_ {
        self.prims.iter().flat_map(|p| p.positions.iter().copied())
    }

    /// The axis-aligned box of every vertex; `None` with no vertices.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut it = self.points();
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), p| {
            ([0, 1, 2].map(|i| lo[i].min(p[i])), [0, 1, 2].map(|i| hi[i].max(p[i])))
        }))
    }

    /// Maps every position by `xf` and every normal by its inverse transpose. A mirroring map
    /// (negative determinant) reverses each triangle's corners, so the fronts stay counter-clockwise.
    pub fn transform(&mut self, xf: &Affine) {
        for prim in &mut self.prims {
            prim.transform(xf);
        }
    }
}

impl Prim {
    /// [`Mesh::transform`] for one primitive.
    pub fn transform(&mut self, xf: &Affine) {
        for p in &mut self.positions {
            *p = xf.point(*p);
        }
        for n in &mut self.normals {
            *n = xf.normal(*n);
        }
        if xf.det() < 0.0 {
            for tri in self.indices.chunks_exact_mut(3) {
                tri.swap(1, 2);
            }
        }
    }
}

/// An affine map in f64, row-major 3x4: `p'[r] = m[r][0] p.x + m[r][1] p.y + m[r][2] p.z + m[r][3]`.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Affine {
    pub m: [[f64; 4]; 3],
}

impl Affine {
    pub const IDENTITY: Affine = Affine { m: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]] };

    pub fn translate(t: [f64; 3]) -> Self {
        let mut a = Affine::IDENTITY;
        for r in 0..3 {
            a.m[r][3] = t[r];
        }
        a
    }

    pub fn scale(s: [f64; 3]) -> Self {
        let mut a = Affine::IDENTITY;
        for r in 0..3 {
            a.m[r][r] = s[r];
        }
        a
    }

    /// A right-handed rotation by `angle` radians about `axis` (any length; a zero or non-finite
    /// axis gives the identity).
    pub fn rotate(axis: [f64; 3], angle: f64) -> Self {
        let len = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        if !(len > 0.0) || !len.is_finite() || !angle.is_finite() {
            return Affine::IDENTITY;
        }
        let [x, y, z] = axis.map(|v| v / len);
        let (s, c) = angle.sin_cos();
        let t = 1.0 - c;
        Affine {
            m: [
                [t * x * x + c, t * x * y - s * z, t * x * z + s * y, 0.0],
                [t * x * y + s * z, t * y * y + c, t * y * z - s * x, 0.0],
                [t * x * z - s * y, t * y * z + s * x, t * z * z + c, 0.0],
            ],
        }
    }

    /// The map that applies `first`, then `self` (the matrix product `self · first`).
    pub fn mul(&self, first: &Affine) -> Affine {
        let (a, b) = (&self.m, &first.m);
        let mut m = [[0.0; 4]; 3];
        for r in 0..3 {
            for c in 0..4 {
                m[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c] + if c == 3 { a[r][3] } else { 0.0 };
            }
        }
        Affine { m }
    }

    pub fn point(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.m;
        let (x, y, z) = (p[0] as f64, p[1] as f64, p[2] as f64);
        [0, 1, 2].map(|r| (m[r][0] * x + m[r][1] * y + m[r][2] * z + m[r][3]) as f32)
    }

    /// A normal mapped by the inverse transpose and normalised; a map that collapses it keeps `n`.
    pub fn normal(&self, n: [f32; 3]) -> [f32; 3] {
        let m = &self.m;
        // The cofactor matrix is det · inverse transpose; the sign of det keeps the side.
        let cof = [
            [m[1][1] * m[2][2] - m[1][2] * m[2][1], m[1][2] * m[2][0] - m[1][0] * m[2][2], m[1][0] * m[2][1] - m[1][1] * m[2][0]],
            [m[0][2] * m[2][1] - m[0][1] * m[2][2], m[0][0] * m[2][2] - m[0][2] * m[2][0], m[0][1] * m[2][0] - m[0][0] * m[2][1]],
            [m[0][1] * m[1][2] - m[0][2] * m[1][1], m[0][2] * m[1][0] - m[0][0] * m[1][2], m[0][0] * m[1][1] - m[0][1] * m[1][0]],
        ];
        let sign = if self.det() < 0.0 { -1.0 } else { 1.0 };
        let (x, y, z) = (n[0] as f64, n[1] as f64, n[2] as f64);
        let v = [0, 1, 2].map(|r| sign * (cof[r][0] * x + cof[r][1] * y + cof[r][2] * z));
        unit(v).map_or(n, |u| u.map(|c| c as f32))
    }

    pub fn det(&self) -> f64 {
        let m = &self.m;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }
}

/// How a polygon's corners get their normals.
#[derive(Copy, Clone, Debug)]
pub enum Normals<'a> {
    /// One per corner, as the file gives them (normalised here; a zero or non-finite one takes the
    /// face's normal). A slice of the wrong length is treated as `Crease { domain: 0, angle: 0 }`.
    Given(&'a [[f32; 3]]),
    /// Computed from the faces: a corner is smoothed with the faces around its position that are in
    /// the same `domain` and turn from its own face by at most `angle` radians. 0 gives flat faces.
    Crease { domain: u32, angle: f32 },
}

struct Tri {
    material: u32,
    p: [[f32; 3]; 3],
    n: Option<[[f32; 3]; 3]>,
    domain: u32,
    /// The cosine a neighbour's normal must reach to smooth with this face.
    min_cos: f64,
}

/// Collects polygons, triangulates them and welds the result into a [`Mesh`].
#[derive(Default)]
pub struct MeshBuilder {
    materials: Vec<Material>,
    tris: Vec<Tri>,
    /// Polygons with no area (under three distinct corners, or all on one line): nothing was added.
    pub degenerate: usize,
    /// Polygons that cross themselves: what clipping could not take was added as a fan, which may
    /// cover some area twice. Readers report the count as a warning.
    pub crossing: usize,
}

impl MeshBuilder {
    pub fn new() -> Self {
        MeshBuilder::default()
    }

    /// Adds a material (clamped) and returns its index.
    pub fn add_material(&mut self, m: Material) -> usize {
        self.materials.push(m.clamped());
        self.materials.len() - 1
    }

    pub fn materials(&self) -> &[Material] {
        &self.materials
    }

    /// For a reader that learns a material's factors after its faces (it is clamped at `finish`).
    pub fn material_mut(&mut self, index: usize) -> &mut Material {
        &mut self.materials[index]
    }

    /// Adds one polygon, corners counter-clockwise seen from its front. Repeated consecutive corners
    /// (and a last corner equal to the first) are dropped; a polygon with no area adds nothing,
    /// counts in [`MeshBuilder::degenerate`] and returns false. A material index out of range is
    /// a reader bug and takes material 0 (added when there is none).
    pub fn polygon(&mut self, material: usize, corners: &[[f32; 3]], normals: Normals) -> bool {
        let (given, domain, angle) = match normals {
            Normals::Given(n) if n.len() == corners.len() => (Some(n), 0, 0.0),
            Normals::Given(_) => (None, 0, 0.0),
            Normals::Crease { domain, angle } => (None, domain, angle),
        };
        let mut idx: Vec<usize> = Vec::with_capacity(corners.len());
        for i in 0..corners.len() {
            if idx.last().is_some_and(|&l| corners[l] == corners[i]) {
                continue;
            }
            idx.push(i);
        }
        while idx.len() > 1 && corners[idx[0]] == corners[idx[idx.len() - 1]] {
            idx.pop();
        }
        if idx.len() < 3 || corners.iter().any(|c| c.iter().any(|v| !v.is_finite())) {
            self.degenerate += 1;
            return false;
        }
        let pts: Vec<[f64; 3]> = idx.iter().map(|&i| corners[i].map(|v| v as f64)).collect();
        let mut newell = [0.0f64; 3];
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            newell[0] += (a[1] - b[1]) * (a[2] + b[2]);
            newell[1] += (a[2] - b[2]) * (a[0] + b[0]);
            newell[2] += (a[0] - b[0]) * (a[1] + b[1]);
        }
        let Some(_) = unit(newell) else {
            self.degenerate += 1;
            return false;
        };
        let mut local = Vec::new();
        if pts.len() == 3 {
            local.push([0, 1, 2]);
        } else if !triangulate(&pts, newell, &mut local) {
            self.crossing += 1;
        }
        if local.is_empty() {
            self.degenerate += 1;
            return false;
        }
        if self.materials.is_empty() {
            self.materials.push(Material::named("default"));
        }
        let material = if material < self.materials.len() { material } else { 0 } as u32;
        let min_cos = if angle.is_finite() && angle > 0.0 { (angle as f64).min(std::f64::consts::PI).cos() } else { 1.0 } - 1e-6;
        for [a, b, c] in local {
            let (a, b, c) = (idx[a], idx[b], idx[c]);
            self.tris.push(Tri {
                material,
                p: [corners[a], corners[b], corners[c]],
                n: given.map(|n| [n[a], n[b], n[c]]),
                domain,
                min_cos,
            });
        }
        true
    }

    /// Welds equal corners, computes the normals and groups the triangles by material: one
    /// [`Prim`] per material that has faces, materials without faces dropped. Deterministic: the
    /// output order follows the input order.
    pub fn finish(self) -> Mesh {
        let tris = self.tris;
        // Face normals, unnormalised: their length is twice the area, so sums are area-weighted.
        let face: Vec<[f64; 3]> = tris
            .iter()
            .map(|t| {
                let [a, b, c] = t.p.map(|p| p.map(|v| v as f64));
                cross(sub(b, a), sub(c, a))
            })
            .collect();
        let face_unit: Vec<Option<[f64; 3]>> = face.iter().map(|&f| unit(f)).collect();

        // Weld positions by their bits (-0 is 0), then list the faces around each position.
        let mut pos_id: HashMap<[u32; 3], u32> = HashMap::new();
        let corner_pos: Vec<[u32; 3]> = tris
            .iter()
            .map(|t| {
                t.p.map(|p| {
                    let next = pos_id.len() as u32;
                    *pos_id.entry(key(p)).or_insert(next)
                })
            })
            .collect();
        let mut start = vec![0u32; pos_id.len() + 1];
        for c in &corner_pos {
            for &p in c {
                start[p as usize + 1] += 1;
            }
        }
        for i in 0..pos_id.len() {
            start[i + 1] += start[i];
        }
        let mut fill = start.clone();
        let mut around = vec![0u32; start[pos_id.len()] as usize];
        for (t, c) in corner_pos.iter().enumerate() {
            for &p in c {
                around[fill[p as usize] as usize] = t as u32;
                fill[p as usize] += 1;
            }
        }

        let mut slot: Vec<Option<usize>> = vec![None; self.materials.len()];
        let mut materials = Vec::new();
        let mut prims: Vec<Prim> = Vec::new();
        let mut welds: Vec<HashMap<([u32; 3], [u32; 3]), u32>> = Vec::new();
        for (t, tri) in tris.iter().enumerate() {
            let Some(fu) = face_unit[t] else { continue };
            let pi = *slot[tri.material as usize].get_or_insert_with(|| {
                materials.push(self.materials[tri.material as usize].clone().clamped());
                prims.push(Prim { material: materials.len() - 1, ..Prim::default() });
                welds.push(HashMap::new());
                prims.len() - 1
            });
            for k in 0..3 {
                let given = tri.n.and_then(|n| unit(n[k].map(|v| v as f64)));
                let n = given.unwrap_or_else(|| {
                    let p = corner_pos[t][k] as usize;
                    let mut sum = [0.0f64; 3];
                    for &g in &around[start[p] as usize..start[p + 1] as usize] {
                        let gt = &tris[g as usize];
                        let Some(gu) = face_unit[g as usize] else { continue };
                        if g as usize == t || (gt.n.is_none() && gt.domain == tri.domain && dot(fu, gu) >= tri.min_cos) {
                            sum = add(sum, face[g as usize]);
                        }
                    }
                    unit(sum).unwrap_or(fu)
                });
                let n = n.map(|v| v as f32);
                let prim = &mut prims[pi];
                let next = prim.positions.len() as u32;
                let v = *welds[pi].entry((key(tri.p[k]), key(n))).or_insert(next);
                if v == next {
                    prim.positions.push(tri.p[k]);
                    prim.normals.push(n);
                }
                prim.indices.push(v);
            }
        }
        Mesh { materials, prims }
    }
}

/// Ear clipping in the plane of the polygon's Newell normal. Returns false when the polygon crosses
/// itself and the rest was added as a fan.
fn triangulate(pts: &[[f64; 3]], newell: [f64; 3], out: &mut Vec<[usize; 3]>) -> bool {
    // Drop the normal's largest axis; the remaining two in cyclic order keep counter-clockwise when
    // that component is positive, and flipping one of them keeps it when it is negative.
    let k = (0..3).max_by(|&a, &b| newell[a].abs().total_cmp(&newell[b].abs())).unwrap_or(2);
    let (u, v) = ((k + 1) % 3, (k + 2) % 3);
    let s = if newell[k] < 0.0 { -1.0 } else { 1.0 };
    let q: Vec<[f64; 2]> = pts.iter().map(|p| [p[u], s * p[v]]).collect();
    let turn = |a: usize, b: usize, c: usize| {
        let (a, b, c) = (q[a], q[b], q[c]);
        (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0])
    };
    let n = q.len();
    if (0..n).all(|i| turn((i + n - 1) % n, i, (i + 1) % n) >= 0.0) {
        // Convex: a fan, skipping the zero-area triangles of collinear corners.
        for i in 1..n - 1 {
            if turn(0, i, i + 1) > 0.0 {
                out.push([0, i, i + 1]);
            }
        }
        return true;
    }
    let inside = |p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        let e = |a: [f64; 2], b: [f64; 2]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        e(a, b) >= 0.0 && e(b, c) >= 0.0 && e(c, a) >= 0.0
    };
    let mut ring: Vec<usize> = (0..n).collect();
    let mut at = 0;
    while ring.len() > 3 {
        let m = ring.len();
        let mut clipped = false;
        for step in 0..m {
            let i = (at + step) % m;
            let (a, b, c) = (ring[(i + m - 1) % m], ring[i], ring[(i + 1) % m]);
            if turn(a, b, c) <= 0.0 {
                continue;
            }
            let blocked = ring.iter().any(|&p| {
                p != a && p != b && p != c && q[p] != q[a] && q[p] != q[b] && q[p] != q[c] && inside(q[p], q[a], q[b], q[c])
            });
            if !blocked {
                out.push([a, b, c]);
                ring.remove(i);
                at = i % ring.len();
                clipped = true;
                break;
            }
        }
        if clipped {
            continue;
        }
        // No ear: drop a corner that adds no area (on a line with its neighbours), if there is one.
        if let Some(i) = (0..m).find(|&i| turn(ring[(i + m - 1) % m], ring[i], ring[(i + 1) % m]) == 0.0) {
            ring.remove(i);
            at = i % ring.len();
            continue;
        }
        for i in 1..ring.len() - 1 {
            out.push([ring[0], ring[i], ring[i + 1]]);
        }
        return false;
    }
    if turn(ring[0], ring[1], ring[2]) > 0.0 {
        out.push([ring[0], ring[1], ring[2]]);
    }
    true
}

fn key(p: [f32; 3]) -> [u32; 3] {
    p.map(|v| if v == 0.0 { 0 } else { v.to_bits() })
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn unit(v: [f64; 3]) -> Option<[f64; 3]> {
    let len = dot(v, v).sqrt();
    (len > 0.0 && len.is_finite()).then(|| v.map(|c| c / len))
}
