//! `model_new`: author a real 3D model in script.
//!
//! The builder records typed `libs/model` operations and never touches
//! geometry itself: `m.build()` hashes the recorded program and posts it
//! to a process-wide mailbox. The build service (`makepad-model-build`)
//! compiles it off the script's time budget,
//! caches the GLB by that hash and the app installs it as the library alias
//! `gen/mdl/<name>`.
//! From then on the alias is an ordinary model: `game.model`, `game.car`,
//! `game.character` and `game.gun` take it like any stock id. An unchanged
//! program is a no-op; an edited one rebuilds and the level re-evaluates once
//! the new revision is installed.
//!
//! Units are metres, Y up. Vehicles face +Z, characters and weapons face -Z.

use crate::rig_rest::{rig_rest, RigRest};
use crate::value::*;
use crate::{Admit, ModelHost};
use makepad_csg_math::{portable::PortableFloat, Vec3d};
use makepad_model::transform::{inverse, matrix_mul, transform_point, Matrix4, IDENTITY_MATRIX};
use makepad_model::{
    Action as MeshAction, Attachment, BooleanMode, CollisionProxy, Construction,
    ConstructionOperation, Decal, GlbLamp, GlbShading, LightEmitter, LightKind,
    LodLevel, Material, MeshEditingOperation, MeshPrimitive, Opening, Operation,
    PatternKind, ProgramOp, RgbaImage, Roof, RoofKind, SceneNode, SceneOperation, SceneState,
    SelectionOperation, SelectionQuery, Shape, Socket, SurfaceAlpha, SurfaceChannel,
    SurfaceDerivation, SurfaceLayer, SurfaceMaterial, SurfaceOperation, SurfacePattern,
    Transform, VehicleWheel, VisualWheelMotion, Wall,
};
use makepad_script::makepad_math::*;
use makepad_script::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub const SCRIPT_MODEL_PREFIX: &str = "gen/mdl/";

/// One compile the app owes the script.
#[derive(Clone, Debug)]
pub struct ScriptModelRequest {
    pub alias: String,
    pub hash: String,
    pub title: String,
    pub class: String,
    pub program: Arc<Vec<ProgramOp>>,
    /// The builder's materials by their bindings (material `i + 1` is `materials[i]`).
    pub materials: Arc<Vec<String>>,
    /// Texture generation before the worker build; excluded from the program hash.
    pub texture_timings: Arc<Vec<makepad_model::TextureTiming>>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptModelState {
    Unknown,
    Building,
    Ready,
    Failed(String),
    /// The host withheld it on this device (`Admit::Withhold`, with the
    /// reason): nothing builds, the app draws a stand-in and keeps the
    /// placement for when the alias is admitted.
    Withheld(String),
}

/// The revision one evaluation asked for, even when it was already queued or ready.
#[derive(Clone, Debug)]
pub struct ScriptModelRevision { pub alias: String, hash: String }
impl ScriptModelRevision {
    /// `None` means a later request replaced this revision (or evicted it).
    /// The registry lock only reads small state; no build work holds it.
    pub fn state(&self) -> Option<ScriptModelState> {
        registry(|r| (r.wanted(&self.alias) == Some(&self.hash)).then(|| r.state(&self.alias)))
    }

    /// The app lost a ready revision's resident bytes. Queue its original
    /// request once; a newer revision or an already pending build wins.
    pub fn request_install(&self) {
        registry(|r| {
            if r.wanted(&self.alias) != Some(&self.hash) || r.state(&self.alias) != ScriptModelState::Ready { return; }
            let request = r.requested[&self.alias].clone();
            r.installed.remove(&self.alias);
            r.queue.retain(|q| q.alias != self.alias);
            r.queue.push(request);
        })
    }
}

#[derive(Default)]
struct Registry {
    /// Keep the original request until eviction so a lost resident product
    /// can be installed again without re-evaluating its level.
    requested: HashMap<String, ScriptModelRequest>,
    installed: HashMap<String, String>,
    failed: HashMap<String, (String, String)>,
    /// Revisions the host withheld: alias -> (hash, reason). Their request
    /// stays in `requested` without its program.
    withheld: HashMap<String, (String, String)>,
    /// Bumped whenever an alias is withheld or admitted after being withheld.
    withheld_rev: u64,
    /// Canonical encoded bytes of each requested program this process holds.
    program_bytes: HashMap<String, usize>,
    queue: Vec<ScriptModelRequest>,
    arrived: bool,
    /// Estimated model-space bounds per alias, for build-time placeholders.
    bounds: HashMap<String, [[f64; 3]; 2]>,
    rigs: HashMap<String, Arc<RigRest>>,
    /// Builds the app is running right now (set by the app each poll).
    running: Vec<String>,
    /// Size of the current batch of level builds (reset when none pend).
    batch: usize,
    /// Where each still-building model is first placed (and in which order),
    /// for the build queue's priority.
    placements: HashMap<String, ([f32; 3], usize)>,
    /// Script models authored with their front on -Z (every class but
    /// vehicle, as `game.model_new` documents): a body wearing one turns it
    /// to face its motion. glTF assets and vehicles front on +Z.
    front_neg_z: std::collections::HashSet<String>,
    /// Aliases the world being played has built (since the last
    /// `begin_script_model_world`): only their failures are its failures.
    world: HashSet<String>,
    /// Vegetation models' habitats (`game.model_new(.., {habitat})`).
    habitats: HashMap<String, String>,
    /// `m.lod_models` chains per alias, and a revision the app watches.
    lod_chains: HashMap<String, Vec<(f32, String)>>,
    lod_rev: u64,
    /// Vehicle bodies whose wheels are a separate model (a kit's own wheel
    /// entities): body alias -> (wheel alias, FL/FR/RL/RR centre xyz and
    /// radius in the body model's frame). The asset library records it.
    separate_wheels: HashMap<String, (String, [[f32; 4]; 4])>,
}
static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);
impl Registry {
    fn wanted(&self, alias: &str) -> Option<&String> { self.requested.get(alias).map(|request| &request.hash) }
    fn state(&self, alias: &str) -> ScriptModelState {
        let Some(want) = self.wanted(alias) else { return ScriptModelState::Unknown };
        if let Some((hash, error)) = self.failed.get(alias) { if hash == want { return ScriptModelState::Failed(error.clone()); } }
        if let Some((hash, reason)) = self.withheld.get(alias) { if hash == want { return ScriptModelState::Withheld(reason.clone()); } }
        if self.installed.get(alias) == Some(want) { ScriptModelState::Ready } else { ScriptModelState::Building }
    }
    /// Nothing more will happen to this revision without a new request: it
    /// installed, failed or was withheld.
    fn settled(&self, alias: &str, hash: &str) -> bool {
        self.installed.get(alias).is_some_and(|h| h == hash)
            || self.failed.get(alias).is_some_and(|(h, _)| h == hash)
            || self.withheld.get(alias).is_some_and(|(h, _)| h == hash)
    }
    /// An admitted request of `alias` ends a withholding: true if there was one.
    fn admit_withheld(&mut self, alias: &str) -> bool {
        let resumed = self.withheld.remove(alias).is_some();
        if resumed { self.withheld_rev += 1; self.arrived = true; }
        resumed
    }

    /// Record which way a posted model's front points: every class but
    /// vehicle faces -Z, the engine's forward (glTF assets and vehicles face
    /// +Z). Every way a model is posted (`m.build`, `game.character_new`,
    /// `game.fps_arms_new`, level models, built-ins) goes through here, so
    /// the movement turn and the catalogue's thumbnail camera read one rule.
    fn note_front(&mut self, alias: &str, class: &str) {
        if class == "vehicle" { self.front_neg_z.remove(alias); } else { self.front_neg_z.insert(alias.to_string()); }
    }
    fn note_rig(&mut self, alias: &str, rig: Option<Arc<RigRest>>) -> Option<Arc<RigRest>> {
        match rig {
            Some(rig) => self.rigs.insert(alias.into(), rig),
            None => self.rigs.remove(alias),
        }
    }
}
fn registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R { f(REGISTRY.lock().unwrap().get_or_insert_with(Registry::default)) }

/// The app's side: programs posted since the last drain.
pub fn take_script_model_requests() -> Vec<ScriptModelRequest> { registry(|r| std::mem::take(&mut r.queue)) }
/// Installed revision `hash` of `alias`. A revision some eval is waiting for
/// raises the arrival flag (`take_script_model_arrivals`).
pub fn mark_script_model_installed(alias: &str, hash: &str) {
    registry(|r| {
        r.installed.insert(alias.into(), hash.into());
        r.failed.remove(alias);
        r.admit_withheld(alias);
        if r.wanted(alias).map(String::as_str) == Some(hash) { r.arrived = true; }
    })
}
pub fn mark_script_model_failed(alias: &str, hash: &str, error: &str) { registry(|r| { r.failed.insert(alias.into(), (hash.into(), error.into())); }) }
/// True once after any requested revision installs: pending placements
/// and body appearances can bind the real model in place.
pub fn take_script_model_arrivals() -> bool { registry(|r| std::mem::take(&mut r.arrived)) }
/// A built-in program that cannot be posted: a failed state the app shows.
fn makepad_platform_log(alias: &str, error: &str) { registry(|r| { r.failed.insert(alias.into(), (String::new(), error.into())); }) }

thread_local! {
    /// While set, posted programs go here instead of the registry (headless evaluation).
    static CAPTURE: std::cell::RefCell<Option<Vec<ScriptModelRequest>>> = const { std::cell::RefCell::new(None) };
    /// Observe a live evaluation without diverting its build requests.
    static OBSERVE: std::cell::RefCell<Option<std::collections::BTreeMap<String, String>>> = const { std::cell::RefCell::new(None) };
}

/// Record the last revision requested per alias by `f`, including unchanged
/// requests. Thread-local scope keeps other evaluations and workers out.
pub fn observe_script_models<R>(f: impl FnOnce() -> R) -> (R, Vec<ScriptModelRevision>) {
    struct Scope(Option<std::collections::BTreeMap<String, String>>);
    impl Drop for Scope {
        fn drop(&mut self) { OBSERVE.with(|o| { o.replace(self.0.take()); }); }
    }
    let _scope = Scope(OBSERVE.with(|o| o.replace(Some(Default::default()))));
    let out = f();
    let models = OBSERVE.with(|o| o.borrow_mut().take()).unwrap_or_default();
    (out, models.into_iter().map(|(alias, hash)| ScriptModelRevision { alias, hash }).collect())
}

fn observe_request(alias: &str, hash: &str) {
    OBSERVE.with(|o| { if let Some(models) = o.borrow_mut().as_mut() { models.insert(alias.into(), hash.into()); } });
}

/// Run `f` with every program it posts (`m.build()`, built-in requests) captured instead
/// of queued for the app's build service.
pub fn capture_programs<R>(f: impl FnOnce() -> R) -> (R, Vec<ScriptModelRequest>) {
    let previous = CAPTURE.with(|c| c.borrow_mut().replace(Vec::new()));
    let out = f();
    let captured = CAPTURE.with(|c| std::mem::replace(&mut *c.borrow_mut(), previous)).unwrap_or_default();
    (out, captured)
}

/// Queue a program the app itself owns (built-in effect models). It builds and
/// installs exactly like a level-authored model.
pub fn request_builtin_model(alias: &str, class: &str, program: Vec<ProgramOp>) { post_builtin_model(None, alias, class, program) }
/// Queue a program a script verb recorded for `host` (a character, a recipe's
/// models): the host admits it first (`ModelHost::admit_program`).
pub fn request_model_for(host: &dyn ModelHost, alias: &str, class: &str, program: Vec<ProgramOp>) { post_builtin_model(Some(host), alias, class, program) }
fn post_builtin_model(host: Option<&dyn ModelHost>, alias: &str, class: &str, program: Vec<ProgramOp>) {
    let (hash, bytes) = match program_key(&program, None) {
        Ok(key) => key,
        Err(e) => {
            makepad_platform_log(alias, &e);
            return;
        }
    };
    let request = ScriptModelRequest { alias: alias.into(), hash: hash.clone(), title: alias.rsplit('/').next().unwrap_or(alias).replace('-', " "), class: class.into(), program: Arc::new(program), materials: Arc::new(Vec::new()), texture_timings: Arc::new(Vec::new()) };
    let Some(request) = CAPTURE.with(|c| match c.borrow_mut().as_mut() { Some(v) => { v.push(request); None } None => Some(request) }) else { return };
    let rig = rig_rest(&request.program).map(Arc::new);
    if let Some(Admit::Withhold(reason)) = host.map(|host| host.admit_program(alias, bytes)) {
        withhold(request, None, rig, reason);
        return;
    }
    observe_request(alias, &hash);
    let _retired = registry(|r| {
        r.note_front(alias, class);
        let old_rig = r.note_rig(alias, rig);
        let resumed = r.admit_withheld(alias);
        if r.wanted(alias) == Some(&hash) && !resumed { return (None, old_rig); }
        r.program_bytes.insert(alias.into(), bytes);
        let retired = r.requested.insert(alias.into(), request.clone());
        r.queue.push(request);
        (retired, old_rig)
    }); // Drop the replaced program outside the registry lock.
}
/// The host withheld `request` on this device: its alias stands for a model
/// that is not built here. No worker receives the program (it is dropped:
/// in this step a program is withheld only when it alone exceeds the
/// device's budget) and the level does not wait for it; the app draws a
/// stand-in and reports `reason` once. A later admitted request of the alias
/// builds it. A revision this process already wants and did not withhold
/// (other content asked for the same program) is that model: nothing is
/// withheld and nothing new is built.
fn withhold(request: ScriptModelRequest, bounds: Option<[[f64; 3]; 2]>, rig: Option<Arc<RigRest>>, reason: String) {
    let alias = request.alias.clone();
    let _retired = registry(|r| {
        r.note_front(&alias, &request.class);
        let old_rig = r.note_rig(&alias, rig);
        r.world.insert(alias.clone());
        if let Some(b) = bounds { r.bounds.insert(alias.clone(), b); }
        if r.wanted(&alias) == Some(&request.hash) && !r.withheld.contains_key(&alias) { return (None, old_rig); }
        r.queue.retain(|q| q.alias != alias);
        r.program_bytes.remove(&alias);
        r.withheld.insert(alias.clone(), (request.hash.clone(), reason));
        r.withheld_rev += 1;
        let retired = r.requested.insert(alias, ScriptModelRequest { program: Arc::new(Vec::new()), ..request });
        (retired, old_rig)
    }); // Drop the replaced program outside the registry lock.
}
/// A program's key, FNV-1a over each operation's own key (`op_key`), then the optional class
/// and both generator versions, and the length of the program's canonical encoding. A program
/// that has no encoding (a non-finite number) is refused.
fn program_key(ops: &[ProgramOp], class: Option<&str>) -> Result<(String, usize), String> {
    let keys = ops.iter().map(op_key).collect::<Result<Vec<_>, _>>()?;
    Ok(key_of(&keys, None, class))
}
const FNV_OFFSET: u64 = 0xcbf29ce484222325;
fn fnv(h: u64, bytes: &[u8]) -> u64 { bytes.iter().fold(h, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3)) }
/// The length of an empty program's encoding (its header and count).
fn empty_program_len() -> usize {
    static EMPTY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *EMPTY.get_or_init(|| makepad_model::program_bytes(&[]).map_or(0, |b| b.len()))
}
/// One operation's key, FNV-1a over its canonical encoding, and that encoding's length. The
/// builder keys each operation as it records it, so `m.build` only combines the keys: encoding
/// and hashing a whole program at the build took 4-7 ms for one of race's cars.
fn op_key(op: &ProgramOp) -> Result<(u64, usize), String> {
    let bytes = makepad_model::program_bytes(std::slice::from_ref(op)).map_err(|e| format!("the model program has an invalid value: {}", makepad_model::describe_error(&e)))?;
    let op = &bytes[empty_program_len()..];
    Ok((fnv(FNV_OFFSET, op), op.len()))
}
/// The key of a program from its operations' keys (and one appended operation's), and the
/// length of the program's canonical encoding (`program_bytes`).
fn key_of(keys: &[(u64, usize)], appended: Option<(u64, usize)>, class: Option<&str>) -> (String, usize) {
    let mut len = empty_program_len();
    let mut h = fnv(FNV_OFFSET, b"MPKEY\0\x02");
    h = fnv(h, &((keys.len() + appended.is_some() as usize) as u64).to_le_bytes());
    for (key, bytes) in keys.iter().chain(appended.as_ref()) {
        h = fnv(fnv(h, &key.to_le_bytes()), &(*bytes as u64).to_le_bytes());
        len += bytes;
    }
    let class = class.unwrap_or("");
    h = fnv(fnv(h, &(class.len() as u64).to_le_bytes()), class.as_bytes());
    h = fnv(h, &makepad_model::MODEL_GENERATOR_VERSION.to_le_bytes());
    h = fnv(h, &makepad_model::character::CHARACTER_GENERATOR_VERSION.to_le_bytes());
    (format!("{h:016x}"), len)
}
/// Estimated model-space bounds for an alias a program owner (a recipe)
/// knows before the build lands; placeholders and vehicle bodies size from it.
pub fn set_script_model_bounds(alias: &str, bounds: [[f64; 3]; 2]) { registry(|r| { r.bounds.insert(alias.into(), bounds); }) }
/// The size (x, y, z) of a script model: its estimated bounds.
pub fn script_model_size(alias: &str) -> Option<[f32; 3]> {
    registry(|r| r.bounds.get(alias).map(|b| [(b[1][0] - b[0][0]) as f32, (b[1][1] - b[0][1]) as f32, (b[1][2] - b[0][2]) as f32]))
}
/// The revision the latest eval asked for.
pub fn script_model_wanted(alias: &str) -> Option<String> { registry(|r| r.wanted(alias).cloned()) }
/// Pin the current request so a worker can reject an obsolete snapshot.
pub fn script_model_revision(alias: &str) -> Option<ScriptModelRevision> {
    registry(|r| r.wanted(alias).map(|hash| ScriptModelRevision { alias: alias.into(), hash: hash.clone() }))
}
/// The latest admitted model request recorded for `alias`, without draining the
/// build queue (`take_script_model_requests`).
///
/// Lets an inspector, offline compiler or preview builder read the recorded
/// `ProgramOp` list and metadata for an alias while leaving the process build
/// queue intact. Returns `None` if `alias` has not been requested, was evicted,
/// or had its current revision withheld (whose program is not retained).
pub fn script_model_program(alias: &str) -> Option<ScriptModelRequest> {
    registry(|r| {
        let request = r.requested.get(alias)?;
        if r.withheld.get(alias).is_some_and(|(hash, _)| hash == &request.hash) {
            return None;
        }
        Some(request.clone())
    })
}
/// The app evicted `alias` from residency: a later request builds it again.
pub fn forget_script_model_install(alias: &str) {
    let _retired = registry(|r| { r.installed.remove(alias); r.program_bytes.remove(alias); r.requested.remove(alias) });
}
pub fn is_script_model(alias: &str) -> bool { alias.starts_with(SCRIPT_MODEL_PREFIX) }
/// A fresh world is being evaluated: failures of models the previous world
/// built stop being reported (the registry itself outlives worlds).
pub fn begin_script_model_world() { registry(|r| r.world.clear()) }
/// The running world's script asked for `alias` (since its world began):
/// the model is in use, whether or not a level report has said so yet.
pub fn script_model_in_world(alias: &str) -> bool { registry(|r| r.world.contains(alias)) }
/// This script model's front is -Z (the engine's forward), not glTF's +Z.
pub fn script_model_fronts_neg_z(alias: &str) -> bool { registry(|r| r.front_neg_z.contains(alias)) }
/// A `game.model_new` model of class vehicle (built or still building).
/// LOD chains declared with `m.lod_models`, when they changed since `rev`:
/// (new revision, every chain). The app hands them to the renderer.
pub fn script_model_lod_chains(rev: u64) -> Option<(u64, Vec<(String, Vec<(f32, String)>)>)> {
    registry(|r| (r.lod_rev != rev).then(|| (r.lod_rev, r.lod_chains.iter().map(|(k, v)| (k.clone(), v.clone())).collect())))
}
/// A kit declares that `body` wears `wheel` at `centres` (see `Registry::separate_wheels`).
pub fn set_script_model_separate_wheels(body: &str, wheel: &str, centres: [[f32; 4]; 4]) {
    registry(|r| { r.separate_wheels.insert(body.to_string(), (wheel.to_string(), centres)); })
}
pub fn script_model_separate_wheels(body: &str) -> Option<(String, [[f32; 4]; 4])> { registry(|r| r.separate_wheels.get(body).cloned()) }
pub fn script_model_is_vehicle(alias: &str) -> bool { registry(|r| r.requested.contains_key(alias) && !r.front_neg_z.contains(alias)) }
pub fn script_model_state(alias: &str) -> ScriptModelState {
    registry(|r| r.state(alias))
}

// ── builder state ───────────────────────────────────────────────────────

fn bounds_of(points: impl IntoIterator<Item = [f64; 3]>) -> Option<[[f64; 3]; 2]> {
    let mut b: Option<[[f64; 3]; 2]> = None;
    for p in points { let e = b.get_or_insert([p, p]); for d in 0..3 { e[0][d] = e[0][d].min(p[d]); e[1][d] = e[1][d].max(p[d]); } }
    b
}
fn merge(a: Option<[[f64; 3]; 2]>, b: Option<[[f64; 3]; 2]>) -> Option<[[f64; 3]; 2]> {
    match (a, b) { (Some(a), Some(b)) => bounds_of([a[0], a[1], b[0], b[1]]), (x, None) | (None, x) => x }
}
fn half_box(h: [f64; 3]) -> Option<[[f64; 3]; 2]> { Some([h.map(|v| -v), h]) }
struct ModelBuilder {
    name: String,
    class: String,
    /// Shared with the request a build posts (no copy at the build); a later operation copies
    /// it once.
    ops: Arc<Vec<ProgramOp>>,
    /// Each operation's key (`op_key`), made as it is recorded.
    op_keys: Vec<(u64, usize)>,
    /// The first operation that has no encoding: the build refuses the program.
    op_error: Option<String>,
    materials: Vec<String>,
    texture_timings: Vec<makepad_model::TextureTiming>,
    /// Texture tile size in metres for materials declared with `tile`.
    tiles: HashMap<String, f64>,
    scene: SceneState,
    bounds: BTreeMap<String, [[f64; 3]; 2]>,
    decals: BTreeSet<String>,
    logged_detached_paint: bool,
    rigged: bool,
    /// Rest position of the hips joint when the rig is not the humanoid
    /// template (`m.character`); clip authoring keys the root from it.
    rest_hips: Option<[f64; 3]>,
    /// `m.clip_rest(pose)`: where the author's pose frame sits relative to
    /// the rig's rest (an A-pose rig posed from arms-hanging keys).
    clip_rest: Vec<(String, [f64; 3])>,
    selections: u32,
    /// `m.lod_models`: other built models drawn instead from a distance on.
    lod_models: Vec<(f32, String)>,
    /// Where the model grows when a level scatters it as vegetation
    /// (`game.model_new(name, {habitat})`): see [`script_model_habitat`].
    habitat: Option<String>,
    /// The names (`let`s) of each source body that made parts: its first line and names.
    body_names: HashMap<u16, Option<(u32, Rc<crate::lang::names::Names>)>>,
    /// Call sites looked up: by body, line, column and whether it is the builder call.
    sites: HashMap<(u16, u32, u32, bool), Option<crate::lang::names::Site>>,
    /// How many parts each looped binding made so far (`bands[2]`).
    looped: HashMap<String, usize>,
    /// Text and sign materials' width / height (`m.aspect(mat)`).
    aspects: HashMap<String, f64>,
}
impl ModelBuilder {
    fn translated_bounds(&self, name: &str) -> Option<[[f64; 3]; 2]> {
        let pos = self.scene.nodes[name].transform.translation;
        self.bounds.get(name).map(|b| b.map(|corner| std::array::from_fn(|d| corner[d] + pos[d])))
    }

    fn detached_paint(&self, removed: &[&str]) -> makepad_model::Result<DetachedPaint> {
        let mut moves = DetachedPaint::default();
        let mut pending = Vec::new();
        for name in &self.decals {
            let Some(parent) = self.scene.nodes[name].parent.as_deref() else { continue };
            if removed.contains(&parent) && !removed.contains(&name.as_str()) {
                moves.roots.push(name.clone());
                let frame = self.scene.world_matrix(parent)?;
                if frame != IDENTITY_MATRIX { pending.push((name.as_str(), frame)); }
            }
        }
        if pending.is_empty() { return Ok(moves); }
        let mut children = BTreeMap::<&str, Vec<&str>>::new();
        for (name, node) in &self.scene.nodes {
            if let Some(parent) = node.parent.as_deref() {
                if !removed.contains(&name.as_str()) { children.entry(parent).or_default().push(name); }
            }
        }
        // Keep each surviving node's local TRS. Express the lost parent frame
        // in that node's axes before applying it to the mesh and its children.
        while let Some((name, frame)) = pending.pop() {
            let local = self.scene.nodes[name].transform.matrix()?;
            let bake = matrix_mul(inverse(local)?, matrix_mul(frame, local));
            moves.bakes.push((name.to_owned(), bake));
            if let Some(children) = children.get(name) {
                pending.extend(children.iter().map(|&child| (child, bake)));
            }
        }
        Ok(moves)
    }
}
#[derive(Default)]
struct DetachedPaint {
    roots: Vec<String>,
    bakes: Vec<(String, Matrix4)>,
}
struct BuilderHandle(ModelBuilder);

/// A part or material a builder made: opaque to the script, named by its binding.
/// A selection (`m.select`) is a part's handle carrying its group name in `sel`.
struct PartRef { name: String, material: bool, sel: Option<String> }
impl ScriptHandleGc for PartRef { fn gc(&mut self) {} }
struct PartType(ScriptHandleType);
impl ScriptHandleGc for PartType { fn gc(&mut self) {} }
const PART_TYPE_GLOBAL: &str = "__model_part_type";

thread_local! {
    /// Parsed sources by content hash: every model a game builds reads the same file.
    static BODY_NAMES: std::cell::RefCell<HashMap<u64, Rc<crate::lang::names::Names>>> = std::cell::RefCell::new(HashMap::new());
}

fn body_names(text: &str) -> Option<Rc<crate::lang::names::Names>> {
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() { h = (h ^ byte as u64).wrapping_mul(0x100000001b3); }
    if let Some(n) = BODY_NAMES.with(|m| m.borrow().get(&h).cloned()) { return Some(n); }
    let n = Rc::new(crate::lang::names::parse(text).ok()?);
    BODY_NAMES.with(|m| { let mut m = m.borrow_mut(); if m.len() > 16 { m.clear(); } m.insert(h, n.clone()); });
    Some(n)
}

fn part_type(vm: &mut ScriptVm) -> ScriptHandleType {
    let key = LiveId::from_str(PART_TYPE_GLOBAL);
    if let Some(ty) = vm.injected_global(key).and_then(|v| v.as_handle()).and_then(|h| vm.bx.heap_ref().handle_ref::<PartType>(h).map(|t| t.0)) {
        return ty;
    }
    let ty = vm.new_handle_type(key);
    let proto = vm.bx.heap_mut().new_handle(ty, Box::new(PartType(ty)));
    vm.set_injected_global(key, proto.into());
    ty
}
impl ScriptHandleGc for BuilderHandle { fn gc(&mut self) {} }
/// Kept in the VM as an injected global: the builder handle type exists once
/// per VM and dies with it, so no process state can outlive its VM.
struct BuilderType(ScriptHandleType);
impl ScriptHandleGc for BuilderType { fn gc(&mut self) {} }
const TYPE_GLOBAL: &str = "__model_builder_type";

fn builder_type(vm: &mut ScriptVm, host: &Rc<dyn ModelHost>) -> ScriptHandleType {
    let key = LiveId::from_str(TYPE_GLOBAL);
    if let Some(ty) = vm.injected_global(key).and_then(|v| v.as_handle()).and_then(|h| vm.bx.heap_ref().handle_ref::<BuilderType>(h).map(|t| t.0)) {
        return ty;
    }
    let ty = vm.new_handle_type(key);
    let host = host.clone();
    vm.set_handle_call(ty, move |vm, args, method| host.call_method(method, &mut || method_call(vm, &*host, args, method)));
    let proto = vm.bx.heap_mut().new_handle(ty, Box::new(BuilderType(ty)));
    vm.set_injected_global(key, proto.into());
    ty
}

pub fn slug(name: &str) -> String {
    let s: String = name.trim().to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() { "model".into() } else { s.chars().take(48).collect() }
}

/// The habitats a model may name: broadleaf trees grow low, conifers higher
/// up to the tree line, shrubs a little above it (libs/gen ecology.rs).
pub const HABITATS: &[&str] = &["broadleaf", "conifer", "shrub"];

/// The habitat a built model named (`game.model_new(name, {habitat})`),
/// for level scatters: None for a model that is not vegetation.
pub fn script_model_habitat(alias: &str) -> Option<String> { registry(|r| r.habitats.get(alias).cloned()) }

/// `game.model_new(name, {class, habitat})` -> builder. Class is prop,
/// vehicle, character, weapon or building (library category only);
/// habitat (vegetation only) is broadleaf, conifer or shrub.
pub fn v_model_new(vm: &mut ScriptVm, host: &Rc<dyn ModelHost>, args: ScriptObject) -> ScriptValue {
    let name = arg_string(vm, args, 0);
    let mut class = "prop".to_string();
    let mut habitat = None;
    if let Some(o) = arg(vm, args, 1).as_object() {
        warn_unknown_keys(vm, &|message| host.log(message), "model_new", o, &[id!(class), id!(habitat)]);
        class = opt_string(vm, o, id!(class)).unwrap_or(class);
        habitat = opt_string(vm, o, id!(habitat));
    }
    if let Some(h) = habitat.as_deref().filter(|h| !HABITATS.contains(h)) {
        host.log(format!("game.model_new: habitat '{h}' unknown ({}); ignored", HABITATS.join(", ")));
        habitat = None;
    }
    if !["prop", "vehicle", "character", "weapon", "building"].contains(&class.as_str()) {
        host.log(format!("game.model_new: class '{class}' unknown (prop, vehicle, character, weapon, building); using prop"));
        class = "prop".into();
    }
    let ty = builder_type(vm, host);
    let builder = ModelBuilder { name: slug(&name), class, ops: Arc::new(Vec::new()), op_keys: Vec::new(), op_error: None, materials: Vec::new(), texture_timings: Vec::new(), tiles: HashMap::new(), scene: SceneState::default(), bounds: BTreeMap::new(), decals: BTreeSet::new(), logged_detached_paint: false, rigged: false, rest_hips: None, clip_rest: Vec::new(), selections: 0, lod_models: Vec::new(), habitat, body_names: HashMap::new(), sites: HashMap::new(), looped: HashMap::new(), aspects: HashMap::new() };
    vm.bx.heap_mut().new_handle(ty, Box::new(BuilderHandle(builder))).into()
}

/// `game.model_status(alias)` -> "ready" | "building" | "failed: why" | "withheld: why" | "unknown".
/// The app's side: the aliases its build pool is working on now.
pub fn set_script_models_running(running: Vec<String>) { registry(|r| r.running = running); }

/// (done, total, queued, current) for the level models of the current batch;
/// effect models (`gen/mdl/fx-*`) are not counted. All zero when idle.
pub fn script_build_progress() -> (usize, usize, usize, Option<String>) {
    registry(|r| {
        let level = |a: &String| !a.starts_with("gen/mdl/fx-");
        let pending: Vec<&String> = r.requested.iter()
            .filter(|(a, request)| level(a) && !r.settled(a, &request.hash))
            .map(|(a, _)| a).collect();
        if pending.is_empty() { r.batch = 0; return (0, 0, 0, None); }
        // A batch grows while new requests arrive and ends when none pend.
        let running: Vec<&String> = r.running.iter().filter(|a| level(a)).collect();
        r.batch = r.batch.max(pending.len());
        let current = running.first().map(|a| a.trim_start_matches(SCRIPT_MODEL_PREFIX).to_string());
        (r.batch - pending.len(), r.batch, pending.len().saturating_sub(running.len()), current)
    })
}

pub fn v_build_progress(vm: &mut ScriptVm) -> ScriptValue {
    let (done, total, queued, current) = script_build_progress();
    let out = vm.bx.heap_mut().new_object();
    vm.bx.heap_mut().set_object_storage_auto(out);
    for (key, value) in [("done", done), ("total", total), ("queued", queued)] {
        vm.bx.heap_mut().set_value(out, LiveId::from_str(key).into(), ScriptValue::from_f64(value as f64), NoTrap);
    }
    let current = match current { Some(name) => vm.bx.heap_mut().new_string_from_str(&name), None => ScriptValue::NIL };
    vm.bx.heap_mut().set_value(out, LiveId::from_str("current").into(), current, NoTrap);
    out.into()
}

pub fn v_model_status(vm: &mut ScriptVm, args: ScriptObject) -> ScriptValue {
    let alias = arg_string(vm, args, 0);
    let text = match script_model_state(&alias) {
        ScriptModelState::Ready => "ready".to_string(),
        ScriptModelState::Building => "building".into(),
        ScriptModelState::Failed(e) => format!("failed: {e}"),
        ScriptModelState::Withheld(reason) => format!("withheld: {reason}"),
        ScriptModelState::Unknown => "unknown".into(),
    };
    vm.bx.heap_mut().new_string_from_str(&text)
}

// ── value conversion ────────────────────────────────────────────────────

fn srgb_to_linear(c: f32) -> f64 { let c = c.clamp(0., 1.) as f64; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } }

fn opt(vm: &mut ScriptVm, o: Option<ScriptObject>, key: LiveId) -> ScriptValue { o.map_or(NIL, |o| opts_value(vm, o, key)) }
fn opt_num(vm: &mut ScriptVm, o: Option<ScriptObject>, key: LiveId) -> Option<f64> {
    let v = opt(vm, o, key);
    if v.is_nil() { None } else { Some(value_f32(vm, v) as f64) }
}
fn opt_text(vm: &mut ScriptVm, o: Option<ScriptObject>, key: LiveId) -> Option<String> { let v = opt(vm, o, key); if v.is_nil() { None } else { Some(value_string(vm, v)) } }
fn opt_v3(vm: &mut ScriptVm, o: Option<ScriptObject>, key: LiveId) -> Option<[f64; 3]> {
    let v = opt(vm, o, key);
    if v.is_nil() { return None; }
    let p = value_vec3(vm, v);
    Some([p.x as f64, p.y as f64, p.z as f64])
}
fn value_v2(vm: &mut ScriptVm, v: ScriptValue) -> [f64; 2] {
    if v.as_array().is_some() {
        crate::value::wrong_type(vm, "a vec2", "write vec2(x, y); an array like [x, y] is not a point");
        return [0., 0.];
    }
    let ip = vm.trap_ip();
    { let numeric = makepad_script::numeric::NumericValue::from_script_value_heap(vm.heap(), v, ip); match numeric.kind() {
        makepad_script::numeric::NumericValueTag::Vec2 => { let p = numeric.as_vec2().unwrap(); [p.x as f64, p.y as f64] },
        makepad_script::numeric::NumericValueTag::Vec3 => { let p = numeric.as_vec3().unwrap(); [p.x as f64, p.y as f64] },
        makepad_script::numeric::NumericValueTag::F64 => { let x = numeric.as_f64().unwrap(); [x, x] },
        _ => [0., 0.],
    } }
}
fn points2(vm: &mut ScriptVm, v: ScriptValue) -> Vec<[f64; 2]> { let n = list_len(vm, v); (0..n).map(|i| { let p = list_value(vm, v, i); value_v2(vm, p) }).collect() }
fn points3(vm: &mut ScriptVm, v: ScriptValue) -> Vec<[f64; 3]> { let n = list_len(vm, v); (0..n).map(|i| { let p = list_value(vm, v, i); let q = value_vec3(vm, p); [q.x as f64, q.y as f64, q.z as f64] }).collect() }
fn color_linear(vm: &mut ScriptVm, v: ScriptValue) -> [f64; 4] {
    let ip = vm.trap_ip();
    { let numeric = makepad_script::numeric::NumericValue::from_script_value_heap(vm.heap(), v, ip); match numeric.kind() {
        makepad_script::numeric::NumericValueTag::Color => { let c = numeric.as_color().unwrap(); [srgb_to_linear(c.x), srgb_to_linear(c.y), srgb_to_linear(c.z), c.w as f64] },
        makepad_script::numeric::NumericValueTag::Vec4 => { let c = numeric.as_vec4().unwrap(); [c.x as f64, c.y as f64, c.z as f64, c.w as f64] },
        makepad_script::numeric::NumericValueTag::Vec3 => { let c = numeric.as_vec3().unwrap(); [c.x as f64, c.y as f64, c.z as f64, 1.] },
        _ => { let c = value_color(vm, v); [srgb_to_linear(c.x), srgb_to_linear(c.y), srgb_to_linear(c.z), c.w as f64] }
    } }
}
fn quat_euler_deg(r: [f64; 3]) -> [f64; 4] {
    let h = r.map(|d| d.to_radians() * 0.5);
    let (qx, qy, qz) = ([h[0].sin(), 0., 0., h[0].cos()], [0., h[1].sin(), 0., h[1].cos()], [0., 0., h[2].sin(), h[2].cos()]);
    quat_mul(qz, quat_mul(qy, qx))
}
fn quat_mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1], a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
     a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3], a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2]]
}
fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let p = quat_mul(quat_mul(q, [v[0], v[1], v[2], 0.]), [-q[0], -q[1], -q[2], q[3]]);
    [p[0], p[1], p[2]]
}
/// Row-major affine rotation*scale matrix (no translation).
fn rs_matrix(q: [f64; 4], s: [f64; 3]) -> [[f64; 4]; 4] {
    let [x, y, z, w] = q;
    let r = [[1. - 2. * (y * y + z * z), 2. * (x * y - z * w), 2. * (x * z + y * w)],
        [2. * (x * y + z * w), 1. - 2. * (x * x + z * z), 2. * (y * z - x * w)],
        [2. * (x * z - y * w), 2. * (y * z + x * w), 1. - 2. * (x * x + y * y)]];
    std::array::from_fn(|i| if i == 3 { [0., 0., 0., 1.] } else { [r[i][0] * s[0], r[i][1] * s[1], r[i][2] * s[2], 0.] })
}

// ── methods ─────────────────────────────────────────────────────────────

const SHAPE_KEYS: &[&str] = &["pos", "rot", "scale", "parent", "material"];

fn method_call(vm: &mut ScriptVm, host: &dyn ModelHost, args: ScriptObject, method: LiveId) -> ScriptValue {
    let this = vm.bx.heap_ref().value(args, id!(self).into(), NoTrap);
    let Some(handle) = this.as_handle() else { return NIL };
    if vm.bx.heap_ref().handle_ref::<BuilderHandle>(handle).is_none() { return NIL; }
    let m = method.to_string();
    let mut recorder = Recorder { vm, host, args, handle, method: m.clone(), error: None };
    let result = recorder.dispatch(&m);
    if let Some(error) = recorder.error.take() {
        let name = vm.bx.heap_ref().handle_ref::<BuilderHandle>(handle).map(|b| b.0.name.clone()).unwrap_or_default();
        host.error(format!("model '{name}'.{m}: {error}"));
    }
    result
}

struct Recorder<'a, 'b> { vm: &'a mut ScriptVm<'b>, host: &'a dyn ModelHost, args: ScriptObject, handle: ScriptHandle, method: String, error: Option<String> }
impl Recorder<'_, '_> {
    fn b(&mut self) -> &mut ModelBuilder { &mut self.vm.bx.heap_mut().handle_mut::<BuilderHandle>(self.handle).unwrap().0 }
    fn fail(&mut self, e: impl Into<String>) -> ScriptValue { self.error = Some(e.into()); NIL }
    fn arg(&mut self, i: usize) -> ScriptValue { arg(self.vm, self.args, i) }
    fn text(&mut self, i: usize) -> String { arg_string(self.vm, self.args, i) }
    fn opts(&mut self, i: usize, allowed: &[&str]) -> Option<ScriptObject> {
        let o = self.arg(i).as_object()?;
        let n = self.vm.bx.heap_ref().iter_len(o);
        for k in 0..n {
            let kv = self.vm.bx.heap_ref().iter_key_value(o, k, NoTrap);
            if let Some(key) = kv.key.as_id() {
                let key = key.to_string();
                if !allowed.contains(&key.as_str()) { self.host.log(format!("model.{}: unknown option `{key}` (ignored); supported: {}", self.method, allowed.join(", "))); }
            }
        }
        Some(o)
    }
    fn push(&mut self, op: ProgramOp) {
        let key = op_key(&op);
        let b = self.b();
        match key {
            Ok(key) => b.op_keys.push(key),
            Err(e) => { b.op_keys.push((0, 0)); b.op_error.get_or_insert(e); }
        }
        Arc::make_mut(&mut b.ops).push(op);
    }
    fn op(&mut self, op: Operation) { self.push(ProgramOp::Op(op)); }
    fn construction(&mut self, object: &str, kind: Construction) {
        self.op(Operation::Construction(ConstructionOperation { object: object.into(), kind }));
    }
    fn edit(&mut self, object: &str, action: MeshAction) {
        self.op(Operation::MeshEditing(MeshEditingOperation { object: object.into(), action }));
    }
    /// Counts were truncated to signed integers before the operation parser
    /// checked their u32 range. Keep that conversion, including its failures.
    fn counts<const N: usize>(&mut self, values: [f64; N]) -> Option<[u32; N]> {
        let mut counts = [0; N];
        for (out, value) in counts.iter_mut().zip(values) {
            match u32::try_from(value as i64) {
                Ok(value) => *out = value,
                Err(_) => { self.error = Some("expected u32 integer".into()); return None; }
            }
        }
        Some(counts)
    }
    /// A part or material handle's name; a string or anything else is an error.
    fn ref_name(&mut self, v: ScriptValue, material: bool, what: &str) -> Option<String> {
        let found = v.as_handle().and_then(|h| self.vm.bx.heap_ref().handle_ref::<PartRef>(h)).map(|p| (p.name.clone(), p.material, p.sel.is_some()));
        match found {
            Some((name, m, false)) if m == material => Some(name),
            _ => {
                let want = if material { "a material (the value m.material(..) returned)" } else { "a part (the value m.box(..) returned)" };
                let got = if v.is_string_like() { format!("the string \"{}\"", value_string(self.vm, v)) } else if v.is_nil() { "nothing".into() } else { "another value".into() };
                self.error = Some(format!("{what} needs {want}, got {got}: pass its binding"));
                None
            }
        }
    }
    /// The part at argument `i`.
    fn part(&mut self, i: usize) -> Option<String> { let v = self.arg(i); self.ref_name(v, false, "this call") }
    /// The part in option `key`, if given.
    fn opt_part(&mut self, o: Option<ScriptObject>, key: LiveId) -> Option<String> {
        let v = opt(self.vm, o, key);
        if v.is_nil() { return None; }
        self.ref_name(v, false, &key.to_string())
    }
    fn handle_of(&mut self, name: &str, material: bool) -> ScriptValue {
        let ty = part_type(self.vm);
        self.vm.bx.heap_mut().new_handle(ty, Box::new(PartRef { name: name.into(), material, sel: None })).into()
    }
    /// The bindings round the call made at `ip` (`own`: this builder call, else a script
    /// call it is made inside), from the names of the source that holds it.
    fn site_at(&mut self, ip: ScriptIp, own: bool) -> Option<crate::lang::names::Site> {
        let loc = self.vm.ip_to_loc(ip)?;
        let key = (ip.body, loc.line, loc.col, own);
        if let Some(site) = self.b().sites.get(&key) { return site.clone(); }
        let names = match self.b().body_names.get(&ip.body) {
            Some(n) => n.clone(),
            None => {
                let n = self.vm.body_text(ip).and_then(|(base, text)| body_names(&text).map(|n| (base, n)));
                self.b().body_names.insert(ip.body, n.clone());
                n
            }
        };
        let site = names.and_then(|(base, names)| {
            let at = (loc.line.checked_sub(base)? + 1, loc.col + 1);
            if own { names.site(Some(at)).cloned() } else { names.call_site(at) }
        });
        self.b().sites.insert(key, site.clone());
        site
    }
    /// A new part's name: the `let`s round its call and round the script
    /// calls it is made in (`left.body`), indexed where a `let` holds a list
    /// (`bands[2]`); a call inside another's arguments gets its builder after
    /// the binding (`hull.box`); an unbound call its builder and line.
    fn new_name(&mut self) -> String {
        let ip = self.vm.trap_ip();
        let mut sites = Vec::new();
        self.vm.call_sites(&mut sites, 16);
        let mut binds = Vec::new();
        for at in sites.iter().skip(1).rev() {
            if let Some(site) = self.site_at(*at, false) { binds.extend(site.binds); }
        }
        let own = self.site_at(ip, true);
        let owns = own.as_ref().is_some_and(|s| s.owns);
        if let Some(site) = own { binds.extend(site.binds); }
        let call = self.method.clone();
        let base = binds.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(".");
        let mut name = match binds.iter().rposition(|b| b.looped) {
            Some(at) => {
                let k = { let n = self.b().looped.entry(format!("{base}/{}", if owns { "" } else { &call })).or_default(); *n += 1; *n - 1 };
                binds.iter().enumerate().map(|(j, b)| if j == at { format!("{}[{k}]", b.name) } else { b.name.clone() }).collect::<Vec<_>>().join(".")
            }
            None => base,
        };
        if name.is_empty() {
            let line = self.vm.ip_to_loc(ip).map_or(0, |l| l.line);
            name = format!("{call}@{line}");
        } else if !owns {
            name = format!("{name}.{call}");
        }
        let taken = |b: &ModelBuilder, n: &str| b.scene.nodes.contains_key(n) || b.materials.iter().any(|m| m == n);
        if taken(self.b(), &name) {
            let first = name.clone();
            name = (2..).map(|n| format!("{first}#{n}")).find(|c| !taken(self.b(), c)).unwrap();
        }
        name
    }
    fn material(&mut self, o: Option<ScriptObject>, key: LiveId) -> Option<u32> {
        let v = opt(self.vm, o, key);
        if v.is_nil() { return None; }
        let name = self.ref_name(v, true, &key.to_string())?;
        self.b().materials.iter().position(|m| *m == name).map(|i| i as u32 + 1)
    }
    fn name_ok(&mut self, name: &str) -> bool {
        if name.is_empty() || name.chars().any(char::is_control) { self.error = Some(format!("object name '{name}' must be nonempty and have no control characters")); return false; }
        true
    }
    fn declare(&mut self, name: &str) -> bool {
        if !self.name_ok(name) { return false; }
        if self.b().scene.nodes.contains_key(name) { self.error = Some(format!("object '{name}' already exists; pick another name or m.delete(\"{name}\") first")); return false; }
        true
    }
    fn select_all(&mut self, object: &str, element: &str, operation: Operation) {
        let sel = { let b = self.b(); b.selections += 1; format!("__s{}", b.selections) };
        self.op(Operation::Selection(SelectionOperation::Query {
            object: object.into(), name: sel.clone(), query: SelectionQuery::All,
            element: element.into(), min: 1, max: 2_000_000,
        }));
        self.op(Operation::Selection(SelectionOperation::Use {
            object: object.into(), name: sel.clone(), element: element.into(), operation: Box::new(operation),
        }));
        self.op(Operation::Selection(SelectionOperation::Delete { object: object.into(), name: sel }));
    }
    fn emit_node(&mut self, name: &str) {
        let node = self.b().scene.nodes[name].clone();
        self.op(Operation::Scene(SceneOperation::Node {
            object: name.into(),
            node,
        }));
    }
    fn transform_mesh(&mut self, name: &str, matrix: Matrix4) {
        if matrix != IDENTITY_MATRIX {
            self.select_all(name, "vertices", Operation::Transform { object: name.into(), vertices: Vec::new(), matrix });
        }
    }
    fn remove_nodes(&mut self, removed: &[&str], parent: Option<&str>, moves: DetachedPaint) {
        for (name, matrix) in moves.bakes {
            self.transform_mesh(&name, matrix);
            if let Some(bounds) = self.b().bounds.get_mut(&name) {
                let corners = (0..8).map(|i| transform_point(matrix, [bounds[i & 1][0], bounds[(i >> 1) & 1][1], bounds[(i >> 2) & 1][2]]));
                *bounds = bounds_of(corners).unwrap();
            }
        }
        if parent.is_none() && !moves.roots.is_empty() && !self.b().logged_detached_paint {
            self.host.log("model.delete: paint from a deleted target moved to the model root".into());
            self.b().logged_detached_paint = true;
        }
        for name in moves.roots {
            self.b().scene.nodes.get_mut(&name).unwrap().parent = parent.map(str::to_owned);
            self.emit_node(&name);
        }
        for &name in removed {
            self.op(Operation::DeleteObject { object: name.to_owned() });
            self.b().scene.nodes.remove(name);
            self.b().decals.remove(name);
            self.b().bounds.remove(name);
        }
    }
    /// A new object: rot/scale are baked into its vertices, so the node keeps
    /// only its position (booleans, wheels and mirrors stay simple).
    fn finish_shape_bounded(&mut self, name: &str, o: Option<ScriptObject>, smooth: Option<f64>, local: Option<[[f64; 3]; 2]>) {
        let rot = opt_v3(self.vm, o, id!(rot));
        let scale = {
            let v = opt(self.vm, o, id!(scale));
            if v.is_nil() { None } else if let Some(s) = v.as_number() { Some([s; 3]) } else { let p = value_vec3(self.vm, v); Some([p.x as f64, p.y as f64, p.z as f64]) }
        };
        if let Some(angle) = smooth { self.edit(name, MeshAction::Normals { smooth: true, angle: angle.to_radians() }); }
        let mut local = local;
        if rot.is_some() || scale.is_some() {
            let (q, s) = (rot.map_or([0., 0., 0., 1.], quat_euler_deg), scale.unwrap_or([1.; 3]));
            local = local.and_then(|b| bounds_of((0..8).map(|i| rotate(q, [b[i & 1][0] * s[0], b[(i >> 1) & 1][1] * s[1], b[(i >> 2) & 1][2] * s[2]]))));
            let m = rs_matrix(q, s);
            self.select_all(name, "vertices", Operation::Transform { object: name.into(), vertices: Vec::new(), matrix: m });
        }
        let parent = self.opt_part(o, id!(parent));
        if let Some(p) = &parent { if !self.b().scene.nodes.contains_key(p) { self.error = Some(format!("parent '{p}' was deleted")); } }
        let pos = opt_v3(self.vm, o, id!(pos)).unwrap_or([0.; 3]);
        self.b().scene.nodes.insert(name.into(), SceneNode {
            parent: parent.filter(|p| p != name),
            transform: Transform { translation: pos, ..Transform::default() },
            ..SceneNode::default()
        });
        if let Some(local) = local { self.b().bounds.insert(name.into(), local); }
        if pos != [0.; 3] || self.b().scene.nodes[name].parent.is_some() { self.emit_node(name); }
    }
    fn shape(&mut self, shape: Shape, keys: &[&str], local: Option<[[f64; 3]; 2]>) -> ScriptValue {
        let name = self.new_name();
        let mut allowed = keys.to_vec(); allowed.extend_from_slice(SHAPE_KEYS);
        let o = self.opts(0, &allowed);
        if !self.declare(&name) { return NIL; }
        let material = self.material(o, id!(material)).unwrap_or(0);
        self.construction(&name, Construction::Shape { shape, material });
        self.tile_uv(&name, material);
        self.finish_shape_bounded(&name, o, None, local);
        self.handle_of(&name, false)
    }
    /// Parts made of a tiled material get box UVs at the tile scale, so
    /// fine patterns (knurling, grain) stay fine on any part size.
    fn tile_uv(&mut self, object: &str, material: u32) {
        let Some(name) = material.checked_sub(1).and_then(|i| self.b().materials.get(i as usize).cloned()) else { return };
        if let Some(t) = self.b().tiles.get(&name).copied() {
            self.select_all(object, "faces", Operation::MeshEditing(MeshEditingOperation { object: object.into(), action: MeshAction::BoxUv { faces: Vec::new(), scale: [1. / t; 2], offset: [0.; 2] } }));
        }
    }
    fn num(&mut self, i: usize, key: LiveId, default: f64) -> f64 { let o = self.arg(i).as_object(); opt_num(self.vm, o, key).unwrap_or(default) }
    fn flag(&mut self, i: usize, key: LiveId, default: bool) -> bool { let o = self.arg(i).as_object(); o.map_or(default, |o| opt_bool(self.vm, o, key, default)) }
    fn smooth_opt(&mut self, i: usize, default: Option<f64>) -> Option<f64> {
        let v = { let o = self.arg(i).as_object(); opt(self.vm, o, id!(smooth)) };
        if v.is_nil() { default } else if let Some(b) = v.as_bool() { b.then_some(default.unwrap_or(40.)) } else { Some(value_f32(self.vm, v) as f64) }
    }
    fn known(&mut self, name: &str) -> bool {
        if self.b().scene.nodes.contains_key(name) { return true; }
        self.error = Some(format!("unknown object '{name}'"));
        false
    }

    fn dispatch(&mut self, m: &str) -> ScriptValue {
        match m {
            "material" => self.m_material(),
            "image" => self.m_image(),
            "box" => {
                /// Grid-arc chord error in model units (metres): half a pixel of the
                /// LOD screen reference (2160 px tall, 62 degree vertical FOV) at 1 m.
                /// Use the smallest m >= 1 with r * (1 - cos(PI / (8 * m))) at most
                /// this target, capped by ceil(segments / 2). Tan-spaced corner grid
                /// steps turn by at most (PI / 4) / m; cylindrical edges attain it.
                /// Record 2 * m - 1 at authoring time so stored ops replay unchanged.
                const CHORD_TOLERANCE: f64 = 0.00025;
                let size = { let o = self.arg(0).as_object(); opt_v3(self.vm, o, id!(size)).unwrap_or([1.; 3]) };
                let (radius, segments) = (self.num(0, id!(radius), 0.), self.num(0, id!(segments), 3.));
                let Some([segments]) = self.counts([segments]) else { return NIL };
                // Leave invalid counts for the existing Shape::Box validation.
                let segments = if radius > 0. && segments >= 1 {
                    let r = radius.min(size[0] * 0.5).min(size[1] * 0.5).min(size[2] * 0.5);
                    let mut m = 1;
                    while m < segments.div_ceil(2) && r * (1. - (std::f64::consts::FRAC_PI_4 / (2. * m as f64)).pcos()) > CHORD_TOLERANCE { m += 1; }
                    2 * m - 1
                } else { segments };
                self.shape(Shape::Box { size, radius, segments }, &["size", "radius", "segments"], half_box(size.map(|v| v * 0.5)))
            }
            "sphere" => {
                let r = self.num(0, id!(radius), 0.5); let seg = self.num(0, id!(segments), 32.); let rings = self.num(0, id!(rings), 16.);
                let Some([segments, rings]) = self.counts([seg, (rings / 2.).ceil().max(1.)]) else { return NIL };
                // Preserve the capsule with no straight section used by this verb.
                self.shape(Shape::Capsule { radius: r, height: r * 2., segments, rings }, &["radius", "segments", "rings"], half_box([r; 3]))
            }
            "cylinder" => {
                let (r, h, seg) = (self.num(0, id!(radius), 0.5), self.num(0, id!(height), 1.), self.num(0, id!(segments), 32.));
                let smooth = self.flag(0, id!(smooth), true);
                let Some([segments]) = self.counts([seg]) else { return NIL };
                // Preserve the equal-radius cone this verb previously recorded.
                self.shape(Shape::Cone { bottom: r, top: r, height: h, segments, smooth }, &["radius", "height", "segments", "smooth"], half_box([r, h * 0.5, r]))
            }
            "cone" => {
                let (r, t, h, seg) = (self.num(0, id!(radius), 0.5), self.num(0, id!(top_radius), 0.), self.num(0, id!(height), 1.), self.num(0, id!(segments), 32.));
                let smooth = self.flag(0, id!(smooth), true);
                let Some([segments]) = self.counts([seg]) else { return NIL };
                self.shape(Shape::Cone { bottom: r, top: t, height: h, segments, smooth }, &["radius", "top_radius", "height", "segments", "smooth"], half_box([r.max(t), h * 0.5, r.max(t)]))
            }
            "capsule" => {
                let (r, h, seg, rings) = (self.num(0, id!(radius), 0.3), self.num(0, id!(height), 1.), self.num(0, id!(segments), 24.), self.num(0, id!(rings), 6.));
                let Some([segments, rings]) = self.counts([seg, rings]) else { return NIL };
                self.shape(Shape::Capsule { radius: r, height: h.max(r * 2.), segments, rings }, &["radius", "height", "segments", "rings"], half_box([r, h.max(r * 2.) * 0.5, r]))
            }
            "torus" => {
                let (r, t, seg, sides) = (self.num(0, id!(radius), 0.5), self.num(0, id!(tube), 0.1), self.num(0, id!(segments), 32.), self.num(0, id!(sides), 12.));
                let Some([segments, sides]) = self.counts([seg, sides]) else { return NIL };
                self.shape(Shape::Torus { radius: r, tube: t, segments, sides }, &["radius", "tube", "segments", "sides"], half_box([r + t, t, r + t]))
            }
            "wedge" => {
                let size = { let o = self.arg(0).as_object(); opt_v3(self.vm, o, id!(size)).unwrap_or([1.; 3]) };
                self.shape(Shape::Wedge { size }, &["size"], half_box(size.map(|v| v * 0.5)))
            }
            "extrude" => self.m_extrude(),
            "lathe" => self.m_lathe(),
            "loft" => self.m_loft(),
            "sweep" => self.m_sweep(),
            "tube" => self.m_tube(),
            "mesh" => self.m_mesh(),
            "wall" => self.m_wall(),
            "stair" => self.m_stair(),
            "roof" => self.m_roof(),
            "move" => self.m_move(),
            "union" => self.m_boolean("union"),
            "subtract" => self.m_boolean("difference"),
            "intersect" => self.m_boolean("intersection"),
            "mirror" => self.m_mirror(),
            "subdivide" => {
                let Some(name) = self.part(0) else { return NIL }; let levels = { let v = self.arg(1); if v.is_nil() { 1. } else { value_f32(self.vm, v) as f64 } };
                if self.known(&name) { self.edit(&name, MeshAction::Subdivide { levels: levels.clamp(1., 3.) as u32 }); }
                NIL
            }
            "smooth" | "flat" => {
                let Some(name) = self.part(0) else { return NIL };
                let angle = if m == "flat" { 0. } else { let v = self.arg(1); if v.is_nil() { 40. } else { value_f32(self.vm, v) as f64 } };
                if self.known(&name) { self.edit(&name, MeshAction::Normals { smooth: m == "smooth", angle: angle.to_radians() }); }
                NIL
            }
            "uv" => self.m_uv(),
            "bevel" => {
                let Some(name) = self.part(0) else { return NIL };
                let o = self.opts(1, &["width", "segments", "angle"]);
                let (w, seg, ang) = (opt_num(self.vm, o, id!(width)).unwrap_or(0.02), opt_num(self.vm, o, id!(segments)).unwrap_or(3.), opt_num(self.vm, o, id!(angle)).unwrap_or(30.));
                let Some([segments]) = self.counts([seg]) else { return NIL };
                if self.known(&name) { self.edit(&name, MeshAction::RoundBevel { width: w, segments, angle: ang.to_radians() }); }
                NIL
            }
            "crease" => {
                let Some(name) = self.part(0) else { return NIL };
                let o = self.opts(1, &["angle", "weight"]);
                let (ang, w) = (opt_num(self.vm, o, id!(angle)).unwrap_or(30.), opt_num(self.vm, o, id!(weight)).unwrap_or(1.));
                if self.known(&name) { self.edit(&name, MeshAction::Crease { angle: ang.to_radians(), weight: w.clamp(0., 1.) }); }
                NIL
            }
            "decal" => self.m_decal(),
            "text_material" => self.m_text_material(),
            "sign_material" => self.m_sign_material(),
            "weathering" => {
                let o = self.opts(0, &["ao", "distance", "edges", "samples", "objects"]);
                let ao = opt_num(self.vm, o, id!(ao)).unwrap_or(0.6);
                let distance = opt_num(self.vm, o, id!(distance)).unwrap_or(0.25);
                let edges = opt_num(self.vm, o, id!(edges)).unwrap_or(0.5);
                let samples = opt_num(self.vm, o, id!(samples)).unwrap_or(16.).clamp(1., 64.) as u32;
                let objects = opt(self.vm, o, id!(objects));
                let objects = if !objects.is_nil() {
                    let n = list_len(self.vm, objects);
                    let mut names = Vec::new();
                    for i in 0..n { let x = list_value(self.vm, objects, i); match self.ref_name(x, false, "weathering objects") { Some(name) => names.push(name), None => return NIL } }
                    Some(names)
                } else { None };
                self.push(ProgramOp::Weathering { ao, distance, edges, samples, objects });
                NIL
            }
            "paint" => {
                let v = self.arg(1);
                let Some(mat) = self.ref_name(v, true, "paint") else { return NIL };
                let Some(i) = self.b().materials.iter().position(|x| *x == mat) else { return self.fail(format!("unknown material '{mat}'")) };
                let target = self.arg(0);
                if target.as_handle().and_then(|h| self.vm.bx.heap_ref().handle_ref::<PartRef>(h)).is_some_and(|p| p.sel.is_some()) {
                    return self.on_selection("faces", Operation::AssignMaterial { object: String::new(), faces: Vec::new(), material: i as u32 + 1 });
                }
                let Some(name) = self.part(0) else { return NIL };
                if self.known(&name) { self.select_all(&name, "faces", Operation::AssignMaterial { object: name.clone(), faces: Vec::new(), material: i as u32 + 1 }); }
                NIL
            }
            "delete" => {
                let Some(name) = self.part(0) else { return NIL };
                if self.known(&name) {
                    match self.b().detached_paint(&[&name]) {
                        Ok(moves) => self.remove_nodes(&[&name], None, moves),
                        Err(error) => return self.fail(makepad_model::describe_error(&error)),
                    }
                }
                NIL
            }
            "socket" => self.m_socket(),
            "wheel" => self.m_wheel(),
            "pivot" => self.m_pivot(),
            "collider" => {
                let Some(name) = self.part(0) else { return NIL }; let kind = self.text(1);
                if !["box", "sphere", "hull", "capsule", "mesh"].contains(&kind.as_str()) { return self.fail(format!("collider kind '{kind}' (box, sphere, capsule, hull, mesh)")); }
                if self.known(&name) {
                    let proxy = match kind.as_str() {
                        "box" => CollisionProxy::Box, "sphere" => CollisionProxy::Sphere,
                        "hull" => CollisionProxy::Hull, "capsule" => CollisionProxy::Capsule,
                        _ => CollisionProxy::Mesh { object: name.clone() },
                    };
                    self.op(Operation::Scene(SceneOperation::Collider { object: name, proxy }));
                }
                NIL
            }
            "lod" => self.m_lod(),
            "lod_models" => {
                // [{model: alias, distance}]: whole other models (built with
                // game.model_new) drawn in this one's place from `distance`
                // on; the shadow uses the last. For hand-built mid and far
                // versions (a far car as a box on four wheels).
                let list = self.arg(0);
                let n = list_len(self.vm, list);
                let mut chain = Vec::new();
                for i in 0..n {
                    let item = list_value(self.vm, list, i).as_object();
                    let model = opt_text(self.vm, item, id!(model)).unwrap_or_default();
                    let d = opt_num(self.vm, item, id!(distance)).unwrap_or(0.);
                    if model.is_empty() || d <= 0. { return self.fail("lod_models needs [{model, distance}] with a built model alias and a distance > 0"); }
                    chain.push((d as f32, model));
                }
                self.b().lod_models = chain;
                NIL
            }
            "impostor" => {
                // The build host bakes the far stand-in. Format changes are
                // covered by the generator versions in program_key.
                let d = self.num(0, id!(distance), 120.).clamp(10., 5000.);
                let cell = self.num(0, id!(cell), 128.).clamp(32., 256.).round();
                self.push(ProgramOp::Impostor { distance: d, cell: cell as u32 });
                NIL
            }
            "light" => self.m_light(),
            "rig_humanoid" => self.m_rig(),
            "character" => self.m_character(),
            "clip" => self.m_clip(),
            "clip_rest" => self.m_clip_rest(),
            "bind" => {
                let Some(name) = self.part(0) else { return NIL }; let joint = self.text(1);
                if !self.b().rigged && !self.b().ops.iter().any(|v| matches!(v, ProgramOp::ImportGlb { .. } | ProgramOp::Op(Operation::SoftBody(_)))) { return self.fail("bind needs m.rig_humanoid(), m.soft_body(..) or an imported rig first"); }
                if self.known(&name) { self.push(ProgramOp::Bind { object: name, joint }); }
                NIL
            }
            "bind_all" => { self.push(ProgramOp::BindAll); NIL }
            "import" => self.m_import(),
            "build" => self.m_build(),
            "aspect" => {
                let v = self.arg(0);
                let Some(name) = self.ref_name(v, true, "aspect") else { return NIL };
                match self.b().aspects.get(&name).copied() { Some(a) => ScriptValue::from_f64(a), None => self.fail(format!("material '{name}' is no text or sign material")) }
            }
            _ => match self.edit_verb(m) { Some(v) => v, None => self.fail(format!("unknown model method '{m}' — see game.api() under model_new")) },
        }
    }

    /// `m.image(material, {image: "name", channel, layer})`: a layer of an image the
    /// document keeps (a generated texture) on a material, with its mips.
    fn m_image(&mut self) -> ScriptValue {
        let v = self.arg(0);
        let Some(name) = self.ref_name(v, true, "m.image's material") else { return NIL };
        let Some(index) = self.b().materials.iter().position(|m| *m == name).map(|i| i as u32 + 1) else { return self.fail(format!("m.image: '{name}' is not a material")) };
        let o = self.opts(1, &["image", "channel", "layer"]);
        let Some(image) = opt_text(self.vm, o, id!(image)) else { return self.fail("m.image needs {image: \"name\"}") };
        let channel = match opt_text(self.vm, o, id!(channel)).as_deref().unwrap_or("base_color") {
            "base_color" => SurfaceChannel::BaseColor,
            "normal" => SurfaceChannel::Normal,
            "metallic_roughness" => SurfaceChannel::MetallicRoughness,
            "occlusion" => SurfaceChannel::Occlusion,
            "emissive" => SurfaceChannel::Emissive,
            other => return self.fail(format!("m.image channel '{other}': base_color, normal, metallic_roughness, occlusion or emissive")),
        };
        let layer = opt_text(self.vm, o, id!(layer)).unwrap_or_else(|| "image".into());
        self.push(ProgramOp::ImageLayer { material: index, channel, layer: layer.clone(), image });
        self.op(Operation::Surface(SurfaceOperation::Mips { material: index, channel, layer }));
        NIL
    }
    fn m_material(&mut self) -> ScriptValue {
        let name = self.new_name();
        let o = self.opts(0, &["color", "metallic", "roughness", "emissive", "emissive_strength", "alpha", "double_sided", "pattern", "tile", "wind", "clearcoat", "flake", "rim", "crown", "cutoff", "lamp"]);
        let index = match self.b().materials.iter().position(|m| *m == name) { Some(i) => i, None => { self.b().materials.push(name.clone()); self.b().materials.len() - 1 } } as u32 + 1;
        if index >= 256 { return self.fail("too many materials (255)"); }
        // `tile`: the pattern repeats every this many metres on the parts
        // using it (they get a box UV projection at that scale).
        if let Some(t) = opt_num(self.vm, o, id!(tile)).filter(|t| *t > 1e-4) { self.b().tiles.insert(name.clone(), t); }
        let color = { let v = opt(self.vm, o, id!(color)); if v.is_nil() { [0.8, 0.8, 0.8, 1.] } else { color_linear(self.vm, v) } };
        let alpha_v = opt(self.vm, o, id!(alpha));
        // A foliage card pattern cuts out between its leaves by default.
        // `cut: true` on any pattern does the same (a fence's mesh, a grille).
        let foliage = opt(self.vm, o, id!(pattern)).as_object().is_some_and(|p| {
            opt_text(self.vm, Some(p), id!(kind)).is_some_and(|k| k == "leaves" || k == "needles") || opt_bool(self.vm, p, id!(cut), false)
        });
        let (alpha_mode, alpha) = if alpha_v.is_nil() && foliage { ("mask", color[3]) } else if alpha_v.is_nil() { ("opaque", color[3]) } else if let Some(a) = alpha_v.as_number() { (if a < 0.999 { "blend" } else { "opaque" }, a) } else { let s = value_string(self.vm, alpha_v); (if s == "mask" { "mask" } else if s == "blend" { "blend" } else { "opaque" }, color[3]) };
        let pattern = opt(self.vm, o, id!(pattern)).as_object();
        let mut surface = SurfaceMaterial {
            base_color: if pattern.is_some() { [1.; 4] } else { [color[0], color[1], color[2], alpha] },
            metallic: opt_num(self.vm, o, id!(metallic)).unwrap_or(0.).clamp(0., 1.),
            roughness: opt_num(self.vm, o, id!(roughness)).unwrap_or(0.6).clamp(0., 1.),
            alpha: match alpha_mode { "mask" => SurfaceAlpha::Mask, "blend" => SurfaceAlpha::Blend, _ => SurfaceAlpha::Opaque },
            ..Default::default()
        };
        let em = opt(self.vm, o, id!(emissive));
        if !em.is_nil() {
            let c = color_linear(self.vm, em);
            surface.emissive = [c[0], c[1], c[2]];
            surface.emissive_strength = opt_num(self.vm, o, id!(emissive_strength)).unwrap_or(1.);
        }
        surface.double_sided = o.is_some_and(|o| opt_bool(self.vm, o, id!(double_sided), false));
        if let Some(c) = opt_num(self.vm, o, id!(cutoff)) { surface.alpha_cutoff = c.clamp(0.05, 0.95); }
        // Foliage wind and race paint (makepadShading), all 0..1.
        let term = |vm: &mut ScriptVm, key: LiveId| opt_num(vm, o, key).map_or(0., |v: f64| v.clamp(0., 1.));
        let (wind, clearcoat, flake, rim) = (term(self.vm, id!(wind)), term(self.vm, id!(clearcoat)), term(self.vm, id!(flake)), term(self.vm, id!(rim)));
        let crown = o.is_some_and(|o| opt_bool(self.vm, o, id!(crown), false));
        // A vehicle signal lamp: its emission is the lamp fully lit, and
        // each car lights it from its own brake and reverse state.
        let lamp = match opt_text(self.vm, o, id!(lamp)) {
            Some(l) => match GlbLamp::from_name(&l) {
                Some(lamp) => Some(lamp),
                None => return self.fail(format!("material lamp '{l}': tail, brake or reverse")),
            },
            None => None,
        };
        if wind > 0. || clearcoat > 0. || flake > 0. || rim > 0. || crown || lamp.is_some() {
            surface.shading = Some(GlbShading { wind, clearcoat, flake, rim: if rim > 0. { rim } else { 0. }, crown: if crown { 1.0 } else { 0.0 }, lamp, ..Default::default() });
        }
        self.op(Operation::SetMaterial { material: index, value: Material { color: [color[0], color[1], color[2]], base_color_png: Vec::new() } });
        self.op(Operation::Surface(SurfaceOperation::Material { material: index, value: surface }));
        if let Some(p) = pattern {
            let kind = opt_text(self.vm, Some(p), id!(kind)).unwrap_or_else(|| "noise".into());
            let kind = match kind.as_str() {
                "checker" => PatternKind::Checker, "stripes" => PatternKind::Stripes,
                "gradient" => PatternKind::Gradient, "noise" => PatternKind::Noise,
                "perlin" => PatternKind::Perlin, "fbm" => PatternKind::Fbm,
                "yarn" => PatternKind::Yarn, "bricks" => PatternKind::Bricks,
                "tiles" => PatternKind::Tiles, "planks" => PatternKind::Planks,
                "vents" => PatternKind::Vents, "grille" => PatternKind::Grille,
                "knurl" => PatternKind::Knurl, "camo" => PatternKind::Camo,
                "leaves" => PatternKind::Leaves, "needles" => PatternKind::Needles,
                _ => return self.fail(format!("surface pattern kind '{kind}'")),
            };
            let mut joint = { let v = opts_value(self.vm, p, id!(color)); if v.is_nil() { [color[0] * 0.5, color[1] * 0.5, color[2] * 0.5, 1.] } else { color_linear(self.vm, v) } };
            // Between the leaves is the cut-out: the joint colour's alpha is 0.
            if foliage { joint[3] = 0.; }
            let scale = { let v = opts_value(self.vm, p, id!(scale)); if v.is_nil() { [8., 8.] } else { value_v2(self.vm, v) } };
            let size = opt_num(self.vm, Some(p), id!(size)).unwrap_or(256.).clamp(32., 1024.);
            let size = (size as u32).next_power_of_two().min(1024);
            let seed = opt_num(self.vm, Some(p), id!(seed)).unwrap_or(1.) as u64;
            let bump = opt_num(self.vm, Some(p), id!(bump)).unwrap_or(0.);
            let texture_timer = makepad_model::TextureGenerationTimer::start();
            let layer = match SurfaceLayer::blank("pattern", SurfaceChannel::BaseColor, size, size, &makepad_model::Limits::default()) {
                Ok(layer) => layer, Err(e) => return self.fail(makepad_model::describe_error(&e)),
            };
            self.b().texture_timings.push(texture_timer.finish(index, SurfaceChannel::BaseColor, kind.name(), size, size));
            self.op(Operation::Surface(SurfaceOperation::Layer { material: index, channel: SurfaceChannel::BaseColor, layer }));
            self.op(Operation::Surface(SurfaceOperation::Pattern {
                material: index, channel: SurfaceChannel::BaseColor, layer: "pattern".into(),
                // Preserve the material colour's alpha between pattern features.
                pattern: SurfacePattern { kind, color_a: joint, color_b: color, scale, seed },
            }));
            self.op(Operation::Surface(SurfaceOperation::Mips { material: index, channel: SurfaceChannel::BaseColor, layer: "pattern".into() }));
            if bump > 0. {
                self.op(Operation::Surface(SurfaceOperation::Derive {
                    material: index, channel: SurfaceChannel::Normal, layer: "bump".into(),
                    recipe: SurfaceDerivation { source_channel: SurfaceChannel::BaseColor, source_layer: None, strength: bump.min(16.), roughness_min: 0.2, roughness_max: 0.9, metallic: 0., wrap: true },
                }));
                self.op(Operation::Surface(SurfaceOperation::Mips { material: index, channel: SurfaceChannel::Normal, layer: "bump".into() }));
            }
        }
        self.handle_of(&name, true)
    }
    fn m_extrude(&mut self) -> ScriptValue {
        let pts = { let v = self.arg(0); points2(self.vm, v) };
        let axis = { let o = self.arg(1).as_object(); match opt_text(self.vm, o, id!(axis)).as_deref() { Some("x") => 0, Some("y") => 1, _ => 2 } };
        let depth = self.num(1, id!(depth), 1.);
        let center = self.flag(1, id!(center), true);
        let (cr, cs, bv, bs) = (self.num(1, id!(corner_radius), 0.), self.num(1, id!(corner_segments), 4.), self.num(1, id!(bevel), 0.), self.num(1, id!(bevel_segments), 3.));
        let Some([corner_segments, bevel_segments]) = self.counts([cs, bs]) else { return NIL };
        let name = self.new_name();
        let opts = self.opts(1, &["depth", "axis", "center", "corner_radius", "corner_segments", "bevel", "bevel_segments", "pos", "rot", "scale", "parent", "material"]);
        if !self.declare(&name) { return NIL; }
        let material = self.material(opts, id!(material)).unwrap_or(0);
        let (w0, w1) = if center { (-depth * 0.5, depth * 0.5) } else { (0., depth) };
        let local = bounds_of(pts.iter().flat_map(|p| [w0, w1].map(|w| match axis { 0 => [w, p[1], p[0]], 1 => [p[0], w, p[1]], _ => [p[0], p[1], w] })));
        self.construction(&name, Construction::Shape { material, shape: Shape::Extrude {
            profile: pts, depth, axis, center, corner_radius: cr,
            corner_segments, bevel: bv, bevel_segments,
        } });
        self.tile_uv(&name, material);
        self.finish_shape_bounded(&name, opts, None, local);
        self.handle_of(&name, false)
    }
    fn simple_new(&mut self, opts_slot: usize, keys: &[&str]) -> Option<(String, Option<ScriptObject>, u32)> {
        let name = self.new_name();
        let mut allowed = keys.to_vec(); allowed.extend_from_slice(SHAPE_KEYS);
        let o = self.opts(opts_slot, &allowed);
        if !self.declare(&name) { return None; }
        let material = self.material(o, id!(material)).unwrap_or(0);
        Some((name, o, material))
    }
    fn m_lathe(&mut self) -> ScriptValue {
        let pts = { let v = self.arg(0); points2(self.vm, v) };
        let smooth = self.smooth_opt(1, Some(40.));
        let Some((name, o, material)) = self.simple_new(1, &["segments", "caps", "smooth"]) else { return NIL };
        let segments = opt_num(self.vm, o, id!(segments)).unwrap_or(32.);
        let Some([segments]) = self.counts([segments]) else { return NIL };
        let caps = o.is_none_or(|o| opt_bool(self.vm, o, id!(caps), true));
        let rmax = pts.iter().map(|p| p[0].abs()).fold(0., f64::max);
        let local = bounds_of(pts.iter().flat_map(|p| [[-rmax, p[1], -rmax], [rmax, p[1], rmax]]));
        // A closed solid faces out whichever way its profile runs.
        self.construction(&name, Construction::Lathe { profile: pts, axis: 1, segments, caps, material, outward: true });
        self.tile_uv(&name, material);
        self.finish_shape_bounded(&name, o, smooth, local);
        self.handle_of(&name, false)
    }
    fn m_loft(&mut self) -> ScriptValue {
        let rings = { let v = self.arg(0); let n = list_len(self.vm, v); (0..n).map(|i| { let r = list_value(self.vm, v, i); points3(self.vm, r) }).collect::<Vec<_>>() };
        let smooth = self.smooth_opt(1, Some(40.));
        let Some((name, o, material)) = self.simple_new(1, &["closed", "caps", "smooth"]) else { return NIL };
        let closed = o.is_none_or(|o| opt_bool(self.vm, o, id!(closed), true));
        let caps = o.is_none_or(|o| opt_bool(self.vm, o, id!(caps), closed));
        let local = bounds_of(rings.iter().flatten().copied());
        self.construction(&name, Construction::Loft { profiles: rings, closed, caps: caps && closed, material });
        self.tile_uv(&name, material);
        self.finish_shape_bounded(&name, o, smooth, local);
        self.handle_of(&name, false)
    }
    fn m_sweep(&mut self) -> ScriptValue {
        let (profile, path) = { let a = self.arg(0); let b = self.arg(1); (points2(self.vm, a), points3(self.vm, b)) };
        let smooth = self.smooth_opt(2, Some(40.));
        let Some((name, o, material)) = self.simple_new(2, &["caps", "smooth"]) else { return NIL };
        let caps = o.is_none_or(|o| opt_bool(self.vm, o, id!(caps), true));
        let pr = profile.iter().map(|p| p[0].hypot(p[1])).fold(0., f64::max);
        let local = bounds_of(path.iter().flat_map(|p| [[p[0] - pr, p[1] - pr, p[2] - pr], [p[0] + pr, p[1] + pr, p[2] + pr]]));
        self.construction(&name, Construction::Sweep { profile, path, caps, material });
        self.finish_shape_bounded(&name, o, smooth, local);
        self.handle_of(&name, false)
    }
    fn m_tube(&mut self) -> ScriptValue {
        let path = { let v = self.arg(0); points3(self.vm, v) };
        let Some((name, o, material)) = self.simple_new(1, &["radius", "radii", "curve", "steps", "segments", "caps"]) else { return NIL };
        let radii = opt(self.vm, o, id!(radii));
        let curve = o.is_some_and(|o| opt_bool(self.vm, o, id!(curve), false));
        let uniform_radius = radii.is_nil().then(|| opt_num(self.vm, o, id!(radius)).unwrap_or(0.05));
        let segments = opt_num(self.vm, o, id!(segments)).unwrap_or(12.);
        let Some([segments]) = self.counts([segments]) else { return NIL };
        let caps = o.is_none_or(|o| opt_bool(self.vm, o, id!(caps), true));
        if let Some(radius) = uniform_radius.filter(|_| !curve) {
            let local = bounds_of(path.iter().flat_map(|p| [p.map(|v| v - radius), p.map(|v| v + radius)]));
            self.construction(&name, Construction::Shape { material, shape: Shape::Tube { path, radius, segments, caps } });
            self.finish_shape_bounded(&name, o, None, local);
        } else {
            let steps = if curve { opt_num(self.vm, o, id!(steps)).unwrap_or(6.).trunc() } else { 1. };
            if !(1.0..=32.0).contains(&steps) { return self.fail(format!("tube '{name}': steps must be in 1..32")); }
            if segments < 3 { return self.fail(format!("tube '{name}': segments must be at least 3")); }
            let controls = match self.tube_controls(path, radii, uniform_radius) {
                Ok(controls) => controls,
                Err(e) => return self.fail(format!("tube '{name}': {e}")),
            };
            let profiles = Self::tube_profiles(&controls, steps as u32, segments);
            let local = bounds_of(profiles.iter().flatten().copied());
            self.construction(&name, Construction::Loft { profiles, closed: true, caps, material });
            self.tile_uv(&name, material);
            self.finish_shape_bounded(&name, o, Some(40.), local);
        }
        self.handle_of(&name, false)
    }

    fn tube_controls(&mut self, path: Vec<[f64; 3]>, radii: ScriptValue, uniform_radius: Option<f64>) -> Result<Vec<(Vec3d, f64)>, String> {
        if let Some(radius) = uniform_radius {
            if !radius.is_finite() || radius <= 0. { return Err("radius must be finite and positive".into()); }
        } else {
            let count = list_len(self.vm, radii);
            if count != path.len() {
                return Err(format!("radii needs one radius per path point ({count} radii for {} path points)", path.len()));
            }
        }
        let mut controls: Vec<(Vec3d, f64)> = Vec::with_capacity(path.len());
        let mut last_index = 0;
        for (i, [x, y, z]) in path.into_iter().enumerate() {
            let point = Vec3d::new(x, y, z);
            if !point.is_finite() { return Err(format!("path[{i}] must be finite")); }
            let radius = match uniform_radius {
                Some(radius) => radius,
                None => list_value(self.vm, radii, i).as_number().filter(|r| r.is_finite() && *r >= 0.)
                    .ok_or_else(|| format!("radii[{i}] must be a finite nonnegative number"))?,
            };
            if let Some(&(last_point, last_radius)) = controls.last() {
                // Match Shape::Tube's adjacent-point tolerance, keeping the first radius.
                if point.distance(last_point) < 1e-7 { continue; }
                if controls.len() > 1 && last_radius == 0. {
                    return Err(format!("radii[{last_index}] is zero inside the path; only an end can be a point"));
                }
            }
            controls.push((point, radius));
            last_index = i;
        }
        if controls.len() < 2 { return Err("tube path needs two distinct points".into()); }
        let last = controls.len() - 1;
        if controls[0].1 == 0. { controls[0].1 = controls[1].1 * 0.02; }
        if controls[last].1 == 0. { controls[last].1 = controls[last - 1].1 * 0.02; }
        Ok(controls)
    }

    fn tube_profiles(controls: &[(Vec3d, f64)], steps: u32, segments: u32) -> Vec<Vec<[f64; 3]>> {
        let last = controls.len() - 1;
        // Catmull-Rom derivative in knot time. Mirroring the phantom neighbour
        // at either end makes its chord equal to the adjacent real chord.
        let slope = |i: usize| {
            let left = i.saturating_sub(1);
            let right = i.min(last - 1);
            let a = controls[left + 1].0 - controls[left].0;
            let b = controls[right + 1].0 - controls[right].0;
            let (da, db) = (a.length().sqrt(), b.length().sqrt());
            (a * (db / da) + b * (da / db)) / (da + db)
        };
        let circle: Vec<_> = (0..segments).map(|j| (std::f64::consts::TAU * j as f64 / segments as f64).psin_cos()).collect();
        let mut start_slope = slope(0);
        let mut previous = start_slope.normalize();
        let axis = if previous.x.abs() <= previous.y.abs() { Vec3d::X } else { Vec3d::Y };
        let mut normal = previous.cross(axis).normalize();
        let mut profiles = Vec::new();
        let mut ring = |center: Vec3d, tangent: Vec3d, radius: f64| {
            // The shortest rotation is two reflections: the first, across the
            // old tangent's normal plane, fixes this normal. At a half turn
            // the bisector is zero and the old normal is the rotation axis.
            let bisector = (previous + tangent).normalize();
            normal = normal - bisector * (2. * bisector.dot(normal));
            let binormal = tangent.cross(normal).normalize();
            normal = binormal.cross(tangent);
            profiles.push(circle.iter().map(|&(sin, cos)| {
                let p = center + (normal * cos + binormal * sin) * radius;
                [p.x, p.y, p.z]
            }).collect());
            previous = tangent;
        };
        for (i, pair) in controls.windows(2).enumerate() {
            let ((start, r0), (end, r1)) = (pair[0], pair[1]);
            let chord = end - start;
            let end_slope = slope(i + 1);
            let dt = chord.length().sqrt();
            // Hermite cubic in unit span time, evaluated with its derivative.
            let (m0, m1) = (start_slope * dt, end_slope * dt);
            let b = chord * 3. - m0 * 2. - m1;
            let c = m0 + m1 - chord * 2.;
            for j in 0..steps {
                let t = j as f64 / steps as f64;
                let center = start + ((c * t + b) * t + m0) * t;
                let velocity = (c * (3. * t) + b * 2.) * t + m0;
                // At an exact reversal the derivative vanishes. Use the outgoing chord.
                let tangent = if velocity.length_sq() == 0. { chord } else { velocity }.normalize();
                ring(center, tangent, r0 * (1. - t) + r1 * t);
            }
            start_slope = end_slope;
        }
        ring(controls[last].0, start_slope.normalize(), controls[last].1);
        profiles
    }

    fn m_mesh(&mut self) -> ScriptValue {
        let positions = { let v = self.arg(0); points3(self.vm, v) };
        let faces = { let v = self.arg(1); let n = list_len(self.vm, v); (0..n).map(|i| { let f = list_value(self.vm, v, i); let k = list_len(self.vm, f); (0..k).map(|j| { let x = list_value(self.vm, f, j); value_f32(self.vm, x) as i64 }).collect::<Vec<_>>() }).collect::<Vec<_>>() };
        let smooth = self.smooth_opt(2, None);
        let Some((name, o, material)) = self.simple_new(2, &["smooth", "uvs", "normals", "ao"]) else { return NIL };
        // `uvs`: one vec2 per vertex (foliage cards, decals on a mesh).
        let uvs = { let v = opt(self.vm, o, id!(uvs)); if v.is_nil() { Vec::new() } else { let n = list_len(self.vm, v); (0..n).map(|i| { let p = list_value(self.vm, v, i); value_v2(self.vm, p) }).collect::<Vec<_>>() } };
        if !uvs.is_empty() && uvs.len() != positions.len() { return self.fail(format!("mesh '{name}': uvs needs one vec2 per vertex ({} for {} vertices)", uvs.len(), positions.len())); }
        let normals = { let v = opt(self.vm, o, id!(normals)); if v.is_nil() { Vec::new() } else { points3(self.vm, v) } };
        if !normals.is_empty() && normals.len() != positions.len() { return self.fail(format!("mesh '{name}': normals needs one vec3 per vertex ({} for {} vertices)", normals.len(), positions.len())); }
        let ao = opt(self.vm, o, id!(ao));
        let colors = if ao.is_nil() { Vec::new() } else {
            let n = list_len(self.vm, ao);
            let mut colors = Vec::with_capacity(n);
            for i in 0..n {
                let value = list_value(self.vm, ao, i);
                let Some(value) = value.as_number().filter(|v| v.is_finite()) else { return self.fail(format!("mesh '{name}': ao needs finite numbers")) };
                // Baked AO darkens RGB in every renderer; alpha stays opaque
                // because surface materials use COLOR_0 alpha for opacity.
                let value = value.clamp(0., 1.);
                colors.push([value, value, value, 1.]);
            }
            colors
        };
        if !colors.is_empty() && colors.len() != positions.len() { return self.fail(format!("mesh '{name}': ao needs one number per vertex ({} for {} vertices)", colors.len(), positions.len())); }
        let mut polygons = Vec::with_capacity(faces.len());
        for face in faces {
            let vertices = match face.into_iter().map(u32::try_from).collect::<Result<Vec<_>, _>>() {
                Ok(vertices) => vertices, Err(_) => return self.fail("mesh vertex indices must be u32 integers"),
            };
            let face_uvs = if uvs.is_empty() { Vec::new() } else { vertices.iter().map(|i| uvs.get(*i as usize).copied().unwrap_or([0., 0.])).collect() };
            polygons.push(makepad_model::mesh::Polygon { vertices, uvs: face_uvs, material });
        }
        let local = bounds_of(positions.iter().copied());
        // The supplied field takes precedence over generated smoothing normals.
        let smooth = if normals.is_empty() { smooth } else { None };
        self.push(ProgramOp::Mesh(MeshPrimitive::Polygons { object: name.clone(), positions, polygons, normals, colors }));
        self.finish_shape_bounded(&name, o, smooth, local);
        self.handle_of(&name, false)
    }
    fn m_wall(&mut self) -> ScriptValue {
        let path = { let v = self.arg(0); points2(self.vm, v) };
        let Some((name, o, material)) = self.simple_new(1, &["height", "thickness", "base", "closed", "openings", "inner_material", "trim_material"]) else { return NIL };
        let height = opt_num(self.vm, o, id!(height)).unwrap_or(3.);
        let thickness = opt_num(self.vm, o, id!(thickness)).unwrap_or(0.2);
        let base = opt_num(self.vm, o, id!(base)).unwrap_or(0.);
        let closed = o.is_some_and(|o| opt_bool(self.vm, o, id!(closed), false));
        let inner_material = self.material(o, id!(inner_material));
        let trim_material = self.material(o, id!(trim_material));
        let list = opt(self.vm, o, id!(openings));
        let n = list_len(self.vm, list);
        let mut openings = Vec::new();
        for i in 0..n {
            let item = list_value(self.vm, list, i).as_object();
            openings.push(Opening {
                at: opt_num(self.vm, item, id!(at)).unwrap_or(0.),
                width: opt_num(self.vm, item, id!(width)).unwrap_or(1.),
                height: opt_num(self.vm, item, id!(height)).unwrap_or(2.1),
                sill: opt_num(self.vm, item, id!(sill)).unwrap_or(0.),
                frame: opt_num(self.vm, item, id!(frame)).unwrap_or(0.),
                frame_depth: opt_num(self.vm, item, id!(frame_depth)).unwrap_or(0.),
                frame_material: self.material(item, id!(frame_material)).unwrap_or(0),
                glass: self.material(item, id!(glass)),
                mullions: item.is_some_and(|it| opt_bool(self.vm, it, id!(mullions), false)),
            });
        }
        let local = bounds_of(path.iter().flat_map(|p| [[p[0] - thickness, base, p[1] - thickness], [p[0] + thickness, base + height, p[1] + thickness]]));
        self.construction(&name, Construction::Shape { material, shape: Shape::Wall(Wall { path, closed, height, thickness, base, openings, inner_material, trim_material }) });
        self.finish_shape_bounded(&name, o, None, local);
        self.handle_of(&name, false)
    }
    fn m_stair(&mut self) -> ScriptValue {
        let Some((name, o, material)) = self.simple_new(0, &["from", "to", "width", "steps", "thickness"]) else { return NIL };
        let from = opt_v3(self.vm, o, id!(from)).unwrap_or([0.; 3]); let to = opt_v3(self.vm, o, id!(to)).unwrap_or([0., 3., 4.]);
        let width = opt_num(self.vm, o, id!(width)).unwrap_or(1.);
        let thickness = opt_num(self.vm, o, id!(thickness)).unwrap_or(0.15);
        let steps = opt_num(self.vm, o, id!(steps)).unwrap_or(0.);
        let Some([steps]) = self.counts([steps]) else { return NIL };
        let steps = if steps == 0 { (((to[1] - from[1]) / 0.18).round() as u32).max(2) } else { steps };
        let w = width * 0.5;
        let local = bounds_of([from.map(|v| v - w), to.map(|v| v + w)]);
        self.construction(&name, Construction::Shape { material, shape: Shape::Stair { from, to, width, steps, thickness } });
        self.finish_shape_bounded(&name, o, None, local);
        self.handle_of(&name, false)
    }
    fn m_roof(&mut self) -> ScriptValue {
        let Some((name, o, material)) = self.simple_new(0, &["kind", "center", "size", "pitch", "overhang", "thickness", "ridge", "trim_material", "gable_material", "gable_thickness"]) else { return NIL };
        let size = { let v = opt(self.vm, o, id!(size)); if v.is_nil() { [6., 5.] } else { value_v2(self.vm, v) } };
        let kind = match opt_text(self.vm, o, id!(kind)).as_deref().unwrap_or("gable") {
            "gable" => RoofKind::Gable, "hip" => RoofKind::Hip, "flat" => RoofKind::Flat,
            _ => return self.fail("roof kind must be gable, hip or flat"),
        };
        let center = opt_v3(self.vm, o, id!(center)).unwrap_or([0.; 3]);
        let pitch = opt_num(self.vm, o, id!(pitch)).unwrap_or(35.);
        let overhang = opt_num(self.vm, o, id!(overhang)).unwrap_or(0.3);
        let thickness = opt_num(self.vm, o, id!(thickness)).unwrap_or(0.12);
        let ridge = match opt_text(self.vm, o, id!(ridge)).as_deref() {
            None => if size[0] >= size[1] { 0 } else { 2 }, Some("x") => 0, Some("z") => 2,
            _ => return self.fail("roof ridge must be x or z"),
        };
        let trim_material = self.material(o, id!(trim_material));
        let gable_material = self.material(o, id!(gable_material));
        let gable_thickness = opt_num(self.vm, o, id!(gable_thickness)).unwrap_or(0.).max(0.);
        let rise = size[0].min(size[1]) * 0.5 * pitch.to_radians().tan() + 0.3;
        let local = bounds_of([[center[0] - size[0] * 0.5 - overhang, center[1] - overhang, center[2] - size[1] * 0.5 - overhang], [center[0] + size[0] * 0.5 + overhang, center[1] + rise, center[2] + size[1] * 0.5 + overhang]]);
        self.construction(&name, Construction::Shape { material, shape: Shape::Roof(Roof { kind, center, size, pitch, overhang, thickness, ridge, trim_material, gable_material, gable_thickness }) });
        self.finish_shape_bounded(&name, o, None, local);
        self.handle_of(&name, false)
    }
    fn m_move(&mut self) -> ScriptValue {
        let Some(name) = self.part(0) else { return NIL };
        let o = self.opts(1, &["pos", "rot", "scale", "parent"]);
        if !self.known(&name) { return NIL; }
        let mut n = self.b().scene.nodes[&name].clone();
        if let Some(p) = opt_v3(self.vm, o, id!(pos)) { n.transform.translation = p; }
        if let Some(r) = opt_v3(self.vm, o, id!(rot)) { n.transform.rotation = quat_euler_deg(r); }
        let s = opt(self.vm, o, id!(scale));
        if !s.is_nil() { n.transform.scale = if let Some(x) = s.as_number() { [x; 3] } else { let p = value_vec3(self.vm, s); [p.x as f64, p.y as f64, p.z as f64] }; }
        if let Some(p) = self.opt_part(o, id!(parent)) { n.parent = (p != name).then_some(p); }
        self.b().scene.nodes.insert(name.clone(), n);
        self.emit_node(&name);
        NIL
    }
    fn m_boolean(&mut self, mode: &str) -> ScriptValue {
        let (Some(a), Some(b)) = (self.part(0), self.part(1)) else { return NIL };
        let name = self.new_name();
        let keep = self.flag(2, id!(keep), false);
        if !self.known(&a) || !self.known(&b) || !self.declare(&name) { return NIL; }
        let bounds_a = self.b().translated_bounds(&a);
        let local = if mode == "union" { merge(bounds_a, self.b().translated_bounds(&b)) } else { bounds_a };
        let moves = if keep { None } else {
            match self.b().detached_paint(&[&a, &b]) {
                Ok(moves) => Some(moves),
                Err(error) => return self.fail(makepad_model::describe_error(&error)),
            }
        };
        self.construction(&name, Construction::Boolean { a: a.clone(), b: b.clone(), mode: match mode { "union" => BooleanMode::Union, "difference" => BooleanMode::Difference, _ => BooleanMode::Intersection } });
        // Boolean results are baked in model space.
        self.b().scene.nodes.insert(name.clone(), SceneNode::default());
        if let Some(local) = local { self.b().bounds.insert(name.clone(), local); }
        if let Some(moves) = moves { self.remove_nodes(&[&a, &b], Some(&name), moves); }
        self.handle_of(&name, false)
    }
    fn m_mirror(&mut self) -> ScriptValue {
        let Some(name) = self.part(0) else { return NIL };
        let axis = match self.text(1).as_str() { "y" => 1, "z" => 2, _ => 0 };
        let offset = self.num(2, id!(offset), 0.);
        if self.known(&name) { self.edit(&name, MeshAction::Mirror { axis, offset }); }
        NIL
    }
    fn m_decal(&mut self) -> ScriptValue {
        let Some(target) = self.part(0) else { return NIL };
        let name = self.new_name();
        let o = self.opts(1, &["pos", "normal", "up", "size", "depth", "offset", "material"]);
        if !self.known(&target) || !self.declare(&name) { return NIL; }
        let material = self.material(o, id!(material)).unwrap_or(0);
        let size = { let v = opt(self.vm, o, id!(size)); if v.is_nil() { [0.1, 0.1] } else { value_v2(self.vm, v) } };
        let center = opt_v3(self.vm, o, id!(pos)).unwrap_or([0.; 3]);
        let normal = opt_v3(self.vm, o, id!(normal)).unwrap_or([0., 0., 1.]);
        // The parser normalizes through the same portable vector length.
        if makepad_model::transform::length(normal) <= 1e-12 { return self.fail("degenerate construction direction"); }
        let normal = match makepad_model::transform::normalized(normal) {
            Ok(normal) => normal, Err(e) => return self.fail(makepad_model::describe_error(&e)),
        };
        let up = opt_v3(self.vm, o, id!(up)).unwrap_or([0., 1., 0.]);
        let depth = opt_num(self.vm, o, id!(depth)).unwrap_or(size[0].max(size[1]) * 0.5);
        let offset = opt_num(self.vm, o, id!(offset)).unwrap_or(0.0015);
        let to_target = match self.b().scene.world_matrix(&target).and_then(inverse) {
            Ok(matrix) => matrix,
            Err(error) => return self.fail(makepad_model::describe_error(&error)),
        };
        self.construction(&name, Construction::Decal { material, decal: Decal { target: target.clone(), center, normal, up, size, depth, offset } });
        self.transform_mesh(&name, to_target);
        self.b().scene.nodes.insert(name.clone(), SceneNode { parent: Some(target), ..SceneNode::default() });
        self.b().decals.insert(name.clone());
        self.emit_node(&name);
        self.handle_of(&name, false)
    }
    /// Shaped vector text in a linear MSDF texture, decoded by the material shader.
    fn m_text_material(&mut self) -> ScriptValue {
        let (name, text) = (self.new_name(), self.text(0));
        let o = self.opts(1, &["color", "background", "emissive", "emissive_strength", "roughness", "metallic", "pixels", "badge", "border", "border_width", "clearcoat", "plate", "font", "weight", "width", "letter_spacing", "resolution", "padding"]);
        if !opt(self.vm, o, id!(pixels)).is_nil() {
            return self.fail("pixels was removed; use resolution (texels per em)");
        }
        let ink = { let v = opt(self.vm, o, id!(color)); if v.is_nil() { [0.9, 0.9, 0.9, 1.] } else { color_linear(self.vm, v) } };
        let fill = { let v = opt(self.vm, o, id!(background)); if v.is_nil() { [0.; 4] } else { color_linear(self.vm, v) } };
        let border_value = opt(self.vm, o, id!(border));
        let border = if border_value.is_nil() { [0.; 4] } else { color_linear(self.vm, border_value) };
        let border_width = opt_num(self.vm, o, id!(border_width)).unwrap_or(if border_value.is_nil() { 0. } else { 0.08 });
        let resolution = opt_num(self.vm, o, id!(resolution)).unwrap_or(64.);
        if !resolution.is_finite() || resolution < 16.0 || resolution > u32::MAX as f64 || resolution.fract() != 0.0 {
            return self.fail("resolution must be a u32 integer >= 16 (texels per em)");
        }
        let spec = makepad_model::MsdfTextSpec {
            text,
            font: opt_text(self.vm, o, id!(font)).unwrap_or_else(|| "plex-semibold".into()),
            weight: opt_num(self.vm, o, id!(weight)).map(|v| v as f32),
            width: opt_num(self.vm, o, id!(width)).map(|v| v as f32),
            letter_spacing: opt_num(self.vm, o, id!(letter_spacing)).unwrap_or(0.) as f32,
            resolution: resolution as u32,
            badge: opt_text(self.vm, o, id!(badge)),
            padding: opt_num(self.vm, o, id!(padding)).unwrap_or(1. / 7.) as f32,
            border_width: border_width as f32,
            plate: opt(self.vm, o, id!(plate)).as_bool().unwrap_or(false),
        };
        // The text renders when the model builds, on its worker (`ProgramOp::TextLayer`): the
        // material needs only the bake's size and distance range here.
        let baked = match makepad_model::msdf_text_metrics(&spec) { Ok(metrics) => metrics, Err(e) => return self.fail(e) };
        let index = match self.b().materials.iter().position(|m| *m == name) { Some(i) => i, None => { self.b().materials.push(name.clone()); self.b().materials.len() - 1 } } as u32 + 1;
        let mut surface = SurfaceMaterial {
            base_color: ink,
            metallic: opt_num(self.vm, o, id!(metallic)).unwrap_or(0.),
            roughness: opt_num(self.vm, o, id!(roughness)).unwrap_or(0.6),
            alpha: SurfaceAlpha::Mask,
            alpha_cutoff: 0.5,
            shading: Some(GlbShading {
                clearcoat: opt_num(self.vm, o, id!(clearcoat)).unwrap_or(0.).clamp(0., 1.),
                plate: if spec.plate { 1. } else { 0. },
                msdf: 1., msdf_fill: fill, msdf_border: border,
                msdf_border_width: border_width * resolution,
                msdf_range: baked.range as f64,
                msdf_plate_cells: baked.plate_cells.map(|c| c.map(f64::from)),
                ..Default::default()
            }),
            ..Default::default()
        };
        let em = opt(self.vm, o, id!(emissive));
        if !em.is_nil() {
            let c = color_linear(self.vm, em);
            surface.emissive = [c[0], c[1], c[2]];
            surface.emissive_strength = opt_num(self.vm, o, id!(emissive_strength)).unwrap_or(1.);
        }
        self.op(Operation::SetMaterial { material: index, value: Material::default() });
        self.op(Operation::Surface(SurfaceOperation::Material { material: index, value: surface }));
        let aspect = baked.width as f64 / (baked.height as f64 * if spec.plate { 0.5 } else { 1. });
        self.push(ProgramOp::TextLayer { material: index, channel: SurfaceChannel::BaseColor, layer: "text".into(), text: spec });
        self.b().aspects.insert(name.clone(), aspect);
        self.handle_of(&name, true)
    }
    /// `m.sign_material(name, {width, height, background, layers, ...})`:
    /// a material painted by the vector canvas (`crate::vector`) —
    /// glyph-outline text, rounded rects and frames, circles, rings,
    /// stripes, gradients and logo marks, each layer optionally glowing
    /// after dark (its `glow` becomes the emissive map). Returns the aspect.
    fn m_sign_material(&mut self) -> ScriptValue {
        use crate::vector::{Align, Canvas, Mark};
        let name = self.new_name();
        let o = self.opts(0, &["width", "height", "background", "layers", "roughness", "metallic", "emissive_strength", "clearcoat"]);
        let w = opt_num(self.vm, o, id!(width)).unwrap_or(512.).clamp(16., 2048.) as usize;
        let h = opt_num(self.vm, o, id!(height)).unwrap_or(256.).clamp(16., 2048.) as usize;
        let enc = |x: f64| { let x = x.clamp(0., 1.); (if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1. / 2.4) - 0.055 }) as f32 };
        let srgb = |c: [f64; 4]| -> [f32; 4] { [enc(c[0]), enc(c[1]), enc(c[2]), c[3].clamp(0., 1.) as f32] };
        let bg = { let v = opt(self.vm, o, id!(background)); if v.is_nil() { [0., 0., 0., 0.] } else { srgb(color_linear(self.vm, v)) } };
        let base_timer = makepad_model::TextureGenerationTimer::start();
        let mut canvas = Canvas::new(w, h, bg);
        let layers = opt(self.vm, o, id!(layers));
        let n = if layers.is_nil() { 0 } else { list_len(self.vm, layers) };
        for i in 0..n {
            let lv = list_value(self.vm, layers, i);
            let Some(l) = lv.as_object() else { continue };
            let l = Some(l);
            let num = |vm: &mut ScriptVm, k: LiveId, d: f64| opt_num(vm, l, k).unwrap_or(d) as f32;
            let color = { let v = opt(self.vm, l, id!(color)); if v.is_nil() { [1., 1., 1., 1.] } else { srgb(color_linear(self.vm, v)) } };
            let glow = num(self.vm, id!(glow), 0.).clamp(0., 1.);
            let (x0, y0, x1, y1) = (num(self.vm, id!(x0), 0.), num(self.vm, id!(y0), 0.), num(self.vm, id!(x1), w as f64), num(self.vm, id!(y1), h as f64));
            let (x, y, r) = (num(self.vm, id!(x), w as f64 * 0.5), num(self.vm, id!(y), h as f64 * 0.5), num(self.vm, id!(r), h as f64 * 0.25));
            let kind = opt_text(self.vm, l, id!(kind)).unwrap_or_default();
            match kind.as_str() {
                "text" => {
                    let text = opt_text(self.vm, l, id!(text)).unwrap_or_default();
                    let size = num(self.vm, id!(size), h as f64 * 0.3);
                    let spacing = num(self.vm, id!(spacing), 0.);
                    let align = match opt_text(self.vm, l, id!(align)).as_deref() { Some("left") => Align::Left, Some("right") => Align::Right, _ => Align::Center };
                    let outline = { let v = opt(self.vm, l, id!(outline)); if v.is_nil() { None } else { let c = srgb(color_linear(self.vm, v)); Some((c, num(self.vm, id!(outline_width), 2.))) } };
                    let font = opt_text(self.vm, l, id!(font)).unwrap_or_else(|| "plex-semibold".into());
                    let face = match makepad_model::msdf_text::text_face(&font,
                        opt_num(self.vm, l, id!(weight)).map(|v| v as f32),
                        opt_num(self.vm, l, id!(width)).map(|v| v as f32)) {
                        Ok(face) => face, Err(e) => return self.fail(e),
                    };
                    // `fit`: measure the selected, shaped variable face too.
                    let fit = num(self.vm, id!(fit), 0.);
                    let mut px = size;
                    if fit > 0. { let tw = Canvas::measure_with_face(&face, &text, px, spacing); if tw > fit { px *= fit / tw; } }
                    canvas.text_with_face(&face, &text, x, y, px, align, spacing, color, glow, outline);
                }
                "rect" => canvas.rounded_rect(x0, y0, x1, y1, num(self.vm, id!(radius), 0.), color, glow),
                "frame" => canvas.rounded_frame(x0, y0, x1, y1, num(self.vm, id!(radius), 0.), num(self.vm, id!(width), 3.), color, glow),
                "circle" => canvas.circle(x, y, r, color, glow),
                "ring" => canvas.ring(x, y, r, num(self.vm, id!(width), 3.), color, glow),
                "stripes" => canvas.stripes(x0, y0, x1, y1, num(self.vm, id!(period), 16.), num(self.vm, id!(duty), 0.5), num(self.vm, id!(slope), 0.), color, glow),
                "gradient" => {
                    let to = { let v = opt(self.vm, l, id!(to)); if v.is_nil() { color } else { srgb(color_linear(self.vm, v)) } };
                    let vertical = opt(self.vm, l, id!(vertical)).as_bool().unwrap_or(true);
                    canvas.gradient(x0, y0, x1, y1, color, to, vertical);
                }
                "mark" => {
                    let mark = match opt_text(self.vm, l, id!(mark)).as_deref() { Some("star") => Mark::Star, Some("drop") => Mark::Drop, Some("wave") => Mark::Wave, Some("bolt") => Mark::Bolt, Some("sun") => Mark::Sun, _ => Mark::Leaf };
                    canvas.mark(mark, x, y, r, color, glow);
                }
                other => { return self.fail(format!("sign_material layer kind '{other}' (text, rect, frame, circle, ring, stripes, gradient, mark)")); }
            }
        }
        let index = match self.b().materials.iter().position(|m| *m == name) { Some(i) => i, None => { self.b().materials.push(name.clone()); self.b().materials.len() - 1 } } as u32 + 1;
        let rgba = |texels: &[u32]| -> Vec<u8> {
            texels.iter().flat_map(|t| [(t >> 16) as u8, (t >> 8) as u8, *t as u8, (t >> 24) as u8]).collect()
        };
        let base = canvas.to_argb(|_, a| a);
        let mut base_timing = base_timer.finish(index, SurfaceChannel::BaseColor, "sign", w as u32, h as u32);
        let glow_timer = makepad_model::TextureGenerationTimer::start();
        let (px, glow) = canvas.pixels();
        let lit = glow.iter().any(|g| *g > 0.02);
        // Emissive: the colour where it glows, black elsewhere.
        let emissive: Vec<u32> = if lit {
            let enc8 = |x: f32| (enc(x as f64) * 255. + 0.5) as u32;
            px.iter().zip(glow).map(|(c, g)| (255u32 << 24) | (enc8(c[0] * g) << 16) | (enc8(c[1] * g) << 8) | enc8(c[2] * g)).collect()
        } else { Vec::new() };
        let mut glow_timing = glow_timer.finish(index, SurfaceChannel::Emissive, "sign", w as u32, h as u32);
        let masked = bg[3] < 0.5;
        let mut surface = SurfaceMaterial {
            metallic: opt_num(self.vm, o, id!(metallic)).unwrap_or(0.),
            roughness: opt_num(self.vm, o, id!(roughness)).unwrap_or(0.55),
            alpha: if masked { SurfaceAlpha::Mask } else { SurfaceAlpha::Opaque },
            ..Default::default()
        };
        if lit {
            surface.emissive = [1.; 3];
            surface.emissive_strength = opt_num(self.vm, o, id!(emissive_strength)).unwrap_or(1.5);
        }
        if let Some(c) = opt_num(self.vm, o, id!(clearcoat)).filter(|c| *c > 0.) { surface.shading = Some(GlbShading { clearcoat: c.min(1.), ..Default::default() }); }
        self.op(Operation::SetMaterial { material: index, value: Material::default() });
        self.op(Operation::Surface(SurfaceOperation::Material { material: index, value: surface }));
        let pixels_timer = makepad_model::TextureGenerationTimer::start();
        self.op(Operation::Surface(SurfaceOperation::Layer {
            material: index, channel: SurfaceChannel::BaseColor,
            layer: SurfaceLayer::new("sign", RgbaImage { width: w as u32, height: h as u32, pixels: rgba(&base) }),
        }));
        base_timing.elapsed += pixels_timer.finish(index, SurfaceChannel::BaseColor, "sign", w as u32, h as u32).elapsed;
        self.b().texture_timings.push(base_timing);
        if lit {
            let pixels_timer = makepad_model::TextureGenerationTimer::start();
            self.op(Operation::Surface(SurfaceOperation::Layer {
                material: index, channel: SurfaceChannel::Emissive,
                layer: SurfaceLayer::new("sign_glow", RgbaImage { width: w as u32, height: h as u32, pixels: rgba(&emissive) }),
            }));
            glow_timing.elapsed += pixels_timer.finish(index, SurfaceChannel::Emissive, "sign", w as u32, h as u32).elapsed;
            self.b().texture_timings.push(glow_timing);
        }
        self.b().aspects.insert(name.clone(), w as f64 / h as f64);
        self.handle_of(&name, true)
    }
    fn m_uv(&mut self) -> ScriptValue {
        let Some(name) = self.part(0) else { return NIL };
        let o = self.opts(1, &["kind", "scale", "axis"]);
        if !self.known(&name) { return NIL; }
        let kind = opt_text(self.vm, o, id!(kind)).unwrap_or_else(|| "box".into());
        let scale = opt_num(self.vm, o, id!(scale)).unwrap_or(1.);
        let action = match kind.as_str() {
            "box" => MeshAction::BoxUv { faces: Vec::new(), scale: [scale; 2], offset: [0.; 2] },
            "cylinder" => MeshAction::CylinderUv {
                faces: Vec::new(), axis: match opt_text(self.vm, o, id!(axis)).as_deref() { Some("x") => 0, Some("z") => 2, _ => 1 },
                scale: [scale; 2], offset: [0.; 2],
            },
            "unwrap" => MeshAction::Unwrap { faces: Vec::new() },
            _ => return self.fail(format!("uv kind '{kind}' (box, cylinder, unwrap)")),
        };
        self.select_all(&name, "faces", Operation::MeshEditing(MeshEditingOperation { object: name.clone(), action }));
        if kind == "unwrap" { self.select_all(&name, "faces", Operation::MeshEditing(MeshEditingOperation { object: name.clone(), action: MeshAction::Pack { faces: Vec::new(), padding: 0.01 } })); }
        NIL
    }
    fn attachment(&mut self, o: Option<ScriptObject>) -> Attachment {
        if let Some(obj) = self.opt_part(o, id!(object)) { return Attachment::Object(obj); }
        if let Some(j) = opt_text(self.vm, o, id!(joint)) {
            let index = makepad_model::templates::humanoid_joints(1.).iter().position(|x| x.0 == j);
            return match index { Some(i) if self.b().rigged => Attachment::Joint(i as u32), _ => { self.error = Some(format!("joint '{j}' needs m.rig_humanoid() and a humanoid joint name")); Attachment::Root } };
        }
        Attachment::Root
    }
    fn m_socket(&mut self) -> ScriptValue {
        let name = self.text(0);
        let o = self.opts(1, &["pos", "rot", "object", "joint"]);
        let attachment = self.attachment(o);
        let pos = opt_v3(self.vm, o, id!(pos)).unwrap_or([0.; 3]);
        let rot = opt_v3(self.vm, o, id!(rot)).map_or([0., 0., 0., 1.], quat_euler_deg);
        self.op(Operation::Scene(SceneOperation::Socket(Socket { name, attachment, transform: Transform { translation: pos, rotation: rot, scale: [1.; 3] } })));
        NIL
    }
    /// `m.pivot(object, name, {pivot})` — make `object` (and its children)
    /// a DRIVEN part: it leaves the static mesh and the engine poses it
    /// every frame about `pivot` (object-local metres; the object's own
    /// origin by default). `game.model_drives` binds `name` to a channel —
    /// control surfaces, gear legs, canopies, propellers, needles. Uses the
    /// generic driven-node contract the wheels ride.
    fn m_pivot(&mut self) -> ScriptValue {
        let Some(name) = self.part(0) else { return NIL }; let connection = self.text(1);
        let o = self.opts(2, &["pivot"]);
        if connection.is_empty() || connection.starts_with("wheel_") {
            return self.fail(format!("pivot name '{connection}' (any name that is not wheel_*)"));
        }
        if !self.known(&name) { return NIL; }
        let pivot = opt_v3(self.vm, o, id!(pivot)).unwrap_or([0.; 3]);
        // The contract wants positive wheel dimensions; a driven part's are
        // nominal (nothing reads them for a non-wheel connection).
        self.op(Operation::Scene(SceneOperation::VehicleWheel {
            object: name, wheel: VehicleWheel { connection, pivot, radius: 0.01, width: 0.01, visual: None },
        }));
        NIL
    }
    fn m_wheel(&mut self) -> ScriptValue {
        let Some(name) = self.part(0) else { return NIL }; let corner = self.text(1);
        let o = self.opts(2, &["radius", "width", "steer", "travel"]);
        let connection = match corner.as_str() { "front_left" | "fl" => "wheel_front_left", "front_right" | "fr" => "wheel_front_right", "rear_left" | "rl" => "wheel_rear_left", "rear_right" | "rr" => "wheel_rear_right",
            // A two-wheeler's wheels sit on the centreline: "front"/"rear"
            // take the left slots, which a motorbike reads as its pair.
            "front" => "wheel_front_left", "rear" => "wheel_rear_left",
            _ => return self.fail(format!("wheel corner '{corner}' (front_left, front_right, rear_left, rear_right; a motorbike: front, rear)")) };
        if !self.known(&name) { return NIL; }
        // The hub is the middle of the tyre's own geometry (its axle is X, so
        // the Y/Z middle of anything turned about it is the axle), not the
        // node origin: a tyre built off its origin (a union, a mesh in model
        // coordinates) still spins in place. Size defaults come from it too.
        let local = self.b().bounds.get(&name).copied();
        let pivot = local.map_or([0.; 3], |b| std::array::from_fn(|d| { let c = (b[0][d] + b[1][d]) * 0.5; if c.abs() < 1e-9 { 0. } else { c } }));
        let radius = local.map_or(0.34, |b| ((b[1][1] - b[0][1]).max(b[1][2] - b[0][2]) * 0.5).max(0.01));
        let width = local.map_or(0.24, |b| (b[1][0] - b[0][0]).max(0.01));
        let radius = opt_num(self.vm, o, id!(radius)).unwrap_or(radius);
        let width = opt_num(self.vm, o, id!(width)).unwrap_or(width);
        let (steer, travel) = (opt_num(self.vm, o, id!(steer)), opt_num(self.vm, o, id!(travel)));
        let visual = if steer.is_some() || travel.is_some() {
            let front = connection.contains("front");
            let t = travel.unwrap_or(0.12);
            Some(VisualWheelMotion {
                steer_gain: if front { 1. } else { 0. },
                steer_max: steer.unwrap_or(if front { 0.6 } else { 0. }).to_radians().min(1.2),
                compression: t, droop: t * 0.8,
            })
        } else { None };
        self.op(Operation::Scene(SceneOperation::VehicleWheel { object: name, wheel: VehicleWheel { connection: connection.into(), pivot, radius, width, visual } }));
        NIL
    }
    fn m_lod(&mut self) -> ScriptValue {
        let Some(name) = self.part(0) else { return NIL };
        let list = self.arg(1);
        let n = list_len(self.vm, list);
        let mut levels = Vec::new();
        for i in 0..n {
            let item = list_value(self.vm, list, i).as_object();
            let faces = opt_num(self.vm, item, id!(faces)).unwrap_or(500.);
            if !faces.is_finite() || faces < 0. || faces > u32::MAX as f64 { self.error = Some("expected u32 integer".into()); return NIL; }
            let Some([faces]) = self.counts([faces]) else { return NIL };
            if faces == 0 && i + 1 != n { self.error = Some("empty LOD must be last".into()); return NIL; }
            levels.push(LodLevel { target_faces: faces as usize, max_error: opt_num(self.vm, item, id!(error)).unwrap_or(0.05), distance: opt_num(self.vm, item, id!(distance)).unwrap_or(30.) });
        }
        if self.known(&name) { self.op(Operation::Scene(SceneOperation::Lods { object: name, levels })); }
        NIL
    }
    fn m_light(&mut self) -> ScriptValue {
        let name = self.text(0);
        let o = self.opts(1, &["kind", "pos", "rot", "color", "intensity", "range", "inner", "outer", "object", "joint"]);
        let attachment = self.attachment(o);
        let kind = opt_text(self.vm, o, id!(kind)).unwrap_or_else(|| "point".into());
        let color = { let v = opt(self.vm, o, id!(color)); if v.is_nil() { [1.; 4] } else { color_linear(self.vm, v) } };
        let pos = opt_v3(self.vm, o, id!(pos)).unwrap_or([0.; 3]);
        let rot = opt_v3(self.vm, o, id!(rot)).map_or([0., 0., 0., 1.], quat_euler_deg);
        let kind = match kind.as_str() {
            "point" => LightKind::Point,
            "spot" => LightKind::Spot { inner: opt_num(self.vm, o, id!(inner)).unwrap_or(0.25), outer: opt_num(self.vm, o, id!(outer)).unwrap_or(0.5) },
            _ => return self.fail("light kind must be point or spot"),
        };
        let intensity = opt_num(self.vm, o, id!(intensity)).unwrap_or(50.);
        let range = opt_num(self.vm, o, id!(range)).unwrap_or(8.);
        self.op(Operation::Scene(SceneOperation::Light(LightEmitter {
            name, attachment, transform: Transform { translation: pos, rotation: rot, scale: [1.; 3] },
            kind, color: [color[0], color[1], color[2]], intensity, range,
        })));
        NIL
    }
    fn m_rig(&mut self) -> ScriptValue {
        let o = self.opts(0, &["height", "clips"]);
        let height = opt_num(self.vm, o, id!(height)).unwrap_or(1.8);
        let clips = opt(self.vm, o, id!(clips));
        let clips = if clips.as_array().is_some() || clips.as_object().is_some_and(|o| self.vm.bx.heap_ref().vec_len(o) > 0) {
            let n = list_len(self.vm, clips);
            let mut names = Vec::new();
            for i in 0..n {
                let v = list_value(self.vm, clips, i);
                if v.is_string_like() { names.push(value_string(self.vm, v)); }
            }
            Some(names)
        } else { None };
        self.push(ProgramOp::HumanoidRig { height, clips, sockets: true });
        self.b().rigged = true;
        // Joint rest positions, so body parts can be placed on the skeleton.
        let joints = makepad_model::templates::humanoid_joints(height);
        let out = self.vm.bx.heap_mut().new_object();
        self.vm.bx.heap_mut().set_object_storage_auto(out);
        for (name, _, p) in joints {
            let value = vec3_value(self.vm, vec3f(p[0] as f32, p[1] as f32, p[2] as f32));
            self.vm.bx.heap_mut().set_value(out, LiveId::from_str(name).into(), value, NoTrap);
        }
        out.into()
    }
    /// `m.character(spec)`: a whole generated character (see
    /// `game.character_new`) inside this model; returns joint rest positions.
    fn m_character(&mut self) -> ScriptValue {
        let v = self.arg(0);
        let mut spec = match crate::character_spec::read(self.vm, v) { Ok(spec) => spec, Err(e) => return self.fail(e) };
        // Generated materials sit above the ones m.material declares.
        spec.material_base = 128;
        for w in &spec.warnings { self.host.log(format!("model.character: {w}")); }
        let joints = makepad_model::character::rest_joints(&spec);
        self.push(ProgramOp::Character(Box::new(spec)));
        self.b().rigged = true;
        self.b().rest_hips = joints.first().map(|j| j.1);
        let out = self.vm.bx.heap_mut().new_object();
        self.vm.bx.heap_mut().set_object_storage_auto(out);
        for (name, p) in joints {
            let value = vec3_value(self.vm, vec3f(p[0] as f32, p[1] as f32, p[2] as f32));
            self.vm.bx.heap_mut().set_value(out, LiveId::from_str(&name).into(), value, NoTrap);
        }
        out.into()
    }
    fn m_import(&mut self) -> ScriptValue {
        let alias = self.text(0);
        let o = self.opts(1, &["prefix", "pos", "rot", "scale"]);
        let prefix = opt_text(self.vm, o, id!(prefix)).unwrap_or_else(|| "import".into());
        let imports = self.b().ops.iter().filter(|v| matches!(v, ProgramOp::ImportGlb { .. })).count();
        let translation = opt_v3(self.vm, o, id!(pos)).unwrap_or([0.; 3]);
        let rotation = opt_v3(self.vm, o, id!(rot)).map_or([0., 0., 0., 1.], quat_euler_deg);
        let s = opt(self.vm, o, id!(scale));
        let scale = if s.is_nil() { [1.; 3] } else if let Some(x) = s.as_number() { [x; 3] } else { let p = value_vec3(self.vm, s); [p.x as f64, p.y as f64, p.z as f64] };
        self.push(ProgramOp::ImportGlb { alias, prefix, material_base: 256 + 32 * imports as u32, transform: Some(Transform { translation, rotation, scale }) });
        NIL
    }
    fn m_build(&mut self) -> ScriptValue {
        let built = {
            let b = self.b();
            let keyed = match b.op_error.clone() {
                Some(e) => Err(e),
                // A rig binds what is still unweighted last: the program the build posts ends
                // with a BindAll the builder's own operations leave out.
                None if b.rigged && !b.ops.iter().any(|v| matches!(v, ProgramOp::BindAll)) => op_key(&ProgramOp::BindAll).map(|bind| {
                    let mut ops = (*b.ops).clone();
                    ops.push(ProgramOp::BindAll);
                    (Arc::new(ops), key_of(&b.op_keys, Some(bind), Some(&b.class)))
                }),
                None => Ok((b.ops.clone(), key_of(&b.op_keys, None, Some(&b.class)))),
            };
            keyed.map(|(program, (hash, bytes))| {
                let alias = format!("{SCRIPT_MODEL_PREFIX}{}", b.name);
                let request = ScriptModelRequest { alias: alias.clone(), hash: hash.clone(), title: b.name.replace('-', " "), class: b.class.clone(), program, materials: Arc::new(b.materials.clone()), texture_timings: Arc::new(b.texture_timings.clone()) };
                let bounds = b.bounds.keys().fold(None, |acc, name| merge(acc, b.translated_bounds(name)));
                (alias, hash, bytes, (request, bounds))
            })
        };
        let (alias, hash, bytes, request) = match built {
            Ok(built) => built,
            Err(e) => return self.fail(format!("m.build: {e}")),
        };
        let (request, bounds) = request;
        // A headless evaluation (an editable model document) takes the program itself.
        if CAPTURE.with(|c| c.borrow_mut().as_mut().map(|v| v.push(request.clone())).is_some()) {
            return self.vm.bx.heap_mut().new_string_from_str(&alias);
        }
        let rig = rig_rest(&request.program).map(Arc::new);
        if let Admit::Withhold(reason) = self.host.admit_program(&alias, bytes) {
            withhold(request, bounds, rig, reason);
            return self.vm.bx.heap_mut().new_string_from_str(&alias);
        }
        observe_request(&alias, &hash);
        let chain = self.b().lod_models.clone();
        let habitat = self.b().habitat.clone();
        registry(|r| {
            r.note_front(&alias, &request.class);
            match &habitat { Some(h) => { r.habitats.insert(alias.clone(), h.clone()); } None => { r.habitats.remove(&alias); } }
            let old = r.lod_chains.get(&alias).cloned().unwrap_or_default();
            if old != chain {
                if chain.is_empty() { r.lod_chains.remove(&alias); } else { r.lod_chains.insert(alias.clone(), chain); }
                r.lod_rev += 1;
            }
        });
        let (failure, _retired, _old_rig) = registry(|r| {
            let old_rig = r.note_rig(&alias, rig);
            let resumed = r.admit_withheld(&alias);
            let failure = r.failed.get(&alias).filter(|(h, _)| *h == hash).map(|(_, e)| e.clone());
            let changed = r.wanted(&alias) != Some(&hash);
            let fresh = (changed || resumed) && failure.is_none();
            if changed || resumed { r.program_bytes.insert(alias.clone(), bytes); }
            let retired = if changed || resumed { r.requested.insert(alias.clone(), request.clone()) } else { None };
            r.world.insert(alias.clone());
            if let Some(b) = bounds { r.bounds.insert(alias.clone(), b); }
            if fresh && r.installed.get(&alias) != Some(&hash) {
                r.queue.retain(|q| q.alias != alias);
                r.queue.push(request);
            }
            (failure, retired, old_rig)
        });
        if let Some(error) = failure { self.host.log(format!("game.model_new('{}'): build failed: {error}", alias)); }
        self.vm.bx.heap_mut().new_string_from_str(&alias)
    }
}

/// Build priority of `alias`: 0 player-facing (first-person arms, characters),
/// 1 placed (then by distance to `focus`, else by placement order), 2 not
/// placed yet (models a script only spawns later).
pub fn script_model_priority(alias: &str, focus: Option<[f32; 3]>) -> (u8, u64) {
    let name = alias.trim_start_matches(SCRIPT_MODEL_PREFIX);
    if name.starts_with("arms-") || name.starts_with("char-") { return (0, 0); }
    registry(|r| match r.placements.get(alias) {
        Some((p, order)) => (1, match focus {
            Some(f) => (((p[0] - f[0]).powi(2) + (p[1] - f[1]).powi(2) + (p[2] - f[2]).powi(2)).sqrt() * 1000.0) as u64,
            None => *order as u64,
        }),
        None => (2, 0),
    })
}

/// Where a still-building model is first placed (the build queue's
/// priority, see `script_model_priority`).
pub fn note_script_model_placement(alias: &str, pos: [f32; 3]) {
    registry(|r| { let n = r.placements.len(); r.placements.entry(alias.to_string()).or_insert((pos, n)); });
}
/// The estimated model-space bounds of a script model (a program's or a recipe's).
pub fn script_model_bounds(alias: &str) -> Option<[[f64; 3]; 2]> { registry(|r| r.bounds.get(alias).copied()) }
/// The script-side rest skeleton of the latest program posted for this alias.
pub fn rig_of(alias: &str) -> Option<Arc<RigRest>> { registry(|r| r.rigs.get(alias).cloned()) }

/// Names with pending builds, for diagnostics.
/// Script models whose current program failed to build: (alias, reason).
pub fn failed_script_models() -> Vec<(String, String)> {
    registry(|r| r.failed.iter().filter(|(a, (h, _))| r.world.contains(*a) && r.wanted(a) == Some(h)).map(|(a, (_, e))| (a.clone(), e.clone())).collect())
}

pub fn pending_script_models() -> HashSet<String> {
    registry(|r| r.requested.iter().filter(|(a, request)| !r.settled(a, &request.hash)).map(|(a, _)| a.clone()).collect())
}
/// Script models the running world asked for that this device withheld: (alias, reason).
pub fn withheld_script_models() -> Vec<(String, String)> {
    registry(|r| r.withheld.iter().filter(|(a, (h, _))| r.world.contains(*a) && r.wanted(a) == Some(h)).map(|(a, (_, reason))| (a.clone(), reason.clone())).collect())
}
/// Moves whenever an alias is withheld or admitted after being withheld.
pub fn script_model_withheld_rev() -> u64 { registry(|r| r.withheld_rev) }
/// Canonical bytes of the programs the running world's requests hold.
pub fn script_program_bytes() -> u64 {
    registry(|r| r.world.iter().filter_map(|a| r.program_bytes.get(a)).map(|b| *b as u64).sum())
}

#[path = "model_clip.rs"]
mod model_clip;
#[path = "model_edit.rs"]
mod model_edit;

thread_local! {
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Quiet;
    impl ModelHost for Quiet { fn log(&self, message: String) { panic!("builder said: {message}") } }

    #[test]
    fn material_crown_reaches_glb_only_when_enabled() {
        let host_box = Box::leak(Box::new(ScriptVmHost::new((), ())));
        let mut vm = ScriptVm { host: host_box.host_context(), bx: Box::new(ScriptVmBase::new()) };
        let host: Rc<dyn ModelHost> = Rc::new(Quiet);
        let module = vm.new_module(id!(game));
        vm.add_method(module, id!(model_new), &[], move |vm, args| v_model_new(vm, &host, args));
        vm.capture_errors();
        let source = r#"let m = mod.game.model_new("crown-test")
let crown = m.material({crown: true})
let plain = m.material({})
let wind = m.material({wind: 0.4})
let card = m.material({wind: 0.4, crown: false})
let leaves = m.box({size: 1, material: crown})
let trunk = m.box({size: 1, material: plain})
let twigs = m.box({size: 1, material: wind})
let grass = m.box({size: 1, material: card})
m
"#;
        let out = vm.eval(ScriptMod::new("", "", "material_crown_test.splash", 1, 0, source, &[]));
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "{errors:?}");
        let handle = out.as_handle().unwrap();
        let builder = &vm.bx.heap_ref().handle_ref::<BuilderHandle>(handle).unwrap().0;
        let built = makepad_model::build_program(&builder.ops, makepad_model::Limits::default(), &mut |_| Err("no external inputs".into()), None).unwrap();
        let materials = &built.document.surface().materials;
        assert_eq!(materials[&1].shading.unwrap().crown, 1.0, "crown alone creates the shading block");
        assert!(materials[&2].shading.is_none(), "plain materials keep their original layout");
        assert_eq!(materials[&3].shading, materials[&4].shading, "explicit false is the default");
        let glb = makepad_gltf::parse_glb_bytes(&built.compiled.glb).unwrap();
        let surfaces: Vec<_> = glb.document.materials_slice().iter().filter(|m| m.extras.as_ref().is_some_and(|e| e.key("makepadSurface").is_some())).collect();
        assert_eq!(surfaces.len(), 4, "all authored materials reach the GLB");
        let crowns: Vec<_> = surfaces.iter().map(|m| m.extras.as_ref().and_then(|e| e.key("makepadShading")).and_then(|s| s.key("crown")).map(|v| v.as_f64().unwrap())).collect();
        assert_eq!(crowns.iter().filter(|c| **c == Some(1.0)).count(), 1, "the crown flag exports as 1");
        assert_eq!(crowns.iter().filter(|c| c.is_none()).count(), 3, "materials without crown omit the key");
    }

    fn record(parts: &str) -> (Vec<ProgramOp>, Vec<String>) {
        #[derive(Default)]
        struct Messages(std::cell::RefCell<Vec<String>>);
        impl ModelHost for Messages { fn log(&self, message: String) { self.0.borrow_mut().push(message); } }
        let host_box = Box::leak(Box::new(ScriptVmHost::new((), ())));
        let mut vm = ScriptVm { host: host_box.host_context(), bx: Box::new(ScriptVmBase::new()) };
        let messages = Rc::new(Messages::default());
        let host: Rc<dyn ModelHost> = messages.clone();
        let module = vm.new_module(id!(game));
        vm.add_method(module, id!(model_new), &[], move |vm, args| v_model_new(vm, &host, args));
        vm.capture_errors();
        let source = format!(r#"use mod.pod.*
let m = mod.game.model_new("mesh-test")
{parts}
m
"#);
        let out = vm.eval(ScriptMod::new("", "", "mesh_test.splash", 1, 0, &source, &[]));
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "{errors:?}");
        let handle = out.as_handle().unwrap();
        let ops = vm.bx.heap_ref().handle_ref::<BuilderHandle>(handle).unwrap().0.ops.to_vec();
        let messages = messages.0.borrow().clone();
        (ops, messages)
    }

    #[test]
    fn decal_follows_its_target_and_survives_reparenting() {
        let (ops, messages) = record(r#"
let base = m.box({pos: vec3(2, 0, 0)})
let hull = m.box({parent: base})
m.move(hull, {pos: vec3(1, 1, 0), rot: vec3(0, 0, 30), scale: vec3(2, 1, 0.5)})
let stripe = m.decal(hull, {pos: vec3(3, 1, 0.25), size: vec2(0.4, 0.3)})
m.move(base, {pos: vec3(4, 0, 0)})
let cutter = m.box({size: vec3(0.2, 0.2, 0.2), pos: vec3(5, 1, 0.25)})
let panel = m.subtract(hull, cutter)
m.move(panel, {pos: vec3(0, 1, 0)})
m.delete(panel)
let badge = m.decal(base, {pos: vec3(4, 0, 0.5)})
m.delete(base)
"#);
        assert_eq!(messages.len(), 1, "two targets deleted, one root-move report: {messages:?}");
        assert!(messages[0].contains("model root"), "{messages:?}");
        let mut scene = makepad_model::Workspace::new(Default::default()).unwrap();
        let mut projected = Vec::new();
        let mut stages = Vec::new();
        for op in ops {
            let ProgramOp::Op(op) = op else { panic!("expected a document operation") };
            scene.execute(std::slice::from_ref(&op), None).unwrap();
            let (stage, parent, offset) = match &op {
                Operation::Construction(c) if c.object == "stripe" => {
                    projected = scene.object("stripe").unwrap().vertices().iter().map(|v| v.position).collect();
                    assert!(projected.len() >= 3, "projection produced a surface");
                    continue;
                }
                Operation::Scene(SceneOperation::Node { object, node }) if object == "stripe" && node.parent.as_deref() == Some("hull") =>
                    ("attached", Some("hull"), [0., 0., 0.]),
                Operation::Scene(SceneOperation::Node { object, .. }) if object == "base" && !projected.is_empty() =>
                    ("moved target", Some("hull"), [2., 0., 0.]),
                Operation::DeleteObject { object } if object == "hull" =>
                    ("boolean", Some("panel"), [2., 0., 0.]),
                Operation::DeleteObject { object } if object == "panel" =>
                    ("deleted", None, [2., 1., 0.]),
                _ => continue,
            };
            assert_eq!(scene.scene().nodes["stripe"].parent.as_deref(), parent, "{stage}");
            let world = scene.scene().world_matrix("stripe").unwrap();
            let vertices = scene.object("stripe").unwrap().vertices();
            assert_eq!(vertices.len(), projected.len(), "{stage} vertex count");
            for (vertex, source) in vertices.iter().zip(&projected) {
                let point = transform_point(world, vertex.position);
                for d in 0..3 {
                    assert!((point[d] - (source[d] + offset[d])).abs() < 1e-10, "{stage} vertex {:?}: {point:?}", vertex.id);
                }
            }
            stages.push(stage);
        }
        assert_eq!(stages, ["attached", "moved target", "boolean", "deleted"]);
    }

    #[test]
    fn mesh_normals_and_ao_reach_glb_and_counts_name_the_mesh() {
        let record_mesh = |options: &str| record(&format!(r#"
let points = [vec3(0, 0, 0), vec3(1, 0, 0), vec3(0, 1, 0), vec3(0, 0, 1)]
let card = m.mesh(points, [[0, 1, 2], [0, 3, 1]], {{{options}}})
"#));
        let positions = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
        let expected_normals = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.], [-1., 0., 0.]];
        for (normals, ao) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut options = String::from("uvs: [vec2(0, 0), vec2(1, 0), vec2(0, 1), vec2(1, 1)]");
            if normals { options.push_str(", smooth: 180, normals: [vec3(2, 0, 0), vec3(0, 3, 0), vec3(0, 0, 4), vec3(-2, 0, 0)]"); }
            if ao { options.push_str(", ao: [-0.2, 0.25, 0.75, 1.2]"); }
            let (ops, messages) = record_mesh(&options);
            assert!(messages.is_empty(), "{messages:?}");
            let built = makepad_model::build_program(&ops, makepad_model::Limits::default(), &mut |_| Err("no external inputs".into()), None).unwrap();
            let loaded = makepad_gltf::load_gltf_from_bytes(&built.compiled.glb, None).unwrap();
            let decoded = makepad_gltf::decode_mesh_primitive(&loaded, 0, 0).unwrap();
            assert_eq!(decoded.colors0.is_some(), ao, "AO alone controls COLOR_0");
            let actual_normals = decoded.normals.as_ref().unwrap();
            for (corner, position) in decoded.positions.iter().enumerate() {
                let vertex = positions.iter().position(|p| p == position).unwrap();
                if normals { assert_eq!(actual_normals[corner], expected_normals[vertex], "normal overrides smoothing at vertex {vertex}"); }
                if let Some(colors) = &decoded.colors0 {
                    let ao = [0., 0.25, 0.75, 1.][vertex];
                    assert_eq!(colors[corner], [ao, ao, ao, 1.], "clamped AO darkens RGB without changing opacity at vertex {vertex}");
                }
                assert_eq!(decoded.texcoords0.as_ref().unwrap()[corner], [[0., 0.], [1., 0.], [0., 1.], [1., 1.]][vertex], "UV at vertex {vertex}");
            }
            if !normals {
                for (face, triangle) in decoded.indices.chunks_exact(3).enumerate() {
                    for &corner in triangle {
                        assert_eq!(actual_normals[corner as usize], [[0., 0., 1.], [0., 1., 0.]][face], "default face normal");
                    }
                }
            }
        }
        for (options, attribute) in [("normals: [vec3(1, 0, 0)]", "normals"), ("ao: [0.5]", "ao")] {
            let (ops, messages) = record_mesh(options);
            assert_eq!(messages.len(), 1, "{messages:?}");
            assert!(messages[0].contains("mesh 'card'") && messages[0].contains(attribute) && messages[0].contains("1 for 4 vertices"), "{messages:?}");
            assert!(!ops.iter().any(|op| matches!(op, ProgramOp::Mesh(_))), "invalid attributes must not record a mesh");
        }

        let (ops, messages) = record(r#"
let path = [vec3(0, 0, 0), vec3(0, 1, 0), vec3(0, 2, 0), vec3(0, 3, 0)]
let taper = m.tube(path, {radii: [0.12, 0.10, 0.075, 0.04]})
let horn = m.tube([vec3(0.2, 3.4, -0.3), vec3(0.5, 3.6, -0.2), vec3(0.6, 3.9, -0.3), vec3(0.55, 4.2, -0.5)], {radii: [0.10, 0.085, 0.055, 0], curve: true})
let yz = m.tube([vec3(0, 0, 0), vec3(0, 1, 0), vec3(0, 1, 4)], {curve: true, steps: 2})
let xy = m.tube([vec3(0, 0, 0), vec3(1, 0, 0), vec3(1, 0, 0), vec3(1, 1, 0)], {radii: [0.05, 0.05, 1, 0.05], segments: 8, caps: false, parent: taper})
let wrong_count = m.tube(path, {radii: [0.1, 0.2]})
let inside_zero = m.tube(path, {radii: [0.1, 0.2, 0, 0.1]})
let plain = m.tube(path, {radius: 0.05})
"#);
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages[0].contains("tube 'wrong_count'") && messages[0].contains("2 radii for 4 path points"), "{messages:?}");
        assert!(messages[1].contains("tube 'inside_zero'") && messages[1].contains("radii[2] is zero inside the path"), "{messages:?}");
        let built = makepad_model::build_program(&ops, makepad_model::Limits::default(), &mut |_| Err("no external inputs".into()), None).unwrap();
        let mut names: Vec<_> = built.document.objects().map(|(name, _)| name).collect();
        names.sort_unstable();
        assert_eq!(names, ["horn", "plain", "taper", "xy", "yz"], "one object per valid tube, under its binding");
        let construction = |name: &str| ops.iter().find_map(|op| match op {
            ProgramOp::Op(Operation::Construction(ConstructionOperation { object, kind })) if object == name => Some(kind),
            _ => None,
        }).unwrap();
        assert!(matches!(construction("plain"), Construction::Shape { shape: Shape::Tube { segments: 12, caps: true, .. }, .. }));
        let profiles = |name: &str| match construction(name) {
            Construction::Loft { profiles, closed, caps, .. } => {
                assert!(*closed, "{name} has closed rings");
                assert_eq!(*caps, name != "xy", "{name} end caps");
                profiles
            }
            _ => panic!("expected a loft for {name}"),
        };
        let point = |[x, y, z]: [f64; 3]| Vec3d::new(x, y, z);
        let center = |ring: &[[f64; 3]]| ring.iter().fold(Vec3d::ZERO, |sum, &p| sum + point(p)) / ring.len() as f64;
        let radius = |ring: &[[f64; 3]]| point(ring[0]).distance(center(ring));
        let taper = profiles("taper");
        assert_eq!(taper.len(), 4);
        for (ring, expected) in taper.iter().zip([0.12, 0.10, 0.075, 0.04]) {
            assert!((radius(ring) - expected).abs() < 1e-7, "radius at each path point");
        }
        let horn = profiles("horn");
        assert_eq!(horn.len(), 19, "six steps per curved span");
        assert!((radius(&horn[18]) - 0.055 * 0.02).abs() < 1e-7, "pointed end radius");
        assert!((radius(&horn[15]) - 0.055 * 0.51).abs() < 1e-7, "linear taper halfway along the last span");
        let (yz, xy) = (profiles("yz"), profiles("xy"));
        assert_eq!((yz.len(), xy.len(), xy[0].len()), (5, 3, 8), "custom steps, duplicate removal and sides");
        assert!(center(&yz[1]).distance(Vec3d::new(0., 13. / 24., -1. / 12.)) < 1e-7, "centripetal midpoint for unequal chord lengths");
        for (rings, axis) in [(yz, 0), (xy, 2)] {
            for ring in &rings[1..] {
                assert!(rings[0].iter().zip(ring).all(|(a, b)| (a[axis] - b[axis]).abs() < 1e-7),
                    "corresponding vertices keep their offset perpendicular to the bend plane");
            }
        }
    }

    #[test]
    fn text_material_rejects_pixels_and_exports_msdf_mask() {
        #[derive(Default)]
        struct Messages(std::cell::RefCell<Vec<String>>);
        impl ModelHost for Messages { fn log(&self, message: String) { self.0.borrow_mut().push(message); } }
        let host_box = Box::leak(Box::new(ScriptVmHost::new((), ())));
        let mut vm = ScriptVm { host: host_box.host_context(), bx: Box::new(ScriptVmBase::new()) };
        let messages = Rc::new(Messages::default());
        let host: Rc<dyn ModelHost> = messages.clone();
        let module = vm.new_module(id!(game));
        vm.add_method(module, id!(model_new), &[], move |vm, args| v_model_new(vm, &host, args));
        vm.capture_errors();
        let source = r#"use mod.pod.*
let m = mod.game.model_new("text-test")
let rejected = m.text_material("27", {pixels: 4})
let ink = m.text_material("27", {resolution: 257, font: "roboto-flex", width: 75})
let label = m.box({size: vec3(1, 1, 0.1), material: ink})
m
"#;
        let out = vm.eval(ScriptMod::new("", "", "text_material_test.splash", 1, 0, source, &[]));
        assert!(vm.take_errors().is_empty());
        let messages = messages.0.borrow();
        assert_eq!(messages.len(), 1, "only pixels is refused: {messages:?}");
        assert!(messages[0].contains("pixels") && messages[0].contains("resolution"));
        let h = out.as_handle().unwrap();
        let b = &vm.bx.heap_ref().handle_ref::<BuilderHandle>(h).unwrap().0;
        assert_eq!(b.materials.len(), 1, "rejected material leaves no recorder entry");
        let built = makepad_model::build_program(&b.ops, makepad_model::Limits::default(), &mut |_| Err("no external inputs".into()), None).unwrap();
        let surface = &built.document.surface().materials[&1];
        assert_eq!(surface.alpha, SurfaceAlpha::Mask);
        assert_eq!(surface.alpha_cutoff, 0.5);
        assert_eq!(surface.shading.unwrap().msdf, 1.0);
        let glb = makepad_gltf::parse_glb_bytes(&built.compiled.glb).unwrap();
        let material = glb.document.materials.as_ref().unwrap().iter().find(|m| m.extras.as_ref()
            .and_then(|e|e.key("makepadShading")).and_then(|s|s.key("msdf")).and_then(|v|v.as_f64()) == Some(1.0)).unwrap();
        assert_eq!(material.alpha_mode.as_deref(), Some("MASK"));
        assert!(material.pbr_metallic_roughness.as_ref().unwrap().base_color_texture.is_some());
    }

    /// Parts are named by their bindings: a looped binding by index, a call inside another's
    /// arguments by its builder after the binding, an unbound call by builder and line; parts
    /// and materials are handles, and a string where a part goes is an error.
    #[test]
    fn parts_are_named_by_their_bindings() {
        let host_box = Box::leak(Box::new(ScriptVmHost::new((), ())));
        let mut vm = ScriptVm { host: host_box.host_context(), bx: Box::new(ScriptVmBase::new()) };
        let module = vm.new_module(id!(game));
        let host: Rc<dyn ModelHost> = Rc::new(Quiet);
        vm.add_method(module, id!(model_new), &[], move |vm, args| v_model_new(vm, &host, args));
        vm.capture_errors();
        let src = "use mod.pod.*\nlet game = mod.game\nfn ring(m, r) {\n    let tube = m.torus({radius: r tube: 0.1})\n    return tube\n}\nfn tower() {\n    let m = game.model_new(\"tower\")\n    let stone = m.material({roughness: 0.9})\n    let base = m.cylinder({radius: 4 height: 2 material: stone})\n    for k in 0..3 {\n        let band = m.cone({radius: 3 height: 1 pos: vec3(0, k, 0) parent: base})\n    }\n    let door = m.box({size: vec3(1, 2, 0.3)})\n    let hull = m.union(m.box({size: vec3(1, 1, 1)}), door)\n    let rim = ring(m, 2)\n    m.box({size: vec3(1, 1, 1)})\n    return m\n}\ntower()\n";
        let out = vm.eval(ScriptMod::new("", "", "builder_test.splash", 1, 0, src, &[]));
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "{errors:?}");
        let h = out.as_handle().expect("the builder");
        let b = &vm.bx.heap_ref().handle_ref::<BuilderHandle>(h).unwrap().0;
        let mut names: Vec<&str> = b.scene.nodes.keys().map(|k| k.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["band[0]", "band[1]", "band[2]", "base", "box@17", "hull", "rim.tube"]);
        assert_eq!(b.materials, vec!["stone".to_string()]);
        assert_eq!(b.scene.nodes["band[1]"].parent.as_deref(), Some("base"));
        assert!(b.ops.iter().any(|op| matches!(op, ProgramOp::Op(Operation::Construction(ConstructionOperation { object, kind: Construction::Shape { shape: Shape::Box { .. }, .. } })) if object == "hull.box")), "{:?}", b.ops);
    }

    #[test]
    fn box_segments_follow_radius_within_the_authored_maximum() {
        let mut host_box = Box::new(ScriptVmHost::new((), ()));
        let mut vm = ScriptVm { host: host_box.host_context(), bx: Box::new(ScriptVmBase::new()) };
        let module = vm.new_module(id!(game));
        let host: Rc<dyn ModelHost> = Rc::new(Quiet);
        vm.add_method(module, id!(model_new), &[], move |vm, args| v_model_new(vm, &host, args));
        vm.capture_errors();
        let src = "use mod.pod.*\nlet game = mod.game\nlet m = game.model_new(\"box-segments\")\nlet tiny = m.box({size: vec3(2, 2, 2) radius: 0.002})\nlet small = m.box({size: vec3(2, 2, 2) radius: 0.006})\nlet large = m.box({size: vec3(2, 2, 2) radius: 0.5})\nlet capped = m.box({size: vec3(2, 2, 2) radius: 0.5 segments: 1})\nlet even_cap = m.box({size: vec3(2, 2, 2) radius: 0.5 segments: 2})\nlet finer = m.box({size: vec3(2, 2, 2) radius: 0.05 segments: 16})\nlet clamped = m.box({size: vec3(2, 0.004, 2) radius: 0.5})\nlet sharp = m.box({size: vec3(2, 2, 2)})\nlet sharp_authored = m.box({size: vec3(2, 2, 2) radius: 0 segments: 4})\nlet invalid_low = m.box({size: vec3(2, 2, 2) radius: 0.002 segments: 0})\nlet high = m.box({size: vec3(2, 2, 2) radius: 0.002 segments: 17})\nm\n";
        let out = vm.eval(ScriptMod::new("", "", "box_segments_test.splash", 1, 0, src, &[]));
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "{errors:?}");
        let h = out.as_handle().expect("the builder");
        let b = &vm.bx.heap_ref().handle_ref::<BuilderHandle>(h).unwrap().0;
        let boxes: Vec<_> = b.ops.iter().filter_map(|op| match op {
            ProgramOp::Op(Operation::Construction(ConstructionOperation { object, kind: Construction::Shape { shape: Shape::Box { segments, .. }, .. } })) => Some((object.as_str(), *segments)),
            _ => None,
        }).collect();
        assert_eq!(boxes, vec![("tiny", 1), ("small", 3), ("large", 3), ("capped", 1), ("even_cap", 1), ("finer", 7), ("clamped", 1), ("sharp", 3), ("sharp_authored", 4), ("invalid_low", 0), ("high", 1)], "recorded segments follow the clamped radius and authored maximum; 17 segments is valid and follows the radius; sharp boxes and a zero count are kept");
    }

    #[test]
    fn lod_accepts_a_final_empty_level() {
        let host_box = Box::leak(Box::new(ScriptVmHost::new((), ())));
        let mut vm = ScriptVm { host: host_box.host_context(), bx: Box::new(ScriptVmBase::new()) };
        let module = vm.new_module(id!(game));
        let host: Rc<dyn ModelHost> = Rc::new(Quiet);
        vm.add_method(module, id!(model_new), &[], move |vm, args| v_model_new(vm, &host, args));
        vm.capture_errors();
        let src = "use mod.pod.*\nlet m = mod.game.model_new(\"lod-part\")\nlet part = m.box({size: vec3(1, 1, 1)})\nm.lod(part, [{faces: 4 error: 0.01 distance: 10}, {faces: 0 distance: 30}])\nm\n";
        let out = vm.eval(ScriptMod::new("", "", "lod_test.splash", 1, 0, src, &[]));
        assert!(vm.take_errors().is_empty());
        let b = &vm.bx.heap_ref().handle_ref::<BuilderHandle>(out.as_handle().unwrap()).unwrap().0;
        assert!(b.ops.iter().any(|op| matches!(op, ProgramOp::Op(Operation::Scene(SceneOperation::Lods { levels, .. })) if levels.len()==2 && levels[1].target_faces==0)));
    }

    #[test]
    fn a_posted_program_is_queued_once_and_answers_its_state() {
        let alias = "gen/mdl/test-registry-crate";
        let program = vec![ProgramOp::Op(Operation::Construction(ConstructionOperation { object: "b".into(), kind: Construction::Shape { shape: Shape::Box { size: [1.; 3], radius: 0., segments: 3 }, material: 0 } }))];
        let ((), revisions) = observe_script_models(|| {
            request_builtin_model(alias, "prop", program.clone());
            request_builtin_model(alias, "prop", program.clone());
        });
        assert_eq!(revisions.len(), 1, "an evaluation observes each alias once");
        assert_eq!(revisions[0].state(), Some(ScriptModelState::Building));
        let posted: Vec<ScriptModelRequest> = take_script_model_requests().into_iter().filter(|r| r.alias == alias).collect();
        assert_eq!(posted.len(), 1, "an unchanged program is not posted again");
        let hash = posted[0].hash.clone();
        assert_eq!(hash.len(), 16);
        assert_eq!(script_model_state(alias), ScriptModelState::Building);
        assert!(pending_script_models().contains(alias));
        mark_script_model_failed(alias, &hash, "no");
        assert_eq!(script_model_state(alias), ScriptModelState::Failed("no".into()));
        mark_script_model_installed(alias, &hash);
        assert_eq!(script_model_state(alias), ScriptModelState::Ready);
        let ((), unchanged) = observe_script_models(|| request_builtin_model(alias, "prop", program));
        assert_eq!(unchanged.len(), 1, "unchanged, already installed models are part of the reply too");
        assert_eq!(unchanged[0].state(), Some(ScriptModelState::Ready));
        assert!(take_script_model_arrivals());
        let revision = script_model_revision(alias).unwrap();
        revision.request_install();
        revision.request_install();
        assert_eq!(revision.state(), Some(ScriptModelState::Building));
        let retry: Vec<_> = take_script_model_requests().into_iter().filter(|r| r.alias == alias).collect();
        assert_eq!(retry.len(), 1, "missing resident bytes queue one reinstall");
        assert_eq!(retry[0].hash, hash);
        assert_eq!(retry[0].class, posted[0].class);
        assert_eq!(retry[0].title, posted[0].title);
        assert!(Arc::ptr_eq(&retry[0].program, &posted[0].program), "reinstall uses the original program");
        assert!(Arc::ptr_eq(&retry[0].materials, &posted[0].materials));
        assert!(Arc::ptr_eq(&retry[0].texture_timings, &posted[0].texture_timings));
        mark_script_model_installed(alias, &hash);
        request_builtin_model(alias, "prop", Vec::new());
        revision.request_install();
        let newer: Vec<_> = take_script_model_requests().into_iter().filter(|r| r.alias == alias).collect();
        assert_eq!(newer.len(), 1, "an old snapshot cannot requeue a superseded program");
        assert_ne!(newer[0].hash, hash);
        forget_script_model_install(alias);
        assert_eq!(script_model_state(alias), ScriptModelState::Unknown);
        assert_eq!(revisions[0].state(), None, "an evicted revision no longer holds a reply");
    }

    #[test]
    fn script_model_program_reads_admitted_request_without_draining_queue() {
        struct WithholdAll;
        impl ModelHost for WithholdAll {
            fn log(&self, _message: String) {}
            fn admit_program(&self, _alias: &str, _bytes: usize) -> Admit {
                Admit::Withhold("over budget".into())
            }
        }

        let alias = "gen/mdl/test-program-lookup";
        let withheld_alias = "gen/mdl/test-program-lookup-withheld";
        forget_script_model_install(alias);
        forget_script_model_install(withheld_alias);
        assert!(script_model_program(alias).is_none());

        let program = vec![ProgramOp::Op(Operation::Construction(ConstructionOperation {
            object: "b".into(),
            kind: Construction::Shape {
                shape: Shape::Box { size: [1.; 3], radius: 0., segments: 3 },
                material: 0,
            },
        }))];
        request_builtin_model(alias, "prop", program.clone());

        let peeked = script_model_program(alias).expect("admitted program must be retained");
        assert_eq!(peeked.alias, alias);
        assert_eq!(peeked.class, "prop");
        assert_eq!(peeked.program.len(), 1);

        // Lookup must not drain the build queue.
        let queued: Vec<_> = take_script_model_requests().into_iter().filter(|r| r.alias == alias).collect();
        assert_eq!(queued.len(), 1, "peeking must leave the build queue intact");
        assert_eq!(queued[0].hash, peeked.hash);
        assert!(Arc::ptr_eq(&queued[0].program, &peeked.program));

        // Still readable after the queue is drained.
        let after_drain = script_model_program(alias).expect("retained after queue drain");
        assert_eq!(after_drain.hash, peeked.hash);
        assert!(Arc::ptr_eq(&after_drain.program, &peeked.program));

        // Withheld revisions drop their program and return None.
        request_model_for(&WithholdAll, withheld_alias, "prop", program);
        assert!(script_model_program(withheld_alias).is_none(), "withheld revision must not return an empty program stub");

        // Eviction clears the lookup.
        forget_script_model_install(alias);
        forget_script_model_install(withheld_alias);
        assert!(script_model_program(alias).is_none());
    }
}
