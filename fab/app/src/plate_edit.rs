//! The direct plate edits ([`PartAction`]). Moves, turns, scales and drops are
//! [`PartMove`]s for `ModelHost::place`, the one edit path the headset shares; the rest
//! (name, filament, nozzle, settings, arrange, duplicate, delete, add) are changes of the
//! project: an object's placement, a script placed again or taken away, a new object
//! script. Either way an edit is one undo step, and none of them touches a script's code.

use fab::api::{PartAction, PlateInfo};
use fab_ai::PartMove;
use fab_model::eval::{free_spot, no_room, spot_reached, Evaluated};
use fab_model::program::{split_id, split_link, Compiled};
use fab_model::project::ColorDef;
use fab_model::{script_file, Change, PlaceArg};

/// A print colour as the project holds it (an implicit one made explicit as it shows).
pub fn color_def(ev: &Evaluated, name: &str) -> ColorDef {
    match ev.colors.iter().find(|c| c.name == name) {
        Some(c) => ColorDef { name: name.into(), rgb: c.rgb.map(fab_model::program::color_srgb), at: c.at.clone(), material: c.material.clone() },
        None => ColorDef::new(name),
    }
}

/// `def` printing from this tray on `printer` (its place there saved, others kept).
pub fn saved_at(mut def: ColorDef, printer: &str, side: &str, tray: &str) -> ColorDef {
    def.at.retain(|a| a.printer != printer);
    def.at.push(fab_plate::PrintColorAt { printer: printer.into(), side: side.into(), tray: tray.into() });
    def
}

/// A colour name no print colour has yet ("Colour 3").
pub fn new_color_name(ev: &Evaluated) -> String {
    (1..).map(|i| format!("Colour {i}")).find(|n| !ev.colors.iter().any(|c| c.name == *n)).unwrap()
}

/// The colour `AssignTray` prints in, saved at its tray: the given one, or a new one.
pub fn tray_color(ev: &Evaluated, color: &Option<String>, printer: &str, side: &str, tray: &str) -> (String, Change) {
    let name = color.clone().unwrap_or_else(|| new_color_name(ev));
    let def = saved_at(color_def(ev, &name), printer, side, tray);
    (name.clone(), Change::Color { name, set: Some(def) })
}

/// A move of `object` by a turn about Z (degrees) and a uniform scale, both
/// about `c`, then a shift by `by` (mm).
fn delta(c: [f64; 3], turn_deg: f64, k: f64, by: [f64; 2]) -> [[f64; 4]; 3] {
    let (s, co) = turn_deg.to_radians().sin_cos();
    let r = [[co * k, -s * k, 0.0], [s * k, co * k, 0.0], [0.0, 0.0, k]];
    let row = |i: usize, shift: f64| [r[i][0], r[i][1], r[i][2], c[i] - (r[i][0] * c[0] + r[i][1] * c[1] + r[i][2] * c[2]) + shift];
    [row(0, by[0]), row(1, by[1]), row(2, 0.0)]
}

/// Where placed part `i`'s model origin sits on the plate (mm).
fn origin(ev: &Evaluated, i: usize) -> Option<[f64; 3]> {
    ev.placed_frame(i).map(|f| [f[0][3], f[1][3], f[2][3]])
}

/// Placed part `i` linked to the host's own geometry (`m.link`, a board's `board:<name>`) with that
/// geometry's frame of the same name: the key and the frame on the plate (mm), where the host
/// draws its geometry in place of the part's own mesh. An insert keeps its link and frame on the
/// plate; a printed part's frame is its object's. O(objects) for a linked printed part, O(1) else.
pub fn linked_place(ev: &Evaluated, i: usize) -> Option<(&str, [[f64; 4]; 3])> {
    let (key, (name, frame), to_plate) = match i.checked_sub(ev.plate.parts.len()) {
        Some(k) => {
            let e = ev.plate.excluded.get(k)?;
            (e.link.as_deref()?, e.frame.as_ref()?, &e.model_to_plate)
        }
        None => {
            let p = &ev.plate.parts[i];
            let key = p.link.as_deref()?;
            (key, ev.objects.iter().find(|o| o.name == p.object)?.frame.as_ref()?, ev.model_to_plate.get(i)?)
        }
    };
    (key == name).then(|| (key, fab_model::weave::compose(to_plate, frame)))
}

/// The plate moves of an edit (`None`: not a move).
pub fn moves(ev: &Evaluated, action: &PartAction) -> Option<Vec<PartMove>> {
    let part = |object: &str| ev.plate.placed_parts().position(|p| p.object == object);
    match action {
        // Where it is is its origin (what its `at` places), and it turns and scales about that.
        PartAction::Place { object, at, rotate } => {
            let i = part(object)?;
            let o = origin(ev, i)?;
            let now = ev.placed_handles(i).map(|h| h.rotate[2]).unwrap_or(0.0);
            Some(vec![PartMove { object: object.clone(), delta: delta(o, rotate - now, 1.0, [at[0] - o[0], at[1] - o[1]]) }])
        }
        PartAction::LayFlat { object, normal } => Some(vec![fab_ai::lay_flat_move(ev, object, *normal)?]),
        PartAction::Drop { object } => {
            let i = part(object)?;
            let c = ev.plate.placed_part(i)?.bounds().center();
            Some(vec![PartMove { object: object.clone(), delta: delta(c, 0.0, 1.0, [0.0, 0.0]) }])
        }
        _ => None,
    }
}

/// Try beside the whole source instance with a 10 mm gap, then use the model's
/// free-spot search, either where every nozzle its colours print with reaches (None: no such
/// instance; an error: no spot there fits it). Clone/Copy anchor their first emitted object,
/// not the group centre.
fn duplicate_at(c: &Compiled, ev: &Evaluated, object: &str) -> Result<Option<[f64; 2]>, String> {
    let Some((inst, _)) = c.object(object) else { return Ok(None) };
    let of_instance: Vec<usize> = ev.plate.placed_parts().enumerate().filter(|(_, p)| split_id(&p.object).is_some_and(|(i, _)| i == inst.name)).map(|(i, _)| i).collect();
    let mut bounds = of_instance.iter().filter_map(|&i| ev.plate.placed_part(i)).map(|p| p.bounds());
    let Some(first) = bounds.next() else { return Ok(None) };
    let (mut lo, mut hi) = (first.min, first.max);
    for b in bounds {
        for k in 0..3 {
            lo[k] = lo[k].min(b.min[k]);
            hi[k] = hi[k].max(b.max[k]);
        }
    }
    let (size, height) = ([hi[0] - lo[0], hi[1] - lo[1]], hi[2] - lo[2]);
    let centre = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
    let beside = [hi[0] + 10.0 + size[0] * 0.5, centre[1]];
    let clear = ev.plate.placed_parts().all(|p| {
        let b = p.bounds();
        beside[0] + size[0] * 0.5 + 10.0 <= b.min[0]
            || beside[0] - size[0] * 0.5 - 10.0 >= b.max[0]
            || beside[1] + size[1] * 0.5 + 10.0 <= b.min[1]
            || beside[1] - size[1] * 0.5 - 10.0 >= b.max[1]
    });
    let colours: Vec<&str> = of_instance.iter().flat_map(|&i| ev.part_colours(i)).collect();
    let nozzles = ev.nozzles_for(&colours);
    let spot = match (clear && spot_reached(ev, beside, size, height, &nozzles)).then_some(beside).or_else(|| free_spot(ev, size, height, &nozzles)) {
        Some(spot) => spot,
        None => return Err(no_room(&ev.machine, &inst.name, size, &nozzles)),
    };
    // The first object's `at` is where its model origin sits: the whole instance shifts from
    // its centre to the spot.
    let anchor = inst.placed.first().and_then(|placed| {
        let id = fab_model::program::part_id(&inst.name, &inst.script.program.solids[placed.solid].id);
        let i = ev.plate.placed_parts().position(|p| p.object == id)?;
        ev.placed_frame(i).map(|f| [f[0][3], f[1][3]])
    }).unwrap_or([centre[0], centre[1]]);
    Ok(Some([spot[0] + anchor[0] - centre[0], spot[1] + anchor[1] - centre[1]]))
}

/// A free spot for a new object of footprint `size` (mm) named `name`, in the colour a new
/// object prints in: where its nozzle reaches, or why there is none. Its height is not known
/// before it builds: the spot keeps its skirt in reach, and an outer brim the job gives every
/// part, not an automatic brim a tall narrow one would get.
fn new_object_at(ev: &Evaluated, name: &str, size: [f64; 2]) -> Result<[f64; 2], String> {
    let nozzles = ev.nozzles_for(&[ev.new_object_colour()]);
    free_spot(ev, size, 0.0, &nozzles).ok_or_else(|| no_room(&ev.machine, name, size, &nozzles))
}

/// Resolve the whole selection before changing anything. A body's emitted object is the
/// unit of deletion; deduplicate assemblies, then remove links before their base can promote
/// one. Count placed objects, not everything the script emits (some were already deleted).
pub fn delete_edits(c: &Compiled, objects: &[String], scripts: bool) -> Result<Vec<Change>, String> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut targets = BTreeMap::<String, BTreeSet<String>>::new();
    for id in objects {
        let Some((instance, object)) = c.object(id) else {
            return Err("The selected part is no longer in the current model. Select it again to delete it.".into());
        };
        targets.entry(if scripts { instance.script_instance.clone() } else { instance.name.clone() })
            .or_default().insert(object.name.clone());
    }
    if scripts {
        return Ok(targets.into_keys().map(|instance| Change::RemoveScript { instance }).collect());
    }
    let mut targets: Vec<_> = targets.into_iter().collect();
    targets.sort_by_key(|(instance, _)| std::cmp::Reverse(split_link(instance).1));
    let mut out = Vec::new();
    for (instance, objects) in targets {
        let inst = c.instance(&instance).expect("resolved instance");
        if inst.placed.iter().all(|p| objects.contains(&p.object)) {
            out.push(Change::RemoveInstance { instance });
        } else {
            // Unspecified objects are placed automatically. Keep an explicit omission until
            // the last object is removed; compile_state omits it from both plate and list.
            out.extend(objects.into_iter().map(|object| Change::Place { instance: instance.clone(), object, arg: PlaceArg::Show(false) }));
        }
    }
    Ok(out)
}

/// One placement change per placement the objects name (assembly rows can name several bodies
/// of the same placement).
fn placements(c: &Compiled, objects: &[String], arg: PlaceArg) -> Vec<Change> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for object in objects {
        if let Some(Change::Place { instance, object, arg }) = Change::place(c, object, arg.clone()) {
            if seen.insert((instance.clone(), object.clone())) { out.push(Change::Place { instance, object, arg }); }
        }
    }
    out
}

/// The changes of one direct edit (empty when there is nothing to do), or why it cannot be
/// made (no free spot for a new object).
pub fn edits(c: &Compiled, ev: &Evaluated, action: &PartAction) -> Result<Vec<Change>, String> {
    let place = |object: &str, arg: PlaceArg| Change::place(c, object, arg).into_iter().collect::<Vec<_>>();
    Ok(match action {
        PartAction::SetPrint { objects, print } => placements(c, objects, PlaceArg::Print(*print)),
        PartAction::SetHidden { objects, hidden } => placements(c, objects, PlaceArg::Hidden(*hidden)),
        PartAction::Rename { object, name } => place(object, PlaceArg::Name(Some(name.clone()))),
        // The insert's own statement in its script: `embed: true`, or none (it rides in its part).
        // Each script once, though linked placements share it.
        PartAction::SetEmbed { objects, embed } => {
            let mut out = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for object in objects {
                // a heat-set insert melts in after the print: it is never printed into its part
                let Some((inst, emitted)) = c.object(object).filter(|(_, e)| e.insert.as_ref().is_some_and(|i| i.melt.is_none())) else { continue };
                let solid = inst.script.program.solids[emitted.solid].id.clone();
                if seen.insert((inst.script.file.clone(), solid.clone())) {
                    let edit = fab_model::Edit::RootArg { kind: "insert".into(), solid, name: "embed".into(), code: embed.then(|| "true".to_string()) };
                    out.push(Change::Script { file: inst.script.file.clone(), edit });
                }
            }
            out
        }
        // Its `place` scale itself, along its own axes, about its origin, which stays: a part the
        // plate arranges is set down where it stands, or its new size would move it.
        PartAction::Scale { object, scale } => {
            let mut out = place(object, PlaceArg::Scale(scale.map(|k| ((k * 1000.0).round() / 1000.0).clamp(0.001, 1000.0))));
            if let Some(o) = ev.plate.placed_parts().position(|p| p.object == *object).and_then(|i| origin(ev, i)) {
                out.extend(place(object, PlaceArg::At(Some([o[0], o[1]]))));
            }
            out
        }
        PartAction::SetColor { objects, color } => {
            // An implicit colour (a part that named none) becomes the project's.
            let mut out: Vec<Change> = (!c.colors.iter().any(|x| x.name == *color)).then(|| Change::Color { name: color.clone(), set: Some(color_def(ev, color)) }).into_iter().collect();
            out.extend(placements(c, objects, PlaceArg::Color(Some(color.clone()))));
            out
        }
        PartAction::MapColor { color, printer, at } => {
            let def = match at {
                Some((side, tray)) => saved_at(color_def(ev, color), printer, side, tray),
                None => {
                    let mut def = color_def(ev, color);
                    def.at.retain(|a| a.printer != *printer);
                    def
                }
            };
            vec![Change::Color { name: color.clone(), set: Some(def) }]
        }
        PartAction::SetColorRgb { color, rgb } => {
            let mut def = color_def(ev, color);
            def.rgb = rgb.map(fab_model::program::color_srgb);
            vec![Change::Color { name: color.clone(), set: Some(def) }]
        }
        PartAction::AssignTray { objects, color, printer, side, tray, paint } if paint.is_empty() => {
            let (name, change) = tray_color(ev, color, printer, side, tray);
            let mut out = vec![change];
            out.extend(placements(c, objects, PlaceArg::Color(Some(name))));
            out
        }
        PartAction::SetPrintSetting { objects, key, value } => objects.iter().flat_map(|object| place(object, PlaceArg::Setting { key: key.clone(), value: value.clone() })).collect(),
        PartAction::DeleteTree { object, tips } => place(object, PlaceArg::NoTree(tips.clone())),
        PartAction::ClearPrintSettings { objects } => objects.iter().flat_map(|object| place(object, PlaceArg::Settings(Vec::new()))).collect(),
        PartAction::CopyPrintSettings { from, to } => {
            let Some(part) = ev.plate.placed_parts().find(|p| p.object == *from) else { return Ok(Vec::new()) };
            let own: Vec<(String, String)> = fab_plate::own_settings(&part.print).cloned().collect();
            to.iter().flat_map(|object| place(object, PlaceArg::Settings(own.clone()))).collect()
        }
        PartAction::SetPlateSetting { key, value } => match value {
            Some(value) => vec![Change::PlateSettings { set: vec![(key.clone(), value.clone())], reset: Vec::new() }],
            None => vec![Change::PlateSettings { set: Vec::new(), reset: vec![key.clone()] }],
        },
        PartAction::ClearPlateSettings => vec![Change::PlateSettings { set: Vec::new(), reset: ev.plate.settings.iter().map(|(k, _)| k.clone()).collect() }],
        PartAction::SetArrangeGap(gap) => vec![Change::ArrangeGap(Some(*gap))],
        // Every object back to its slot in the plate's arrangement.
        PartAction::Arrange => {
            let mut out = Vec::new();
            for i in &c.instances {
                for o in &i.script.objects {
                    out.push(Change::Place { instance: i.name.clone(), object: o.name.clone(), arg: PlaceArg::At(None) });
                }
            }
            out
        }
        // One linked placement or an independent script, with its source overrides.
        PartAction::DuplicateLinked { object } | PartAction::DuplicateCopy { object } => {
            let Some((inst, _)) = c.object(object) else { return Ok(Vec::new()) };
            let Some(at) = duplicate_at(c, ev, object)? else { return Ok(Vec::new()) };
            vec![if matches!(action, PartAction::DuplicateCopy { .. }) {
                Change::Copy { instance: inst.name.clone(), at: Some(at) }
            } else {
                Change::Clone { instance: inst.name.clone(), at: Some(at) }
            }]
        }
        // A demo object's script (once in the project, however it was edited), placed in a free spot.
        PartAction::Add { shape } if crate::demo_part::ADDABLE.iter().any(|(id, ..)| id == shape) => {
            let (_, file, source, size) = crate::demo_part::ADDABLE.iter().find(|(id, ..)| id == shape).unwrap();
            let source = (!c.instances.iter().any(|i| i.script.file == *file)).then(|| source.to_string());
            vec![Change::AddInstance { file: file.to_string(), source, name: None, at: Some(new_object_at(ev, shape, *size)?) }]
        }
        // A new object script of one primitive, placed in a free spot.
        PartAction::Add { shape } => {
            let taken = |n: &str| c.instance(n).is_some() || c.instances.iter().any(|i| i.script.file == script_file(n));
            let name = (1..).map(|i| format!("{shape}{i}")).find(|n| !taken(n)).unwrap();
            let code = match shape.as_str() {
                "cylinder" => "m.cylinder({radius: 10mm height: 20mm segments: 64})",
                "sphere" => "m.sphere({radius: 12mm segments: 64 rings: 32})",
                "cone" => "m.cylinder({radius: 12mm top_radius: 0 height: 24mm segments: 64})",
                "torus" => "m.torus({radius: 14mm tube: 4mm segments: 64 sides: 24})",
                _ => "m.box({size: [20mm 20mm 20mm] radius: 1mm})",
            };
            let source = format!("{}let {name} = {code}\nobject({name})\n", fab_model::program::EMPTY);
            let at = new_object_at(ev, &name, [30.0, 30.0])?;
            vec![Change::AddInstance { file: script_file(&name), source: Some(source), name: Some(name), at: Some(at) }]
        }
        _ => Vec::new(),
    })
}

/// The name of object `id` (`instance/object`), which its bodies are named by: its placement's
/// own, else the object's.
fn object_name(instance: Option<&fab_model::program::Instance>, id: &str) -> String {
    let object = split_id(id).map_or(id, |(_, o)| o);
    instance.and_then(|i| i.placed.iter().find(|p| p.object == object)).and_then(|p| p.name.clone()).unwrap_or_else(|| object.to_string())
}

/// The plate as the panels see it.
pub fn plate_info(c: Option<&Compiled>, ev: &Evaluated, revision: u64) -> PlateInfo {
    let instances: std::collections::HashMap<_, _> = c.into_iter().flat_map(|c| &c.instances).map(|i| (i.name.as_str(), i)).collect();
    let parts = ev
        .plate
        .placed_parts()
        .enumerate()
        .map(|(i, p)| {
            let b = p.bounds();
            let h = ev.placed_handles(i).cloned().unwrap_or_default();
            let report = ev.reports.get(i);
            let excluded = i.checked_sub(ev.plate.parts.len()).and_then(|i| ev.plate.excluded.get(i));
            let instance = split_id(&p.object).map_or(p.object.as_str(), |(i, _)| i);
            let compiled = instances.get(instance);
            let (script, link) = split_link(instance);
            fab::api::PlatePart {
                printed: excluded.is_none(),
                hidden: h.hidden,
                insert: excluded.is_some_and(|p| p.insert.is_some()),
                embed: excluded.and_then(|p| p.insert.as_ref()).is_some_and(|insert| insert.embed),
                melt: excluded.and_then(|p| p.insert.as_ref()?.melt.as_ref()).map(|m| m.size.clone()).unwrap_or_default(),
                insert_into: excluded.and_then(|p| p.insert.as_ref()).map(|insert| insert.into.clone()).unwrap_or_default(),
                object: p.object.clone(),
                script: compiled.map_or(script, |i| i.script_instance.as_str()).to_string(),
                link: compiled.map_or(link, |i| i.link),
                file: compiled.map(|i| i.script.file.clone()).unwrap_or_default(),
                name: p.name.clone(),
                print_color: ev.part_color(i).unwrap_or_default().to_string(),
                color: p.color,
                paint: ev.placed_paint(i).map(|p| std::sync::Arc::new(fab_plate::paint::PartPaint { colors: p.colors.clone(), tri: Vec::new() })),
                model_from_printer: ev.placed_frame(i).and_then(crate::paint_edit::inverse).unwrap_or([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]),
                features: std::sync::Arc::new(ev.placed_features(i).map(|f| f.iter().map(|f| fab::api::PaintFeature { name: f.name.clone(), region: f.region.clone(), volume: f.volume }).collect()).unwrap_or_default()),
                // what the views show: a picture's face in its stacks' colours, else the paint
                paint_view: ev.placed_paint(i).filter(|p| [fab_paint_field::Channel::Color, fab_paint_field::Channel::SupportBlock, fab_paint_field::Channel::SupportEnforce].into_iter().any(|c| p.program.has(c))).map(|p| fab::api::PaintView {
                    program: p.shown().clone(),
                    colors: p.palette(),
                    from_printer: p.from_printer,
                    key: p.shown_key(),
                }),
                print: p.print.clone(),
                min_mm: b.min,
                max_mm: b.max,
                origin: origin(ev, i).expect("every placed part has its frame"),
                rotate: h.rotate[2],
                turn: h.rotate,
                scale: h.scale,
                dropped: h.drop,
                arriving: false,
                triangles: p.mesh.triangles.len(),
                imported_mesh: c.is_some_and(|c| fab_ai::printable_tool::is_imported_mesh(c, &p.object)),
                blocking: report.is_some_and(|r| r.blocking()),
                issues: report.map(|r| r.issues.iter().map(|i| i.describe(&ev.machine)).collect()).unwrap_or_default(),
                element: None,
                // a part linked to the host's own geometry, with that geometry's frame of the same
                // name: an insert, or a printed part such as a bare board
                linked: linked_place(ev, i).map(|(key, plate_from_frame)| fab::api::LinkedPlace { key: key.to_string(), plate_from_frame }),
                // the field as a distance: a part scaled by axis has its stretch in its shape
                sdf: ev.placed_field(i).map(|f| {
                    let (shape, key, to_printer, _) = f.distance();
                    fab::api::PartSdf { shape: shape.clone(), to_printer: *to_printer, key: *key }
                }),
                body_of: p.body_of.as_ref().map(|id| (id.clone(), object_name(compiled.copied(), id))),
            }
        })
        .collect();
    let bed = &ev.machine.bed;
    PlateInfo {
        parts,
        arrange_gap: ev.arrange_gap,
        settings: ev.plate.settings.clone(),
        nozzles: ev.machine.nozzles.iter().map(|z| z.name.clone()).collect(),
        colors: ev.colors.clone(),
        machine: ev.machine.name.clone(),
        bed_min: bed.min,
        bed_max: bed.max,
        max_height: ev.machine.max_height,
        context_elements: Vec::new(),
        zone_elements: Vec::new(),
        floor_elements: Vec::new(),
        revision,
        preview: false,
        whole: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_model::{eval::Evaluator, FabModel};
    use fab_plate::machine::Machine;

    fn model(body: &str) -> (FabModel, Evaluator, Compiled, Evaluated) {
        let model = FabModel::with_script("parts", "bracket.cad.splash", &format!("{}{body}", fab_model::program::EMPTY));
        let mut evaluator = Evaluator::new(Machine::dual_nozzle_350());
        let (c, ev) = model.evaluate(&mut evaluator, None).expect("test model builds");
        (model, evaluator, c, ev)
    }

    fn loop_field(body: &str) -> String {
        format!("let body = m.sdf({{field: fn(p) {{ let i = 0\n{body}\nreturn length(p) - 0.001mm - 0.0000001mm * float(i) }} bounds: [[-0.002mm, -0.002mm, -0.002mm], [0.002mm, 0.002mm, 0.002mm]] cell: 0.001mm}})\nobject(body)\n")
    }

    /// An insert set to print into its part writes `embed: true` in its script, and back; a printed
    /// part is no insert and is left alone.
    #[test]
    fn an_insert_is_set_to_print_into_its_part_and_back() {
        let body = "let Host = m.box({size: [20mm 10mm 20mm]})\nobject(Host)\ninsert(m.box({size: [5mm 3mm 5mm] at: [0 4mm 0]}), name: \"Nut\", into: \"Host\")\n";
        let (mut model, mut evaluator, mut c, mut ev) = model(body);
        let nut = ev.plate.excluded[0].part.object.clone();
        for embed in [true, false] {
            let changes = edits(&c, &ev, &PartAction::SetEmbed { objects: vec![nut.clone(), nut.clone()], embed }).unwrap();
            assert_eq!(changes.len(), 1, "one edit of the insert's statement");
            let (_, update) = model.apply(&mut evaluator, &format!("embed {embed}"), model.head(), &changes, None).expect("the edit builds");
            (c, ev) = update.expect("a new plate");
            assert_eq!(ev.plate.excluded[0].insert.as_ref().map(|i| i.embed), Some(embed), "the plate's insert");
            assert_eq!(plate_info(Some(&c), &ev, 2).parts.iter().find(|p| p.object == nut).map(|p| (p.insert, p.embed)), Some((true, embed)), "the panels' part");
            assert_eq!(model.state().scripts["bracket.cad.splash"].contains("embed: true"), embed, "the script says so");
        }
        let host = ev.plate.parts[0].object.clone();
        assert!(edits(&c, &ev, &PartAction::SetEmbed { objects: vec![host], embed: true }).unwrap().is_empty(), "a printed part is no insert");
    }

    /// The plate's floor is the heated bed, the build plate and its face, never a part on it:
    /// what the view leaves out while the camera is under the bed plane, and draws again over it.
    #[test]
    fn the_floor_is_the_bed_the_plate_and_its_face() {
        let (_, _, c, ev) = model("let body = m.box({size: [20mm 12mm 8mm]})\nobject(body)\n");
        let mut state = fab::AppState::default();
        state.plate = plate_info(Some(&c), &ev, 1);
        state.scene = crate::plate_scene::build(&ev, "floor");
        state.sync_plate_elements();
        let name = |e: &fab::model::ElementId| state.scene.elements[e.index()].name.clone();
        assert_eq!(state.plate.floor_elements.iter().map(name).collect::<Vec<_>>(), ["fab:bed", "fab:plate", "fab:plate-face"]);
        let part = state.plate.parts[0].element.expect("the part's element");
        for (eye_z, floor) in [(0.3, true), (-0.05, false), (0.001, true)] {
            state.view.camera_mut(state.view.lead_plate()).eye.z = eye_z;
            assert!(state.plate.floor_elements.iter().all(|e| state.is_drawn(*e) == floor), "eye at {eye_z} m");
            assert!(state.is_drawn(part), "the part is drawn from either side");
        }
    }

    #[test]
    fn field_loop_issue_clears_after_a_script_edit() {
        let body = loop_field("while i < int(1025.0 + 0.0 * p.x) { i = i + 1 }");
        let (mut model, mut evaluator, c, ev) = model(&body);
        let mut state = fab::AppState::default();
        state.plate = plate_info(Some(&c), &ev, 1);
        let old_field = state.plate.parts[0].sdf.clone().expect("a field part");
        fab::fab_sdf_engine::makepad_csg_sdf::distance(&old_field.shape.root, makepad_csg_math::Vec3d::new(0.0, 0.0, 0.0));
        assert!(old_field.shape.root.loop_stopped(), "the original script reaches the stop");
        state.sync_plate_elements();
        assert_eq!(state.plate.parts[0].issues.len(), 1, "the stopped field has an issue before editing");
        assert!(state.plate.parts[0].issues[0].contains("limit of 1024 passes"));
        let object = state.plate.parts[0].object.clone();

        let edit = Change::Script {
            file: "bracket.cad.splash".into(),
            edit: fab_model::Edit::Program { source: format!("{}{}", fab_model::program::EMPTY, body.replace("1025.0", "2.0")) },
        };
        let (_, update) = model.apply(&mut evaluator, "shorten loop", model.head(), &[edit], None).expect("the script edit builds");
        let (c, ev) = update.expect("editing the script publishes a new plate");
        state.plate = plate_info(Some(&c), &ev, 2);
        state.sync_plate_elements();
        let part = &state.plate.parts[0];
        assert_eq!(part.object, object, "the edit keeps the same part");
        let field = part.sdf.as_ref().expect("the edited part is still a field");
        assert_ne!(field.key, old_field.key, "the edited script replaces the field");
        fab::fab_sdf_engine::makepad_csg_sdf::distance(&field.shape.root, makepad_csg_math::Vec3d::new(0.0, 0.0, 0.0));
        assert!(!field.shape.root.loop_stopped(), "the shortened loop ends normally");
        assert!(old_field.shape.root.loop_stopped(), "an old reader still holds the stopped field");
        assert!(!state.mark_field_loop_stops(), "the old field cannot mark the edited part again");
        assert!(state.plate.parts[0].issues.is_empty(), "the edited part's issue clears");
    }

    #[test]
    fn field_loop_issue_is_marked_without_view_rays() {
        let (_, _, c, ev) = model(&loop_field("loop { i = i + 1\n if i >= int(1025.0 + 0.0 * p.x) { break } }"));
        let mut state = fab::AppState::default();
        state.plate = plate_info(Some(&c), &ev, 1);
        let field = state.plate.parts[0].sdf.as_ref().expect("a field part");
        // A direct field read stands for a slice or proof. No viewport, sync or view action
        // runs: only the method called on print events can publish this issue.
        fab::fab_sdf_engine::makepad_csg_sdf::distance(&field.shape.root, makepad_csg_math::Vec3d::new(0.0, 0.0, 0.0));
        assert!(field.shape.root.loop_stopped(), "the field reader reaches the stop");
        assert!(state.plate.parts[0].issues.is_empty(), "the field flag alone has not marked the part");
        let marks = state.field_loop_marks;
        assert!(state.mark_field_loop_stops(), "the print-event mark publishes the issue by itself");
        assert_eq!(state.field_loop_marks, marks + 1, "the parts list is invalidated");
        assert_eq!(state.plate.parts[0].issues, ["A loop in this part's field stopped at its limit of 1024 passes, so its surface can be wrong. Make the loop end sooner."]);
        assert!(!state.mark_field_loop_stops(), "another print event adds no duplicate");
        assert_eq!(state.plate.parts[0].issues.len(), 1);
        assert_eq!(state.field_loop_marks, marks + 1, "an unchanged issue does not invalidate the rows again");
    }

    /// The Part panel's default Position is the part's origin. Its footprint fields move it by
    /// the difference from the selected anchor. A turn and a scale keep the origin. A scale by
    /// axis is one undo step, which takes the part back to its uniform scale of 1.
    #[test]
    fn position_is_the_parts_origin() {
        use fab::ui::parts::PositionAnchor;
        // The box sits 15 mm off its model origin along X (the plate's X too).
        let (mut model_, mut evaluator, c, ev) = model("let body = m.box({size: [20mm 12mm 20mm] at: [15mm 6mm 0]})\nobject(body)\n");
        let p = &plate_info(Some(&c), &ev, 1).parts[0];
        let o = p.origin;
        assert!((p.center()[0] - o[0] - 15.0).abs() < 1e-6, "{:?} about {o:?}", p.center());
        let object = p.object.clone();
        let near = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9);
        let origin_after = |action: PartAction| -> [f64; 3] {
            let d = moves(&ev, &action).unwrap()[0].delta;
            std::array::from_fn(|r| d[r][0] * o[0] + d[r][1] * o[1] + d[r][2] * o[2] + d[r][3])
        };
        let to = origin_after(PartAction::Place { object: object.clone(), at: [o[0] + 10.0, o[1] - 4.0], rotate: p.rotate });
        assert!(near(to, [o[0] + 10.0, o[1] - 4.0, o[2]]), "an origin move must apply both XY deltas: {to:?}");
        let to = origin_after(PositionAnchor::Centre.place_field(p, 0, p.center()[0] + 7.0));
        assert!(near(to, [o[0] + 7.0, o[1], o[2]]), "Centre X must move by the footprint delta and preserve Y: {to:?}");
        let to = origin_after(PositionAnchor::Corner.place_field(p, 1, p.min_mm[1] - 4.0));
        assert!(near(to, [o[0], o[1] - 4.0, o[2]]), "Corner Y must move by the min-corner delta and preserve X: {to:?}");
        let turned = origin_after(PartAction::Place { object: object.clone(), at: [o[0], o[1]], rotate: 90.0 });
        assert!(near(turned, o), "a turn keeps the Position: {turned:?}");
        // a scale is its `place` scale itself, by axis too
        let batch = edits(&c, &ev, &PartAction::Scale { object, scale: [2.0, 0.5, 1.0] }).unwrap();
        let (_, update) = model_.apply(&mut evaluator, "scale", model_.head(), &batch, None).unwrap();
        let (c, ev) = update.unwrap();
        let scaled = &plate_info(Some(&c), &ev, 2).parts[0];
        assert!(near(scaled.origin, o), "a scale keeps the Position: {:?}", scaled.origin);
        assert_eq!(scaled.scale, [2.0, 0.5, 1.0]);
        let (_, c, back) = model_.history(&mut evaluator, model_.head(), fab_model::HistoryAction::Undo, None).unwrap();
        let unscaled = &plate_info(Some(&c), &back, 3).parts[0];
        assert_eq!(unscaled.scale, [1.0; 3], "one undo takes the scale back");
        assert!(near(unscaled.origin, o) && (unscaled.center()[0] - o[0] - 15.0).abs() < 1e-6, "{:?} about {:?}", unscaled.center(), unscaled.origin);
    }

    #[test]
    fn eye_batch_is_one_undo_step_and_excluded_parts_keep_scene_and_move_handles() {
        let (mut model, mut evaluator, _, _) = model("let body = m.box({size: [20mm 12mm 8mm]})\nobject(body)\n");
        let (_, update) = model.apply(&mut evaluator, "clone", model.head(), &[Change::Clone { instance: "bracket".into(), at: Some([240.0, 160.0]) }], None).unwrap();
        let (c, ev) = update.unwrap();
        let plate = plate_info(Some(&c), &ev, 1);
        let action = PartAction::SetPrint { objects: plate.parts.iter().map(|p| p.object.clone()).collect(), print: false };
        let batch = edits(&c, &ev, &action).unwrap();
        assert_eq!(batch.len(), 2);
        let (_, update) = model.apply(&mut evaluator, "eyes", model.head(), &batch, None).unwrap();
        let (c, ev) = update.unwrap();
        let plate = plate_info(Some(&c), &ev, 2);
        assert_eq!(plate.parts.len(), 2);
        assert!(plate.parts.iter().all(|p| !p.printed));
        assert_eq!(plate.parts.iter().map(|p| p.link).collect::<Vec<_>>(), [0, 1]);
        let scene = crate::plate_scene::build(&ev, "eyes");
        for part in &plate.parts {
            assert!(scene.elements.iter().any(|e| e.name == part.object));
            let at = [part.center()[0] + 10.0, part.center()[1]];
            let moves = moves(&ev, &PartAction::Place { object: part.object.clone(), at, rotate: 90.0 }).unwrap();
            assert_eq!(moves.len(), 1);
            assert_eq!(moves[0].object, part.object);
        }
        let (_, _, back) = model.history(&mut evaluator, model.head(), fab_model::HistoryAction::Undo, None).unwrap();
        assert_eq!(back.plate.parts.len(), 2);
        assert!(back.plate.excluded.is_empty());
    }

    #[test]
    fn duplicates_map_to_clone_and_copy_next_to_the_source() {
        let (mut model, mut evaluator, c, ev) = model("let body = m.box({size: [20mm 12mm 20mm]})\nobject(body)\n");
        let object = ev.plate.parts[0].object.clone();
        let bounds = ev.plate.parts[0].bounds();
        let at = Some([bounds.max[0] + 10.0 + bounds.size()[0] * 0.5, bounds.center()[1]]);
        let linked = edits(&c, &ev, &PartAction::DuplicateLinked { object: object.clone() }).unwrap();
        let copy = edits(&c, &ev, &PartAction::DuplicateCopy { object }).unwrap();
        assert_eq!(linked, vec![Change::Clone { instance: "bracket".into(), at }]);
        assert_eq!(copy, vec![Change::Copy { instance: "bracket".into(), at }]);
        let (_, update) = model.apply(&mut evaluator, "linked", model.head(), &linked, None).unwrap();
        let (c, ev) = update.unwrap();
        let plate = plate_info(Some(&c), &ev, 2);
        assert_eq!(plate.parts.len(), 2);
        assert!(plate.parts.iter().all(|p| p.script == "bracket" && p.file == "bracket.cad.splash"));
        let clone = plate.parts.iter().find(|p| p.link > 0).unwrap();
        assert_eq!(delete_edits(&c, std::slice::from_ref(&clone.object), false).unwrap(), vec![Change::RemoveInstance { instance: clone.object.split('/').next().unwrap().into() }]);
        assert_eq!(delete_edits(&c, std::slice::from_ref(&clone.object), true).unwrap(), vec![Change::RemoveScript { instance: "bracket".into() }]);
        let copy = edits(&c, &ev, &PartAction::DuplicateCopy { object: clone.object.clone() }).unwrap();
        let (_, update) = model.apply(&mut evaluator, "copy", model.head(), &copy, None).unwrap();
        let (c, ev) = update.unwrap();
        let plate = plate_info(Some(&c), &ev, 3);
        assert_eq!(plate.parts.len(), 3);
        let independent = plate.parts.iter().find(|p| p.script != "bracket").unwrap();
        assert_eq!(independent.link, 0);
        assert_ne!(independent.file, "bracket.cad.splash");
    }

    /// Deleting a part takes its print settings box and its own settings off the plate with it,
    /// and one undo brings all of them back.
    #[test]
    fn deleting_a_part_takes_its_boxes_and_settings_with_it() {
        let (mut model, mut evaluator, _, _) = model("let body = m.box({size: [20mm 20mm 20mm]})\nobject(body)\nlet settings1 = m.box({size: [4mm 4mm 4mm]})\nm.settings(settings1, {sparse_infill_density: \"100%\"})\n");
        let own = [Change::Place { instance: "bracket".into(), object: "body".into(), arg: PlaceArg::Settings(vec![("wall_loops".into(), "4".into())]) }];
        let (_, update) = model.apply(&mut evaluator, "own", model.head(), &own, None).unwrap();
        let (c, ev) = update.unwrap();
        let with_both = |ev: &Evaluated| {
            let [part] = &ev.plate.parts[..] else { panic!("one part: {}", ev.plate.parts.len()) };
            assert_eq!(part.paint.as_ref().map(|p| p.settings.len()), Some(1), "its box");
            assert!(part.print.iter().any(|(k, v)| k == "wall_loops" && v == "4"), "its own setting: {:?}", part.print);
        };
        with_both(&ev);
        let batch = delete_edits(&c, &["bracket/body".into()], false).unwrap();
        assert_eq!(batch, [Change::RemoveInstance { instance: "bracket".into() }]);
        let (_, update) = model.apply(&mut evaluator, "delete", model.head(), &batch, None).unwrap();
        let (_, ev) = update.unwrap();
        assert!(ev.plate.parts.is_empty(), "nothing of it stays on the plate");
        let (_, _, back) = model.history(&mut evaluator, model.head(), fab_model::HistoryAction::Undo, None).unwrap();
        with_both(&back);
    }

    #[test]
    fn deleting_one_object_of_an_assembly_keeps_its_siblings() {
        let (_, _, c, _) = model("let body = m.box({size: [20mm 12mm 20mm]})\nlet lid = m.box({size: [20mm 2mm 20mm]})\nobject(body)\nobject(lid)\n");
        assert_eq!(delete_edits(&c, &["bracket/body".into()], false).unwrap(), vec![Change::Place { instance: "bracket".into(), object: "body".into(), arg: PlaceArg::Show(false) }]);
        assert_eq!(delete_edits(&c, &["bracket/lid".into()], true).unwrap(), vec![Change::RemoveScript { instance: "bracket".into() }]);
    }

    #[test]
    fn delete_assembly_body_resolves_emitted_name_and_deduplicates_bodies() {
        let (mut model, mut evaluator, c, ev) = model("let links = for i in 0..2 { m.box({size: [12mm 6mm 5mm] at: [i * 12mm + 6mm 3mm 0]}) }\nlet assembly = m.chain(links)\nobject(links[0], name: \"lattice\")\n");
        let plate = plate_info(Some(&c), &ev, 1);
        assert_eq!(plate.parts.len(), 2);
        let object = plate.parts[1].object.clone();
        assert_ne!(object, "bracket/lattice");
        let expected = vec![Change::RemoveInstance { instance: "bracket".into() }];
        assert_eq!(delete_edits(&c, &[object], false).unwrap(), expected);
        let batch = delete_edits(&c, &plate.parts.iter().map(|p| p.object.clone()).collect::<Vec<_>>(), false).unwrap();
        assert_eq!(batch, expected);
        let (_, update) = model.apply(&mut evaluator, "delete assembly", model.head(), &batch, None).unwrap();
        assert_eq!(update.unwrap().1.plate.placed_parts().count(), 0);
        let (_, _, back) = model.history(&mut evaluator, model.head(), fab_model::HistoryAction::Undo, None).unwrap();
        assert_eq!(back.plate.placed_parts().count(), 2);
    }

    #[test]
    fn delete_multi_object_removes_list_row_and_last_object_removes_script() {
        let (mut model, mut evaluator, c, _) = model("let body = m.box({size: [20mm 12mm 8mm]})\nlet lid = m.box({size: [20mm 12mm 2mm]})\nobject(body)\nobject(lid)\n");
        let original = model.state().clone();
        let batch = delete_edits(&c, &["bracket/body".into()], false).unwrap();
        let (_, update) = model.apply(&mut evaluator, "delete body", model.head(), &batch, None).unwrap();
        let (c, ev) = update.unwrap();
        let plate = plate_info(Some(&c), &ev, 2);
        assert_eq!(plate.parts.iter().map(|p| p.object.as_str()).collect::<Vec<_>>(), ["bracket/lid"]);
        assert!(model.source("bracket.cad.splash").is_some());
        let batch = delete_edits(&c, &["bracket/lid".into()], false).unwrap();
        assert_eq!(batch, [Change::RemoveInstance { instance: "bracket".into() }]);
        let (_, update) = model.apply(&mut evaluator, "delete last", model.head(), &batch, None).unwrap();
        let (c, ev) = update.unwrap();
        assert!(plate_info(Some(&c), &ev, 3).parts.is_empty());
        assert!(model.project().places.is_empty());
        assert!(model.source("bracket.cad.splash").is_none());
        model.history(&mut evaluator, model.head(), fab_model::HistoryAction::Undo, None).unwrap();
        model.history(&mut evaluator, model.head(), fab_model::HistoryAction::Undo, None).unwrap();
        assert_eq!(model.state(), &original);
    }

    #[test]
    fn delete_clone_keeps_base_and_multi_selection_undo_restores_all_links() {
        let (mut model, mut evaluator, _, _) = model("let body = m.box({size: [20mm 12mm 8mm]})\nobject(body)\n");
        model.apply(&mut evaluator, "clone one", model.head(), &[Change::Clone { instance: "bracket".into(), at: None }], None).unwrap();
        let (_, update) = model.apply(&mut evaluator, "clone two", model.head(), &[Change::Clone { instance: "bracket".into(), at: None }], None).unwrap();
        let (c, _) = update.unwrap();
        let batch = delete_edits(&c, &["bracket~1/body".into()], false).unwrap();
        assert_eq!(batch, [Change::RemoveInstance { instance: "bracket~1".into() }]);
        let (_, update) = model.apply(&mut evaluator, "delete clone", model.head(), &batch, None).unwrap();
        let (c, _) = update.unwrap();
        assert!(c.instance("bracket").is_some());
        assert!(c.instance("bracket~2").is_some());
        assert!(model.source("bracket.cad.splash").is_some());
        let before = model.state().clone();
        let steps = model.undo_labels().count();
        let batch = delete_edits(&c, &["bracket/body".into(), "bracket~2/body".into(), "bracket/body".into()], false).unwrap();
        assert_eq!(batch, [Change::RemoveInstance { instance: "bracket~2".into() }, Change::RemoveInstance { instance: "bracket".into() }]);
        model.apply(&mut evaluator, "delete selection", model.head(), &batch, None).unwrap();
        assert_eq!(model.undo_labels().count(), steps + 1);
        assert!(model.source("bracket.cad.splash").is_none());
        model.history(&mut evaluator, model.head(), fab_model::HistoryAction::Undo, None).unwrap();
        assert_eq!(model.state(), &before);
    }

    #[test]
    fn deleting_one_of_two_instances_using_the_same_file_keeps_source() {
        let (mut model, mut evaluator, _, _) = model("let body = m.box({size: [20mm 12mm 8mm]})\nobject(body)\n");
        let (_, update) = model.apply(&mut evaluator, "another reference", model.head(), &[Change::AddInstance { file: "bracket.cad.splash".into(), source: None, name: Some("other".into()), at: None }], None).unwrap();
        let (c, _) = update.unwrap();
        let batch = delete_edits(&c, &["bracket/body".into()], false).unwrap();
        model.apply(&mut evaluator, "delete first reference", model.head(), &batch, None).unwrap();
        assert!(model.source("bracket.cad.splash").is_some());
        assert_eq!(model.project().scripts[0].name, "other");
    }

    #[test]
    fn unresolved_selection_is_an_error_instead_of_a_partial_delete() {
        let (_, _, c, _) = model("let body = m.box({size: [20mm 12mm 8mm]})\nobject(body)\n");
        assert!(delete_edits(&c, &["bracket/body".into(), "missing/body".into()], false).is_err());
    }

    #[test]
    fn deleting_link_geometry_resolves_its_emitted_name() {
        let (mut model, mut evaluator, c, ev) = model("let chain = m.links([[0 0 0] [20mm 0 0]], {size: 6mm bar: 0.8mm})\nobject(chain, name: \"lattice\")\n");
        let plate = plate_info(Some(&c), &ev, 1);
        assert_eq!(plate.parts.len(), 1);
        assert_eq!(plate.parts[0].name, "lattice");
        assert!(plate.parts[0].sdf.is_some(), "Links use the editable GPU field path");
        let batch = delete_edits(&c, std::slice::from_ref(&plate.parts[0].object), false).unwrap();
        assert_eq!(batch, [Change::RemoveInstance { instance: "bracket".into() }]);
        let (_, update) = model.apply(&mut evaluator, "delete links", model.head(), &batch, None).unwrap();
        assert_eq!(update.unwrap().1.plate.placed_parts().count(), 0);
    }

    #[test]
    fn large_delete_selection_is_atomic_instead_of_hitting_the_general_edit_limit() {
        let (model, _, _, _) = model("let body = m.box({size: [2mm 2mm 2mm]})\nobject(body)\n");
        let mut state = model.state().clone();
        let source = state.project.scripts[0].clone();
        state.project.scripts = (0..300).map(|i| fab_model::project::ScriptRef { name: format!("part{i}"), ..source.clone() }).collect();
        let model = FabModel::open("large selection", state);
        let c = model.compile().unwrap();
        let targets = c.instances.iter().map(|i| format!("{}/body", i.name)).collect::<Vec<_>>();
        let batch = delete_edits(&c, &targets, false).unwrap();
        assert_eq!(batch.len(), 300);
        let (state, c) = model.edited(&batch).unwrap();
        assert!(c.instances.is_empty());
        assert!(state.project.places.is_empty());
        assert!(state.scripts.is_empty());
    }

}
