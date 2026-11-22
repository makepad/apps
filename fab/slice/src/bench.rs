//! Slicer benchmark parts, generated here (no downloaded models), and the
//! measurements `fab-slice bench` reports.
//!
//! Parts (all in mm, base on z = 0, centred on the origin):
//! - `vase`: a twisted, ridged, hollow vase (~1.7M triangles).
//! - `gyroid`: a 100x100x60 block of gyroid sheet (thin walls), by marching
//!   tetrahedra.
//! - `bracket`: a filleted bracket with vertical and horizontal holes and a
//!   cantilevered shelf that needs supports, by marching tetrahedra.
//! - `knob`: a ridged knob with a bore, placed 24 times on one plate.
//! - `painted`: a small twisted vase with painted stripes (filament 2).
//!
//! Every part is checked closed and consistently oriented after the round
//! trip through the file format the slicers read.

use makepad_fab_slicer::mesh::Mesh;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::fmt::Write as _;

// ---------------------------------------------------------------- checks

#[derive(Debug, Default, Clone)]
pub struct MeshCheck {
    pub tris: usize,
    pub verts: usize,
    /// Directed edges with no opposite (holes).
    pub open_edges: usize,
    /// Directed edges used by more than one triangle (non-manifold or flipped).
    pub bad_edges: usize,
    pub degenerate: usize,
    pub volume: f64,
    /// V - E + F.
    pub euler: i64,
}

impl MeshCheck {
    pub fn manifold(&self) -> bool {
        self.open_edges == 0 && self.bad_edges == 0 && self.degenerate == 0 && self.volume > 0.0
    }
}

/// Closed, edge-manifold, consistently oriented (each directed edge once,
/// its reverse once), positive volume.
pub fn check(m: &Mesh) -> MeshCheck {
    let mut dir: HashMap<(u32, u32), u32> = HashMap::with_capacity(m.tris.len() * 3);
    let mut degenerate = 0;
    for t in &m.tris {
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            degenerate += 1;
            continue;
        }
        for k in 0..3 {
            *dir.entry((t[k], t[(k + 1) % 3])).or_insert(0) += 1;
        }
    }
    let mut open = 0;
    let mut bad = 0;
    let mut undirected = 0usize;
    for (&(a, b), &n) in &dir {
        if n > 1 {
            bad += 1;
        }
        match dir.get(&(b, a)) {
            None => {
                open += 1;
                undirected += 2; // counted once here
            }
            Some(_) => undirected += 1,
        }
    }
    let used: std::collections::HashSet<u32> = m.tris.iter().flat_map(|t| t.iter().copied()).collect();
    MeshCheck {
        tris: m.tris.len(),
        verts: used.len(),
        open_edges: open,
        bad_edges: bad,
        degenerate,
        volume: m.volume(),
        euler: used.len() as i64 - (undirected / 2) as i64 + m.tris.len() as i64,
    }
}

// ---------------------------------------------------------------- writers

/// Binary STL (f32 coordinates, as every slicer reads them).
pub fn stl_bytes(m: &Mesh) -> Vec<u8> {
    let mut out = Vec::with_capacity(84 + m.tris.len() * 50);
    let mut head = b"fab bench part".to_vec();
    head.resize(80, b' ');
    out.extend_from_slice(&head);
    out.extend_from_slice(&(m.tris.len() as u32).to_le_bytes());
    for t in &m.tris {
        out.extend_from_slice(&[0u8; 12]);
        for &i in t {
            for c in m.verts[i as usize] {
                out.extend_from_slice(&(c as f32).to_le_bytes());
            }
        }
        out.extend_from_slice(&[0, 0]);
    }
    out
}

/// One 3MF object, placed once per `at` (build items share the mesh).
pub struct Object3mf<'a> {
    pub name: &'a str,
    pub mesh: &'a Mesh,
    pub extruder: u32,
    pub at: Vec<(f64, f64)>,
}

/// A plain 3MF (core spec + the vendor's per-object extruder metadata and
/// `paint_color` facet attributes), coordinates rounded to f32.
pub fn threemf_bytes(objects: &[Object3mf]) -> Result<Vec<u8>, String> {
    let mut model = String::from(r#"<?xml version="1.0" encoding="UTF-8"?><model unit="millimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02" xmlns:slic3rpe="http://schemas.slic3r.org/3mf/2017/06"><resources>"#);
    let mut build = String::from("<build>");
    let mut cfg = String::from(r#"<?xml version="1.0" encoding="UTF-8"?><config>"#);
    for (i, o) in objects.iter().enumerate() {
        let id = i + 1;
        let _ = write!(model, r#"<object id="{id}" type="model" name="{}"><mesh><vertices>"#, o.name);
        for v in &o.mesh.verts {
            let _ = write!(model, r#"<vertex x="{}" y="{}" z="{}"/>"#, v[0] as f32, v[1] as f32, v[2] as f32);
        }
        model.push_str("</vertices><triangles>");
        for (k, t) in o.mesh.tris.iter().enumerate() {
            let paint = o.mesh.paint.get(k).map(|s| s.as_str()).unwrap_or("");
            if paint.is_empty() {
                let _ = write!(model, r#"<triangle v1="{}" v2="{}" v3="{}"/>"#, t[0], t[1], t[2]);
            } else {
                let _ = write!(model, r#"<triangle v1="{}" v2="{}" v3="{}" paint_color="{paint}"/>"#, t[0], t[1], t[2]);
            }
        }
        model.push_str("</triangles></mesh></object>");
        for (x, y) in &o.at {
            let _ = write!(build, r#"<item objectid="{id}" transform="1 0 0 0 1 0 0 0 1 {x} {y} 0" printable="1"/>"#);
        }
        let _ = write!(cfg, r#"<object id="{id}"><metadata key="name" value="{}"/><metadata key="extruder" value="{}"/></object>"#, o.name, o.extruder);
    }
    model.push_str("</resources>");
    build.push_str("</build></model>");
    model.push_str(&build);
    cfg.push_str("</config>");
    let ct = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#;
    let mut zip = makepad_zip_file::ZipWriter::new();
    let e = |e: makepad_zip_file::ZipWriteError| format!("zip: {e:?}");
    zip.add("[Content_Types].xml", ct.as_bytes(), makepad_zip_file::ZipMethod::Deflate).map_err(e)?;
    zip.add("_rels/.rels", rels.as_bytes(), makepad_zip_file::ZipMethod::Deflate).map_err(e)?;
    zip.add("3D/3dmodel.model", model.as_bytes(), makepad_zip_file::ZipMethod::Deflate).map_err(e)?;
    zip.add("Metadata/model_settings.config", cfg.as_bytes(), makepad_zip_file::ZipMethod::Deflate).map_err(e)?;
    zip.finish().map_err(e)
}

// ---------------------------------------------------------------- parametric tubes

/// Vertex rings around the z axis: `r(theta, z)` at each height.
struct Rings {
    n: usize,
    first: Vec<u32>,
}

fn rings(m: &mut Mesh, n: usize, zs: &[f64], r: &dyn Fn(f64, f64) -> f64) -> Rings {
    let mut first = Vec::new();
    for &z in zs {
        first.push(m.verts.len() as u32);
        for i in 0..n {
            let th = 2.0 * PI * i as f64 / n as f64;
            let rr = r(th, z);
            m.verts.push([rr * th.cos(), rr * th.sin(), z]);
        }
    }
    Rings { n, first }
}

impl Rings {
    fn v(&self, j: usize, i: usize) -> u32 {
        self.first[j] + (i % self.n) as u32
    }
}

/// The wall between consecutive rings, facing out (or in).
fn wall(m: &mut Mesh, r: &Rings, outward: bool) {
    for j in 0..r.first.len() - 1 {
        for i in 0..r.n {
            let (a, b, c, d) = (r.v(j, i), r.v(j, i + 1), r.v(j + 1, i + 1), r.v(j + 1, i));
            if outward {
                m.tris.push([a, b, c]);
                m.tris.push([a, c, d]);
            } else {
                m.tris.push([a, c, b]);
                m.tris.push([a, d, c]);
            }
        }
    }
}

/// A disc fan on ring `j`, facing up or down.
fn fan(m: &mut Mesh, r: &Rings, j: usize, z: f64, up: bool) {
    let c = m.verts.len() as u32;
    m.verts.push([0.0, 0.0, z]);
    for i in 0..r.n {
        let (a, b) = (r.v(j, i), r.v(j, i + 1));
        m.tris.push(if up { [c, a, b] } else { [c, b, a] });
    }
}

/// The annulus between ring `jo` of `outer` and ring `ji` of `inner` (same
/// count), facing up or down.
fn annulus(m: &mut Mesh, outer: &Rings, jo: usize, inner: &Rings, ji: usize, up: bool) {
    for i in 0..outer.n {
        let (o0, o1, n0, n1) = (outer.v(jo, i), outer.v(jo, i + 1), inner.v(ji, i), inner.v(ji, i + 1));
        if up {
            m.tris.push([o0, o1, n1]);
            m.tris.push([o0, n1, n0]);
        } else {
            m.tris.push([o0, n1, o1]);
            m.tris.push([o0, n0, n1]);
        }
    }
}

fn steps(z0: f64, z1: f64, n: usize) -> Vec<f64> {
    (0..=n).map(|k| z0 + (z1 - z0) * k as f64 / n as f64).collect()
}

/// A hollow vase: outer surface `r_out`, wall `wall_t` (radial), floor
/// `floor_t`, open top with a flat rim.
fn hollow(n_theta: usize, n_z: usize, height: f64, wall_t: f64, floor_t: f64, r_out: &dyn Fn(f64, f64) -> f64) -> Mesh {
    let mut m = Mesh::default();
    let outer = rings(&mut m, n_theta, &steps(0.0, height, n_z), r_out);
    let n_in = ((n_z as f64) * (height - floor_t) / height).round().max(1.0) as usize;
    let inner = rings(&mut m, n_theta, &steps(floor_t, height, n_in), &|th, z| r_out(th, z) - wall_t);
    wall(&mut m, &outer, true);
    wall(&mut m, &inner, false);
    fan(&mut m, &outer, 0, 0.0, false);
    fan(&mut m, &inner, 0, floor_t, true);
    annulus(&mut m, &outer, outer.first.len() - 1, &inner, inner.first.len() - 1, true);
    m
}

/// (a) Twisted, ridged vase, 150 mm tall, ~1.7M triangles.
pub fn vase() -> Mesh {
    hollow(1440, 300, VASE_H, 2.4, 3.0, &vase_radius)
}

pub const VASE_H: f64 = 150.0;

/// The vase's outer radius at angle `th` and height `z` (its wall is 2.4 mm
/// radially inside, its floor 3 mm thick): the analytic surface.
pub fn vase_radius(th: f64, z: f64) -> f64 {
    let h = VASE_H;
    let profile = 34.0 + 12.0 * (PI * z / h * 1.25).sin() - 6.0 * (z / h).powi(3);
    profile + 2.5 * (18.0 * th + 3.0 * PI * z / h).cos() + 0.8 * (54.0 * th - 2.0 * PI * z / h).cos()
}

/// (e) Small twisted vase (60 mm), the twisted ridges painted filament 2.
pub fn painted() -> Mesh {
    let h = 60.0;
    let twist = |th: f64, z: f64| 8.0 * th + 2.0 * PI * z / h;
    let mut m = hollow(720, 120, h, 2.4, 3.0, &|th, z| 24.0 + 6.0 * (PI * z / h).sin() + 2.0 * twist(th, z).cos());
    // Paint outer-wall facets on the ridge crests. Facet state "8" =
    // filament 2 (whole facet).
    let n_outer_tris = 720 * 120 * 2;
    m.paint = (0..m.tris.len())
        .map(|k| {
            if k >= n_outer_tris {
                return String::new();
            }
            let t = m.tris[k];
            let c: [f64; 3] = [0, 1, 2].map(|a| (m.verts[t[0] as usize][a] + m.verts[t[1] as usize][a] + m.verts[t[2] as usize][a]) / 3.0);
            let th = c[1].atan2(c[0]);
            if twist(th, c[2]).cos() > 0.4 { "8".to_string() } else { String::new() }
        })
        .collect();
    m
}

/// (d) A ridged knob with a bore, 30 mm across, 16 mm tall.
pub fn knob() -> Mesh {
    let mut m = Mesh::default();
    let zs = steps(0.0, 16.0, 32);
    let outer = rings(&mut m, 360, &zs, &|th, z| 13.5 + 0.9 * (24.0 * th).cos() - if z > 14.0 { (z - 14.0) * 0.8 } else { 0.0 });
    let inner = rings(&mut m, 360, &zs, &|_, _| 4.0);
    wall(&mut m, &outer, true);
    wall(&mut m, &inner, false);
    annulus(&mut m, &outer, 0, &inner, 0, false);
    annulus(&mut m, &outer, zs.len() - 1, &inner, zs.len() - 1, true);
    m
}

// ---------------------------------------------------------------- marching tetrahedra

/// The six tetrahedra of a cube around its main diagonal (corner 0 to 7;
/// corner bits: x = 1, y = 2, z = 4). This split is conforming between
/// neighbouring cubes.
const KUHN: [[usize; 4]; 6] = [[0, 1, 3, 7], [0, 1, 5, 7], [0, 2, 3, 7], [0, 2, 6, 7], [0, 4, 5, 7], [0, 4, 6, 7]];

fn parity(p: &[usize; 4]) -> bool {
    let mut inv = 0;
    for i in 0..4 {
        for j in i + 1..4 {
            if p[i] > p[j] {
                inv += 1;
            }
        }
    }
    inv % 2 == 0
}

/// Even permutations of 0..4 (orientation-preserving vertex orders).
fn even_perms() -> Vec<[usize; 4]> {
    let mut out = Vec::new();
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let p = [a, b, c, d];
                    let mut s = p;
                    s.sort();
                    if s == [0, 1, 2, 3] && parity(&p) {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}

/// Extract the closed surface `f = 0` (inside: `f < 0`) on a regular grid
/// over `lo..hi` with spacing `h`. The grid is padded by one cell so the
/// surface closes against the bounds.
pub fn marching_tets(lo: [f64; 3], hi: [f64; 3], h: f64, f: &(dyn Fn([f64; 3]) -> f64 + Sync)) -> Mesh {
    let n = [0, 1, 2].map(|a| ((hi[a] - lo[a]) / h).ceil() as usize + 3);
    let org = [0, 1, 2].map(|a| lo[a] - h);
    let pos = |i: usize, j: usize, k: usize| [org[0] + i as f64 * h, org[1] + j as f64 * h, org[2] + k as f64 * h];
    // Field values, nudged off zero so every crossing is strictly inside an
    // edge (and stays a distinct point after rounding to f32).
    let eps = 1e-3 * h;
    let sample = |k: usize| -> Vec<f64> {
        let mut v = Vec::with_capacity(n[0] * n[1]);
        for j in 0..n[1] {
            for i in 0..n[0] {
                let bound = i == 0 || j == 0 || k == 0 || i == n[0] - 1 || j == n[1] - 1 || k == n[2] - 1;
                let x = if bound { h } else { f(pos(i, j, k)) };
                v.push(if x.abs() < eps { if x < 0.0 { -eps } else { eps } } else { x });
            }
        }
        v
    };
    let id = |i: usize, j: usize, k: usize| (k * n[1] + j) * n[0] + i;
    let even = even_perms();
    // Tetra orientation: Kuhn tets (in the corner order above) and whether
    // each is positively oriented.
    let corner = |c: usize| [(c & 1) as f64, ((c >> 1) & 1) as f64, ((c >> 2) & 1) as f64];
    let positive: Vec<bool> = KUHN
        .iter()
        .map(|t| {
            let p = t.map(corner);
            let d = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
            let (u, v, w) = (d(p[1], p[0]), d(p[2], p[0]), d(p[3], p[0]));
            u[0] * (v[1] * w[2] - v[2] * w[1]) - u[1] * (v[0] * w[2] - v[2] * w[0]) + u[2] * (v[0] * w[1] - v[1] * w[0]) > 0.0
        })
        .collect();
    let mut m = Mesh::default();
    let mut edge_vert: HashMap<(usize, usize), u32> = HashMap::new();
    let mut planes = vec![sample(0), sample(1)];
    for k in 0..n[2] - 1 {
        if k > 0 {
            planes.remove(0);
            planes.push(sample(k + 1));
        }
        for j in 0..n[1] - 1 {
            for i in 0..n[0] - 1 {
                let gid: [usize; 8] = std::array::from_fn(|c| id(i + (c & 1), j + ((c >> 1) & 1), k + ((c >> 2) & 1)));
                let val: [f64; 8] = std::array::from_fn(|c| planes[(c >> 2) & 1][(j + ((c >> 1) & 1)) * n[0] + i + (c & 1)]);
                let inside = val.map(|x| x < 0.0);
                if inside.iter().all(|&x| x) || inside.iter().all(|&x| !x) {
                    continue;
                }
                for (ti, tet) in KUHN.iter().enumerate() {
                    // A positively oriented vertex order of this tet.
                    let t: [usize; 4] = if positive[ti] { *tet } else { [tet[1], tet[0], tet[2], tet[3]] };
                    let ins: Vec<usize> = (0..4).filter(|&a| inside[t[a]]).collect();
                    if ins.is_empty() || ins.len() == 4 {
                        continue;
                    }
                    let mut cross = |a: usize, b: usize| -> u32 {
                        let (ca, cb) = (t[a], t[b]);
                        let key = if gid[ca] < gid[cb] { (gid[ca], gid[cb]) } else { (gid[cb], gid[ca]) };
                        *edge_vert.entry(key).or_insert_with(|| {
                            let (pa, pb) = (pos(i + (ca & 1), j + ((ca >> 1) & 1), k + ((ca >> 2) & 1)), pos(i + (cb & 1), j + ((cb >> 1) & 1), k + ((cb >> 2) & 1)));
                            let s = val[ca] / (val[ca] - val[cb]);
                            m.verts.push([pa[0] + (pb[0] - pa[0]) * s, pa[1] + (pb[1] - pa[1]) * s, pa[2] + (pb[2] - pa[2]) * s]);
                            (m.verts.len() - 1) as u32
                        })
                    };
                    let mut tris: Vec<[u32; 3]> = Vec::new();
                    match ins.len() {
                        1 | 3 => {
                            // Lone vertex: inside (1) or outside (3).
                            let lone = if ins.len() == 1 { ins[0] } else { (0..4).find(|a| !ins.contains(a)).unwrap() };
                            let p = even.iter().find(|p| p[0] == lone).unwrap();
                            let (e1, e2, e3) = (cross(p[0], p[1]), cross(p[0], p[2]), cross(p[0], p[3]));
                            tris.push(if ins.len() == 1 { [e1, e2, e3] } else { [e1, e3, e2] });
                        }
                        _ => {
                            let p = even.iter().find(|p| ins.contains(&p[0]) && ins.contains(&p[1])).unwrap();
                            let (ac, ad, bd, bc) = (cross(p[0], p[2]), cross(p[0], p[3]), cross(p[1], p[3]), cross(p[1], p[2]));
                            tris.push([ac, ad, bd]);
                            tris.push([ac, bd, bc]);
                        }
                    }
                    m.tris.extend(tris);
                }
            }
        }
        // Edges below plane k are never touched again.
        let floor = k * n[1] * n[0];
        edge_vert.retain(|key, _| key.0 >= floor);
    }
    m
}

fn sd_box(p: [f64; 3], c: [f64; 3], half: [f64; 3]) -> f64 {
    let q = [0, 1, 2].map(|a| (p[a] - c[a]).abs() - half[a]);
    let out = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2) + q[2].max(0.0).powi(2)).sqrt();
    out + q[0].max(q[1]).max(q[2]).min(0.0)
}

fn sd_round_box(p: [f64; 3], c: [f64; 3], half: [f64; 3], r: f64) -> f64 {
    sd_box(p, c, [half[0] - r, half[1] - r, half[2] - r]) - r
}

/// Capped cylinder along `axis` (0 = x, 1 = y, 2 = z).
fn sd_cyl(p: [f64; 3], c: [f64; 3], axis: usize, r: f64, half_len: f64) -> f64 {
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let d_r = ((p[u] - c[u]).powi(2) + (p[v] - c[v]).powi(2)).sqrt() - r;
    let d_a = (p[axis] - c[axis]).abs() - half_len;
    let out = (d_r.max(0.0).powi(2) + d_a.max(0.0).powi(2)).sqrt();
    out + d_r.max(d_a).min(0.0)
}

/// Smooth union (fillet radius ~k).
fn smin(a: f64, b: f64, k: f64) -> f64 {
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    b + (a - b) * h - k * h * (1.0 - h)
}

/// Smooth subtraction of b from a (rounded hole edges).
fn ssub(a: f64, b: f64, k: f64) -> f64 {
    -smin(-a, b, k)
}

/// (b) Gyroid sheet block, 100x100x60 mm: period 12 mm, walls ~1.2 mm.
pub fn gyroid() -> Mesh {
    let half = [50.0, 50.0, 30.0];
    let k = 2.0 * PI / 12.0;
    let f = move |p: [f64; 3]| {
        let (x, y, z) = (p[0] * k, p[1] * k, (p[2] - 30.0) * k);
        let g = x.sin() * y.cos() + y.sin() * z.cos() + z.sin() * x.cos();
        // |g| / |grad| ~ distance; |grad g| averages ~1.1 k.
        let sheet = g.abs() / (1.1 * k) - 0.6;
        sheet.max(sd_box(p, [0.0, 0.0, 30.0], half))
    };
    marching_tets([-50.0, -50.0, 0.0], [50.0, 50.0, 60.0], 0.8, &f)
}

/// (c) Bracket: filleted base with 8 vertical holes, an upright with 3
/// horizontal holes, a boss with a bore, and a 40 mm cantilevered shelf.
pub fn bracket() -> Mesh {
    marching_tets([-46.0, -31.0, 0.0], [46.0, 31.0, 49.0], 0.35, &bracket_field)
}

/// The bracket's distance field (the surface the mesh above samples).
pub fn bracket_field(p: [f64; 3]) -> f64 {
    let f = |p: [f64; 3]| {
        let base = sd_round_box(p, [0.0, 0.0, 5.0], [45.0, 30.0, 5.0], 3.0);
        let upright = sd_round_box(p, [0.0, 23.0, 25.0], [40.0, 5.0, 25.0], 2.0);
        let shelf = sd_round_box(p, [0.0, -2.0, 45.0], [32.0, 23.0, 3.5], 2.0);
        let boss = sd_cyl(p, [22.0, -12.0, 12.0], 2, 10.0, 12.0);
        let mut d = smin(base, upright, 5.0);
        d = smin(d, shelf, 4.0);
        d = smin(d, boss, 4.0);
        // Vertical holes through the base, rounded edges.
        for (x, y) in [(-38.0, -22.0), (-38.0, 0.0), (-38.0, 14.0), (38.0, -22.0), (38.0, 0.0), (38.0, 14.0), (-12.0, -22.0), (0.0, -10.0)] {
            d = ssub(d, sd_cyl(p, [x, y, 5.0], 2, 3.0, 8.0), 1.0);
        }
        // Bore through the boss.
        d = ssub(d, sd_cyl(p, [22.0, -12.0, 12.0], 2, 4.0, 14.0), 1.0);
        // Horizontal holes through the upright (overhanging hole roofs).
        for x in [-24.0, 0.0, 24.0] {
            d = ssub(d, sd_cyl(p, [x, 23.0, 26.0], 1, 6.0, 8.0), 1.0);
        }
        // A slot through the shelf.
        d = ssub(d, sd_round_box(p, [-14.0, -8.0, 45.0], [10.0, 4.0, 6.0], 1.0), 1.0);
        d
    };
    f(p)
}

// ---------------------------------------------------------------- cases

pub struct BenchCase {
    pub name: &'static str,
    /// Model file in the models folder.
    pub model: &'static str,
    pub process: &'static str,
    pub filaments: &'static [&'static str],
    pub map: &'static [u32],
}

pub const STD: &str = "process_0.20_standard.json";
pub const TREE: &str = "process_tree_organic.json";
pub const PLA: &str = "filament_generic_pla.json";

pub const CASES: &[BenchCase] = &[
    BenchCase { name: "b_vase", model: "vase.stl", process: STD, filaments: &[PLA], map: &[1] },
    BenchCase { name: "b_gyroid", model: "gyroid.stl", process: STD, filaments: &[PLA], map: &[1] },
    BenchCase { name: "b_bracket_tree", model: "bracket.stl", process: TREE, filaments: &[PLA], map: &[1] },
    BenchCase { name: "b_plate24", model: "plate24.3mf", process: STD, filaments: &[PLA], map: &[1] },
    BenchCase { name: "b_painted_dual", model: "painted.3mf", process: "process_0.20_standard_tower165.json", filaments: &["filament_generic_pla_white.json", "filament_bambu_pla_basic_red.json"], map: &[1, 2] },
];

/// The 24 knob positions (bed mm): 6 x 4 at 40 mm pitch around the centre.
pub fn plate_positions() -> Vec<(f64, f64)> {
    let mut v = Vec::new();
    for j in 0..4 {
        for i in 0..6 {
            v.push((175.0 + (i as f64 - 2.5) * 40.0, 160.0 + (j as f64 - 1.5) * 40.0));
        }
    }
    v
}

/// Write every part into `dir` and check each one as the slicers will read it.
pub fn generate(dir: &std::path::Path) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut report = String::new();
    let mut checked = |name: &str, file: &str, bytes: Vec<u8>, t0: std::time::Instant| -> Result<(), String> {
        let path = dir.join(file);
        std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let model = makepad_fab_slicer::mesh::load_model(path.to_str().unwrap_or(""))?;
        for o in &model.objects {
            let mut mesh = o.mesh.clone();
            mesh.weld();
            let c = check(&mesh);
            let _ = writeln!(
                report,
                "{name} ({file}, {:.1} MB, {:.1} s): {} tris, {} verts, open {}, bad {}, degenerate {}, euler {}, volume {:.0} mm3 -> {}",
                bytes.len() as f64 / 1e6,
                t0.elapsed().as_secs_f64(),
                c.tris,
                c.verts,
                c.open_edges,
                c.bad_edges,
                c.degenerate,
                c.euler,
                c.volume,
                if c.manifold() { "MANIFOLD" } else { "NOT MANIFOLD" }
            );
            if !c.manifold() {
                return Err(format!("{name}: not manifold: {c:?}"));
            }
            if model.objects.len() > 1 {
                let _ = writeln!(report, "  ({} objects, the same mesh)", model.objects.len());
                break;
            }
        }
        Ok(())
    };
    let t = std::time::Instant::now();
    checked("vase", "vase.stl", stl_bytes(&vase()), t)?;
    let t = std::time::Instant::now();
    checked("gyroid", "gyroid.stl", stl_bytes(&gyroid()), t)?;
    let t = std::time::Instant::now();
    checked("bracket", "bracket.stl", stl_bytes(&bracket()), t)?;
    let t = std::time::Instant::now();
    let k = knob();
    checked("plate24", "plate24.3mf", threemf_bytes(&[Object3mf { name: "knob", mesh: &k, extruder: 1, at: plate_positions() }])?, t)?;
    let t = std::time::Instant::now();
    let p = painted();
    checked("painted", "painted.3mf", threemf_bytes(&[Object3mf { name: "painted_vase", mesh: &p, extruder: 1, at: vec![(175.0, 160.0)] }])?, t)?;
    Ok(report)
}

// ---------------------------------------------------------------- process stats

/// (no peak RSS, user+sys CPU s) of this process, from its times (Windows).
#[cfg(windows)]
pub fn proc_usage() -> (Option<f64>, Option<f64>) {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn GetProcessTimes(process: *mut std::ffi::c_void, creation: *mut u64, exit: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
    }
    let (mut creation, mut exit, mut kernel, mut user) = (0, 0, 0, 0);
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut creation, &mut exit, &mut kernel, &mut user) } != 0;
    (None, ok.then(|| (kernel + user) as f64 * 1e-7))
}

/// (peak RSS MB, user+sys CPU s) of this process, from /proc (Linux).
#[cfg(not(windows))]
pub fn proc_usage() -> (Option<f64>, Option<f64>) {
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|kb| kb.parse::<f64>().ok()))
        .map(|kb| kb / 1024.0);
    let cpu = std::fs::read_to_string("/proc/self/stat").ok().and_then(|s| {
        // Fields after the ")" of the command name: utime is field 14, stime 15.
        let rest = s.rsplit_once(')')?.1;
        let f: Vec<&str> = rest.split_whitespace().collect();
        let ut: f64 = f.get(11)?.parse().ok()?;
        let st: f64 = f.get(12)?.parse().ok()?;
        Some((ut + st) / 100.0)
    });
    (rss, cpu)
}

/// Per-nozzle XY extents of extrusion (all layers, and the first layer),
/// from the G-code (T<n> -> filament_map -> nozzle). [minx, miny, maxx, maxy].
pub fn nozzle_extents(text: &str) -> ([[f64; 4]; 2], [[f64; 4]; 2], Option<f64>) {
    let empty = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut all = [empty; 2];
    let mut first = [empty; 2];
    let map: Vec<usize> = text
        .lines()
        .find_map(|l| l.strip_prefix("; filament_map = "))
        .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
        .unwrap_or_default();
    let word = |c: &str, w: char| c.split_whitespace().find(|t| t.starts_with(w)).and_then(|t| t[1..].parse::<f64>().ok());
    let (mut tool, mut x, mut y, mut z) = (None::<usize>, 0.0, 0.0, 0.0);
    let mut first_z = None;
    let mut started = false;
    let mut rel_e = false;
    for l in text.lines() {
        if l.starts_with("; MACHINE_START_GCODE_END") {
            started = true;
            continue;
        }
        if l.starts_with("; MACHINE_END_GCODE_START") {
            break;
        }
        let c = l.split(';').next().unwrap_or("").trim();
        if c.is_empty() {
            continue;
        }
        let cmd = c.split_whitespace().next().unwrap_or("");
        if cmd == "M83" {
            rel_e = true;
        }
        if let Some(t) = cmd.strip_prefix('T').and_then(|t| t.parse::<usize>().ok()).filter(|t| *t < 255) {
            tool = map.get(t).and_then(|m| m.checked_sub(1));
        }
        if matches!(cmd, "G0" | "G1" | "G2" | "G3") {
            if let Some(v) = word(c, 'X') {
                x = v;
            }
            if let Some(v) = word(c, 'Y') {
                y = v;
            }
            if let Some(v) = word(c, 'Z') {
                z = v;
            }
            if started && rel_e && word(c, 'E').is_some_and(|e| e > 0.0) {
                let fz = *first_z.get_or_insert(z);
                if let Some(t) = tool.filter(|t| *t < 2) {
                    let grow = |e: &mut [f64; 4]| *e = [e[0].min(x), e[1].min(y), e[2].max(x), e[3].max(y)];
                    grow(&mut all[t]);
                    if (z - fz).abs() < 1e-6 {
                        grow(&mut first[t]);
                    }
                }
            }
        }
    }
    (all, first, first_z)
}

/// Arcs (G2/G3) that sweep the long way round (> 180 degrees) although
/// their extrusion fits the short chord: the direction letter is wrong, and
/// the machine would draw (most of) a full circle. Returns (arcs, wrong,
/// largest wrong radius mm, first wrong G-code line).
pub fn wrong_direction_arcs(text: &str) -> (usize, usize, f64, usize) {
    let (mut x, mut y) = (0.0f64, 0.0f64);
    let (mut n, mut bad, mut worst, mut first) = (0, 0, 0.0f64, 0);
    // E per mm of a 0.42 x 0.2 bead of 1.75 mm filament (order of magnitude).
    let e_per_mm = 0.035;
    for (ln, l) in text.lines().enumerate() {
        let c = l.split(';').next().unwrap_or("").trim();
        let mut it = c.split_whitespace();
        let cmd = it.next().unwrap_or("");
        if !matches!(cmd, "G0" | "G1" | "G2" | "G3") {
            continue;
        }
        let (mut nx, mut ny, mut i, mut j, mut e) = (x, y, 0.0, 0.0, 0.0);
        for w in it {
            let v = w[1..].parse::<f64>().unwrap_or(0.0);
            match w.as_bytes()[0] {
                b'X' => nx = v,
                b'Y' => ny = v,
                b'I' => i = v,
                b'J' => j = v,
                b'E' => e = v,
                _ => {}
            }
        }
        if cmd == "G2" || cmd == "G3" {
            n += 1;
            let (cx, cy) = (x + i, y + j);
            let side = (x - cx) * (ny - cy) - (y - cy) * (nx - cx);
            let long_way = (cmd == "G3" && side < 0.0) || (cmd == "G2" && side > 0.0);
            let r = i.hypot(j);
            let chord = (nx - x).hypot(ny - y);
            let long_len = r * (std::f64::consts::TAU - 2.0 * (chord / (2.0 * r)).min(1.0).asin());
            if long_way && e > 0.0 && e / e_per_mm < 0.5 * long_len {
                bad += 1;
                worst = worst.max(r);
                if first == 0 {
                    first = ln + 1;
                }
            }
        }
        x = nx;
        y = ny;
    }
    (n, bad, worst, first)
}
