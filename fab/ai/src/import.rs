//! Mesh files as parts in Fab's model frame, ready to become assets and object scripts.

use crate::img2mesh::{self, mesh_from_gltf, weld_and_orient};
use crate::out::{arr, int, mm, obj, s};
use crate::{Reply, Session};
use fab_model::{project::ColorDef, script_file, Change, FabModel, PlaceArg};
use fab_plate::machine::Machine;
use makepad_fab_slicer::mesh::{load_3mf, load_stl};
use makepad_model_script::lang::csg_sdf::mesh_field::{MeshData, MeshField};
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What a mesh file brings: its objects, in the file's order.
#[derive(Clone, Debug)]
pub struct ImportFile {
    /// The file's name without its extension (the plate's name when it opens as a plate).
    pub name: String,
    pub objects: Vec<ImportObject>,
    /// Filament colours by slot (sRGB 0..1 RGBA) from the file; may be empty.
    pub colours: Vec<[f32; 4]>,
}

#[derive(Clone, Debug)]
pub struct ImportObject {
    /// The object's name in the file ("" when it has none).
    pub name: String,
    /// Model frame (Y up, mm), centred over the origin in X and Z and standing on Y = 0, welded.
    pub mesh: MeshData,
    /// Its largest dimension (mm).
    pub size_mm: f64,
    /// Where its footprint centre was in the file, in printer millimetres (X right, Y back,
    /// the bed's front-left corner at the origin). Only a 3MF supplies build placement.
    pub at: Option<[f64; 2]>,
    /// The 1-based filament the file gives the object, None when unset.
    pub filament: Option<u32>,
    /// Painted leaf triangles in the mesh's model frame and their states (0 = the object's
    /// own filament, n = filament n). None when nothing is painted.
    pub facets: Option<(Vec<[[f32; 3]; 3]>, Vec<u8>)>,
}

/// Mesh files Fab imports, by lower-case extension.
pub const EXTENSIONS: &[&str] = &["stl", "obj", "3mf", "glb", "gltf"];

pub fn is_mesh_file(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|s| EXTENSIONS.iter().any(|ext| s.eq_ignore_ascii_case(ext)))
}

pub fn read(path: &Path) -> Result<ImportFile, String> {
    read_file(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_file(path: &Path) -> Result<ImportFile, String> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    if !EXTENSIONS.contains(&ext.as_str()) {
        return Err("unknown mesh file extension".into());
    }
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if ext == "gltf" || ext == "glb" {
        let loaded = makepad_gltf::load_gltf_from_path(path).map_err(|e| format!("unreadable glTF: {e}"))?;
        let raw = mesh_from_gltf(&loaded)?;
        let positions: Vec<[f64; 3]> = raw.positions.iter().map(|p| p.map(|x| x as f64 * 1000.0)).collect();
        let (positions, triangles, _) = weld_and_orient(&positions, &raw.triangles)?;
        let (mesh, size_mm, _) = centred(&positions, triangles, false)?;
        let object_name = loaded.document.scenes_slice().get(loaded.document.scene.unwrap_or(0))
            .and_then(|s| s.name.clone()).unwrap_or_default();
        let object = ImportObject { name: object_name, mesh, size_mm, at: None, filament: None, facets: None };
        return Ok(ImportFile { name, objects: vec![object], colours: Vec::new() });
    }
    let data = std::fs::read(path).map_err(|e| format!("unreadable mesh file: {e}"))?;
    let mut colours = Vec::new();
    let objects = match ext.as_str() {
        "stl" => {
            let mesh = load_stl(&data)?;
            vec![object_from_printer("", &mesh.verts, &mesh.tris, None)?]
        }
        "obj" => {
            let (positions, triangles) = read_obj(&data)?;
            vec![object_from_printer("", &positions, &triangles, None)?]
        }
        "3mf" => {
            let model = load_3mf(&data)?;
            colours = file_colours(&data)?;
            let mut objects = Vec::with_capacity(model.objects.len());
            for object in model.objects {
                let leaves = makepad_fab_slicer::paint::leaves(&object.mesh);
                let (mesh, size_mm, origin) = centred(&object.mesh.verts, object.mesh.tris, true)?;
                let facets = if leaves.is_empty() {
                    None
                } else {
                    let mut triangles = Vec::with_capacity(leaves.len());
                    let mut states = Vec::with_capacity(leaves.len());
                    for leaf in leaves {
                        let state = u8::try_from(leaf.state).map_err(|_| format!("object '{}' has paint state {}, above 255", object.name, leaf.state))?;
                        triangles.push(leaf.v.map(|p| model_point(p, origin, true)));
                        states.push(state);
                    }
                    Some((triangles, states))
                };
                objects.push(ImportObject {
                    name: object.name, mesh, size_mm, at: Some([origin[0], origin[1]]),
                    filament: (object.extruder > 0).then_some(object.extruder), facets,
                });
            }
            // A slot the file names no colour for (a file without project settings) gets a
            // distinct one, so its parts and painted facets still show apart.
            let used = objects.iter().flat_map(|o| o.filament.into_iter().chain(o.facets.iter().flat_map(|f| f.1.iter().map(|s| *s as u32)))).max().unwrap_or(0) as usize;
            while colours.len() < used {
                colours.push(slot_colour(colours.len()));
            }
            objects
        }
        _ => unreachable!(),
    };
    if objects.is_empty() {
        return Err("no triangles in the mesh file".into());
    }
    Ok(ImportFile { name, objects, colours })
}

/// For callers that have meshes already (a calibration coupon's parts, printer frame mm).
pub fn object_from_printer(name: &str, positions: &[[f64; 3]], triangles: &[[u32; 3]], at: Option<[f64; 2]>) -> Result<ImportObject, String> {
    let (positions, triangles, _) = weld_and_orient(positions, triangles)?;
    let (mesh, size_mm, _) = centred(&positions, triangles, true)?;
    Ok(ImportObject { name: name.into(), mesh, size_mm, at, filament: None, facets: None })
}

fn model_point(p: [f64; 3], origin: [f64; 3], printer: bool) -> [f32; 3] {
    let [x, y, z] = std::array::from_fn(|i| p[i] - origin[i]);
    if printer { [x as f32, z as f32, -y as f32] } else { [x as f32, y as f32, z as f32] }
}

fn centred(positions: &[[f64; 3]], triangles: Vec<[u32; 3]>, printer: bool) -> Result<(MeshData, f64, [f64; 3]), String> {
    if triangles.is_empty() {
        return Err("no triangles in the mesh".into());
    }
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in positions {
        for i in 0..3 {
            if !p[i].is_finite() {
                return Err("the mesh has a non-finite vertex".into());
            }
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    let size_mm = (0..3).map(|i| hi[i] - lo[i]).fold(0.0, f64::max);
    if !size_mm.is_finite() || size_mm <= 0.0 {
        return Err("the mesh has no usable size".into());
    }
    let mut origin = std::array::from_fn(|i| lo[i] * 0.5 + hi[i] * 0.5);
    let up = if printer { 2 } else { 1 };
    origin[up] = lo[up];
    let mut points = Vec::with_capacity(positions.len());
    for &p in positions {
        let p = model_point(p, origin, printer);
        if p.iter().any(|x| !x.is_finite()) {
            return Err("the mesh is too large for model coordinates".into());
        }
        points.push(p);
    }
    Ok((MeshData { positions: points, triangles }, size_mm, origin))
}

fn read_obj(data: &[u8]) -> Result<(Vec<[f64; 3]>, Vec<[u32; 3]>), String> {
    let text = std::str::from_utf8(data).map_err(|_| "unreadable OBJ text")?;
    // Count first so a large triangle file grows neither vector one face at a time.
    let (mut vertex_count, mut triangle_count) = (0, 0);
    for line in text.lines() {
        let mut words = line.split('#').next().unwrap_or("").split_whitespace();
        match words.next() {
            Some("v") => vertex_count += 1,
            Some("f") => triangle_count += words.count().saturating_sub(2),
            _ => {}
        }
    }
    let mut positions = Vec::with_capacity(vertex_count);
    let mut triangles = Vec::with_capacity(triangle_count);
    for (line, text) in text.lines().enumerate() {
        let mut words = text.split('#').next().unwrap_or("").split_whitespace();
        let line = line + 1;
        match words.next() {
            Some("v") => {
                let mut p = [0.0f64; 3];
                for x in &mut p {
                    *x = words.next().and_then(|s| s.parse().ok()).filter(|x: &f64| x.is_finite())
                        .ok_or_else(|| format!("OBJ line {line}: unreadable vertex"))?;
                }
                positions.push(p);
            }
            Some("f") => {
                let mut indices = words.map(|s| {
                    let index = s.split('/').next().and_then(|s| s.parse::<i64>().ok())
                        .ok_or_else(|| format!("OBJ line {line}: unreadable face index"))?;
                    let index = if index < 0 { positions.len() as i64 + index } else { index - 1 };
                    if index < 0 || index as usize >= positions.len() || index > u32::MAX as i64 {
                        return Err(format!("OBJ line {line}: face vertex is out of range"));
                    }
                    Ok(index as u32)
                });
                let first = indices.next().ok_or_else(|| format!("OBJ line {line}: face has fewer than three vertices"))??;
                let mut previous = indices.next().ok_or_else(|| format!("OBJ line {line}: face has fewer than three vertices"))??;
                let start = triangles.len();
                for next in indices {
                    let next = next?;
                    triangles.push([first, previous, next]);
                    previous = next;
                }
                if triangles.len() == start {
                    return Err(format!("OBJ line {line}: face has fewer than three vertices"));
                }
            }
            _ => {}
        }
    }
    Ok((positions, triangles))
}

/// Filament colours from the file's project settings, if present.
fn file_colours(data: &[u8]) -> Result<Vec<[f32; 4]>, String> {
    let mut cur = Cursor::new(data);
    let dir = makepad_zip_file::zip_read_central_directory(&mut cur).map_err(|e| format!("3MF colours: {e:?}"))?;
    let Some(h) = dir.file_headers.iter().find(|h| h.file_name == "Metadata/project_settings.config") else { return Ok(Vec::new()) };
    let bytes = h.extract(&mut cur).map_err(|e| format!("3MF colours: {e:?}"))?;
    let v = makepad_strict_json::parse_depth_last_key_wins(&bytes, 32).map_err(|e| format!("3MF colours: {e}"))?;
    match v.get("filament_colour") {
        Some(makepad_strict_json::Value::Arr(a)) => a.iter().enumerate().map(|(i, c)| {
            c.as_str().and_then(parse_colour).ok_or_else(|| format!("3MF filament {} has an unreadable colour", i + 1))
        }).collect(),
        None => Ok(Vec::new()),
        _ => Err("3MF filament colours must be a list".into()),
    }
}

/// A distinct colour for filament slot `i` (0-based).
fn slot_colour(i: usize) -> [f32; 4] {
    const C: [[f32; 3]; 8] = [[0.92, 0.92, 0.90], [0.85, 0.22, 0.20], [0.20, 0.45, 0.90], [0.95, 0.75, 0.15], [0.20, 0.70, 0.35], [0.60, 0.30, 0.75], [0.15, 0.15, 0.17], [0.95, 0.50, 0.15]];
    let c = C[i % C.len()];
    [c[0], c[1], c[2], 1.0]
}

fn parse_colour(s: &str) -> Option<[f32; 4]> {
    let h = s.trim().strip_prefix('#')?;
    if !matches!(h.len(), 6 | 8) || !h.is_ascii() {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|v| v as f32 / 255.0);
    Some([c(0)?, c(2)?, c(4)?, if h.len() == 8 { c(6)? } else { 1.0 }])
}

/// A painted state's material: the file's filament colour (sRGB) and the print colour it
/// prints as.
pub struct Material {
    pub albedo: [f32; 4],
    pub color: String,
}

/// The print colour of the file's filament `f` (1-based).
fn filament_color(f: u32) -> String {
    format!("Filament {f}")
}

/// A part ready to become a project asset and an object script.
pub struct NewPart {
    pub wanted: String,
    pub mesh: MeshData,
    pub size_mm: f64,
    pub facets: Option<(Vec<[[f32; 3]; 3]>, Vec<u8>, Vec<Option<Material>>)>,
    /// Leading comments; `{name}` stands for the free asset name.
    pub header: String,
    pub size_bounds: (f64, f64),
    pub place: Vec<PlaceArg>,
    /// The print colours it and its painted facets print as, with the file's colours; added
    /// to the project unless it defines them already.
    pub colors: Vec<ColorDef>,
}

pub(crate) fn colour(c: [f32; 4]) -> u32 {
    c.into_iter().fold(0, |v, c| (v << 8) | (c.clamp(0.0, 1.0) * 255.0).round() as u32)
}

/// The group's footprint in printer coordinates, from centred model meshes.
fn footprint(objects: &[ImportObject]) -> fab_plate::Bounds {
    let mut bounds = fab_plate::Bounds::EMPTY;
    for o in objects {
        if let Some(at) = o.at {
            for p in &o.mesh.positions {
                let p = [at[0] + p[0] as f64, at[1] - p[2] as f64, p[1] as f64];
                for k in 0..3 {
                    bounds.min[k] = bounds.min[k].min(p[k]);
                    bounds.max[k] = bounds.max[k].max(p[k]);
                }
            }
        }
    }
    bounds
}

fn parts(objects: Vec<ImportObject>, colours: &[[f32; 4]], file: &str, machine: &Machine, centre: bool) -> Result<Vec<NewPart>, String> {
    let bounds = footprint(&objects);
    let shift = if objects.iter().any(|o| o.at.is_some()) && (centre || !machine.bed.contains_footprint(&bounds, 0.0)) {
        let c = machine.bed_center();
        [c[0] - (bounds.min[0] + bounds.max[0]) * 0.5, c[1] - (bounds.min[1] + bounds.max[1]) * 0.5]
    } else { [0.0; 2] };
    objects.into_iter().map(|o| {
        let wanted = if o.name.is_empty() { Path::new(file).file_stem().unwrap_or_default().to_string_lossy().into_owned() } else { o.name.clone() };
        let mut place = Vec::new();
        let mut colors: Vec<ColorDef> = Vec::new();
        let mut color = |f: u32| {
            let name = filament_color(f);
            if !colors.iter().any(|c| c.name == name) {
                let rgb = f.checked_sub(1).and_then(|i| colours.get(i as usize)).map(|c| colour(*c));
                colors.push(ColorDef { name: name.clone(), rgb, at: Vec::new(), material: None });
            }
            name
        };
        if !o.name.is_empty() {
            place.push(PlaceArg::Name(Some(o.name)));
        }
        if let Some(f) = o.filament {
            place.push(PlaceArg::Color(Some(color(f))));
        }
        if let Some(at) = o.at {
            place.push(PlaceArg::At(Some([at[0] + shift[0], at[1] + shift[1]])));
        }
        let painted = if o.facets.is_some() { ", its painted facets beside it" } else { "" };
        let file = file.replace(['\n', '\r'], " ");
        let header = format!("// {{name}}: imported from {file}.\n// The mesh is this project's `{{name}}` (in its .meshes folder){painted}; Size scales it.");
        let facets = o.facets.map(|(triangles, states)| {
            let mut used = [false; 256];
            for &state in &states { used[state as usize] = true; }
            let count = states.iter().copied().max().unwrap_or(0) as usize;
            let materials = (0..count).map(|i| {
                if !used[i + 1] { return Ok(None); }
                let &albedo = colours.get(i).ok_or_else(|| format!("{file}: painted state {} has no filament colour", i + 1))?;
                Ok(Some(Material { albedo, color: color(i as u32 + 1) }))
            }).collect::<Result<Vec<_>, String>>()?;
            Ok::<_, String>((triangles, states, materials))
        }).transpose()?;
        Ok(NewPart { wanted, mesh: o.mesh, size_mm: o.size_mm, facets, header,
            size_bounds: (((o.size_mm / 10.0).min(1.0) * 10.0).round() / 10.0, (4.0 * o.size_mm).max(300.0)), place, colors })
    }).collect()
}

fn reserved(name: &str) -> bool {
    matches!(name, "m" | "size" | "object" | "param") || makepad_script::syntax::write::key(name).map_or(true, |key| key != name)
}

impl Session {
    pub(crate) fn import_name(&self, wanted: &str) -> String {
        img2mesh::free_name(&img2mesh::ident(wanted), &|n| reserved(n) || self.model.assets.contains_key(n) || self.model.facets.contains_key(n)
            || self.model.source(&script_file(n)).is_some() || self.model.project().scripts.iter().any(|s| s.name == n))
    }

    /// Add every part and the plate settings in one undo step.
    pub fn add_imported(&mut self, what: &str, parts: Vec<NewPart>, plate_settings: Vec<(String, String)>) -> Reply {
        self.add_imported_timed(what, parts, plate_settings).0
    }

    fn add_imported_timed(&mut self, what: &str, parts: Vec<NewPart>, plate_settings: Vec<(String, String)>) -> (Reply, Duration, Duration) {
        let mut fields = Duration::ZERO;
        let previous_assets = self.model.assets.clone();
        let previous_facets = self.model.facets.clone();
        let mut assets = (*previous_assets).clone();
        let mut facets = (*previous_facets).clone();
        let mut changes = Vec::new();
        let mut names = Vec::new();
        let mut scripts = Vec::new();
        let mut meshes = Vec::new();
        // The print colours the parts print as come first, so their placements name them.
        for def in parts.iter().flat_map(|p| &p.colors) {
            let defined = |n: &str| self.model.project().colors.iter().any(|c| c.name == n);
            if !defined(&def.name) && !changes.iter().any(|c| matches!(c, Change::Color { name, .. } if *name == def.name)) {
                changes.push(Change::Color { name: def.name.clone(), set: Some(def.clone()) });
            }
        }
        for part in parts {
            let wanted = img2mesh::ident(&part.wanted);
            let name = img2mesh::free_name(&wanted, &|n| reserved(n) || assets.contains_key(n) || facets.contains_key(n)
                || self.model.source(&script_file(n)).is_some() || self.model.project().scripts.iter().any(|s| s.name == n));
            let file = script_file(&name);
            let header = part.header.replace("{name}", &name);
            let source = img2mesh::script_source(&header, &name, part.size_mm, part.size_bounds, part.facets.as_ref().map(|f| f.2.as_slice()));
            let (lo, hi) = img2mesh::bounds(&part.mesh.positions);
            meshes.push(obj(vec![("name", s(&name)), ("triangles", int(part.mesh.triangles.len())),
                ("size", obj(vec![("x", mm((hi[0] - lo[0]) as f64)), ("y", mm((hi[1] - lo[1]) as f64)), ("z", mm((hi[2] - lo[2]) as f64))]))]));
            let started = Instant::now();
            assets.insert(name.clone(), Arc::new(MeshField::new(part.mesh)));
            if let Some((triangles, states, _)) = part.facets {
                facets.insert(name.clone(), Arc::new(fab_paint_field::Facets::new(triangles, states)));
            }
            fields += started.elapsed();
            changes.push(Change::AddInstance { file: file.clone(), source: Some(source), name: Some(name.clone()), at: None });
            for arg in part.place {
                if matches!(&arg, PlaceArg::Name(Some(n)) if n == &name) { continue; }
                changes.push(Change::Place { instance: name.clone(), object: name.clone(), arg });
            }
            names.push(name);
            scripts.push(file);
        }
        if !plate_settings.is_empty() {
            changes.push(Change::PlateSettings { set: plate_settings, reset: Vec::new() });
        }
        self.model.assets = Arc::new(assets);
        self.model.facets = Arc::new(facets);
        let build = Instant::now();
        let mut reply = self.commit(what, changes);
        let built = build.elapsed();
        if !reply.ok {
            self.model.assets = previous_assets;
            self.model.facets = previous_facets;
        } else if let Some(ev) = &self.last {
            let objects = ev.plate.parts.iter().filter(|p| names.iter().any(|n| p.object.split('/').next() == Some(n.as_str()))).map(|p| s(&p.object));
            reply.text = crate::out::text(&obj(vec![("imported", arr(names.iter().map(s))), ("scripts", arr(scripts.iter().map(s))),
                ("objects", arr(objects)), ("meshes", arr(meshes)), ("check", crate::check_value(&ev.printability)), ("head", s(self.model.head().text()))]));
        }
        (reply, fields, built)
    }

    /// Read a mesh file on the model worker and place its objects together.
    pub fn import_file(&mut self, path: &Path, new_plate: bool) -> Reply {
        let started = Instant::now();
        let file = match read(path) { Ok(file) => file, Err(e) => return Reply::err(e) };
        let read = started.elapsed();
        let objects = file.objects.len();
        let triangles: usize = file.objects.iter().map(|o| o.mesh.triangles.len()).sum();
        let machine = self.last.as_ref().map(|e| &e.machine).unwrap_or(&self.eval.machine).clone();
        let filename = path.file_name().unwrap_or_default().to_string_lossy();
        let parts = match parts(file.objects, &file.colours, &filename, &machine, false) {
            Ok(parts) => parts, Err(e) => return Reply::err(e),
        };
        if new_plate {
            if let Err(e) = self.replace_model(FabModel::new(&file.name), None) { return Reply::err(e); }
        }
        let (mut reply, fields, build) = self.add_imported_timed("import", parts, Vec::new());
        if new_plate && reply.update.is_none() {
            reply.update = self.compiled.clone().zip(self.last.clone());
        }
        eprintln!("fab: imported {} ({objects} objects, {triangles} triangles): read {} ms, fields {} ms, build {} ms, total {} ms",
            path.display(), read.as_millis(), fields.as_millis(), build.as_millis(), started.elapsed().as_millis());
        reply
    }

    /// A calibration coupon becomes a project with the same assets and history as an import.
    pub fn calibration(&mut self, coupon: &fab_calibrate::Coupon, colour: [f32; 4]) -> Reply {
        let mut objects = Vec::with_capacity(coupon.parts.len());
        for (name, mesh) in &coupon.parts {
            let b = mesh.bounds();
            let at = [(b.min[0] + b.max[0]) * 0.5, (b.min[1] + b.max[1]) * 0.5];
            let mut object = match object_from_printer(name, &mesh.positions, &mesh.triangles, Some(at)) {
                Ok(o) => o, Err(e) => return Reply::err(e),
            };
            object.filament = Some(1);
            objects.push(object);
        }
        let machine = self.last.as_ref().map(|e| &e.machine).unwrap_or(&self.eval.machine).clone();
        let parts = match parts(objects, &[colour], &coupon.name, &machine, true) {
            Ok(parts) => parts, Err(e) => return Reply::err(e),
        };
        if let Err(e) = self.replace_model(FabModel::new(&coupon.name), None) { return Reply::err(e); }
        let mut settings = vec![("fab_calibration_bands".into(), coupon.bands_text())];
        settings.extend(coupon.process.clone());
        let mut reply = self.add_imported("calibration", parts, settings);
        if reply.update.is_none() {
            reply.update = self.compiled.clone().zip(self.last.clone());
        }
        reply
    }
}
