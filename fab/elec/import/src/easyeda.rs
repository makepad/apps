//! The LCSC/EasyEDA part source: the one module that names the real service (a data-source label).
//!
//! Requests (the service's public endpoints, facts about the service, `eparts/format-notes.md`): the
//! component by its LCSC number, then the 3D model by the uuid the component's footprint names. Each
//! request carries the number or the uuid alone, over HTTPS, with the network layer's generic client
//! name. Made only when a person imports a part ([`crate::library`]).
//!
//! The converter reads the symbol, copper and placement from the supplier's component, and places its
//! OBJ in the board's millimetre, Y-up frame. Unsupported drawings are counted, never made into copper.

use crate::fit::{fit, Land, REACH, SHIFT};
use crate::library::{Converted, ImportError, Raw, Source};
use crate::mesh::linear_to_srgb;
use crate::part::{write_part, BodyOut, PadKindOut, PadOut, PadShapeOut, PartOut, PinOut, Side, Size, StrokeOut, SymbolOut};
use crate::{Affine, Issue, Material, Mesh};
use makepad_strict_json::Value;
use std::collections::{HashMap, HashSet};

/// The supplier's part numbers: `C` and digits.
pub struct Lcsc;

/// The component response (symbol, footprint, and the reference to the 3D model).
pub const COMPONENT: &str = "component.json";
/// The 3D model response (OBJ text with its materials inline).
pub const MODEL: &str = "model.obj";

/// Separates this supplier's metals (display Ks/Kd about 0.80 for gold, 0.88 for bright tin)
/// from its Ks = Kd/2 plastics (0.50), using the brightest diffuse channel.
const METAL_SPECULAR_RATIO: f32 = 0.55;

/// Applies this supplier's own material pattern: its OBJ carries no Pm and writes Ks = Kd/2
/// on plastics and ceramics, with a larger Ks on metals. The display-space ratio at the brightest
/// diffuse channel selects the finish; decoded Kd stays the base colour. Without Ks nothing changes.
pub fn supplier_finish(material: &mut Material) {
    let Some(specular) = material.specular else { return };
    let base = [0, 1, 2].map(|i| linear_to_srgb(material.base_color[i]));
    let mut i = 0;
    for channel in 1..3 {
        if base[channel] > base[i] { i = channel; }
    }
    let ratio = if base[i] == 0.0 { 0.0 } else { linear_to_srgb(specular[i]) / base[i] };
    if ratio >= METAL_SPECULAR_RATIO {
        material.metallic = 1.0;
        material.roughness = 0.25;
    } else {
        material.metallic = 0.0;
    }
}

/// The converter's version: 2 places the model on its drawn outline instead of `c_origin`; 3 writes
/// polygon pads' outlines without repeated or collinear points and refuses a polygon that crosses itself; 4
/// decodes the model's display colours to linear and finishes the supplier's metals (`supplier_finish`); 5
/// names an LED model's lens material `lens` (`name_lens`); 6 warns when the placed model does not sit on its
/// pads (`fit_warning`); 7 reads a through pad's plated flag as the supplier spells it (`yes` as well as `Y`).
const VERSION: u32 = 7;

/// Nesting the component's JSON reaches (the footprint's SVG nodes nest further, in their own strings).
const DEPTH: u32 = 64;

impl Lcsc {
    pub fn component_url(number: &str) -> String {
        format!("https://easyeda.com/api/products/{number}/components?version=6.4.19.5")
    }

    pub fn model_url(uuid: &str) -> String {
        format!("https://modules.easyeda.com/3dmodel/{uuid}")
    }

    /// The uuid of the 3D model the component's footprint places: the `SVGNODE` shape whose
    /// `attrs.c_etype` is `outline3D`. None when the footprint places no model; an issue when the
    /// response is not a component.
    pub fn model_uuid(component: &[u8]) -> Result<Option<String>, Issue> {
        let json = makepad_strict_json::parse_depth_last_key_wins(component, DEPTH)
            .map_err(|e| Issue::new(0, "json", e))?;
        if json.get("success").and_then(|v| v.as_bool()) != Some(true) {
            let message = json.get("message").and_then(|v| v.as_str()).unwrap_or("no message");
            return Err(Issue::new(0, "success", format!("the service answered without a component: {message}")));
        }
        let shapes = json
            .get("result")
            .and_then(|r| r.get("packageDetail"))
            .and_then(|p| p.get("dataStr"))
            .and_then(|d| d.get("shape"))
            .and_then(|s| s.as_arr())
            .unwrap_or(&[]);
        for shape in shapes.iter().filter_map(|s| s.as_str()) {
            let Some(node) = shape.strip_prefix("SVGNODE~") else { continue };
            let node = makepad_strict_json::parse_depth_last_key_wins(node.as_bytes(), DEPTH)
                .map_err(|e| Issue::new(0, "SVGNODE", e))?;
            let Some(attrs) = node.get("attrs") else { continue };
            if attrs.get("c_etype").and_then(|v| v.as_str()) != Some("outline3D") {
                continue;
            }
            let uuid = attrs.get("uuid").and_then(|v| v.as_str()).unwrap_or("");
            if uuid.is_empty() || uuid.len() > 64 || !uuid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Issue::new(0, "SVGNODE", format!("the 3D model's uuid {uuid:?} is not hex digits")));
            }
            return Ok(Some(uuid.to_string()));
        }
        Ok(None)
    }
}

impl Source for Lcsc {
    fn folder(&self) -> &'static str {
        "lcsc"
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn number(&self, text: &str) -> Result<String, Issue> {
        let t = text.trim();
        let digits = t.strip_prefix('C').or_else(|| t.strip_prefix('c')).unwrap_or("");
        if digits.is_empty() || digits.len() > 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Issue::new(0, "number", format!("{t:?} is not an LCSC part number (C and digits)")));
        }
        Ok(format!("C{digits}"))
    }

    fn fetch(&self, number: &str, get: &mut dyn FnMut(&str) -> Result<Vec<u8>, ImportError>, raw: &mut Raw) -> Result<(), ImportError> {
        let url = Lcsc::component_url(number);
        let component = get(&url)?;
        let uuid = Lcsc::model_uuid(&component)
            .map_err(|issue| ImportError::Response { number: number.to_string(), url: url.clone(), issue })?;
        raw.put(COMPONENT, component);
        if let Some(uuid) = uuid {
            raw.put(MODEL, get(&Lcsc::model_url(&uuid))?);
        }
        Ok(())
    }

    fn convert(&self, number: &str, raw: &Raw) -> Result<Converted, Issue> {
        convert(self, &self.number(number)?, raw)
    }
}

const U: f64 = 0.254;

fn convert(source: &Lcsc, number: &str, raw: &Raw) -> Result<Converted, Issue> {
    let bytes = raw.get(COMPONENT).ok_or_else(|| Issue::new(0, COMPONENT, "the component is missing"))?;
    let json = makepad_strict_json::parse_depth_last_key_wins(bytes, DEPTH)
        .map_err(|e| Issue::new(0, "json", e))?;
    if json.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(Issue::new(0, "success", "the service answered without a component"));
    }
    let result = object(&json, "result")?;
    let symbol = object(result, "dataStr")?;
    let footprint = object(object(result, "packageDetail")?, "dataStr")?;
    let head = object(footprint, "head")?;
    let origin = [json_number(head, "x", 0, "x")?, json_number(head, "y", 0, "y")?];
    let symbol_para = object(object(symbol, "head")?, "c_para")?;
    let prefix: String = string(symbol_para, "pre")?.unwrap_or("")
        .chars().filter(|c| *c != '?' && !c.is_ascii_digit()).collect();
    let mut warnings = Vec::new();
    let kind = part_kind(&prefix, &mut warnings);
    let mut pads = Vec::new();
    let mut graphics = Vec::new();
    let mut body_box = Bounds::default();
    let mut pad_box = Bounds::default();
    let mut placement = None;
    let mut thru = false;
    let mut omitted: Vec<(String, String, usize)> = Vec::new();
    for (index, value) in shapes(footprint)?.iter().enumerate() {
        let line = record_line(index);
        let text = value.as_str().ok_or_else(|| Issue::new(line, "shape", "a record must be a string"))?;
        let record = Record::new(text, line);
        let mut left_out = None;
        match record.kind() {
            "PAD" => {
                let pad = read_pad(&record, origin, number)?;
                thru |= matches!(pad.kind, PadKindOut::Thru { .. });
                pads.push(pad);
            }
            "HOLE" => pads.push(read_hole(&record, origin)?),
            "TRACK" => {
                let layer = record.number(2)?;
                if layer == 3.0 || layer == 99.0 {
                    let points = board_points(record.field(4)?, origin, line, "TRACK", 2)?;
                    if layer == 3.0 {
                        graphics.push(StrokeOut { layer: "F.SilkS".into(), points, w: record.positive(1)? * U });
                    } else {
                        for point in points { body_box.add(point); }
                    }
                }
                if layer != 3.0 { left_out = Some(layer.to_string()); }
            }
            "CIRCLE" => {
                let layer = record.number(5)?;
                if layer == 3.0 {
                    let centre = board([record.number(1)?, record.number(2)?], origin, line, "CIRCLE")?;
                    let radius = record.positive(3)? * U;
                    let mut points = Vec::with_capacity(33);
                    for i in 0..32 {
                        let (s, c) = (i as f64 * std::f64::consts::TAU / 32.0).sin_cos();
                        points.push([finite(centre[0] + radius * c, line, "CIRCLE")?,
                            finite(centre[1] + radius * s, line, "CIRCLE")?]);
                    }
                    if let Some(first) = points.first().copied() { points.push(first); }
                    graphics.push(StrokeOut { layer: "F.SilkS".into(), points, w: record.positive(4)? * U });
                } else {
                    left_out = Some(layer.to_string());
                }
            }
            "SOLIDREGION" => {
                let layer = record.number(1)?;
                if layer == 99.0 {
                    for point in path_points(record.field(3)?, line, &mut warnings)? {
                        body_box.add(board(point, origin, line, "SOLIDREGION")?);
                    }
                }
                left_out = Some(layer.to_string());
            }
            "SVGNODE" => {
                let node = text.strip_prefix("SVGNODE~").ok_or_else(|| record.issue("missing node JSON"))?;
                let node = makepad_strict_json::parse_depth_last_key_wins(node.as_bytes(), DEPTH)
                    .map_err(|e| record.issue(e))?;
                let attrs = node.get("attrs").ok_or_else(|| record.issue("missing attrs"))?;
                if attrs.get("c_etype").and_then(Value::as_str) == Some("outline3D") && placement.is_none() {
                    placement = Some(Placement::read(&node, attrs, line)?);
                }
            }
            // The notes do not define layer fields for other drawing kinds.
            _ => left_out = Some("unknown".into()),
        }
        if let Some(layer) = left_out {
            if let Some((_, _, count)) = omitted.iter_mut().find(|(k, l, _)| k == record.kind() && *l == layer) {
                *count += 1;
            } else {
                omitted.push((record.kind().into(), layer, 1));
            }
        }
    }
    if pads.is_empty() {
        return Err(Issue::new(0, "shape", "the footprint has no pads"));
    }
    for pad in &pads {
        let size = match pad.kind {
            PadKindOut::Smd { size } => size,
            PadKindOut::Thru { d: Size::Round(d), .. } => [d, d],
            PadKindOut::Thru { d: Size::Wh(w, h), .. } => [w, h],
        };
        for sign in [-0.5, 0.5] {
            pad_box.add([finite(pad.at[0] + sign * size[0], 0, "shape")?,
                finite(pad.at[1] + sign * size[1], 0, "shape")?]);
        }
    }
    for (kind, layer, count) in omitted {
        warnings.push(Issue::new(0, &kind, format!("{kind} on layer {layer}: {count} left out (drawings only)")));
    }
    let (pins, pad_map) = read_pins(symbol, kind, &pads, &mut warnings)?;
    let mut placed = place_model(raw, placement.as_ref(), footprint, origin, &mut warnings)?;
    if kind == "led" {
        if let Some(mesh) = &mut placed { name_lens(mesh); }
    }
    if let Some(mesh) = &placed {
        fit_warning(mesh, &pads, placement.as_ref().map_or(0, |p| p.line), &mut warnings);
    }
    let mut body = None;
    if let Some(mesh) = &placed {
        if let Some((lo, hi)) = mesh.bounds() {
            if body_box.0.is_none() {
                body_box.add([lo[0] as f64, lo[1] as f64]);
                body_box.add([hi[0] as f64, hi[1] as f64]);
            }
            body = Some(BodyOut { mesh: format!("{number}.glb"), height: (hi[2] as f64).max(0.01), offset: [0.0; 3], rotation: [0.0; 3] });
        }
    }
    let courtyard = body_box.0.or(pad_box.0).ok_or_else(|| Issue::new(0, "shape", "the footprint has no bounds"))?;
    let mut fields = vec![("supplier".into(), source.folder().into()), ("supplier_part".into(), number.into())];
    if let Some(mpn) = string(symbol_para, "Manufacturer Part")? { fields.push(("mpn".into(), mpn.into())); }
    let package = match head.get("c_para") {
        Some(para) => string(para, "package")?,
        None => None,
    };
    let package = match package { Some(p) => Some(p), None => string(symbol_para, "package")? };
    if let Some(package) = package { fields.push(("package".into(), package.into())); }
    fields.extend([("source".into(), source.folder().into()), ("converter".into(), VERSION.to_string().into()),
        ("raw_sha256".into(), raw.digest())]);
    let two_terminal = matches!(kind, "resistor" | "capacitor" | "inductor" | "fuse" | "diode" | "led") && pins.len() == 2;
    let out = PartOut {
        supplier: source.folder().into(), number: number.into(), kind: kind.into(), prefix,
        symbol: if two_terminal { SymbolOut::TwoTerminal } else { SymbolOut::Box }, pins, pads, courtyard, thru,
        graphics, body, pad_map, fields, note: format!("Converted by fab-elec-import converter {VERSION} from the supplier's data."),
    };
    Ok(Converted { part: write_part(&out), mesh: placed.as_ref().map(crate::glb::write_glb), warnings })
}

fn record_line(index: usize) -> u32 {
    u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX)
}

fn object<'a>(value: &'a Value, key: &str) -> Result<&'a Value, Issue> {
    match value.get(key) {
        Some(v @ Value::Obj(_)) => Ok(v),
        _ => Err(Issue::new(0, key, "missing object")),
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>, Issue> {
    match value.get(key) {
        None => Ok(None),
        Some(Value::Str(s)) => Ok(Some(s)),
        _ => Err(Issue::new(0, key, "expected a string")),
    }
}

fn shapes(value: &Value) -> Result<&[Value], Issue> {
    value.get("shape").and_then(Value::as_arr).ok_or_else(|| Issue::new(0, "shape", "missing shape list"))
}

fn finite(value: f64, line: u32, field: &str) -> Result<f64, Issue> {
    if value.is_finite() { Ok(value) } else { Err(Issue::new(line, field, "a number is not finite")) }
}

fn number(text: &str, line: u32, field: &str) -> Result<f64, Issue> {
    let n = text.trim().parse::<f64>().map_err(|_| Issue::new(line, field, format!("{text:?} is not a number")))?;
    finite(n, line, field)
}

fn json_number(value: &Value, key: &str, line: u32, field: &str) -> Result<f64, Issue> {
    match value.get(key) {
        Some(Value::Int(n)) => Ok(*n as f64),
        Some(Value::F64(n)) => finite(*n, line, field),
        Some(Value::Str(s)) => number(s, line, field),
        _ => Err(Issue::new(line, field, format!("missing numeric {key}"))),
    }
}

struct Record<'a> {
    fields: Vec<&'a str>,
    line: u32,
}

impl<'a> Record<'a> {
    fn new(text: &'a str, line: u32) -> Self { Self { fields: text.split('~').collect(), line } }
    fn kind(&self) -> &'a str { self.fields.first().copied().unwrap_or("") }
    fn issue(&self, message: impl Into<String>) -> Issue { Issue::new(self.line, self.kind(), message) }
    fn field(&self, index: usize) -> Result<&'a str, Issue> {
        self.fields.get(index).copied().ok_or_else(|| self.issue(format!("missing field {index}")))
    }
    fn number(&self, index: usize) -> Result<f64, Issue> { number(self.field(index)?, self.line, self.kind()) }
    fn positive(&self, index: usize) -> Result<f64, Issue> {
        let n = self.number(index)?;
        if n > 0.0 { Ok(n) } else { Err(self.issue(format!("field {index} must be positive"))) }
    }
}

fn board([x, y]: [f64; 2], [ox, oy]: [f64; 2], line: u32, field: &str) -> Result<[f64; 2], Issue> {
    Ok([finite((x - ox) * U, line, field)?, finite(-(y - oy) * U, line, field)?])
}

fn numbers(text: &str, line: u32, field: &str) -> Result<Vec<f64>, Issue> {
    text.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty())
        .map(|s| number(s, line, field)).collect()
}

fn board_points(text: &str, origin: [f64; 2], line: u32, field: &str, minimum: usize) -> Result<Vec<[f64; 2]>, Issue> {
    let values = numbers(text, line, field)?;
    if values.len() % 2 != 0 || values.len() / 2 < minimum {
        return Err(Issue::new(line, field, format!("expected at least {minimum} point pairs")));
    }
    values.chunks_exact(2).map(|pair| match pair {
        &[x, y] => board([x, y], origin, line, field),
        _ => Err(Issue::new(line, field, "incomplete point")),
    }).collect()
}

fn read_pad(r: &Record<'_>, origin: [f64; 2], number: &str) -> Result<PadOut, Issue> {
    if r.fields.len() < 16 { return Err(r.issue("PAD needs at least 16 fields")); }
    let layer = r.number(6)?;
    if layer != 1.0 && layer != 11.0 {
        return Err(r.issue(format!("a pad on layer {layer} is not supported")));
    }
    let centre = [r.number(2)?, r.number(3)?];
    let (w, h) = (r.positive(4)?, r.positive(5)?);
    let same = (w - h).abs() <= 1e-6;
    let hole_r = r.number(9)?;
    let hole_len = r.number(13)?;
    if hole_r < 0.0 || hole_len < 0.0 { return Err(r.issue("hole sizes cannot be negative")); }
    let mut rot = if r.field(11)?.trim().is_empty() { 0.0 } else { (-r.number(11)?).rem_euclid(360.0) };
    if rot == 360.0 { rot = 0.0; }
    let mut outline = Vec::new();
    let shape;
    let kind = if layer == 1.0 {
        if hole_r > 0.0 { return Err(r.issue("an SMD pad cannot have a hole")); }
        let mut size = [w * U, h * U];
        shape = Some(match r.field(1)? {
            "RECT" => PadShapeOut::Rect,
            "OVAL" => PadShapeOut::Oval,
            "ELLIPSE" => if same { PadShapeOut::Circle } else { PadShapeOut::Oval },
            "POLYGON" => {
                // A land that is not the supplier's would change copper: refused, never replaced.
                outline = pad_outline(&board_points(r.field(10)?, centre, r.line, "PAD", 3)?).map_err(|fault| {
                    let pad = r.field(8).unwrap_or("");
                    r.issue(match fault {
                        OutlineFault::TooFew => format!("{number} pad {pad}: outline has fewer than three corners"),
                        OutlineFault::Crosses(p) => {
                            let at = board(centre, origin, r.line, "PAD").unwrap_or([0.0; 2]);
                            format!("{number} pad {pad}: outline crosses itself at ({}mm, {}mm)",
                                round_mm(at[0] + p[0]), round_mm(at[1] + p[1]))
                        }
                    })
                })?;
                let s = 0.5 * w.min(h) * U;
                size = [s, s];
                rot = 0.0;
                PadShapeOut::Rect
            }
            other => return Err(r.issue(format!("pad shape {other} is not supported"))),
        });
        PadKindOut::Smd { size }
    } else {
        if hole_r <= 0.0 { return Err(r.issue("a through pad needs a positive hole radius")); }
        let land = r.field(1)?;
        let d = match land {
            "RECT" | "OVAL" => Size::Wh(w * U, h * U),
            "ELLIPSE" => if same { Size::Round(w * U) } else { Size::Wh(w * U, h * U) },
            other => return Err(r.issue(format!("through pad shape {other} is not supported"))),
        };
        let width = hole_r * (2.0 * U);
        let drill = if hole_len == 0.0 {
            Size::Round(width)
        } else {
            let points = board_points(r.field(14)?, centre, r.line, "PAD", 2)?;
            let &[a, b] = points.as_slice() else { return Err(r.issue("a slot needs exactly two centre-line points")); };
            let dx = finite(b[0] - a[0], r.line, "PAD")?;
            let dy = finite(b[1] - a[1], r.line, "PAD")?;
            if dx == 0.0 && dy == 0.0 { return Err(r.issue("a slot centre line has no direction")); }
            let (s, c) = rot.to_radians().sin_cos();
            let x = finite(c * dx + s * dy, r.line, "PAD")?;
            let y = finite(-s * dx + c * dy, r.line, "PAD")?;
            if x.abs() > y.abs() { Size::Wh(hole_len * U, width) } else { Size::Wh(width, hole_len * U) }
        };
        shape = None;
        // Suppliers spell the flag `Y`/`N` or `yes`/`no`; a land ring on every layer with any other
        // flag is refused rather than guessed, since plating is copper.
        let plated = match r.field(15)?.trim() {
            f if f.eq_ignore_ascii_case("y") || f.eq_ignore_ascii_case("yes") => true,
            f if f.eq_ignore_ascii_case("n") || f.eq_ignore_ascii_case("no") => false,
            other => return Err(r.issue(format!("{number} pad {}: plated flag {other:?} is neither yes nor no",
                r.field(8).unwrap_or("")))),
        };
        PadKindOut::Thru { d, drill, square: land == "RECT", plated }
    };
    Ok(PadOut { number: r.field(8)?.into(), at: board(centre, origin, r.line, "PAD")?,
        kind, shape, rot, ratio: None, outline })
}

/// Why a polygon pad's points are no outline the language takes.
enum OutlineFault {
    /// Fewer than three corners remain.
    TooFew,
    /// Two edges that are not neighbours meet here (pad mm).
    Crosses([f64; 2]),
}

/// A polygon pad's outline as the language takes it (pad mm, on the written 0.1 µm grid): no
/// repeated point, none on the line through its neighbours, no closing copy of the first. Supplier
/// polygons repeat corners and run collinear joins (a USB-C receptacle's merged pads); dropping
/// those points is exact, the land unchanged. O(n²) in the polygon's points.
fn pad_outline(points: &[[f64; 2]]) -> Result<Vec<[f64; 2]>, OutlineFault> {
    let mut ring: Vec<[i64; 2]> = Vec::with_capacity(points.len());
    for p in points {
        let q = [(p[0] * 1e4).round() as i64, (p[1] * 1e4).round() as i64];
        if ring.last() != Some(&q) { ring.push(q); }
    }
    let cross = |a: [i64; 2], b: [i64; 2], c: [i64; 2]| -> i128 {
        (b[0] - a[0]) as i128 * (c[1] - a[1]) as i128 - (b[1] - a[1]) as i128 * (c[0] - a[0]) as i128
    };
    // A repeat (the closing copy included) is on the line through its neighbours too.
    loop {
        let n = ring.len();
        if n < 3 { return Err(OutlineFault::TooFew); }
        let Some(i) = (0..n).find(|&i| cross(ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]) == 0) else { break };
        ring.remove(i);
    }
    let within = |p: [i64; 2], a: [i64; 2], b: [i64; 2]| (0..2).all(|k| a[k].min(b[k]) <= p[k] && p[k] <= a[k].max(b[k]));
    let n = ring.len();
    for i in 0..n {
        for j in i + 2..n {
            if i == 0 && j == n - 1 { continue; }
            let (a, b, c, d) = (ring[i], ring[(i + 1) % n], ring[j], ring[(j + 1) % n]);
            let (ac, ad, ca, cb) = (cross(a, b, c), cross(a, b, d), cross(c, d, a), cross(c, d, b));
            if ac.signum() * ad.signum() > 0 || ca.signum() * cb.signum() > 0 { continue; }
            // Touching: an end on the other edge is where they meet.
            let touch = [(ac, c, a, b), (ad, d, a, b), (ca, a, c, d), (cb, b, c, d)]
                .into_iter().find(|&(o, p, e0, e1)| o == 0 && within(p, e0, e1)).map(|(_, p, _, _)| p);
            let at = match touch {
                Some(p) => [p[0] as f64, p[1] as f64],
                None if ac == 0 && ad == 0 => continue, // collinear, apart
                None => {
                    let t = ca as f64 / (ca - cb) as f64;
                    [a[0] as f64 + t * (b[0] - a[0]) as f64, a[1] as f64 + t * (b[1] - a[1]) as f64]
                }
            };
            return Err(OutlineFault::Crosses([at[0] / 1e4, at[1] / 1e4]));
        }
    }
    Ok(ring.iter().map(|q| [q[0] as f64 / 1e4, q[1] as f64 / 1e4]).collect())
}

/// A length in mm to 0.1 µm, for messages; never `-0`.
fn round_mm(v: f64) -> f64 {
    let r = (v * 1e4).round() / 1e4;
    if r == 0.0 { 0.0 } else { r }
}

fn read_hole(r: &Record<'_>, origin: [f64; 2]) -> Result<PadOut, Issue> {
    let d = Size::Round(r.positive(3)? * (2.0 * U));
    Ok(PadOut {
        number: String::new(), at: board([r.number(1)?, r.number(2)?], origin, r.line, "HOLE")?,
        kind: PadKindOut::Thru { d, drill: d, square: false, plated: false },
        shape: None, rot: 0.0, ratio: None, outline: Vec::new(),
    })
}

/// Accumulates geometry without inventing a zero-sized box at the origin.
#[derive(Default)]
struct Bounds(Option<[[f64; 2]; 2]>);

impl Bounds {
    fn add(&mut self, p: [f64; 2]) {
        self.0 = Some(match self.0 {
            None => [p, p],
            Some([lo, hi]) => [[lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]],
        });
    }
}

/// Only absolute M/L points contribute to a body box; curves and relative commands stay drawings.
fn path_points(text: &str, line: u32, warnings: &mut Vec<Issue>) -> Result<Vec<[f64; 2]>, Issue> {
    let mut chars = text.chars().peekable();
    let mut command = None;
    let mut pending = None;
    let mut points = Vec::new();
    let mut skipped = String::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() || c == ',' { continue; }
        if c.is_ascii_alphabetic() {
            if pending.is_some() { return Err(Issue::new(line, "SOLIDREGION", "incomplete path point")); }
            if !"MmLlZzHhVvCcSsQqTtAa".contains(c) {
                return Err(Issue::new(line, "SOLIDREGION", format!("invalid path command {c}")));
            }
            command = Some(c);
            if !matches!(c, 'M' | 'L' | 'Z') && !skipped.contains(c) { skipped.push(c); }
            continue;
        }
        let mut token = String::from(c);
        while let Some(&next) = chars.peek() {
            let exponent_sign = (next == '-' || next == '+') && (token.ends_with('e') || token.ends_with('E'));
            if next.is_ascii_digit() || next == '.' || matches!(next, 'e' | 'E') || exponent_sign {
                token.push(next);
                let _ = chars.next();
            } else { break; }
        }
        let value = number(&token, line, "SOLIDREGION")?;
        match command {
            Some('M' | 'L') => {
                if let Some(x) = pending.take() { points.push([x, value]); } else { pending = Some(value); }
            }
            None | Some('Z') => return Err(Issue::new(line, "SOLIDREGION", "a path point needs M or L")),
            _ => {}
        }
    }
    if pending.is_some() { return Err(Issue::new(line, "SOLIDREGION", "incomplete path point")); }
    if !skipped.is_empty() {
        warnings.push(Issue::new(line, "SOLIDREGION", format!("path commands {skipped} are left out of the body box")));
    }
    if points.is_empty() && skipped.is_empty() { return Err(Issue::new(line, "SOLIDREGION", "the path has no points")); }
    Ok(points)
}

fn part_kind(prefix: &str, warnings: &mut Vec<Issue>) -> &'static str {
    match prefix.to_uppercase().as_str() {
        "R" => "resistor", "C" => "capacitor", "L" => "inductor", "F" => "fuse", "D" => "diode",
        "LED" => "led", "Q" => "transistor", "Y" | "X" => "crystal", "SW" => "switch",
        "J" | "P" | "CN" | "CON" | "USB" | "USBC" => "connector", "U" | "IC" => "ic",
        _ => {
            warnings.push(Issue::new(0, "pre", format!("prefix {prefix:?} is unknown; the kind is ic")));
            "ic"
        }
    }
}

struct Pin {
    name: String,
    number: String,
    x: f64,
    rot: Option<f64>,
    kind: &'static str,
    line: u32,
}

type PadMap = Vec<(String, Vec<String>)>;

fn read_pins(symbol: &Value, kind: &str, pads: &[PadOut], warnings: &mut Vec<Issue>) -> Result<(Vec<PinOut>, PadMap), Issue> {
    let passive = matches!(kind, "resistor" | "capacitor" | "inductor" | "fuse" | "crystal" | "led" | "diode" | "connector");
    let mut records = Vec::new();
    let mut drawn = false;
    for (index, value) in shapes(symbol)?.iter().enumerate() {
        let line = record_line(index);
        let text = value.as_str().ok_or_else(|| Issue::new(line, "shape", "a record must be a string"))?;
        let mut groups = text.split("^^");
        let r = Record::new(groups.next().unwrap_or(""), line);
        if r.kind() != "P" { drawn = true; continue; }
        let name_group = groups.nth(2).ok_or_else(|| r.issue("missing pin name group"))?;
        let name_record = Record::new(name_group, line);
        let name = name_record.fields.get(4).copied().ok_or_else(|| r.issue("missing pin name"))?;
        let number = r.field(3)?;
        if number.is_empty() { return Err(r.issue("a pin number is empty")); }
        let name = if name.is_empty() { number } else { name };
        let x = r.number(4)?;
        r.number(5)?;
        let rot = if r.field(6)?.trim().is_empty() { None } else { Some(r.number(6)?) };
        let kind = if passive { "passive" } else { match r.field(2)? {
            "1" => "in", "2" => "out", "3" => "bidi", _ => "unspecified",
        } };
        records.push(Pin { name: name.into(), number: number.into(), x, rot, kind, line });
    }
    if drawn { warnings.push(Issue::new(0, "symbol", "the drawn symbol is replaced by the standard symbol")); }
    let count = records.len().max(1) as f64;
    let mean = records.iter().try_fold(0.0, |sum, p| finite(sum + p.x / count, p.line, "P"))?;
    let pad_numbers: HashSet<&str> = pads.iter().filter(|p| !p.number.is_empty()).map(|p| p.number.as_str()).collect();
    let mut used = HashSet::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    let mut pins = Vec::new();
    let mut map: PadMap = Vec::new();
    let mut overrides = 0usize;
    for pin in records {
        if !pad_numbers.contains(pin.number.as_str()) {
            return Err(Issue::new(pin.line, "P", format!("pin {} ({}) has no pad", pin.name, pin.number)));
        }
        used.insert(pin.number.clone());
        if let Some(entry) = by_name.get(&pin.name).and_then(|&i| map.get_mut(i)) {
            if !entry.1.contains(&pin.number) { entry.1.push(pin.number); }
            continue;
        }
        let upper = pin.name.to_uppercase();
        let supply = !passive && ["GND", "AGND", "DGND", "PGND", "VSS", "VDD", "VCC", "AVDD", "DVDD",
            "IOVDD", "VBUS", "VIN"].iter().any(|prefix| upper.starts_with(*prefix));
        if supply && pin.kind != "power_in" { overrides += 1; }
        let side = match pin.rot {
            Some(0.0) => Side::Right, Some(90.0) => Side::Top, Some(180.0) => Side::Left, Some(270.0) => Side::Bottom,
            _ => if pin.x > mean { Side::Right } else { Side::Left },
        };
        by_name.insert(pin.name.clone(), pins.len());
        pins.push(PinOut { name: pin.name.clone(), kind: if supply { "power_in" } else { pin.kind }, side });
        map.push((pin.name, vec![pin.number]));
    }
    if overrides > 0 {
        warnings.push(Issue::new(0, "P", format!("{overrides} pins named as supplies are power inputs")));
    }
    let mut unclaimed = Vec::new();
    for pad in pads {
        if !pad.number.is_empty() && !used.contains(&pad.number) && !unclaimed.contains(&pad.number.as_str()) {
            unclaimed.push(pad.number.as_str());
        }
    }
    if !unclaimed.is_empty() {
        warnings.push(Issue::new(0, "PAD", format!("pads with no symbol pin are kept: {}", unclaimed.join(", "))));
    }
    Ok((pins, map))
}

struct Placement {
    /// Where the model's centre sits (canvas units): the centre of the outline the supplier draws
    /// for it, which is where its own footprint shows the model. `c_origin` only when the outline
    /// has no points: on some parts (an 0805 resistor, an 0603 capacitor) it lies up to 1.4 mm off
    /// the drawn outline, and on others in another frame altogether.
    origin: [f64; 2],
    rotation: [f64; 3],
    z: f64,
    line: u32,
}

impl Placement {
    fn read(node: &Value, attrs: &Value, line: u32) -> Result<Self, Issue> {
        let list = |key: &str| -> Result<Vec<f64>, Issue> {
            let text = attrs.get(key).and_then(Value::as_str)
                .ok_or_else(|| Issue::new(line, "SVGNODE", format!("missing {key}")))?;
            numbers(text, line, "SVGNODE")
        };
        let origin = list("c_origin")?;
        let &[x, y] = origin.as_slice() else { return Err(Issue::new(line, "SVGNODE", "c_origin needs two numbers")); };
        let origin = outline_centre(node, line)?.unwrap_or([x, y]);
        let rotation = list("c_rotation")?;
        let &[rx, ry, rz] = rotation.as_slice() else {
            return Err(Issue::new(line, "SVGNODE", "c_rotation needs three numbers"));
        };
        Ok(Self { origin, rotation: [rx, ry, rz], z: json_number(attrs, "z", line, "SVGNODE")?, line })
    }
}

/// The centre of the bounds of every point the outline's child shapes draw, or none without points.
fn outline_centre(node: &Value, line: u32) -> Result<Option<[f64; 2]>, Issue> {
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for child in node.get("childNodes").and_then(Value::as_arr).unwrap_or(&[]) {
        let Some(points) = child.get("attrs").and_then(|a| a.get("points")).and_then(Value::as_str) else { continue };
        let values = numbers(points, line, "SVGNODE")?;
        if values.len() % 2 != 0 {
            return Err(Issue::new(line, "SVGNODE", "the model outline's points need x, y pairs"));
        }
        for p in values.chunks_exact(2) {
            for i in 0..2 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
    }
    Ok((lo[0] <= hi[0]).then(|| [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5]))
}

fn place_model(raw: &Raw, placement: Option<&Placement>, footprint: &Value, origin: [f64; 2],
    warnings: &mut Vec<Issue>) -> Result<Option<Mesh>, Issue>
{
    let Some(placement) = placement else {
        warnings.push(Issue::new(0, "SVGNODE", "the footprint places no 3D model"));
        return Ok(None);
    };
    let Some(bytes) = raw.get(MODEL) else { return Ok(None); };
    let line = placement.line;
    let mut read = crate::obj::read_obj(bytes, &mut |_| None).map_err(|e| Issue::new(0, MODEL, e.to_string()))?;
    warnings.extend(read.warnings.into_iter().filter(|w| w.field != "endmtl"));
    for material in &mut read.mesh.materials {
        supplier_finish(material);
        material.base_color[3] = 1.0;
    }
    let [rx, ry, rz] = placement.rotation;
    if rx != 0.0 || ry != 0.0 {
        warnings.push(Issue::new(line, "SVGNODE", "model rotation about x or y is applied unconfirmed"));
    }
    let rotation = Affine::rotate([0.0, 0.0, 1.0], rz.rem_euclid(360.0).to_radians())
        .mul(&Affine::rotate([0.0, 1.0, 0.0], ry.rem_euclid(360.0).to_radians()))
        .mul(&Affine::rotate([1.0, 0.0, 0.0], rx.rem_euclid(360.0).to_radians()));
    read.mesh.transform(&rotation);
    let (lo, hi) = model_bounds(&read.mesh)?;
    let bbox = object(footprint, "BBox")?;
    let x = json_number(bbox, "x", 0, "BBox")?;
    let y = json_number(bbox, "y", 0, "BBox")?;
    let w = json_number(bbox, "width", 0, "BBox")?;
    let h = json_number(bbox, "height", 0, "BBox")?;
    if w < 0.0 || h < 0.0 { return Err(Issue::new(0, "BBox", "bounding-box sizes cannot be negative")); }
    let [cx, cy] = placement.origin;
    let outside = cx < finite(x - w, 0, "BBox")? || cx > finite(x + 2.0 * w, 0, "BBox")?
        || cy < finite(y - h, 0, "BBox")? || cy > finite(y + 2.0 * h, 0, "BBox")?;
    let centre = if outside {
        warnings.push(Issue::new(line, "SVGNODE", format!(
            "the model origin ({cx}, {cy}) is outside the footprint; the model is centred on the footprint origin")));
        [0.0, 0.0]
    } else { board([cx, cy], origin, line, "SVGNODE")? };
    read.mesh.transform(&Affine::translate([
        centre[0] - (lo[0] as f64 + hi[0] as f64) * 0.5,
        centre[1] - (lo[1] as f64 + hi[1] as f64) * 0.5,
        placement.z * U - lo[2] as f64,
    ]));
    model_bounds(&read.mesh)?;
    Ok(Some(read.mesh))
}

/// The material the 3D view lights on an LED (its emission colour comes from the part's emitter, never
/// from this material). The lens is the material that reaches the model's top; on a tie, the one that
/// spans the larger area across the board. Any other material the supplier named `lens` is renamed, so
/// one material carries the name.
fn name_lens(mesh: &mut Mesh) {
    const TIE: f32 = 1e-4;
    let mut lens: Option<(usize, f32, f32)> = None;
    for m in 0..mesh.materials.len() {
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in mesh.prims.iter().filter(|p| p.material == m).flat_map(|p| &p.positions) {
            for i in 0..3 { lo[i] = lo[i].min(p[i]); hi[i] = hi[i].max(p[i]); }
        }
        if lo[2] > hi[2] { continue; }
        let area = (hi[0] - lo[0]) * (hi[1] - lo[1]);
        if lens.map_or(true, |(_, top, a)| hi[2] > top + TIE || (hi[2] >= top - TIE && area > a)) {
            lens = Some((m, hi[2], area));
        }
    }
    let Some((lens, ..)) = lens else { return };
    for (m, material) in mesh.materials.iter_mut().enumerate() {
        if m == lens { material.name = "lens".into(); } else if material.name == "lens" { material.name = format!("lens {m}"); }
    }
}

/// The lands a placed model must reach: each surface pad's land (a polygon pad's outline with its
/// anchor) and each hole's drill, in the footprint frame.
fn lands(pads: &[PadOut]) -> Vec<Land> {
    pads.iter().map(|pad| match pad.kind {
        PadKindOut::Smd { size } => {
            let mut land = Land::turned(pad.at, size, pad.rot, false);
            for p in &pad.outline {
                for i in 0..2 {
                    land.lo[i] = land.lo[i].min(pad.at[i] + p[i]);
                    land.hi[i] = land.hi[i].max(pad.at[i] + p[i]);
                }
            }
            land
        }
        PadKindOut::Thru { drill, .. } => {
            let size = match drill { Size::Round(d) => [d, d], Size::Wh(w, h) => [w, h] };
            Land::turned(pad.at, size, pad.rot, true)
        }
    }).collect()
}

/// Warns when the placed model does not sit on its own footprint (`fit`): the pads and holes it does
/// not reach, or how far its contact sits off them on average. The model is kept as placed.
fn fit_warning(mesh: &Mesh, pads: &[PadOut], line: u32, warnings: &mut Vec<Issue>) {
    if pads.is_empty() { return; }
    let fit = fit(mesh, &lands(pads));
    if !fit.missed.is_empty() {
        let names: Vec<String> = fit.missed.iter().map(|&i| {
            let pad = &pads[i];
            if pad.number.is_empty() {
                format!("the hole at ({}mm, {}mm)", round_mm(pad.at[0]), round_mm(pad.at[1]))
            } else {
                format!("pad {}", pad.number)
            }
        }).collect();
        warnings.push(Issue::new(line, "SVGNODE", format!("the 3D model does not reach {} (within {REACH}mm)", names.join(", "))));
    }
    if fit.shift_len() > SHIFT {
        warnings.push(Issue::new(line, "SVGNODE", format!(
            "the 3D model sits {:.2}mm off its pads on average (more than {SHIFT}mm)", fit.shift_len())));
    }
}

fn model_bounds(mesh: &Mesh) -> Result<([f32; 3], [f32; 3]), Issue> {
    if mesh.points().any(|p| p.iter().any(|v| !v.is_finite())) {
        return Err(Issue::new(0, MODEL, "placed model coordinates are not finite"));
    }
    mesh.bounds().ok_or_else(|| Issue::new(0, MODEL, "the model has no geometry"))
}
