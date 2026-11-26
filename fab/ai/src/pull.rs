//! `fab.pull`: a picked face of an exact part moved along its normal by one number, the face's
//! travel. The view's Pull drags it at a handle on the face; the AI gives it. Both write the same
//! edit: the written values that make that face, each moved as far as the face goes:
//!
//! - a box's face: its size on that axis, and its centre (`at`) by half, so the opposite face
//!   stays (`at` written where the box had none);
//! - a cylinder's top or bottom: its height and its centre by half; its wall: its radius (and a
//!   cone's top radius);
//! - a sphere's wall: its radius; an extrude's top: its height (twice the travel when it is
//!   pulled both ways, its bottom then moving as far).
//!
//! A solid cut away from another (a subtract's second operand) has its faces turned inside
//! out: pulling such a face out of the material makes it smaller (a bore's wall pulled out is
//! its radius made smaller).
//!
//! A value written as a number is set where it is, a declared parameter set as that parameter;
//! a value worked out from others is refused, naming what it is made of. A flat face no written
//! value moves on its own (a side of an extrude, a face of a boolean's cut) is pulled by a step
//! of its own: the part's body becomes `m.brep.pull(body, {face: <face> by: d})` right after
//! what it was (`Edit::After`, the part keeping its name), and once that is written, its `by`.
//! A curved face no value moves is refused. The plan is read from the pick, the exact body and
//! the committed program; nothing is built for it.
use crate::picks::{Kind, Named, Pick};
use crate::round::{mm_arg, site_or_param, unit, written, Plan};
use crate::hoist::Own;
use crate::{inspect, Reply, Session};
use fab_model::params::{Kind as PKind, Linked, PartParams, Template, Writes, AT_VALUE};
use fab_model::program::Instance;
use makepad_csg_graph::edges::Role;
use makepad_model_script::lang::{edit, Solid};
use makepad_script::syntax::{Kind as SKind, Node};

/// The tool's name, for the AI and for the view's own pulls.
pub const TOOL: &str = "pull";

/// What a pick must be for a pull.
const NOTHING: &str = "pick a face of an exact part to pull first (E)";

/// The smallest a pulled size gets (mm).
const LEAST: f64 = 0.1;

/// The farthest a face pulls out in one go (mm), as far as a part on the bed goes.
const MOST: f64 = 500.0;

/// What moves a builder's face along its normal: the fields that size it, each by its rate per mm
/// of travel, and the axis and side of a centred builder's `at`, which moves by half.
struct Drive {
    sizes: Vec<(&'static str, f64)>,
    centre: Option<(usize, f64)>,
}

/// The drive of face `role` of a builder `proto`; none: no written value moves it on its own.
fn drive(proto: &str, role: &str, written: &dyn Fn(&str) -> Option<String>) -> Option<Drive> {
    const SIZE: [&str; 3] = ["size.0", "size.1", "size.2"];
    let sized = |sizes: Vec<(&'static str, f64)>, centre| Some(Drive { sizes, centre });
    match (proto, role) {
        ("BrepBox", role) => {
            // (the faces by axis and side as the kernel names them)
            let (k, side) = match role {
                "left" => (0, -1.0),
                "right" => (0, 1.0),
                "bottom" => (1, -1.0),
                "top" => (1, 1.0),
                "back" => (2, -1.0),
                "front" => (2, 1.0),
                _ => return None,
            };
            sized(vec![(SIZE[k], 1.0)], Some((k, side)))
        }
        ("BrepCylinder", "top") => sized(vec![("height", 1.0)], Some((1, 1.0))),
        ("BrepCylinder", "bottom") => sized(vec![("height", 1.0)], Some((1, -1.0))),
        ("BrepCylinder", "wall") => {
            let mut sizes = vec![("radius", 1.0)];
            if written("top_radius").is_some() {
                sizes.push(("top_radius", 1.0));
            }
            sized(sizes, None)
        }
        ("BrepSphere", "wall") => sized(vec![("radius", 1.0)], None),
        ("BrepExtrude", "top") => sized(vec![("height", if written("both").as_deref() == Some("true") { 2.0 } else { 1.0 })], None),
        _ => None,
    }
}

/// The node of value path `path` (`height`, `size.1`) in a call's fields: an element of a written
/// list by its index.
fn path_node<'a>(syn: &'a makepad_script::syntax::Syntax, fields: &[(String, edit::FieldAt)], path: &str) -> Option<&'a Node> {
    let (field, index) = match path.split_once('.') {
        Some((f, i)) => (f, Some(i.parse::<usize>().ok()?)),
        None => (path, None),
    };
    let node = fields.iter().find(|(f, _)| f == field).and_then(|(_, at)| syn.node(at.loc()))?;
    match (index, &node.kind) {
        (None, _) => Some(node),
        (Some(i), SKind::Array { items, .. }) => items.get(i),
        _ => None,
    }
}

/// A cutter's faces point into its material. Count reversals only between the picked
/// step and the builder that supplied the face, including when another step shares it.
fn material_side(solids: &[Solid], step: usize, origin: usize) -> Option<f64> {
    let mut pending = vec![(step, 1.0)];
    let mut seen = vec![false; solids.len()];
    while let Some((i, sign)) = pending.pop() {
        if i == origin {
            return Some(sign);
        }
        if !seen[i] {
            seen[i] = true;
            let cut = matches!(solids[i].proto, "BrepDifference" | "Difference");
            for (n, &input) in solids[i].inputs.iter().enumerate().rev() {
                pending.push((input, if cut && n > 0 { -sign } else { sign }));
            }
        }
    }
    None
}

impl Session {
    /// The pull of the face picked last among picks `ids` (none: all of them).
    pub fn pull_plan(&mut self, ids: &[String]) -> Result<Plan, String> {
        let picks = self.chosen_picks(ids)?;
        let faces: Vec<_> = picks.iter().filter(|p| p.kind == Kind::Face).collect();
        let p = *faces.last().ok_or(NOTHING)?;
        let Some(Named::Face(name)) = &p.named else { return Err(NOTHING.into()) };
        let b = self.exact_body(&p.on)?.ok_or_else(|| format!("`{}` is not an exact part: pull takes an exact part's face", p.on))?;
        let params = self.params()?;
        let (inst, k) = self.build_on(&faces)?;
        let program = &inst.script.program;
        let step = &program.solids[k];
        if !step.named || step.call.is_some() {
            return Err(format!("`{}` has no `let` of its own at the top level: name its body to pull `{name}`", step.id));
        }
        // (by its copy's own name, where the body holds the part twice)
        let mut own = Own::new(program, b);
        let (face, name) = own.face(name, p.at.map(|(q, _)| q))?;
        let b = &own.body;
        let face = b.faces.iter().find(|f| f.id == face).expect("own.face found it");
        let pull_step = || Self::pull_step(p, &name, face, inst, step, &params);
        if !own.hoists.is_empty() {
            // an inline copy named on the way: its builder is every copy's, so a pull step of its
            // own, once the copy has its `let`
            let hoists = own.hoists.clone();
            let mut plan = pull_step()?;
            plan.template.writes = Writes::Hoisted { hoists, then: Box::new(plan.template.writes) };
            return Ok(plan);
        }
        let named = inspect::face_named(b, face.id, &name).ok_or_else(|| format!("the part has no face `{name}` any more"))?;
        let sid = b.parts.get(&named.origin).cloned().ok_or_else(|| format!("`{name}` comes from no solid of the script"))?;
        let Role::Named(role) = &named.role else {
            return pull_step();
        };
        let role = role.name();
        let i = program.solids.iter().position(|s| s.id == sid).ok_or_else(|| format!("no solid '{sid}'"))?;
        let solid = &program.solids[i];
        let proto = makepad_model_script::lang::protos::proto(solid.proto).ok_or_else(|| format!("`{sid}` has no builder"))?;
        let call = edit::part_call(program, solid)?;
        let syn = &program.names.syntax;
        let fields = edit::call_fields(syn, &call, proto);
        let src = inst.script.source();
        let node_text = |path: &str| path_node(syn, &fields, path).map(|n| crate::round::text(src, n).trim().to_string());
        let Some(d) = drive(solid.proto, role, &node_text) else {
            return pull_step();
        };
        if d.centre.is_some() && node_text("rotate").is_some() {
            return Err(format!("`{sid}` is turned (`rotate:`): its size and `at` move along different axes, so pulling `{name}` would move the faces beside it"));
        }
        let Some(flip) = material_side(&program.solids, k, i) else { return pull_step() };
        let mut links = Vec::new();
        let (mut least, mut most) = (f64::MIN, MOST);
        let mut sized: Option<Writes> = None;
        for (path, rate) in &d.sizes {
            let Some((writes, base)) = site_or_param(&params, inst, &sid, path, path_node(syn, &fields, path))? else {
                return Err(format!("`{sid}`'s {path} is not written in its call: write it to pull `{name}`"));
            };
            // (no size below the least)
            let rate = rate * flip;
            if rate > 0.0 {
                least = least.max(-(base - LEAST) / rate);
            } else {
                most = most.min((base - LEAST) / -rate);
            }
            sized.get_or_insert(writes.clone());
            links.push(Linked { writes, base, rate });
        }
        if let Some((k, side)) = d.centre.map(|(k, side)| (k, side * flip)) {
            let path = format!("at.{k}");
            match fields.iter().find(|(f, _)| f == "at").and_then(|(_, at)| syn.node(at.loc())) {
                // centred on the origin: `at` written, the face's axis moving
                None => {
                    let mut point = ["0"; 3];
                    point[k] = AT_VALUE;
                    links.push(Linked { writes: Writes::Field { solid: sid.clone(), field: "at".into(), code: format!("[{}]", point.join(", ")) }, base: 0.0, rate: side * 0.5 });
                }
                Some(at) => {
                    let at_k = path_node(syn, &fields, &path).ok_or_else(|| format!("`{sid}`'s at is not written as [x y z] (`{}`): pull its size instead", crate::round::text(src, at).trim()))?;
                    match site_or_param(&params, inst, &sid, &path, Some(at_k)) {
                        Ok(Some((writes, base))) if Some(&writes) != sized.as_ref() => links.push(Linked { writes, base, rate: side * 0.5 }),
                        Ok(_) => return Err(format!("`{sid}`'s size and at are one parameter: pulling `{name}` would move both faces")),
                        // worked out from the parameter that sizes it: the script keeps the face
                        // across from it where it is already
                        Err(_) if matches!(&sized, Some(Writes::Param { name: n, .. }) if crate::round::text(src, at_k).split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).any(|w| w == n)) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        // the handle: where the face was pointed at (on its plane where it is flat), out along
        // its normal
        let (q, pointed) = p.at.unwrap_or((face.at, face.normal.unwrap_or([0.0, 1.0, 0.0])));
        let (at, dir) = match face.normal.and_then(unit) {
            Some(n) => {
                let off = (0..3).map(|k| (face.at[k] - q[k]) * n[k]).sum::<f64>();
                ([q[0] + n[0] * off, q[1] + n[1] * off, q[2] + n[2] * off], n)
            }
            None => (q, unit(pointed).unwrap_or([0.0, 1.0, 0.0])),
        };
        let what = d.sizes.iter().map(|(path, _)| *path).chain(d.centre.map(|_| "at")).collect::<Vec<_>>().join(" and ");
        Ok(Plan {
            object: p.object.clone(),
            label: "Pull".into(),
            says: format!("pulls `{name}` out (in, below 0): `{sid}`'s {what}"),
            template: Template { file: inst.script.file.clone(), kind: PKind::Length, writes: Writes::Many(links) },
            then: None,
            value: 0.0,
            min: least.min(0.0),
            max: most.max(0.0),
            step: 0.1,
            at,
            dir,
            picks: vec![p.id.clone()],
        })
    }

    /// A flat face no written value moves: a pull step of its own on the picked step (`by` the
    /// face's travel), the step keeping its name; once written, its `by`.
    fn pull_step(p: &Pick, name: &str, face: &makepad_csg_graph::BrepEntity, inst: &Instance, solid: &Solid, params: &[PartParams]) -> Result<Plan, String> {
        let Some(normal) = face.normal else {
            return Err(format!("no written value moves `{name}` on its own, and only a flat face pulls by a step of its own: `{name}` is curved"));
        };
        let sid = solid.id.clone();
        // the face the part's own pull step moves: that step's `by`, not a second pull
        let mut again = None;
        if solid.proto == "BrepPull" {
            let program = &inst.script.program;
            let proto = makepad_model_script::lang::protos::proto(solid.proto).ok_or_else(|| format!("`{sid}` has no builder"))?;
            let call = edit::part_call(program, solid)?;
            let syn = &program.names.syntax;
            let fields = edit::call_fields(syn, &call, proto);
            let src = inst.script.source();
            if path_node(syn, &fields, "face").is_some_and(|n| crate::round::text(src, n).trim() == name) {
                again = site_or_param(params, inst, &sid, "by", path_node(syn, &fields, "by"))?;
            }
        }
        let at = p.at.map_or(face.at, |(q, _)| q);
        let dir = unit(normal).unwrap_or([0.0, 1.0, 0.0]);
        let file = inst.script.file.clone();
        if let Some((writes, base)) = again {
            return Ok(Plan {
                object: p.object.clone(),
                label: "Pull".into(),
                says: format!("pulls `{name}` out (in, below 0): `{sid}`'s by"),
                template: Template { file, kind: PKind::Length, writes: Writes::Many(vec![Linked { writes, base, rate: 1.0 }]) },
                then: None,
                value: 0.0,
                min: -MOST,
                max: MOST,
                step: 0.1,
                at,
                dir,
                picks: vec![p.id.clone()],
            });
        }
        Ok(Plan {
            object: p.object.clone(),
            label: "Pull".into(),
            says: format!("pulls `{name}` out (in, below 0): `{sid}` becomes its pull"),
            template: Template { file: file.clone(), kind: PKind::Length, writes: Writes::After { solid: sid.clone(), code: format!("m.brep.pull({sid}, {{face: {name} by: {AT_VALUE}}})") } },
            then: Some(Template { file, kind: PKind::Length, writes: Writes::Set { site: format!("{sid}.by") } }),
            value: 0.0,
            min: -MOST,
            max: MOST,
            step: 0.1,
            at,
            dir,
            picks: vec![p.id.clone()],
        })
    }

    /// The AI's pull: the face picked last among `picks` (default all) moved `by` mm along its
    /// normal (out; in below 0), committed as one undo step; without `by`, what it would write.
    pub(crate) fn pull_tool(&mut self, args: &makepad_model::json::Value) -> Reply {
        use crate::out::{arr, n, obj, s};
        let ids = crate::round::pick_ids(args);
        let plan = match self.pull_plan(&ids) {
            Ok(plan) => plan,
            Err(e) => return Reply::err(e),
        };
        let Some(by) = args.get("by").and_then(mm_arg) else {
            let writes = written(&plan.template, "<by>");
            return Reply::ok(obj(vec![("label", s(&plan.label)), ("says", s(&plan.says)), ("object", s(&plan.object)), ("min", n(plan.min)), ("max", n(plan.max)), ("writes", s(writes)), ("picks", arr(plan.picks.iter().map(s)))]));
        };
        if !(plan.min..=plan.max).contains(&by) {
            return Reply::err(format!("pull takes {:.1} to {} mm here", plan.min, plan.max));
        }
        self.commit(TOOL, plan.template.changes(by))
    }
}
