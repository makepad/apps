//! OBJ polygons and MTL factors, read at import time. Material libraries are supplied by the caller;
//! the reader does no file or network I/O. Inline and library materials use the same statement reader.

use crate::mesh::srgb_to_linear;
use crate::{Issue, Material, MeshBuilder, MeshRead, Normals};
use std::collections::HashMap;

/// CAD exports rarely give normals; 30 degrees keeps box edges sharp while rounding cylinders.
pub const DEFAULT_CREASE: f32 = std::f32::consts::PI / 6.0;

/// Reads an OBJ. Materials come from the MTL files its `mtllib` statements name, fetched through
/// `mtl` by the name as written (None: not available, a warning), and from material statements written
/// inline in the OBJ itself.
pub fn read_obj(bytes: &[u8], mtl: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Result<MeshRead, Issue> {
    let mut materials = Materials::default();
    let mut inline = MaterialReader::default();
    let mut warnings = Warnings::default();
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut texcoords = 0usize;
    let mut current = None;
    let mut domain = 0;
    let mut angle = DEFAULT_CREASE;
    let mut saw_face = false;

    for (line, bytes) in logical_lines(bytes) {
        let text = String::from_utf8_lossy(&bytes);
        let Some((field, rest)) = statement(&text) else { continue };
        match field {
            "v" => {
                let values = numbers(rest, line, field)?;
                let position = match values.as_slice() {
                    &[x, y, z] => [x, y, z],
                    &[x, y, z, w] => {
                        if w == 0.0 {
                            return Err(Issue::new(line, field, "vertex weight is zero"));
                        }
                        let p = [x / w, y / w, z / w];
                        if p.iter().any(|v| !v.is_finite()) {
                            return Err(Issue::new(line, field, "homogeneous position is not finite"));
                        }
                        p
                    }
                    &[x, y, z, _, _, _] => {
                        warnings.count(WarningKind::Colours, line, field, |n| {
                            format!("vertex colours are left out ({n} vertices)")
                        });
                        [x, y, z]
                    }
                    _ => return Err(Issue::new(line, field, "v takes 3, 4 or 6 numbers")),
                };
                positions.push(position);
            }
            "vn" => {
                let values = numbers(rest, line, field)?;
                let &[x, y, z] = values.as_slice() else {
                    return Err(Issue::new(line, field, "vn takes 3 numbers"));
                };
                normals.push([x, y, z]);
            }
            "vt" => {
                let values = numbers(rest, line, field)?;
                if !(1..=3).contains(&values.len()) {
                    return Err(Issue::new(line, field, "vt takes 1, 2 or 3 numbers"));
                }
                texcoords = texcoords.saturating_add(1);
            }
            "f" => {
                saw_face = true;
                let mut corners = Vec::new();
                let mut given = Vec::new();
                for token in tokens(rest) {
                    let (p, n) = corner(token, &positions, texcoords, &normals, line)?;
                    corners.push(p);
                    if let Some(n) = n {
                        given.push(n);
                    }
                }
                if corners.len() < 3 {
                    warnings.count(WarningKind::ShortFaces, line, field, |n| {
                        format!("{n} faces with under three corners are left out")
                    });
                    continue;
                }
                let material = *current.get_or_insert_with(|| materials.slot("default"));
                let shading = if given.len() == corners.len() {
                    Normals::Given(&given)
                } else {
                    Normals::Crease { domain, angle }
                };
                materials.builder.polygon(material, &corners, shading);
            }
            "s" => {
                if rest == "off" || rest == "0" {
                    (domain, angle) = (u32::MAX, 0.0);
                } else if let Ok(n) = rest.parse::<u32>() {
                    (domain, angle) = if n == 0 { (u32::MAX, 0.0) } else { (n, DEFAULT_CREASE) };
                } else {
                    warnings.push(Issue::new(line, field, format!("smoothing group '{rest}' is invalid; treated as off")));
                    (domain, angle) = (u32::MAX, 0.0);
                }
            }
            "usemtl" => {
                let index = materials.slot(rest);
                let state = &mut materials.states[index];
                if state.first_use.is_none() {
                    let slot = warnings.items.len();
                    warnings.items.push(None);
                    state.first_use = Some((line, slot));
                }
                current = Some(index);
            }
            "mtllib" => {
                for name in tokens(rest) {
                    match mtl(name) {
                        Some(bytes) => {
                            let issues = read_materials(&bytes, &mut materials).map_err(|e| library_issue(name, e))?;
                            for issue in issues {
                                warnings.push(library_issue(name, issue));
                            }
                        }
                        None => warnings.push(Issue::new(
                            line, field, format!("{name} is not available; its materials are grey"),
                        )),
                    }
                }
            }
            "o" | "g" => {}
            "l" => warnings.count(WarningKind::Lines, line, field, |n| format!("{n} lines are left out")),
            "p" => warnings.count(WarningKind::Points, line, field, |n| format!("{n} points are left out")),
            "vp" | "cstype" | "deg" | "bmat" | "step" | "curv" | "curv2" | "surf" | "parm" | "trim"
            | "hole" | "scrv" | "sp" | "end" | "con" => {
                warnings.count(WarningKind::FreeForm, line, field, |n| {
                    format!("free-form geometry is left out ({n} statements)")
                });
            }
            "mg" | "bevel" | "c_interp" | "d_interp" | "lod" | "usemap" | "maplib" | "shadow_obj"
            | "trace_obj" | "ctech" | "stech" => {}
            _ if material_keyword(field) => inline.read(line, field, rest, &mut materials, &mut warnings)?,
            _ => warnings.unknown(line, field),
        }
    }

    inline.finish(&mut materials);
    for (material, state) in materials.builder.materials().iter().zip(&materials.states) {
        if !state.defined {
            if let Some((line, slot)) = state.first_use {
                warnings.items[slot] = Some(Issue::new(
                    line, "usemtl", format!("material {} is not defined; grey", material.name),
                ));
            }
        }
    }
    if materials.builder.degenerate > 0 {
        warnings.push(Issue::new(0, "f", format!("{} faces with no area are left out", materials.builder.degenerate)));
    }
    if materials.builder.crossing > 0 {
        warnings.push(Issue::new(0, "f", format!("{} faces cross themselves; filled as fans", materials.builder.crossing)));
    }
    if !saw_face {
        warnings.push(Issue::new(0, "f", "the file has no faces"));
    }
    Ok(MeshRead { mesh: materials.builder.finish(), warnings: warnings.finish() })
}

/// Reads one MTL file: its materials in file order, and its warnings.
pub fn read_mtl(bytes: &[u8]) -> Result<(Vec<Material>, Vec<Issue>), Issue> {
    let mut materials = Materials::default();
    let warnings = read_materials(bytes, &mut materials)?;
    // Do not finish the builder: standalone MTL factors stay unclamped, including unused materials.
    Ok((materials.builder.materials().to_vec(), warnings))
}

/// Reads libraries into the same name table as inline definitions so the first definition wins
/// across files too. Each library has its own current material, preamble and warning counts.
fn read_materials(bytes: &[u8], materials: &mut Materials) -> Result<Vec<Issue>, Issue> {
    let mut reader = MaterialReader::default();
    let mut warnings = Warnings::default();
    for (line, bytes) in logical_lines(bytes) {
        let text = String::from_utf8_lossy(&bytes);
        if let Some((field, rest)) = statement(&text) {
            reader.read(line, field, rest, materials, &mut warnings)?;
        }
    }
    reader.finish(materials);
    Ok(warnings.finish())
}

fn library_issue(name: &str, mut issue: Issue) -> Issue {
    issue.message = format!("{name}: {}", issue.message);
    issue
}

/// Strip comments on physical lines before joining them, retaining the first physical line number.
fn logical_lines(bytes: &[u8]) -> impl Iterator<Item = (u32, Vec<u8>)> + '_ {
    let mut lines = bytes.split(|&c| c == b'\n');
    let mut number = 0u32;
    std::iter::from_fn(move || {
        let first = number.saturating_add(1);
        let mut joined = Vec::new();
        loop {
            let Some(line) = lines.next() else {
                return if joined.is_empty() { None } else { Some((first, joined)) };
            };
            number = number.saturating_add(1);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let comment = line.iter().position(|&c| c == b'#').unwrap_or(line.len());
            let line = &line[..comment];
            let end = line.iter().rposition(|&c| c != b' ' && c != b'\t').map_or(0, |i| i + 1);
            let line = &line[..end];
            if let Some(line) = line.strip_suffix(b"\\") {
                joined.extend_from_slice(line);
                joined.push(b' ');
            } else {
                joined.extend_from_slice(line);
                return Some((first, joined));
            }
        }
    })
}

fn statement(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_matches([' ', '\t']);
    if text.is_empty() {
        return None;
    }
    let end = text.find([' ', '\t']).unwrap_or(text.len());
    Some((&text[..end], text[end..].trim_matches([' ', '\t'])))
}

fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split([' ', '\t']).filter(|t| !t.is_empty())
}

fn numbers(text: &str, line: u32, field: &str) -> Result<Vec<f32>, Issue> {
    tokens(text).map(|token| {
        token.parse::<f32>().ok().filter(|v| v.is_finite())
            .ok_or_else(|| Issue::new(line, field, format!("'{token}' is not a number")))
    }).collect()
}

fn scalar(text: &str, line: u32, field: &str) -> Result<f32, Issue> {
    let values = numbers(text, line, field)?;
    match values.as_slice() {
        &[v] => Ok(v),
        _ => Err(Issue::new(line, field, format!("{field} takes 1 number"))),
    }
}

fn colour(text: &str, line: u32, field: &str) -> Result<[f32; 3], Issue> {
    let values = numbers(text, line, field)?;
    match values.as_slice() {
        &[v] => Ok([v; 3]),
        &[r, g, b] => Ok([r, g, b]),
        _ => Err(Issue::new(line, field, format!("{field} takes 1 or 3 numbers"))),
    }
}

fn index(token: &str, count: usize, kind: &str, line: u32) -> Result<usize, Issue> {
    let resolved = token.parse::<i128>().ok().and_then(|n| {
        if n > 0 { n.checked_sub(1) } else if n < 0 { (count as i128).checked_add(n) } else { None }
    }).and_then(|n| usize::try_from(n).ok()).filter(|&i| i < count);
    resolved.ok_or_else(|| Issue::new(line, "f", format!("{kind} {token} is not defined ({count} so far)")))
}

fn corner(
    token: &str, positions: &[[f32; 3]], texcoords: usize, normals: &[[f32; 3]], line: u32,
) -> Result<([f32; 3], Option<[f32; 3]>), Issue> {
    let mut parts = token.split('/');
    let (v, vt, vn) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(v), None, None, None) if !v.is_empty() => (v, None, None),
        (Some(v), Some(t), None, None) if !v.is_empty() && !t.is_empty() => (v, Some(t), None),
        // An empty index is absent: `1//` (no normal written) is the vertex alone, as `1`.
        (Some(v), Some(t), Some(n), None) if !v.is_empty() => {
            (v, (!t.is_empty()).then_some(t), (!n.is_empty()).then_some(n))
        }
        _ => return Err(Issue::new(line, "f", format!("'{token}' is not a face corner"))),
    };
    let p = index(v, positions.len(), "vertex", line)?;
    if let Some(t) = vt {
        index(t, texcoords, "texture coordinate", line)?;
    }
    let n = vn.map(|n| index(n, normals.len(), "normal", line)).transpose()?;
    // Both indices were checked against these exact slices, including negative indices.
    Ok((positions[p], n.map(|i| normals[i])))
}

#[derive(Default)]
struct MaterialState {
    defined: bool,
    duplicate_warned: bool,
    texture_warned: bool,
    first_use: Option<(u32, usize)>,
}

#[derive(Default)]
struct Materials {
    builder: MeshBuilder,
    by_name: HashMap<String, usize>,
    states: Vec<MaterialState>,
}

impl Materials {
    fn slot(&mut self, name: &str) -> usize {
        if let Some(&index) = self.by_name.get(name) {
            return index;
        }
        let index = self.builder.add_material(Material::named(name));
        self.by_name.insert(name.to_owned(), index);
        self.states.push(MaterialState::default());
        index
    }

    fn define(&mut self, name: &str, line: u32, warnings: &mut Warnings) -> (usize, bool) {
        let index = self.slot(name);
        let state = &mut self.states[index];
        let keep = !state.defined;
        if !keep && !state.duplicate_warned {
            warnings.push(Issue::new(line, "newmtl", format!("material {name} is defined twice; the first stands")));
            state.duplicate_warned = true;
        }
        state.defined = true;
        (index, keep)
    }
}

struct ActiveMaterial {
    index: usize,
    keep: bool,
    material: Material,
    has_pr: bool,
}

#[derive(Default)]
struct MaterialReader {
    current: Option<ActiveMaterial>,
    preamble_warned: bool,
}

impl MaterialReader {
    fn finish(&mut self, materials: &mut Materials) {
        let Some(active) = self.current.take() else { return };
        if active.keep {
            *materials.builder.material_mut(active.index) = active.material;
        }
    }

    fn read(
        &mut self, line: u32, field: &str, rest: &str, materials: &mut Materials, warnings: &mut Warnings,
    ) -> Result<(), Issue> {
        if field == "newmtl" {
            self.finish(materials);
            let (index, keep) = materials.define(rest, line, warnings);
            self.current = Some(ActiveMaterial {
                index, keep, material: Material::named(rest), has_pr: false,
            });
            return Ok(());
        }
        let Some(active) = self.current.as_mut() else {
            if !self.preamble_warned {
                warnings.push(Issue::new(line, field, "statements before newmtl are left out"));
                self.preamble_warned = true;
            }
            return Ok(());
        };
        match field {
            "Kd" if matches!(tokens(rest).next(), Some("spectral" | "xyz")) => {
                warnings.push(Issue::new(line, field, "non-RGB colour is left out; the colour is kept"));
            }
            "Kd" => {
                let [r, g, b] = colour(rest, line, field)?.map(srgb_to_linear);
                active.material.base_color[..3].copy_from_slice(&[r, g, b]);
            }
            "Ks" => active.material.specular = Some(colour(rest, line, field)?.map(srgb_to_linear)),
            "Ke" => active.material.emissive = colour(rest, line, field)?.map(srgb_to_linear),
            "d" => {
                let alpha = if tokens(rest).next() == Some("-halo") {
                    let value = rest.strip_prefix("-halo").unwrap_or(rest).trim_matches([' ', '\t']);
                    let alpha = scalar(value, line, field)?;
                    warnings.push(Issue::new(line, field, "halo is left out"));
                    alpha
                } else {
                    scalar(rest, line, field)?
                };
                active.material.base_color[3] = alpha;
            }
            "Tr" => active.material.base_color[3] = 1.0 - scalar(rest, line, field)?,
            "Ns" => {
                let s = scalar(rest, line, field)?;
                if !active.has_pr {
                    active.material.roughness = 1.0 - (s.clamp(0.0, 1000.0) / 1000.0).sqrt();
                }
            }
            "Pr" => {
                active.material.roughness = scalar(rest, line, field)?;
                active.has_pr = true;
            }
            "Pm" => active.material.metallic = scalar(rest, line, field)?,
            "Ka" | "Tf" | "Ni" | "illum" | "sharpness" | "Ps" | "Pc" | "Pcr" | "aniso" | "anisor" => {
                let _ = numbers(rest, line, field)?;
            }
            _ if texture_keyword(field) => {
                let state = &mut materials.states[active.index];
                if !state.texture_warned {
                    warnings.push(Issue::new(line, field, "textures are left out; the colours are kept"));
                    state.texture_warned = true;
                }
            }
            _ => warnings.unknown(line, field),
        }
        Ok(())
    }
}

fn texture_keyword(field: &str) -> bool {
    field.starts_with("map_") || matches!(field, "bump" | "disp" | "decal" | "refl" | "norm")
}

fn material_keyword(field: &str) -> bool {
    matches!(field,
        "newmtl" | "Kd" | "d" | "Tr" | "Ke" | "Ns" | "Pr" | "Pm" | "Ka" | "Ks" | "Tf" | "Ni" | "illum"
        | "sharpness" | "Ps" | "Pc" | "Pcr" | "aniso" | "anisor"
    ) || texture_keyword(field)
}

#[derive(PartialEq, Eq, Hash)]
enum WarningKind {
    Colours,
    ShortFaces,
    Lines,
    Points,
    FreeForm,
    Unknown(String),
}

/// Counted warnings update their first slot. Unresolved materials reserve a slot at first use,
/// filled only after reading all definitions, so late definitions cannot reorder other warnings.
#[derive(Default)]
struct Warnings {
    items: Vec<Option<Issue>>,
    counts: HashMap<WarningKind, (usize, usize)>,
}

impl Warnings {
    fn push(&mut self, issue: Issue) {
        self.items.push(Some(issue));
    }

    fn count(&mut self, kind: WarningKind, line: u32, field: &str, message: impl FnOnce(usize) -> String) {
        if let Some((slot, count)) = self.counts.get_mut(&kind) {
            *count = count.saturating_add(1);
            if let Some(issue) = &mut self.items[*slot] {
                issue.message = message(*count);
            }
        } else {
            self.counts.insert(kind, (self.items.len(), 1));
            self.push(Issue::new(line, field, message(1)));
        }
    }

    fn unknown(&mut self, line: u32, field: &str) {
        self.count(WarningKind::Unknown(field.to_owned()), line, field, |n| {
            format!("unknown statement, left out ({n} statements)")
        });
    }

    fn finish(self) -> Vec<Issue> {
        self.items.into_iter().flatten().collect()
    }
}
