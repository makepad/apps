//! The evaluated plate as a viewport scene: the printed parts in their
//! filament colours (tinted red when the bed check blocks them) on the
//! printer's plate: a heated bed under a textured build plate, the build
//! volume's edges, and the nozzle-zone overlay (single-nozzle strips,
//! keep-outs, the lower nozzle's ceiling) that the view shows only on request.
//! The plate's grain and rim are drawn here; the viewport's composite draws
//! its 10 / 50 mm grid lines, crisp at every zoom.
//!
//! Scene objects are named so the app can tell them apart: every printer
//! object starts with [`CONTEXT`] (zones with [`ZONE`]) and each part is named
//! by its model object, which is unique.
//!
//! The Fab viewport is Z up in metres; the plate is Z up in millimetres, so
//! the mapping is a plain scale.

use fab::document::{DocumentBuilder, Mesh, MeshInstance, Object, ObjectId, PbrMaterial, MaterialId, Texture, TextureId, TextureSlot};
use fab::model::{Handedness, Scene, Units, UpAxis};
use fab_model::eval::Evaluated;
use fab_plate::machine::{Machine, Rect};
use makepad_widgets::makepad_platform::thread::TaskPool;
use std::sync::Arc;

const MM: f32 = 0.001;
/// The bed slab's thickness under the plate (mm).
pub(crate) const BED_THICKNESS: f32 = 3.0;
/// The build plate on top of it (mm).
const PLATE_THICKNESS: f32 = 1.2;
/// Build-volume edge beams (mm square).
const EDGE: f32 = 0.5;
/// Name prefixes the shell reads (never listed or picked; the zone overlay).
use fab::api::{CONTEXT_PREFIX as CONTEXT, FILAMENT_ROUGHNESS, ZONE_PREFIX as ZONE};
/// Whether scenes carry the nozzle-zone overlay (the view's Nozzle zones
/// chip). Read by every build, on whichever thread builds.
pub static SHOW_ZONES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Texels per millimetre of the plate texture.
const TEXELS_PER_MM: f32 = 3.0;

fn material(b: &mut DocumentBuilder, name: &str, color: [f32; 4], roughness: f32) -> MaterialId {
    let mut m = PbrMaterial::new(MaterialId::default(), name);
    m.base_color = color;
    m.roughness = roughness;
    b.add_material(m)
}

/// A placed part takes its tint, transmission and roughness from a translucent print swatch.
/// Other colours keep the placement's display colour; inserts keep their fade. A part shown
/// without printing keeps its own material: the views draw it as a ghost, and Mechanics' closed
/// box solid in its case.
fn part_material(b: &mut DocumentBuilder, ev: &Evaluated, i: usize, mut color: [f32; 4]) -> MaterialId {
    if ev.plate.excluded.get(i.wrapping_sub(ev.plate.parts.len())).is_some_and(|x| x.insert.is_some()) {
        color[3] = 0.18;
        return material(b, "Excluded from print", color, 0.65);
    }
    let name = ev.part_color(i).unwrap_or("Part");
    if let Some(swatch) = fab_materials::swatch::swatch(name).filter(|s| s.colour.translucency > 0.0) {
        let [red, green, blue] = swatch.colour.srgb;
        let tint = makepad_render::srgb_to_linear(makepad_widgets::vec3f(red, green, blue));
        let mut m = PbrMaterial::new(MaterialId::default(), name);
        m.base_color = [tint.x, tint.y, tint.z, 1.0];
        m.transmission = swatch.colour.translucency;
        m.ior = 1.5;
        m.roughness = swatch.roughness.unwrap_or(FILAMENT_ROUGHNESS);
        return b.add_material(m);
    }
    material(b, name, color, FILAMENT_ROUGHNESS)
}

fn object(b: &mut DocumentBuilder, name: &str, mesh: Mesh, tag: &str) {
    let mesh_id = b.add_mesh(mesh);
    let mut o = Object::new(ObjectId::default(), name);
    o.tags.push(tag.into());
    o.meshes.push(MeshInstance::new(mesh_id));
    b.add_object(o);
}

/// An axis-aligned box mesh in metres from millimetre corners, flat-shaded.
fn box_mesh(name: &str, min: [f32; 3], max: [f32; 3], material: MaterialId) -> Mesh {
    let mut positions = Vec::with_capacity(24);
    let mut normals = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    let p = |x: usize, y: usize, z: usize| [if x == 0 { min[0] } else { max[0] } * MM, if y == 0 { min[1] } else { max[1] } * MM, if z == 0 { min[2] } else { max[2] } * MM];
    // (normal, four corners counter-clockwise seen from outside)
    let faces: [([f32; 3], [[usize; 3]; 4]); 6] = [
        ([0.0, 0.0, -1.0], [[0, 0, 0], [0, 1, 0], [1, 1, 0], [1, 0, 0]]),
        ([0.0, 0.0, 1.0], [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]]),
        ([0.0, -1.0, 0.0], [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]]),
        ([0.0, 1.0, 0.0], [[0, 1, 0], [0, 1, 1], [1, 1, 1], [1, 1, 0]]),
        ([-1.0, 0.0, 0.0], [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]]),
        ([1.0, 0.0, 0.0], [[1, 0, 0], [1, 1, 0], [1, 1, 1], [1, 0, 1]]),
    ];
    for (n, corners) in faces {
        let base = positions.len() as u32;
        for c in corners {
            positions.push(p(c[0], c[1], c[2]));
            normals.push(n);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    let tris = indices.len() / 3;
    Mesh {
        name: name.into(),
        positions: positions.into(),
        normals: normals.into(),
        indices: indices.into(),
        material_slots: vec![material],
        face_material_slots: vec![0; tris],
        ..Default::default()
    }
}

/// Twelve beams along a box's edges, as one mesh.
fn frame_mesh(name: &str, min: [f32; 3], max: [f32; 3], material: MaterialId, edges: &[usize]) -> Mesh {
    let h = EDGE * 0.5;
    let mut out = Mesh { name: name.into(), material_slots: vec![material], ..Default::default() };
    let (mut positions, mut normals, mut indices): (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) = (Vec::new(), Vec::new(), Vec::new());
    let corner = |i: usize| [if i & 1 == 0 { min[0] } else { max[0] }, if i & 2 == 0 { min[1] } else { max[1] }, if i & 4 == 0 { min[2] } else { max[2] }];
    // The 12 edges as corner pairs (bit 0 = x, bit 1 = y, bit 2 = z).
    const EDGES: [(usize, usize); 12] = [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)];
    for &e in edges {
        let (a, b) = EDGES[e];
        let (pa, pb) = (corner(a), corner(b));
        let lo = [pa[0].min(pb[0]) - h, pa[1].min(pb[1]) - h, pa[2].min(pb[2]) - h];
        let hi = [pa[0].max(pb[0]) + h, pa[1].max(pb[1]) + h, pa[2].max(pb[2]) + h];
        let beam = box_mesh("", lo, hi, material);
        let base = positions.len() as u32;
        positions.extend_from_slice(&beam.positions);
        normals.extend_from_slice(&beam.normals);
        indices.extend(beam.indices.iter().map(|i| i + base));
    }
    let tris = indices.len() / 3;
    out.positions = positions.into();
    out.normals = normals.into();
    out.indices = indices.into();
    out.face_material_slots = vec![0; tris];
    out
}

fn strip(b: &mut DocumentBuilder, name: &str, r: Rect, lift: f32, material: MaterialId) {
    if r.width() <= 0.0 || r.depth() <= 0.0 {
        return;
    }
    let mesh = box_mesh(name, [r.min[0] as f32, r.min[1] as f32, -0.3 + lift], [r.max[0] as f32, r.max[1] as f32, lift], material);
    object(b, &format!("{ZONE}{name}"), mesh, "fab.bed");
}

/// A small integer hash: the plate's grain, the same every run.
fn hash(x: u32, y: u32) -> u32 {
    let mut h = x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ 0x9e37_79b9;
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^ (h >> 15)
}

/// The build plate's face: a fine textured-PEI grain and a lighter rim.
/// The viewport's composite draws the 10 / 50 mm grid lines, crisp at every zoom.
fn plate_texture(width_mm: f32, depth_mm: f32) -> (u32, u32, Arc<[u8]>) {
    let w = ((width_mm * TEXELS_PER_MM) as u32).clamp(16, 2048);
    let h = ((depth_mm * TEXELS_PER_MM) as u32).clamp(16, 2048);
    let mut px = vec![0u8; (w * h * 4) as usize];
    let base = [44.0f32, 46.0, 50.0];
    for y in 0..h {
        for x in 0..w {
            let mm = [x as f32 / TEXELS_PER_MM, (h - 1 - y) as f32 / TEXELS_PER_MM];
            // Grain: two octaves of hashed noise, very low contrast.
            let n = (hash(x, y) & 0xff) as f32 / 255.0 - 0.5;
            let n2 = (hash(x / 3, y / 3) & 0xff) as f32 / 255.0 - 0.5;
            let mut c = base.map(|v| v + n * 7.0 + n2 * 5.0);
            // The rim: a few millimetres lighter all round.
            let edge = mm[0].min(mm[1]).min(width_mm - mm[0]).min(depth_mm - mm[1]);
            if edge < 2.5 {
                c = c.map(|v| v + 14.0);
            }
            let o = ((y * w + x) * 4) as usize;
            px[o] = c[0].clamp(0.0, 255.0) as u8;
            px[o + 1] = c[1].clamp(0.0, 255.0) as u8;
            px[o + 2] = c[2].clamp(0.0, 255.0) as u8;
            px[o + 3] = 255;
        }
    }
    (w, h, px.into())
}

/// The plate texture of a bed size, made once per size.
fn plate_pixels(width_mm: f32, depth_mm: f32) -> (u32, u32, Arc<[u8]>) {
    use std::sync::Mutex;
    static CACHE: Mutex<Option<((u32, u32), (u32, u32, Arc<[u8]>))>> = Mutex::new(None);
    let key = ((width_mm * 10.0) as u32, (depth_mm * 10.0) as u32);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, v)) = cache.as_ref() {
        if *k == key {
            return v.clone();
        }
    }
    let v = plate_texture(width_mm, depth_mm);
    *cache = Some((key, v.clone()));
    v
}

/// The top face of the plate as one quad with UVs over the texture.
fn plate_top(name: &str, min: [f32; 2], max: [f32; 2], z: f32, material: MaterialId) -> Mesh {
    let p = |x: f32, y: f32| [x * MM, y * MM, z * MM];
    Mesh {
        name: name.into(),
        positions: vec![p(min[0], min[1]), p(max[0], min[1]), p(max[0], max[1]), p(min[0], max[1])].into(),
        normals: vec![[0.0, 0.0, 1.0]; 4].into(),
        uvs: vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]].into(),
        indices: vec![0u32, 1, 2, 0, 2, 3].into(),
        material_slots: vec![material],
        face_material_slots: vec![0; 2],
        ..Default::default()
    }
}

fn add_bed(b: &mut DocumentBuilder, machine: &Machine) {
    let bed = machine.bed;
    let (bx0, by0, bx1, by1) = (bed.min[0] as f32, bed.min[1] as f32, bed.max[0] as f32, bed.max[1] as f32);
    // The heated bed: a dark slab a little larger than the plate.
    let slab = material(b, "Heated bed", [0.035, 0.036, 0.040, 1.0], 0.55);
    let m = 9.0;
    let mesh = box_mesh("Heated bed", [bx0 - m, by0 - m, -PLATE_THICKNESS - BED_THICKNESS], [bx1 + m, by1 + m, -PLATE_THICKNESS], slab);
    object(b, &format!("{CONTEXT}bed"), mesh, "fab.bed");
    // The build plate: steel edges, a textured face.
    let edge = material(b, "Plate edge", [0.20, 0.21, 0.23, 1.0], 0.4);
    let mesh = box_mesh("Plate", [bx0, by0, -PLATE_THICKNESS], [bx1, by1, -0.02], edge);
    object(b, &format!("{CONTEXT}plate"), mesh, "fab.bed");
    let (tw, th, pixels) = plate_pixels(bx1 - bx0, by1 - by0);
    let tex = b.add_texture(Texture { id: TextureId::default(), name: "Plate".into(), width: tw, height: th, rgba8: pixels, properties: Default::default() });
    let mut face = PbrMaterial::new(MaterialId::default(), "Plate face");
    face.base_color = [1.0, 1.0, 1.0, 1.0];
    face.roughness = 0.82;
    face.base_color_texture = Some(TextureSlot::new(tex));
    let face = b.add_material(face);
    object(b, &format!("{CONTEXT}plate-face"), plate_top("Plate face", [bx0, by0], [bx1, by1], 0.0, face), "fab.bed");

    let zones = SHOW_ZONES.load(std::sync::atomic::Ordering::Relaxed);
    // The nozzle-zone overlay: where only one nozzle reaches, keep-outs.
    let tints = [[0.10, 0.32, 0.80, 1.0], [0.85, 0.42, 0.06, 1.0]];
    for (i, nozzle) in machine.nozzles.iter().enumerate() {
        let others: Vec<&Rect> = machine.nozzles.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, n)| &n.reach).collect();
        if !zones {
            break;
        }
        let tint = material(b, &format!("{} nozzle only", nozzle.name), tints[i % tints.len()], 0.8);
        let r = nozzle.reach;
        for o in others {
            if o.min[0] > r.min[0] {
                strip(b, &format!("{} only", nozzle.name), Rect::new(r.min[0], r.min[1], o.min[0].min(r.max[0]), r.max[1]), 0.06, tint);
            }
            if o.max[0] < r.max[0] {
                strip(b, &format!("{} only", nozzle.name), Rect::new(o.max[0].max(r.min[0]), r.min[1], r.max[0], r.max[1]), 0.06, tint);
            }
        }
    }
    let keep_out = if zones { material(b, "Keep-out", [0.70, 0.06, 0.06, 1.0], 0.8) } else { MaterialId::default() };
    for (i, r) in machine.keep_out.iter().enumerate().filter(|_| zones) {
        let clipped = Rect::new(r.min[0].max(bed.min[0]), r.min[1].max(bed.min[1]), r.max[0].min(bed.max[0]), r.max[1].min(bed.max[1]));
        strip(b, &format!("Keep-out {}", i + 1), clipped, 0.08, keep_out);
    }
    // The build volume's edges are drawn by the view as clean lines; the
    // lower nozzle ceiling belongs to the zone overlay.
    let top = machine.max_height as f32;
    for (i, nozzle) in machine.nozzles.iter().enumerate() {
        if zones && (nozzle.max_height as f32) < top - 0.01 {
            let ring = material(b, &format!("{} nozzle ceiling", nozzle.name), tints[i % tints.len()], 0.6);
            let z = nozzle.max_height as f32;
            let r = nozzle.reach;
            let mesh = frame_mesh(&format!("{} nozzle ceiling", nozzle.name), [r.min[0] as f32, r.min[1] as f32, z - 1.0], [r.max[0] as f32, r.max[1] as f32, z], ring, &[2, 3, 6, 7]);
            object(b, &format!("{ZONE}{} nozzle ceiling", nozzle.name), mesh, "fab.bed");
        }
    }
}

/// The bed and build volume alone (the print preview hides the parts).
pub fn build_bed(machine: &Machine, name: &str) -> Arc<Scene> {
    let mut b = DocumentBuilder::new(name);
    b.coordinates(Units { source_to_meters: 1.0, ..Units::millimeters() }, UpAxis::Z, Handedness::Right);
    add_bed(&mut b, machine);
    Arc::new(Scene::from_document_with(b.finish(), &mut |_, _| {}))
}

/// The viewport scene of `ev`.
pub fn build(ev: &Evaluated, name: &str) -> Arc<Scene> {
    build_analysis(ev, name, false)
}

pub fn build_analysis(ev: &Evaluated, name: &str, overhangs: bool) -> Arc<Scene> {
    build_with(ev, name, None, overhangs, false, None, &[], &[], "", &[], &[], &[], None, &mut |_, _| {})
}

/// The worker's analysis scene, borrowing a retained scene for unchanged overlap inputs.
/// `pool`: the workers its build's big passes spread over.
/// `progress`: the build's stage as it goes ("geometry") and how far it is, 0..=1.
pub fn build_analysis_reusing(ev: &Evaluated, name: &str, overhangs: bool, previous: Option<&Scene>, pool: Option<&TaskPool>, progress: &mut dyn FnMut(&'static str, f32)) -> Arc<Scene> {
    build_with(ev, name, previous, overhangs, false, None, &[], &[], "", &[], &[], &[], pool, progress)
}


/// The Mechanics scene: like [`build`], but a part made of links (a chain, chainmail) is one object per
/// link, all sharing one link mesh, named `<object>~<link>`, so the sim moves each link on its own; and
/// the ball a cloth drapes over (printer mm), when there is one.
pub fn build_mechanics(ev: &Evaluated, name: &str, ball: Option<([f64; 3], f64)>) -> Arc<Scene> {
    build_with(ev, name, None, false, true, ball, &[], &[], "", &[], &[], &[], None, &mut |_, _| {})
}

/// The Mechanics scene of a jointed piece that has a flexible part or strings: its parts but the flexible
/// ones (`skip`, their objects), the flexible parts' pieces (name, plate mm mesh, colour), each moved on
/// its own as the part bends, and its strings' segments (their objects' names), each a thin rod from the
/// origin along x, 1 mm long, that the sim poses from end to end every frame.
pub fn build_jointed(ev: &Evaluated, name: &str, pieces: &[(String, &fab_loose::mesh::PieceMesh, [f32; 4])], skip: &[String], strings: &[String]) -> Arc<Scene> {
    build_with(ev, name, None, false, false, None, pieces, &[], "Flexible part", skip, strings, &[], None, &mut |_, _| {})
}

/// The Mechanics scene of free parts some of which move as their solids: its parts but those (`skip`,
/// their objects), and each solid as an object of its own: a mesh part's (name, plate mm mesh, colour),
/// a field part's (name and its field's box on the plate, mm: the view draws it by its field).
pub fn build_solids(ev: &Evaluated, name: &str, solids: &[(String, &fab_loose::mesh::PieceMesh, [f32; 4])], fields: &[(String, [f64; 3], [f64; 3])], skip: &[String]) -> Arc<Scene> {
    build_with(ev, name, None, false, false, None, solids, &[], "Part", skip, &[], fields, None, &mut |_, _| {})
}

/// The Mechanics scene of the plate's supports: its parts, and each loose piece as an object of its
/// own (name, plate mm mesh, colour).
pub fn build_loose(ev: &Evaluated, name: &str, pieces: &[(String, &fab_loose::mesh::PieceMesh, [f32; 4])]) -> Arc<Scene> {
    build_with(ev, name, None, false, false, None, pieces, &[], "Support", &[], &[], &[], None, &mut |_, _| {})
}

/// The Mechanics scene of a joint check: its parts (but `skip`, flexible parts shown as their
/// pieces), and the moving part of each joint once per turn the check tests (name, plate mm mesh,
/// colour), which it shows see-through, turned. `copies` names those ghosts and the part each
/// repeats: they draw in the ghosts' own pass and take their part's draw priority.
pub fn build_ghosts(ev: &Evaluated, name: &str, pieces: &[(String, &fab_loose::mesh::PieceMesh, [f32; 4])], copies: &[(String, String)], skip: &[String]) -> Arc<Scene> {
    build_with(ev, name, None, false, false, None, pieces, copies, "Joint check", skip, &[], &[], None, &mut |_, _| {})
}

/// The scene of a resin printer's slice: the plate's, and the supports its parts stand on
/// (`supports`, plate mm as they print) in the resin's colour, lightened so they read apart from
/// the parts. The supports are the printer's, not a part: never listed or picked.
pub fn build_resin(ev: &Evaluated, name: &str, supports: &fab_plate::PartMesh) -> Arc<Scene> {
    let c = ev.plate.parts.first().map_or([0.6, 0.6, 0.6, 1.0], |p| p.color);
    let colour = [0.6 * c[0] + 0.4 * 0.85, 0.6 * c[1] + 0.4 * 0.85, 0.6 * c[2] + 0.4 * 0.85, 1.0];
    let piece = fab_loose::mesh::creased(&supports.positions, &supports.triangles, 40.0);
    build_with(ev, name, None, false, false, None, &[(format!("{CONTEXT}resin-supports"), &piece, colour)], &[], "Support", &[], &[], &[], None, &mut |_, _| {})
}

/// A string's colour: undyed thread.
const STRING_COLOUR: [f32; 4] = [0.9, 0.87, 0.8, 1.0];

/// A string segment as drawn: an open eight-sided rod of the string's radius from the origin along x, 1 mm
/// long (metres), its sides smooth.
fn string_mesh(material: MaterialId) -> Mesh {
    let sides = 8u32;
    let r = crate::mechanics::STRING_RADIUS as f32;
    let (mut positions, mut normals, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    for k in 0..sides {
        let a = std::f32::consts::TAU * k as f32 / sides as f32;
        let (c, s) = (a.cos(), a.sin());
        for x in [0.0, 1.0] {
            positions.push([x * MM, r * c * MM, r * s * MM]);
            normals.push([0.0, c, s]);
        }
    }
    for k in 0..sides {
        let (a, b) = (2 * k, 2 * ((k + 1) % sides));
        indices.extend_from_slice(&[a, b, b + 1, a, b + 1, a + 1]);
    }
    let tris = indices.len() / 3;
    Mesh { name: "string".into(), positions: positions.into(), normals: normals.into(), indices: indices.into(), material_slots: vec![material], face_material_slots: vec![0; tris], ..Default::default() }
}

/// A UV sphere in metres.
fn ball_mesh(centre: [f64; 3], r: f64, material: MaterialId) -> Mesh {
    let (rings, segs) = (24u32, 48u32);
    let (mut positions, mut normals, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..=rings {
        let th = std::f64::consts::PI * i as f64 / rings as f64;
        for j in 0..=segs {
            let ph = std::f64::consts::TAU * j as f64 / segs as f64;
            let n = [th.sin() * ph.cos(), th.sin() * ph.sin(), th.cos()];
            positions.push([((centre[0] + r * n[0]) as f32) * MM, ((centre[1] + r * n[1]) as f32) * MM, ((centre[2] + r * n[2]) as f32) * MM]);
            normals.push([n[0] as f32, n[1] as f32, n[2] as f32]);
        }
    }
    for i in 0..rings {
        for j in 0..segs {
            let a = i * (segs + 1) + j;
            let b = a + segs + 1;
            indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    let tris = indices.len() / 3;
    Mesh { name: "ball".into(), positions: positions.into(), normals: normals.into(), indices: indices.into(), material_slots: vec![material], face_material_slots: vec![0; tris], ..Default::default() }
}

/// The name of a link's object in the Mechanics scene.
pub fn link_object(object: &str, link: usize) -> String {
    format!("{object}~{link}")
}

/// A rigid move in millimetres as the column-major matrix of the metre scene.
pub(crate) fn rigid_m(m: &[[f64; 4]; 3]) -> [f32; 16] {
    let r = |i: usize, j: usize| m[i][j] as f32;
    let t = |i: usize| m[i][3] as f32 * MM;
    [r(0, 0), r(1, 0), r(2, 0), 0.0, r(0, 1), r(1, 1), r(2, 1), 0.0, r(0, 2), r(1, 2), r(2, 2), 0.0, t(0), t(1), t(2), 1.0]
}

/// One link's mesh (metres, smooth normals) for every instance of a weave part.
fn link_mesh(name: &str, links: &fab_model::weave::LinkInstances, materials: Vec<MaterialId>) -> Mesh {
    let m = &links.mesh;
    let mut normals = vec![[0.0f32; 3]; m.vertices.len()];
    for t in &m.triangles {
        let (a, b, c) = (m.vertices[t[0] as usize], m.vertices[t[1] as usize], m.vertices[t[2] as usize]);
        let n = (b - a).cross(c - a);
        for &i in t {
            let v = &mut normals[i as usize];
            v[0] += n.x as f32;
            v[1] += n.y as f32;
            v[2] += n.z as f32;
        }
    }
    for v in &mut normals {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
        *v = [v[0] / l, v[1] / l, v[2] / l];
    }
    Mesh {
        name: name.into(),
        positions: m.vertices.iter().map(|p| [p.x as f32 * MM, p.y as f32 * MM, p.z as f32 * MM]).collect::<Vec<_>>().into(),
        normals: normals.into(),
        indices: m.triangles.iter().flat_map(|t| t.iter().copied()).collect::<Vec<_>>().into(),
        material_slots: materials,
        face_material_slots: vec![0; m.triangles.len()],
        ..Default::default()
    }
}

fn build_with(ev: &Evaluated, name: &str, previous: Option<&Scene>, overhangs: bool, links: bool, ball: Option<([f64; 3], f64)>, pieces: &[(String, &fab_loose::mesh::PieceMesh, [f32; 4])], copies: &[(String, String)], piece_material: &str, skip: &[String], strings: &[String], fields: &[(String, [f64; 3], [f64; 3])], pool: Option<&TaskPool>, progress: &mut dyn FnMut(&'static str, f32)) -> Arc<Scene> {
    let mut b = DocumentBuilder::new(name);
    b.coordinates(Units { source_to_meters: 1.0, ..Units::millimeters() }, UpAxis::Z, Handedness::Right);
    add_bed(&mut b, &ev.machine);
    if let Some((c, r)) = ball {
        let m = material(&mut b, "Drape ball", [0.62, 0.64, 0.68, 1.0], 0.7);
        object(&mut b, &format!("{CONTEXT}mech-ball"), ball_mesh(c, r, m), "fab.mech");
    }
    for (i, (part, rm)) in ev.plate.parts.iter().zip(&ev.render).enumerate() {
        if skip.contains(&part.object) {
            continue;
        }
        let blocked = ev.reports.get(i).is_some_and(|r| r.blocking());
        let color = if blocked {
            let c = part.color;
            [0.35 * c[0] + 0.65 * 0.8, 0.35 * c[1] + 0.65 * 0.05, 0.35 * c[2] + 0.65 * 0.03, 1.0]
        } else {
            part.color
        };
        let mat = part_material(&mut b, ev, i, color);
        // Only the mechanics scene separates the links into moving bodies.
        if let Some(w) = ev.weaves.iter().find(|w| w.part == i).filter(|_| links) {
            // a painted link takes its paint colour's
            let colors = ev.paint.get(i).and_then(|p| p.as_ref()).map(|p| p.colors.clone()).unwrap_or_default();
            let mut by_slot = vec![mat];
            for (k, c) in colors.iter().enumerate().skip(1) {
                by_slot.push(material(&mut b, &format!("{} link colour {k}", part.object), [c.rgb[0], c.rgb[1], c.rgb[2], 1.0], FILAMENT_ROUGHNESS));
            }
            // one mesh, one object per link
            let mesh_id = b.add_mesh(link_mesh(&part.object, &w.instances, vec![mat]));
            for (k, pose) in w.instances.poses.iter().enumerate() {
                let mut o = Object::new(ObjectId::default(), &link_object(&part.object, k));
                o.tags.push("fab.link".into());
                o.transform.matrix.v = rigid_m(&fab_model::weave::compose(&w.to_plate, pose));
                let mut inst = MeshInstance::new(mesh_id);
                let slot = w.instances.slots.get(k).copied().unwrap_or(0) as usize;
                if slot > 0 {
                    inst.material_overrides = vec![by_slot.get(slot).copied()];
                }
                o.meshes.push(inst);
                b.add_object(o);
            }
            continue;
        }
        let tris = rm.indices.len() / 3;
        // A painted part's paint is drawn by the views' paint lookup (its program by position).
        let mesh = Mesh {
            name: part.name.clone(),
            positions: rm.positions.iter().map(|p| [p[0] * MM, p[1] * MM, p[2] * MM]).collect::<Vec<_>>().into(),
            normals: rm.normals.clone().into(),
            indices: rm.indices.clone().into(),
            material_slots: if overhangs {
                fab_printability::OVERHANG_COLORS.iter().enumerate().map(|(d, c)| material(&mut b, &format!("Overhang {d}"), *c, 0.65)).collect()
            } else { vec![mat] },
            face_material_slots: if overhangs {
                ev.shown_overhang(i).map(|v| v.iter().map(|d| *d as u32).collect()).unwrap_or_else(|| vec![0; tris])
            } else { vec![0; tris] },
            ..Default::default()
        };
        // Named by the model object (unique), not the display name.
        object(&mut b, &part.object, mesh, "fab.part");
    }
    // Excluded placements (inserts faded, parts shown without printing in their own material)
    // remain pickable.
    if !links {
        for (i, excluded) in ev.plate.excluded.iter().enumerate() {
            let (part, rm) = (&excluded.part, &excluded.render);
            let mat = part_material(&mut b, ev, ev.plate.parts.len() + i, part.color);
            let mesh = Mesh {
                name: part.name.clone(),
                positions: rm.positions.iter().map(|p| [p[0] * MM, p[1] * MM, p[2] * MM]).collect::<Vec<_>>().into(),
                normals: rm.normals.clone().into(),
                indices: rm.indices.clone().into(),
                material_slots: vec![mat],
                face_material_slots: vec![0; rm.indices.len() / 3],
                ..Default::default()
            };
            object(&mut b, &part.object, mesh, "fab.part");
        }
    }
    for (name, piece, colour) in pieces {
        let m = material(&mut b, piece_material, *colour, FILAMENT_ROUGHNESS);
        let mesh = Mesh {
            name: name.clone(),
            positions: piece.positions.iter().map(|p| [p[0] * MM, p[1] * MM, p[2] * MM]).collect::<Vec<_>>().into(),
            normals: piece.normals.clone().into(),
            indices: piece.indices.clone().into(),
            material_slots: vec![m],
            face_material_slots: vec![0; piece.indices.len() / 3],
            ..Default::default()
        };
        object(&mut b, name, mesh, "fab.mech");
    }
    // a field part's pieces: drawn by their fields, each object with no triangles takes its field's box
    for (name, ..) in fields {
        let m = material(&mut b, "Part", [0.6, 0.6, 0.6, 1.0], FILAMENT_ROUGHNESS);
        object(&mut b, name, Mesh { name: name.clone(), material_slots: vec![m], ..Default::default() }, "fab.mech");
    }
    // one rod mesh, one object per string segment
    if !strings.is_empty() {
        let m = material(&mut b, "String", STRING_COLOUR, 0.8);
        let mesh_id = b.add_mesh(string_mesh(m));
        for name in strings {
            let mut o = Object::new(ObjectId::default(), name);
            o.tags.push("fab.mech".into());
            o.meshes.push(MeshInstance::new(mesh_id));
            b.add_object(o);
        }
    }
    let document = b.finish();
    let build = |progress: &mut dyn FnMut(&'static str, f32)| match copies {
        [] => Scene::from_document_reusing(document, previous, pool, progress),
        _ => Scene::from_document_with_copies(document, copies, progress),
    };
    let mut scene = if std::env::var_os("FAB_SCENE_TIMING").is_some() {
        let mut last = (std::time::Instant::now(), "start");
        let s = build(&mut |stage, f| {
            if stage != last.1 {
                println!("scene stage {:>10} took {:.0} ms", last.1, last.0.elapsed().as_secs_f64() * 1000.0);
                last = (std::time::Instant::now(), stage);
            }
            progress(stage, f);
        });
        println!("scene stage {:>10} took {:.0} ms", last.1, last.0.elapsed().as_secs_f64() * 1000.0);
        s
    } else {
        build(progress)
    };
    // A part drawn from its field has no triangles here: its element takes the field's box.
    for (part, rm) in ev.plate.parts.iter().zip(&ev.render).chain(ev.plate.excluded.iter().map(|e| (&e.part, &e.render))) {
        let (Some(fb), true) = (part.field_bounds, rm.indices.is_empty()) else { continue };
        let Some(id) = scene.elements.iter().find(|e| e.name == part.object).map(|e| e.id) else { continue };
        let v = |p: [f64; 3]| fab::model::makepad_math::Vec3f { x: p[0] as f32 * MM, y: p[1] as f32 * MM, z: p[2] as f32 * MM };
        scene.set_field_bounds(id, fab::Aabb { min: v(fb.min), max: v(fb.max) });
    }
    for (name, lo, hi) in fields {
        let Some(id) = scene.elements.iter().find(|e| e.name == *name).map(|e| e.id) else { continue };
        let v = |p: [f64; 3]| fab::model::makepad_math::Vec3f { x: p[0] as f32 * MM, y: p[1] as f32 * MM, z: p[2] as f32 * MM };
        scene.set_field_bounds(id, fab::Aabb { min: v(*lo), max: v(*hi) });
    }
    Arc::new(scene)
}
