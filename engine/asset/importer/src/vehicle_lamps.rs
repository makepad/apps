//! A car model's lamps, found in its own geometry and written into it.
//!
//! Kit cars (Kenney's car-kit and friends) paint their lamps from a palette
//! texture: the headlights are yellow swatches on the nose's front faces, the
//! taillights red swatches on the tail's back faces, and nothing in the file
//! names them. [`annotate`] finds those faces by their colour, where they sit
//! and which way they face, and writes what the engine reads:
//!
//! - sockets `headlight_left`, `headlight_right`, `taillight_left`,
//!   `taillight_right` (nodes with `extras.MAKEPAD_socket`, model space, left
//!   = +X: the model faces +Z), at each lamp's middle;
//! - an emissive texture that lights exactly the lamp swatches (texels no
//!   other face uses), so the lenses glow;
//! - a lamp role (`extras.makepadShading.lamp`) on materials named as tail,
//!   brake or reversing lamps (a modelled car's own lens materials), so each
//!   car lights them from its brake and reverse state. A lens with no
//!   emission of its own gets one: the role scales it per car.
//!
//! Geometry, node order and every existing accessor are untouched: a
//! revision written from it keeps its derived files (AO, shadow SDF).

use makepad_asset_client::json::{self, Value};

/// The lamps found, model space (left = +X).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lamps {
    pub head: Option<[[f32; 3]; 2]>,
    pub tail: Option<[[f32; 3]; 2]>,
    /// Texels lit by the emissive texture.
    pub lit_texels: usize,
}

pub const SOCKETS: [&str; 4] = ["headlight_left", "headlight_right", "taillight_left", "taillight_right"];

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::F64(f) => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

fn get_usize(v: &Value, key: &str) -> Option<usize> {
    v.get(key).and_then(num).filter(|n| *n >= 0.0).map(|n| n as usize)
}

fn vec3(v: Option<&Value>, default: [f64; 3]) -> [f64; 3] {
    match v.and_then(Value::as_arr) {
        Some([x, y, z]) => [num(x).unwrap_or(default[0]), num(y).unwrap_or(default[1]), num(z).unwrap_or(default[2])],
        _ => default,
    }
}

/// Column-major 4x4 from a glTF node (matrix, or T·R·S).
fn node_matrix(node: &Value) -> [f64; 16] {
    if let Some(m) = node.get("matrix").and_then(Value::as_arr).filter(|m| m.len() == 16) {
        let mut out = [0.0; 16];
        for (o, v) in out.iter_mut().zip(m) {
            *o = num(v).unwrap_or(0.0);
        }
        return out;
    }
    let t = vec3(node.get("translation"), [0.0; 3]);
    let s = vec3(node.get("scale"), [1.0; 3]);
    let (x, y, z, w) = match node.get("rotation").and_then(Value::as_arr) {
        Some([x, y, z, w]) => (num(x).unwrap_or(0.0), num(y).unwrap_or(0.0), num(z).unwrap_or(0.0), num(w).unwrap_or(1.0)),
        _ => (0.0, 0.0, 0.0, 1.0),
    };
    let r = [
        1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + w * z), 2.0 * (x * z - w * y),
        2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x),
        2.0 * (x * z + w * y), 2.0 * (y * z - w * x), 1.0 - 2.0 * (x * x + y * y),
    ];
    [
        r[0] * s[0], r[1] * s[0], r[2] * s[0], 0.0,
        r[3] * s[1], r[4] * s[1], r[5] * s[1], 0.0,
        r[6] * s[2], r[7] * s[2], r[8] * s[2], 0.0,
        t[0], t[1], t[2], 1.0,
    ]
}

fn mul(a: &[f64; 16], b: &[f64; 16]) -> [f64; 16] {
    let mut o = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            o[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    o
}

fn apply(m: &[f64; 16], p: [f64; 3]) -> [f64; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

struct Glb {
    doc: Value,
    bin: Vec<u8>,
}

fn parse(glb: &[u8]) -> Option<Glb> {
    if glb.len() < 20 || &glb[0..4] != b"glTF" || &glb[16..20] != b"JSON" {
        return None;
    }
    let len = u32::from_le_bytes(glb[12..16].try_into().ok()?) as usize;
    let doc = json::parse(glb.get(20..20 + len)?).ok()?;
    let rest = 20 + len;
    let bin = match glb.get(rest..rest + 8) {
        Some(head) if &head[4..8] == b"BIN\0" => {
            let n = u32::from_le_bytes(head[0..4].try_into().ok()?) as usize;
            glb.get(rest + 8..rest + 8 + n)?.to_vec()
        }
        _ => Vec::new(),
    };
    Some(Glb { doc, bin })
}

fn write(doc: &Value, bin: &[u8]) -> Vec<u8> {
    let mut text = doc.to_json().into_bytes();
    while text.len() % 4 != 0 {
        text.push(b' ');
    }
    let mut bin = bin.to_vec();
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let total = 12 + 8 + text.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&text);
    if !bin.is_empty() {
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
    }
    out
}

/// Accessor `index` as f32 rows of `width` (float, or normalized ints for UVs)
/// / u32 (indices).
fn read_f32(g: &Glb, index: usize, width: usize) -> Option<Vec<f32>> {
    let a = g.doc.get("accessors")?.as_arr()?.get(index)?;
    let count = get_usize(a, "count")?;
    let view = g.doc.get("bufferViews")?.as_arr()?.get(get_usize(a, "bufferView")?)?;
    if get_usize(view, "buffer").unwrap_or(0) != 0 {
        return None;
    }
    let start = get_usize(view, "byteOffset").unwrap_or(0) + get_usize(a, "byteOffset").unwrap_or(0);
    let comp = get_usize(a, "componentType")?;
    let size = match comp { 5126 => 4, 5123 => 2, 5121 => 1, _ => return None };
    let stride = get_usize(view, "byteStride").unwrap_or(size * width);
    let normalized = a.get("normalized").and_then(Value::as_bool).unwrap_or(false);
    let mut out = Vec::with_capacity(count * width);
    for i in 0..count {
        for c in 0..width {
            let at = start + i * stride + c * size;
            let b = g.bin.get(at..at + size)?;
            out.push(match comp {
                5126 => f32::from_le_bytes(b.try_into().ok()?),
                5123 => { let v = u16::from_le_bytes(b.try_into().ok()?) as f32; if normalized { v / 65535.0 } else { v } }
                _ => { let v = b[0] as f32; if normalized { v / 255.0 } else { v } }
            });
        }
    }
    Some(out)
}

fn read_indices(g: &Glb, index: usize) -> Option<Vec<u32>> {
    let a = g.doc.get("accessors")?.as_arr()?.get(index)?;
    let count = get_usize(a, "count")?;
    let view = g.doc.get("bufferViews")?.as_arr()?.get(get_usize(a, "bufferView")?)?;
    let start = get_usize(view, "byteOffset").unwrap_or(0) + get_usize(a, "byteOffset").unwrap_or(0);
    let size = match get_usize(a, "componentType")? { 5125 => 4, 5123 => 2, 5121 => 1, _ => return None };
    (0..count).map(|i| {
        let b = g.bin.get(start + i * size..start + (i + 1) * size)?;
        Some(match size { 4 => u32::from_le_bytes(b.try_into().ok()?), 2 => u16::from_le_bytes(b.try_into().ok()?) as u32, _ => b[0] as u32 })
    }).collect()
}

/// Image `index` decoded to RGBA (any format the app's picture decoder reads).
fn read_image(g: &Glb, index: usize) -> Option<(Vec<u8>, u32, u32)> {
    let img = g.doc.get("images")?.as_arr()?.get(index)?;
    let view = g.doc.get("bufferViews")?.as_arr()?.get(get_usize(img, "bufferView")?)?;
    let start = get_usize(view, "byteOffset").unwrap_or(0);
    let bytes = g.bin.get(start..start + get_usize(view, "byteLength")?)?;
    let (w, h, rgba) = makepad_image_decode::decode_rgba(bytes).ok()?;
    Some((rgba, w as u32, h as u32))
}

#[derive(Clone, Copy, PartialEq)]
enum Lamp { Head, Tail }

/// What a palette colour says a lamp lens is, by its hue alone.
fn lamp_colour(rgb: [u8; 3]) -> Option<Lamp> {
    let [r, g, b] = rgb.map(|c| c as f32 / 255.0);
    // Warm yellow / amber-white headlight glass.
    if r > 0.88 && (0.62..0.95).contains(&g) && b < 0.5 && r - b > 0.45 {
        return Some(Lamp::Head);
    }
    // Deep red tail glass (a red or orange body is lighter in green).
    if r > 0.62 && g < 0.33 && b < 0.36 && r - g > 0.42 {
        return Some(Lamp::Tail);
    }
    None
}

struct Tri {
    centre: [f64; 3],
    normal: [f64; 3],
    area: f64,
    texel: Option<(usize, u32, u32)>,
    rgb: Option<[u8; 3]>,
}

/// Find a car model's lamps and write their sockets and glow into it.
/// `Ok(None)`: nothing to write (no lamp-coloured faces where lamps sit, or
/// the sockets are already there). The model faces +Z (glTF's front).
pub fn annotate(glb: &[u8]) -> Result<Option<(Vec<u8>, Lamps)>, String> {
    let mut g = parse(glb).ok_or("not a GLB")?;
    // Only a car: it has wheels (named, or bound as vehicle wheels). A
    // yellow-nosed prop never gets headlights.
    if wheel_count(&g.doc) < 3 {
        return Ok(None);
    }
    // An earlier bake of ours is undone first (its sockets, its glow
    // texture), so a better detector rewrites it; the same answer again is
    // no change. Sockets a modeller authored are theirs: left alone.
    let ours = strip_previous(&mut g);
    let authored = !ours && g.doc.get("nodes").and_then(Value::as_arr).is_some_and(|n| n.iter().any(|n| n.get("name").and_then(Value::as_str).is_some_and(|name| SOCKETS.contains(&name))));
    let roles = tag_lamp_materials(&mut g.doc);
    let lamps = if authored { None } else { annotate_doc(&mut g)? };
    if lamps.is_none() && roles == 0 {
        return Ok(None);
    }
    let bin_len = g.bin.len().div_ceil(4) * 4;
    if let Some(Value::Arr(buffers)) = field_mut(&mut g.doc, "buffers") {
        if let Some(b) = buffers.get_mut(0) { set_field(b, "byteLength", Value::Int(bin_len as i64)); }
    }
    let out = write(&g.doc, &g.bin);
    Ok((out.as_slice() != glb).then(|| (out, lamps.unwrap_or_default())))
}

fn is_wheel_name(name: &str) -> bool {
    let s = name.to_ascii_lowercase();
    s.contains("wheel") || s.contains("tire") || s.contains("tyre")
}

fn wheel_count(doc: &Value) -> usize {
    doc.get("nodes").and_then(Value::as_arr).map_or(0, |nodes| nodes.iter().filter(|n| {
        n.get("name").and_then(Value::as_str).is_some_and(is_wheel_name)
            || n.get("extras").and_then(|e| e.get("kind")).and_then(Value::as_str) == Some("vehicle_wheel")
    }).count())
}

/// The lamp role a material's name gives it: a tail, brake or stop lamp
/// is a tail lamp (dim running light, full when braking), a reversing or
/// back-up lamp a reversing lamp. Only names that say lamp or light: a
/// brake disc or calliper is no lamp.
fn lamp_role_of_name(name: &str) -> Option<&'static str> {
    let n: String = name.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
    let lamp = n.contains("light") || n.contains("lamp");
    if !lamp { return None; }
    if ["reverse", "reversing", "backup"].iter().any(|k| n.contains(k)) {
        Some("reverse")
    } else if ["tail", "brake", "stop", "rear"].iter().any(|k| n.contains(k)) {
        Some("tail")
    } else {
        None
    }
}

/// Give materials named as signal lamps their role (`makepadShading.lamp`),
/// and an emission to scale when they have none (glTF's default emissive
/// factor is black, which no role could light). Returns how many carry a
/// role; tagging again changes nothing.
fn tag_lamp_materials(doc: &mut Value) -> usize {
    let mut tagged = 0;
    let mut strength_used = false;
    if let Some(Value::Arr(mats)) = field_mut(doc, "materials") {
        for m in mats.iter_mut() {
            let Some(role) = m.get("name").and_then(Value::as_str).and_then(lamp_role_of_name) else { continue };
            tagged += 1;
            let lit = m.get("emissiveFactor").and_then(Value::as_arr).is_some_and(|f| f.iter().any(|v| num(v).is_some_and(|v| v > 0.0)));
            if !lit {
                // A lens fully lit: red stop lamp or white reversing lamp,
                // bright enough to bloom (the role dims it per car).
                let colour = if role == "reverse" { [1.0, 0.96, 0.9] } else { [1.0, 0.08, 0.05] };
                set_field(m, "emissiveFactor", Value::Arr(colour.iter().map(|v| Value::F64(*v)).collect()));
                // Fields are only ever added, never moved, so a second
                // pass writes the same bytes.
                if !matches!(m.get("extensions"), Some(Value::Obj(_))) { set_field(m, "extensions", json::obj(vec![])); }
                let extensions = field_mut(m, "extensions").expect("extensions set above");
                if extensions.get("KHR_materials_emissive_strength").is_none() {
                    set_field(extensions, "KHR_materials_emissive_strength", json::obj(vec![("emissiveStrength", Value::F64(4.0))]));
                }
                strength_used = true;
            }
            if !matches!(m.get("extras"), Some(Value::Obj(_))) { set_field(m, "extras", json::obj(vec![])); }
            let extras = field_mut(m, "extras").expect("extras set above");
            if !matches!(extras.get("makepadShading"), Some(Value::Obj(_))) { set_field(extras, "makepadShading", json::obj(vec![])); }
            let shading = field_mut(extras, "makepadShading").expect("shading set above");
            if shading.get("lamp").and_then(Value::as_str) != Some(role) { set_field(shading, "lamp", json::s(role)); }
        }
    }
    if strength_used {
        let used = arr_mut(doc, "extensionsUsed");
        if !used.iter().any(|e| e.as_str() == Some("KHR_materials_emissive_strength")) {
            used.push(json::s("KHR_materials_emissive_strength"));
        }
    }
    tagged
}

/// Remove what a previous [`annotate`] appended: socket nodes marked by it
/// (`extras.MAKEPAD_lamp`) and its `lamps` emissive images/textures.
fn strip_previous(g: &mut Glb) -> bool {
    let doc = &mut g.doc;
    // Marked, or (the first bakes, unmarked) a lamp socket with a bare
    // translation, as this writer alone leaves one.
    let marked = |n: &Value| n.get("extras").and_then(|e| e.get("MAKEPAD_lamp")).and_then(Value::as_bool) == Some(true)
        || (n.get("name").and_then(Value::as_str).is_some_and(|name| SOCKETS.contains(&name))
            && n.get("rotation").is_none() && n.get("scale").is_none() && n.get("mesh").is_none() && n.get("children").is_none());
    let Some(nodes) = doc.get("nodes").and_then(Value::as_arr).map(|n| n.to_vec()) else { return false };
    let keep = nodes.iter().rposition(|n| !marked(n)).map_or(0, |i| i + 1);
    if keep == nodes.len() { return false; }
    arr_mut(doc, "nodes").truncate(keep);
    if let Some(Value::Arr(scenes)) = field_mut(doc, "scenes") {
        for scene in scenes.iter_mut() {
            arr_mut(scene, "nodes").retain(|r| num(r).is_some_and(|n| (n as usize) < keep));
        }
    }
    // The glow textures were appended last, image and texture together.
    let images: Vec<Value> = doc.get("images").and_then(Value::as_arr).map(|i| i.to_vec()).unwrap_or_default();
    let first_lamp_image = images.iter().position(|i| i.get("name").and_then(Value::as_str) == Some("lamps")).unwrap_or(images.len());
    // Their PNG bytes were the last views and the end of the BIN chunk.
    if let Some(view) = images.get(first_lamp_image).and_then(|i| get_usize(i, "bufferView")) {
        let offset = doc.get("bufferViews").and_then(Value::as_arr).and_then(|v| v.get(view)).and_then(|v| get_usize(v, "byteOffset"));
        if let Some(offset) = offset.filter(|o| *o <= g.bin.len()) {
            arr_mut(doc, "bufferViews").truncate(view);
            g.bin.truncate(offset);
        }
    }
    arr_mut(doc, "images").truncate(first_lamp_image);
    let textures: Vec<Value> = doc.get("textures").and_then(Value::as_arr).map(|t| t.to_vec()).unwrap_or_default();
    let first_lamp_texture = textures.iter().position(|t| get_usize(t, "source").is_some_and(|s| s >= first_lamp_image)).unwrap_or(textures.len());
    arr_mut(doc, "textures").truncate(first_lamp_texture);
    if let Some(Value::Arr(mats)) = field_mut(doc, "materials") {
        for m in mats.iter_mut() {
            if m.get("emissiveTexture").and_then(|t| get_usize(t, "index")).is_some_and(|i| i >= first_lamp_texture) {
                if let Value::Obj(f) = m { f.retain(|(k, _)| k != "emissiveTexture" && k != "emissiveFactor"); }
            }
        }
    }
    true
}

fn annotate_doc(g: &mut Glb) -> Result<Option<Lamps>, String> {
    let nodes: Vec<Value> = g.doc.get("nodes").and_then(Value::as_arr).map(|n| n.to_vec()).unwrap_or_default();
    // World matrices from the scene roots down.
    let mut world: Vec<Option<[f64; 16]>> = vec![None; nodes.len()];
    let roots: Vec<usize> = g.doc.get("scenes").and_then(Value::as_arr).and_then(|s| s.get(get_usize(&g.doc, "scene").unwrap_or(0)))
        .and_then(|s| s.get("nodes")).and_then(Value::as_arr).map(|r| r.iter().filter_map(num).map(|n| n as usize).collect())
        .unwrap_or_else(|| (0..nodes.len()).collect());
    let identity = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
    let mut stack: Vec<(usize, [f64; 16], bool)> = roots.into_iter().map(|r| (r, identity, false)).collect();
    // `wheel` = under a node named like a wheel (its faces are no lamp).
    let mut wheel_node = vec![false; nodes.len()];
    while let Some((n, parent, wheel)) = stack.pop() {
        let Some(node) = nodes.get(n) else { continue };
        if world[n].is_some() { continue; }
        let m = mul(&parent, &node_matrix(node));
        world[n] = Some(m);
        let wheel = wheel || node.get("name").and_then(Value::as_str).is_some_and(is_wheel_name);
        wheel_node[n] = wheel;
        for c in node.get("children").and_then(Value::as_arr).map(|c| c.to_vec()).unwrap_or_default() {
            if let Some(c) = num(&c) { stack.push((c as usize, m, wheel)); }
        }
    }
    let mut images: Vec<Option<(Vec<u8>, u32, u32)>> = Vec::new();
    let image_of_material = |doc: &Value, material: usize| -> Option<usize> {
        let tex = doc.get("materials")?.as_arr()?.get(material)?.get("pbrMetallicRoughness")?.get("baseColorTexture")?;
        let texture = doc.get("textures")?.as_arr()?.get(get_usize(tex, "index")?)?;
        get_usize(texture, "source")
    };
    let mut tris: Vec<Tri> = Vec::new();
    let mut materials_by_image: Vec<(usize, usize)> = Vec::new();
    for (n, node) in nodes.iter().enumerate() {
        let (Some(mesh), Some(m)) = (get_usize(node, "mesh"), world[n]) else { continue };
        if wheel_node[n] { continue; }
        let Some(prims) = g.doc.get("meshes").and_then(Value::as_arr).and_then(|ms| ms.get(mesh)).and_then(|m| m.get("primitives")).and_then(Value::as_arr).map(|p| p.to_vec()) else { continue };
        for prim in prims {
            let Some(attrs) = prim.get("attributes") else { continue };
            let Some(pos) = get_usize(attrs, "POSITION").and_then(|a| read_f32(&g, a, 3)) else { continue };
            let uv = get_usize(attrs, "TEXCOORD_0").and_then(|a| read_f32(&g, a, 2));
            let idx = match get_usize(&prim, "indices") {
                Some(a) => read_indices(&g, a).ok_or("unreadable indices")?,
                None => (0..(pos.len() / 3) as u32).collect(),
            };
            let image = get_usize(&prim, "material").and_then(|mat| {
                let img = image_of_material(&g.doc, mat)?;
                if !materials_by_image.contains(&(mat, img)) { materials_by_image.push((mat, img)); }
                Some(img)
            });
            if let Some(img) = image {
                if images.len() <= img { images.resize(img + 1, None); }
                if images[img].is_none() { images[img] = read_image(&g, img); }
            }
            for t in idx.chunks_exact(3) {
                let p = |i: u32| { let i = i as usize * 3; apply(&m, [pos[i] as f64, pos[i + 1] as f64, pos[i + 2] as f64]) };
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let cr = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
                let len = (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt();
                if len < 1e-12 { continue; }
                let centre = [(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0, (a[2] + b[2] + c[2]) / 3.0];
                let (mut texel, mut rgb) = (None, None);
                if let (Some(uv), Some(img)) = (&uv, image) {
                    if let Some(Some((px, w, h))) = images.get(img) {
                        let u = (uv[t[0] as usize * 2] + uv[t[1] as usize * 2] + uv[t[2] as usize * 2]) / 3.0;
                        let v = (uv[t[0] as usize * 2 + 1] + uv[t[1] as usize * 2 + 1] + uv[t[2] as usize * 2 + 1]) / 3.0;
                        let x = ((u.rem_euclid(1.0) * *w as f32) as u32).min(w - 1);
                        let y = ((v.rem_euclid(1.0) * *h as f32) as u32).min(h - 1);
                        let at = ((y * w + x) * 4) as usize;
                        rgb = Some([px[at], px[at + 1], px[at + 2]]);
                        texel = Some((img, x, y));
                    }
                }
                tris.push(Tri { centre, normal: [cr[0] / len, cr[1] / len, cr[2] / len], area: len * 0.5, texel, rgb });
            }
        }
    }
    if tris.is_empty() { return Ok(None); }
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for t in &tris { for d in 0..3 { lo[d] = lo[d].min(t.centre[d]); hi[d] = hi[d].max(t.centre[d]); } }
    let length = (hi[2] - lo[2]).max(1e-6);
    let mid_x = (lo[0] + hi[0]) * 0.5;
    // A lamp face: lamp-coloured, at its end of the car, facing out of it.
    let lamp_of = |t: &Tri| -> Option<Lamp> {
        let kind = lamp_colour(t.rgb?)?;
        match kind {
            Lamp::Head => (t.normal[2] > 0.3 && t.centre[2] > hi[2] - 0.22 * length).then_some(Lamp::Head),
            Lamp::Tail => (t.normal[2] < -0.3 && t.centre[2] < lo[2] + 0.22 * length).then_some(Lamp::Tail),
        }
    };
    let pair = |kind: Lamp| -> Option<[[f32; 3]; 2]> {
        let mut sides = [([0.0f64; 3], 0.0f64); 2];
        for t in tris.iter().filter(|t| lamp_of(t) == Some(kind)) {
            let side = if t.centre[0] >= mid_x { 0 } else { 1 };
            for d in 0..3 { sides[side].0[d] += t.centre[d] * t.area; }
            sides[side].1 += t.area;
        }
        let point = |s: &([f64; 3], f64)| (s.1 > 0.0).then(|| [(s.0[0] / s.1) as f32, (s.0[1] / s.1) as f32, (s.0[2] / s.1) as f32]);
        match (point(&sides[0]), point(&sides[1])) {
            (Some(l), Some(r)) => Some([l, r]),
            // One lamp across the middle (a light bar): a pair either side.
            (Some(one), None) | (None, Some(one)) => {
                let off = ((one[0] as f64 - mid_x).abs().max((hi[0] - lo[0]) * 0.3)) as f32;
                Some([[mid_x as f32 + off, one[1], one[2]], [mid_x as f32 - off, one[1], one[2]]])
            }
            (None, None) => None,
        }
    };
    let head = pair(Lamp::Head);
    let tail = pair(Lamp::Tail);
    if head.is_none() && tail.is_none() { return Ok(None); }

    // Emissive: the lamp swatches no other face uses.
    let mut lamp_texels: Vec<(usize, u32, u32)> = tris.iter().filter(|t| lamp_of(t).is_some()).filter_map(|t| t.texel).collect();
    // A lamp is a little box: its sides and rim share the lens colour but
    // face sideways, so they fail the facing test. Faces of a lamp colour
    // within a hand's width of a lamp face are that lamp (they glow with
    // it); only the same colour elsewhere on the car (a light bar, a stripe,
    // a yellow cab) keeps a swatch dark.
    let lamp_centres: Vec<[f64; 3]> = tris.iter().filter(|t| lamp_of(t).is_some()).map(|t| t.centre).collect();
    let near_lamp = |t: &Tri| t.rgb.and_then(lamp_colour).is_some() && lamp_centres.iter().any(|c| {
        let d = [t.centre[0] - c[0], t.centre[1] - c[1], t.centre[2] - c[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < 0.12 * length.max(1.0)
    });
    let other: std::collections::HashSet<(usize, u32, u32)> = tris.iter().filter(|t| lamp_of(t).is_none() && !near_lamp(t)).filter_map(|t| t.texel).collect();
    lamp_texels.sort_unstable();
    lamp_texels.dedup();
    let mut lit = 0;
    let mut emissive_images: Vec<(usize, Vec<u8>, u32, u32)> = Vec::new();
    for (img, x, y) in lamp_texels {
        let Some(Some((px, w, h))) = images.get(img) else { continue };
        let seed = { let at = ((y * w + x) * 4) as usize; [px[at], px[at + 1], px[at + 2]] };
        // The whole same-coloured swatch around the texel, unless another
        // face samples any of it.
        let mut region = Vec::new();
        let mut todo = vec![(x, y)];
        let mut seen = std::collections::HashSet::new();
        while let Some((cx, cy)) = todo.pop() {
            if !seen.insert((cx, cy)) || region.len() > 1025 { continue; }
            let at = ((cy * w + cx) * 4) as usize;
            if [px[at], px[at + 1], px[at + 2]] != seed { continue; }
            region.push((cx, cy));
            if cx > 0 { todo.push((cx - 1, cy)); }
            if cy > 0 { todo.push((cx, cy - 1)); }
            if cx + 1 < *w { todo.push((cx + 1, cy)); }
            if cy + 1 < *h { todo.push((cx, cy + 1)); }
        }
        if region.iter().any(|&(rx, ry)| other.contains(&(img, rx, ry))) { continue; }
        // A swatch is a small block; a big same-coloured field is shared
        // paint that only happens to be unsampled: light the texel alone.
        if region.len() > 1024 { region = vec![(x, y)]; }
        let slot = match emissive_images.iter().position(|e| e.0 == img) {
            Some(s) => s,
            None => { emissive_images.push((img, vec![0u8; (*w * *h * 4) as usize], *w, *h)); emissive_images.len() - 1 }
        };
        for (rx, ry) in region {
            let at = ((ry * w + rx) * 4) as usize;
            let out = &mut emissive_images[slot].1;
            out[at..at + 3].copy_from_slice(&seed);
            out[at + 3] = 255;
            lit += 1;
        }
    }

    // Write: sockets, then the emissive textures.
    let lamps = Lamps { head, tail, lit_texels: lit };
    let mut new_nodes = Vec::new();
    for (names, points) in [(&SOCKETS[0..2], head), (&SOCKETS[2..4], tail)] {
        let Some(points) = points else { continue };
        for (name, p) in names.iter().zip(points) {
            new_nodes.push(json::obj(vec![
                ("name", json::s(*name)),
                ("translation", Value::Arr(p.iter().map(|v| Value::F64(*v as f64)).collect())),
                ("extras", json::obj(vec![("MAKEPAD_socket", Value::Bool(true)), ("MAKEPAD_lamp", Value::Bool(true))])),
            ]));
        }
    }
    let first = nodes.len();
    let added = new_nodes.len();
    push_all(&mut g.doc, "nodes", new_nodes);
    let scene = get_usize(&g.doc, "scene").unwrap_or(0);
    if let Some(Value::Arr(scenes)) = field_mut(&mut g.doc, "scenes") {
        if let Some(s) = scenes.get_mut(scene) {
            let roots = arr_mut(s, "nodes");
            roots.extend((first..first + added).map(|i| Value::Int(i as i64)));
        }
    }
    for (img, rgba, w, h) in emissive_images {
        let png = crate::png::encode_png_rgba(&rgba, w, h)?;
        while g.bin.len() % 4 != 0 { g.bin.push(0); }
        let offset = g.bin.len();
        g.bin.extend_from_slice(&png);
        let view = arr_len(&g.doc, "bufferViews");
        push_all(&mut g.doc, "bufferViews", vec![json::obj(vec![("buffer", Value::Int(0)), ("byteOffset", Value::Int(offset as i64)), ("byteLength", Value::Int(png.len() as i64))])]);
        let image = arr_len(&g.doc, "images");
        push_all(&mut g.doc, "images", vec![json::obj(vec![("bufferView", Value::Int(view as i64)), ("mimeType", json::s("image/png")), ("name", json::s("lamps"))])]);
        let sampler = g.doc.get("textures").and_then(Value::as_arr).and_then(|t| t.iter().find(|t| get_usize(t, "source") == Some(img))).and_then(|t| get_usize(t, "sampler"));
        let texture = arr_len(&g.doc, "textures");
        let mut tex = vec![("source", Value::Int(image as i64))];
        if let Some(s) = sampler { tex.push(("sampler", Value::Int(s as i64))); }
        push_all(&mut g.doc, "textures", vec![json::obj(tex)]);
        for &(mat, mimg) in &materials_by_image {
            if mimg != img { continue; }
            if let Some(Value::Arr(mats)) = field_mut(&mut g.doc, "materials") {
                if let Some(m) = mats.get_mut(mat) {
                    set_field(m, "emissiveTexture", json::obj(vec![("index", Value::Int(texture as i64))]));
                    set_field(m, "emissiveFactor", Value::Arr(vec![Value::F64(1.0); 3]));
                }
            }
        }
    }
    Ok(Some(lamps))
}

fn field_mut<'a>(v: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    match v {
        Value::Obj(f) => f.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn set_field(v: &mut Value, key: &str, value: Value) {
    if !matches!(v, Value::Obj(_)) { *v = Value::Obj(Vec::new()); }
    if let Value::Obj(f) = v {
        f.retain(|(k, _)| k != key);
        f.push((key.to_string(), value));
    }
}

fn arr_len(doc: &Value, key: &str) -> usize {
    doc.get(key).and_then(Value::as_arr).map_or(0, <[Value]>::len)
}

fn arr_mut<'a>(doc: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    if !matches!(field_mut(doc, key), Some(Value::Arr(_))) { set_field(doc, key, Value::Arr(Vec::new())); }
    match field_mut(doc, key) { Some(Value::Arr(a)) => a, _ => unreachable!() }
}

fn push_all(doc: &mut Value, key: &str, values: Vec<Value>) {
    arr_mut(doc, key).extend(values);
}

/// What [`bake_tree`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BakeStats {
    pub written: usize,
    pub fresh: usize,
    pub removed: usize,
}

/// `<stem>.lamps.glb` beside every car GLB under `root` (the pack import
/// publishes it as that mesh's render GLB). A bake newer than its GLB is
/// kept; one whose GLB no longer yields lamps is removed. A folder that
/// cannot be written keeps importing the source GLBs.
pub fn bake_tree(root: &std::path::Path) -> BakeStats {
    let mut stats = BakeStats::default();
    for glb in crate::ao_bake::collect_glbs(root) {
        let name = glb.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
        if name.ends_with(".lamps.glb") { continue; }
        let Some(stem) = glb.file_stem().and_then(|s| s.to_str()) else { continue };
        let out = glb.with_file_name(format!("{stem}.lamps.glb"));
        let newer = |a: &std::path::Path, b: &std::path::Path| -> bool {
            match (std::fs::metadata(a).and_then(|m| m.modified()), std::fs::metadata(b).and_then(|m| m.modified())) {
                (Ok(a), Ok(b)) => a >= b,
                _ => false,
            }
        };
        if out.is_file() && newer(&out, &glb) {
            stats.fresh += 1;
            continue;
        }
        let Ok(bytes) = std::fs::read(&glb) else { continue };
        match annotate(&bytes) {
            Ok(Some((baked, _))) => {
                if std::fs::write(&out, baked).is_ok() { stats.written += 1; }
            }
            _ => {
                if out.is_file() && std::fs::remove_file(&out).is_ok() { stats.removed += 1; }
            }
        }
    }
    stats
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A box car: one quad facing +Z at the nose painted with texel 0
    /// (yellow), one facing -Z at the tail with texel 1 (red), a side quad
    /// with texel 2 (body grey); a 3x1 palette texture.
    pub(crate) fn kit_car() -> Vec<u8> {
        let png = crate::png::encode_png_rgba(&[255, 210, 45, 255, 220, 60, 60, 255, 110, 112, 133, 255], 3, 1).unwrap();
        let u = |i: f32| (i + 0.5) / 3.0;
        // (positions, uvs) of 3 quads: nose (left half and right half), tail, side.
        let quads: [([[f32; 3]; 4], f32); 5] = [
            ([[0.2, 0.5, 2.0], [0.6, 0.5, 2.0], [0.6, 0.7, 2.0], [0.2, 0.7, 2.0]], u(0.0)),
            ([[-0.6, 0.5, 2.0], [-0.2, 0.5, 2.0], [-0.2, 0.7, 2.0], [-0.6, 0.7, 2.0]], u(0.0)),
            ([[0.6, 0.5, -2.0], [-0.6, 0.5, -2.0], [-0.6, 0.7, -2.0], [0.6, 0.7, -2.0]], u(1.0)),
            ([[0.9, 0.0, -2.0], [0.9, 0.0, 2.0], [0.9, 1.0, 2.0], [0.9, 1.0, -2.0]], u(2.0)),
            ([[-0.9, 0.0, 2.0], [-0.9, 0.0, -2.0], [-0.9, 1.0, -2.0], [-0.9, 1.0, 2.0]], u(2.0)),
        ];
        let mut pos = Vec::new();
        let mut uv = Vec::new();
        let mut idx: Vec<u16> = Vec::new();
        for (q, (corners, u)) in quads.iter().enumerate() {
            for c in corners { pos.extend_from_slice(c); uv.extend_from_slice(&[*u, 0.5]); }
            let b = (q * 4) as u16;
            idx.extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
        }
        let mut bin = Vec::new();
        for v in &pos { bin.extend_from_slice(&v.to_le_bytes()); }
        let uv_at = bin.len();
        for v in &uv { bin.extend_from_slice(&v.to_le_bytes()); }
        let idx_at = bin.len();
        for v in &idx { bin.extend_from_slice(&v.to_le_bytes()); }
        while bin.len() % 4 != 0 { bin.push(0); }
        let png_at = bin.len();
        bin.extend_from_slice(&png);
        while bin.len() % 4 != 0 { bin.push(0); }
        let doc = format!(r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
            "nodes":[{{"name":"body","mesh":0}},{{"name":"wheel-front-left"}},{{"name":"wheel-front-right"}},{{"name":"wheel-back-left"}},{{"name":"wheel-back-right"}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"TEXCOORD_0":1}},"indices":2,"material":0}}]}}],
            "materials":[{{"pbrMetallicRoughness":{{"baseColorTexture":{{"index":0}}}}}}],
            "textures":[{{"source":0}}],"images":[{{"bufferView":3,"mimeType":"image/png"}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":{n},"type":"VEC3","min":[-0.9,0,-2],"max":[0.9,1,2]}},
                {{"bufferView":1,"componentType":5126,"count":{n},"type":"VEC2"}},
                {{"bufferView":2,"componentType":5123,"count":{i},"type":"SCALAR"}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{pl}}},{{"buffer":0,"byteOffset":{uv_at},"byteLength":{ul}}},
                {{"buffer":0,"byteOffset":{idx_at},"byteLength":{il}}},{{"buffer":0,"byteOffset":{png_at},"byteLength":{gl}}}],
            "buffers":[{{"byteLength":{bl}}}]}}"#,
            n = pos.len() / 3, i = idx.len(), pl = pos.len() * 4, ul = uv.len() * 4, il = idx.len() * 2, gl = png.len(), bl = bin.len());
        let doc = json::parse(doc.as_bytes()).unwrap();
        write(&doc, &bin)
    }

    #[test]
    fn a_kit_cars_palette_lamps_become_sockets_and_glow() {
        let (out, lamps) = annotate(&kit_car()).unwrap().expect("lamps found");
        let [hl, hr] = lamps.head.expect("headlights");
        assert!(hl[0] > 0.3 && hr[0] < -0.3 && (hl[2] - 2.0).abs() < 1e-4, "{hl:?} {hr:?}");
        let [tl, tr] = lamps.tail.expect("taillights: one bar split either side");
        assert!(tl[0] > 0.0 && tr[0] < 0.0 && (tl[2] + 2.0).abs() < 1e-4, "{tl:?} {tr:?}");
        assert_eq!(lamps.lit_texels, 2, "the yellow and red swatches glow, the grey body does not");
        let g = parse(&out).unwrap();
        let names: Vec<_> = g.doc.get("nodes").unwrap().as_arr().unwrap().iter().filter_map(|n| n.get("name").and_then(Value::as_str).map(str::to_string)).collect();
        for s in SOCKETS { assert!(names.iter().any(|n| n == s), "{s} in {names:?}"); }
        assert!(g.doc.get("materials").unwrap().as_arr().unwrap()[0].get("emissiveTexture").is_some());
        // The geometry's accessors read back unchanged, and a second pass
        // finds the sockets already written.
        assert_eq!(read_f32(&g, 0, 3), read_f32(&parse(&kit_car()).unwrap(), 0, 3));
        assert!(annotate(&out).unwrap().is_none(), "the same answer again is no change");
    }

    #[test]
    fn named_lamp_materials_get_their_roles_and_a_lens_to_light() {
        // The kit car with its body material named as a modelled car's tail
        // lens, plus a reversing lens and a brake calliper.
        let g = parse(&kit_car()).unwrap();
        let mut doc = g.doc.clone();
        if let Some(Value::Arr(mats)) = field_mut(&mut doc, "materials") {
            set_field(&mut mats[0], "name", json::s("TailLight_Glass"));
            mats.push(json::obj(vec![("name", json::s("Reverse Lamp")), ("emissiveFactor", Value::Arr(vec![Value::F64(0.9); 3]))]));
            mats.push(json::obj(vec![("name", json::s("brake_caliper"))]));
        }
        let (out, _) = annotate(&write(&doc, &g.bin)).unwrap().expect("roles written");
        let back = parse(&out).unwrap();
        let mats = back.doc.get("materials").unwrap().as_arr().unwrap();
        let role = |m: &Value| m.get("extras").and_then(|e| e.get("makepadShading")).and_then(|s| s.get("lamp")).and_then(Value::as_str).map(str::to_string);
        assert_eq!(role(&mats[0]).as_deref(), Some("tail"));
        assert_eq!(role(&mats[1]).as_deref(), Some("reverse"));
        assert_eq!(role(&mats[2]), None, "a calliper is no lamp");
        // The reversing lens keeps its own emission; the tail lens had the
        // palette glow written, which the role now scales per car.
        assert_eq!(mats[1].get("emissiveFactor").unwrap().as_arr().unwrap().len(), 3);
        assert!(mats[1].get("extensions").is_none());
        assert!(annotate(&out).unwrap().is_none(), "tagging again changes nothing");
    }

    #[test]
    fn a_car_without_lamp_colours_is_left_alone() {
        let mut glb = kit_car();
        // Repaint the palette grey: no lamps anywhere.
        let g = parse(&glb).unwrap();
        let png = crate::png::encode_png_rgba(&[110, 112, 133, 255].repeat(3), 3, 1).unwrap();
        let mut bin = g.bin.clone();
        let at = get_usize(&g.doc.get("bufferViews").unwrap().as_arr().unwrap()[3], "byteOffset").unwrap();
        bin.truncate(at);
        bin.extend_from_slice(&png);
        let mut doc = g.doc.clone();
        if let Some(Value::Arr(views)) = field_mut(&mut doc, "bufferViews") { set_field(&mut views[3], "byteLength", Value::Int(png.len() as i64)); }
        glb = write(&doc, &bin);
        assert!(annotate(&glb).unwrap().is_none());
    }
}
