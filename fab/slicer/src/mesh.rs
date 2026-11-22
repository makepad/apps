//! Triangle meshes and the model files they come from (STL, 3MF).

use crate::xml::{attr, XmlEvent, XmlReader};
use std::collections::HashMap;
use std::io::Cursor;

#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub verts: Vec<[f64; 3]>,
    /// Counter-clockwise seen from outside.
    pub tris: Vec<[u32; 3]>,
    /// Per-triangle colour paint (the 3MF `paint_color` bit stream as hex),
    /// empty when nothing is painted.
    pub paint: Vec<String>,
}

impl Mesh {
    pub fn bbox(&self) -> ([f64; 3], [f64; 3]) {
        let mut lo = [f64::MAX; 3];
        let mut hi = [f64::MIN; 3];
        for v in &self.verts {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        (lo, hi)
    }

    pub fn translate(&mut self, d: [f64; 3]) {
        for v in &mut self.verts {
            for k in 0..3 {
                v[k] += d[k];
            }
        }
    }

    pub fn transform(&mut self, m: &[f64; 12]) {
        for v in &mut self.verts {
            let (x, y, z) = (v[0], v[1], v[2]);
            v[0] = m[0] * x + m[3] * y + m[6] * z + m[9];
            v[1] = m[1] * x + m[4] * y + m[7] * z + m[10];
            v[2] = m[2] * x + m[5] * y + m[8] * z + m[11];
        }
    }

    pub fn append(&mut self, o: &Mesh) {
        let base = self.verts.len() as u32;
        self.verts.extend_from_slice(&o.verts);
        let painted = !self.paint.is_empty() || !o.paint.is_empty();
        if painted {
            self.paint.resize(self.tris.len(), String::new());
        }
        for t in &o.tris {
            self.tris.push([t[0] + base, t[1] + base, t[2] + base]);
        }
        if painted {
            if o.paint.is_empty() {
                self.paint.resize(self.tris.len(), String::new());
            } else {
                self.paint.extend(o.paint.iter().cloned());
            }
        }
    }

    pub fn volume(&self) -> f64 {
        let mut v = 0.0;
        for t in &self.tris {
            let a = self.verts[t[0] as usize];
            let b = self.verts[t[1] as usize];
            let c = self.verts[t[2] as usize];
            v += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0]);
        }
        v / 6.0
    }

    /// Merge exactly coincident vertices (STL stores each triangle's corners).
    pub fn weld(&mut self) {
        // Ids in first-seen order, through an open-addressing table on the
        // coordinates' bits (a slot holds its vertex's new id + 1, 0 = free).
        let n = self.verts.len();
        let cap = (n * 2).next_power_of_two().max(16);
        let shift = 64 - cap.trailing_zeros();
        let mut slot: Vec<u32> = vec![0; cap];
        let mut verts: Vec<[f64; 3]> = Vec::with_capacity(n / 4 + 16);
        let mut remap = Vec::with_capacity(n);
        for v in &self.verts {
            let (x, y, z) = (v[0].to_bits(), v[1].to_bits(), v[2].to_bits());
            let h = (x.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ y.wrapping_mul(0xC2B2_AE3D_27D4_EB4F).rotate_left(21) ^ z.wrapping_mul(0x1656_67B1_9E37_79F9).rotate_left(42)).wrapping_mul(0x2545_F491_4F6C_DD1D);
            let mut i = (h >> shift) as usize;
            let id = loop {
                let s = slot[i];
                if s == 0 {
                    verts.push(*v);
                    slot[i] = verts.len() as u32;
                    break verts.len() as u32 - 1;
                }
                let w = verts[s as usize - 1];
                if w[0].to_bits() == x && w[1].to_bits() == y && w[2].to_bits() == z {
                    break s - 1;
                }
                i = (i + 1) & (cap - 1);
            };
            remap.push(id);
        }
        for t in &mut self.tris {
            for k in t.iter_mut() {
                *k = remap[*k as usize];
            }
        }
        self.verts = verts;
        // Drop degenerate triangles.
        let keep: Vec<bool> = self.tris.iter().map(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]).collect();
        if keep.iter().any(|k| !k) {
            let mut tris = Vec::new();
            let mut paint = Vec::new();
            for (i, t) in self.tris.iter().enumerate() {
                if keep[i] {
                    tris.push(*t);
                    if !self.paint.is_empty() {
                        paint.push(self.paint[i].clone());
                    }
                }
            }
            self.tris = tris;
            self.paint = paint;
        }
    }

    /// Flip every triangle when the mesh is inside out.
    pub fn fix_orientation(&mut self) {
        if self.volume() < 0.0 {
            for t in &mut self.tris {
                t.swap(1, 2);
            }
        }
    }
}

/// One printable object of a model file.
#[derive(Clone, Debug)]
pub struct ModelObject {
    pub name: String,
    pub mesh: Mesh,
    /// 1-based filament slot from the file's settings, 0 when unset.
    pub extruder: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    pub objects: Vec<ModelObject>,
}

pub fn load_model(path: &str) -> Result<Model, String> {
    let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let lower = path.to_ascii_lowercase();
    let name = std::path::Path::new(path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    if lower.ends_with(".3mf") {
        load_3mf(&data)
    } else if lower.ends_with(".stl") {
        let mesh = load_stl(&data)?;
        Ok(Model { objects: vec![ModelObject { name, mesh, extruder: 0 }] })
    } else {
        Err(format!("{path}: unknown model format"))
    }
}

pub fn load_stl(data: &[u8]) -> Result<Mesh, String> {
    let mut mesh = Mesh::default();
    let binary = data.len() >= 84 && {
        let n = u32::from_le_bytes([data[80], data[81], data[82], data[83]]) as usize;
        84 + n * 50 == data.len()
    };
    if binary {
        let n = u32::from_le_bytes([data[80], data[81], data[82], data[83]]) as usize;
        mesh.verts.reserve(n * 3);
        for i in 0..n {
            let o = 84 + i * 50 + 12;
            let f = |k: usize| f32::from_le_bytes([data[o + k * 4], data[o + k * 4 + 1], data[o + k * 4 + 2], data[o + k * 4 + 3]]) as f64;
            let base = mesh.verts.len() as u32;
            for c in 0..3 {
                mesh.verts.push([f(c * 3), f(c * 3 + 1), f(c * 3 + 2)]);
            }
            mesh.tris.push([base, base + 1, base + 2]);
        }
    } else {
        let text = std::str::from_utf8(data).map_err(|_| "STL: neither binary nor ASCII".to_string())?;
        let mut cur = Vec::new();
        for line in text.lines() {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix("vertex") {
                let v: Vec<f64> = rest.split_whitespace().filter_map(|s| s.parse::<f32>().ok()).map(|f| f as f64).collect();
                if v.len() == 3 {
                    cur.push([v[0], v[1], v[2]]);
                }
            } else if t.starts_with("endfacet") {
                if cur.len() == 3 {
                    let base = mesh.verts.len() as u32;
                    mesh.verts.extend_from_slice(&cur);
                    mesh.tris.push([base, base + 1, base + 2]);
                }
                cur.clear();
            }
        }
    }
    mesh.weld();
    mesh.fix_orientation();
    Ok(mesh)
}

fn parse_transform(s: &str) -> Option<[f64; 12]> {
    let v: Vec<f64> = s.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    if v.len() == 12 {
        let mut m = [0.0; 12];
        m.copy_from_slice(&v);
        Some(m)
    } else {
        None
    }
}

fn mul_transform(a: &[f64; 12], b: &[f64; 12]) -> [f64; 12] {
    // Row-vector convention of 3MF: p' = p * M; apply b then a.
    let ap = |m: &[f64; 12], x: f64, y: f64, z: f64, w: f64| -> [f64; 3] {
        [m[0] * x + m[3] * y + m[6] * z + m[9] * w, m[1] * x + m[4] * y + m[7] * z + m[10] * w, m[2] * x + m[5] * y + m[8] * z + m[11] * w]
    };
    let mut r = [0.0; 12];
    for (c, basis) in [(0usize, [1.0, 0.0, 0.0, 0.0]), (1, [0.0, 1.0, 0.0, 0.0]), (2, [0.0, 0.0, 1.0, 0.0]), (3, [0.0, 0.0, 0.0, 1.0])] {
        let p = ap(b, basis[0], basis[1], basis[2], basis[3]);
        let q = ap(a, p[0], p[1], p[2], basis[3]);
        r[c * 3] = q[0];
        r[c * 3 + 1] = q[1];
        r[c * 3 + 2] = q[2];
    }
    r
}

struct ObjDef {
    name: String,
    mesh: Mesh,
    components: Vec<(String, u32, Option<[f64; 12]>)>,
}

fn parse_model_xml(text: &str, part: &str, defs: &mut HashMap<(String, u32), ObjDef>, build: &mut Vec<(u32, Option<[f64; 12]>)>) -> Result<f64, String> {
    let mut r = XmlReader::new(text);
    let mut cur: Option<(u32, ObjDef)> = None;
    let mut unit_mm = 1.0;
    while let Some(ev) = r.next_event() {
        match ev? {
            XmlEvent::Start { name, attrs, .. } => match name {
                "model" => {
                    unit_mm = match attr(&attrs, "unit") {
                        None | Some("millimeter") => 1.0,
                        Some("micron") => 0.001,
                        Some("centimeter") => 10.0,
                        Some("inch") => 25.4,
                        Some("foot") => 304.8,
                        Some("meter") => 1000.0,
                        Some(unit) => return Err(format!("3MF {part}: unknown model unit '{unit}'")),
                    };
                }
                "object" => {
                    let id = attr(&attrs, "id").and_then(|s| s.parse().ok()).unwrap_or(0);
                    let nm = attr(&attrs, "name").unwrap_or("").to_string();
                    cur = Some((id, ObjDef { name: nm, mesh: Mesh::default(), components: Vec::new() }));
                }
                "vertex" => {
                    if let Some((_, d)) = cur.as_mut() {
                        let g = |k: &str| attr(&attrs, k).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                        d.mesh.verts.push([g("x"), g("y"), g("z")]);
                    }
                }
                "triangle" => {
                    if let Some((_, d)) = cur.as_mut() {
                        let g = |k: &str| attr(&attrs, k).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                        d.mesh.tris.push([g("v1"), g("v2"), g("v3")]);
                        let paint = attr(&attrs, "paint_color").unwrap_or("");
                        if !paint.is_empty() || !d.mesh.paint.is_empty() {
                            let n = d.mesh.tris.len();
                            d.mesh.paint.resize(n - 1, String::new());
                            d.mesh.paint.push(paint.to_string());
                        }
                    }
                }
                "component" => {
                    if let Some((_, d)) = cur.as_mut() {
                        let oid = attr(&attrs, "objectid").and_then(|s| s.parse().ok()).unwrap_or(0);
                        let path = attr(&attrs, "p:path").or_else(|| attr(&attrs, "path")).map(|p| p.trim_start_matches('/').to_string()).unwrap_or_else(|| part.to_string());
                        let tr = attr(&attrs, "transform").and_then(parse_transform);
                        d.components.push((path, oid, tr));
                    }
                }
                "item" => {
                    let oid = attr(&attrs, "objectid").and_then(|s| s.parse().ok()).unwrap_or(0);
                    let tr = attr(&attrs, "transform").and_then(parse_transform);
                    build.push((oid, tr));
                }
                _ => {}
            },
            XmlEvent::End { name } => {
                if name == "object" {
                    if let Some((id, mut d)) = cur.take() {
                        if !d.mesh.paint.is_empty() {
                            let n = d.mesh.tris.len();
                            d.mesh.paint.resize(n, String::new());
                        }
                        defs.insert((part.to_string(), id), d);
                    }
                }
            }
            XmlEvent::Text(_) => {}
        }
    }
    Ok(unit_mm)
}

fn flatten(defs: &HashMap<(String, u32), ObjDef>, key: &(String, u32), tr: &[f64; 12], out: &mut Mesh, depth: u32) {
    if depth > 16 {
        return;
    }
    let Some(d) = defs.get(key) else { return };
    if !d.mesh.tris.is_empty() {
        let mut m = d.mesh.clone();
        m.transform(tr);
        out.append(&m);
    }
    for (path, oid, ctr) in &d.components {
        let t = match ctr {
            Some(c) => mul_transform(tr, c),
            None => *tr,
        };
        flatten(defs, &(path.clone(), *oid), &t, out, depth + 1);
    }
}

pub fn load_3mf(data: &[u8]) -> Result<Model, String> {
    let mut cur = Cursor::new(data);
    let dir = makepad_zip_file::zip_read_central_directory(&mut cur).map_err(|e| format!("3MF zip: {e:?}"))?;
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for h in &dir.file_headers {
        let n = h.file_name.clone();
        if n.ends_with(".model") || n.ends_with("model_settings.config") {
            let bytes = h.extract(&mut cur).map_err(|e| format!("3MF {n}: {e:?}"))?;
            files.insert(n, bytes);
        }
    }
    let root = "3D/3dmodel.model".to_string();
    let mut defs = HashMap::new();
    let mut build = Vec::new();
    let mut unit_mm = 1.0;
    let mut names: Vec<&String> = files.keys().filter(|k| k.ends_with(".model")).collect();
    names.sort();
    for n in names {
        let text = std::str::from_utf8(&files[n]).map_err(|_| format!("3MF {n}: not UTF-8"))?;
        let mut b = Vec::new();
        let scale = parse_model_xml(text, n, &mut defs, &mut b)?;
        if *n == root {
            build = b;
            unit_mm = scale;
        }
    }
    // Per-object extruder from the vendor model settings.
    let mut extruders: HashMap<u32, u32> = HashMap::new();
    if let Some(cfg) = files.get("Metadata/model_settings.config") {
        if let Ok(text) = std::str::from_utf8(cfg) {
            let mut r = XmlReader::new(text);
            let mut obj: Option<u32> = None;
            let mut depth_part = 0;
            while let Some(Ok(ev)) = r.next_event() {
                match ev {
                    XmlEvent::Start { name: "object", attrs, .. } => {
                        obj = attr(&attrs, "id").and_then(|s| s.parse().ok());
                    }
                    XmlEvent::Start { name: "part", .. } => depth_part += 1,
                    XmlEvent::End { name: "part" } => depth_part -= 1,
                    XmlEvent::End { name: "object" } => obj = None,
                    XmlEvent::Start { name: "metadata", attrs, .. } => {
                        if depth_part == 0 && attr(&attrs, "key") == Some("extruder") {
                            if let (Some(o), Some(v)) = (obj, attr(&attrs, "value").and_then(|s| s.parse().ok())) {
                                extruders.insert(o, v);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let ident = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    let mut model = Model::default();
    for (oid, tr) in build {
        let key = (root.clone(), oid);
        let mut mesh = Mesh::default();
        flatten(&defs, &key, &tr.unwrap_or(ident), &mut mesh, 0);
        if mesh.tris.is_empty() {
            continue;
        }
        // Scale after the build transform so placement is in millimetres too.
        for v in &mut mesh.verts {
            for x in v {
                *x *= unit_mm;
            }
        }
        mesh.weld();
        mesh.fix_orientation();
        let name = defs.get(&key).map(|d| d.name.clone()).unwrap_or_default();
        model.objects.push(ModelObject { name, mesh, extruder: extruders.get(&oid).copied().unwrap_or(0) });
    }
    Ok(model)
}
