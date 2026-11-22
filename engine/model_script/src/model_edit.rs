//! Face-level edits, soft bodies and rig controls in builder form.
//!
//! Faces and vertices are reached through selections: `let top = m.select(body, {box: [lo, hi]})`
//! queries the part's mesh when the program builds, and the edit verbs take that
//! selection (`m.extrude_faces(top, vec3(0, 0.2, 0))`). Joints are named by strings;
//! the build resolves them against the skeleton as it stands at that point, so a
//! soft body's attachment frames and `m.joint(..)` children can be posed and keyed.
//! Angles are degrees, like every `rot:` in the builder.

use super::*;
use makepad_model::mesh::{Deformation, SculptBrush, SculptKind};
use makepad_model::{
    ClipEvent, ClipOptions, ConstraintKind, Interpolation, MorphKey, Pose as RigPose, RigConstraint, RigOperation,
    SoftBodyAttachmentRequest, SoftBodyBind, SoftBodySettings, WeightEdit,
};
use std::collections::{BTreeMap, BTreeSet};

/// Option keys a soft body's settings take, with the generic defaults and the
/// `yarn_ball` preset (a firm wound-textile response that keeps its silhouette).
const SOFT_KEYS: &[&str] = &["preset", "deform", "attach", "mass", "edge_compliance", "volume_compliance", "pose_compliance", "damping", "gravity", "contact_radius", "friction", "substeps", "iterations", "max_speed", "max_displacement"];

/// A program's joint names: rig controls carry indices into it.
#[derive(Default)]
struct Joints(Vec<String>);
impl Joints {
    fn at(&mut self, name: &str) -> u32 {
        match self.0.iter().position(|n| n == name) { Some(i) => i as u32, None => { self.0.push(name.into()); self.0.len() as u32 - 1 } }
    }
}

/// Two numbers: a list `[a, b]` or a vec2.
fn pair(vm: &mut ScriptVm, v: ScriptValue) -> [f64; 2] {
    if list_len(vm, v) >= 2 { let (a, b) = (list_value(vm, v, 0), list_value(vm, v, 1)); return [value_f32(vm, a) as f64, value_f32(vm, b) as f64]; }
    value_v2(vm, v)
}

fn axis_of(vm: &mut ScriptVm, v: ScriptValue) -> Option<usize> {
    if let Some(n) = v.as_number() { return (0. ..=2.).contains(&n).then_some(n as usize); }
    match value_string(vm, v).as_str() { "x" => Some(0), "y" => Some(1), "z" => Some(2), _ => None }
}

impl Recorder<'_, '_> {
    /// The selection at argument `i`: (part, group).
    fn selection(&mut self, i: usize) -> Option<(String, String)> {
        let v = self.arg(i);
        let found = v.as_handle().and_then(|h| self.vm.bx.heap_ref().handle_ref::<PartRef>(h)).and_then(|p| p.sel.clone().map(|s| (p.name.clone(), s)));
        if found.is_none() { self.error = Some(format!("{} needs a selection (the value m.select(..) returned)", self.method)); }
        found
    }
    /// Run `operation` on the selection at argument 0, filling its `element` field.
    pub(super) fn on_selection(&mut self, element: &str, operation: Operation) -> ScriptValue {
        let Some((object, name)) = self.selection(0) else { return NIL };
        self.op(Operation::Selection(SelectionOperation::Use { object, name, element: element.into(), operation: Box::new(operation) }));
        NIL
    }
    fn mesh_edit(&mut self, element: &str, action: MeshAction) -> ScriptValue {
        self.on_selection(element, Operation::MeshEditing(MeshEditingOperation { object: String::new(), action }))
    }
    fn joint_name(&mut self, v: ScriptValue, what: &str) -> Option<String> {
        if v.is_string_like() { let s = value_string(self.vm, v); if !s.is_empty() { return Some(s); } }
        self.error = Some(format!("{} {what} needs a joint name (a string)", self.method));
        None
    }
    fn rig(&mut self, joints: Joints, op: RigOperation) -> ScriptValue { self.push(ProgramOp::Rig { joints: joints.0, op }); NIL }
    /// `{pos, rot, scale}` as a local transform (rot in degrees).
    fn transform(&mut self, o: Option<ScriptObject>) -> Transform {
        let scale = { let v = opt(self.vm, o, id!(scale)); if v.is_nil() { [1.; 3] } else if let Some(s) = v.as_number() { [s; 3] } else { let p = value_vec3(self.vm, v); [p.x as f64, p.y as f64, p.z as f64] } };
        Transform { translation: opt_v3(self.vm, o, id!(pos)).unwrap_or([0.; 3]), rotation: opt_v3(self.vm, o, id!(rot)).map_or([0., 0., 0., 1.], quat_euler_deg), scale }
    }
    fn axis(&mut self, o: Option<ScriptObject>, default: usize) -> Option<usize> {
        let v = opt(self.vm, o, id!(axis));
        if v.is_nil() { return Some(default); }
        let a = axis_of(self.vm, v);
        if a.is_none() { self.error = Some(format!("{}: axis is \"x\", \"y\" or \"z\"", self.method)); }
        a
    }
    /// The axis span a deformation acts over: `range: [lo, hi]` along its axis.
    fn range(&mut self, o: Option<ScriptObject>) -> Option<[f64; 2]> {
        let v = opt(self.vm, o, id!(range));
        if v.is_nil() { self.error = Some(format!("{} needs {{range: [lo, hi]}} along its axis", self.method)); return None; }
        Some(pair(self.vm, v))
    }
    fn parts(&mut self, v: ScriptValue, what: &str) -> Option<Vec<String>> {
        let n = list_len(self.vm, v);
        let mut out = Vec::with_capacity(n);
        for i in 0..n { let x = list_value(self.vm, v, i); out.push(self.ref_name(x, false, what)?); }
        Some(out)
    }

    /// Verbs of this module; `None` when `m` is none of them.
    pub(super) fn edit_verb(&mut self, m: &str) -> Option<ScriptValue> {
        Some(match m {
            "select" => self.m_select(),
            "extrude_faces" => {
                let Some(offset) = ({ let v = self.arg(1); (!v.is_nil()).then(|| { let p = value_vec3(self.vm, v); [p.x as f64, p.y as f64, p.z as f64] }) }) else { return Some(self.fail("extrude_faces(sel, vec3 offset)")) };
                self.mesh_edit("faces", MeshAction::Region { faces: Vec::new(), offset })
            }
            "inset" => { let d = self.num_arg(1, 0.05); self.on_selection("face", Operation::Inset { object: String::new(), face: makepad_model::mesh::FaceId(0), distance: d }) }
            "delete_faces" => self.on_selection("faces", Operation::DeleteFaces { object: String::new(), faces: Vec::new() }),
            "flip_faces" => self.mesh_edit("faces", MeshAction::FlipFaces { faces: Vec::new() }),
            "move_verts" => {
                let o = self.opts(1, &["pos", "rot", "scale", "pivot"]);
                let t = self.transform(o);
                let pivot = opt_v3(self.vm, o, id!(pivot)).unwrap_or([0.; 3]);
                let mut matrix = rs_matrix(t.rotation, t.scale);
                for r in 0..3 { matrix[r][3] = t.translation[r] + pivot[r] - (0..3).map(|c| matrix[r][c] * pivot[c]).sum::<f64>(); }
                self.on_selection("vertices", Operation::Transform { object: String::new(), vertices: Vec::new(), matrix })
            }
            "smooth_verts" => {
                let o = self.opts(1, &["iterations", "factor", "boundary"]);
                let iterations = opt_num(self.vm, o, id!(iterations)).unwrap_or(1.).clamp(1., 64.) as u32;
                let factor = opt_num(self.vm, o, id!(factor)).unwrap_or(0.5);
                let preserve_boundary = o.map_or(true, |o| opt_bool(self.vm, o, id!(boundary), true));
                self.on_selection("vertices", Operation::Smooth { object: String::new(), vertices: Vec::new(), iterations, factor, preserve_boundary })
            }
            "weld" => { let distance = self.num_arg(1, 0.001); self.on_selection("vertices", Operation::Weld { object: String::new(), vertices: Vec::new(), distance }) }
            "bend" | "twist" | "taper" => {
                let o = self.opts(1, &["axis", "angle", "range", "scales"]);
                let Some(axis) = self.axis(o, 1) else { return Some(NIL) };
                let Some(range) = self.range(o) else { return Some(NIL) };
                let deformation = match m {
                    "taper" => { let v = opt(self.vm, o, id!(scales)); Deformation::Taper { axis, scales: if v.is_nil() { [1., 0.5] } else { pair(self.vm, v) }, range } }
                    _ => {
                        let angle = opt_num(self.vm, o, id!(angle)).unwrap_or(45.).to_radians();
                        if m == "bend" { Deformation::Bend { axis, angle, range } } else { Deformation::Twist { axis, angle, range } }
                    }
                };
                self.mesh_edit("vertices", MeshAction::Deform { vertices: Vec::new(), deformation })
            }
            "sculpt" => {
                let o = self.opts(1, &["kind", "center", "normal", "radius", "strength", "max", "symmetry"]);
                let kind = match opt_text(self.vm, o, id!(kind)).as_deref().unwrap_or("inflate") {
                    "inflate" => SculptKind::Inflate, "flatten" => SculptKind::Flatten, "crease" => SculptKind::Crease,
                    other => return Some(self.fail(format!("sculpt kind '{other}' (inflate, flatten, crease)"))),
                };
                let Some(center) = opt_v3(self.vm, o, id!(center)) else { return Some(self.fail("sculpt needs {center}")) };
                let normal = opt_v3(self.vm, o, id!(normal)).unwrap_or([0., 1., 0.]);
                let radius = opt_num(self.vm, o, id!(radius)).unwrap_or(0.2);
                let strength = opt_num(self.vm, o, id!(strength)).unwrap_or(0.5);
                let max_displacement = opt_num(self.vm, o, id!(max)).unwrap_or(radius * 0.5);
                let symmetry = { let v = opt(self.vm, o, id!(symmetry)); if v.is_nil() { 0 } else { let s = value_string(self.vm, v); (s.contains('x') as u8) | ((s.contains('y') as u8) << 1) | ((s.contains('z') as u8) << 2) } };
                let brush = SculptBrush { kind, center, normal, radius, strength, max_displacement, masks: Vec::new(), symmetry };
                self.mesh_edit("vertices", MeshAction::Sculpt { vertices: Vec::new(), brush })
            }
            "shrinkwrap" => {
                let Some(reference) = self.part(1) else { return Some(NIL) };
                let o = self.opts(2, &["distance", "offset"]);
                let max_distance = opt_num(self.vm, o, id!(distance)).unwrap_or(1.);
                let offset = opt_num(self.vm, o, id!(offset)).unwrap_or(0.);
                self.mesh_edit("vertices", MeshAction::Shrinkwrap { vertices: Vec::new(), reference, max_distance, offset })
            }
            "lattice" => {
                let o = self.opts(1, &["min", "max", "divisions", "moves"]);
                let (Some(lo), Some(hi)) = (opt_v3(self.vm, o, id!(min)), opt_v3(self.vm, o, id!(max))) else { return Some(self.fail("lattice needs {min, max, divisions, moves}")) };
                let d = opt_v3(self.vm, o, id!(divisions)).unwrap_or([2.; 3]);
                let Some(divisions) = self.counts(d) else { return Some(NIL) };
                let moves = opt(self.vm, o, id!(moves));
                let displacements = points3(self.vm, moves);
                self.mesh_edit("vertices", MeshAction::Lattice { vertices: Vec::new(), bounds: [lo, hi], divisions, displacements })
            }
            "uv_box" | "uv_cylinder" => {
                let o = self.opts(1, &["scale", "offset", "axis"]);
                let scale = { let v = opt(self.vm, o, id!(scale)); if v.is_nil() { [1.; 2] } else { pair(self.vm, v) } };
                let offset = { let v = opt(self.vm, o, id!(offset)); if v.is_nil() { [0.; 2] } else { pair(self.vm, v) } };
                if m == "uv_box" { return Some(self.mesh_edit("faces", MeshAction::BoxUv { faces: Vec::new(), scale, offset })); }
                let Some(axis) = self.axis(o, 1) else { return Some(NIL) };
                self.mesh_edit("faces", MeshAction::CylinderUv { faces: Vec::new(), axis, scale, offset })
            }
            "uv_unwrap" => self.mesh_edit("faces", MeshAction::Unwrap { faces: Vec::new() }),
            "uv_pack" => { let padding = self.num(1, id!(padding), 0.01); self.mesh_edit("faces", MeshAction::Pack { faces: Vec::new(), padding }) }
            "uv_relax" => {
                let iterations = self.num(1, id!(iterations), 8.).clamp(1., 128.) as u32;
                let factor = self.num(1, id!(factor), 0.5);
                self.mesh_edit("faces", MeshAction::Relax { faces: Vec::new(), iterations, factor })
            }
            "solidify" | "array" | "decimate" => {
                let Some(name) = self.part(0) else { return Some(NIL) };
                if !self.known(&name) { return Some(NIL); }
                let action = match m {
                    "solidify" => MeshAction::Solidify { thickness: self.num_arg(1, 0.02) },
                    "array" => {
                        let count = self.num_arg(1, 2.);
                        let Some([count]) = self.counts([count]) else { return Some(NIL) };
                        let v = self.arg(2);
                        if v.is_nil() { return Some(self.fail("array(part, count, vec3 offset)")); }
                        let p = value_vec3(self.vm, v);
                        MeshAction::Array { count, offset: [p.x as f64, p.y as f64, p.z as f64] }
                    }
                    _ => MeshAction::Decimate { target_faces: self.num(1, id!(faces), 500.).max(4.) as usize, max_error: self.num(1, id!(error), 0.01) },
                };
                self.edit(&name, action);
                NIL
            }
            "soft_body" => self.m_soft_body(),
            "joint" => {
                let name = self.text(0);
                let p = self.arg(1);
                let parent = if p.is_nil() { String::new() } else { value_string(self.vm, p) };
                if name.is_empty() || (parent.is_empty() && !p.is_nil() && !p.is_string_like()) { return Some(self.fail("joint(name, parent, vec3 offset)")); }
                if parent.is_empty() {
                    let builder = self.b();
                    if builder.rigged || builder.ops.iter().any(|op| matches!(op, ProgramOp::Op(Operation::SoftBody(_)))) {
                        return Some(self.fail("joint(name, parent, vec3 offset): this model already has a skeleton, so name its parent"));
                    }
                    builder.rigged = true;
                }
                let v = self.arg(2);
                let translation = if v.is_nil() { [0.; 3] } else { let p = value_vec3(self.vm, v); [p.x as f64, p.y as f64, p.z as f64] };
                self.push(ProgramOp::Joint { name, parent, translation });
                NIL
            }
            "rest" => {
                let v = self.arg(0);
                let Some(name) = self.joint_name(v, "joint") else { return Some(NIL) };
                let o = self.opts(1, &["pos", "rot", "scale"]);
                let transform = self.transform(o);
                let mut j = Joints::default(); let joint = j.at(&name);
                self.rig(j, RigOperation::Rest { joint, transform })
            }
            "roll" => {
                let v = self.arg(0);
                let Some(name) = self.joint_name(v, "joint") else { return Some(NIL) };
                let angle = self.num_arg(1, 0.).to_radians();
                let mut j = Joints::default(); let joint = j.at(&name);
                self.rig(j, RigOperation::Roll { joint, angle })
            }
            "rename_joint" => {
                let v = self.arg(0);
                let Some(name) = self.joint_name(v, "joint") else { return Some(NIL) };
                let to = self.text(1);
                let mut j = Joints::default(); let joint = j.at(&name);
                self.rig(j, RigOperation::RenameJoint { joint, name: to })
            }
            "reparent" => {
                let v = self.arg(0);
                let Some(name) = self.joint_name(v, "joint") else { return Some(NIL) };
                let mut j = Joints::default(); let joint = j.at(&name);
                let p = self.arg(1);
                let parent = if p.is_nil() { None } else { let Some(p) = self.joint_name(p, "parent") else { return Some(NIL) }; Some(j.at(&p)) };
                self.rig(j, RigOperation::ReparentJoint { joint, parent })
            }
            "mirror_joints" => {
                let o = self.opts(0, &["joints", "names", "axis"]);
                let (jv, nv) = (opt(self.vm, o, id!(joints)), opt(self.vm, o, id!(names)));
                let (n, k) = (list_len(self.vm, jv), list_len(self.vm, nv));
                if n == 0 || n != k { return Some(self.fail("mirror_joints({joints: [..], names: [..], axis}) with one new name per joint")); }
                let Some(axis) = self.axis(o, 0) else { return Some(NIL) };
                let mut j = Joints::default();
                let (mut joints, mut names) = (Vec::new(), BTreeMap::new());
                for i in 0..n {
                    let x = list_value(self.vm, jv, i);
                    let Some(name) = self.joint_name(x, "joints") else { return Some(NIL) };
                    let at = j.at(&name);
                    let to = list_value(self.vm, nv, i);
                    let to = value_string(self.vm, to);
                    joints.push(at); names.insert(at, to);
                }
                self.rig(j, RigOperation::MirrorJoints { joints, axis, names })
            }
            "pose" => {
                let name = self.text(0);
                let rows = self.arg(1);
                let mut j = Joints::default();
                let mut joints = BTreeMap::new();
                for i in 0..list_len(self.vm, rows) {
                    let row = list_value(self.vm, rows, i).as_object();
                    let jv = opt(self.vm, row, id!(joint));
                    let Some(joint) = self.joint_name(jv, "row") else { return Some(NIL) };
                    let t = self.transform(row);
                    if joints.insert(j.at(&joint), t).is_some() { return Some(self.fail(format!("pose '{name}' names joint '{joint}' twice"))); }
                }
                self.rig(j, RigOperation::Pose(RigPose { name, joints }))
            }
            "constraint" => self.m_constraint(),
            "solve_pose" => { let pose = self.text(0); self.rig(Joints::default(), RigOperation::SolvePose { pose }) }
            "ik" => {
                let pose = self.text(0);
                let o = self.opts(1, &["root", "middle", "end", "target", "pole", "clamp"]);
                let mut j = Joints::default();
                let mut idx = |s: &mut Self, key: LiveId, what: &str| -> Option<u32> { let v = opt(s.vm, o, key); s.joint_name(v, what).map(|n| j.at(&n)) };
                let (Some(root), Some(middle), Some(end)) = (idx(self, id!(root), "root"), idx(self, id!(middle), "middle"), idx(self, id!(end), "end")) else { return Some(NIL) };
                let (Some(target), Some(pole)) = (opt_v3(self.vm, o, id!(target)), opt_v3(self.vm, o, id!(pole))) else { return Some(self.fail("ik needs {target, pole} positions")) };
                let clamp_reach = o.is_some_and(|o| opt_bool(self.vm, o, id!(clamp), false));
                self.rig(j, RigOperation::Ik { pose, root, middle, end, target, pole, clamp_reach })
            }
            "bake_clip" => {
                let name = self.text(0);
                let keys = self.arg(1);
                let mut poses = Vec::new();
                for i in 0..list_len(self.vm, keys) {
                    let k = list_value(self.vm, keys, i).as_object();
                    let t = opt_num(self.vm, k, id!(t)).unwrap_or(0.);
                    let Some(p) = opt_text(self.vm, k, id!(pose)) else { return Some(self.fail("bake_clip keys are [{t, pose: \"name\"}]")) };
                    poses.push((t, p));
                }
                let fps = self.num(2, id!(fps), 30.);
                let solve_constraints = self.flag(2, id!(solve), true);
                self.rig(Joints::default(), RigOperation::BakeClip { name, poses, fps, solve_constraints })
            }
            "blend_clip" => {
                let (name, a, b) = (self.text(0), self.text(1), self.text(2));
                let weight = self.num_arg(3, 0.5);
                let fps = self.num(4, id!(fps), 30.);
                self.rig(Joints::default(), RigOperation::BlendClip { name, a, b, weight, fps })
            }
            "retarget_clip" => {
                let (name, source) = (self.text(0), self.text(1));
                let rows = self.arg(2);
                let mut j = Joints::default();
                let mut joints = BTreeMap::new();
                for i in 0..list_len(self.vm, rows) {
                    let row = list_value(self.vm, rows, i).as_object();
                    let (f, t) = (opt(self.vm, row, id!(from)), opt(self.vm, row, id!(to)));
                    let (Some(f), Some(t)) = (self.joint_name(f, "from"), self.joint_name(t, "to")) else { return Some(NIL) };
                    joints.insert(j.at(&f), j.at(&t));
                }
                let translation_scale = self.num(3, id!(scale), 1.);
                self.rig(j, RigOperation::RetargetClip { name, source, joints, translation_scale })
            }
            "clip_options" => self.m_clip_options(),
            "keys" => self.m_keys(),
            "weights" => self.m_weights(),
            "morph" => {
                let name = self.text(0);
                let (Some(object), Some(target)) = (self.part(1), self.part(2)) else { return Some(NIL) };
                let weight = self.num_arg(3, 0.);
                self.rig(Joints::default(), RigOperation::CaptureMorph { name, object, target, weight })
            }
            "morph_weight" => { let name = self.text(0); let weight = self.num_arg(1, 0.); self.rig(Joints::default(), RigOperation::MorphWeight { name, weight }) }
            _ => return None,
        })
    }
    fn num_arg(&mut self, i: usize, default: f64) -> f64 { let v = self.arg(i); if v.is_nil() { default } else { value_f32(self.vm, v) as f64 } }

    /// `m.select(part, {box: [lo, hi] | material: mat | ray: [origin, dir] | all: true,
    /// world, faces: [min, max]})`: the part's vertices and faces the query finds when the
    /// program builds. A box takes the faces whose corners all lie in it. `faces` bounds
    /// how many faces must match (default: at least one vertex).
    fn m_select(&mut self) -> ScriptValue {
        let Some(object) = self.part(0) else { return NIL };
        if !self.known(&object) { return NIL; }
        let o = self.opts(1, &["box", "material", "ray", "all", "world", "faces"]);
        let world = o.is_some_and(|o| opt_bool(self.vm, o, id!(world), false));
        let (bx, ray) = (opt(self.vm, o, LiveId::from_str("box")), opt(self.vm, o, id!(ray)));
        let query = if !bx.is_nil() {
            let p = points3(self.vm, bx);
            if p.len() != 2 { return self.fail("select box is [lo, hi]"); }
            SelectionQuery::Bounds { min: p[0], max: p[1], world }
        } else if !ray.is_nil() {
            let p = points3(self.vm, ray);
            if p.len() != 2 { return self.fail("select ray is [origin, direction]"); }
            SelectionQuery::Ray { origin: p[0], direction: p[1], world }
        } else if let Some(material) = self.material(o, id!(material)) {
            SelectionQuery::Material(material)
        } else if o.is_some_and(|o| opt_bool(self.vm, o, id!(all), false)) {
            SelectionQuery::All
        } else {
            return self.fail("select needs {box: [lo, hi]}, {ray: [origin, dir]}, {material} or {all: true}");
        };
        let faces = opt(self.vm, o, id!(faces));
        let (element, min, max) = if faces.is_nil() { ("vertices", 1, 2_000_000) } else { let r = pair(self.vm, faces); ("faces", r[0].max(0.) as usize, r[1].max(0.) as usize) };
        let name = self.new_name();
        if !self.name_ok(&name) { return NIL; }
        self.op(Operation::Selection(SelectionOperation::Query { object: object.clone(), name: name.clone(), query, element: element.into(), min, max }));
        let ty = part_type(self.vm);
        self.vm.bx.heap_mut().new_handle(ty, Box::new(PartRef { name: object, material: false, sel: Some(name) })).into()
    }

    /// `m.soft_body(body, {preset: "yarn_ball", deform: [parts], attach: [{name, parts, pivot}], ..settings})`:
    /// the body simulates as a soft cage; deform parts ride it, each attach group rides one
    /// rigid frame joint named `name` (pose and key it, or hang `m.joint` children from it).
    fn m_soft_body(&mut self) -> ScriptValue {
        let Some(object) = self.part(0) else { return NIL };
        if !self.known(&object) { return NIL; }
        let o = self.opts(1, SOFT_KEYS);
        let mut s = SoftBodySettings { mass: 1., edge_compliance: 0.02, volume_compliance: 1e-7, pose_compliance: 0.02, damping: 4., gravity: [0., -9.81, 0.], contact_radius: 0.01, friction: 0.5, substeps: 4, iterations: 6, max_speed: 20., max_displacement: 2. };
        match opt_text(self.vm, o, id!(preset)).as_deref() {
            None => {}
            Some("yarn_ball") => { s.edge_compliance = 0.001; s.pose_compliance = 0.001; s.damping = 22.; }
            Some(other) => return self.fail(format!("soft_body preset '{other}' (yarn_ball)")),
        }
        let f = |s: &mut Self, key: LiveId, out: &mut f32| { if let Some(v) = opt_num(s.vm, o, key) { *out = v as f32; } };
        f(self, id!(mass), &mut s.mass); f(self, id!(edge_compliance), &mut s.edge_compliance); f(self, id!(volume_compliance), &mut s.volume_compliance);
        f(self, id!(pose_compliance), &mut s.pose_compliance); f(self, id!(damping), &mut s.damping); f(self, id!(contact_radius), &mut s.contact_radius);
        f(self, id!(friction), &mut s.friction); f(self, id!(max_speed), &mut s.max_speed); f(self, id!(max_displacement), &mut s.max_displacement);
        if let Some(g) = opt_v3(self.vm, o, id!(gravity)) { s.gravity = g.map(|v| v as f32); }
        if let Some(n) = opt_num(self.vm, o, id!(substeps)) { s.substeps = n.clamp(0., 255.) as u8; }
        if let Some(n) = opt_num(self.vm, o, id!(iterations)) { s.iterations = n.clamp(0., 255.) as u8; }
        let dv = opt(self.vm, o, id!(deform));
        let Some(deform_objects) = (if dv.is_nil() { Some(Vec::new()) } else { self.parts(dv, "soft_body deform") }) else { return NIL };
        let av = opt(self.vm, o, id!(attach));
        let mut attachments = Vec::new();
        for i in 0..list_len(self.vm, av) {
            let a = list_value(self.vm, av, i).as_object();
            let Some(name) = opt_text(self.vm, a, id!(name)) else { return self.fail("soft_body attach groups are {name, parts: [..], pivot}") };
            let pv = opt(self.vm, a, id!(parts));
            let Some(objects) = self.parts(pv, "soft_body attach parts") else { return NIL };
            let pivot = opt_v3(self.vm, a, id!(pivot));
            attachments.push(SoftBodyAttachmentRequest { name, objects, pivot });
        }
        self.op(Operation::SoftBody(SoftBodyBind { object, deform_objects, attachments, settings: s }));
        NIL
    }

    /// `m.constraint(name, joint, {copy|aim|ik: target joint, ..} | {limit: {..}}, enabled)`.
    fn m_constraint(&mut self) -> ScriptValue {
        let name = self.text(0);
        let v = self.arg(1);
        let Some(joint_name) = self.joint_name(v, "joint") else { return NIL };
        let o = self.opts(2, &["copy", "aim", "ik", "limit", "translation", "rotation", "scale", "influence", "axis", "middle", "end", "pole", "clamp", "enabled"]);
        let mut j = Joints::default();
        let joint = j.at(&joint_name);
        let influence = opt_num(self.vm, o, id!(influence)).unwrap_or(1.);
        let (copy, aim, ik, limit) = (opt(self.vm, o, id!(copy)), opt(self.vm, o, id!(aim)), opt(self.vm, o, id!(ik)), opt(self.vm, o, id!(limit)));
        let kind = if !copy.is_nil() {
            let Some(t) = self.joint_name(copy, "copy") else { return NIL };
            let b = |s: &mut Self, key: LiveId, d: bool| o.map_or(d, |o| opt_bool(s.vm, o, key, d));
            let (translation, rotation, scale) = (b(self, id!(translation), true), b(self, id!(rotation), true), b(self, id!(scale), false));
            ConstraintKind::Copy { target: j.at(&t), translation, rotation, scale, influence }
        } else if !aim.is_nil() {
            let Some(t) = self.joint_name(aim, "aim") else { return NIL };
            ConstraintKind::Aim { target: j.at(&t), axis: opt_v3(self.vm, o, id!(axis)).unwrap_or([0., 1., 0.]), influence }
        } else if !ik.is_nil() {
            let Some(t) = self.joint_name(ik, "ik") else { return NIL };
            let (mv, ev) = (opt(self.vm, o, id!(middle)), opt(self.vm, o, id!(end)));
            let (Some(mid), Some(end)) = (self.joint_name(mv, "middle"), self.joint_name(ev, "end")) else { return NIL };
            let Some(pole) = opt_v3(self.vm, o, id!(pole)) else { return self.fail("an ik constraint needs {pole} (a position)") };
            let clamp_reach = o.is_some_and(|o| opt_bool(self.vm, o, id!(clamp), false));
            ConstraintKind::TwoBoneIk { middle: j.at(&mid), end: j.at(&end), target: j.at(&t), pole, clamp_reach }
        } else if let Some(l) = limit.as_object() {
            let l = Some(l);
            ConstraintKind::Limit {
                min_translation: opt_v3(self.vm, l, id!(min_pos)).unwrap_or([-1e3; 3]),
                max_translation: opt_v3(self.vm, l, id!(max_pos)).unwrap_or([1e3; 3]),
                min_scale: opt_v3(self.vm, l, id!(min_scale)).unwrap_or([1e-3; 3]),
                max_scale: opt_v3(self.vm, l, id!(max_scale)).unwrap_or([1e3; 3]),
                max_angle: opt_num(self.vm, l, id!(max_angle)).unwrap_or(180.).to_radians(),
            }
        } else {
            return self.fail("constraint needs {copy: joint}, {aim: joint}, {ik: joint, middle, end, pole} or {limit: {..}}");
        };
        let enabled = o.map_or(true, |o| opt_bool(self.vm, o, id!(enabled), true));
        self.rig(j, RigOperation::Constraint(RigConstraint { name, joint, enabled, kind }))
    }

    /// `m.keys(clip, [{joint, rot: [{t, v}], pos: [..], scale: [..]}])`: a clip keyed on named
    /// joints (any rig: a humanoid, `m.joint` children, a soft body's frame children). `rot`
    /// values are degrees, `pos` offsets from the joint's rest, `scale` factors; times in seconds.
    fn m_keys(&mut self) -> ScriptValue {
        use makepad_model::{AnimationChannel, AnimationClip, AnimationPath, Keyframe};
        let name = self.text(0);
        if name.is_empty() { return self.fail("keys(clip, [{joint, rot|pos|scale: [{t, v}]}])"); }
        let tracks = self.arg(1);
        let mut j = Joints::default();
        let mut channels = Vec::new();
        for i in 0..list_len(self.vm, tracks) {
            let track = list_value(self.vm, tracks, i).as_object();
            let jv = opt(self.vm, track, id!(joint));
            let Some(joint) = self.joint_name(jv, "track") else { return NIL };
            let joint = j.at(&joint);
            for (key, path) in [(id!(rot), AnimationPath::Rotation), (id!(pos), AnimationPath::Translation), (id!(scale), AnimationPath::Scale)] {
                let kv = opt(self.vm, track, key);
                let n = list_len(self.vm, kv);
                if n == 0 { continue; }
                let mut keys = Vec::with_capacity(n);
                for k in 0..n {
                    let item = list_value(self.vm, kv, k).as_object();
                    let time = opt_num(self.vm, item, id!(t)).unwrap_or(0.);
                    let v = opt(self.vm, item, id!(v));
                    let x = if path == AnimationPath::Scale && v.as_number().is_some() { [v.as_number().unwrap(); 3] } else { let p = value_vec3(self.vm, v); [p.x as f64, p.y as f64, p.z as f64] };
                    let value = match path { AnimationPath::Rotation => quat_euler_deg(x), _ => [x[0], x[1], x[2], 0.] };
                    keys.push(Keyframe { time, value });
                }
                channels.push(AnimationChannel { joint, path, keys });
            }
        }
        if channels.is_empty() { return self.fail(format!("clip '{name}' keys no joint")); }
        self.push(ProgramOp::Clip { joints: j.0, clip: AnimationClip { name, channels } });
        NIL
    }

    /// `m.clip_options(clip, {interpolation, root_motion, events: [{t, name, payload}],
    /// morphs: [{morph, keys: [{t, w}]}]})`.
    fn m_clip_options(&mut self) -> ScriptValue {
        let name = self.text(0);
        let o = self.opts(1, &["interpolation", "root_motion", "events", "morphs"]);
        let interpolation = match opt_text(self.vm, o, id!(interpolation)).as_deref().unwrap_or("linear") {
            "linear" => Interpolation::Linear, "step" => Interpolation::Step, "cubic" => Interpolation::Cubic,
            other => return self.fail(format!("interpolation '{other}' (linear, step, cubic)")),
        };
        let root_motion = o.is_some_and(|o| opt_bool(self.vm, o, id!(root_motion), false));
        let ev = opt(self.vm, o, id!(events));
        let mut events = Vec::new();
        for i in 0..list_len(self.vm, ev) {
            let e = list_value(self.vm, ev, i).as_object();
            let time = opt_num(self.vm, e, id!(t)).unwrap_or(0.);
            let Some(name) = opt_text(self.vm, e, id!(name)) else { return self.fail("clip events are [{t, name, payload}]") };
            events.push(ClipEvent { time, name, payload: opt_text(self.vm, e, id!(payload)).unwrap_or_default() });
        }
        let mv = opt(self.vm, o, id!(morphs));
        let mut morph_keys = BTreeMap::new();
        for i in 0..list_len(self.vm, mv) {
            let row = list_value(self.vm, mv, i).as_object();
            let Some(morph) = opt_text(self.vm, row, id!(morph)) else { return self.fail("clip morphs are [{morph, keys: [{t, w}]}]") };
            let kv = opt(self.vm, row, id!(keys));
            let mut keys = Vec::new();
            for k in 0..list_len(self.vm, kv) {
                let key = list_value(self.vm, kv, k).as_object();
                keys.push(MorphKey { time: opt_num(self.vm, key, id!(t)).unwrap_or(0.), weight: opt_num(self.vm, key, id!(w)).unwrap_or(0.) });
            }
            morph_keys.insert(morph, keys);
        }
        self.rig(Joints::default(), RigOperation::ClipOptions { name, options: ClipOptions { interpolation, root_motion, events, morph_keys } })
    }

    /// `m.weights(part, {normalize: n} | {smooth: {iterations, factor}} | {auto: {influences, power}}
    /// | {transfer: part, distance} | {mirror: {axis, pairs: [{from, to}], tolerance}} | {lock: [joints]})`.
    /// A rigid binding to one joint is `m.bind(part, joint)`.
    fn m_weights(&mut self) -> ScriptValue {
        let Some(object) = self.part(0) else { return NIL };
        if !self.known(&object) { return NIL; }
        let o = self.opts(1, &["normalize", "smooth", "auto", "transfer", "distance", "mirror", "lock"]);
        let mut j = Joints::default();
        let (lock, normalize, smooth, auto, transfer, mirror) = (opt(self.vm, o, id!(lock)), opt(self.vm, o, id!(normalize)), opt(self.vm, o, id!(smooth)), opt(self.vm, o, id!(auto)), opt(self.vm, o, id!(transfer)), opt(self.vm, o, id!(mirror)));
        if !lock.is_nil() {
            let mut joints = BTreeSet::new();
            for i in 0..list_len(self.vm, lock) { let x = list_value(self.vm, lock, i); let Some(n) = self.joint_name(x, "lock") else { return NIL }; joints.insert(j.at(&n)); }
            return self.rig(j, RigOperation::WeightLocks { object, joints });
        }
        let edit = if !normalize.is_nil() {
            WeightEdit::Normalize { max_influences: value_f32(self.vm, normalize).max(1.) as usize }
        } else if !smooth.is_nil() {
            let s = smooth.as_object();
            WeightEdit::Smooth { iterations: opt_num(self.vm, s, id!(iterations)).unwrap_or(1.).clamp(1., 64.) as u32, factor: opt_num(self.vm, s, id!(factor)).unwrap_or(0.5) }
        } else if !auto.is_nil() {
            let a = auto.as_object();
            WeightEdit::Bind { max_influences: opt_num(self.vm, a, id!(influences)).unwrap_or(4.).max(1.) as usize, power: opt_num(self.vm, a, id!(power)).unwrap_or(2.) }
        } else if !transfer.is_nil() {
            let Some(source) = self.ref_name(transfer, false, "weights transfer") else { return NIL };
            WeightEdit::Transfer { source, max_distance: opt_num(self.vm, o, id!(distance)).unwrap_or(0.05) }
        } else if let Some(mo) = mirror.as_object() {
            let mo = Some(mo);
            let ax = opt(self.vm, mo, id!(axis));
            let axis = if ax.is_nil() { 0 } else { match axis_of(self.vm, ax) { Some(a) => a, None => return self.fail("weights mirror axis is \"x\", \"y\" or \"z\"") } };
            let pv = opt(self.vm, mo, id!(pairs));
            let mut joints = BTreeMap::new();
            for i in 0..list_len(self.vm, pv) {
                let row = list_value(self.vm, pv, i).as_object();
                let (f, t) = (opt(self.vm, row, id!(from)), opt(self.vm, row, id!(to)));
                let (Some(f), Some(t)) = (self.joint_name(f, "from"), self.joint_name(t, "to")) else { return NIL };
                joints.insert(j.at(&f), j.at(&t));
            }
            WeightEdit::Mirror { axis, joints, tolerance: opt_num(self.vm, mo, id!(tolerance)).unwrap_or(0.001) }
        } else {
            return self.fail("weights needs {normalize}, {smooth}, {auto}, {transfer}, {mirror} or {lock}");
        };
        self.rig(j, RigOperation::Weights { object, vertices: Vec::new(), edit })
    }
}

#[cfg(test)]
mod tests {
    use crate::headless::evaluate;
    use makepad_model::{build_program, BuiltProgram, Limits};

    fn build(body: &str) -> BuiltProgram {
        let src = format!("use mod.pod.*\nlet game = mod.game\nlet m = game.model_new(\"t\")\n{body}\nm.build()\n");
        let out = evaluate("t.splash", &src).unwrap_or_else(|e| panic!("{e:?}"));
        let limits = Limits::default();
        build_program(&out.programs[0].program, limits, &mut |_| Err("no imports".into()), None).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Faces are reached by a selection: the top of a box extrudes, takes a material,
    /// and a vertex group bends and moves.
    #[test]
    fn selections_drive_face_and_vertex_edits() {
        let plain = build("let body = m.box({size: vec3(1, 1, 1)})");
        let faces = |b: &BuiltProgram| b.document.objects().map(|(_, m)| m.faces().len()).sum::<usize>();
        let built = build("let red = m.material({color: #f00})\nlet body = m.box({size: vec3(1, 1, 1)})\nlet top = m.select(body, {box: [vec3(-1, 0.45, -1), vec3(1, 1, 1)] faces: [1, 64]})\nm.extrude_faces(top, vec3(0, 0.3, 0))\nm.paint(top, red)\nlet upper = m.select(body, {box: [vec3(-1, 0.6, -1), vec3(1, 2, 1)]})\nm.move_verts(upper, {pos: vec3(0.1, 0, 0) rot: vec3(0, 20, 0)})\nm.bend(upper, {axis: \"y\" angle: 10 range: [0.5, 0.8]})\nm.solidify(body, 0.01)");
        assert!(faces(&built) > faces(&plain), "extrude adds side faces");
        let (_, body) = built.document.objects().next().unwrap();
        assert!(body.faces().iter().any(|f| f.material == 1), "the selection took the material");
        let top = body.vertices().iter().map(|v| v.position[1]).fold(f64::MIN, f64::max);
        assert!(top > 0.7, "top extruded to {top}");
    }

    /// A soft body's attachment frames are joints by name: a child joint hangs from one,
    /// a part binds to it, and poses baked into a clip key it.
    #[test]
    fn soft_body_frames_are_posed_and_keyed_by_name() {
        let built = build("let body = m.sphere({radius: 0.5})\nlet eye = m.sphere({radius: 0.08 pos: vec3(0.15, 0.2, -0.45)})\nm.soft_body(body, {preset: \"yarn_ball\" attach: [{name: \"face\" parts: [eye] pivot: vec3(0, 0.2, -0.4)}]})\nm.joint(\"blink\", \"face\", vec3(0, 0, 0))\nm.bind(eye, \"blink\")\nm.keys(\"idle\", [{joint: \"blink\" scale: [{t: 0 v: 1}, {t: 0.1 v: vec3(1, 0.1, 1)}, {t: 0.2 v: 1}]}])\nm.clip_options(\"idle\", {interpolation: \"step\"})");
        let skeleton = built.document.skeleton().expect("a skeleton");
        assert!(skeleton.joints.iter().any(|j| j.name == "blink"));
        assert!(built.document.soft_body().is_some());
        assert!(built.document.clips().contains_key("idle"));
    }

    /// Rig controls on the humanoid: a constraint, an IK solve and a weight edit resolve
    /// their joints by name; an unknown joint fails the build with its name.
    #[test]
    fn rig_controls_resolve_joints_by_name() {
        use crate::rig_rest::rig_rest;

        build("m.rig_humanoid()\nlet torso = m.box({size: vec3(0.3, 0.5, 0.2) pos: vec3(0, 1.2, 0)})\nm.bind(torso, \"spine\")\nm.pose(\"reach\", [{joint: \"spine\" rot: vec3(10, 0, 0)}])\nm.constraint(\"look\", \"head\", {aim: \"hand_r\" influence: 0.5})\nm.solve_pose(\"reach\")\nm.pose(\"rest\", [{joint: \"spine\"}])\nm.bake_clip(\"wave\", [{t: 0 pose: \"rest\"}, {t: 0.5 pose: \"reach\"}])\nm.weights(torso, {normalize: 4})");
        let src = "use mod.pod.*\nlet game = mod.game\nlet m = game.model_new(\"t\")\nm.rig_humanoid()\nm.rest(\"no_such_joint\", {rot: vec3(0, 10, 0)})\nm.build()\n";
        let out = evaluate("t.splash", src).unwrap();
        let e = build_program(&out.programs[0].program, Limits::default(), &mut |_| Err("no".into()), None).err().expect("unknown joint fails");
        assert!(e.message.contains("no_such_joint"), "{}", e.message);

        let out = evaluate("rig.splash", r#"use mod.pod.*
let game = mod.game
let m = game.model_new("rig")
m.rig_humanoid({clips: []})
m.joint("arm", "chest", vec3(0.1, 0, 0))
m.joint("probe", "arm", vec3(0.3, 0, 0))
m.rest("arm", {pos: vec3(0.2, 0.1, 0) rot: vec3(0, 0, 90)})
m.roll("probe", 35)
let probe = m.box({size: vec3(0.1, 0.1, 0.1)})
m.bind(probe, "probe")
m.build()
let machine = game.model_new("machine")
machine.joint("base", nil, vec3(0, 0.5, 0))
machine.joint("mid", "base", vec3(0, 0.4, 0))
machine.joint("tip", "mid", vec3(0, 0.3, 0))
let segment = machine.box({size: vec3(0.1, 0.1, 0.1)})
machine.bind(segment, "mid")
machine.build()
let plain = game.model_new("plain")
plain.box({size: vec3(1, 1, 1)})
plain.build()
"#).expect("record the humanoid, machine and plain box");
        assert_eq!(out.programs.len(), 3, "each builder must post its own program");
        let compile = |i: usize| build_program(&out.programs[i].program, Limits::default(), &mut |_| Err("no imports".into()), None);
        let near = |a: [f32; 3], b: [f64; 3]| (0..3).all(|k| (f64::from(a[k]) - b[k]).abs() <= 1e-5);

        let rig = rig_rest(&out.programs[0].program).expect("the extended humanoid has a known rig");
        let built = compile(0).expect("the extended humanoid builds");
        let skeleton = built.document.skeleton().expect("compiled humanoid skeleton");
        assert_eq!(rig.names.len(), skeleton.joints.len(), "humanoid joint count");
        for (i, joint) in skeleton.joints.iter().enumerate() {
            let rest = built.document.rig().local_rest(skeleton, i).expect("compiled local rest");
            assert_eq!(rig.names[i], joint.name, "joint name at index {i}");
            assert_eq!(rig.parent[i], joint.parent.map_or(-1, |p| p as i16), "parent of {}", joint.name);
            assert!(near(rig.pos[i], rest.translation), "local translation of {}", joint.name);
            let same_rotation = [1., -1.].into_iter().any(|sign| (0..4).all(|k| (f64::from(rig.rot[i][k]) - sign * rest.rotation[k]).abs() <= 1e-5));
            assert!(same_rotation, "local rotation of {}", joint.name);
        }
        let tip = rig.index("probe").expect("probe resolves by name");
        let world = built.document.rig().global_rest(skeleton).expect("compiled global rest");
        assert!(near(rig.model_space()[tip], makepad_model::transform::transform_point(world[tip], [0.; 3])), "model-space probe position after its parent's rest rotation");

        let machine = compile(1).expect("a model with its own root and a bound part builds");
        let skeleton = machine.document.skeleton().expect("compiled machine skeleton");
        assert_eq!(skeleton.joints.iter().map(|j| j.parent).collect::<Vec<_>>(), [None, Some(0), Some(1)], "compiled machine has exactly the three authored joints in a chain");
        let (_, segment) = machine.document.objects().next().expect("compiled machine box");
        assert!(segment.vertices().iter().all(|v| v.weights.len() == 1 && v.weights[0].joint == 1 && v.weights[0].weight == 1.), "the box must stay bound to mid");
        let rig = rig_rest(&out.programs[1].program).expect("the machine's own skeleton is known");
        assert_eq!(rig.names, ["base", "mid", "tip"], "machine joint names in declaration order");
        assert_eq!(rig.parent, [-1, 0, 1], "machine joint parents");
        assert_eq!(rig.pos, [[0., 0.5, 0.], [0., 0.4, 0.], [0., 0.3, 0.]], "machine parent-local offsets");
        assert!(rig_rest(&out.programs[2].program).is_none(), "a box alone has no rest rig");

        let errors = evaluate("second_root.splash", "use mod.pod.*\nlet m = mod.game.model_new(\"t\")\nm.rig_humanoid()\nm.joint(\"x\", nil, vec3(0, 0, 0))\n").err().expect("a second root fails at the joint call");
        assert!(errors.iter().any(|e| e.contains("joint(name, parent, vec3 offset): this model already has a skeleton, so name its parent")), "second root must report the missing parent: {errors:?}");
    }
}
