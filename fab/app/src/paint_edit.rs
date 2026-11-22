//! Colour and brush edits, assembled on the model worker against its latest document: the
//! print colour a painted material prints as is the project's (the object's placement); a
//! brush stroke is one record in the object's strokes file
//! (`fab_model::program::STROKES_EXTENSION`), read by its script with `m.strokes`.

use fab::api::{PaintPattern, PaintTarget, PartAction, PatternKind};
use fab_ai::out::{mm3, text};
use fab_model::{eval::Evaluated, program::Compiled, Change, Edit, PlaceArg};
use makepad_script::syntax;

/// sRGB 0xRRGGBBff of a linear colour.
fn srgb(rgb: [f32; 3]) -> u32 {
    let byte = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (e * 255.0).round() as u32
    };
    (byte(rgb[0]) << 24) | (byte(rgb[1]) << 16) | (byte(rgb[2]) << 8) | 0xff
}

/// The paint pairs with `rgb` printing as print colour `name`: its pair replaced, or added;
/// the others kept.
pub fn merge_paint(old: &[(u32, String)], rgb: u32, name: &str) -> Vec<(u32, String)> {
    let mut out = old.to_vec();
    match out.iter_mut().find(|(c, _)| *c | 0xff == rgb | 0xff) {
        Some(pair) => pair.1 = name.to_string(),
        None => out.push((rgb, name.to_string())),
    }
    out
}

/// A paint material of `rgb`, printing as print colour `name` when given.
fn material(rgb: &[f32; 3], name: Option<&str>) -> String {
    let [r, g, b] = rgb;
    match name {
        Some(n) => format!("Material{{albedo: [{r} {g} {b}] name: {}}}", makepad_script::syntax::write::quote(n)),
        None => format!("Material{{albedo: [{r} {g} {b}]}}"),
    }
}

/// The edits pairing `object`'s painted colour values `rgbs` with print colour `name`, adding
/// the colour to the project when it has none of that name.
fn paint_as(c: &Compiled, ev: &Evaluated, object: &str, rgbs: &[[f32; 3]], name: &str, color: Option<Change>) -> Result<Vec<Change>, String> {
    let (inst, solid) = c.part(object).ok_or("The part is no longer on the plate")?;
    let placed = inst.placed_of(solid).ok_or("The part is no longer on the plate")?;
    let old: Vec<(u32, String)> = placed.paint.iter().map(|(c, n)| (srgb(*c), n.clone())).collect();
    let mut edits: Vec<Change> = color.into_iter().collect();
    if edits.is_empty() && !ev.colors.iter().any(|c| c.name == name) {
        edits.push(Change::Color { name: name.into(), set: Some(fab_model::project::ColorDef::new(name)) });
    }
    let paint = rgbs.iter().fold(old, |paint, rgb| merge_paint(&paint, srgb(*rgb), name));
    edits.push(Change::place(c, object, PlaceArg::Paint(paint)).ok_or("The part's object is gone")?);
    Ok(edits)
}

/// Inverse of the evaluated model-to-plate affine (model and plate both mm).
pub fn inverse(m: &[[f64; 4]; 3]) -> Option<[[f64; 4]; 3]> {
    let a = |r: usize, c: usize| m[r][c];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)) + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    if !det.is_finite() || det.abs() < 1e-12 { return None; }
    let mut r = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let (i1, i2, j1, j2) = ((j + 1) % 3, (j + 2) % 3, (i + 1) % 3, (i + 2) % 3);
            r[i][j] = (a(i1, j1) * a(i2, j2) - a(i1, j2) * a(i2, j1)) / det;
        }
        r[i][3] = -(0..3).map(|j| r[i][j] * m[j][3]).sum::<f64>();
    }
    Some(r)
}

/// What the person reads when a colour or brush edit is refused, from the worker's answer.
pub fn refusal(reply: &str) -> String {
    let why = syntax::parse_value(reply).ok().and_then(|v| v.field("error").and_then(|f| f.value.string()).map(str::to_string));
    format!("Not painted: {}", why.as_deref().unwrap_or(reply))
}

/// The strokes file beside an object script for what the brush paints (`vase.cad.splash` ->
/// `vase.strokes`, and for the support channels `vase.block.strokes`, `vase.enforce.strokes`).
pub fn strokes_file_of(script: &str, target: PaintTarget) -> String {
    let stem = script.rsplit('/').next().unwrap_or(script);
    let stem = stem.strip_suffix(&format!(".{}", fab_model::program::SCRIPT_EXTENSION)).unwrap_or(stem);
    let kind = match target {
        PaintTarget::Color => "",
        PaintTarget::SupportBlock => ".block",
        PaintTarget::SupportEnforce => ".enforce",
    };
    format!("{stem}{kind}.{}", fab_model::program::STROKES_EXTENSION)
}

/// The `Mix{..}` a Pattern fill writes, as the guide's examples read: `own` where the pattern's
/// field is 0, `paint` where it is 1. `span`: the part's height range in its model frame (mm),
/// which a ramp climbs.
pub fn pattern_mix(p: &PaintPattern, own: &str, paint: &str, span: (f64, f64)) -> String {
    let n = |v: f64| format!("{}", (v * 100.0).round() / 100.0);
    let (s, seed) = (n(p.scale), p.seed);
    let fn_of = |body: String| format!("fn(p) {{ {body} }}");
    let field = match p.kind {
        // grown on the part's skin (`Turing{..}`), not a field of the point
        PatternKind::Spots => format!("Turing{{preset: @spots scale: {s}mm seed: {seed}}}"),
        PatternKind::Stripes => format!("Turing{{preset: @stripes scale: {s}mm seed: {seed}}}"),
        PatternKind::Cells => fn_of(format!("noise.cell_id(p / {s}mm, {seed})")),
        PatternKind::Noise => fn_of(format!("0.5 + 0.5 * noise.fbm(p / {s}mm, {seed}, 3)")),
        PatternKind::Marble => fn_of(format!("noise.bands(p.x / {s}mm + 2 * noise.fbm(p / {}mm, {seed}, 4))", n(p.scale * 3.0))),
        PatternKind::Bands => fn_of(format!("noise.bands(p.y / {s}mm + 0.4 * noise.fbm(p / {s}mm, {seed}, 2))")),
        PatternKind::Ramp => {
            let height = n((span.1 - span.0).max(0.01));
            fn_of(match n(span.0).as_str() {
                "0" | "-0" => format!("p.y / {height}mm"),
                foot if span.0 > 0.0 => format!("(p.y - {foot}mm) / {height}mm"),
                _ => format!("(p.y + {}mm) / {height}mm", n(-span.0)),
            })
        }
    };
    let mut out = format!("Mix{{materials: [{own} {paint}] field: {field}");
    if p.sharp > 0.0 {
        out += &format!(" sharp: {}", n(p.sharp));
    }
    if p.balance != 0.0 {
        out += &format!(" balance: {}", n(p.balance));
    }
    out + "}"
}

/// The statement a script reads `region` with for `target`, without its material.
fn support_statement(target: PaintTarget, region: &str) -> Option<String> {
    match target {
        PaintTarget::Color => None,
        PaintTarget::SupportBlock => Some(format!("m.support_block({region})\n")),
        PaintTarget::SupportEnforce => Some(format!("m.support_enforce({region})\n")),
    }
}

pub fn edits(c: &Compiled, ev: &Evaluated, action: &PartAction, frame: Option<&[[f64; 4]; 3]>) -> Result<Vec<Change>, String> {
    match action {
        PartAction::SetPaintColor { object, rgb, color } => paint_as(c, ev, object, std::slice::from_ref(rgb), color, None),
        PartAction::AssignTray { objects, color, printer, side, tray, paint } if !paint.is_empty() => {
            let object = objects.first().ok_or("No painted part")?;
            let (name, def) = crate::plate_edit::tray_color(ev, color, printer, side, tray);
            paint_as(c, ev, object, paint, &name, Some(def))
        }
        PartAction::PaintStroke { object, rgb, color, erase, radius, points, target } => {
            // A stroke is one record of the object's strokes file (`fab_model::program`), read
            // by its script with `m.strokes`; paint never enters the part's shape, so every part
            // type takes it. Its colour value prints as `color`: the placement pairs the two.
            if !ev.plate.placed_parts().any(|p| p.object == *object) {
                return Err("The painted part is no longer placed".into());
            }
            if points.is_empty() || !radius.is_finite() || *radius <= 0.0 || !rgb.iter().all(|v| v.is_finite()) || !points.iter().flatten().all(|v| v.is_finite()) {
                return Err("The paint stroke has no valid brush points".into());
            }
            let (inst, _) = c.part(object).ok_or("The painted solid no longer exists")?;
            let frame = frame.ok_or("The painted part has no model frame")?;
            let inv = inverse(frame).ok_or("The painted part has a singular model frame")?;
            let scale = (0..3).map(|i| frame[i][0].powi(2)).sum::<f64>().sqrt();
            let points: Vec<[f64; 3]> = points.iter().map(|p| std::array::from_fn(|i| inv[i][0] * p[0] + inv[i][1] * p[1] + inv[i][2] * p[2] + inv[i][3])).collect();
            let file = inst.script.file.clone();
            // A support channel's strokes are a file of their own, read by its support statement.
            let strokes_file = strokes_file_of(&file, *target);
            let mut edits = vec![Change::Stroke { file: strokes_file.clone(), op: fab_paint_field::StrokeOp::Erase, rgb: (!erase).then_some(*rgb), radius: radius / scale, strength: 1.0, points }];
            if let (false, Some(name), PaintTarget::Color) = (*erase, color, target) {
                edits.extend(paint_as(c, ev, object, std::slice::from_ref(rgb), name, None)?);
            }
            // The object script reads its strokes file once.
            if !fab_model::program::strokes_files(inst.script.source()).contains(&strokes_file) {
                let src = inst.script.source();
                let quoted = format!("\"{strokes_file}\"");
                let line = support_statement(*target, &quoted).unwrap_or_else(|| format!("m.strokes({quoted})\n"));
                edits.push(Change::Script { file, edit: Edit::Splice { range: src.len()..src.len(), text: format!("{}{line}", if src.ends_with('\n') { "" } else { "\n" }) } });
            }
            Ok(edits)
        }
        PartAction::SculptStroke { object, op, radius, strength, points } => {
            // A shape record of the same strokes file: it sculpts the part's distance field
            // (fab_model's eval wraps the field in an SdfNode::Sculpt), so only field parts take
            // it. The points and radius are the model frame's, as the view drew them live.
            let i = ev.plate.placed_parts().position(|p| p.object == *object).ok_or("The sculpted part is no longer placed")?;
            if op.is_paint() {
                return Err("Not a sculpt brush".into());
            }
            if ev.placed_field(i).is_none() {
                return Err("Only distance-field parts can be sculpted (this part is a mesh or an exact body)".into());
            }
            if points.is_empty() || !radius.is_finite() || *radius <= 0.0 || !strength.is_finite() || !points.iter().flatten().all(|v| v.is_finite()) {
                return Err("The sculpt stroke has no valid brush points".into());
            }
            let (inst, solid) = c.part(object).ok_or("The sculpted solid no longer exists")?;
            let file = inst.script.file.clone();
            let strokes_file = strokes_file_of(&file, PaintTarget::Color);
            let mut edits = vec![Change::Stroke { file: strokes_file.clone(), op: *op, rgb: None, radius: *radius, strength: *strength, points: points.clone() }];
            // A sculpt never moves its part: the plate arranges a free object by its box, which
            // a stroke can grow, so a free object is pinned where it is (its model origin's place
            // on the bed) in the same step.
            if inst.placed.iter().any(|p| p.solid == solid && p.at.is_none()) {
                let m = ev.placed_field(i).map(|f| f.to_printer).ok_or("The sculpted part has no field")?;
                edits.extend(Change::place(c, object, PlaceArg::At(Some([m[0][3], m[1][3]]))));
            }
            if !fab_model::program::strokes_files(inst.script.source()).contains(&strokes_file) {
                let src = inst.script.source();
                edits.push(Change::Script { file, edit: Edit::Splice { range: src.len()..src.len(), text: format!("{}m.strokes(\"{strokes_file}\")\n", if src.ends_with('\n') { "" } else { "\n" }) } });
            }
            Ok(edits)
        }
        PartAction::PaintFill { object, features, rgb, color, target } => {
            // One m.fill per feature (the whole part: the object's own solid), appended to its
            // script in one batch: one undo step.
            let (inst, solid) = c.part(object).ok_or("The part is no longer on the plate")?;
            let whole = &inst.script.program.solids[solid];
            if features.is_empty() || !rgb.iter().all(|v| v.is_finite()) {
                return Err("Nothing to fill".into());
            }
            let mat = material(rgb, color.as_deref());
            let mut text = String::new();
            for f in features {
                let name = match f {
                    Some(name) => name.clone(),
                    None if whole.named && !whole.id.is_empty() => whole.id.clone(),
                    None => return Err("This part's solid has no name to fill by: give it a let".into()),
                };
                text.push_str(&support_statement(*target, &name).unwrap_or_else(|| format!("m.fill({name}, {mat})\n")));
            }
            let src = inst.script.source();
            if !src.ends_with('\n') {
                text.insert(0, '\n');
            }
            Ok(vec![Change::Script { file: inst.script.file.clone(), edit: Edit::Splice { range: src.len()..src.len(), text } }])
        }
        PartAction::PaintPattern { object, own, own_color, rgb, color, pattern } => {
            // One m.fill of the whole part by the pattern's field, appended to its script: the
            // part's own colour where the field is 0, the paint colour where it is 1.
            let (inst, solid) = c.part(object).ok_or("The part is no longer on the plate")?;
            let whole = &inst.script.program.solids[solid];
            if !whole.named || whole.id.is_empty() {
                return Err("This part's solid has no name to fill by: give it a let".into());
            }
            if !rgb.iter().chain(own).all(|v| v.is_finite()) {
                return Err("Nothing to fill".into());
            }
            // a ramp climbs the part's height in its model frame
            let span = if pattern.kind == PatternKind::Ramp {
                let inv = inverse(frame.ok_or("The part has no model frame")?).ok_or("The part has a singular model frame")?;
                let b = ev.plate.parts.iter().find(|p| p.object == *object).ok_or("The part is no longer on the plate")?.bounds();
                let ys: Vec<f64> = (0..8).map(|k| {
                    let q = [if k & 1 == 0 { b.min[0] } else { b.max[0] }, if k & 2 == 0 { b.min[1] } else { b.max[1] }, if k & 4 == 0 { b.min[2] } else { b.max[2] }];
                    inv[1][0] * q[0] + inv[1][1] * q[1] + inv[1][2] * q[2] + inv[1][3]
                }).collect();
                (ys.iter().copied().fold(f64::INFINITY, f64::min), ys.iter().copied().fold(f64::NEG_INFINITY, f64::max))
            } else {
                (0.0, 0.0)
            };
            let mix = pattern_mix(pattern, &material(own, own_color.as_deref()), &material(rgb, color.as_deref()), span);
            let src = inst.script.source();
            let text = format!("{}m.fill({}, {mix})\n", if src.ends_with('\n') { "" } else { "\n" }, whole.id);
            Ok(vec![Change::Script { file: inst.script.file.clone(), edit: Edit::Splice { range: src.len()..src.len(), text } }])
        }
        PartAction::PaintFlood { object, seed, angle, rgb, color, target } => {
            // One m.fill of a smooth-region flood, the seed in the model frame, appended to the
            // object's script: it is recomputed on the part's surface at every build.
            if !seed.iter().all(|v| v.is_finite()) || !angle.is_finite() || !rgb.iter().all(|v| v.is_finite()) {
                return Err("The smooth fill has no valid seed".into());
            }
            let (inst, _) = c.part(object).ok_or("The part is no longer on the plate")?;
            let frame = frame.ok_or("The part has no model frame")?;
            let inv = inverse(frame).ok_or("The part has a singular model frame")?;
            let q: [f64; 3] = std::array::from_fn(|i| inv[i][0] * seed[0] + inv[i][1] * seed[1] + inv[i][2] * seed[2] + inv[i][3]);
            let src = inst.script.source();
            let flood = format!("Flood{{seed: {} angle: {}deg}}", text(&mm3(q.map(|v| (v * 100.0).round() / 100.0))), angle.clamp(1.0, 180.0).round());
            let line = support_statement(*target, &flood).unwrap_or_else(|| format!("m.fill({flood}, {})\n", material(rgb, color.as_deref())));
            let text = format!("{}{line}", if src.ends_with('\n') { "" } else { "\n" });
            Ok(vec![Change::Script { file: inst.script.file.clone(), edit: Edit::Splice { range: src.len()..src.len(), text } }])
        }
        PartAction::Study { object, edit } => crate::study_edit::edits(c, object, edit, frame),
        // the bytes it takes out are those of the program the worker applies it to
        PartAction::ResetAreaSetting { key } => Ok(vec![fab_model::params::reset_region_setting(c, key)?]),
        _ => Err("Not a colour or paint edit".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every engine's part takes a stroke: the bracket (CSG mesh), the vase (field), the motor
    /// mount (exact). A stroke that cannot paint says why and changes nothing.
    #[test]
    fn strokes_paint_every_part_type_and_a_bad_one_says_why() {
        for (name, model) in [("demo", crate::demo_part::model()), ("motor mount", crate::demo_part::demo("motor_mount").unwrap().model().unwrap())] {
            let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
            session.model = model;
            session.path = None;
            let ev = session.rebuild().expect("it builds");
            for i in 0..ev.plate.parts.len() {
                let ev = session.last.clone().unwrap();
                let part = ev.plate.parts[i].clone();
                if part.object.starts_with("bracket/") {
                    // A stroke that cannot paint (zero radius) says why and changes nothing.
                    let action = PartAction::PaintStroke { object: part.object.clone(), rgb: [0.8, 0.05, 0.05], color: None, erase: false, radius: 0.0, points: vec![[0.0; 3]], target: fab::api::PaintTarget::Color };
                    let why = edits(session.compiled.as_ref().unwrap(), &ev, &action, ev.model_to_plate.get(i)).err().expect("a zero radius is refused");
                    assert!(!why.is_empty());
                    let reply = fab_ai::Reply { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&why)), images: Vec::new(), update: None, render: None };
                    assert!(refusal(&reply.text).starts_with("Not painted: "));
                }
                let b = part.bounds();
                let top = [0.5 * (b.min[0] + b.max[0]), 0.5 * (b.min[1] + b.max[1]), b.max[2]];
                let action = PartAction::PaintStroke { object: part.object.clone(), rgb: [0.8, 0.05, 0.05], color: None, erase: false, radius: 2.0, points: vec![top, [top[0] + 5.0, top[1], top[2]]], target: fab::api::PaintTarget::Color };
                let batch = edits(session.compiled.as_ref().unwrap(), &ev, &action, ev.model_to_plate.get(i)).unwrap_or_else(|e| panic!("{name} {}: {e}", part.object));
                let reply = session.commit("paint", batch);
                assert!(reply.ok, "{name} {}: {}", part.object, reply.text);
                let after = session.last.clone().unwrap();
                let k = after.plate.parts.iter().position(|p| p.object == part.object).unwrap();
                let paint = after.paint[k].as_ref().unwrap_or_else(|| panic!("{name} {} is painted", part.object));
                assert_eq!(paint.slot_at(top), 1, "{name} {}", part.object);
                assert_eq!(after.fields[k].as_ref().map(|f| f.key), ev.fields[i].as_ref().map(|f| f.key), "{name} {}: the field is untouched", part.object);
            }
        }
    }

    #[test]
    fn colour_pairs_merge_keeping_the_others() {
        let p = |c: u32, n: &str| (c, n.to_string());
        assert_eq!(merge_paint(&[], 0xff0000ff, "Red"), vec![p(0xff0000ff, "Red")]);
        assert_eq!(merge_paint(&[p(0xff0000ff, "Red"), p(0x00ff00ff, "Green")], 0xff0000ff, "Blue"), vec![p(0xff0000ff, "Blue"), p(0x00ff00ff, "Green")]);
        assert_eq!(merge_paint(&[p(0xff0000ff, "Red")], 0x0000ffff, "Blue"), vec![p(0xff0000ff, "Red"), p(0x0000ffff, "Blue")]);
        assert_eq!(srgb([1.0, 0.0, 0.0]), 0xff0000ff);
    }

    #[test]
    fn brush_points_invert_placement_turn_scale_and_translation() {
        let m = [[0.0, 0.0, -2.0, 100.0], [2.0, 0.0, 0.0, 150.0], [0.0, 2.0, 0.0, 20.0]];
        let inv = inverse(&m).unwrap();
        let model = [4.0, 5.0, 6.0];
        let plate: [f64; 3] = std::array::from_fn(|i| (0..3).map(|j| m[i][j] * model[j]).sum::<f64>() + m[i][3]);
        for i in 0..3 {
            let back = (0..3).map(|j| inv[i][j] * plate[j]).sum::<f64>() + inv[i][3];
            assert!((back - model[i]).abs() < 1e-9);
        }
    }

    #[test]
    fn strokes_are_records_of_a_strokes_file_keep_placement_and_undo_once() {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
        let source = format!("{}let body = m.box({{size: [20mm 20mm 20mm]}})\nobject(body)\n", fab_model::program::EMPTY);
        let (reply, _) = session.set_source("t.cad.splash", &source);
        assert!(reply.ok, "{}", reply.text);
        let place = |arg| Change::Place { instance: "t".into(), object: "body".into(), arg };
        let reply = session.commit("place", vec![place(PlaceArg::At(Some([100.0, 120.0]))), place(PlaceArg::Rotate([0.0, 0.0, 45.0])), place(PlaceArg::Scale([2.0; 3])), place(PlaceArg::Color(Some("Red".into())))]);
        assert!(reply.ok, "{}", reply.text);
        let placed = session.model.project_text();
        let frame = session.last.as_ref().unwrap().model_to_plate[0];
        let to = |model: [f64; 3]| -> [f64; 3] { std::array::from_fn(|i| (0..3).map(|j| frame[i][j] * model[j]).sum::<f64>() + frame[i][3]) };
        let log = |session: &fab_ai::Session| session.model.state().strokes.get("t.strokes").cloned();
        let mut previous = None;
        for y in [0.0, 4.0] {
            // a drag of 50 dabs is one stroke
            let points = (0..50).map(|k| to([10.0, y, -9.0 + k as f64 * 0.36])).collect();
            let action = PartAction::PaintStroke { object: "t/body".into(), rgb: [0.8, 0.05, 0.05], color: None, erase: false, radius: 3.0, points, target: fab::api::PaintTarget::Color };
            let batch = edits(session.compiled.as_ref().unwrap(), session.last.as_ref().unwrap(), &action, Some(&frame)).unwrap();
            previous = log(&session);
            let reply = session.commit("paint", batch);
            assert!(reply.ok, "{}", reply.text);
        }
        let script = session.source("t.cad.splash").unwrap();
        let strokes = log(&session).expect("the strokes file");
        assert_eq!(session.model.project_text(), placed, "painting never moves the object");
        assert_eq!(script.matches("m.strokes(\"t.strokes\")").count(), 1, "{script}");
        assert_eq!(strokes.len(), 2, "one record per stroke");
        assert_eq!(strokes.palette.len(), 1, "one swatch for one colour");
        assert!((strokes.stroke(0).radius - 1.5).abs() < 1e-9, "the radius in the model frame");
        assert!(!script.contains("m.sphere"));
        let ev = session.last.clone().unwrap();
        let paint = ev.paint[0].clone().expect("the strokes paint the part");
        assert_eq!(paint.program.layers.len(), 1);
        assert_eq!(paint.colors.len(), 2);
        assert_eq!(paint.slot_at(to([10.0, 0.0, 0.0])), 1);
        assert_eq!(paint.slot_at(to([-10.0, -10.0, 0.0])), 0);
        let reply = session.undo_redo(fab_model::HistoryAction::Undo);
        assert!(reply.ok, "{}", reply.text);
        assert_eq!(log(&session), previous);
        let reply = session.undo_redo(fab_model::HistoryAction::Undo);
        assert!(reply.ok, "{}", reply.text);
        assert_eq!(session.source("t.cad.splash").unwrap(), source);
    }

    /// The brush paints the support channels the same way it paints colour: a stroke is one record
    /// of the part's own file for that channel, read by its support statement, and the colour is
    /// left alone. The region holds where the stroke went and nowhere else; an erase stroke takes
    /// it off. A fill writes the statement with the feature.
    #[test]
    fn a_support_stroke_is_a_record_of_its_own_file_read_by_its_statement() {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
        let source = format!("{}let body = m.box({{size: [20mm 20mm 20mm]}})\nobject(body)\n", fab_model::program::EMPTY);
        let (reply, _) = session.set_source("t.cad.splash", &source);
        assert!(reply.ok, "{}", reply.text);
        let reply = session.commit("place", vec![Change::Place { instance: "t".into(), object: "body".into(), arg: PlaceArg::At(Some([100.0, 120.0])) }]);
        assert!(reply.ok, "{}", reply.text);
        let frame = session.last.as_ref().unwrap().model_to_plate[0];
        let to = |model: [f64; 3]| -> [f64; 3] { std::array::from_fn(|i| (0..3).map(|j| frame[i][j] * model[j]).sum::<f64>() + frame[i][3]) };
        let stroke = |session: &mut fab_ai::Session, target: PaintTarget, erase: bool| {
            let points = (0..20).map(|k| to([10.0, 0.0, -9.0 + k as f64 * 0.9])).collect();
            let action = PartAction::PaintStroke { object: "t/body".into(), rgb: target.tint().unwrap(), color: None, erase, radius: 3.0, points, target };
            let batch = edits(session.compiled.as_ref().unwrap(), session.last.as_ref().unwrap(), &action, Some(&frame)).unwrap();
            let reply = session.commit("paint", batch);
            assert!(reply.ok, "{}", reply.text);
        };
        let holds = |session: &fab_ai::Session, block: bool, p: [f64; 3]| -> Option<bool> {
            let ev = session.last.as_ref().unwrap();
            let paint = ev.plate.parts[0].paint.as_ref()?;
            let region = if block { paint.support_block.as_ref() } else { paint.support_enforce.as_ref() }?;
            Some((region.at)(to(p)) < 0.0)
        };
        for (target, file, statement) in [(PaintTarget::SupportBlock, "t.block.strokes", "m.support_block(\"t.block.strokes\")"), (PaintTarget::SupportEnforce, "t.enforce.strokes", "m.support_enforce(\"t.enforce.strokes\")")] {
            let block = target == PaintTarget::SupportBlock;
            assert_eq!(holds(&session, block, [10.0, 0.0, 0.0]), None, "{target:?}: nothing before the stroke");
            stroke(&mut session, target, false);
            stroke(&mut session, target, false);
            let script = session.source("t.cad.splash").unwrap();
            assert_eq!(script.matches(statement).count(), 1, "{script}");
            assert!(!script.contains("m.strokes("), "the colour's file is not read: {script}");
            assert_eq!(session.model.state().strokes.get(file).map(|l| l.len()), Some(2), "{target:?}: one record per stroke in its own file");
            assert_eq!(holds(&session, block, [10.0, 0.0, 0.0]), Some(true), "{target:?}: under the stroke");
            assert_eq!(holds(&session, block, [-10.0, 0.0, 0.0]), Some(false), "{target:?}: away from it");
            let ev = session.last.clone().unwrap();
            let paint = ev.paint[0].as_ref().unwrap();
            assert!(!paint.program.has(fab_paint_field::Channel::Color), "the colour is left alone");
            assert_eq!(paint.colors.len(), 1, "{target:?}: its strokes add no colour");
            let r = ev.plate.parts[0].paint.as_ref().and_then(|p| if block { p.support_block.clone() } else { p.support_enforce.clone() }).unwrap();
            let (lo, hi) = r.bounds.expect("a strokes file holds only where its strokes go");
            let under = to([10.0, 0.0, 0.0]);
            assert!((0..3).all(|k| lo[k] <= under[k] && under[k] <= hi[k]), "{target:?}: the strokes' box {lo:?} {hi:?} holds the stroke");
            assert!(hi[0] - lo[0] < 12.0 && hi[1] - lo[1] < 30.0, "{target:?}: the strokes' box {lo:?} {hi:?}");
            stroke(&mut session, target, true);
            assert_eq!(holds(&session, block, [10.0, 0.0, 0.0]), Some(false), "{target:?}: an erase stroke takes it off");
        }
        let action = PartAction::PaintFill { object: "t/body".into(), features: vec![None], rgb: [0.0; 3], color: None, target: PaintTarget::SupportEnforce };
        let batch = edits(session.compiled.as_ref().unwrap(), session.last.as_ref().unwrap(), &action, None).unwrap();
        let reply = session.commit("fill", batch);
        assert!(reply.ok, "{}", reply.text);
        assert!(session.source("t.cad.splash").unwrap().ends_with("m.support_enforce(body)\n"));
        assert_eq!(holds(&session, false, [-5.0, 0.0, 0.0]), Some(true), "the filled part is enforced all over");
    }

    /// The Pattern fill writes the guide's `m.fill(part, Mix{..})` of every kind, and each
    /// builds into a mix of the part's own colour and the paint colour through the part: cells
    /// and bands regions of both, noise and marble a blend, a ramp the own colour at the foot
    /// and the paint colour at the top.
    #[test]
    fn a_pattern_fill_writes_a_mix_of_the_part_and_the_paint_colour() {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
        let source = format!("{}let body = m.box({{size: [20mm 20mm 20mm] at: [0 10mm 0]}})\nobject(body)\n", fab_model::program::EMPTY);
        for kind in PatternKind::ALL {
            let (reply, _) = session.set_source("t.cad.splash", &source);
            assert!(reply.ok, "{}", reply.text);
            let ev = session.last.clone().unwrap();
            let frame = ev.model_to_plate[0];
            let pattern = PaintPattern { kind, scale: 4.0, seed: 3, sharp: kind.sharp(), balance: if kind == PatternKind::Noise { -0.1 } else { 0.0 } };
            let action = PartAction::PaintPattern { object: "t/body".into(), own: [0.05, 0.2, 0.85], own_color: Some("Blue".into()), rgb: [0.9, 0.75, 0.05], color: Some("Yellow".into()), pattern };
            let batch = edits(session.compiled.as_ref().unwrap(), &ev, &action, Some(&frame)).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
            let reply = session.commit("pattern", batch);
            assert!(reply.ok, "{kind:?}: {}", reply.text);
            let script = session.source("t.cad.splash").unwrap().to_string();
            let line = script.lines().last().unwrap();
            assert!(line.starts_with("m.fill(body, Mix{materials: [Material{"), "{kind:?}: {line}");
            let paint = session.last.as_ref().unwrap().paint[0].clone().expect("painted");
            let Some(fab_paint_field::Op::Mix { mix, .. }) = paint.program.layers.last().map(|l| l.op.clone()) else { panic!("{kind:?}: no mix layer in {:?}", paint.program.layers) };
            // yellow's share at points through the block (model frame, y up), by height
            let yellow = |slot: u8| (paint.colors.get(slot as usize).and_then(|c| c.color.as_deref()) == Some("Yellow")) as u8 as f64;
            let share = |q: [f64; 3]| {
                let s = paint.program.sample(fab_paint_field::Channel::Color, q);
                match s.mix {
                    Some((o, t)) => yellow(s.slot) * (1.0 - t as f64) + yellow(o) * t as f64,
                    None => yellow(s.slot),
                }
            };
            // through the block; a pattern grown on the skin, over its faces
            let grid: Vec<[f64; 3]> = match kind {
                PatternKind::Spots | PatternKind::Stripes => (0..1200).map(|i| {
                    let (face, u, v) = (i / 200, -9.5 + ((i % 200) % 20) as f64, -9.5 + ((i % 200) / 20) as f64 * 2.0);
                    let w = if face % 2 == 0 { -9.8 } else { 9.8 };
                    match face / 2 { 0 => [w, 10.0 + u, v], 1 => [u, 10.0 + w, v], _ => [u, 10.0 + v, w] }
                }).collect(),
                _ => (0..1000).map(|i| [-9.0 + (i % 10) as f64 * 2.0, 0.5 + ((i / 10) % 10) as f64 * 2.0, -9.0 + (i / 100) as f64 * 2.0]).collect(),
            };
            let mean = grid.iter().map(|&q| share(q)).sum::<f64>() / grid.len() as f64;
            let blended = grid.iter().filter(|&&q| paint.program.sample(fab_paint_field::Channel::Color, q).mix.is_some()).count();
            println!("{kind:?}: {line}\n  yellow {mean:.2}, {blended} of {} points blend, slots {:?}", grid.len(), mix.slots);
            assert_eq!(mix.slots.len(), 2, "{kind:?}");
            assert!((0.15..=0.85).contains(&mean), "{kind:?}: yellow {mean:.2}");
            match kind {
                PatternKind::Cells | PatternKind::Spots | PatternKind::Stripes => assert_eq!(blended, 0, "{kind:?} is sharp"),
                PatternKind::Noise | PatternKind::Marble => assert!(blended > 500, "{kind:?} blends: {blended}"),
                PatternKind::Bands => {}
                PatternKind::Ramp => {
                    assert!(share([0.0, 0.2, 0.0]) < 0.05 && share([0.0, 19.8, 0.0]) > 0.95, "the ramp climbs from blue to yellow: {} {}", share([0.0, 0.2, 0.0]), share([0.0, 19.8, 0.0]));
                }
            }
        }
    }

    /// An object with no name of its own is known by the line it is written on: a support stroke,
    /// a fill and a smooth fill on it add their statements where they move no such object, so it
    /// keeps its name and its placement.
    #[test]
    fn support_paint_keeps_an_unnamed_objects_placement() {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
        let source = format!("{}object(m.box({{size: [20mm 20mm 20mm]}}))\nlet spare = m.sphere({{radius: 2mm}})\n", fab_model::program::EMPTY);
        let (reply, _) = session.set_source("t.cad.splash", &source);
        assert!(reply.ok, "{}", reply.text);
        let name = session.compiled.as_ref().unwrap().instances[0].script.objects[0].name.clone();
        assert!(name.contains('@'), "{name}");
        let reply = session.commit("place", vec![Change::Place { instance: "t".into(), object: name.clone(), arg: PlaceArg::At(Some([100.0, 120.0])) }]);
        assert!(reply.ok, "{}", reply.text);
        let placed = (session.model.project_text(), session.last.as_ref().unwrap().model_to_plate[0]);
        let frame = placed.1;
        let object = format!("t/{name}");
        let to = |model: [f64; 3]| -> [f64; 3] { std::array::from_fn(|i| (0..3).map(|j| frame[i][j] * model[j]).sum::<f64>() + frame[i][3]) };
        let actions = [
            PartAction::PaintStroke { object: object.clone(), rgb: PaintTarget::SupportBlock.tint().unwrap(), color: None, erase: false, radius: 3.0, points: (0..20).map(|k| to([10.0, 0.0, -9.0 + k as f64 * 0.9])).collect(), target: PaintTarget::SupportBlock },
            PartAction::PaintFlood { object: object.clone(), seed: to([0.0, -10.0, 0.0]), angle: 30.0, rgb: [0.0; 3], color: None, target: PaintTarget::SupportEnforce },
        ];
        for action in actions {
            let batch = edits(session.compiled.as_ref().unwrap(), session.last.as_ref().unwrap(), &action, Some(&frame)).unwrap();
            let reply = session.commit("paint", batch);
            assert!(reply.ok, "{}", reply.text);
            let script = session.source("t.cad.splash").unwrap().to_string();
            assert_eq!(session.compiled.as_ref().unwrap().instances[0].script.objects[0].name, name, "{script}");
            assert_eq!((session.model.project_text(), session.last.as_ref().unwrap().model_to_plate[0]), placed, "{script}");
        }
    }

    /// Procedural paint: the noise vase's yellow region is a Splash noise field, read through
    /// the same field evaluator as a shape, over a vase filled blue whole (its own colour).
    /// Round the belly about half the surface is yellow, and the share of the yellow wall length
    /// that comes in runs at least a nozzle wide (what the slicer's walls can print) is reported.
    /// The guide's materials by a field (a noise blend, Voronoi cells) build the same way: the
    /// blend's share about half, blending nearly everywhere, the cells sharp.
    #[test]
    fn the_noise_vase_mixes_two_colours_by_a_field() {
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = crate::demo_part::demo("noise_vase").unwrap().model().unwrap();
        session.path = None;
        let ev = session.rebuild().expect("the noise vase builds");
        let p = ev.paint[0].as_ref().expect("painted");
        assert_eq!(p.colors.iter().map(|c| c.color.as_deref()).collect::<Vec<_>>(), vec![None, Some("Blue"), Some("Yellow")]);
        // the vase is filled blue whole, so blue is its own colour and the noise the one layer over it
        assert_eq!(ev.part_color(0), Some("Blue"), "the vase prints blue all through");
        assert!(matches!(p.program.layers.as_slice(), [l] if matches!(l.region, fab_paint_field::Region::Field { .. })), "the noise is a field region over it");
        // rings round the belly (model frame, y up): the slot along each, in 0.02 mm steps
        let nozzle = 0.4;
        let (mut yellow, mut total, mut kept) = (0.0f64, 0.0f64, 0.0f64);
        for k in 0..24 {
            let y = 10.0 + k as f64 * 1.2;
            let v = (y - 23.8) / 25.2;
            let r = 21.0 * (1.0 - v * v).sqrt();
            let steps = (std::f64::consts::TAU * r / 0.02) as usize;
            let mut run = 0.0f64;
            for i in 0..=steps {
                let a = i as f64 / steps as f64 * std::f64::consts::TAU;
                let slot = p.program.sample(fab_paint_field::Channel::Color, [r * a.cos(), y, r * a.sin()]).slot;
                if i < steps {
                    total += 0.02;
                }
                if slot == 2 && i < steps {
                    yellow += 0.02;
                    run += 0.02;
                } else {
                    if run >= nozzle {
                        kept += run;
                    }
                    run = 0.0;
                }
            }
        }
        let share = yellow / total;
        let survives = kept / yellow.max(1e-9);
        println!("[noise] yellow {:.0}% of the belly; {:.1}% of the yellow wall length is in runs of at least {nozzle} mm", share * 100.0, survives * 100.0);
        assert!((0.3..=0.7).contains(&share), "noise at zero splits about evenly: {:.0}% yellow", share * 100.0);
        assert!(survives > 0.9, "a 7 mm noise prints at a 0.4 mm nozzle: {:.1}% survives", survives * 100.0);
        // the guide's materials by a field: a noise blend and Voronoi cells, both of blue and yellow
        let vase = include_str!("../resources/vase.cad.splash");
        let colours = "let blue = Material{albedo: [0.05 0.2 0.85] name: \"Blue\"}\nlet yellow = Material{albedo: [0.9 0.75 0.05] name: \"Yellow\"}\n";
        for (name, fill) in [("blend", "Mix{materials: [blue yellow] field: fn(p) { 0.5 + 0.5 * noise.fbm(p / 8mm, 1, 3) }}"), ("cells", "Mix{materials: [blue yellow] field: fn(p) { noise.cell_id(p / 6mm, 1) } sharp: 1}")] {
            let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
            session.model = fab_model::FabModel::with_script(name, "mix.cad.splash", &format!("{vase}\n{colours}m.fill(vase, {fill})\n"));
            session.path = None;
            let ev = session.rebuild().unwrap_or_else(|e| panic!("the {name} vase builds: {e:?}"));
            let p = ev.paint[0].as_ref().expect("painted");
            // yellow's share at each point on the belly, and how many points blend
            let (mut yellow, mut blended, mut n) = (0.0f64, 0usize, 0usize);
            for k in 0..20 {
                let y = 12.0 + k as f64 * 1.3;
                let v = (y - 23.8) / 25.2;
                let r = 21.0 * (1.0 - v * v).sqrt();
                for i in 0..400 {
                    let a = i as f64 / 400.0 * std::f64::consts::TAU;
                    let s = p.program.sample(fab_paint_field::Channel::Color, [r * a.cos(), y, r * a.sin()]);
                    let here = (s.slot == 2) as u8 as f64;
                    yellow += match s.mix {
                        Some((other, t)) => here * (1.0 - t as f64) + (other == 2) as u8 as f64 * t as f64,
                        None => here,
                    };
                    blended += s.mix.is_some() as usize;
                    n += 1;
                }
            }
            let (share, blends) = (yellow / n as f64, blended as f64 / n as f64);
            println!("[noise] {name}: {:.0}% yellow, {:.0}% of the points blend", share * 100.0, blends * 100.0);
            assert!((0.3..=0.7).contains(&share), "{name}: {:.0}% yellow", share * 100.0);
            if name == "blend" {
                assert!(blends > 0.9, "the noise blends: {:.0}% of the points", blends * 100.0);
            } else {
                assert!(blended == 0, "sharp cells blend nowhere: {blended} points");
            }
        }
    }
}
