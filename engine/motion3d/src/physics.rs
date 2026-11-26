//! Effect physics in a Scene3D (KERNELS.md §3.6, P7): the document's
//! `physics: Physics{...}`, its `Sim{...}` blocks and its physics kernels,
//! read into a `makepad-effect-sim` description; the stepper seeked to
//! every evaluated time; and what nodes read from it.
//!
//! ```text
//! let launch = Kernel{ out: output(Body)  instance: fn(i) { out[i] = Body{pos: .. vel: ..} } }
//! let sparkles = Kernel{ hits: input(contacts("confetti"))  out: emit_buffer(Sprite, 1)  element: fn(i) { .. } }
//! Scene3D{
//!     physics: Physics{step: 1 / 240 seed: 7 retain: 0.3
//!         bodies: [Body{name: "floor" shape: @plane fixed: true}]
//!         spawn: [Spawn{at: beat(16) group: "confetti" count: 600 shape: @box from: launch}]}
//!     children: [
//!         Instances{source: bodies("confetti") geometry: PlaneGeometry{..} material: ..}
//!         Points{source: sparkles.out}
//!     ]
//! }
//! ```
//!
//! Physics is effects only and time is forward only: the same document
//! gives the same frames at any seek, export or re-render on the machine
//! that renders (same-machine reproducibility).
//!
//! **Per evaluation.** The stepper is seeked to the frame's time (a
//! restore from an in-memory checkpoint and steps forward when needed;
//! nothing when the time is within the frames it keeps, as motion-blur
//! sub-frames are). Kinematic bodies are driven from their keyable
//! `position`/`rotation`, evaluated at each step's time. Bodies are
//! published per group as instance records through a
//! `makepad-render-kernels` ring (interpolated between steps) and read
//! from the published slot.

use crate::anim::{Anim, AudioSource, EvalCtx, ScriptEval};
use crate::scene::NodeDef;
use crate::script::{Reader, Semantic};
use makepad_effect_sim::*;
// makepad_widgets' glob also brings a `Motion` (the animation one) when a
// family that exports it is linked in the same build; this is the body's.
use makepad_effect_sim::Motion;
use makepad_render_kernels::{ManualFences, OutputRing};
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;

/// What moves a kinematic body.
#[derive(Clone, Debug)]
pub enum KinSource {
    /// Its own keyable `position` and `rotation` (degrees).
    Keys(Anim<3>, Anim<3>),
    /// A scene node's world pose (`kinematic: "rio_node"`): a walking
    /// character's collider, a prop's.
    Node(String),
}

/// A document's physics: the description and what drives its kinematic
/// bodies.
pub struct PhysicsDef {
    pub desc: PhysicsDesc,
    /// (key, source).
    pub kinematic: Vec<(String, KinSource)>,
}

impl std::fmt::Debug for PhysicsDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PhysicsDef({} bodies, {} spawns, {} sims)", self.desc.bodies.len(), self.desc.spawns.len(), self.desc.sims.len())
    }
}

const PHYSICS_FIELDS: &[&str] = &["step", "substeps", "seed", "gravity", "retain", "hit_speed", "sleep", "bodies", "spawn", "checkpoint_mb"];
const BODY_FIELDS: &[&str] = &[
    "name", "group", "shape", "fixed", "kinematic", "position", "rotation", "size", "vel", "spin", "drag", "spin_drag", "density", "friction", "bounce", "lift", "color",
    "scale", "life", "model",
];
const SPAWN_FIELDS: &[&str] = &["at", "group", "count", "shape", "size", "fixed", "from", "model"];

/// `model: <built alias or file>`: the model's own collider (its
/// `m.collider` metadata) as the shape; a convex hull for a dynamic body,
/// the proxies' triangles for a fixed or kinematic one. `None` when there
/// is no `model`; `Some(None)` when it has no usable collider yet (still
/// building, or an error was reported).
fn model_shape(r: &mut Reader, cx: &mut crate::doc::LoadCx, o: ScriptObject, path: &str, dynamic: bool) -> Option<Option<Shape>> {
    if r.get(o, "model").is_nil() {
        return None;
    }
    let loc = r.loc(o);
    let media = r.media(o, path, "model")?;
    let glb = match &media {
        crate::script::MediaRef::File(alias) => match crate::models::built_glb(&mut cx.models, alias) {
            Some(crate::models::BuiltGlb::Ready(_, glb)) => Ok(glb.to_vec()),
            Some(crate::models::BuiltGlb::Pending) => return Some(None),
            Some(crate::models::BuiltGlb::Failed(e)) => Err(format!("the model `{alias}` did not build: {e}")),
            None => cx.resolver.load(&media).map(|b| b.to_vec()),
        },
        _ => cx.resolver.load(&media).map(|b| b.to_vec()),
    };
    match glb.and_then(|glb| crate::models::collider(&glb, dynamic)) {
        Ok(shape) => Some(Some(shape)),
        Err(e) => {
            r.error(loc, &format!("{path}.model"), e, None);
            Some(None)
        }
    }
}

fn shape_of(r: &mut Reader, o: ScriptObject, path: &str, default: Shape) -> Shape {
    let v = r.get(o, "shape");
    if v.is_nil() {
        return default;
    }
    let name = v.as_id().map(|id| id.to_string()).or_else(|| r.kind(v)).unwrap_or_default();
    match name.as_str() {
        "box" | "cube" => Shape::Box,
        "sphere" | "ball" => Shape::Sphere,
        "capsule" => Shape::Capsule,
        "plane" | "floor" => Shape::Plane,
        other => {
            let loc = r.loc(o);
            r.error(loc, &format!("{path}.shape"), format!("no body shape `{other}`"), Some("one of @box @sphere @capsule @plane".into()));
            default
        }
    }
}

fn record_of(r: &mut Reader, o: ScriptObject, path: &str) -> BodyRecord {
    let v3 = |r: &mut Reader, n: &str, d: [f32; 3]| r.plain::<3>(r.get(o, n), Semantic::Plain).unwrap_or(d);
    let rot = v3(r, "rotation", [0.0; 3]);
    let q = makepad_geom3d::math::quat_from_euler_deg(rot);
    BodyRecord {
        pos: v3(r, "position", [0.0; 3]),
        size: v3(r, "size", [0.0; 3]),
        rot: q,
        vel: v3(r, "vel", [0.0; 3]),
        spin: v3(r, "spin", [0.0; 3]),
        drag: r.f32(o, path, "drag", 0.0),
        spin_drag: r.f32(o, path, "spin_drag", 0.0),
        density: r.f32(o, path, "density", 0.0),
        friction: r.f32(o, path, "friction", 0.0),
        bounce: r.f32(o, path, "bounce", 0.0),
        lift: r.f32(o, path, "lift", 0.0),
        delay: 0.0,
        life: r.f32(o, path, "life", 0.0),
        color: r.plain::<4>(r.get(o, "color"), Semantic::Color).unwrap_or([0.0; 4]),
        scale: v3(r, "scale", [0.0; 3]),
        shape: 0,
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = Fnv::default();
    h.bytes(s.as_bytes());
    h.finish()
}

/// The kernel a spawn's `from` names: a `Kernel{..}` writing `Body`
/// records, or one of its outputs (`launch.out`).
fn spawn_kernel(r: &mut Reader, v: ScriptValue, path: &str) -> Option<KernelCall> {
    let handle = if r.is_anim(v).as_deref() == Some("kernel_buf") {
        v
    } else {
        let o = v.as_object()?;
        // The kernel's outputs, by the keys themselves (their names need
        // not be in the id table).
        let keys: Vec<ScriptValue> = r.vm.bx.heap_ref().map_entries(o).iter().map(|e| e.key).collect();
        keys.into_iter().map(|k| r.vm.bx.heap_ref().value(o, k, NoTrap)).find(|x| r.is_anim(*x).as_deref() == Some("kernel_buf"))?
    };
    let o = handle.as_object()?;
    let id = r.number(r.get(o, "kernel"))? as usize;
    let spec = r.get(o, "spec").as_object();
    let (name, kernel, outputs, params, physics, reads) = {
        let h = crate::script::host(r.vm)?;
        let dk = h.kernels.list.get(id)?;
        (dk.name.clone(), dk.kernel.clone(), dk.outputs.clone(), dk.params.clone(), dk.physics.clone(), dk.inputs.len())
    };
    let loc = r.loc(o);
    let Some(out) = outputs.iter().find(|b| b.ty == "Body") else {
        r.error(loc, path, format!("kernel `{name}` writes no Body records"), Some("a spawn's kernel has `out: output(Body)` or `out: emit_buffer(Body, n)`".into()));
        return None;
    };
    if reads > 0 {
        r.error(loc, path, format!("kernel `{name}` reads another kernel's output; a spawn's kernel reads only physics (bodies(..), contacts(..), events(..))"), None);
        return None;
    }
    let mut values = Vec::new();
    for p in &params {
        let v = spec.map(|so| r.get(so, p)).unwrap_or(NIL);
        match r.number(v) {
            Some(n) => values.push((p.clone(), n as f32)),
            None if v.is_nil() => {}
            None => r.error(loc, &format!("{path}.{p}"), format!("a spawn kernel's param `{p}` is a number (it runs once, at the spawn)"), None),
        }
    }
    Some(KernelCall { kernel, buffer: out.name.clone(), params: values, inputs: physics, program: hash_str(&format!("{name}#{id}")) })
}

/// Reads `physics: Physics{...}` (or nothing, when the document has only
/// `Sim{}` blocks) into a description; the document's sims join it.
pub(crate) fn parse_physics(r: &mut Reader, cx: &mut crate::doc::LoadCx, v: ScriptValue, doc_hash: u64, root: &NodeDef) -> Option<PhysicsDef> {
    let path = "Scene3D.physics";
    let sims: Vec<SimDesc> = {
        let h = crate::script::host(r.vm)?;
        h.kernels
            .sims
            .iter()
            .map(|s| {
                let call = |kernel: &makepad_script_compute::kernel::KernelRef| KernelCall { kernel: kernel.clone(), buffer: "next".into(), params: s.params.clone(), inputs: s.physics.clone(), program: s.program };
                SimDesc {
                    name: s.name.clone(),
                    state: s.state.clone(),
                    count: s.count,
                    every: s.every,
                    init: call(&s.init),
                    update: call(&s.update),
                    prev: "prev".into(),
                    spawn: s.spawn.as_ref().map(|b| (b.clone(), b.clone(), Shape::Sphere)),
                }
            })
            .collect()
    };
    if v.is_nil() && sims.is_empty() {
        return None;
    }
    let mut desc = PhysicsDesc::new(doc_hash);
    desc.sims = sims;
    let mut kinematic = Vec::new();
    let Some(o) = v.as_object() else {
        return Some(PhysicsDef { desc, kinematic });
    };
    if r.kind(v).as_deref() != Some("physics") {
        let loc = r.loc(o);
        r.error(loc, path, format!("`physics` is a Physics{{..}}, not {}", r.describe(v)), None);
        return None;
    }
    r.check_fields(o, path, "Physics", PHYSICS_FIELDS);
    let loc = r.loc(o);
    if r.has(o, "step") {
        let s = r.number(r.get(o, "step")).unwrap_or(1.0 / 240.0);
        match StepDt::from_secs(s) {
            Ok(dt) => desc.dt = dt,
            Err(e) => r.error(loc, &format!("{path}.step"), e.to_string(), Some("write `step: 1 / 240` (or 1 / 120, 1 / 480)".into())),
        }
    }
    desc.substeps = r.u32(o, path, "substeps", 2, 16).max(1);
    desc.seed = r.number(r.get(o, "seed")).unwrap_or(0.0).max(0.0) as u64;
    desc.gravity = r.plain::<3>(r.get(o, "gravity"), Semantic::Plain).unwrap_or([0.0, -9.8, 0.0]);
    desc.retain = SimTime::from_secs(r.number(r.get(o, "retain")).unwrap_or(0.3).clamp(0.0, 60.0));
    desc.hit_speed = r.f32(o, path, "hit_speed", 0.05).max(0.0);
    desc.sleep = r.bool(o, "sleep", true);
    if r.has(o, "checkpoint_mb") {
        desc.checkpoint_bytes = (r.f32(o, path, "checkpoint_mb", 64.0).clamp(1.0, 16384.0) as usize) << 20;
    }
    for (i, bv) in r.list(r.get(o, "bodies")).into_iter().enumerate() {
        let bpath = format!("{path}.bodies[{i}]");
        let Some(bo) = bv.as_object().filter(|_| r.kind(bv).as_deref() == Some("body")) else {
            let at = bv.as_object().map(|x| r.loc(x)).unwrap_or(loc);
            r.error(at, &bpath, format!("expected a Body{{..}}, found {}", r.describe(bv)), None);
            continue;
        };
        r.check_fields(bo, &bpath, "Body", BODY_FIELDS);
        let name = r.string(bo, "name").unwrap_or_else(|| format!("body{i}"));
        let group = r.string(bo, "group").unwrap_or_default();
        let mut shape = shape_of(r, bo, &bpath, Shape::Box);
        // `kinematic: true` (its own keys) or `kinematic: "node"` (follows
        // that scene node).
        let kin_v = r.get(bo, "kinematic");
        let kin_node = if kin_v.is_string_like() { r.string_of(kin_v) } else { None };
        let is_kinematic = kin_node.is_some() || r.bool(bo, "kinematic", false);
        let dynamic = !is_kinematic && !r.bool(bo, "fixed", false);
        match model_shape(r, cx, bo, &bpath, dynamic) {
            Some(Some(s)) => shape = s,
            Some(None) => continue,
            None => {}
        }
        let record = record_of(r, bo, &bpath);
        let motion = if let Some(node) = kin_node {
            if find_node(root, &node).is_none() {
                let at = r.loc(bo);
                let mut names = Vec::new();
                node_names(root, &mut names);
                let hint = crate::anim::closest(&node, names.iter().map(|s| s.as_str())).map(|c| format!("did you mean \"{c}\"?")).or_else(|| Some("name the node: `Model{name: \"rio_node\" ..}`".into()));
                r.error(at, &format!("{bpath}.kinematic"), format!("no scene node named \"{node}\" for the body to follow"), hint);
                continue;
            }
            kinematic.push((name.clone(), KinSource::Node(node)));
            Motion::Kinematic { key: name.clone() }
        } else if is_kinematic {
            let p = r.anim::<3>(bo, &bpath, "position", [0.0; 3], Semantic::Plain);
            let q = r.anim::<3>(bo, &bpath, "rotation", [0.0; 3], Semantic::Plain);
            kinematic.push((name.clone(), KinSource::Keys(p, q)));
            Motion::Kinematic { key: name.clone() }
        } else if r.bool(bo, "fixed", false) || shape == Shape::Plane {
            Motion::Fixed
        } else {
            Motion::Dynamic
        };
        if let Err(e) = record.validate(i) {
            let at = r.loc(bo);
            r.error(at, &bpath, e.to_string(), None);
            continue;
        }
        desc.bodies.push(BodyDesc { name, group, shape, motion, record });
    }
    for (i, sv) in r.list(r.get(o, "spawn")).into_iter().enumerate() {
        let spath = format!("{path}.spawn[{i}]");
        let Some(so) = sv.as_object().filter(|_| r.kind(sv).as_deref() == Some("spawn")) else {
            let at = sv.as_object().map(|x| r.loc(x)).unwrap_or(loc);
            r.error(at, &spath, format!("expected a Spawn{{..}}, found {}", r.describe(sv)), None);
            continue;
        };
        r.check_fields(so, &spath, "Spawn", SPAWN_FIELDS);
        let at_loc = r.loc(so);
        let at = r.number(r.get(so, "at")).unwrap_or(0.0);
        let Some(group) = r.string(so, "group") else {
            r.error(at_loc, &spath, "a Spawn names its group: `group: \"confetti\"`", None);
            continue;
        };
        let count = r.u32(so, &spath, "count", 1, 1_000_000);
        let mut shape = shape_of(r, so, &spath, Shape::Box);
        let size = r.plain::<3>(r.get(so, "size"), Semantic::Plain).unwrap_or([0.1; 3]);
        let fixed = r.bool(so, "fixed", false);
        match model_shape(r, cx, so, &spath, !fixed) {
            Some(Some(s)) => shape = s,
            Some(None) => continue,
            None => {}
        }
        let from_v = r.get(so, "from");
        let from = if from_v.is_nil() {
            r.error(at_loc, &spath, "a Spawn needs `from`: a kernel writing Body records, or a list of Body{..}", None);
            continue;
        } else if r.kind(from_v).is_none() && r.is_anim(from_v).is_none() && !r.list(from_v).is_empty() {
            let mut recs = Vec::new();
            for (k, bv) in r.list(from_v).into_iter().enumerate() {
                if let Some(bo) = bv.as_object().filter(|_| r.kind(bv).as_deref() == Some("body")) {
                    recs.push(record_of(r, bo, &format!("{spath}.from[{k}]")));
                }
            }
            SpawnFrom::Records(Arc::new(recs))
        } else {
            match spawn_kernel(r, from_v, &format!("{spath}.from")) {
                Some(k) => SpawnFrom::Kernel(k),
                None => {
                    if !r.diagnostics.iter().any(|d| d.is_error()) {
                        r.error(at_loc, &format!("{spath}.from"), format!("`from` is a kernel writing Body records or a list of Body{{..}}, not {}", r.describe(from_v)), Some("a spawn kernel declares `out: output(Body)` (or `emit_buffer(Body, n)`)".into()));
                    }
                    continue;
                }
            }
        };
        desc.spawns.push(SpawnDesc { at: SimTime::from_secs(at), group, count, shape, fixed, size, from });
    }
    Some(PhysicsDef { desc, kinematic })
}

/// The named node, and the nodes above it (root first).
fn find_node<'a>(n: &'a NodeDef, name: &str) -> Option<Vec<&'a NodeDef>> {
    if n.name.as_deref() == Some(name) {
        return Some(vec![n]);
    }
    for c in &n.children {
        if let Some(mut path) = find_node(c, name) {
            path.insert(0, n);
            return Some(path);
        }
    }
    None
}

fn node_names(n: &NodeDef, out: &mut Vec<String>) {
    if let Some(name) = &n.name {
        out.push(name.clone());
    }
    for c in &n.children {
        node_names(c, out);
    }
}

/// The rotation of a matrix's upper 3x3 (scale removed), as x, y, z, w.
fn rotation_of(m: &Mat4f) -> [f32; 4] {
    let col = |k: usize| {
        let c = [m.v[k * 4], m.v[k * 4 + 1], m.v[k * 4 + 2]];
        let l = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt().max(1e-12);
        [c[0] / l, c[1] / l, c[2] / l]
    };
    let (x, y, z) = (col(0), col(1), col(2));
    // Row-major r[row][col] of the rotation whose columns are x, y, z.
    let (m00, m11, m22) = (x[0], y[1], z[2]);
    let tr = m00 + m11 + m22;
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [(y[2] - z[1]) / s, (z[0] - x[2]) / s, (x[1] - y[0]) / s, 0.25 * s]
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [0.25 * s, (y[0] + x[1]) / s, (z[0] + x[2]) / s, (y[2] - z[1]) / s]
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [(y[0] + x[1]) / s, 0.25 * s, (z[1] + y[2]) / s, (z[0] - x[2]) / s]
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [(z[0] + x[2]) / s, (z[1] + y[2]) / s, 0.25 * s, (x[1] - y[0]) / s]
    };
    makepad_geom3d::math::quat_normalize(q)
}

/// Kinematic poses from the document: a body's own keyables, or a scene
/// node's world pose at the step's time.
struct KinDrive<'a> {
    def: &'a PhysicsDef,
    root: &'a NodeDef,
    script: &'a mut dyn ScriptEval,
    audio: &'a dyn AudioSource,
    edit_offset: f32,
}

impl Drive for KinDrive<'_> {
    fn pose(&mut self, key: &str, t: SimTime) -> Option<([f32; 3], [f32; 4])> {
        use makepad_geom3d::math::*;
        use makepad_geom3d::mesh::sub3;
        let (_, source) = self.def.kinematic.iter().find(|k| k.0 == key)?;
        let t = t.as_secs() as f32;
        let mut cx = EvalCtx { audio: self.audio, edit_offset: self.edit_offset, script: Some(&mut *self.script) };
        match source {
            KinSource::Keys(p, q) => {
                let pos = p.eval(t, &mut cx);
                let rot = q.eval(t, &mut cx);
                Some((pos, quat_from_euler_deg(rot)))
            }
            KinSource::Node(name) => {
                // The node's world matrix at t, as the scene walk composes
                // it (position, rotation or look_at, scale, pivot; a
                // camera-facing node turns by its rotation only).
                let chain = find_node(self.root, name)?;
                let mut world = Mat4f::identity();
                for n in chain {
                    let tr = &n.transform;
                    let p = tr.position.eval(t, &mut cx);
                    let s = tr.scale.eval(t, &mut cx);
                    let pivot = tr.pivot.eval(t, &mut cx);
                    let q = match &tr.look_at {
                        Some(la) if !tr.face_camera => {
                            let target = la.eval(t, &mut cx);
                            look_rotation(sub3(target, transform_point(&world, p)), [0.0, 1.0, 0.0])
                        }
                        _ => quat_from_euler_deg(tr.rotation.eval(t, &mut cx)),
                    };
                    let mut local = compose(p, q, s);
                    if pivot != [0.0; 3] {
                        local = mul(&local, &translation([-pivot[0], -pivot[1], -pivot[2]]));
                    }
                    world = mul(&world, &local);
                }
                let pos = mat_translation(&world);
                let q = rotation_of(&world);
                (pos.iter().chain(q.iter()).all(|x| x.is_finite())).then_some((pos, q))
            }
        }
    }
}

/// The stepper of a loaded document and the rings its bodies publish
/// through.
pub struct PhysicsRuntime {
    pub def: Arc<PhysicsDef>,
    stepper: Option<Stepper>,
    rings: HashMap<String, OutputRing>,
    /// The CPU-side draw: the instance data is copied into the scene's own
    /// buffers during the evaluation, so each published slot is read by
    /// exactly one evaluation and completes with it.
    fences: ManualFences,
    layout: makepad_render_kernels::compute::boundary::LayoutView<'static>,
    /// Interactive preview: step at most this long per evaluation and show
    /// the newest state until the time is reached (`None`: locked time,
    /// every evaluation reaches its time; exports).
    realtime: Option<std::time::Duration>,
    /// The time views are taken at this evaluation (the asked time, or
    /// the newest reached one when behind in realtime).
    shown: SimTime,
}

impl PhysicsRuntime {
    pub fn new(def: Arc<PhysicsDef>) -> Self {
        Self { def, stepper: None, rings: HashMap::new(), fences: ManualFences::new(), layout: RIGID, realtime: None, shown: SimTime::ZERO }
    }

    /// See [`Self::realtime`]: a budget per evaluation for interactive
    /// preview, `None` (the default) for locked time.
    pub fn set_realtime(&mut self, budget: Option<std::time::Duration>) {
        self.realtime = budget;
    }

    /// Seeks the stepper to `t` (layer seconds), driving kinematic bodies
    /// through `script`; returns the problems physics met so far.
    pub fn seek(&mut self, t: f32, script: &mut dyn ScriptEval, audio: &dyn AudioSource, edit_offset: f32, root: &NodeDef) {
        let def = self.def.clone();
        let mut drive = KinDrive { def: &def, root, script, audio, edit_offset };
        let st = self.stepper.get_or_insert_with(|| Stepper::new(def.desc.clone(), &mut drive));
        let want = SimTime::from_secs(t as f64);
        self.shown = want;
        match self.realtime {
            None => st.seek(want, &mut drive),
            Some(budget) => {
                if let Progress::Behind { .. } = st.advance_to(want, budget, &mut drive) {
                    // The last state stays up until physics catches up.
                    self.shown = st.view_or_latest(want).time();
                }
            }
        }
    }

    pub fn stepper(&self) -> Option<&Stepper> {
        self.stepper.as_ref()
    }

    /// A group's bodies at `t` (after [`Self::seek`]), through the group's
    /// instance ring: published, then read back from the slot the draw
    /// uses.
    pub fn bodies(&mut self, group: &str, t: f32, out: &mut Vec<RigidRecord>) {
        out.clear();
        let Some(st) = self.stepper.as_ref() else { return };
        let Some(view) = st.view(self.at(t)) else { return };
        let layout = &self.layout;
        let ring = self.rings.entry(group.to_string()).or_insert_with(|| body_ring(0x5e_0001, layout));
        let fences = &self.fences;
        ring.collect(fences);
        if publish_bodies(&view, group, layout, ring, fences).is_err() {
            return;
        }
        if let Some(d) = ring.draw(fences) {
            let n = d.count as usize;
            for k in 0..n {
                let w = &d.data[k * RIGID_WORDS..(k + 1) * RIGID_WORDS];
                let f = |i: usize| f32::from_bits(w[i]);
                out.push(RigidRecord {
                    pos: [f(0), f(1), f(2)],
                    rot: [f(3), f(4), f(5), f(6)],
                    scale: [f(7), f(8), f(9)],
                    color: [f(10), f(11), f(12), f(13)],
                    seed: w[14],
                    id: w[15],
                    prev_pos: [f(16), f(17), f(18)],
                    prev_rot: [f(19), f(20), f(21), f(22)],
                    age: f(23),
                    radius: f(24),
                    speed: f(25),
                    group: w[26],
                });
            }
        }
        // This evaluation has copied what it read.
        let serial = fences.submit();
        fences.complete(serial);
    }

    /// A kernel input at `t`: the records as the kernel reads them (words
    /// in f32 slots, at least one record) and how many there are.
    pub fn input(&self, what: &SimInput, t: f32) -> (Arc<Vec<f32>>, usize) {
        let Some(view) = self.stepper.as_ref().and_then(|st| st.view(self.at(t))) else {
            return (Arc::new(vec![0.0; RIGID_WORDS]), 0);
        };
        let (words, n, stride) = match what {
            SimInput::Contacts(g) => {
                let (w, n) = view.contact_words(g);
                (w, n, CONTACT_WORDS)
            }
            SimInput::Events(g) => {
                let (w, n) = view.event_words(g);
                (w, n, EVENT_WORDS)
            }
            SimInput::Bodies(g) => {
                let mut w = Vec::new();
                let n = view.write_bodies(g, &RIGID, &mut w);
                (w, n, RIGID_WORDS)
            }
            SimInput::Sim(name) => {
                let w = view.sim(name).map(|s| s.to_vec()).unwrap_or_default();
                let stride = self.def.desc.sims.iter().find(|s| &s.name == name).map(|s| s.state.stride as usize).unwrap_or(1);
                let n = w.len() / stride.max(1);
                (w, n, stride)
            }
        };
        let mut f: Vec<f32> = words.into_iter().map(f32::from_bits).collect();
        if f.is_empty() {
            f = vec![0.0; stride];
        }
        (Arc::new(f), n)
    }

    /// A sim's state at `t` as floats (by its layout), with its layout.
    pub fn sim_state(&self, name: &str, t: f32) -> Option<(Vec<f32>, makepad_render_kernels::RecordLayout)> {
        let st = self.stepper.as_ref()?;
        let layout = self.def.desc.sims.iter().find(|s| s.name == name)?.state.clone();
        let view = st.view(self.at(t))?;
        Some((view.sim(name)?.iter().map(|w| f32::from_bits(*w)).collect(), layout))
    }

    /// The time a view reads for layer time `t`: `t`, or the newest
    /// reached time while realtime physics is behind.
    fn at(&self, t: f32) -> SimTime {
        let want = SimTime::from_secs(t as f64);
        if self.realtime.is_some() && self.shown < want {
            self.shown
        } else {
            want
        }
    }

    /// Problems physics met (refused records, kernel errors), each once.
    pub fn errors(&self) -> Vec<String> {
        self.stepper.as_ref().map(|s| s.errors().cloned().collect()).unwrap_or_default()
    }
}
