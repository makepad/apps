//! `fab.image_to_mesh`: a picture turned into a part on the plate by the AI hub's
//! image-to-3D model.
//!
//! The app runs the hub job (it owns the fleet link, the progress and the cancel); this
//! module holds what does not need the hub: the request's arguments, the picture made
//! ready for the wire (PNG, JPEG, WebP, GIF or BMP in, a PNG at most [`MAX_SIDE`] wide out), the GLB the
//! hub returns turned into one welded triangle mesh in millimetres, sized and stood on
//! the ground, and the object script that places it. The mesh is a project asset
//! (`<project>.meshes/<name>.mesh`, read by `m.import("name")`); the script references
//! it by name and scales it with a `Size` parameter, so nothing is pasted into a script.

use makepad_model::json::Value;
use makepad_model_script::lang::csg_sdf::mesh_field::MeshData;
use std::collections::HashMap;

/// The tool's short name.
pub const TOOL: &str = "image_to_mesh";

/// A figure is organic: overhangs everywhere; 25% gyroid is sturdy and isotropic;
/// 3 walls and 5/4 solid layers let thin features and tops close.
pub const PRINT_DEFAULTS: &[(&str, &str)] = &[
    ("enable_support", "1"),
    ("support_type", "tree(auto)"),
    ("support_style", "tree_organic"),
    ("sparse_infill_pattern", "gyroid"),
    ("sparse_infill_density", "25%"),
    ("wall_loops", "3"),
    ("top_shell_layers", "5"),
    ("bottom_shell_layers", "4"),
];

/// The hub model that turns a picture into a mesh.
pub const MODEL: &str = "trellis-2";

/// The longest side of the picture sent to the hub (the model conditions on far less).
pub const MAX_SIDE: u32 = 1024;
/// The smallest picture worth sending.
pub const MIN_SIDE: u32 = 64;

pub const SIZE_DEFAULT_MM: f64 = 60.0;
/// A piece whose mean thickness (twice its volume over its area) at the asked size is under
/// this is a sheet, not a part: two printed lines of a 0.4 mm nozzle.
pub const SHEET_MM: f64 = 0.8;
pub const SIZE_MIN_MM: f64 = 5.0;
pub const SIZE_MAX_MM: f64 = 300.0;

/// Surface detail: the hub's remesh grid (cells along the object's longest side).
pub const DETAIL_DEFAULT: u32 = 256;
pub const DETAIL_MIN: u32 = 64;
pub const DETAIL_MAX: u32 = 512;

/// One request, checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// A file path, or the name of a picture the person sent in the chat.
    pub image: String,
    /// The finished part's largest dimension.
    pub size_mm: f64,
    /// The remesh grid.
    pub detail: u32,
    /// The asset's and the script's name (default: from the picture's name).
    pub name: Option<String>,
    pub seed: Option<u64>,
    /// Repair and strengthen the fitted mesh before placing it (default true).
    pub printable: bool,
}

impl Request {
    /// The face budget the hub decimates to: about what the grid resolves.
    pub fn face_target(&self) -> u32 {
        (self.detail * self.detail + self.detail * self.detail / 4).clamp(4_000, 400_000)
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::F64(f) => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

/// The tool's arguments: `image` (required), `size_mm`, `detail`, `name`, `seed`, `printable`.
pub fn parse_args(args: &Value) -> Result<Request, String> {
    let image = args.get("image").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if image.is_empty() {
        return Err("missing 'image': a picture file's path, or the name of a picture sent in the chat (chat.pictures lists them)".into());
    }
    let size_mm = match args.get("size_mm") {
        None | Some(Value::Null) => SIZE_DEFAULT_MM,
        Some(v) => match num(v) {
            Some(x) if (SIZE_MIN_MM..=SIZE_MAX_MM).contains(&x) => x,
            _ => return Err(format!("'size_mm' is the part's largest dimension, {SIZE_MIN_MM} to {SIZE_MAX_MM} mm")),
        },
    };
    let detail = match args.get("detail") {
        None | Some(Value::Null) => DETAIL_DEFAULT,
        Some(v) => match num(v) {
            Some(x) if x.fract() == 0.0 && (DETAIL_MIN as f64..=DETAIL_MAX as f64).contains(&x) => x as u32,
            _ => return Err(format!("'detail' is the surface grid, a whole number from {DETAIL_MIN} to {DETAIL_MAX} (default {DETAIL_DEFAULT})")),
        },
    };
    let name = match args.get("name").and_then(Value::as_str) {
        Some(n) => {
            let n = ident(n);
            if n.is_empty() {
                return Err("'name' needs a letter or digit".into());
            }
            Some(n)
        }
        None => None,
    };
    let seed = match args.get("seed") {
        None | Some(Value::Null) => None,
        Some(Value::Int(i)) if *i >= 0 => Some(*i as u64),
        Some(_) => return Err("'seed' is a whole number from 0".into()),
    };
    let printable = match args.get("printable") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(v)) => *v,
        Some(_) => return Err("'printable' is true or false".into()),
    };
    Ok(Request { image: image.to_string(), size_mm, detail, name, seed, printable })
}

/// A Splash identifier from free text: lower case letters, digits and `_`, starting with a
/// letter (`"Red Fox.png"` is `red_fox`).
pub fn ident(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
        if out.len() >= 32 {
            break;
        }
    }
    let out = out.trim_end_matches('_').to_string();
    match out.chars().next() {
        Some(c) if c.is_ascii_digit() => format!("m_{out}"),
        _ => out,
    }
}

/// The name of a new asset: `wanted`, else `<wanted>_2`, ... — free among the project's
/// meshes and scripts.
pub fn free_name(wanted: &str, taken: &dyn Fn(&str) -> bool) -> String {
    let base = if wanted.is_empty() { "figure".to_string() } else { wanted.to_string() };
    if !taken(&base) {
        return base;
    }
    (2..).map(|i| format!("{base}_{i}")).find(|n| !taken(n)).unwrap()
}

/// The picture as the hub takes it: a PNG, JPEG, WebP, GIF or BMP decoded (the app's one
/// decoder, [`fab_colour::decode_rgba`]), shrunk so its longest side is at most [`MAX_SIDE`]
/// (alpha kept: a cut-out subject skips the hub's matting), written as a PNG. A file that is no
/// picture, or one too small to read a shape from, is refused with the reason.
pub fn prepare_image(bytes: &[u8]) -> Result<(Vec<u8>, [u32; 2]), String> {
    let limits = makepad_model::Limits { max_texture_dimension: 16384, ..Default::default() };
    let (width, height, pixels) = fab_colour::decode_rgba(bytes)?;
    let image = makepad_model::RgbaImage { width: width as u32, height: height as u32, pixels };
    let (w, h) = (image.width, image.height);
    if w < MIN_SIDE || h < MIN_SIDE {
        return Err(format!("the picture is {w} x {h} pixels: at least {MIN_SIDE} x {MIN_SIDE} are needed to read a shape"));
    }
    let k = (MAX_SIDE as f64 / w.max(h) as f64).min(1.0);
    let image = if k < 1.0 { shrink(&image, ((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1)) } else { image };
    let size = [image.width, image.height];
    let png = image.to_png(&limits).map_err(|e| format!("the picture could not be written as PNG: {e}"))?;
    Ok((png, size))
}

/// Box-filter shrink (each output pixel averages the source pixels it covers).
pub(crate) fn shrink(src: &makepad_model::RgbaImage, w: u32, h: u32) -> makepad_model::RgbaImage {
    let (sw, sh) = (src.width as usize, src.height as usize);
    let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        let (y0, y1) = (y * sh / h as usize, ((y + 1) * sh / h as usize).max(y * sh / h as usize + 1));
        for x in 0..w as usize {
            let (x0, x1) = (x * sw / w as usize, ((x + 1) * sw / w as usize).max(x * sw / w as usize + 1));
            let mut sum = [0u32; 4];
            for sy in y0..y1.min(sh) {
                for sx in x0..x1.min(sw) {
                    let p = &src.pixels[(sy * sw + sx) * 4..][..4];
                    for c in 0..4 {
                        sum[c] += p[c] as u32;
                    }
                }
            }
            let count = ((y1.min(sh) - y0) * (x1.min(sw) - x0)).max(1) as u32;
            pixels.extend(sum.iter().map(|s| (s / count) as u8));
        }
    }
    makepad_model::RgbaImage { width: w, height: h, pixels }
}

type M4 = [f64; 16];

fn m4_mul(a: &M4, b: &M4) -> M4 {
    // column-major, glTF's order
    std::array::from_fn(|i| {
        let (c, r) = (i / 4, i % 4);
        (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum()
    })
}

fn node_matrix(node: &makepad_gltf::GltfNode) -> M4 {
    if let Some(m) = node.matrix {
        return m.map(|x| x as f64);
    }
    let t = node.translation.unwrap_or([0.0; 3]).map(|x| x as f64);
    let [x, y, z, w] = node.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]).map(|x| x as f64);
    let s = node.scale.unwrap_or([1.0; 3]).map(|x| x as f64);
    [
        (1.0 - 2.0 * (y * y + z * z)) * s[0], 2.0 * (x * y + z * w) * s[0], 2.0 * (x * z - y * w) * s[0], 0.0,
        2.0 * (x * y - z * w) * s[1], (1.0 - 2.0 * (x * x + z * z)) * s[1], 2.0 * (y * z + x * w) * s[1], 0.0,
        2.0 * (x * z + y * w) * s[2], 2.0 * (y * z - x * w) * s[2], (1.0 - 2.0 * (x * x + y * y)) * s[2], 0.0,
        t[0], t[1], t[2], 1.0,
    ]
}

/// Every triangle of a GLB's default scene, in its own frame (glTF: Y up), as one
/// triangle list. Without scenes, use its root nodes, or its meshes if there are no
/// nodes. A file with no triangles is refused.
pub fn mesh_from_glb(glb: &[u8]) -> Result<MeshData, String> {
    let loaded = makepad_gltf::load_gltf_from_bytes(glb, None).map_err(|e| format!("the hub's GLB does not read: {e}"))?;
    mesh_from_gltf(&loaded).map_err(|e| format!("the hub's GLB: {e}"))
}

/// The loaded scene in glTF's frame, with every node's transform applied.
pub(crate) fn mesh_from_gltf(loaded: &makepad_gltf::LoadedGltf) -> Result<MeshData, String> {
    let doc = &loaded.document;
    let nodes = doc.nodes_slice();
    let mut placed: Vec<(usize, M4)> = Vec::with_capacity(nodes.len());
    let roots = if !doc.scenes_slice().is_empty() || doc.scene.is_some() {
        let scene = doc.scenes_slice().get(doc.scene.unwrap_or(0)).ok_or("the glTF scene is missing")?;
        scene.nodes.clone().unwrap_or_default()
    } else {
        let mut child = vec![false; nodes.len()];
        for node in nodes {
            for &c in node.children.as_deref().unwrap_or(&[]) {
                *child.get_mut(c).ok_or("a glTF child node is missing")? = true;
            }
        }
        (0..nodes.len()).filter(|&n| !child[n]).collect()
    };
    let mut stack: Vec<(usize, M4)> = roots.iter().rev().map(|&n| (n, m4_identity())).collect();
    let mut seen = vec![false; nodes.len()];
    while let Some((n, parent)) = stack.pop() {
        let node = nodes.get(n).ok_or("a glTF scene node is missing")?;
        if seen[n] {
            return Err("the glTF scene repeats a node or has a cycle".into());
        }
        seen[n] = true;
        let world = m4_mul(&parent, &node_matrix(node));
        if let Some(mesh) = node.mesh {
            placed.push((mesh, world));
        }
        for &c in node.children.as_deref().unwrap_or(&[]).iter().rev() {
            stack.push((c, world));
        }
    }
    if doc.scenes_slice().is_empty() && nodes.is_empty() {
        placed = (0..doc.meshes_slice().len()).map(|m| (m, m4_identity())).collect();
    }
    let mut out = MeshData { positions: Vec::new(), triangles: Vec::new() };
    for (mesh, world) in placed {
        let m = doc.meshes_slice().get(mesh).ok_or("a glTF node's mesh is missing")?;
        for (pi, prim) in m.primitives.iter().enumerate() {
            if prim.mode() != makepad_gltf::GLTF_MODE_TRIANGLES {
                continue;
            }
            let p = makepad_gltf::decode_mesh_primitive(loaded, mesh, pi).map_err(|e| format!("glTF mesh {mesh} does not read: {e}"))?;
            if p.indices.len() % 3 != 0 {
                return Err(format!("glTF mesh {mesh} has an incomplete triangle"));
            }
            if out.positions.len().saturating_add(p.positions.len()) > u32::MAX as usize {
                return Err("the glTF has too many vertices for mesh indices".into());
            }
            let base = out.positions.len() as u32;
            out.positions.reserve(p.positions.len());
            out.triangles.reserve(p.indices.len() / 3);
            for v in &p.positions {
                let v = v.map(|x| x as f64);
                let at = |r: usize| world[r] * v[0] + world[4 + r] * v[1] + world[8 + r] * v[2] + world[12 + r];
                out.positions.push([at(0) as f32, at(1) as f32, at(2) as f32]);
            }
            let det = world[0] * (world[5] * world[10] - world[9] * world[6]) - world[4] * (world[1] * world[10] - world[9] * world[2]) + world[8] * (world[1] * world[6] - world[5] * world[2]);
            for t in p.indices.chunks_exact(3) {
                if t.iter().any(|&i| i as usize >= p.positions.len()) {
                    return Err(format!("glTF mesh {mesh} has a triangle with a missing vertex"));
                }
                let t = if det < 0.0 { [t[0], t[2], t[1]] } else { [t[0], t[1], t[2]] };
                out.triangles.push(t.map(|i| base + i));
            }
        }
    }
    if out.triangles.is_empty() {
        return Err("no triangles in the glTF scene".into());
    }
    Ok(out)
}

fn m4_identity() -> M4 {
    std::array::from_fn(|i| if i % 5 == 0 { 1.0 } else { 0.0 })
}

/// What fitting found about the mesh.
#[derive(Clone, Debug, PartialEq)]
pub struct Fitted {
    pub mesh: MeshData,
    /// Width (X), height (Y) and depth (Z) in mm, as fitted.
    pub size: [f64; 3],
    /// Edges not shared by exactly two triangles, after welding: 0 is watertight.
    pub open_edges: usize,
    /// Edges shared by more than two triangles.
    pub nonmanifold_edges: usize,
    /// Separate pieces (connected by shared vertices), as kept.
    pub shells: usize,
    /// Pieces left out as sheets thinner than [`SHEET_MM`] (the picture's background).
    pub dropped_sheets: usize,
    /// The triangles were wound inside out and were turned.
    pub flipped: bool,
    /// A thin ground plate the generator fused under the figure was cut away.
    pub ground_plate: bool,
}

impl Fitted {
    pub fn watertight(&self) -> bool {
        self.open_edges == 0 && self.nonmanifold_edges == 0
    }
}

/// Centre a repaired mesh, stand it on Y=0 and restore its requested largest dimension.
/// Unlike `fit`, this does not weld, strip shells or change winding after repair.
pub fn restand(mut mesh: MeshData, size_mm: f64) -> Result<Fitted, String> {
    let (lo, hi) = bounds(&mesh.positions);
    let extent = (0..3).map(|i| (hi[i] - lo[i]) as f64).fold(0.0, f64::max);
    if !extent.is_finite() || extent <= 0.0 || !size_mm.is_finite() || size_mm <= 0.0 {
        return Err("the repaired mesh has no usable size".into());
    }
    let k = size_mm / extent;
    let origin = [(lo[0] as f64 + hi[0] as f64) * 0.5, lo[1] as f64, (lo[2] as f64 + hi[2] as f64) * 0.5];
    for p in &mut mesh.positions {
        *p = std::array::from_fn(|i| ((p[i] as f64 - origin[i]) * k) as f32);
    }
    let stats = crate::printable::stats(&mesh);
    Ok(Fitted { mesh, size: stats.size, open_edges: stats.open_edges, nonmanifold_edges: stats.nonmanifold_edges,
        shells: stats.shells, dropped_sheets: 0, flipped: false, ground_plate: false })
}

/// Weld on a grid of a millionth of the extent, drop empty triangles and wind outward.
pub(crate) fn weld_and_orient<T: Copy + Into<f64>>(points: &[[T; 3]], faces: &[[u32; 3]]) -> Result<(Vec<[f64; 3]>, Vec<[u32; 3]>, bool), String> {
    if faces.is_empty() {
        return Err("no triangles in the mesh".into());
    }
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in points {
        for i in 0..3 {
            let x = p[i].into();
            if !x.is_finite() {
                return Err("the mesh has a non-finite vertex".into());
            }
            lo[i] = lo[i].min(x);
            hi[i] = hi[i].max(x);
        }
    }
    let extent = (0..3).map(|i| hi[i] - lo[i]).fold(0.0, f64::max);
    if !extent.is_finite() || extent <= 0.0 {
        return Err("the mesh has no usable size".into());
    }
    let q = extent * 1e-6;
    let mut index: HashMap<[i64; 3], u32> = HashMap::with_capacity(points.len());
    let mut remap = Vec::with_capacity(points.len());
    let mut positions: Vec<[f64; 3]> = Vec::with_capacity(points.len());
    for p in points {
        let p = p.map(Into::into);
        let key = std::array::from_fn(|i| ((p[i] - lo[i]) / q).round() as i64);
        let i = *index.entry(key).or_insert_with(|| {
            positions.push(p);
            positions.len() as u32 - 1
        });
        remap.push(i);
    }
    drop(index);
    let mut triangles = Vec::with_capacity(faces.len());
    let mut volume = 0.0;
    for t in faces {
        let mut t = *t;
        for v in &mut t {
            *v = *remap.get(*v as usize).ok_or("a mesh triangle has a missing vertex")?;
        }
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] || tri_area(&positions, &t) == 0.0 {
            continue;
        }
        // A nearby origin keeps the volume sum stable for a distant placement.
        volume += tri_volume(&positions, &t, lo);
        triangles.push(t);
    }
    if triangles.is_empty() {
        return Err("no triangles with area in the mesh".into());
    }
    let flipped = volume < 0.0;
    if flipped {
        for t in &mut triangles {
            t.swap(1, 2);
        }
    }
    // Keep only vertices reached by a triangle, so unused OBJ vertices do not set its size.
    remap.clear();
    remap.resize(positions.len(), u32::MAX);
    for t in &triangles {
        for &v in t {
            remap[v as usize] = 0;
        }
    }
    let mut kept = 0;
    for i in 0..positions.len() {
        if remap[i] != u32::MAX {
            remap[i] = kept as u32;
            positions[kept] = positions[i];
            kept += 1;
        }
    }
    positions.truncate(kept);
    for t in &mut triangles {
        *t = t.map(|v| remap[v as usize]);
    }
    Ok((positions, triangles, flipped))
}

fn tri_volume(positions: &[[f64; 3]], t: &[u32; 3], origin: [f64; 3]) -> f64 {
    let [a, b, c]: [[f64; 3]; 3] = t.map(|i| std::array::from_fn(|k| positions[i as usize][k] - origin[k]));
    (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0])) / 6.0
}

fn tri_area(positions: &[[f64; 3]], t: &[u32; 3]) -> f64 {
    let [a, b, c] = t.map(|i| positions[i as usize]);
    let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt() * 0.5
}

/// The mesh welded (vertices closer than a millionth of its size are one: the hub's UV
/// seams split them), wound outward, sheets of background left out, scaled so its largest dimension is `size_mm`, centred
/// over the origin and standing on Y = 0 (Fab's model frame is Y up, like glTF's).
pub fn fit(mesh: &MeshData, size_mm: f64) -> Result<Fitted, String> {
    let (lo, hi) = bounds(&mesh.positions);
    let extent = (0..3).map(|i| (hi[i] - lo[i]) as f64).fold(0.0, f64::max);
    if !(extent > 0.0) || !extent.is_finite() {
        return Err("the hub's mesh has no size".into());
    }
    let (mut positions, triangles, flipped) = weld_and_orient(&mesh.positions, &mesh.triangles)?;
    // shells (union-find over triangle vertices)
    let mut parent: Vec<u32> = (0..positions.len() as u32).collect();
    fn root(p: &mut [u32], mut i: u32) -> u32 {
        while p[i as usize] != i {
            p[i as usize] = p[p[i as usize] as usize];
            i = p[i as usize];
        }
        i
    }
    for t in &triangles {
        let a = root(&mut parent, t[0]);
        for &v in &t[1..] {
            let b = root(&mut parent, v);
            parent[b as usize] = a;
        }
    }
    // Per piece: its volume and area. A piece far thinner than a printed line is a sheet the
    // model made of the picture's background (a table top, a hand under the object): it
    // cannot print and is left out, unless it is the largest piece.
    let mut pieces: HashMap<u32, (f64, f64)> = HashMap::new();
    let roots: Vec<u32> = triangles.iter().map(|t| root(&mut parent, t[0])).collect();
    for (t, r) in triangles.iter().zip(&roots) {
        let e = pieces.entry(*r).or_default();
        e.0 += tri_volume(&positions, t, [0.0; 3]);
        e.1 += tri_area(&positions, t);
    }
    let main = pieces.iter().max_by(|a, b| a.1 .0.abs().total_cmp(&b.1 .0.abs())).map(|(r, _)| *r);
    let k0 = size_mm / extent;
    let sheet = |r: &u32| Some(*r) != main && pieces.get(r).is_some_and(|(v, a)| *a > 0.0 && 2.0 * v.abs() * k0 / a < SHEET_MM);
    let dropped_sheets = pieces.keys().filter(|r| sheet(r)).count();
    let mut triangles: Vec<[u32; 3]> = triangles.iter().zip(&roots).filter(|(_, r)| !sheet(r)).map(|(t, _)| *t).collect();
    let shells = pieces.len() - dropped_sheets;
    // The generator's ground plane, fused under the figure: cut away, the figure's own contact
    // patches capped so it stands.
    let ground_plate = strip_ground_plate(&mut positions, &mut triangles);
    // edges of what is kept
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for t in &triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let open_edges = edges.values().filter(|&&c| c == 1).count();
    let nonmanifold_edges = edges.values().filter(|&&c| c > 2).count();
    // only the vertices that are used, then sized and stood on the ground
    let mut index = vec![u32::MAX; positions.len()];
    let mut kept: Vec<[f32; 3]> = Vec::new();
    let triangles: Vec<[u32; 3]> = triangles
        .iter()
        .map(|t| {
            t.map(|v| {
                if index[v as usize] == u32::MAX {
                    index[v as usize] = kept.len() as u32;
                    kept.push(positions[v as usize].map(|x| x as f32));
                }
                index[v as usize]
            })
        })
        .collect();
    let (lo, hi) = bounds(&kept);
    let extent = (0..3).map(|i| (hi[i] - lo[i]) as f64).fold(0.0, f64::max);
    let k = size_mm / extent;
    let centre = [(lo[0] + hi[0]) as f64 * 0.5, lo[1] as f64, (lo[2] + hi[2]) as f64 * 0.5];
    let positions: Vec<[f32; 3]> = kept.iter().map(|p| std::array::from_fn(|i| ((p[i] as f64 - centre[i]) * k) as f32)).collect();
    let size = std::array::from_fn(|i| (hi[i] - lo[i]) as f64 * k);
    Ok(Fitted { mesh: MeshData { positions, triangles }, size, open_edges, nonmanifold_edges, shells, dropped_sheets, flipped, ground_plate })
}

/// A fitted mesh thinner than this share of its largest dimension is a sheet, not an object.
pub const FLAT_SHARE: f64 = 0.015;

/// Why a fitted mesh is no object to print, when it is a flat sheet (the hub's model gave a
/// relief or a plate where a figure was asked for): its size and what to try.
pub fn flat_sheet(fitted: &Fitted) -> Option<String> {
    let largest = fitted.size.iter().copied().fold(0.0, f64::max);
    let thinnest = fitted.size.iter().copied().fold(f64::MAX, f64::min);
    if largest > 0.0 && thinnest < largest * FLAT_SHARE {
        Some(format!(
            "the hub's model made a flat sheet ({:.1} x {:.1} x {:.1} mm), not a solid object: try again, with a different picture or seed (one whole object, lit from the front, on a plain background)",
            fitted.size[0], fitted.size[1], fitted.size[2]
        ))
    } else {
        None
    }
}

/// The generators draw the figure on a ground plane and the model makes it solid: a thin slab
/// far wider than the figure standing on it. Found by its flat top (horizontal triangles, up
/// to a tenth of the height above the lowest point, as large as the slab's footprint) with the
/// figure's footprint above it a fraction of the slab's; the slab is then cut away at its
/// top and the figure's contact patches are closed flat, so it stands on its own feet. Y is
/// up. A mesh without such a slab is untouched.
fn strip_ground_plate(positions: &mut Vec<[f64; 3]>, triangles: &mut Vec<[u32; 3]>) -> bool {
    if triangles.is_empty() {
        return false;
    }
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in positions.iter() {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    let height = hi[1] - lo[1];
    if !(height > 0.0) {
        return false;
    }
    let area_xz = |from: f64, to: f64| {
        let (mut a, mut b) = ([f64::MAX; 2], [f64::MIN; 2]);
        let mut any = false;
        for p in positions.iter().filter(|p| p[1] >= from && p[1] < to) {
            any = true;
            a = [a[0].min(p[0]), a[1].min(p[2])];
            b = [b[0].max(p[0]), b[1].max(p[2])];
        }
        if any { (b[0] - a[0]) * (b[1] - a[1]) } else { 0.0 }
    };
    // Horizontal, upward faces within the slab's possible thickness, by level.
    let q = height * 1e-3;
    let mut levels: HashMap<i64, f64> = HashMap::new();
    for t in triangles.iter() {
        let [a, b, c] = t.map(|i| positions[i as usize]);
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len <= 0.0 || n[1] / len < 0.999 || (a[1] - b[1]).abs() > q || (a[1] - c[1]).abs() > q {
            continue;
        }
        if a[1] - lo[1] > height * 0.12 || a[1] - lo[1] < q * 2.0 {
            continue;
        }
        *levels.entry(((a[1] - lo[1]) / q).round() as i64).or_default() += len * 0.5;
    }
    let foot = area_xz(lo[1], lo[1] + q * 3.0);
    if foot <= 0.0 {
        return false;
    }
    // The slab's top: the highest level whose flat faces cover half the slab's footprint.
    let Some(top) = levels.iter().filter(|(_, a)| **a >= foot * 0.5).map(|(k, _)| lo[1] + *k as f64 * q).fold(None, |m: Option<f64>, y| Some(m.map_or(y, |m| m.max(y)))) else { return false };
    // The figure above it must be a small part of it.
    let above = area_xz(top + q * 2.0, f64::MAX);
    if above <= 0.0 || above > foot * 0.35 {
        return false;
    }
    let before = triangles.len();
    triangles.retain(|t| t.iter().any(|&i| positions[i as usize][1] > top + q * 0.5));
    if triangles.len() == before {
        return false;
    }
    // The open outline the figure leaves at the cut: closed flat, one fan per loop.
    let mut next: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut directed: HashMap<(u32, u32), i32> = HashMap::new();
    for t in triangles.iter() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if directed.remove(&(b, a)).is_none() {
                *directed.entry((a, b)).or_default() += 1;
            }
        }
    }
    for &(a, b) in directed.keys() {
        if positions[a as usize][1] <= top + q && positions[b as usize][1] <= top + q {
            next.entry(a).or_default().push(b);
        }
    }
    // Loops: follow the edges; each loop's fan centre is the mean of its vertices.
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let starts: Vec<u32> = next.keys().copied().collect();
    for s in starts {
        if seen.contains(&s) {
            continue;
        }
        let mut loop_: Vec<u32> = Vec::new();
        let mut at = s;
        while seen.insert(at) {
            loop_.push(at);
            match next.get(&at).and_then(|n| n.first()) {
                Some(&n) => at = n,
                None => break,
            }
        }
        if loop_.len() < 3 {
            continue;
        }
        let c = loop_.iter().fold([0.0; 3], |m, &i| std::array::from_fn(|k| m[k] + positions[i as usize][k] / loop_.len() as f64));
        positions.push([c[0], top, c[2]]);
        let centre = positions.len() as u32 - 1;
        for k in 0..loop_.len() {
            // the cut face looks down: the loop's edge a -> b is closed as b -> a to the centre
            triangles.push([loop_[(k + 1) % loop_.len()], loop_[k], centre]);
        }
    }
    true
}

pub(crate) fn bounds(p: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for v in p {
        for i in 0..3 {
            lo[i] = lo[i].min(v[i]);
            hi[i] = hi[i].max(v[i]);
        }
    }
    (lo, hi)
}

/// Where a mesh came from, written into its script's header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Provenance {
    /// The picture's name (a file name, or the chat picture's name).
    pub image: String,
    pub model: String,
}

/// The object script that places asset `name` at `size_mm` (its largest dimension, a
/// parameter: the mesh is stored at that size, so Size scales it from there).
pub fn script_source(header: &str, name: &str, size_mm: f64, bounds: (f64, f64), materials: Option<&[Option<crate::import::Material>]>) -> String {
    let (min, max) = bounds;
    // Written to a tenth of a millimetre: the mesh is stored at its size, so Size scales from it.
    let size_mm = (size_mm * 10.0).round() / 10.0;
    let max = (max * 10.0).round() / 10.0;
    let mut source = format!(
        "{header}\n\
         use mod.model.*\n\
         use mod.fab.*\n\
         let m = mod.model\n\
         \n\
         let size = param({size_mm}mm, min: {min}mm, max: {max}mm, step: 0.5mm, label: \"Size\", hint: \"the largest dimension\")\n\
         let {name} = m.import(\"{name}\", {{scale: size / {size_mm}mm}})\n\
         object({name})\n",
    );
    if let Some(materials) = materials {
        let materials = materials.iter().map(|m| match m {
            Some(m) => format!("Material{{albedo: #{:06x}, name: {}}}", crate::import::colour(m.albedo) >> 8, makepad_script::syntax::write::quote(&m.color)),
            None => "nil".into(),
        }).collect::<Vec<_>>().join(" ");
        source.push_str(&format!("m.facets({name}, [{materials}])\n"));
    }
    source
}
