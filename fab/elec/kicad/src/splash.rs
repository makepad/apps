//! Convert a standalone imported board using the existing part and board writers.

use crate::PcbRead;
use fab_elec_circuit::{value_unit, Circuit, QtyUnit, Quantity};
use fab_elec_core::{canon::Canon, geom::ArcGeom, Angle, BBox, CompId, ContentHash, LayerId, LayerSet, Nm, Path, PolySet, Seg, Shape, P};
use fab_elec_import::part::{write_part, BodyOut, PadKindOut, PadOut, PadShapeOut, PartOut, PinOut, Side, Size, StrokeOut, SymbolOut};
use fab_elec_lang::units::{literal_text, value_text};
use fab_elec_parts::{Footprint, FpAttrs, FpTextKind, Gfx, Pad, PadKind, PadShape, PartKind, PasteRule};
use std::{collections::{BTreeMap, HashMap, HashSet}, fmt::Write, sync::Arc, time::Instant};

pub struct SplashProject {
    /// File names relative to the project folder and texts: circuit, board, then parts.
    pub docs: Vec<(String, String)>,
    /// Features the writers cannot preserve, counted over groups rather than placements.
    pub losses: BTreeMap<&'static str, usize>,
    pub parts_ms: f64,
    pub circuit_ms: f64,
    pub board_ms: f64,
}

type Losses = BTreeMap<&'static str, usize>;

fn loss(losses: &mut Losses, key: &'static str) {
    *losses.entry(key).or_default() += 1;
}

fn mm(n: Nm) -> f64 { f64::from(n) * 1e-6 }
/// An exact length literal: nm are whole, so six decimals of mm never round.
fn mm_text(n: Nm) -> String {
    let (sign, n) = if n < 0 { ("-", -i64::from(n)) } else { ("", i64::from(n)) };
    let frac = format!("{:06}", n % 1_000_000);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() { format!("{sign}{}mm", n / 1_000_000) } else { format!("{sign}{}.{frac}mm", n / 1_000_000) }
}
fn point(p: P) -> [f64; 2] { [mm(p.x), mm(p.y)] }
fn millis(start: Instant) -> f64 { start.elapsed().as_secs_f64() * 1000.0 }

// Use the part writer's escaping so both document kinds interpret strings identically.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {},
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn prefix(refdes: &str) -> &str {
    let end = refdes.bytes().take_while(u8::is_ascii_alphabetic).count();
    if end == 0 { "U" } else { &refdes[..end] }
}

fn kind(prefix: &str) -> &'static str {
    match prefix {
        "R" => "resistor", "C" => "capacitor", "L" => "inductor", "D" => "diode",
        "LED" => "led", "Q" => "transistor", "F" => "fuse", "Y" | "X" => "crystal",
        "SW" | "S" => "switch", "J" | "P" | "CN" => "connector", _ => "ic",
    }
}

/// A KiCad value text read as a quantity in `unit`: the whole text, else the first of its words
/// (split at spaces and `_ / , ; -`) that reads as one: `10k`, `4k7`, `0R`, `R47`, `100n`, `2u2`,
/// `10UF`, `16.000MHz`. A word is digits, then a decimal point or a multiplier letter in its place
/// (`4k7`), more digits, a multiplier when none stood in for the point, and the unit's symbol in any
/// case. A bare number reads only as ohms and only as the whole text (a capacitor's `100` names no
/// farads, and the `0603` of `R_0603_10k` is a size). None when no word reads.
fn value_quantity(text: &str, unit: QtyUnit) -> Option<Quantity> {
    word_quantity(text.trim(), unit, true).or_else(|| {
        text.split(|c: char| c.is_whitespace() || "_/,;-".contains(c)).find_map(|word| word_quantity(word, unit, false))
    })
}

/// The `e.part` options after `pins: [..]` that a placement's KiCad value text gives. A kind valued
/// in a quantity (a resistor's ohms .. a crystal's hertz) takes one, as `e.r` does: its text read
/// by value_quantity, the circuit then showing it as it writes it. The text itself is kept as
/// `fields: {source_value: ..}` wherever the value does not show it as written: a quantity the
/// circuit writes differently (`10UF` shows as `10uF`), or a text that reads as none (`DNP`), which
/// then gives no value and is counted (`comp.value_unread`). Every other kind takes its text as a
/// label. An absent or blank text gives nothing and is counted (`comp.value_missing`).
fn value_opts(text: Option<&str>, unit: Option<QtyUnit>, losses: &mut Losses) -> String {
    let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
        loss(losses, "comp.value_missing");
        return String::new();
    };
    let Some(unit) = unit else { return format!(" value: {}", quote(text)) };
    let kept = format!(" fields: {{source_value: {}}}", quote(text));
    match value_quantity(text, unit) {
        Some(q) if value_text(&q) == text => format!(" value: {}", literal_text(&q)),
        Some(q) => format!(" value: {}{kept}", literal_text(&q)),
        None => {
            loss(losses, "comp.value_unread");
            kept
        }
    }
}

fn word_quantity(word: &str, unit: QtyUnit, whole: bool) -> Option<Quantity> {
    let multiplier = |c: char| Some(match c {
        'p' | 'P' => -12, 'n' | 'N' => -9, 'u' | 'U' | '\u{b5}' | '\u{3bc}' => -6, 'm' => -3,
        'k' | 'K' => 3, 'M' => 6, 'G' => 9,
        // `4R7` is 4.7 ohms.
        'R' | 'r' if unit == QtyUnit::Ohm => 0,
        _ => return None,
    });
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    let int_len = digits(word);
    let (int, mut rest) = word.split_at(int_len);
    let mut power = None;
    let mut frac = "";
    if let Some(after) = rest.strip_prefix('.') {
        (frac, rest) = after.split_at(digits(after));
    } else if let Some(c) = rest.chars().next().filter(|&c| multiplier(c).is_some()) {
        let after = &rest[c.len_utf8()..];
        (frac, rest) = after.split_at(digits(after));
        power = multiplier(c);
    }
    if power.is_none() {
        // The multiplier after a decimal part (`2.2k`, `16.000MHz`); no unit's symbol starts with one.
        if let Some(c) = rest.chars().next().filter(|&c| multiplier(c).is_some()) {
            power = multiplier(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    let symbol = rest.is_empty() || match unit {
        QtyUnit::Ohm => ["ohm", "ohms"].iter().any(|s| rest.eq_ignore_ascii_case(s)) || rest == "\u{3a9}" || rest == "\u{2126}",
        QtyUnit::Farad => rest.eq_ignore_ascii_case("f"),
        QtyUnit::Henry => rest.eq_ignore_ascii_case("h"),
        QtyUnit::Hertz => rest.eq_ignore_ascii_case("hz"),
        _ => false,
    };
    let bare = power.is_none() && rest.is_empty() && (unit != QtyUnit::Ohm || !whole);
    if int.len() + frac.len() == 0 || !symbol || bare { return None; }
    let exponent = power.unwrap_or(0) - frac.len() as i32;
    let si: f64 = format!("{int}{frac}e{exponent}").parse().ok()?;
    si.is_finite().then_some(Quantity { si, unit })
}

fn ref_number(refdes: &str) -> &str {
    let start = refdes.bytes().take_while(u8::is_ascii_alphabetic).count();
    let rest = &refdes[start..];
    let end = rest.bytes().take_while(u8::is_ascii_digit).count();
    rest[..end].trim_start_matches('0')
}

fn identifier(s: &str) -> bool {
    let mut chars = s.bytes();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

// Chords span at most ten degrees, including the last partial step of an arc.
fn arc_points(out: &mut Vec<P>, c: (f64, f64), r: f64, start: f64, sweep: f64) {
    let steps = (sweep.abs() / 10f64.to_radians()).ceil().max(1.0) as usize;
    for i in 0..=steps {
        let a = start + sweep * i as f64 / steps as f64;
        out.push(P::from_f(c.0 + r * a.cos(), c.1 + r * a.sin()));
    }
}

fn path_points(path: &Path) -> Vec<P> {
    let mut points = vec![path.start];
    for (a, seg) in path.steps() {
        if let Seg::Arc { mid, end } = seg {
            if let Some(g) = ArcGeom::new(a, mid, end) {
                let steps = (g.sweep.abs() / 10f64.to_radians()).ceil().max(1.0) as usize;
                points.extend((1..steps).map(|i| g.at(i as f64 / steps as f64)));
            }
        }
        points.push(seg.end());
    }
    points
}

fn shape_poly(shape: &Shape) -> PolySet {
    let mut points = Vec::new();
    let pi = std::f64::consts::PI;
    match shape {
        Shape::Poly(poly) => return poly.clone(),
        Shape::Circle { c, r } => arc_points(&mut points, c.f(), f64::from(*r), 0.0, 2.0 * pi),
        Shape::Rect { c, half, rot, corner } => {
            let contour = fab_elec_core::Contour::rect(half.neg(), *half, *corner);
            points = path_points(&contour.path).into_iter().map(|p| p.rotated(*rot).add(*c)).collect();
        }
        Shape::Oval { c, half, rot } => {
            let r = half.x.min(half.y);
            let v = if half.x > half.y { P::new(half.x - r, 0) } else { P::new(0, half.y - r) }.rotated(*rot);
            return shape_poly(&Shape::Segment { a: c.sub(v), b: c.add(v), w: r.saturating_mul(2) });
        }
        Shape::Segment { a, b, w } => {
            let t = (f64::from(b.y) - f64::from(a.y)).atan2(f64::from(b.x) - f64::from(a.x));
            arc_points(&mut points, b.f(), f64::from(*w) / 2.0, t - pi / 2.0, pi);
            arc_points(&mut points, a.f(), f64::from(*w) / 2.0, t + pi / 2.0, pi);
        }
        Shape::Arc { a, mid, b, w } => {
            let Some(g) = ArcGeom::new(*a, *mid, *b) else {
                return shape_poly(&Shape::Segment { a: *a, b: *b, w: *w });
            };
            let r = f64::from(*w) / 2.0;
            let (start, sweep, a, b) = if g.sweep > 0.0 { (g.start, g.sweep, *a, *b) }
                else { (g.start + g.sweep, -g.sweep, *b, *a) };
            arc_points(&mut points, g.center, g.radius + r, start, sweep);
            arc_points(&mut points, b.f(), r, start + sweep, pi);
            if g.radius > r {
                arc_points(&mut points, g.center, g.radius - r, start + sweep, -sweep);
            } else {
                points.push(P::from_f(g.center.0, g.center.1));
            }
            arc_points(&mut points, a.f(), r, start + pi, pi);
        }
    }
    PolySet::from_ring_nonzero(points)
}

fn shape_size(shape: &PadShape) -> P {
    match shape {
        PadShape::Circle { d } => P::new(*d, *d),
        PadShape::Rect { size } | PadShape::RoundRect { size, .. } | PadShape::Chamfer { size, .. }
        | PadShape::Oval { size } | PadShape::Trapezoid { size, .. } => *size,
        PadShape::Custom { anchor, .. } => shape_size(anchor),
    }
}

fn hole_size(p: P) -> Size {
    if p.x == p.y { Size::Round(mm(p.x)) } else { Size::Wh(mm(p.x), mm(p.y)) }
}

fn thru_land(shape: &PadShape) -> (Size, bool) {
    let size = shape_size(shape);
    match shape {
        PadShape::Circle { d } => (Size::Round(mm(*d)), false),
        PadShape::Custom { anchor, .. } => thru_land(anchor),
        PadShape::Oval { .. } => (Size::Wh(mm(size.x), mm(size.y)), false),
        PadShape::RoundRect { ratio_ppm, .. } if *ratio_ppm >= 250_000 => (Size::Wh(mm(size.x), mm(size.y)), false),
        _ => (Size::Wh(mm(size.x), mm(size.y)), true),
    }
}

fn simple_outline(ring: &[P]) -> Option<Vec<[f64; 2]>> {
    use fab_elec_core::geom::orient;
    // Validate the writer's 0.1 um grid, where narrow edges can collapse or touch.
    let points: Vec<_> = ring.iter().map(|p| P::from_f(
        (f64::from(p.x) / 100.0).round() * 100.0,
        (f64::from(p.y) / 100.0).round() * 100.0,
    )).collect();
    let n = points.len();
    if n < 3 { return None; }
    for i in 0..n {
        let (a, b, next) = (points[i], points[(i + 1) % n], points[(i + 2) % n]);
        if a == b || (orient(a, b, next) == 0 && !BBox::new(a, next).contains(b)) { return None; }
        for j in i + 1..n {
            if j == i + 1 || (i == 0 && j == n - 1) { continue; }
            let (c, d) = (points[j], points[(j + 1) % n]);
            if BBox::new(a, b).intersects(&BBox::new(c, d))
                && orient(a, b, c).signum() * orient(a, b, d).signum() <= 0
                && orient(c, d, a).signum() * orient(c, d, b).signum() <= 0 {
                return None;
            }
        }
    }
    Some(points.into_iter().map(point).collect())
}

fn smd_shape(shape: &PadShape, losses: &mut Losses) -> (PadShapeOut, Option<f64>, Vec<[f64; 2]>) {
    let (kind, ratio) = match shape {
        PadShape::Circle { .. } => (PadShapeOut::Circle, None),
        PadShape::Rect { .. } => (PadShapeOut::Rect, None),
        PadShape::Oval { .. } => (PadShapeOut::Oval, None),
        PadShape::RoundRect { ratio_ppm, .. } => (PadShapeOut::RoundRect, Some(f64::from(*ratio_ppm) * 1e-6)),
        PadShape::Chamfer { ratio_ppm, .. } => {
            loss(losses, "pad.chamfer");
            (PadShapeOut::RoundRect, Some(f64::from(*ratio_ppm) * 1e-6))
        }
        PadShape::Trapezoid { .. } => { loss(losses, "pad.trapezoid"); (PadShapeOut::Rect, None) }
        PadShape::Custom { anchor, shapes } => {
            let (kind, ratio, _) = smd_shape(anchor, losses);
            let mut poly = shape_poly(&anchor.shape(P::ZERO, Angle::ZERO));
            for shape in shapes { poly = poly.union(&shape_poly(shape)); }
            if matches!(kind, PadShapeOut::Rect | PadShapeOut::Circle) {
                if let [one] = poly.polygons() {
                    if one.holes.is_empty() {
                        if let Some(outline) = simple_outline(&one.outer) { return (kind, ratio, outline); }
                    }
                }
            }
            loss(losses, "pad.custom_dropped");
            return (kind, ratio, Vec::new());
        }
    };
    (kind, ratio, Vec::new())
}

fn pad_out(pad: &Pad, losses: &mut Losses) -> PadOut {
    let thru = matches!(pad.kind, PadKind::Thru | PadKind::Npth);
    let default_tech = if thru { LayerSet::of(&[LayerId::F_MASK, LayerId::B_MASK]) }
        else { LayerSet::of(&[LayerId::F_MASK, LayerId::F_PASTE]) };
    for (key, missing) in [
        ("pad.mask_margin", pad.mask_margin.is_some()),
        ("pad.paste", pad.paste != PasteRule::default()),
        ("pad.thermal", pad.thermal.is_some()),
        ("pad.zone_connect", pad.zone_connect.is_some()),
        ("pad.die_len", pad.die_len != 0),
        ("pad.stack_shapes", [&pad.stack.inner, &pad.stack.back].iter().any(|s| s.as_ref().is_some_and(|s| s != &pad.stack.front))),
        ("pad.copper_layers", !thru && pad.stack.copper != LayerSet::one(LayerId::F_CU)),
        ("pad.tech_layers", pad.stack.tech != default_tech),
        ("pad.drill_offset", pad.drill.as_ref().is_some_and(|d| d.offset != P::ZERO)),
    ] { if missing { loss(losses, key); } }
    if pad.kind == PadKind::Edge { loss(losses, "pad.kind.Edge"); }
    let mut out = PadOut {
        number: pad.no.clone(), at: point(pad.at), rot: pad.rot.degrees(),
        kind: PadKindOut::Smd { size: point(shape_size(&pad.stack.front)) },
        shape: None, ratio: None, outline: Vec::new(),
    };
    if thru {
        let drill = hole_size(pad.drill.as_ref().map_or(P::ZERO, |d| d.size));
        let (d, square) = if pad.kind == PadKind::Npth { (drill, false) } else {
            if !matches!(pad.stack.front, PadShape::Circle { .. } | PadShape::Oval { .. } | PadShape::Rect { .. }) {
                loss(losses, "pad.thru_shape");
            }
            thru_land(&pad.stack.front)
        };
        out.kind = PadKindOut::Thru { d, drill, square, plated: pad.kind != PadKind::Npth };
    } else {
        let (shape, ratio, outline) = smd_shape(&pad.stack.front, losses);
        out.shape = Some(shape);
        out.ratio = ratio;
        out.outline = outline;
    }
    out
}

struct Group {
    part: PartOut,
    pads: Vec<Vec<usize>>,
}

/// The 3D model KiCad draws for a placement: its first model not hidden.
fn shown_model(data: Option<&crate::PlacementData>) -> Option<&crate::Model3d> {
    data?.models.iter().find(|m| !m.hidden)
}

/// What sets a placement's model apart, for grouping placements into parts: its path, offset,
/// scale and rotation as written.
fn model_key(model: Option<&crate::Model3d>) -> String {
    model.map_or(String::new(), |m| format!("{}|{:?}|{:?}|{:?}", m.path, m.offset, m.scale, m.rotate))
}

/// The KiCad major a board's own library is, from its generator version (`"9.0"`) or file
/// format date: the library a model path without a version names.
pub fn board_major(read: &PcbRead) -> Option<u32> {
    if let Some(major) = read.report.generator_version.as_deref().and_then(|v| v.split('.').next()?.trim().parse().ok()) {
        return Some(major);
    }
    let version = read.report.version;
    [(20241229, 9), (20240108, 8), (20221018, 7), (20211014, 6), (20171130, 5)].iter().find(|(from, _)| version >= *from).map(|(_, m)| *m)
}

/// The part's body from its footprint's KiCad model: the model's project path (its `.glb`,
/// with KiCad's scale in the file), and KiCad's offset and turn in Fab's terms. None, with the
/// reason counted, when the model names no file of KiCad's library.
fn model_body(model: &crate::Model3d, major: Option<u32>, losses: &mut Losses) -> Option<BodyOut> {
    use fab_elec_import::kicad3d::{body_pose, parse_model_path};
    let Ok((lib, _kind)) = parse_model_path(&model.path, major) else {
        loss(losses, "fp.model_not_library");
        return None;
    };
    let (offset, rotation) = body_pose(model.offset, model.rotate);
    // The mesh is named from the part file in `parts/kicad/`; its project path is
    // `parts/kicad/3d/<major>/<Lib>.3dshapes/<Name>.glb`.
    let mesh = lib.project_path(model.scale).strip_prefix("parts/kicad/").map(str::to_string)?;
    // KiCad states no body height: 0 until the model is read (the body draws the mesh).
    loss(losses, "fp.body_height");
    Some(BodyOut { mesh, height: 0.0, offset, rotation })
}

fn part_out(fp: &Footprint, prefix: &str, number: String, model: Option<&crate::Model3d>, major: Option<u32>, losses: &mut Losses) -> Group {
    let mut pins = Vec::new();
    let mut pads: Vec<Vec<usize>> = Vec::new();
    let mut pin_ids = HashMap::new();
    for (i, pad) in fp.pads.iter().enumerate().filter(|(_, p)| !p.no.is_empty()) {
        let pin = *pin_ids.entry(pad.no.as_str()).or_insert_with(|| {
            let index = pins.len();
            pins.push(PinOut { name: pad.no.clone(), kind: "passive", side: Side::Left });
            pads.push(Vec::new());
            index
        });
        pads[pin].push(i);
    }
    let pad_map = pins.iter().zip(&pads).map(|(pin, pads)| (pin.name.clone(), vec![pin.name.clone(); pads.len()])).collect();
    let bbox = fp.courtyard.bbox();
    if fp.courtyard != PolySet::rect(bbox) || fp.courtyard.is_empty() {
        loss(losses, "fp.courtyard_not_rect");
    }
    let courtyard = if fp.courtyard.is_empty() {
        let bbox = fp.pads.iter().fold(BBox::EMPTY, |b, p| b.union(p.stack.front.shape(p.at, p.rot).bbox()));
        if bbox.is_empty() { [[0.0; 2]; 2] } else { [point(bbox.min), point(bbox.max)] }
    } else {
        [[mm(bbox.min.x) + 0.25, mm(bbox.min.y) + 0.25], [mm(bbox.max.x) - 0.25, mm(bbox.max.y) - 0.25]]
    };
    let mut graphics = Vec::new();
    for (layer, gfx) in &fp.graphics {
        match gfx {
            Gfx::Stroke { path, w } => {
                let mut points = path_points(path);
                // A point stroke needs two equal endpoints for f.stroke.
                if points.len() == 1 { points.push(path.start); }
                graphics.push(StrokeOut { layer: layer.name(), points: points.into_iter().map(point).collect(), w: mm(*w) });
            }
            Gfx::Fill(shape) => {
                loss(losses, "fp.fill_as_outline");
                for ring in shape_poly(shape).rings() {
                    let points = ring.iter().chain(ring.first()).copied().map(point).collect();
                    graphics.push(StrokeOut { layer: layer.name(), points, w: 0.05 });
                }
            }
        }
    }
    for text in &fp.texts {
        if !matches!(text.kind, FpTextKind::Reference | FpTextKind::Value) { loss(losses, "fp.text"); }
    }
    for _ in &fp.keepouts { loss(losses, "fp.keepout"); }
    if fp.anchor != P::ZERO { loss(losses, "fp.anchor"); }
    if fp.attrs != (FpAttrs { mount: fp.attrs.mount, ..FpAttrs::default() }) { loss(losses, "fp.attrs"); }
    let body = model.and_then(|m| model_body(m, major, losses));
    let mut fields = vec![("kicad_footprint".into(), fp.name.clone())];
    if let Some(model) = model {
        fields.push(("kicad_model".into(), model.path.clone()));
    }
    Group {
        part: PartOut {
            supplier: "kicad".into(), number, kind: kind(prefix).into(), prefix: prefix.into(),
            symbol: if pins.len() == 2 { SymbolOut::TwoTerminal } else { SymbolOut::Box },
            pins, pads: fp.pads.iter().map(|p| pad_out(p, losses)).collect(), courtyard,
            thru: fp.pads.iter().any(|p| matches!(p.kind, PadKind::Thru | PadKind::Npth)),
            graphics, body, pad_map, fields,
            note: format!("converted from KiCad footprint {}", fp.name),
        },
        pads,
    }
}

/// Write parts, a circuit and the board. Nothing is evaluated: the caller evaluates the project
/// (and `refdes_changes` then counts the references the language renumbered).
pub fn to_splash(read: &PcbRead, stem: &str) -> Result<SplashProject, String> {
    if stem.is_empty() || stem == "." || stem == ".." || stem.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return Err("the project stem must be a non-empty file name without separators or control characters".into());
    }
    let start = Instant::now();
    let mut losses = Losses::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut group_ids = HashMap::new();
    let mut placements = Vec::new();
    let mut numbers = HashSet::new();
    let mut part_docs = Vec::new();
    // A placement's footprint copy carries its own reference and value texts, so equal land
    // patterns compare equal only without their texts; the part writer does not write texts.
    let mut shapes: HashMap<(ContentHash, String, String), Vec<(Footprint, usize)>> = HashMap::new();
    let major = board_major(read);
    for (index, placed) in read.board.placed.iter().enumerate() {
        let prefix = prefix(&placed.refdes);
        let model = shown_model(read.placements.get(index));
        let key = (Arc::as_ptr(&placed.footprint), prefix.to_string(), model_key(model));
        let group = match group_ids.get(&key) {
            Some(&group) => group,
            None => {
                let mut bare = (*placed.footprint).clone();
                bare.texts.clear();
                let bucket = shapes.entry((bare.content_hash(), prefix.to_string(), model_key(model))).or_default();
                let group = match bucket.iter().find(|(same, _)| same == &bare) {
                    Some(&(_, group)) => group,
                    None => {
                        let base: String = placed.footprint.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
                        let mut number = base.clone();
                        let mut k = 2usize;
                        while !numbers.insert(number.to_ascii_lowercase()) {
                            number = format!("{base}_{k}");
                            k += 1;
                        }
                        let group = part_out(&placed.footprint, prefix, number, model, major, &mut losses);
                        part_docs.push((format!("parts/kicad/{}.part.splash", group.part.number), write_part(&group.part)));
                        let id = groups.len();
                        groups.push(group);
                        bucket.push((bare, id));
                        id
                    }
                };
                group_ids.insert(key, group);
                group
            }
        };
        placements.push(group);
    }
    losses.insert("parts", part_docs.len());
    let parts_ms = millis(start);

    let start = Instant::now();
    let mut board = read.board.clone();
    // Copper without a net has the empty name, which no circuit net can have: give it one net of
    // its own name (all netless copper then counts as one net).
    let netless: HashSet<usize> = board.tracks.iter().map(|t| t.net).chain(board.vias.iter().map(|v| v.net))
        .chain(board.zones.iter().filter_map(|z| z.net)).chain(board.holes.iter().filter_map(|h| h.net))
        .map(|net| net.ix()).filter(|&i| board.nets.get(i).is_some_and(|n| n.name.is_empty())).collect();
    if !netless.is_empty() {
        let mut name = "(no net)".to_string();
        let mut k = 2;
        while board.nets.iter().any(|n| n.name == name) {
            name = format!("(no net {k})");
            k += 1;
        }
        for &i in &netless {
            board.nets[i].name = name.clone();
        }
        loss(&mut losses, "board.netless_copper");
    }
    let mut text = "// Circuit converted from an imported KiCad board.\nuse mod.elec.*\nlet e = mod.elec\n".to_string();
    for class in board.classes.iter().filter(|c| c.id.0 != 0) {
        let r = &class.rules;
        let mut fields = Vec::new();
        if let Some(v) = r.track { fields.push(format!("track: {}", mm_text(v))); }
        if let Some(v) = r.clearance { fields.push(format!("clearance: {}", mm_text(v))); }
        if let Some(v) = r.via { fields.push(format!("via: {{drill: {} pad: {}}}", mm_text(v.drill), mm_text(v.pad))); }
        if r.pair.is_some() { loss(&mut losses, "class.pair"); }
        for other in [r.length_tol.is_some(), r.z0.is_some(), r.neck.is_some(), r.max_stub.is_some(), r.via_in_pad.is_some(), r.return_via.is_some()] {
            if other { loss(&mut losses, "class.other"); }
        }
        let _ = writeln!(text, "netclass({}, {{{}}})", quote(&class.name), fields.join(" "));
    }
    let mut used: HashSet<String> = ["let", "fn", "if", "else", "for", "in", "return", "true", "false", "nil", "use", "mod", "e", "netclass"]
        .into_iter().map(str::to_string).collect();
    for group in &groups {
        let binding = group.part.binding();
        used.insert(format!("__{binding}_e"));
        used.insert(format!("__{binding}_f"));
        used.insert(binding);
    }
    let mut net_bindings = HashMap::new();
    for (i, net) in board.nets.iter().enumerate().filter(|(_, n)| !n.name.is_empty()) {
        let class = if net.class.0 == 0 { String::new() } else {
            let class = board.classes.iter().find(|c| c.id == net.class).ok_or_else(|| format!("net {} has no class {}", net.name, net.class.0))?;
            format!(" class: {}", quote(&class.name))
        };
        let binding = format!("n{i}");
        used.insert(binding.clone());
        let _ = writeln!(text, "let {binding} = e.net({{name: {}{class}}})", quote(&net.name));
        net_bindings.insert(i, binding);
    }
    let mut order: Vec<_> = (0..board.placed.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&board.placed[a].refdes, &board.placed[b].refdes);
        let (an, bn) = (ref_number(a), ref_number(b));
        prefix(a).cmp(prefix(b)).then(an.len().cmp(&bn.len())).then(an.cmp(bn)).then(a.cmp(b))
    });
    let mut paths = Vec::with_capacity(order.len());
    for (i, &index) in order.iter().enumerate() {
        let placed = &board.placed[index];
        let group = &groups[placements[index]];
        let mut path = placed.refdes.clone();
        if !identifier(&path) || !used.insert(path.clone()) {
            let mut n = i;
            loop {
                path = format!("c_{n}");
                if used.insert(path.clone()) { break; }
                n += 1;
            }
        }
        let mut pins = Vec::new();
        for pads in &group.pads {
            let mut nets = pads.iter().filter_map(|&p| placed.pad_nets.get(p).copied().flatten());
            let first = nets.next();
            if nets.any(|net| Some(net) != first) { loss(&mut losses, "pin.split_nets"); }
            pins.push(first.and_then(|net| net_bindings.get(&net.ix())).map_or("nil", String::as_str));
        }
        // A placement's value is its own when set (the reader sets it and leaves the footprint's
        // Value text blank), else its footprint's Value text, as the board draws it.
        let value = placed.value.as_deref()
            .or_else(|| placed.footprint.texts.iter().find(|t| t.kind == FpTextKind::Value).map(|t| t.text.as_str()));
        let value = value_opts(value, value_unit(&PartKind::of_name(&group.part.kind)), &mut losses);
        let _ = writeln!(text, "let {path} = e.part({}, {{pins: [{}]{value}}})", quote(&group.part.name()), pins.join(", "));
        paths.push(path);
    }
    let circuit_name = format!("{stem}.ckt.splash");
    let circuit_ms = millis(start);

    let start = Instant::now();
    // No evaluation: the board writer reads its circuit only for a placement's body band (an
    // imported placement has none, and one that had would be written explicitly) and for block
    // paths (an import has no blocks). Each placement keeps the path its `e.part` binds and is
    // the component of its position in the circuit document.
    for (i, (&index, path)) in order.iter().zip(paths).enumerate() {
        let placed = &mut board.placed[index];
        placed.path = path;
        placed.comp = CompId(i as u32);
    }
    let circuit = Circuit { unit: board.unit, name: stem.to_string(), ..Circuit::default() };
    // The writer indexes these nets directly; refuse an invalid model instead of panicking.
    for net in board.tracks.iter().map(|t| t.net).chain(board.vias.iter().map(|v| v.net))
        .chain(board.zones.iter().filter_map(|z| z.net)).chain(board.holes.iter().filter_map(|h| h.net)) {
        if board.nets.get(net.ix()).is_none() { return Err(format!("board item refers to missing net {}", net.0)); }
    }
    // These base rules have no fields in the existing board document writer.
    let defaults = fab_elec_board::RuleSet::standard();
    for (key, differs) in [
        ("board.mask_margin", board.rules.base.mask_margin != defaults.mask_margin),
        ("board.paste_margin", board.rules.base.paste_margin != defaults.paste_margin),
        ("board.paste_ratio", board.rules.base.paste_ratio_ppm != defaults.paste_ratio_ppm),
    ] { if differs { loss(&mut losses, key); } }
    let board_text = fab_elec_lang::write(&board, &circuit);
    let board_ms = millis(start);
    let mut docs = vec![(circuit_name, text), (format!("{stem}.pcb.splash"), board_text)];
    docs.extend(part_docs);
    Ok(SplashProject { docs, losses, parts_ms, circuit_ms, board_ms })
}

/// How many components of a converted project's evaluated circuit the language numbered other
/// than their KiCad reference. The converter binds each part to its reference when that is an
/// identifier (`c_<n>` otherwise), so a reference is kept exactly when the component's reference
/// equals its path.
pub fn refdes_changes(circuit: &Circuit) -> usize {
    circuit.comps.iter().filter(|c| c.refdes != c.path).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_lang::{evaluate, std_libs, Sources};

    #[test]
    fn board_round_trip_preserves_placements_and_connections() {
        // A reduced legacy-board example, with both sides and a repeated pad number.
        let read = crate::read_kicad_pcb(br#"(kicad_pcb (version 20171130) (host pcbnew "5.0.2")
 (general (thickness 1.6))
 (layers (0 F.Cu signal) (31 B.Cu signal) (32 B.Mask user) (33 F.Mask user)
  (34 F.Paste user) (37 F.SilkS user) (38 B.SilkS user) (44 Edge.Cuts user))
 (net 0 "") (net 3 GND) (net 9 CLK)
 (gr_line (start 0 0) (end 12 0) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 12 0) (end 12 12) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 12 12) (end 0 12) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 0 12) (end 0 0) (layer Edge.Cuts) (width 0.05))
 (module lib:Part (layer B.Cu) (at 5 5 90)
  (fp_text reference R9 (at 0 0 90) (layer B.SilkS) (effects (font (size 1 1)) (justify mirror)))
  (fp_text value "10K" (at 0 1 90) (layer B.SilkS) (effects (font (size 1 1)) (justify mirror)))
  (pad 1 thru_hole circle (at 1 0) (size 1 1) (drill 0.4) (layers *.Cu *.Mask) (net 3 GND))
  (pad 2 thru_hole oval (at -1 0) (size 1 1.5) (drill 0.4) (layers *.Cu *.Mask) (net 9 CLK)))
 (module lib:part (layer F.Cu) (at 1 1)
  (fp_text reference R2 (at 0 0) (layer F.SilkS) (effects (font (size 1 1))))
  (pad 1 smd rect (at 0 0) (size 1 1) (layers F.Cu F.Paste F.Mask) (net 3 GND))
  (pad 1 smd circle (at 0 2) (size 1 1) (layers F.Cu F.Paste F.Mask) (net 3 GND))
  (pad 2 smd oval (at 0 4) (size 1 1.5) (layers F.Cu F.Paste F.Mask) (net 9 CLK)))
 (segment (start 1 1) (end 4 1) (width 0.3) (layer F.Cu) (net 3))
 (via (at 4 1) (size 0.8) (drill 0.4) (layers F.Cu B.Cu) (net 3))
 (zone (net 3) (layer F.Cu) (name pour) (connect_pads yes) (min_thickness 0.2)
  (polygon (pts (xy 7 7) (xy 8 7) (xy 8 8) (xy 7 8)))))"#, "roundtrip")
            .expect("small legacy board reads");
        let project = to_splash(&read, "roundtrip").expect("board converts with existing writers");
        assert_eq!(project.losses.get("parts"), Some(&2), "one document per footprint group");
        assert_eq!(project.losses.get("comp.value_missing"), Some(&1), "only the absent value is lost");
        let mut sources = Sources::new();
        for (name, text) in project.docs { sources.add(name, text).expect("unique project document"); }
        let evaluated = evaluate(&sources, &std_libs()).expect("converted project evaluates");
        let unit = evaluated.design.units.first().expect("one evaluated unit");
        assert_eq!(refdes_changes(&unit.circuit), 2, "the language renumbers both references");
        let board = unit.board.as_deref().expect("unit has a board");
        assert_eq!(board.placed.len(), 2, "every component is placed");
        for imported in &read.board.placed {
            let placed = board.placed.iter().find(|p| p.path == imported.refdes).expect("placement keeps its binding");
            assert_eq!((placed.at, placed.side, placed.rot), (imported.at, imported.side, imported.rot), "placement frame survives");
            assert_eq!(placed.footprint.pads.len(), imported.footprint.pads.len(), "all pads survive");
            let names = |b: &fab_elec_board::Board, p: &fab_elec_board::Placed| {
                p.pad_nets.iter().map(|n| n.and_then(|n| b.nets.get(n.ix())).map(|n| n.name.clone())).collect::<Vec<_>>()
            };
            assert_eq!(names(board, placed), names(&read.board, imported), "pad nets survive renumbering and repeated pad numbers");
        }
        assert_eq!(board.tracks.len(), read.board.tracks.len(), "track count survives");
        assert_eq!(board.vias.len(), read.board.vias.len(), "via count survives");
        assert_eq!(board.zones.len(), read.board.zones.len(), "zone count survives");
        let r9 = unit.circuit.comps.iter().find(|c| c.path == "R9").expect("R9 is a component");
        let value = r9.value.as_ref();
        assert_eq!(value.map(|v| v.text.as_str()), Some("10k"), "the circuit shows the value as it writes it");
        assert_eq!(value.and_then(|v| v.qty).map(|q| q.si), Some(10_000.0), "a resistor's value is its ohms");
        assert_eq!(r9.fields.get("source_value"), Some("10K"), "the board's own text is kept beside the value it reads as");
        let mut losses = Losses::new();
        for (text, unit, opts) in [
            (Some("10k"), Some(QtyUnit::Ohm), " value: 10kohm"),
            (Some("10UF"), Some(QtyUnit::Farad), " value: 10uF fields: {source_value: \"10UF\"}"),
            (Some("DNP"), Some(QtyUnit::Ohm), " fields: {source_value: \"DNP\"}"),
            (Some("ATmega328P"), None, " value: \"ATmega328P\""),
            (Some(" "), Some(QtyUnit::Ohm), ""),
            (None, None, ""),
        ] {
            assert_eq!(value_opts(text, unit, &mut losses), opts, "{text:?} in {unit:?}");
        }
        assert_eq!((losses.get("comp.value_unread"), losses.get("comp.value_missing")), (Some(&1), Some(&2)), "an unread text and the absent ones are counted, a kept rewording is not");
        let si_of = |text, unit| value_quantity(text, unit).map(|q| q.si);
        for (text, unit, si) in [
            ("4k7", QtyUnit::Ohm, Some(4_700.0)), ("0R", QtyUnit::Ohm, Some(0.0)), ("R47", QtyUnit::Ohm, Some(0.47)),
            ("100", QtyUnit::Ohm, Some(100.0)), ("1M", QtyUnit::Ohm, Some(1e6)), ("10k 1%", QtyUnit::Ohm, Some(10_000.0)),
            ("R_0603_10k", QtyUnit::Ohm, Some(10_000.0)), ("2.2 kohm", QtyUnit::Ohm, None),
            ("100nF", QtyUnit::Farad, Some(100e-9)), ("2u2", QtyUnit::Farad, Some(2.2e-6)),
            ("CC1206_10UF_25V_10%_X7R", QtyUnit::Farad, Some(10e-6)), ("100", QtyUnit::Farad, None),
            ("10mH", QtyUnit::Henry, Some(10e-3)), ("1H", QtyUnit::Henry, Some(1.0)),
            ("16.000MHz", QtyUnit::Hertz, Some(16e6)), ("ABM8-32.768kHz", QtyUnit::Hertz, Some(32_768.0)),
            ("DNP", QtyUnit::Ohm, None), ("10V", QtyUnit::Farad, None),
        ] {
            assert_eq!(si_of(text, unit), si, "{text:?} reads as {si:?} in {unit:?}");
        }
    }

    /// A footprint's KiCad model becomes its part's body: the `.glb` of its library path (KiCad's
    /// scale in the file), KiCad's offset, and its turn negated in KiCad's order, so a point of
    /// the model lands where KiCad draws it. Hidden and extra models, and models no library holds,
    /// are reported.
    #[test]
    fn footprint_models_become_bodies_where_kicad_draws_them() {
        use fab_elec_parts::BodySource;
        let read = crate::read_kicad_pcb(br#"(kicad_pcb (version 20171130) (host pcbnew "5.0.2")
 (general (thickness 1.6))
 (layers (0 F.Cu signal) (31 B.Cu signal) (32 B.Mask user) (33 F.Mask user)
  (34 F.Paste user) (37 F.SilkS user) (38 B.SilkS user) (44 Edge.Cuts user))
 (net 0 "") (net 3 GND)
 (gr_line (start 0 0) (end 30 0) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 30 0) (end 30 20) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 30 20) (end 0 20) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 0 20) (end 0 0) (layer Edge.Cuts) (width 0.05))
 (module lib:SOIC (layer F.Cu) (at 5 5 90)
  (fp_text reference U1 (at 0 0 90) (layer F.SilkS) (effects (font (size 1 1))))
  (pad 1 smd rect (at -2 0) (size 1 1) (layers F.Cu F.Paste F.Mask) (net 3 GND))
  (pad 2 smd rect (at 2 0) (size 1 1) (layers F.Cu F.Paste F.Mask))
  (model "${KICAD8_3DMODEL_DIR}/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.step"
   (offset (xyz 0.5 -0.25 0.1)) (scale (xyz 1 1 1)) (rotate (xyz -90 0 30)))
  (model "${KICAD8_3DMODEL_DIR}/Package_SO.3dshapes/Hidden.wrl" hide (offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0)))
  (model "${KICAD8_3DMODEL_DIR}/Package_SO.3dshapes/Extra.wrl" (offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0))))
 (module lib:R (layer B.Cu) (at 20 10)
  (fp_text reference R1 (at 0 0) (layer B.SilkS) (effects (font (size 1 1)) (justify mirror)))
  (pad 1 smd rect (at -1 0) (size 1 1) (layers B.Cu B.Paste B.Mask) (net 3 GND))
  (pad 2 smd rect (at 1 0) (size 1 1) (layers B.Cu B.Paste B.Mask))
  (model ${KISYS3DMOD}/Resistor_SMD.3dshapes/R_0805_2012Metric.wrl
   (at (xyz 0.1 0 0)) (scale (xyz 2 1 1)) (rotate (xyz 0 0 0))))
 (module lib:J (layer F.Cu) (at 25 15)
  (fp_text reference J1 (at 0 0) (layer F.SilkS) (effects (font (size 1 1))))
  (pad 1 thru_hole circle (at 0 0) (size 1.5 1.5) (drill 0.8) (layers *.Cu *.Mask))
  (model ${KIPRJMOD}/own.wrl (at (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0)))))"#, "models")
            .expect("a board with models reads");
        let gaps = |key: &str| read.report.gaps.get(key).map_or(0, |g| g.count);
        assert_eq!((gaps("model.hidden"), gaps("model.extra"), gaps("model")), (1, 1, 0), "hidden and extra models are reported; models are read");
        let u1 = &read.placements[0].models;
        assert_eq!((u1.len(), u1[0].offset, u1[0].rotate, u1[1].hidden), (3, [0.5, -0.25, 0.1], [-90.0, 0.0, 30.0], true), "U1's models as written");
        assert_eq!(read.placements[1].models[0].offset.map(|v| (v * 1e6).round() / 1e6), [2.54, 0.0, 0.0], "a model's legacy `at` is inches");
        assert_eq!(board_major(&read), Some(5), "a 2017 file is KiCad 5's");

        let project = to_splash(&read, "models").expect("the board converts");
        let doc = |name: &str| project.docs.iter().find(|(n, _)| n.ends_with(name)).map(|(_, t)| t.as_str()).unwrap_or_else(|| panic!("{name} is written"));
        let soic = doc("lib_SOIC.part.splash");
        assert!(soic.contains(r#"body: {mesh: "3d/8/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.glb" offset: [0.5mm, -0.25mm, 0.1mm] rotation: [90deg, 0deg, -30deg] height: 0mm}"#),
            "the STEP's .wrl beside it, KiCad's offset and its negated turn:\n{soic}");
        assert!(soic.contains(r#"kicad_model: "${KICAD8_3DMODEL_DIR}/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.step""#), "the model as KiCad names it is kept:\n{soic}");
        assert!(doc("lib_R.part.splash").contains(r#"mesh: "3d/5/Resistor_SMD.3dshapes/R_0805_2012Metric.scale-2_1_1.glb" offset: [2.54mm, 0mm, 0mm]"#),
            "KiCad 5's library, its scale in the file name:\n{}", doc("lib_R.part.splash"));
        assert!(!doc("lib_J.part.splash").contains("body: {mesh:"), "a model of the board's own folder gives no body");
        assert_eq!(project.losses.get("fp.model_not_library"), Some(&1), "and is counted");

        let mut sources = Sources::new();
        for (name, text) in project.docs { sources.add(name, text).expect("unique project document"); }
        let soic_path = "parts/kicad/3d/8/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.glb";
        let r_path = "parts/kicad/3d/5/Resistor_SMD.3dshapes/R_0805_2012Metric.scale-2_1_1.glb";
        let missing = evaluate(&sources, &std_libs()).expect("the project evaluates").design.missing_meshes();
        assert_eq!(missing, [r_path, soic_path], "before the models are there, the design lists the KiCad models it lacks");
        sources.set_library(vec![soic_path.into(), r_path.into()]);
        let mut design = evaluate(&sources, &std_libs()).expect("the project evaluates with its models").design;
        let keys: BTreeMap<String, String> = [soic_path, r_path].iter().map(|p| (p.to_string(), format!("cache/{p}"))).collect();
        design.resolve_meshes(&keys);
        assert!(design.missing_meshes().is_empty(), "no model is missing");
        let unit = &design.units[0];
        let u1 = unit.circuit.comps.iter().find(|c| c.part.name.contains("SOIC")).expect("U1");
        let body = u1.part.body.as_ref().expect("U1 has a body");
        assert_eq!(body.source, BodySource::Mesh(format!("cache/{soic_path}")), "the body draws the cached model");
        // KiCad: q = offset + Rz(-rz) Ry(-ry) Rx(-rx) p, with right-handed turns; Fab: body.offset.
        let rot = |axis: usize, deg: f64, p: [f64; 3]| {
            let (s, c) = deg.to_radians().sin_cos();
            let (i, j) = [(1, 2), (2, 0), (0, 1)][axis];
            let mut q = p;
            q[i] = c * p[i] - s * p[j];
            q[j] = s * p[i] + c * p[j];
            q
        };
        for corner in 0..8 {
            let p = [if corner & 1 == 0 { -1.95 } else { 1.95 }, if corner & 2 == 0 { -2.45 } else { 2.45 }, if corner & 4 == 0 { 0.0 } else { 1.75 }];
            let kicad = rot(2, 30.0 * -1.0, rot(1, 0.0, rot(0, 90.0, p)));
            let kicad = [kicad[0] + 0.5, kicad[1] - 0.25, kicad[2] + 0.1];
            let fab = body.offset.apply_f(p.map(|v| v * 1e6)).map(|v| v * 1e-6);
            for k in 0..3 {
                assert!((fab[k] - kicad[k]).abs() < 1e-6, "corner {corner}: Fab puts it at {fab:?}, KiCad at {kicad:?}");
            }
        }
    }

}
