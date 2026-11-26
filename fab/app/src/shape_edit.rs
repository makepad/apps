//! Shape edits: a primitive cut away from or joined to a feature of an object's script, and the
//! slabs that cut its bottom flat or cut it along a plane. Each is a few changes of the
//! script's code ([`Edit`]s, one transaction: one undo step) that leave the program in the
//! builder style, every value a literal its handles edit later:
//!
//! ```text
//! let lattice_render_base = m.import("lattice_render", {scale: size / 100mm})
//! let cut1 = m.box({size: [30mm 60mm 40mm] at: [-40mm 50mm 0mm]})
//! let lattice_render = m.carve(lattice_render_base, [cut1], 0mm)
//! object(lattice_render)
//! ```
//!
//! A feature that already is such a boolean takes the new tool into its list. Any other named
//! feature keeps its name (its uses and the object it emits stay as they are): its code moves
//! to a `let <name>_base` and the name becomes the boolean of that with the tool. A distance
//! field (an import, a field primitive or blend) is carved and blended, which the SDF view
//! redraws locally and live while a tool's handle is dragged; a meshed solid is cut and joined
//! by the mesh booleans.
//!
//! Tools are written in the object's model frame (Y up, mm), placed from the part as it sits on
//! the plate: a bottom cut is level with the bed whatever the object's turn.
//!
//! A split ([`split`]) keeps both sides of a cut plane instead: the object's solid becomes an
//! `m.split` at the slab's plane, with alignment pins, and each half an object of its own.

use fab::api::{settings_span, BedArea, ShapeOp, ShapePrim, NO_TREES_HEIGHT};
use fab_model::eval::{euler_xyz, rotation_xyz, turn_onto, Evaluated};
use fab_model::program::{part_id, Compiled, Instance};
use fab_model::volumes::{free_name, mm, settings_statement, taken, ModelWriter, DEFAULT_SETTINGS};
use fab_model::{Anchor, Change, Edit, PlaceArg};
use makepad_model_script::lang::csg_graph::{eval::transform_matrix, Op};
use fab_plate::machine::Rect;
use fab_plate::Bounds;
use makepad_model_script::lang::edit::{call_fields, part_call};
use makepad_model_script::lang::{protos, Program, Solid};
use makepad_script::syntax::write::list;

/// Where the object sits: its model-to-plate frame (row-major 3x4, mm) and its box on the plate.
pub struct Placed {
    pub frame: [[f64; 4]; 3],
    pub bounds: Bounds,
}

/// The changes of `op` on feature `target` (a solid of `inst`), and the plate part id of the
/// tool it adds (to pick it in the tree). `bed`: where a split's other half may go.
pub fn changes(inst: &Instance, target: &str, op: ShapeOp, at: &Placed, bed: &Rect) -> Result<(Vec<Change>, String), String> {
    let p = &inst.script.program;
    let s = p.solid(target).ok_or_else(|| format!("'{target}' is not a solid of {}", inst.script.file))?;
    if op == ShapeOp::Split {
        return split(inst, target, at, bed);
    }
    let cut = !matches!(op, ShapeOp::Add(_));
    let stem = match op {
        ShapeOp::Subtract(_) => "cut",
        ShapeOp::Add(_) => "add",
        ShapeOp::CutBottom => "cut_bottom",
        ShapeOp::CutPlane | ShapeOp::Split => "cut_plane",
    };
    let name = free_name(p, stem, matches!(op, ShapeOp::Subtract(_) | ShapeOp::Add(_)));
    let tool = format!("let {name} = {}", tool_code(op, at));
    let file = inst.script.file.clone();
    let script = |edit: Edit| Change::Script { file: file.clone(), edit };
    let list_field = match (cut, s.proto) {
        (true, "Carve" | "Difference") | (false, "Union") => Some("b"),
        (false, "Blend") => Some("of"),
        _ => None,
    };
    let mut edits = Vec::new();
    match list_field.filter(|_| s.named) {
        // Already the boolean: the tool joins its list.
        Some(field) => {
            let old = field_code(p, s, field)?;
            let old = old.strip_prefix('[').and_then(|o| o.strip_suffix(']')).unwrap_or(&old).trim().to_string();
            edits.push(Edit::Add { code: tool, at: Anchor::Before(target.to_string()) });
            edits.push(Edit::Field { solid: target.to_string(), field: field.to_string(), code: format!("[{old} {name}]") });
        }
        None => {
            if !s.named {
                return Err(format!("'{}' is written inside other code: give it a `let` of its own first", s.id));
            }
            let span = s.span.clone().ok_or_else(|| format!("'{target}' is made by code, not a call"))?;
            let stmt = p.names.syntax.find_let(target).and_then(|loc| p.names.syntax.node(&loc));
            let whole = stmt.is_some_and(|n| matches!(&n.kind, makepad_script::syntax::Kind::Let { value: Some(v), .. } if v.span == span));
            if !whole {
                return Err(format!("'{target}' is not the whole of its `let`: edit its code"));
            }
            let base = (1..).map(|i| if i == 1 { format!("{target}_base") } else { format!("{target}_base{i}") }).find(|n| !taken(p, n)).unwrap();
            // Inside a field (a carve's cutter, a blend's operand) the whole subtree stays a field.
            let field = is_field(s) || std::iter::successors(s.parent, |&i| p.solids[i].parent).any(|i| is_field(&p.solids[i]));
            let joined = match (cut, field) {
                (true, true) => format!("m.carve({base}, [{name}], 0mm)"),
                (true, false) => format!("m.subtract({base}, [{name}])"),
                (false, true) => format!("m.blend([{base} {name}], 0mm)"),
                (false, false) => format!("m.union({base}, [{name}])"),
            };
            edits.push(Edit::Add { code: format!("let {base} = {}", &p.source()[span]), at: Anchor::Before(target.to_string()) });
            edits.push(Edit::Add { code: tool, at: Anchor::Before(target.to_string()) });
            edits.push(Edit::Replace { solid: target.to_string(), code: joined });
        }
    }
    Ok((edits.into_iter().map(script).collect(), part_id(&inst.name, &name)))
}

/// Both sides of the object's cut plane kept, with alignment pins (the Edit shape strip's "Keep
/// both halves, add pins"): its solid `target` becomes an `m.split` where its last Cut plane slab
/// cuts (none: level through its middle), the slab gone, and each half an object of its own,
/// turned so the cut's normal points up: the half with the pegs stands on its other side with
/// them up, the half with the holes on its cut. The side the slab kept keeps the object's name and
/// spot, turned about its middle; the other goes beside it on `bed` (else where the plate puts a
/// free object) with the object's turn, scale, colour and settings. One transaction (one undo
/// step); the changes and the kept half's plate part id.
pub fn split(inst: &Instance, target: &str, at: &Placed, bed: &Rect) -> Result<(Vec<Change>, String), String> {
    let p = &inst.script.program;
    let object = inst.script.objects.iter().find(|o| p.solids[o.solid].id == target).ok_or_else(|| format!("'{target}' is not an object of {}: pick the whole part to split it", inst.script.file))?;
    let s = &p.solids[object.solid];
    if !s.named {
        return Err(format!("'{}' is written inside other code: give it a `let` of its own first", s.id));
    }
    if object.bodies.len() > 1 {
        return Err(format!("'{}' is several bodies: split each on its own", object.name));
    }
    let w = ModelWriter::new(&at.frame);
    let slab = if s.proto == "Difference" { s.children.iter().rev().copied().find(|&c| p.solids[c].id.starts_with("cut_plane")) } else { None };
    let (center, normal) = match slab {
        Some(c) => slab_plane(p, &p.solids[c]).ok_or_else(|| format!("'{}' is no box to cut along", p.solids[c].id))?,
        None => (w.to_model(at.bounds.center()), w.turned([0.0, 0.0, 1.0])),
    };
    let dir = |v: f64| {
        let r = (v * 10_000.0).round() / 10_000.0;
        if r == 0.0 { 0.0 } else { r }
    };
    let opts = format!("{{center: {} normal: {}}}", list(center.map(mm)), list(normal.map(|v| dir(v).to_string())));
    let mut edits = vec![Edit::Root { kind: "object".into(), solid: target.to_string(), add: false }];
    // The slab alone cut the part: its base splits. Else the rest of the part does.
    let sole = slab.filter(|&c| field_code(p, s, "b").is_ok_and(|b| b.trim_matches(|ch: char| ch == '[' || ch == ']' || ch.is_whitespace()) == p.solids[c].id));
    match sole {
        Some(c) => {
            edits.push(Edit::Replace { solid: target.to_string(), code: format!("m.split({}, {opts})", field_code(p, s, "a")?) });
            edits.push(Edit::Remove { solid: p.solids[c].id.clone() });
        }
        None => {
            let whole = free_name(p, &format!("{target}_whole"), false);
            edits.push(Edit::Rename { solid: target.to_string(), to: whole.clone() });
            edits.push(Edit::Add { code: format!("let {target} = m.split({whole}, {opts})"), at: Anchor::Auto });
            if let Some(c) = slab {
                edits.push(Edit::Remove { solid: p.solids[c].id.clone() });
            }
        }
    }
    let other = (2..).map(|i| format!("{}_{i}", object.name)).find(|n| !inst.script.objects.iter().any(|o| o.name == *n) && !taken(p, n)).unwrap();
    edits.push(Edit::Add { code: format!("object({target}[1], name: \"{}\")", object.name), at: Anchor::Auto });
    edits.push(Edit::Add { code: format!("object({target}[0], name: \"{other}\")"), at: Anchor::Auto });
    let file = inst.script.file.clone();
    let mut changes: Vec<Change> = edits.into_iter().map(|edit| Change::Script { file: file.clone(), edit }).collect();
    // Both halves turn the cut's normal up, the kept one about its middle.
    let placed = inst.placed.iter().find(|q| q.object == object.name);
    let r0 = rotation_xyz(placed.map_or([0.0; 3], |q| q.rotate));
    let f = &at.frame;
    let up: [f64; 3] = std::array::from_fn(|i| f[i][0] * normal[0] + f[i][1] * normal[1] + f[i][2] * normal[2]);
    let turn = turn_onto(up, [0.0, 0.0, 1.0]);
    let turned: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| turn[i][k] * r0[k][j]).sum()));
    let round = |v: f64| (v * 1000.0).round() / 1000.0;
    let rotate = euler_xyz(&turned).map(round);
    let c = at.bounds.center();
    let v = [f[0][3] - c[0], f[1][3] - c[1], f[2][3] - c[2]];
    let origin: [f64; 3] = std::array::from_fn(|i| c[i] + turn[i][0] * v[0] + turn[i][1] * v[1] + turn[i][2] * v[2]);
    let place = |object: &str, arg: PlaceArg| Change::Place { instance: inst.name.clone(), object: object.to_string(), arg };
    changes.push(place(&object.name, PlaceArg::At(Some([round(origin[0]), round(origin[1])]))));
    changes.push(place(&object.name, PlaceArg::Rotate(rotate)));
    changes.push(place(&object.name, PlaceArg::Drop(true)));
    // The other half beside the whole part as turned (the kept half lies inside that box), a
    // gap off it and 5 mm inside the bed: right of it, left, behind, in front.
    let (lo, hi) = (at.bounds.min, at.bounds.max);
    let (mut tlo, mut thi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for k in 0..8 {
        let q = [if k & 1 == 0 { lo[0] } else { hi[0] }, if k & 2 == 0 { lo[1] } else { hi[1] }, if k & 4 == 0 { lo[2] } else { hi[2] }];
        let d = [q[0] - c[0], q[1] - c[1], q[2] - c[2]];
        for i in 0..2 {
            let t = c[i] + turn[i][0] * d[0] + turn[i][1] * d[1] + turn[i][2] * d[2];
            tlo[i] = tlo[i].min(t);
            thi[i] = thi[i].max(t);
        }
    }
    const GAP: f64 = 10.0;
    let (w, d) = (thi[0] - tlo[0] + GAP, thi[1] - tlo[1] + GAP);
    let fits = |off: [f64; 2]| (0..2).all(|i| tlo[i] + off[i] >= bed.min[i] + 5.0 && thi[i] + off[i] <= bed.max[i] - 5.0);
    let beside = [[w, 0.0], [-w, 0.0], [0.0, d], [0.0, -d]].into_iter().find(|off| fits(*off));
    changes.push(place(&other, PlaceArg::At(beside.map(|off| [round(origin[0] + off[0]), round(origin[1] + off[1])]))));
    changes.push(place(&other, PlaceArg::Rotate(rotate)));
    changes.push(place(&other, PlaceArg::Drop(true)));
    if let Some(q) = placed {
        changes.push(place(&other, PlaceArg::Scale(q.scale)));
        changes.push(place(&other, PlaceArg::Color(q.color.clone())));
        changes.push(place(&other, PlaceArg::Print(q.print)));
        changes.push(place(&other, PlaceArg::Settings(q.settings.clone())));
    }
    Ok((changes, part_id(&inst.name, &format!("{target}[1]"))))
}

/// A box solid's +X face in the model frame: its centre and its outward normal. A Cut plane
/// slab's face on the cut, facing the side the cut keeps.
fn slab_plane(p: &Program, s: &Solid) -> Option<([f64; 3], [f64; 3])> {
    let mut m = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
    let mut node = s.node;
    loop {
        match &p.graph.node(node).op {
            Op::Tag { source, .. } => node = *source,
            Op::Transform { source, at, rotate, scale } => {
                m = fab_model::weave::compose(&m, &transform_matrix(*at, *rotate, *scale));
                node = *source;
            }
            Op::Box { size, .. } => {
                let q = |v: [f64; 4]| -> [f64; 3] { std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2] + m[i][3] * v[3]) };
                let n = q([1.0, 0.0, 0.0, 0.0]);
                let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                return (l > 1e-9).then(|| (q([0.5 * size[0], 0.0, 0.0, 1.0]), n.map(|x| x / l)));
            }
            _ => return None,
        }
    }
}

/// The part an area drawn over the bed is for: of the parts that print (`parts`, the plate's),
/// the one whose footprint it overlaps most, else the one nearest it. None when none prints.
pub fn area_part(parts: &[fab_plate::Part], area: BedArea) -> Option<usize> {
    let (lo, hi) = match area {
        BedArea::Rect { min, max } => (min, max),
        BedArea::Disc { centre, radius } => ([centre[0] - radius, centre[1] - radius], [centre[0] + radius, centre[1] + radius]),
    };
    let fit = |b: Bounds| {
        let overlap: f64 = (0..2).map(|k| (hi[k].min(b.max[k]) - lo[k].max(b.min[k])).max(0.0)).product();
        let gap: f64 = (0..2).map(|k| (lo[k] - b.max[k]).max(b.min[k] - hi[k]).max(0.0).powi(2)).sum();
        (overlap, gap)
    };
    let fits: Vec<(f64, f64)> = parts.iter().map(|p| fit(p.bounds())).collect();
    (0..parts.len()).max_by(|&a, &b| fits[a].0.total_cmp(&fits[b].0).then(fits[b].1.total_cmp(&fits[a].1)))
}

/// An area drawn on the bed as an edit of the plate ([`no_trees`] in the script of the part it
/// is for, [`area_part`]): the changes, the new solid's plate part id and the part's name; or,
/// in the person's words, why it goes nowhere.
pub fn no_trees_on_plate(program: &Compiled, ev: &Evaluated, area: BedArea) -> Result<(Vec<Change>, String, String), String> {
    let i = area_part(&ev.plate.parts, area).ok_or("No trees here keeps a printing part's trees out: no part on the plate prints")?;
    let part = &ev.plate.parts[i];
    let (inst, _) = program.object(&part.object).ok_or_else(|| format!("No trees here: {} has no script to write the area into", part.name))?;
    let frame = ev.placed_frame(i).ok_or_else(|| format!("No trees here: {} has no place on the plate yet", part.name))?;
    let (changes, solid) = no_trees(inst, &Placed { frame: *frame, bounds: part.bounds() }, area);
    Ok((changes, solid, part.name.clone()))
}

/// The changes that keep `inst`'s trees out of `area` on the bed, up to [`NO_TREES_HEIGHT`]: a
/// box (or a cylinder for a disc) in the object's model frame, marked by `m.no_trees`, at the
/// script's end (one undo step); and the plate part id of the solid (to pick it in the tree).
pub fn no_trees(inst: &Instance, at: &Placed, area: BedArea) -> (Vec<Change>, String) {
    let p = &inst.script.program;
    let name = free_name(p, "no_trees", true);
    let w = ModelWriter::new(&at.frame);
    let h = NO_TREES_HEIGHT;
    let solid = match area {
        BedArea::Rect { min, max } => w.boxed([max[0] - min[0], max[1] - min[1], h], [0.5 * (min[0] + max[0]), 0.5 * (min[1] + max[1]), 0.5 * h]),
        BedArea::Disc { centre, radius } => w.cylinder(radius, h, [centre[0], centre[1], 0.5 * h]),
    };
    let src = p.source();
    let text = format!("{}let {name} = {solid}\nm.no_trees({name})\n", if src.is_empty() || src.ends_with('\n') { "" } else { "\n" });
    let edit = Edit::Splice { range: src.len()..src.len(), text };
    (vec![Change::Script { file: inst.script.file.clone(), edit }], part_id(&inst.name, &name))
}

/// A volume drawn on the layer at plate height `z` as an edit of the plate ([`settings_here`]
/// in the script of plate part `object`, the one picked: the box sticks to it): the changes, the
/// new solid's plate part id and the part's name; or, in the person's words, why it goes nowhere.
pub fn settings_on_plate(program: &Compiled, ev: &Evaluated, object: &str, area: BedArea, z: f64) -> Result<(Vec<Change>, String, String), String> {
    let i = ev.plate.parts.iter().position(|p| p.object == object).ok_or("Print settings here: the picked part has left the plate")?;
    let part = &ev.plate.parts[i];
    let (inst, _) = program.object(&part.object).ok_or_else(|| format!("Print settings here: {} has no script to write the volume into", part.name))?;
    let frame = ev.placed_frame(i).ok_or_else(|| format!("Print settings here: {} has no place on the plate yet", part.name))?;
    let (changes, solid) = settings_here(inst, &Placed { frame: *frame, bounds: part.bounds() }, area, z);
    Ok((changes, solid, part.name.clone()))
}

/// The changes that give `inst` print settings of their own in the volume drawn on the layer at
/// plate height `z` ([`settings_span`] high): a box on the rectangle (a sphere on a disc) in the
/// object's model frame with `m.settings`, a solid infill to start, at the script's end (one undo
/// step); and the plate part id of the solid (to pick it in the tree).
pub fn settings_here(inst: &Instance, at: &Placed, area: BedArea, z: f64) -> (Vec<Change>, String) {
    let w = ModelWriter::new(&at.frame);
    let (lo, hi) = settings_span(area, z);
    let solid = match area {
        BedArea::Rect { min, max } => w.boxed([max[0] - min[0], max[1] - min[1], hi - lo], [0.5 * (min[0] + max[0]), 0.5 * (min[1] + max[1]), 0.5 * (lo + hi)]),
        BedArea::Disc { centre, radius } => w.sphere(radius, [centre[0], centre[1], 0.5 * (lo + hi)]),
    };
    settings_statement(inst, "settings", true, &solid, DEFAULT_SETTINGS)
}

/// A distance-field solid (carved and blended as one field).
fn is_field(s: &Solid) -> bool {
    matches!(s.proto, "Import" | "Blend" | "Carve" | "Ellipsoid" | "Capsule" | "Sdf" | "Instance" | "Bone" | "Twist" | "Bend" | "Taper" | "Fold")
}

/// The code a field of the solid's call is written as.
fn field_code(p: &Program, s: &Solid, field: &str) -> Result<String, String> {
    let call = part_call(p, s)?;
    let proto = protos::proto(s.proto).ok_or("not a part")?;
    let (_, at) = call_fields(&p.names.syntax, &call, proto).into_iter().find(|(n, _)| n == field).ok_or_else(|| format!("'{}' writes no '{field}'", s.id))?;
    let node = p.names.syntax.node(at.loc()).ok_or("no node")?;
    Ok(p.source()[node.span.clone()].to_string())
}

/// A tool's call: the primitive in the part's box on the plate, written in the model frame.
fn tool_code(op: ShapeOp, at: &Placed) -> String {
    let w = ModelWriter::new(&at.frame);
    let k = w.k;
    let (lo, hi) = (at.bounds.min, at.bounds.max);
    let size: [f64; 3] = std::array::from_fn(|i| (hi[i] - lo[i]).max(1.0));
    let mid: [f64; 3] = std::array::from_fn(|i| 0.5 * (lo[i] + hi[i]));
    let span = size[0].max(size[1]).max(size[2]);
    let place = |c: [f64; 3]| w.place(c);
    let boxed = |sz: [f64; 3], c: [f64; 3]| w.boxed(sz, c);
    let r_small = 0.2 * size[0].min(size[1]).min(size[2]);
    match op {
        // A tool against the part's left side, half in: where it starts cutting.
        ShapeOp::Subtract(prim) => {
            let c = [lo[0] + 0.1 * size[0], mid[1], mid[2]];
            match prim {
                ShapePrim::Box => boxed([0.4 * size[0], 0.5 * size[1], 0.5 * size[2]], c),
                ShapePrim::Cylinder => w.cylinder(r_small, 0.6 * size[2], c),
                ShapePrim::Sphere => format!("m.sphere({{radius: {} {}}})", mm(r_small / k), place(c)),
            }
        }
        // A plinth under the part (box, disc), or a ball at its middle.
        ShapeOp::Add(prim) => {
            let t = 3.0;
            let under = [mid[0], mid[1], lo[2] - t * 0.5 + 0.5];
            match prim {
                ShapePrim::Box => boxed([1.1 * size[0], 1.1 * size[1], t], under),
                ShapePrim::Cylinder => w.cylinder(0.55 * size[0].max(size[1]), t, under),
                ShapePrim::Sphere => format!("m.sphere({{radius: {} {}}})", mm(r_small / k), place(mid)),
            }
        }
        // Everything below a level just over the bed: a slab wider than the part, its top there.
        ShapeOp::CutBottom => {
            let level = lo[2] + (0.02 * size[2]).clamp(1.0, 5.0);
            let deep = 2.0 * span;
            boxed([size[0] + span, size[1] + span, deep], [mid[0], mid[1], level - 0.5 * deep])
        }
        // Everything left of a plane through the part's left part: a slab to turn and move.
        ShapeOp::CutPlane | ShapeOp::Split => {
            let plane = lo[0] + 0.15 * size[0];
            let deep = 2.0 * span;
            boxed([deep, size[1] + span, size[2] + span], [plane - 0.5 * deep, mid[1], mid[2]])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_model::eval::Evaluator;
    use fab_model::{FabModel, PlaceArg};
    use fab_plate::machine::Machine;

    const FILE: &str = "t.cad.splash";

    /// A project written before objects were named: an object known by its line (`box@7`)
    /// placed by that name, its code without one. A cut on the `let` above it adds two lines
    /// before that `let`; the cut's write names the object, so it stays where it was placed.
    #[test]
    fn a_cut_before_a_named_let_leaves_an_unnamed_object_in_place() {
        let source = format!("{}let lid = m.box({{size: [30mm 4mm 30mm]}})\nobject(lid)\nobject(m.box({{size: [10mm 10mm 10mm]}}))\n", fab_model::program::EMPTY);
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let mut first = FabModel::with_script("t", FILE, &source);
        let place = [Change::Place { instance: "t".into(), object: "box@7".into(), arg: PlaceArg::At(Some([60.0, 70.0])) }];
        first.apply(&mut e, "place", first.head(), &place, None).unwrap();
        let mut m = FabModel::load("t", &first.project_text(), std::collections::BTreeMap::from([(FILE.to_string(), source.clone())])).unwrap();
        let (c, ev) = m.evaluate(&mut e, None).unwrap();
        let at = |ev: &Evaluated| ev.plate.parts.iter().find(|p| p.object == "t/box@7").map(|p| p.mesh.bounds().center());
        let placed = (m.project_text(), at(&ev).expect("loads placed"));
        let i = ev.plate.placed_parts().position(|p| p.object == "t/lid").unwrap();
        let on = Placed { frame: *ev.placed_frame(i).unwrap(), bounds: ev.plate.placed_part(i).unwrap().bounds() };
        let (box_cut, _) = changes(&c.instances[0], "lid", ShapeOp::Subtract(ShapePrim::Box), &on, &ev.machine.bed).unwrap();
        let (_, built) = m.apply(&mut e, "cut", m.head(), &box_cut, None).unwrap();
        let (_, ev) = built.unwrap();
        let script = m.source(FILE).unwrap();
        assert!(script.contains("let lid_base = m.box") && script.contains("let cut1 = "), "{script}");
        assert_eq!((m.project_text(), at(&ev).expect("still placed")), placed, "{script}");
        assert!(script.contains("object(m.box({size: [10mm 10mm 10mm]}), name: \"box@7\")"), "{script}");
        // A cut plane on it, then both halves kept: the slab gone, the box cut kept in what
        // splits, each half an object turned onto the bed with its cut level.
        let lid = |ev: &Evaluated| {
            let i = ev.plate.placed_parts().position(|p| p.object == "t/lid").unwrap();
            Placed { frame: *ev.placed_frame(i).unwrap(), bounds: ev.plate.placed_part(i).unwrap().bounds() }
        };
        let (c, ev) = m.evaluate(&mut e, None).unwrap();
        let (cut, _) = changes(&c.instances[0], "lid", ShapeOp::CutPlane, &lid(&ev), &ev.machine.bed).unwrap();
        m.apply(&mut e, "cut plane", m.head(), &cut, None).unwrap();
        let (c, ev) = m.evaluate(&mut e, None).unwrap();
        let (split, kept) = changes(&c.instances[0], "lid", ShapeOp::Split, &lid(&ev), &ev.machine.bed).unwrap();
        let (_, built) = m.apply(&mut e, "split", m.head(), &split, None).unwrap();
        let (_, ev) = built.unwrap();
        let script = m.source(FILE).unwrap();
        assert!(script.contains("let lid = m.split(lid_whole, {center: [") && script.contains("m.subtract(lid_base, [cut1])") && !script.contains("cut_plane"), "{script}");
        assert_eq!(kept, "t/lid[1]");
        let on_plate = |n: &str| ev.plate.parts.iter().find(|p| p.object == format!("t/{n}")).unwrap_or_else(|| panic!("{n} is on the plate: {script}")).bounds();
        let (kept, other) = (on_plate("lid[1]"), on_plate("lid[0]"));
        // the slab took the left 15%: the kept side stands on its cut 85% of the lid's 30mm tall
        assert!((kept.size()[2] - 25.5).abs() < 0.2 && (other.size()[2] - 4.5).abs() < 0.2, "{kept:?} {other:?}: {script}");
        let apart = (0..2).any(|i| kept.max[i] <= other.min[i] || other.max[i] <= kept.min[i]);
        assert!(apart, "the halves stand apart: {kept:?} {other:?}");
    }

    /// The person's plate: a shelf bracket and a vase that do not print beside a cube that does.
    /// An area drawn on the vase, beside the cube or far off goes to the cube, the one part whose
    /// trees it can keep out, and lands in its script; on a plate where nothing prints it goes
    /// nowhere and says why.
    #[test]
    fn a_drawn_area_goes_to_the_printing_part_it_is_for() {
        let script = |w: f64| format!("{}let body = m.box({{size: [{w}mm 20mm {w}mm]}})\nobject(body)\n", fab_model::program::EMPTY);
        let project = |cube_prints: bool| format!("use mod.fab.*\n\nscript(\"bracket.cad.splash\")\nscript(\"vase.cad.splash\")\nscript(\"cube.cad.splash\")\n\nplace(\"bracket\", object: \"body\", at: [150mm, 165mm], name: \"Shelf bracket\", print: false)\nplace(\"vase\", object: \"body\", at: [222mm, 165mm], name: \"Bud vase\", print: false)\nplace(\"cube\", object: \"body\", at: [80mm, 165mm], scale: 1.6, name: \"Lattice cube\"{})\n", if cube_prints { "" } else { ", print: false" });
        let scripts = || std::collections::BTreeMap::from([("bracket.cad.splash".to_string(), script(40.0)), ("vase.cad.splash".to_string(), script(30.0)), ("cube.cad.splash".to_string(), script(27.0))]);
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let mut m = FabModel::load("p", &project(true), scripts()).unwrap();
        let (c, ev) = m.evaluate(&mut e, None).unwrap();
        assert_eq!(ev.plate.parts.iter().map(|p| p.object.as_str()).collect::<Vec<_>>(), ["cube/body"], "only the cube prints");
        let on_vase = BedArea::Rect { min: [215.0, 160.0], max: [230.0, 170.0] };
        let beside = BedArea::Rect { min: [100.0, 150.0], max: [112.0, 180.0] };
        let far = BedArea::Disc { centre: [300.0, 300.0], radius: 5.0 };
        for area in [on_vase, beside, far] {
            let (changes, solid, name) = no_trees_on_plate(&c, &ev, area).unwrap();
            assert_eq!((solid.as_str(), name.as_str()), ("cube/no_trees1", "Lattice cube"), "{area:?}");
            assert!(matches!(&changes[..], [Change::Script { file, .. }] if file == "cube.cad.splash"), "{area:?}");
        }
        let (changes, ..) = no_trees_on_plate(&c, &ev, beside).unwrap();
        let (_, built) = m.apply(&mut e, "no trees", m.head(), &changes, None).unwrap();
        let (_, ev) = built.unwrap();
        assert!(m.source("cube.cad.splash").unwrap().ends_with("})\nm.no_trees(no_trees1)\n"), "in the cube's script");
        assert!(ev.plate.parts[0].paint.as_ref().and_then(|p| p.no_trees.as_ref()).is_some(), "the cube keeps its trees out of it");
        let m = FabModel::load("p", &project(false), scripts()).unwrap();
        let (c, ev) = m.evaluate(&mut e, None).unwrap();
        let why = no_trees_on_plate(&c, &ev, beside).unwrap_err();
        assert!(why.contains("no part on the plate prints"), "{why}");
    }

    /// A rectangle drawn on the bed beside a scaled part becomes a box of the part's script, in
    /// its model frame, that `m.no_trees` marks: on the plate it stands on the drawn rectangle
    /// up to [`NO_TREES_HEIGHT`], the tree lists it as "No trees here", and the tree's delete
    /// takes it and its statement out again, one undo step each.
    #[test]
    fn a_drawn_no_trees_area_lands_in_the_script_and_the_tree_deletes_it() {
        let source = format!("{}let lid = m.box({{size: [30mm 4mm 30mm]}})\nobject(lid)\n", fab_model::program::EMPTY);
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let mut m = FabModel::with_script("t", FILE, &source);
        let place = [Change::Place { instance: "t".into(), object: "lid".into(), arg: PlaceArg::Scale([1.5; 3]) }];
        m.apply(&mut e, "scale", m.head(), &place, None).unwrap();
        let (c, ev) = m.evaluate(&mut e, None).unwrap();
        let i = ev.plate.placed_parts().position(|p| p.object == "t/lid").unwrap();
        let on = Placed { frame: *ev.placed_frame(i).unwrap(), bounds: ev.plate.placed_part(i).unwrap().bounds() };
        let b = on.bounds;
        let (min, max) = ([b.max[0] + 2.0, b.min[1]], [b.max[0] + 12.0, b.min[1] + 6.0]);
        let (changes, solid) = no_trees(&c.instances[0], &on, BedArea::Rect { min, max });
        assert_eq!(solid, "t/no_trees1");
        let (_, built) = m.apply(&mut e, "no trees", m.head(), &changes, None).unwrap();
        let (c, ev) = built.unwrap();
        let script = m.source(FILE).unwrap().to_string();
        assert!(script.starts_with(&source) && script.contains("\nlet no_trees1 = m.box({size: [") && script.ends_with("})\nm.no_trees(no_trees1)\n"), "{script}");
        // Where it holds on the plate: inside the drawn rectangle under its height, nowhere else.
        let region = ev.plate.parts[i].paint.as_ref().and_then(|p| p.no_trees.clone()).expect("the part keeps its trees out of the area");
        let mid = [0.5 * (min[0] + max[0]), 0.5 * (min[1] + max[1])];
        assert!((region.at)([mid[0], mid[1], 0.5 * NO_TREES_HEIGHT]) < -0.5, "inside the area");
        for (q, what) in [([max[0] + 0.3, mid[1], 1.0], "past its side"), ([mid[0], max[1] + 0.3, 1.0], "past its end"), ([mid[0], mid[1], NO_TREES_HEIGHT + 0.3], "over its top")] {
            assert!((region.at)(q) > 0.0, "{what}");
        }
        assert_eq!(ev.plate.parts[i].paint.as_ref().unwrap().key, 0, "the slice reads none of it");
        let rows = crate::code_edit::rows(&c.instances[0]);
        let row = rows.iter().find(|r| r.id == "no_trees1").expect("a row");
        assert_eq!((row.detail.as_str(), row.depth), (fab::ui::code::NO_TREES, 0));
        let state = fab::ui::code::SourceState { file: FILE.into(), text: std::sync::Arc::new(script.clone()), features: rows, ..Default::default() };
        let (_, built) = m.apply(&mut e, "remove", m.head(), &crate::code_edit::removed(&state, "no_trees1").unwrap(), None).unwrap();
        let (_, ev) = built.unwrap();
        assert_eq!(m.source(FILE).unwrap(), source, "the area and its statement go");
        assert!(ev.plate.parts[i].paint.is_none(), "no area left");
    }

    /// A rectangle drawn on the shown layer over a scaled cube becomes a box of the cube's script,
    /// in its model frame, with `m.settings` and a solid infill to start: on the plate it stands
    /// about the layer, as tall as the rectangle's shorter side, and the part prints with the
    /// region's setting inside it. The tree lists it as "Print settings here"; its panel rows
    /// write into its statement (an unwritten setting goes in, a written one is rewritten); a
    /// disc gives a sphere; the tree's delete takes the box and its statement out again.
    #[test]
    fn a_drawn_settings_volume_lands_in_the_script_and_its_rows_edit_it() {
        let source = format!("{}let body = m.box({{size: [20mm 20mm 20mm]}})\nobject(body)\n", fab_model::program::EMPTY);
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        let mut m = FabModel::with_script("t", FILE, &source);
        let place = [Change::Place { instance: "t".into(), object: "body".into(), arg: PlaceArg::Scale([1.5; 3]) }];
        m.apply(&mut e, "scale", m.head(), &place, None).unwrap();
        let (c, ev) = m.evaluate(&mut e, None).unwrap();
        let i = ev.plate.placed_parts().position(|p| p.object == "t/body").unwrap();
        let on = Placed { frame: *ev.placed_frame(i).unwrap(), bounds: ev.plate.placed_part(i).unwrap().bounds() };
        let b = on.bounds;
        let mid = [0.5 * (b.min[0] + b.max[0]), 0.5 * (b.min[1] + b.max[1])];
        let (min, max) = ([mid[0] - 3.0, mid[1] - 4.0], [mid[0] + 3.0, mid[1] + 4.0]);
        let (changes, solid) = settings_here(&c.instances[0], &on, BedArea::Rect { min, max }, 12.0);
        assert_eq!(solid, "t/settings1");
        let (_, built) = m.apply(&mut e, "settings here", m.head(), &changes, None).unwrap();
        let (c, ev) = built.unwrap();
        let script = m.source(FILE).unwrap().to_string();
        assert!(script.starts_with(&source) && script.contains("\nlet settings1 = m.box({size: [") && script.ends_with("})\nm.settings(settings1, {sparse_infill_density: \"100%\"})\n"), "{script}");
        // Where it holds on the plate: inside the rectangle from 9 to 15 mm, nowhere else.
        let paint = ev.plate.parts[i].paint.clone().expect("the part paints its region");
        let [region] = &paint.settings[..] else { panic!("one region: {:?}", paint.settings.len()) };
        assert_eq!(region.values, [("sparse_infill_density".to_string(), "100%".to_string())]);
        assert!((region.region.at)([mid[0], mid[1], 12.0]) < -0.5, "inside the volume");
        for (q, what) in [([max[0] + 0.3, mid[1], 12.0], "past its side"), ([mid[0], max[1] + 0.3, 12.0], "past its end"), ([mid[0], mid[1], 15.3], "over its top"), ([mid[0], mid[1], 8.7], "under its foot")] {
            assert!((region.region.at)(q) > 0.0, "{what}");
        }
        assert_ne!(paint.key, 0, "the slice reads it");
        let rows = crate::code_edit::rows(&c.instances[0]);
        let row = rows.iter().find(|r| r.id == "settings1").expect("a row");
        assert_eq!((row.detail.as_str(), row.depth), (fab::ui::code::SETTINGS_HERE, 0));
        // Its panel rows: the density as written, the wall loops not written, beside its size.
        let parts = fab_model::params::model(&c, &ev);
        let rows_of = |parts: &[fab_model::params::PartParams], k: &str| parts.iter().flat_map(|pp| pp.params.iter()).find(|p| p.key == format!("t/settings1#settings.{k}")).cloned();
        let density = rows_of(&parts, "sparse_infill_density").expect("a density row");
        assert_eq!((density.value.clone(), density.site.is_empty()), (fab_model::params::PValue::Num(1.0), false));
        let walls = rows_of(&parts, "wall_loops").expect("a wall loops row");
        assert!(walls.site.is_empty() && walls.group == density.group, "{walls:?}");
        assert!(parts.iter().flat_map(|pp| pp.params.iter()).any(|p| p.key.starts_with("t/settings1.size") && p.group == density.group), "beside its size");
        let walls_to = fab_model::params::edit(&c, &parts, &walls.key, &fab_model::params::PValue::Num(4.0)).unwrap();
        let (_, built) = m.apply(&mut e, "walls", m.head(), &[walls_to], None).unwrap();
        let (c, ev) = built.unwrap();
        assert!(m.source(FILE).unwrap().ends_with("m.settings(settings1, {sparse_infill_density: \"100%\" wall_loops: 4})\n"), "{}", m.source(FILE).unwrap());
        let parts = fab_model::params::model(&c, &ev);
        let density = rows_of(&parts, "sparse_infill_density").unwrap();
        let half = fab_model::params::edit(&c, &parts, &density.key, &fab_model::params::PValue::Num(0.5)).unwrap();
        let (_, built) = m.apply(&mut e, "density", m.head(), &[half], None).unwrap();
        let (c, ev) = built.unwrap();
        assert!(m.source(FILE).unwrap().ends_with("m.settings(settings1, {sparse_infill_density: \"50%\" wall_loops: 4})\n"), "{}", m.source(FILE).unwrap());
        let values = &ev.plate.parts[i].paint.as_ref().unwrap().settings[0].values;
        assert_eq!(values, &[("sparse_infill_density".to_string(), "50%".to_string()), ("wall_loops".to_string(), "4".to_string())]);
        let parts = fab_model::params::model(&c, &ev);
        let too_much = fab_model::params::edit(&c, &parts, &density.key, &fab_model::params::PValue::Num(1.5));
        assert!(too_much.is_err(), "150% is refused: {too_much:?}");
        let state = fab::ui::code::SourceState { file: FILE.into(), text: std::sync::Arc::new(m.source(FILE).unwrap().to_string()), features: crate::code_edit::rows(&c.instances[0]), ..Default::default() };
        let (_, built) = m.apply(&mut e, "remove", m.head(), &crate::code_edit::removed(&state, "settings1").unwrap(), None).unwrap();
        let (c, ev) = built.unwrap();
        assert_eq!(m.source(FILE).unwrap(), source, "the volume and its statement go");
        assert!(ev.plate.parts[i].paint.is_none(), "no region left");
        let (changes, _) = settings_here(&c.instances[0], &on, BedArea::Disc { centre: mid, radius: 3.0 }, 12.0);
        let [Change::Script { edit: Edit::Splice { text, .. }, .. }] = &changes[..] else { panic!("{changes:?}") };
        assert!(text.starts_with("let settings1 = m.sphere({radius: 2mm at: ["), "a sphere in the 1.5x frame: {text}");
    }
}
