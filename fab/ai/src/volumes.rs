//! `fab.settings_volume`: print settings volumes, what the Slicer tab's "+ Settings volume" calls
//! and what the chat calls, one path. A call names what it covers, one target or several ("arms,
//! legs and neck"), and writes one volume each ([`fab_model::volumes`]) as one undo step. A word
//! that matches nothing is never guessed at: nothing is written and the answer lists the features
//! there are, with their kinds and boxes, and how to fit a shape round what the word meant. A model
//! with no names at all is found by looking (`fab.render`, `fab.pick`) and takes a shape by numbers:
//! a capsule from A to B, a cylinder, a sphere, a box, at any angle, in the frame `fab.pick`'s
//! `point` is in. `fab.fit` is the read-only step between the two: from points on the part to its
//! axis, ends and radius, measured from the slice's layers.

use crate::{out::{arr, flag, int, mm3, n, obj, s, S}, str_arg, supports::ColoredMesh, Reply, Session};
use fab_model::eval::Evaluated;
use fab_model::params::xf_point;
use fab_model::program::{area_solids, part_id, settings_solids, split_id, Compiled, Instance};
use fab_model::volumes::{batch_statements, box_code, capsule_code, cylinder_code, fit_points, fitted, follows_code, named_features, over_part, sphere_code, volume_stem, Fit, ModelWriter, NewVolume, VolumeShape, DEFAULT_GROW, DEFAULT_SETTINGS, FIT_MARGIN};
use fab_paint_field::Region;
use fab_printability::{fit_limb, printed_inside, solid_samples, LimbEnd, LimbFit, Printed};
use makepad_fab_slice::live::PlatePaths;
use makepad_fab_slicer::geom::ExPolygon;
use fab_model::Change;
use makepad_model::json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

pub const TOOL: &str = "settings_volume";
pub const FIT_TOOL: &str = "fit";

/// What one target of a call covers.
enum Target {
    /// A feature by the name a script gives it, with a shape and a grow of its own if it says.
    Feature { id: String, shape: Option<VolumeShape>, grow: Option<f64> },
    /// What no feature covers: a box in the model frame (mm) the shape is fitted round, named.
    Around { name: String, object: Option<String>, min: [f64; 3], max: [f64; 3], shape: Option<VolumeShape> },
    /// A shape by its own numbers in the model frame (mm): its code.
    Placed { name: String, object: Option<String>, code: String },
    /// The whole of a plate part.
    Part { object: String },
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::F64(x) if x.is_finite() => Some(*x),
        Value::Int(x) => Some(*x as f64),
        _ => None,
    }
}

fn shape_arg(args: &Value) -> Result<Option<VolumeShape>, String> {
    match str_arg(args, "shape") {
        None => Ok(None),
        Some(name) => VolumeShape::parse(name).map(Some).ok_or_else(|| format!("'shape' is follows, box, sphere, cylinder or capsule, not '{name}'")),
    }
}

fn length(v: &Value, name: &str, key: &str) -> Result<f64, String> {
    v.get(key).and_then(number).ok_or_else(|| format!("'{name}': '{key}' is a number in mm"))
}

/// The code of a target given by numbers (`a`, `b`, `at`, `size`, `radius`), if it is one.
fn placed_code(v: &Value, name: &str) -> Option<Result<String, String>> {
    let named = |why: String| format!("'{name}': {why}");
    let shape = str_arg(v, "shape");
    let code = |v: &Value| -> Result<String, String> {
        match shape {
            Some("capsule") => capsule_code(corner(v.get("a"), "a")?, corner(v.get("b"), "b")?, length(v, name, "radius")?),
            Some("cylinder") => cylinder_code(corner(v.get("a"), "a")?, corner(v.get("b"), "b")?, length(v, name, "radius")?),
            Some("sphere") => sphere_code(corner(v.get("at"), "at")?, length(v, name, "radius")?),
            Some("box") => box_code(corner(v.get("at"), "at")?, corner(v.get("size"), "size")?, v.get("up").map(|up| corner(Some(up), "up")).transpose()?),
            other => Err(format!("a shape by numbers is capsule or cylinder {{a b radius}}, sphere {{at radius}} or box {{at size up}}, not {other:?}")),
        }
    };
    ["a", "at", "size"].iter().any(|k| v.get(k).is_some()).then(|| code(v).map_err(named))
}

fn grow_arg(args: &Value) -> Result<Option<f64>, String> {
    match args.get("grow") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => number(v).map(Some).ok_or_else(|| "'grow' is a number in mm".to_string()),
    }
}

fn corner(v: Option<&Value>, key: &str) -> Result<[f64; 3], String> {
    let a = v.and_then(Value::as_arr).filter(|a| a.len() == 3).ok_or_else(|| format!("'{key}' is a corner [x y z] in the model frame, mm"))?;
    let mut out = [0.0; 3];
    for (to, v) in out.iter_mut().zip(a) {
        *to = number(v).ok_or_else(|| format!("'{key}' is a corner [x y z] in the model frame, mm"))?;
    }
    Ok(out)
}

/// The call's targets: `targets` (names, or `{feature, shape, grow}` / `{name, min, max, shape}`),
/// else its one `feature`, else the whole of `object`.
fn targets(args: &Value) -> Result<Vec<Target>, String> {
    let one = |v: &Value| -> Result<Target, String> {
        if let Some(id) = v.as_str() {
            return Ok(Target::Feature { id: id.to_string(), shape: None, grow: None });
        }
        if let Some(id) = str_arg(v, "feature") {
            return Ok(Target::Feature { id: id.to_string(), shape: shape_arg(v)?, grow: grow_arg(v)? });
        }
        if let Some(name) = str_arg(v, "name") {
            let object = str_arg(v, "object").map(str::to_string);
            if let Some(code) = placed_code(v, name) {
                return Ok(Target::Placed { name: name.to_string(), object, code: code? });
            }
            return Ok(Target::Around { name: name.to_string(), object, min: corner(v.get("min"), "min")?, max: corner(v.get("max"), "max")?, shape: shape_arg(v)? });
        }
        Err("a target is a feature's name, {feature, shape, grow}, {name, min, max, shape} to fit a shape round a box, or {name, shape, ..} by numbers: capsule or cylinder {a b radius}, sphere {at radius}, box {at size up}".into())
    };
    if let Some(list) = args.get("targets") {
        let list = list.as_arr().filter(|l| !l.is_empty()).ok_or("'targets' is a list of what to cover, at least one")?;
        return list.iter().map(one).collect();
    }
    if let Some(id) = str_arg(args, "feature") {
        return Ok(vec![Target::Feature { id: id.to_string(), shape: None, grow: None }]);
    }
    match str_arg(args, "object") {
        Some(object) => Ok(vec![Target::Part { object: object.to_string() }]),
        None => Err("say what it covers: 'targets' (feature names), 'feature', or 'object' for a whole part".into()),
    }
}

/// `settings` as `(key, text)`; none given: a solid infill.
fn settings_arg(args: &Value) -> Result<Vec<(String, String)>, String> {
    let Some(v) = args.get("settings") else {
        return Ok(DEFAULT_SETTINGS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect());
    };
    let Value::Obj(fields) = v else { return Err("'settings' is {key: value}: {sparse_infill_density: \"100%\" wall_loops: 4}".into()) };
    if fields.is_empty() {
        return Err("'settings' sets nothing: {sparse_infill_density: \"100%\" wall_loops: 4}".into());
    }
    fields.iter().map(|(k, v)| {
        let text = match v {
            Value::Str(t) => t.clone(),
            Value::Int(x) => x.to_string(),
            Value::F64(x) => x.to_string(),
            _ => return Err(format!("the value of '{k}' is text or a number")),
        };
        Ok((k.clone(), text))
    }).collect()
}

/// What `settings` set, as a person says it: `infill 100%, walls 4`.
pub fn settings_words(settings: &[(String, String)]) -> String {
    settings.iter().map(|(k, v)| match k.as_str() {
        "sparse_infill_density" => format!("infill {v}"),
        "wall_loops" => format!("walls {v}"),
        "sparse_infill_pattern" => format!("pattern {v}"),
        _ => format!("{k} {v}"),
    }).collect::<Vec<_>>().join(", ")
}

/// A box's sides and middle, as a word is matched against it.
fn box_words(b: &([f64; 3], [f64; 3])) -> String {
    let d: [f64; 3] = std::array::from_fn(|k| b.1[k] - b.0[k]);
    let c: [f64; 3] = std::array::from_fn(|k| 0.5 * (b.0[k] + b.1[k]));
    format!("{:.1} x {:.1} x {:.1} mm at [{:.1} {:.1} {:.1}]", d[0], d[1], d[2], c[0], c[1], c[2])
}

/// A solid's box in the last build.
pub(crate) fn solid_box(ev: &Evaluated, name: &str) -> Option<([f64; 3], [f64; 3])> {
    ev.objects.iter().find(|o| o.name == name).and_then(|o| o.bounds)
}

/// A volume's or a target's name as a script can start one: what `said` will read.
fn plan_named(inst: &Instance, name: &str, code: String) -> Planned {
    Planned { instance: inst.name.clone(), file: inst.script.file.clone(), volume: NewVolume { stem: volume_stem(name), numbered: false, solid: code }, said: name.to_string() }
}

/// One line per feature of an instance: its name, what makes it, its box.
fn listing(inst: &Instance, ev: &Evaluated) -> String {
    let p = &inst.script.program;
    let names: Vec<String> = named_features(p, &inst.script.objects).into_iter().map(|i| {
        let solid = &p.solids[i];
        match solid_box(ev, &part_id(&inst.name, &solid.id)) {
            Some(b) => format!("{} ({}, {})", solid.id, solid.proto.to_lowercase(), box_words(&b)),
            None => format!("{} ({})", solid.id, solid.proto.to_lowercase()),
        }
    }).collect();
    if names.is_empty() {
        format!("{} has no named features: it is built as one piece", inst.name)
    } else {
        format!("the features of {}: {}", inst.name, names.join("; "))
    }
}

/// The features of a build as `fab.tree` lists them, to match a person's words against: names, kinds
/// and boxes in the model frame; its print settings volumes among them, of kind `settings volume`.
pub fn features_value(c: &Compiled, ev: &Evaluated) -> S {
    arr(c.instances.iter().flat_map(|inst| {
        let p = &inst.script.program;
        let settings = settings_solids(p);
        let volumes = settings.iter().copied().filter(|i| p.solids[*i].parent.is_none()).collect::<Vec<_>>();
        named_features(p, &inst.script.objects).into_iter().chain(volumes).map(move |i| {
            let solid = &p.solids[i];
            let kind = if settings.contains(&i) { "settings volume".to_string() } else { solid.proto.to_lowercase() };
            let mut fields = vec![("id", s(part_id(&inst.name, &solid.id))), ("name", s(&solid.id)), ("kind", s(kind))];
            if let Some(b) = solid_box(ev, &part_id(&inst.name, &solid.id)) {
                fields.push(("min", mm3(b.0)));
                fields.push(("max", mm3(b.1)));
            }
            obj(fields)
        })
    }))
}

/// The print settings volumes of a build, as `fab.inspect` lists them: name, shape, what it follows,
/// how far it grows, its box (model mm), its values.
pub fn volumes_value(c: &Compiled, ev: &Evaluated) -> S {
    arr(c.instances.iter().flat_map(|inst| {
        let p = &inst.script.program;
        settings_solids(p).into_iter().filter(|i| p.solids[*i].parent.is_none()).map(move |i| {
            let solid = &p.solids[i];
            let id = part_id(&inst.name, &solid.id);
            let (target, grow) = fab_model::volumes::followed(p, i).map_or((None, None), |(t, g)| (Some(t), Some(g)));
            let mut fields = vec![("id", s(&id)), ("name", s(&solid.id)), ("shape", s(if target.is_some() { "follows".to_string() } else { solid.proto.to_lowercase() }))];
            if let Some(target) = target {
                fields.push(("target", s(target)));
            }
            if let Some(grow) = grow {
                fields.push(("grow", n(grow)));
            }
            if let Some(b) = solid_box(ev, &id) {
                fields.push(("min", mm3(b.0)));
                fields.push(("max", mm3(b.1)));
            }
            fields.push(("values", obj(fab_model::paint::volume_settings(p, i).iter().map(|(k, v)| (k.as_str(), s(v))).collect())));
            obj(fields)
        })
    }))
}

/// The instance a feature's name is in: `instance/solid`, or a bare name in `object`'s script or
/// the one script that has it.
fn find_feature<'c>(c: &'c Compiled, ev: &Evaluated, object: Option<&str>, id: &str) -> Result<(&'c Instance, usize), String> {
    let (instance, solid) = match split_id(id) {
        Some((i, solid)) => (Some(i), solid),
        None => (object.and_then(split_id).map(|(i, _)| i), id),
    };
    let inst = match instance {
        Some(name) => c.instance(name).ok_or_else(|| format!("no instance '{name}' on the plate"))?,
        None => {
            let has: Vec<&Instance> = c.instances.iter().filter(|i| i.script.program.solid(solid).is_some()).collect();
            match has.as_slice() {
                [one] => *one,
                [] => {
                    let all: Vec<String> = c.instances.iter().map(|i| listing(i, ev)).collect();
                    return Err(format!("no feature is called '{solid}'; {}", all.join("; ")));
                }
                many => return Err(format!("'{solid}' is a solid of several scripts: name one ({})", many.iter().map(|i| part_id(&i.name, solid)).collect::<Vec<_>>().join(", "))),
            }
        }
    };
    let p = &inst.script.program;
    let Some(i) = p.solids.iter().position(|s| s.id == solid) else { return Err(format!("no feature is called '{solid}'; {}", listing(inst, ev))) };
    if inst.script.objects.iter().any(|o| o.bodies.contains(&i)) {
        return Err(format!("'{solid}' is the whole part: give it settings of its own with fab.place, or name a feature; {}", listing(inst, ev)));
    }
    if area_solids(p).contains(&i) {
        return Err(format!("'{solid}' is a marked area itself, no feature to cover"));
    }
    if !named_features(p, &inst.script.objects).contains(&i) {
        return Err(format!("'{solid}' has no name a statement can write, or is in no printed object; {}", listing(inst, ev)));
    }
    Ok((inst, i))
}

/// The index of the placed part (printed, then excluded) plate part id `object` is.
fn placed_index(ev: &Evaluated, object: &str) -> Option<usize> {
    (0..ev.plate.parts.len() + ev.plate.excluded.len()).find(|&k| ev.placed_part(k).object == object)
}

/// The placed part a solid of `inst` is built into.
fn built_into(inst: &Instance, ev: &Evaluated, i: usize) -> Option<usize> {
    let p = &inst.script.program;
    let mut at = Some(i);
    while let Some(j) = at {
        if let Some(k) = placed_index(ev, &part_id(&inst.name, &p.solids[j].id)) {
            return Some(k);
        }
        at = p.solids[j].parent;
    }
    None
}

/// The placed part a solid of `inst` is built into: its model-to-plate frame.
fn frame_of(inst: &Instance, ev: &Evaluated, i: usize) -> Option<[[f64; 4]; 3]> {
    built_into(inst, ev, i).and_then(|k| ev.placed_frame(k).copied())
}

/// The placed part `word` names as a whole, a word from a person or `fab.tree`: its id, as
/// `instance/object` or by its object's name, when one instance of the plate has it.
fn whole_part(c: &Compiled, ev: &Evaluated, word: &str) -> Option<usize> {
    if let Some(k) = placed_index(ev, word) {
        return Some(k);
    }
    let mut found = c.instances.iter().filter_map(|inst| placed_index(ev, &part_id(&inst.name, word)));
    found.next().filter(|_| found.next().is_none())
}

/// The box round a feature or a whole part, on the plate.
pub struct FeatureBox {
    /// The part it is on: what its box lives in.
    pub object: String,
    /// The word's feature, as it is named.
    pub name: String,
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl FeatureBox {
    pub fn center(&self) -> [f64; 3] {
        std::array::from_fn(|k| 0.5 * (self.min[k] + self.max[k]))
    }

    pub fn size(&self) -> [f64; 3] {
        std::array::from_fn(|k| self.max[k] - self.min[k])
    }
}

/// The frame a solid's parents put it in: identity when nothing round it moves it.
fn parents_frame(inst: &Instance, ev: &Evaluated, i: usize) -> [[f64; 4]; 3] {
    const IDENTITY: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
    let p = &inst.script.program;
    let Some(parent) = p.solids[i].parent else { return IDENTITY };
    let name = part_id(&inst.name, &p.solids[parent].id);
    ev.objects.iter().find(|o| o.name == name).map_or(IDENTITY, |o| [o.world[0], o.world[1], o.world[2]])
}

/// One volume a call writes: where, its code, and the name its answer says.
struct Planned {
    instance: String,
    file: String,
    volume: NewVolume,
    said: String,
}

impl Session {
    /// What `word` names on the plate, a box [`fab_model::volumes::FIT_MARGIN`] clear round it (plate
    /// mm). A part that is one piece of the plate is its own feature here: a box can cut it whole.
    pub(crate) fn feature_box(&mut self, c: &Compiled, ev: &Evaluated, word: &str, object: Option<&str>) -> Result<FeatureBox, String> {
        if let Some(k) = whole_part(c, ev, word) {
            let part = ev.placed_part(k);
            let b = part.bounds();
            let points: Vec<[f64; 3]> = (0..8).map(|c| std::array::from_fn(|k| if c >> k & 1 == 0 { b.min[k] } else { b.max[k] })).collect();
            let Some(Fit::Box { min, max }) = fit_points(VolumeShape::Box, &points, false) else { return Err(format!("'{word}' has no box to fit round")) };
            let name = split_id(&part.object).map_or(part.object.as_str(), |(_, solid)| solid).to_string();
            return Ok(FeatureBox { object: part.object.clone(), name, min, max });
        }
        let (inst, i) = find_feature(c, ev, object, word)?;
        let solid_id = inst.script.program.solids[i].id.clone();
        let k = built_into(inst, ev, i).ok_or_else(|| format!("'{solid_id}' is in no part on the plate"))?;
        let frame = *ev.placed_frame(k).ok_or_else(|| format!("'{solid_id}' is in no part on the plate"))?;
        let mesh = self.eval.object_mesh(&part_id(&inst.name, &solid_id))?;
        let moved = parents_frame(inst, ev, i);
        let points: Vec<[f64; 3]> = mesh.vertices.iter().map(|v| xf_point(&frame, xf_point(&moved, [v.x, v.y, v.z]))).collect();
        let Some(Fit::Box { min, max }) = fit_points(VolumeShape::Box, &points, false) else { return Err(format!("'{solid_id}' builds no solid to fit a box round")) };
        Ok(FeatureBox { object: ev.placed_part(k).object.clone(), name: solid_id, min, max })
    }

    fn plan_target(&mut self, c: &Compiled, ev: &Evaluated, target: &Target, shape: Option<VolumeShape>, grow: Option<f64>, object: Option<&str>) -> Result<Planned, String> {
        match target {
            Target::Feature { id, shape: own_shape, grow: own_grow } => {
                let (inst, i) = find_feature(c, ev, object, id)?;
                let solid_id = inst.script.program.solids[i].id.clone();
                let shape = own_shape.or(shape).unwrap_or(VolumeShape::Follows);
                let stem = volume_stem(&solid_id);
                let code = if shape == VolumeShape::Follows {
                    let moved = parents_frame(inst, ev, i);
                    let identity = (0..3).all(|r| (0..4).all(|k| (moved[r][k] - if r == k { 1.0 } else { 0.0 }).abs() < 1e-9));
                    if !identity {
                        return Err(format!("what '{solid_id}' is part of moves it, so a volume cannot follow it there: use shape box, sphere or cylinder"));
                    }
                    follows_code(&solid_id, own_grow.or(grow).unwrap_or(DEFAULT_GROW))
                } else {
                    let frame = frame_of(inst, ev, i).ok_or_else(|| format!("'{solid_id}' is in no part on the plate"))?;
                    let mesh = self.eval.object_mesh(&part_id(&inst.name, &solid_id))?;
                    let moved = parents_frame(inst, ev, i);
                    let points: Vec<[f64; 3]> = mesh.vertices.iter().map(|v| xf_point(&frame, xf_point(&moved, [v.x, v.y, v.z]))).collect();
                    fitted(shape, &ModelWriter::new(&frame), &points, false).ok_or_else(|| format!("'{solid_id}' builds no solid to fit a shape round"))?
                };
                Ok(Planned { instance: inst.name.clone(), file: inst.script.file.clone(), volume: NewVolume { stem, numbered: false, solid: code }, said: solid_id })
            }
            Target::Around { name, object: own, min, max, shape: own_shape } => {
                let (inst, frame) = part_frame(c, ev, own.as_deref().or(object)).ok_or_else(|| format!("'{name}': say which part it is on with 'object'"))?;
                let shape = own_shape.or(shape).unwrap_or(VolumeShape::Box);
                if shape == VolumeShape::Follows {
                    return Err(format!("'{name}' is no feature to follow: use shape box, sphere, cylinder or capsule"));
                }
                let points: Vec<[f64; 3]> = (0..8).map(|c| xf_point(&frame, std::array::from_fn(|k| if c >> k & 1 == 0 { min[k] } else { max[k] }))).collect();
                let code = fitted(shape, &ModelWriter::new(&frame), &points, false).ok_or_else(|| format!("'{name}' has no box to fit round"))?;
                Ok(plan_named(inst, name, code))
            }
            Target::Placed { name, object: own, code } => {
                let (inst, _) = part_frame(c, ev, own.as_deref().or(object)).ok_or_else(|| format!("'{name}': say which part it is on with 'object'"))?;
                Ok(plan_named(inst, name, code.clone()))
            }
            Target::Part { object: part } => {
                let (inst, emitted) = c.object(part).ok_or_else(|| format!("no part '{part}' on the plate"))?;
                let k = placed_index(ev, part).ok_or_else(|| format!("'{part}' has no place on the plate yet"))?;
                let frame = *ev.placed_frame(k).ok_or_else(|| format!("'{part}' has no place on the plate yet"))?;
                let bounds = ev.placed_part(k).bounds();
                let w = ModelWriter::new(&frame);
                let shape = shape.unwrap_or(VolumeShape::Box);
                let code = match shape {
                    VolumeShape::Box => over_part(&w, bounds.min, bounds.max),
                    VolumeShape::Follows => return Err(format!("'{part}' is the whole part: give it settings of its own with fab.place, or name a feature")),
                    other => {
                        let points: Vec<[f64; 3]> = (0..8).map(|c| std::array::from_fn(|k| if c >> k & 1 == 0 { bounds.min[k] } else { bounds.max[k] })).collect();
                        fitted(other, &w, &points, true).ok_or_else(|| format!("'{part}' has no box to fit round"))?
                    }
                };
                Ok(Planned { instance: inst.name.clone(), file: inst.script.file.clone(), volume: NewVolume { stem: shape.stem().into(), numbered: true, solid: code }, said: emitted.name.clone() })
            }
        }
    }

    /// The call's changes, one transaction, or (nothing written) everything wrong with it.
    pub(crate) fn volume_plan(&mut self, args: &Value) -> Result<(Vec<Change>, Vec<String>, String), String> {
        let (Some(c), Some(ev)) = (self.compiled.clone(), self.last.clone()) else { return Err("nothing is built yet".into()) };
        let settings = settings_arg(args)?;
        let (shape, grow) = (shape_arg(args)?, grow_arg(args)?);
        let object = str_arg(args, "object");
        let wanted = targets(args)?;
        let mut planned = Vec::new();
        let mut problems = Vec::new();
        for target in &wanted {
            match self.plan_target(&c, &ev, target, shape, grow, object) {
                Ok(p) => planned.push(p),
                Err(why) => problems.push(why),
            }
        }
        if !problems.is_empty() {
            return Err(format!("{}. Nothing was changed. Name features that exist; or find it by looking (fab.render, fab.pick, fab.fit) and give {{name, shape, a, b, radius}} (model mm), or {{name, min, max}} for a shape fitted round a box; or ask the person.", problems.join(". ")));
        }
        let pairs: Vec<(&str, &str)> = settings.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        // one splice per script: its volumes one after the other
        let mut scripts: BTreeMap<&str, Vec<&Planned>> = BTreeMap::new();
        for p in &planned {
            scripts.entry(p.file.as_str()).or_default().push(p);
        }
        let (mut changes, mut solids) = (Vec::new(), Vec::new());
        for (_, group) in scripts {
            let inst = c.instance(&group[0].instance).ok_or("the plate changed")?;
            let volumes: Vec<NewVolume> = group.iter().map(|p| NewVolume { stem: p.volume.stem.clone(), numbered: p.volume.numbered, solid: p.volume.solid.clone() }).collect();
            let (ch, ids) = batch_statements(inst, &volumes, &pairs);
            changes.extend(ch);
            solids.extend(ids);
        }
        let names: Vec<&str> = planned.iter().map(|p| p.said.as_str()).collect();
        Ok((changes, solids, format!("{}: {}", names.join(", "), settings_words(&settings))))
    }

    pub(crate) fn settings_volume_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        let expected = match self.head_arg(args) {
            Ok(h) => h,
            Err(r) => return r,
        };
        let (changes, solids, done) = match self.volume_plan(args) {
            Ok(plan) => plan,
            Err(why) => return Reply::err(why),
        };
        if args.get("dry_run").and_then(Value::as_bool) == Some(true) {
            return match self.model.preview(&mut self.eval, &changes, cancelled) {
                Ok(_) => Reply::ok(obj(vec![("dry_run", flag(true)), ("committed", flag(false)), ("done", s(done)), ("solids", arr(solids.iter().map(s)))])),
                Err(e) => self.apply_error(e),
            };
        }
        let request_id = str_arg(args, "request_id").map_or_else(|| self.next_request_id(TOOL), str::to_string);
        // the slice before it, while it's still the plate's: what the volumes' reads compare with
        let before = self.last.clone().and_then(|ev| Some(Arc::new(VolumeBase::of(self.slices.as_ref()?.made(&ev)?, &ev))));
        match self.commit_changes(&request_id, expected, &changes, cancelled, true) {
            Ok((applied, built)) => {
                makepad_error_log::flight::mark_ab("fab.volume", &done, solids.len() as u64, 0);
                if let Some(base) = before {
                    self.volume_bases.extend(solids.iter().map(|id| (id.clone(), base.clone())));
                }
                self.commit_reply(vec![("head", s(applied.head.text())), ("done", s(done)), ("volumes", int(solids.len())), ("solids", arr(solids.iter().map(s)))], built)
            }
            Err(e) => self.apply_error(e),
        }
    }
}

/// The instance and frame a box in the model frame is written for: `object`'s, or the plate's only
/// part's.
fn part_frame<'c>(c: &'c Compiled, ev: &Evaluated, object: Option<&str>) -> Option<(&'c Instance, [[f64; 4]; 3])> {
    let object = match object {
        Some(o) => o.to_string(),
        None => match ev.plate.parts.as_slice() {
            [one] => one.object.clone(),
            _ => return None,
        },
    };
    let (inst, _) = c.object(&object)?;
    let k = placed_index(ev, &object)?;
    Some((inst, *ev.placed_frame(k)?))
}

/// How far past where a limb meets the body its capsule's cap reaches, in radii: a share of the
/// joint, which is where it breaks, and no more of the body.
const JOINT_REACH: f64 = 0.5;

/// How many samples a read takes of what's inside a volume, at most.
const READ_SAMPLES: f64 = 200_000.0;

/// The shape `limb` (plate mm) fits: a cylinder or capsule along its own axis, its radius
/// [`FIT_MARGIN`] past its widest section, a capsule's cap ending at a tip and just into a joint;
/// a box or sphere round its outline.
pub fn limb_shape(limb: &LimbFit, shape: VolumeShape) -> Option<Fit> {
    let radius = limb.radius + FIT_MARGIN;
    let at = |from: [f64; 3], t: f64| -> [f64; 3] { std::array::from_fn(|k| from[k] + limb.axis[k] * t) };
    let length: f64 = (0..3).map(|k| (limb.end[k] - limb.start[k]).powi(2)).sum::<f64>().sqrt();
    match shape {
        VolumeShape::Capsule => {
            let inset = |end: &LimbEnd| match end {
                LimbEnd::Tip => radius,
                LimbEnd::Meets { .. } => JOINT_REACH * radius,
            };
            let (ia, ib) = (inset(&limb.ends[0]), inset(&limb.ends[1]));
            if ia + ib >= length {
                let mid = at(limb.start, 0.5 * length);
                return Some(Fit::Capsule { a: mid, b: mid, radius });
            }
            Some(Fit::Capsule { a: at(limb.start, ia), b: at(limb.end, -ib), radius })
        }
        VolumeShape::Cylinder => {
            let past = |end: &LimbEnd| if matches!(end, LimbEnd::Tip) { FIT_MARGIN } else { 0.0 };
            Some(Fit::Cylinder { a: at(limb.start, -past(&limb.ends[0])), b: at(limb.end, past(&limb.ends[1])), radius })
        }
        other => fit_points(other, &limb.outline, false),
    }
}

/// A part's layers to read: the current slice's, or, none being current, the model's sections.
pub(crate) struct PartLayers {
    pub(crate) layers: Vec<(f64, f64, Vec<ExPolygon>)>,
    /// Where they sit on the plate.
    pub(crate) at: [f64; 2],
    pub(crate) from: &'static str,
}

/// The layer height of a read taken from sections (mm).
const SECTION_HEIGHT: f64 = 0.2;

/// A plate part's frame as `fab.pick` reads it: the frame its points are in.
pub(crate) fn pick_frame(ev: &Evaluated, k: usize) -> [[f64; 4]; 3] {
    ev.fields.get(k).and_then(|f| f.as_ref()).map_or(ev.model_to_plate[k], |f| f.to_printer)
}

impl Session {
    /// The layers of plate part `k` ([`PartLayers`]).
    pub(crate) fn part_layers(&mut self, ev: &Evaluated, k: usize) -> Result<PartLayers, String> {
        if let Some((plan, at)) = self.slices.as_ref().and_then(|s| s.made(ev)).and_then(|paths| paths.plans().get(k).cloned()) {
            let layers = plan_regions(&plan);
            if !layers.is_empty() {
                return Ok(PartLayers { layers, at: [at.x_mm(), at.y_mm()], from: "slice" });
            }
        }
        let part = &ev.plate.parts[k];
        let frame = pick_frame(ev, k);
        let mesh = self.eval.object_mesh(&part.object)?;
        let verts: Vec<[f64; 3]> = mesh.vertices.iter().map(|v| xf_point(&frame, [v.x, v.y, v.z])).collect();
        let mesh = makepad_fab_slicer::mesh::Mesh { verts, tris: mesh.triangles.clone(), paint: Vec::new() };
        let (lo, hi) = mesh.bbox();
        let count = ((hi[2] - lo[2]) / SECTION_HEIGHT).ceil().max(1.0) as usize;
        let zs: Vec<f64> = (0..count).map(|i| lo[2] + (i as f64 + 0.5) * SECTION_HEIGHT).collect();
        let cut = makepad_fab_slicer::slice::slice_mesh(&mesh, &zs, &Default::default());
        let layers = zs.iter().zip(cut).map(|(z, regions)| (z + 0.5 * SECTION_HEIGHT, SECTION_HEIGHT, regions)).collect();
        Ok(PartLayers { layers, at: [0.0, 0.0], from: "sections" })
    }

    pub(crate) fn fit_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        match self.fit_answer(args, cancelled) {
            Ok(answer) => Reply::ok(answer),
            Err(why) => Reply::err(why),
        }
    }

    fn fit_answer(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Result<S, String> {
        let ev = self.last.clone().ok_or("nothing is built yet")?;
        let points = args.get("points").and_then(Value::as_arr).filter(|l| !l.is_empty()).ok_or("'points' is a list of points on the part, [x y z] in model mm, from fab.pick")?;
        let points: Vec<[f64; 3]> = points.iter().map(|p| corner(Some(p), "points")).collect::<Result<_, _>>()?;
        let k = match str_arg(args, "object") {
            Some(object) => {
                // a pick answers the solid it hit: the part it prints in
                let (c, id) = (self.compiled.clone().ok_or("nothing is built yet")?, object.to_string());
                match placed_index(&ev, &id) {
                    Some(k) if k < ev.plate.parts.len() => k,
                    _ => self.locate(&id).map(|l| l.part).map_err(|why| format!("{why}; the parts on the plate: {}", c.instances.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")))?,
                }
            }
            None if ev.plate.parts.len() == 1 => 0,
            None => return Err("the plate has several parts: say which with 'object'".into()),
        };
        let shape = match str_arg(args, "shape") {
            None => VolumeShape::Capsule,
            Some("follows") => return Err("a fit makes a shape: capsule, cylinder, box or sphere".into()),
            Some(name) => VolumeShape::parse(name).ok_or_else(|| format!("'shape' is capsule, cylinder, box or sphere, not '{name}'"))?,
        };
        let frame = pick_frame(&ev, k);
        let to_model = fab_model::weave::invert(&frame);
        let read = self.part_layers(&ev, k)?;
        let plan = |p: [f64; 3]| [p[0] - read.at[0], p[1] - read.at[1], p[2]];
        let unplan = |p: [f64; 3]| [p[0] + read.at[0], p[1] + read.at[1], p[2]];
        let model = |p: [f64; 3]| xf_point(&to_model, unplan(p));
        let on_layers: Vec<[f64; 3]> = points.iter().map(|p| plan(xf_point(&frame, *p))).collect();
        let stop = || cancelled.is_some_and(|c| c());
        let limb = fit_limb(&read.layers, &on_layers, &stop)?;
        // the shape in the model frame, the frame the target's numbers are written in
        let scale = (frame[0][0].powi(2) + frame[1][0].powi(2) + frame[2][0].powi(2)).sqrt().max(1e-9);
        let plate_fit = limb_shape(&limb, shape).ok_or("that part has nothing to fit a shape round")?;
        let fit = match plate_fit {
            Fit::Capsule { a, b, radius } => Fit::Capsule { a: model(a), b: model(b), radius: radius / scale },
            Fit::Cylinder { a, b, radius } => Fit::Cylinder { a: model(a), b: model(b), radius: radius / scale },
            Fit::Sphere { at, radius } => Fit::Sphere { at: model(at), radius: radius / scale },
            Fit::Box { .. } => {
                let outline: Vec<[f64; 3]> = limb.outline.iter().map(|p| model(*p)).collect();
                fit_points(VolumeShape::Box, &outline, false).ok_or("that part has nothing to fit a shape round")?
            }
        };
        let mut target = vec![("shape", s(shape.name()))];
        target.extend(fit.numbers().into_iter().map(|(key, values)| (key, if values.len() == 3 { mm3([values[0], values[1], values[2]]) } else { n(values[0]) })));
        let end_word = |end: &LimbEnd| match end {
            LimbEnd::Tip => obj(vec![("is", s("a tip"))]),
            LimbEnd::Meets { radius } => obj(vec![("is", s("a thicker part")), ("radius", n(*radius / scale))]),
        };
        let radii = || limb.stations.iter().map(|st| st.radius / scale);
        let mut fields = vec![
            ("from", s(read.from)),
            ("target", obj(target)),
            ("axis", arr(fab_model::volumes::ModelWriter::new(&frame).turned(limb.axis).iter().map(|x| n(*x)))),
            ("start", mm3(model(limb.start))),
            ("end", mm3(model(limb.end))),
            ("ends", arr(limb.ends.iter().map(end_word))),
            ("radius", n(limb.radius / scale)),
            ("thinnest", n(radii().fold(f64::MAX, f64::min))),
            ("thickest", n(radii().fold(0.0, f64::max))),
            ("bend", n(limb.bend / scale)),
            ("volume_mm3", n(limb.volume)),
            ("frame", s("model mm, the frame of fab.pick's point")),
        ];
        if read.from != "slice" {
            fields.push(("note", s("no current slice: measured from sections of the model")));
        }
        if limb.bend > 0.5 * limb.radius {
            fields.push(("bends", s("it leaves the straight line by more than half its radius: fit each side of the bend from points of its own")));
        }
        if let Some(name) = str_arg(args, "volume") {
            fields.push(("volume", self.limb_share(&ev, k, name, &read, &limb, &to_model, &stop)?));
        }
        makepad_error_log::flight::mark_ab("fab.fit", read.from, k as u64, limb.stations.len() as u64);
        Ok(obj(fields))
    }

    /// What a volume holds of `limb`, and what else it holds: the shares of the solid inside it.
    fn limb_share(&self, ev: &Evaluated, k: usize, name: &str, read: &PartLayers, limb: &LimbFit, to_model: &[[f64; 4]; 3], stop: &dyn Fn() -> bool) -> Result<S, String> {
        let c = self.compiled.as_ref().ok_or("nothing is built yet")?;
        let (inst, _) = c.object(&ev.plate.parts[k].object).ok_or("that part has no script")?;
        let region = fab_model::paint::volume_region(&inst.script, name).ok_or_else(|| format!("no print settings volume '{name}' in {}", inst.name))?;
        let unplan = |p: [f64; 3]| xf_point(to_model, [p[0] + read.at[0], p[1] + read.at[1], p[2]]);
        let in_volume = |p: [f64; 3]| region.distance(unplan(p)) <= 0.0;
        // of the part: its stations' section cells; of the volume: the layers' solid in its box
        let (mut held, mut cells) = (0usize, 0usize);
        limb.each_cell(&mut |p| {
            cells += 1;
            if in_volume(p) {
                held += 1;
            }
        });
        let bounds = solid_box(ev, &part_id(&inst.name, name)).map(|b| on_layers(&pick_frame(ev, k), read, b)).ok_or_else(|| format!("'{name}' has no box yet"))?;
        let (mut inside, mut other) = (0usize, 0usize);
        solid_samples(&read.layers, bounds, read_cell(&bounds), stop, &mut |p| {
            if in_volume(p) {
                inside += 1;
                if limb.off_rod(p) > 0.0 {
                    other += 1;
                }
            }
        })?;
        let share = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 / b as f64 };
        Ok(obj(vec![
            ("name", s(name)),
            ("holds_of_this_part", n(share(held, cells))),
            ("of_what_it_holds_is_something_else", n(share(other, inside))),
        ]))
    }
}

/// A volume's tint in a picture.
pub(crate) const VOLUME_TINT: [f32; 3] = [0.98, 0.55, 0.08];
/// How far the tint sits off the surface it colours, so it wins over the part drawn under it (mm).
const TINT_LIFT: f64 = 0.12;
/// A facet crossing a volume's edge splits until its sides are this short (mm).
const TINT_EDGE: f64 = 0.6;

/// The part of a facet `corners` (plate mm) inside `inside`'s region, tinted, lifted along `normal`.
fn tint_facet(corners: [[f64; 3]; 3], normal: [f64; 3], inside: &dyn Fn([f64; 3]) -> f64, color: [f32; 3], mesh: &mut ColoredMesh) {
    let centre: [f64; 3] = std::array::from_fn(|k| (corners[0][k] + corners[1][k] + corners[2][k]) / 3.0);
    let distance = |a: [f64; 3], b: [f64; 3]| (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>().sqrt();
    let reach = corners.iter().map(|c| distance(*c, centre)).fold(0.0, f64::max);
    let d = inside(centre);
    if d > reach {
        return;
    }
    if d < -reach || reach < TINT_EDGE {
        if d <= 0.0 {
            mesh.triangle(corners.map(|c| std::array::from_fn(|k| c[k] + normal[k] * TINT_LIFT)), color);
        }
        return;
    }
    let mid = |a: [f64; 3], b: [f64; 3]| -> [f64; 3] { std::array::from_fn(|k| 0.5 * (a[k] + b[k])) };
    let [a, b, c] = corners;
    let (ab, bc, ca) = (mid(a, b), mid(b, c), mid(c, a));
    for piece in [[a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]] {
        tint_facet(piece, normal, inside, color, mesh);
    }
}

impl Session {
    /// The surfaces of the parts inside the volumes `names`, tinted: what a picture aimed at a
    /// volume shows. An unknown name lists the volumes there are.
    pub(crate) fn tint_volumes(&mut self, ev: &Evaluated, names: &[String], objects: &[String], mesh: &mut ColoredMesh) -> Result<(), String> {
        let c = self.compiled.clone().ok_or("nothing is built yet")?;
        for name in names {
            let holder = c.instances.iter().find(|inst| settings_solids(&inst.script.program).into_iter().any(|i| inst.script.program.solids[i].id == *name));
            let Some(inst) = holder else {
                let all: Vec<&str> = c.instances.iter().flat_map(|inst| settings_solids(&inst.script.program).into_iter().map(|i| inst.script.program.solids[i].id.as_str()).collect::<Vec<_>>()).collect();
                return Err(format!("no print settings volume '{name}': the volumes are [{}]", all.join(", ")));
            };
            let region = fab_model::paint::volume_region(&inst.script, name).ok_or_else(|| format!("'{name}' isn't a solid a picture can tint"))?;
            for (k, part) in ev.plate.parts.iter().enumerate() {
                let shown = objects.is_empty() || objects.iter().any(|o| *o == part.object);
                if shown && split_id(&part.object).is_some_and(|(i, _)| i == inst.name) {
                    self.tint_surface(ev, k, &|q| region.distance(q), VOLUME_TINT, mesh)?;
                }
            }
        }
        Ok(())
    }

    /// The surface of plate part `k` where `inside` (model frame, negative in) holds, tinted.
    pub(crate) fn tint_surface(&mut self, ev: &Evaluated, k: usize, inside: &dyn Fn([f64; 3]) -> f64, color: [f32; 3], mesh: &mut ColoredMesh) -> Result<(), String> {
        let frame = pick_frame(ev, k);
        let to_model = fab_model::weave::invert(&frame);
        let plate = |p: [f64; 3]| inside(xf_point(&to_model, p));
        let surface = self.eval.object_mesh(&ev.plate.parts[k].object)?;
        for t in &surface.triangles {
            let corners = t.map(|i| {
                let v = &surface.vertices[i as usize];
                xf_point(&frame, [v.x, v.y, v.z])
            });
            let (u, v): ([f64; 3], [f64; 3]) = (std::array::from_fn(|i| corners[1][i] - corners[0][i]), std::array::from_fn(|i| corners[2][i] - corners[0][i]));
            let cross = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let length = (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
            if length > 0.0 {
                tint_facet(corners, cross.map(|x| x / length), &plate, color, mesh);
            }
        }
        Ok(())
    }
}

/// The slice of the plate before a volume was placed: what its read says it changed against.
pub(crate) struct VolumeBase {
    paths: Arc<PlatePaths>,
    /// Each plate part's model frame then.
    frames: Vec<[[f64; 4]; 3]>,
}

impl VolumeBase {
    pub(crate) fn of(paths: Arc<PlatePaths>, ev: &Evaluated) -> VolumeBase {
        VolumeBase { paths, frames: (0..ev.plate.parts.len()).map(|k| slice_frame(ev, k)).collect() }
    }
}

/// Under this share of what it holds, a volume's change is nothing.
const NO_CHANGE: f64 = 0.002;

/// A plan's layers as `(top z, height, the solid's regions)`, bottom up, what it prints of the
/// part and not its supports.
pub(crate) fn plan_regions(plan: &makepad_fab_toolpath::ObjectPlan) -> Vec<(f64, f64, Vec<ExPolygon>)> {
    let mut rows: Vec<&(f64, f64, makepad_fab_toolpath::ObjectLayer)> = plan.layers.iter().filter(|(_, _, l)| !l.support_only).collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut layers: Vec<(f64, f64, Vec<ExPolygon>)> = Vec::with_capacity(rows.len());
    for (z, h, l) in rows {
        match layers.last_mut() {
            Some(last) if last.0 == *z => last.2.extend(l.slices.iter().cloned()),
            _ => layers.push((*z, *h, l.slices.to_vec())),
        }
    }
    layers
}

/// A plate part's frame as the slice's region settings read it.
pub(crate) fn slice_frame(ev: &Evaluated, k: usize) -> [[f64; 4]; 3] {
    ev.paint.get(k).and_then(|p| p.as_ref()).map_or_else(|| pick_frame(ev, k), |p| p.to_printer)
}

/// What each plate part's plan in `paths` prints inside a region in the model frame.
fn plate_printed(paths: &PlatePaths, frames: &[[[f64; 4]; 3]], parts: &[usize], region: &Region, bounds: Option<([f64; 3], [f64; 3])>, stop: &(dyn Fn() -> bool + Sync)) -> Option<Printed> {
    let mut total = Printed::default();
    for &k in parts {
        let (Some((plan, at)), Some(frame)) = (paths.plans().get(k), frames.get(k)) else { continue };
        let to_model = fab_model::weave::invert(frame);
        let at = [at.x_mm(), at.y_mm()];
        let inside = |p: [f64; 3]| {
            let q = xf_point(&to_model, [p[0] + at[0], p[1] + at[1], p[2]]);
            let near = bounds.is_none_or(|(lo, hi)| (0..3).all(|i| q[i] >= lo[i] - 0.5 && q[i] <= hi[i] + 0.5));
            near && region.distance(q) <= 0.0
        };
        total.add(&printed_inside(&plan.layers, &inside, stop)?);
    }
    Some(total)
}

impl Session {
    /// The slice's read of the volumes of a build: `fab.inspect {read_volumes: true}`. Each says
    /// what the current slice prints inside it, what that adds against the slice before it was
    /// placed, what's inside it by feature, and when it changes nothing; with no slice made, this is
    /// what waits for it, as `fab.supports` does.
    pub(crate) fn volumes_read(&mut self, c: &Compiled, ev: &Evaluated, cancelled: Option<&dyn Fn() -> bool>) -> S {
        let Some(source) = self.slices.clone() else { return obj(vec![("current", flag(false)), ("note", s("this session has no slicer to read a slice from"))]) };
        let paths = match source.made(ev) {
            Some(paths) => paths,
            None => match source.input(ev).and_then(|(input, _)| source.sliced(&input, cancelled)) {
                Ok(paths) => paths,
                Err(why) => return obj(vec![("current", flag(false)), ("note", s(why))]),
            },
        };
        let frames: Vec<[[f64; 4]; 3]> = (0..ev.plate.parts.len()).map(|k| slice_frame(ev, k)).collect();
        let density = self.slices.as_ref().and_then(|src| src.input(ev).ok()).and_then(|(input, _)| (input.setup)(None).ok()).and_then(|setup| setup.filaments.first().map(|f| f.values.f64_at("filament_density", 0))).filter(|d| *d > 0.0);
        let grams = |mm3: f64| density.map_or(S::Nil, |d| n(mm3 * d / 1000.0));
        let flagged = std::sync::atomic::AtomicBool::new(false);
        let stop = || flagged.load(std::sync::atomic::Ordering::Relaxed);
        let every: Vec<usize> = (0..ev.plate.parts.len()).collect();
        let mut plate = Printed::default();
        for &k in &every {
            if let Some((plan, _)) = paths.plans().get(k) {
                plate.add(&printed_inside(&plan.layers, &|_| true, &stop).unwrap_or_default());
            }
        }
        let mut rows = Vec::new();
        for inst in &c.instances {
            let p = &inst.script.program;
            let parts: Vec<usize> = every.iter().copied().filter(|k| split_id(&ev.plate.parts[*k].object).is_some_and(|(i, _)| i == inst.name)).collect();
            for i in settings_solids(p).into_iter().filter(|i| p.solids[*i].parent.is_none()) {
                if cancelled.is_some_and(|f| f()) {
                    flagged.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                let id = part_id(&inst.name, &p.solids[i].id);
                let Some(region) = fab_model::paint::volume_region(&inst.script, &p.solids[i].id) else {
                    rows.push(obj(vec![("name", s(&p.solids[i].id)), ("note", s("its region isn't a solid a read can walk"))]));
                    continue;
                };
                let bounds = solid_box(ev, &id);
                let Some(now) = plate_printed(&paths, &frames, &parts, &region, bounds, &stop) else { continue };
                let mut row = vec![
                    ("name", s(&p.solids[i].id)),
                    ("layers", int(now.layers)),
                    ("from_z", n(now.from_z)),
                    ("to_z", n(now.to_z)),
                    ("walls_mm3", n(now.walls_mm3)),
                    ("sparse_mm3", n(now.sparse_mm3)),
                    ("solid_fill_mm3", n(now.solid_mm3)),
                    ("grams", grams(now.mm3())),
                    ("print_s", n(now.seconds)),
                ];
                let reads: Vec<(usize, PartLayers)> = parts.iter().filter_map(|k| Some((*k, self.part_layers(ev, *k).ok()?))).collect();
                let solid = bounds.map(|b| solid_inside(ev, &reads, &region, b, &stop));
                let (solid_mm3, area) = solid.as_ref().map_or((0.0, 0.0), |i| (i.mm3, i.area_mm2));
                row.push(("solid_mm3", n(solid_mm3)));
                if solid_mm3 > 0.0 {
                    row.push(("filled", n(now.mm3() / solid_mm3)));
                }
                row.push(("area_mm2", n(area)));
                if let Some(base) = self.volume_bases.get(&id) {
                    if let Some(then) = plate_printed(&base.paths, &base.frames, &parts, &region, bounds, &stop) {
                        let (added, took) = (now.mm3() - then.mm3(), now.seconds - then.seconds);
                        row.push(("added_mm3", n(added)));
                        row.push(("added_grams", grams(added)));
                        row.push(("added_s", n(took)));
                        if added.abs() <= NO_CHANGE * now.mm3().max(1.0) && took.abs() <= NO_CHANGE * now.seconds.max(1.0) {
                            row.push(("changed_nothing", s("the slice inside it prints as it did before it was placed: what it sets is what the part has there already")));
                        }
                    }
                } else {
                    row.push(("added_mm3", S::Nil));
                }
                if now.layers == 0 {
                    row.push(("changed_nothing", s("it reaches nothing that prints")));
                }
                row.push(("inside", solid.map_or_else(|| arr([]), |i| i.by_feature)));
                rows.push(obj(row));
            }
        }
        obj(vec![("current", flag(true)), ("plate_mm3", n(plate.mm3())), ("plate_grams", grams(plate.mm3())), ("plate_print_s", n(plate.seconds)), ("volumes", arr(rows))])
    }
}

/// What's solid inside a region: how much, its largest section, and by the named feature it's in.
struct SolidInside {
    mm3: f64,
    area_mm2: f64,
    by_feature: S,
}

/// A box in the model frame as a box in a part's layers' frame.
pub(crate) fn on_layers(frame: &[[f64; 4]; 3], read: &PartLayers, (lo, hi): ([f64; 3], [f64; 3])) -> ([f64; 3], [f64; 3]) {
    let corners: Vec<[f64; 3]> = (0..8).map(|c| xf_point(frame, std::array::from_fn(|i| if c >> i & 1 == 0 { lo[i] } else { hi[i] }))).collect();
    let low: [f64; 3] = std::array::from_fn(|i| corners.iter().map(|p| p[i]).fold(f64::MAX, f64::min));
    let high: [f64; 3] = std::array::from_fn(|i| corners.iter().map(|p| p[i]).fold(f64::MIN, f64::max));
    ([low[0] - read.at[0], low[1] - read.at[1], low[2]], [high[0] - read.at[0], high[1] - read.at[1], high[2]])
}

/// The cell of a read of a box: about [`READ_SAMPLES`] of them, however big.
pub(crate) fn read_cell(b: &([f64; 3], [f64; 3])) -> f64 {
    ((0..3).map(|i| (b.1[i] - b.0[i]).max(0.1)).product::<f64>() / READ_SAMPLES).cbrt().max(0.1)
}

/// Walks the solid of `reads` in a region; `rest` names what's in no feature. A model with no named
/// features has only that.
fn solid_inside(ev: &Evaluated, reads: &[(usize, PartLayers)], region: &Region, bounds: ([f64; 3], [f64; 3]), stop: &dyn Fn() -> bool) -> SolidInside {
    const REST: &str = "rest";
    let (mut mm3, mut samples) = (0.0, 0usize);
    let mut sections: BTreeMap<i64, f64> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (k, read) in reads {
        let frame = slice_frame(ev, *k);
        let to_model = fab_model::weave::invert(&frame);
        let plan = on_layers(&frame, read, bounds);
        let cell = read_cell(&plan);
        // a stopped walk reads as what it got to: the caller's cancel ends the whole answer
        let _ = solid_samples(&read.layers, plan, cell, stop, &mut |p| {
            let q = xf_point(&to_model, [p[0] + read.at[0], p[1] + read.at[1], p[2]]);
            if region.distance(q) > 0.0 {
                return;
            }
            mm3 += cell * cell * cell;
            samples += 1;
            *sections.entry((p[2] / cell).round() as i64).or_default() += cell * cell;
            let found = ev.features.get(*k).and_then(|f| fab_model::paint::feature_at(f, q, 0.0));
            *counts.entry(found.map_or(REST.to_string(), |f| f.name.clone())).or_default() += 1;
        });
    }
    let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    SolidInside {
        mm3,
        area_mm2: sections.values().copied().fold(0.0, f64::max),
        by_feature: arr(rows.into_iter().map(|(name, count)| obj(vec![("solid", s(name)), ("share", n(count as f64 / samples.max(1) as f64))]))),
    }
}

#[cfg(test)]
#[path = "volumes_tests.rs"]
mod tests;
