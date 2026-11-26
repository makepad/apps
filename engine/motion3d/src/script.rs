//! The Splash side of a Scene3D document: the `mod.scene` module a scene
//! is written in (node, geometry, material, light and effect types, and
//! the scene's time and animation natives), the reader that turns evaluated
//! values into typed data with teaching diagnostics, and the VM that keeps
//! a document's `fn(t)` closures alive for per-frame evaluation.
//!
//! Type names follow three.js (`MeshPhysicalMaterial`, `TorusKnotGeometry`,
//! `SpotLight`) with snake_case parameters (`clearcoat_roughness`), because
//! that is the vocabulary people and models already know; camelCase is
//! answered with the snake_case spelling.

use crate::anim::{self, Anim, AudioFeature, Ease, Key, LoopMode};
use makepad_widgets::makepad_script::numeric::{NumericValue, NumericValueTag};
use makepad_widgets::*;
use std::collections::HashMap;

/// 1-based line and column in the document's own source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct Loc {
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
}

/// A problem in a document: where, which property, what, and the fix.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub loc: Loc,
    /// `children[2].material.roughness`; empty for the whole document.
    pub path: String,
    pub message: String,
    pub hint: Option<String>,
}

impl Diagnostic {
    pub fn error(loc: Loc, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { severity: Severity::Error, loc, path: path.into(), message: message.into(), hint: None }
    }
    pub fn warning(loc: Loc, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { severity: Severity::Warning, loc, path: path.into(), message: message.into(), hint: None }
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let severity = if self.is_error() { "error" } else { "warning" };
        write!(f, "{}:{}: {severity}: ", self.loc.line, self.loc.col)?;
        if !self.path.is_empty() {
            write!(f, "{}: ", self.path)?;
        }
        write!(f, "{}", self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, "\n    hint: {hint}")?;
        }
        Ok(())
    }
}

/// An input value a host fills (`in(@name)`): the document's parameters.
#[derive(Clone, Debug, PartialEq)]
pub enum InputValue {
    Number(f64),
    Text(String),
    /// Straight sRGB RGBA 0..1, as written.
    Color([f32; 4]),
    Vec3([f32; 3]),
    Bool(bool),
    /// A list (`in(@words)[2]`).
    List(Vec<InputValue>),
    /// An object of named values (`in(@title).size`).
    Record(Vec<(String, InputValue)>),
}

/// The audio a closure reads through `music(@m)` / `voice(@vo)` at the
/// moment it runs (the scene's music source, sampled at the edit time).
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioNow {
    pub energy: f32,
    pub low: f32,
    pub mid: f32,
    pub high: f32,
    pub beat: f32,
    pub beat_phase: f32,
    pub bar_phase: f32,
    pub beat_index: f32,
    pub onset: f32,
    pub voice: f32,
}

/// A song's timing, on the scene's clock: what `word(..)`, `line(..)`,
/// `beat_near(..)` and `downbeat_near(..)` read, so a scene is timed by the
/// lyric and the beat grid instead of hand-written seconds.
#[derive(Clone, Debug, Default)]
pub struct SongTiming {
    pub words: Vec<SongWord>,
    /// Beat times, increasing.
    pub beats: Vec<f32>,
    pub beats_per_bar: u32,
    /// Index into `beats` of the first downbeat.
    pub downbeat: u32,
}

#[derive(Clone, Debug, Default)]
pub struct SongWord {
    pub text: String,
    pub start: f32,
    pub end: f32,
    pub line: u32,
}

/// Lower case letters, digits and apostrophes: how words are matched.
fn word_key(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric() || *c == '\'').flat_map(char::to_lowercase).collect()
}

impl SongTiming {
    /// The `nth` occurrence of a word, or of a phrase (consecutive words).
    pub fn find(&self, text: &str, nth: usize) -> Option<(usize, usize)> {
        self.find_after(text, nth, None)
    }

    /// [`Self::find`] counting only runs that start at or after `after`.
    pub fn find_after(&self, text: &str, nth: usize, after: Option<f32>) -> Option<(usize, usize)> {
        let want: Vec<String> = text.split_whitespace().map(word_key).filter(|w| !w.is_empty()).collect();
        if want.is_empty() {
            return None;
        }
        let keys: Vec<String> = self.words.iter().map(|w| word_key(&w.text)).collect();
        (0..keys.len().saturating_sub(want.len() - 1))
            .filter(|&i| keys[i..i + want.len()] == want[..] && after.is_none_or(|a| self.words[i].start >= a))
            .nth(nth)
            .map(|i| (i, i + want.len() - 1))
    }

    /// The beat time nearest `t` (among downbeats with `down`).
    pub fn nearest_beat(&self, t: f32, down: bool) -> Option<f32> {
        let per = self.beats_per_bar.max(1) as usize;
        let d = self.downbeat as usize % per;
        self.beats.iter().enumerate().filter(|(i, _)| !down || (*i + per - d) % per == 0).map(|(_, b)| *b).min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
    }
}

/// Host state the natives read.
#[derive(Default)]
pub struct M3Host {
    pub inputs: HashMap<String, InputValue>,
    /// While the document loads: the inputs it reads (`in(@name)`, the
    /// tempo); after, None. A change to any other input reaches the loaded
    /// document through [`crate::doc::Scene3dDoc::set_inputs`]: only its
    /// per-frame closures read it.
    pub read_at_load: Option<std::collections::HashSet<String>>,
    /// Set while closures run: `music(...)` then returns live values.
    pub evaluating: bool,
    pub audio_now: AudioNow,
    /// The document's kernels (its `Kernel{...}` and `Sim{...}` objects,
    /// loaded: crate::objects).
    pub kernels: crate::kernels::DocKernels,
    /// The host's kernel entries in this VM (crate::objects::HostEntries).
    pub kernel_entries: Option<std::rc::Rc<crate::objects::HostEntries>>,
    /// The song's words and beats, when the host has them.
    pub song: Option<std::sync::Arc<SongTiming>>,
    /// Errors from natives that ran at load (kernels), for the reader.
    pub load_errors: Vec<(Loc, String)>,
    /// What the model builders (`mod.model`) said while the document
    /// evaluated: its warnings.
    pub model_messages: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    /// A Motion document's VM: forget what its `memo()` kept (a new
    /// evaluation of a scene begins). Standalone VMs clear [`memo_clear`]'s.
    pub new_evaluation: Option<fn(&mut ScriptVm)>,
    /// A Motion document's VM: some types are Motion's prototypes (marked
    /// `__host`), whose own field values (a post kit's defaults) are Motion's
    /// meaning: a scene reads what its objects write, not those.
    pub adopted: bool,
}

/// The standalone VM a scene-only source lives in (a Motion document's
/// scenes live in the document's own VM, see [`install_document`]). Its
/// [`M3Host`] is in the std slot, where a document's VM keeps it too.
pub struct DocVm {
    pub host: Box<ScriptVmHost<(), M3Host>>,
    pub base: Option<Box<ScriptVmBase>>,
}

/// Natives a host puts on every scene VM's `mod.scene` (Motion's text functions: a scene runs its document's
/// top-level code again, and that code may lay text out at load).
static HOST_NATIVES: std::sync::OnceLock<fn(&mut ScriptVm, ScriptObject)> = std::sync::OnceLock::new();

/// Give every scene VM made from now on the host's natives (`f(vm, mod.scene)`); the first call wins.
pub fn set_host_natives(f: fn(&mut ScriptVm, ScriptObject)) {
    let _ = HOST_NATIVES.set(f);
}

impl DocVm {
    pub fn new(inputs: HashMap<String, InputValue>) -> Self {
        let mut vm = DocVm { host: Box::new(ScriptVmHost::new((), M3Host { inputs, read_at_load: Some(Default::default()), ..Default::default() })), base: Some(Box::new(ScriptVmBase::new())) };
        vm.with(|vm| {
            vm.set_script_domain("scene3d");
            let scene = register(vm);
            if let Some(f) = HOST_NATIVES.get() {
                f(vm, scene);
            }
            crate::models::register(vm);
            crate::kits::install(vm);
            load_modules(vm, &[]);
        });
        vm
    }

    pub fn with<R>(&mut self, f: impl FnOnce(&mut ScriptVm) -> R) -> R {
        let bx = self.base.take().expect("DocVm re-entered");
        let mut vm = ScriptVm {
            host: self.host.host_context(),
            bx,
        };
        let r = f(&mut vm);
        self.base = Some(vm.bx);
        r
    }
}

/// What a standalone scene's source is evaluated after: the language core
/// (`mod.std`, the maths and the vector types). The scene writes its
/// vocabulary itself: `use mod.scene.*`, `use mod.post` (then
/// `post.Glow{..}`), `use mod.kernel` when it declares kernels.
pub const PRELUDE: &str = "use mod.std.*\nuse mod.math.*\nuse mod.pod.*\n";

/// The core and the scene vocabulary: for the scene snippets the host writes itself
/// (a look's light rig).
pub const SCENE_PRELUDE: &str = "use mod.std.*\nuse mod.math.*\nuse mod.pod.*\nuse mod.scene.*\n";
pub const SCENE_PRELUDE_LINES: u32 = 4;

/// `mod.shared` (eases, `wrap`, `sd_*`, ...) in a scene's VM: `mod.scene`
/// carries it (see [`register`]).
fn register_host_kits(vm: &mut ScriptVm) {
    if makepad_script_compute::module::register_shared_std(vm) {
        makepad_tween_script::install_ease(vm);
    }
}

/// `value` as the named module `mod.motion.modules.<name>` (after
/// [`load_modules`] made that table).
pub fn set_named_module(vm: &mut ScriptVm, name: &str, value: ScriptValue) {
    let Some(motion) = vm.bx.heap_ref().value(vm.modules(), id!(motion).into(), NoTrap).as_object() else { return };
    let Some(named) = vm.bx.heap_ref().value(motion, id_lut!(modules).into(), NoTrap).as_object() else { return };
    let key = LiveId::from_str_with_lut(name).unwrap_or_else(|_| LiveId::from_str(name));
    vm.bx.heap_mut().set_value_def(named, key.into(), value);
}

/// The host document's Splash modules in a scene's VM: `import(asset(id,
/// rev))` (on `mod.scene` and `mod.motion`, so in scope) and `mod.motion.modules.<name>` for the
/// built-in ones the document uses, evaluated in order (dependencies first).
/// What went wrong, if anything.
pub fn load_modules(vm: &mut ScriptVm, modules: &[makepad_script_imports::LoadedModule]) -> Vec<String> {
    let motion = vm.new_module(id!(motion));
    let named = vm.bx.heap_mut().new_object();
    vm.bx.heap_mut().set_value_def(motion, id_lut!(modules).into(), named.into());
    let scene = vm.bx.heap_ref().value(vm.modules(), id!(scene).into(), NoTrap).as_object().expect("mod.scene");
    // A module of a Motion document (`use mod.motion.*`) runs here with the
    // scene's names: this VM's `mod.motion` carries `mod.scene`'s.
    crate::forms::reexport(vm, scene, motion);
    let core = makepad_script_imports::language_core(vm);
    let errors = makepad_script_imports::load(vm, motion, &core, modules, Some(named), 20_000_000);
    let import = vm.bx.heap_ref().value(motion, id_lut!(import).into(), NoTrap);
    vm.bx.heap_mut().set_value_def(scene, id_lut!(import).into(), import);
    errors.into_iter().map(|e| e.text().to_owned()).collect()
}

pub const PRELUDE_LINES: u32 = 3;

// ---------------------------------------------------------------------------
// The module
// ---------------------------------------------------------------------------

/// Every type a document can construct: (Splash name, kind). Aliases share
/// a kind. The kind is stored on the prototype as `__m3`.
pub const TYPES: &[(&str, &str)] = &[
    ("Scene3D", "scene"),
    ("Group3D", "group"),
    ("Object3D", "group"),
    ("Mesh", "mesh"),
    ("Model", "model"),
    ("Text3D", "text3d"),
    ("Instances", "instances"),
    ("InstancedMesh", "instances"),
    ("Points", "points"),
    ("Particles", "particles"),
    ("Line3D", "line"),
    // Cameras.
    ("Camera", "camera"),
    ("PerspectiveCamera", "camera"),
    ("OrthographicCamera", "ortho_camera"),
    ("Camera3D", "camera"),
    // Lights.
    ("AmbientLight", "ambient_light"),
    ("HemisphereLight", "hemisphere_light"),
    ("DirectionalLight", "directional_light"),
    ("PointLight", "point_light"),
    ("SpotLight", "spot_light"),
    ("Light3D", "light3d"),
    // Geometry.
    ("BoxGeometry", "box"),
    ("SphereGeometry", "sphere"),
    ("TorusGeometry", "torus"),
    ("TorusKnotGeometry", "torus_knot"),
    ("CylinderGeometry", "cylinder"),
    ("ConeGeometry", "cone"),
    ("PlaneGeometry", "plane"),
    ("CircleGeometry", "circle"),
    ("RingGeometry", "ring"),
    ("CapsuleGeometry", "capsule"),
    ("TetrahedronGeometry", "tetrahedron"),
    ("OctahedronGeometry", "octahedron"),
    ("IcosahedronGeometry", "icosahedron"),
    ("DodecahedronGeometry", "dodecahedron"),
    ("ShapeGeometry", "shape_geo"),
    ("ExtrudeGeometry", "extrude"),
    ("LatheGeometry", "lathe"),
    ("TubeGeometry", "tube"),
    ("ParametricGeometry", "parametric"),
    ("HeightfieldGeometry", "heightfield"),
    ("SvgGeometry", "svg_geo"),
    ("EngineGeometry", "engine_geo"),
    ("RibbonGeometry", "ribbon_geo"),
    // 2D shapes for extrusion.
    ("RectShape", "rect_shape"),
    ("CircleShape", "circle_shape"),
    ("StarShape", "star_shape"),
    ("PolygonShape", "polygon_shape"),
    ("PathShape", "path_shape"),
    // Curves.
    ("CatmullRomCurve3", "catmull_rom"),
    ("CubicBezierCurve3", "bezier_curve"),
    ("LineCurve3", "polyline"),
    ("CircleCurve", "circle_curve"),
    ("HelixCurve", "helix_curve"),
    ("LissajousCurve", "lissajous_curve"),
    ("TorusKnotCurve", "torus_knot_curve"),
    // Materials.
    ("MeshStandardMaterial", "standard"),
    ("MeshPhysicalMaterial", "physical"),
    ("MeshBasicMaterial", "basic"),
    ("MeshLambertMaterial", "lambert"),
    ("MeshPhongMaterial", "phong"),
    ("MeshToonMaterial", "toon"),
    ("MeshMatcapMaterial", "matcap"),
    ("MeshNormalMaterial", "normal"),
    ("MeshDepthMaterial", "depth"),
    ("ShaderMaterial", "shader"),
    ("Material3D", "material3d"),
    // Instance layouts.
    ("GridLayout", "grid_layout"),
    ("LineLayout", "line_layout"),
    ("CircleLayout", "circle_layout"),
    ("SphereLayout", "sphere_layout"),
    ("CubeLayout", "cube_layout"),
    ("BallLayout", "ball_layout"),
    ("SpiralLayout", "spiral_layout"),
    ("HelixLayout", "helix_layout"),
    ("PhyllotaxisLayout", "phyllotaxis_layout"),
    ("SurfaceLayout", "surface_layout"),
    ("PointsLayout", "points_layout"),
    ("CurveLayout", "curve_layout"),
    ("TextLayout", "text_layout"),
    ("VoxelLayout", "voxel_layout"),
    // Particle emitters.
    ("PointEmitter", "point_emitter"),
    ("SphereEmitter", "sphere_emitter"),
    ("BoxEmitter", "box_emitter"),
    ("DiscEmitter", "disc_emitter"),
    ("RingEmitter", "ring_emitter"),
    ("LineEmitter", "line_emitter"),
    ("SurfaceEmitter", "surface_emitter"),
    ("CurveEmitter", "curve_emitter"),
    ("TextEmitter", "text_emitter"),
    // Scene settings.
    ("Environment", "environment"),
    ("Fog", "fog"),
    ("FogExp2", "fog_exp2"),
    ("Shadows", "shadows"),
    // Post.
    ("SSAO", "ssao"),
    ("FilmGrain", "grain"),
    ("ColorGrade", "grade"),
    ("ContactShadows", "contact"),
    // Helpers.
    ("LightRig", "light_rig"),
    ("Ground", "ground"),
    // Scene kits (built by Splash templates in `kits.rs`).
    ("Device", "device"),
    ("Turntable", "turntable"),
    ("Field", "field"),
    ("ShapeSet", "shape_set"),
    ("Fx", "fx"),
    ("Crowd", "crowd"),
    ("Spotlights", "spotlights"),
    ("Callout", "callout"),
    ("Seq3D", "seq3d"),
    ("Paper", "paper"),
    ("ZoomBlur", "zoom_blur"),
    ("LightShafts", "light_shafts"),
    ("PostPass", "post_pass"),
    // Camera shots and moves, curves and layouts.
    ("CameraShot", "camera_shot"),
    ("PaperclipCurve", "paperclip_curve"),
    ("BraidCurve", "braid_curve"),
    ("FnCurve", "fn_curve"),
    ("FieldGeometry", "field_geo"),
    ("CrowdLayout", "crowd_layout"),
    ("MazeLayout", "maze_layout"),
    ("Heightfield", "heightfield"),
    ("CameraMove", "camera_move"),
    // Effect physics (KERNELS.md §3.6).
    ("Physics", "physics"),
    ("Body", "body"),
    ("Spawn", "spawn"),
    // Model parts.
    ("AnimationClip", "clip"),
    ("Clip", "clip"),
    ("NodeOverride", "node_override"),
    ("Node3D", "node_override"),
    ("MaterialOverride", "material_override"),
];

/// Kind of an animation value made by a native.
const ANIM_KIND: &str = "__anim";

/// The scene native a value stands for when it is one of the animation forms
/// (`mod.forms`: a `tween(..)` is `"tween"`), by its prototype chain
/// against the records `mod.forms` lists ([`crate::forms::READS_AS`]).
fn form_op_of(heap: &ScriptHeap, o: ScriptObject) -> Option<String> {
    let module = ScriptValue::from(heap.module(id!(forms))).as_object()?;
    let forms = heap.value(module, id!(__forms).into(), NoTrap).as_object()?;
    let mut at = heap.proto(o).as_object();
    let mut depth = 0;
    while let Some(level) = at {
        for entry in heap.map_entries(forms).iter() {
            if entry.value.as_object() == Some(level) {
                return entry.key.as_id().map(|id| id.to_string());
            }
        }
        depth += 1;
        if depth > 64 {
            return None;
        }
        at = heap.proto(level).as_object();
    }
    None
}

/// Whether `v` is an animation value: a scene native's, or one of Motion's
/// forms.
pub(crate) fn is_animation_value(heap: &ScriptHeap, v: ScriptValue) -> bool {
    let Some(o) = v.as_object() else { return false };
    heap.value(o, id!(__m3).into(), NoTrap).as_id() == Some(intern(ANIM_KIND)) || form_op_of(heap, o).is_some()
}

/// An id whose name `to_string` can give back.
fn intern(s: &str) -> LiveId {
    LiveId::from_str_with_lut(s).unwrap_or_else(|_| LiveId::from_str(s))
}

/// An animation value made by a native: `kind` and its fields.
fn tagged(vm: &mut ScriptVm, kind: &str, fields: &[(LiveId, ScriptValue)]) -> ScriptValue {
    let o = vm.bx.heap_mut().new_object();
    let ip = vm.trap_ip();
    vm.bx.heap_mut().set_made_at(o, ip);
    vm.bx.heap_mut().set_value_def(o, id!(__m3).into(), intern(ANIM_KIND).into());
    vm.bx.heap_mut().set_value_def(o, id!(op).into(), intern(kind).into());
    for (k, v) in fields {
        vm.bx.heap_mut().set_value_def(o, (*k).into(), *v);
    }
    o.into()
}

/// An input value as the scene's script sees it (`in(@name)`).
pub fn input_to_script(vm: &mut ScriptVm, value: &InputValue) -> ScriptValue {
    match value {
        InputValue::Number(n) => (*n).into(),
        InputValue::Bool(b) => (*b).into(),
        InputValue::Text(s) => vm.new_string_with(|_, out| out.push_str(s)),
        InputValue::Color(c) => ScriptValue::from_color(Vec4f { x: c[0], y: c[1], z: c[2], w: c[3] }.to_u32()),
        InputValue::Vec3(v) => tagged(vm, "vec3", &[(id!(x), (v[0] as f64).into()), (id!(y), (v[1] as f64).into()), (id!(z), (v[2] as f64).into())]),
        InputValue::List(items) => {
            let array = vm.bx.heap_mut().new_array();
            for item in items {
                let item = input_to_script(vm, item);
                vm.bx.heap_mut().array_push(array, item, NoTrap);
            }
            array.into()
        }
        InputValue::Record(fields) => {
            let object = vm.bx.heap_mut().new_object();
            for (name, value) in fields {
                let value = input_to_script(vm, value);
                vm.bx.heap_mut().set_value_def(object, LiveId::from_str(name).into(), value);
            }
            object.into()
        }
    }
}

/// While a scene's closure runs, `music(@m)` / `voice(@vo)` give an object
/// with every audio feature's level at that moment (the host's
/// [`AudioNow`]); otherwise `None` (they give the track, or at load a
/// binding). Motion's `music`/`voice` call this, so one native serves a
/// document and its scenes.
pub fn live_audio(vm: &mut ScriptVm) -> Option<ScriptValue> {
    let (evaluating, now) = host(vm).map(|h| (h.evaluating, h.audio_now))?;
    if !evaluating {
        return None;
    }
    let o = vm.bx.heap_mut().new_object();
    let set = |vm: &mut ScriptVm, k: LiveId, v: f32| vm.bx.heap_mut().set_value_def(o, k.into(), (v as f64).into());
    set(vm, id!(energy), now.energy);
    set(vm, id!(low), now.low);
    set(vm, id!(bass), now.low);
    set(vm, id!(mid), now.mid);
    set(vm, id!(high), now.high);
    set(vm, id!(treble), now.high);
    set(vm, id!(beat), now.beat);
    set(vm, id!(beat_phase), now.beat_phase);
    set(vm, id!(bar_phase), now.bar_phase);
    set(vm, id!(beat_index), now.beat_index);
    set(vm, id!(onset), now.onset);
    set(vm, id!(amplitude), now.voice);
    set(vm, id!(voice), now.voice);
    Some(o.into())
}

/// Types a Motion document and its scenes share, one prototype each: the
/// scene's name for what `mod.motion` registers (the layer types and the
/// camera), read as the scene's kind.
const ADOPTED: &[&str] = &["Scene3D", "Text3D", "Seq3D", "Camera"];

/// A standalone scene VM's modules: `mod.forms`, `mod.shared`, `mod.post`,
/// `mod.kernel` and `mod.scene` (the scene vocabulary, carrying the forms
/// and the stdlib). The scene module.
pub fn register(vm: &mut ScriptVm) -> ScriptObject {
    let forms = vm.new_module(id!(forms));
    crate::forms::plain_types(vm, forms);
    let errors = crate::forms::install(vm, forms);
    debug_assert!(errors.is_empty(), "mod.forms: {errors:?}");
    register_host_kits(vm);
    let errors = makepad_render_graph::script::install_post(vm);
    debug_assert!(errors.is_empty(), "mod.post: {errors:?}");
    install_kernels(vm);
    let m = vm.new_module(id!(scene));
    register_vocabulary(vm, m, None);
    finish(vm, m, None);
    m
}

/// A Motion document's VM gets the scene vocabulary, `mod.scene` (types,
/// natives and kits for `Scene3D{..}` objects), and `mod.kernel`, beside its
/// `mod.motion`, `mod.forms`, `mod.shared` and `mod.post`. The types both
/// have ([`ADOPTED`]) are Motion's prototypes.
pub fn install_document(vm: &mut ScriptVm, motion: ScriptObject) {
    install_kernels(vm);
    let m = vm.new_module(id!(scene));
    register_vocabulary(vm, m, Some(motion));
    finish(vm, m, Some(motion));
    crate::kits::install(vm);
}

/// The module `name` of the VM, if it has one.
fn module_of(vm: &ScriptVm, name: LiveId) -> Option<ScriptObject> {
    vm.bx.heap_ref().value(vm.modules(), name.into(), NoTrap).as_object()
}

/// The last of a scene module's setup: it carries the forms and the shared
/// stdlib (what it has of its own wins), and the post protos are read as the
/// scene's kinds. In a document, `mod.motion` has the kernel declarations
/// `Kernel{..}` and `Sim{..}` too.
fn finish(vm: &mut ScriptVm, scene: ScriptObject, motion: Option<ScriptObject>) {
    for name in [id!(forms), id!(shared)] {
        if let Some(from) = module_of(vm, name) {
            crate::forms::reexport(vm, from, scene);
        }
    }
    if let Some(kernel) = module_of(vm, id!(kernel)) {
        for name in ["Kernel", "Sim"] {
            let value = vm.bx.heap_ref().value(kernel, LiveId::from_str(name).into(), NoTrap);
            for target in std::iter::once(scene).chain(motion) {
                vm.bx.heap_mut().set_value_def(target, LiveId::from_str(name).into(), value);
            }
        }
    }
    // `post.Glow{..}` is read as the kind `glow`; its own kit defaults are
    // the render graph's meaning, not the scene's.
    if let Some(post) = module_of(vm, id!(post)) {
        let types = [("Pass", "pass"), ("MotionBlur", "motion_blur")].into_iter().chain(makepad_render_graph::kits::KITS.iter().map(|k| (k.name, k.kind)));
        for (name, kind) in types {
            if let Some(proto) = vm.bx.heap_ref().value(post, LiveId::from_str(name).into(), NoTrap).as_object() {
                vm.bx.heap_mut().set_value_def(proto, id_lut!(__m3).into(), intern(kind).into());
                vm.bx.heap_mut().set_value_def(proto, id_lut!(__host).into(), true.into());
            }
        }
        if let Some(h) = host_mut(vm) {
            h.adopted = true;
        }
    }
}

fn register_vocabulary(vm: &mut ScriptVm, m: ScriptObject, document: Option<ScriptObject>) {
    // Motion's prototype of a type both have.
    let adopt = |vm: &ScriptVm, name: &str| -> Option<ScriptObject> {
        if !ADOPTED.contains(&name) {
            return None;
        }
        document.and_then(|d| vm.bx.heap_ref().value(d, LiveId::from_str(name).into(), NoTrap).as_object())
    };
    macro_rules! add {
        ($name:ident, $args:expr, $f:expr) => {
            vm.add_method(m, id_lut!($name), $args, $f);
        };
    }
    for (name, kind) in TYPES {
        if let Some(proto) = adopt(vm, name) {
            // Motion's prototype of the type, read as the scene's kind.
            vm.bx.heap_mut().set_value_def(proto, id_lut!(__m3).into(), intern(kind).into());
            vm.bx.heap_mut().set_value_def(proto, id_lut!(__host).into(), true.into());
            vm.bx.heap_mut().set_value_def(m, LiveId::from_str(name).into(), proto.into());
            if let Some(h) = host_mut(vm) {
                h.adopted = true;
            }
            continue;
        }
        let proto = vm.bx.heap_mut().new_object();
        vm.bx.heap_mut().set_value_def(proto, id!(__m3).into(), intern(kind).into());
        vm.bx.heap_mut().set_value_def(m, LiveId::from_str(name).into(), proto.into());
    }
    // The host document's modules name faces at their top level (`let MONO
    // = Font{family: "JetBrains Mono"}`): in a document `Font` is Motion's
    // type; a scene of its own draws no Motion text, so there a Font is a
    // plain record.
    let font = document.and_then(|d| vm.bx.heap_ref().value(d, id!(Font).into(), NoTrap).as_object()).unwrap_or_else(|| vm.bx.heap_mut().new_object());
    vm.bx.heap_mut().set_value_def(m, id_lut!(Font).into(), font.into());

    fn arg(vm: &ScriptVm, args: ScriptObject, name: LiveId) -> ScriptValue {
        let v = vm.bx.heap_ref().value(args, name.into(), NoTrap);
        if v.is_err() { NIL } else { v }
    }

    macro_rules! native {
        ($name:ident, [$($a:ident),*]) => {
            add!($name, script_args!($($a = NIL),*), |vm, args| {
                let fields = [$((id!($a), arg(vm, args, id!($a)))),*];
                tagged(vm, stringify!($name), &fields)
            });
        };
    }
    // The scene's time: `beat(n, offset)` is n beats after the scene starts,
    // `bar(n, beat, offset)` the same by bars (a document's `beat(n)` is the
    // song's beat n: `use mod.motion.*`).
    native!(beat, [n, offset]);
    native!(bar, [n, beat, offset]);
    native!(pingpong, [anim, period]);
    native!(delay, [anim, seconds]);
    native!(wiggle, [base, amount, frequency, seed]);
    native!(spin, [base, rate]);
    native!(add, [a, b]);
    native!(mul, [a, b]);
    native!(file, [path]);
    // Effect physics: a group's bodies (an Instances or Points source), its
    // contacts and events (kernel inputs, Points sources).
    native!(bodies, [group]);
    native!(contacts, [group]);
    native!(events, [group]);
    // `follow_music(@feature, lo, hi, smooth)` at load: a binding; inside a running
    // closure: an object with every feature's value now.
    fn audio_native(vm: &mut ScriptVm, args: ScriptObject, voice: bool) -> ScriptValue {
        if let Some(live) = live_audio(vm) {
            return live;
        }
        let fields = [
            (id!(feature), arg(vm, args, id!(feature))),
            (id!(lo), arg(vm, args, id!(lo))),
            (id!(hi), arg(vm, args, id!(hi))),
            (id!(smooth), arg(vm, args, id!(smooth))),
            (id!(source), arg(vm, args, id!(source))),
        ];
        tagged(vm, if voice { "voice" } else { "music" }, &fields)
    }
    add!(follow_music, script_args!(feature = NIL, lo = NIL, hi = NIL, smooth = NIL, source = NIL), |vm, args| audio_native(vm, args, false));
    add!(follow_voice, script_args!(feature = NIL, lo = NIL, hi = NIL, smooth = NIL, source = NIL), |vm, args| audio_native(vm, args, true));
    // `music(@m)` / `voice(@vo)`: the track (its live levels inside a fn(t)).
    add!(music, script_args!(feature = NIL), |vm, args| audio_native(vm, args, false));
    add!(voice, script_args!(feature = NIL), |vm, args| audio_native(vm, args, true));
    // `in(@name)` / `in(@name, default)`: a host parameter.
    vm.add_method(
        m,
        id_lut!(in),
        script_args!(name = NIL, default = NIL),
        |vm, args| {
            let name = arg(vm, args, id!(name));
            let default = arg(vm, args, id!(default));
            let key = name
                .as_id()
                .map(|id| id.to_string())
                .or_else(|| vm.bx.heap_mut().cast_to_owned_string(name, "in"));
            if let (Some(k), Some(h)) = (&key, host_mut(vm)) {
                if let Some(read) = &mut h.read_at_load {
                    read.insert(k.clone());
                }
            }
            let value = key.and_then(|k| host(vm).and_then(|h| h.inputs.get(&k).cloned()));
            match value {
                Some(value) => input_to_script(vm, &value),
                None => default,
            }
        },
    );
    register_song(vm, m);
    vm.set_object_op_hook(Some(anim_arith));
}

/// `mod.kernel`: the kernel vocabulary (`Kernel{..}`, `Sim{..}`,
/// `output(..)`, `emit_buffer(..)`, `param(..)`, `input(..)`, the record
/// layouts `Sprite`, `Segment`, ...), and the host's kernel entries in the
/// VM's [`M3Host`]: a document's kernels, of its 2D draws and its scenes,
/// load once from them. A file that declares kernels names it: `use
/// mod.kernel.*` (a file of kernels only) or `use mod.kernel` and
/// `kernel.input(..)`.
pub fn install_kernels(vm: &mut ScriptVm) {
    let module = vm.new_module(id!(kernel));
    let entries = crate::objects::register(vm, module);
    if let Some(h) = host_mut(vm) {
        h.kernel_entries = Some(std::rc::Rc::new(entries));
    }
}

/// Arithmetic on animation values (`tween(..) + vec3(..)`, `keys(..) * 2`,
/// `wiggle(..) - 1`, `beat(4) + 0.25`): the VM has none on objects, so the
/// document's VM hands it here and gets back the derived animation (the
/// `add(..)`/`mul(..)` a document can also write). Time markers (`beat`,
/// `bar`) plus or minus seconds move their offset. Anything else (media,
/// kernel handles, eases, objects of the scene) is left to the VM.
fn anim_arith(heap: &mut ScriptHeap, op: ScriptBinOp, a: ScriptValue, b: ScriptValue, ip: ScriptIp) -> Option<ScriptValue> {
    const ARITH: &[&str] = &["tween", "keys", "pingpong", "delay", "wiggle", "spin", "add", "mul", "repeat", "music", "voice", "vec3"];
    let kind = |heap: &ScriptHeap, v: ScriptValue| -> Option<String> {
        let o = v.as_object()?;
        if heap.value(o, id!(__m3).into(), NoTrap).as_id() != Some(intern(ANIM_KIND)) {
            return form_op_of(heap, o);
        }
        heap.value(o, id!(op).into(), NoTrap).as_id().map(|id| id.to_string())
    };
    let (ka, kb) = (kind(heap, a), kind(heap, b));
    let make = |heap: &mut ScriptHeap, op: &str, fields: &[(LiveId, ScriptValue)]| -> ScriptValue {
        let o = heap.new_object();
        heap.set_made_at(o, ip);
        heap.set_value_def(o, id!(__m3).into(), intern(ANIM_KIND).into());
        heap.set_value_def(o, id!(op).into(), intern(op).into());
        for (k, v) in fields {
            heap.set_value_def(o, (*k).into(), *v);
        }
        o.into()
    };
    // A time marker plus or minus seconds: the marker, moved.
    let marker = |k: &Option<String>| matches!(k.as_deref(), Some("beat") | Some("bar"));
    if matches!(op, ScriptBinOp::Add | ScriptBinOp::Sub) && (marker(&ka) || (marker(&kb) && op == ScriptBinOp::Add)) {
        let (m, k, secs) = if marker(&ka) { (a, ka.clone(), b.as_number()?) } else { (b, kb.clone(), a.as_number()?) };
        let secs = if op == ScriptBinOp::Sub { -secs } else { secs };
        let mo = m.as_object()?;
        let get = |heap: &ScriptHeap, f: LiveId| heap.value(mo, f.into(), NoTrap);
        let n = get(heap, id!(n));
        let offset = get(heap, id!(offset)).as_number().unwrap_or(0.0) + secs;
        let op_name = k.unwrap();
        let mut fields = vec![(id!(n), n), (id!(offset), offset.into())];
        if op_name == "bar" {
            fields.push((id!(beat), get(heap, id!(beat))));
        }
        return Some(make(heap, &op_name, &fields));
    }
    // Every object operand must be an animation; the other may be a number,
    // vector or colour.
    let arith = |k: &Option<String>, v: ScriptValue| match k {
        Some(k) => ARITH.contains(&k.as_str()),
        None => v.as_object().is_none() && !v.is_nil() && !v.is_string_like(),
    };
    if !(ka.is_some() || kb.is_some()) || !arith(&ka, a) || !arith(&kb, b) {
        return None;
    }
    Some(match op {
        ScriptBinOp::Add => make(heap, "add", &[(id!(a), a), (id!(b), b)]),
        ScriptBinOp::Mul => make(heap, "mul", &[(id!(a), a), (id!(b), b)]),
        ScriptBinOp::Sub => {
            let neg = make(heap, "mul", &[(id!(a), b), (id!(b), (-1.0f64).into())]);
            make(heap, "add", &[(id!(a), a), (id!(b), neg)])
        }
        ScriptBinOp::Div => {
            // Only by a number: an animation of the divisor has no form.
            let d = b.as_number().filter(|d| *d != 0.0)?;
            make(heap, "mul", &[(id!(a), a), (id!(b), (1.0 / d).into())])
        }
    })
}

/// The scene state of a VM: in the std slot of a standalone scene VM and of
/// a Motion document's VM alike.
pub fn host<'a>(vm: &'a ScriptVm) -> Option<&'a M3Host> {
    vm.host.std_ref::<M3Host>()
}

pub fn host_mut<'a>(vm: &'a mut ScriptVm<'_>) -> Option<&'a mut M3Host> {
    vm.host.std_mut::<M3Host>()
}

fn arg_of(vm: &ScriptVm, args: ScriptObject, name: LiveId) -> ScriptValue {
    let v = vm.bx.heap_ref().value(args, name.into(), NoTrap);
    if v.is_err() { NIL } else { v }
}

/// The song's timing on the scene's clock, in Motion's words:
/// `word(@song, "road")` is when that word (or phrase) starts,
/// `word(@song, "road", offset, nth)` with an offset and the nth match, and
/// `word(@song, 3)` the fourth word; `word_end(..)` is when it ends;
/// `words(@song, "We fold a silver")` is [vec2(start, end), ...] per word
/// (what Text3D's `karaoke:` takes); `beat_near(t)` / `downbeat_near(t)`
/// the nearest beat. The voice is the scene's song (one sung voice).
fn register_song(vm: &mut ScriptVm, m: ScriptObject) {
    macro_rules! add {
        ($name:ident, $args:expr, $f:expr) => {
            vm.add_method(m, id_lut!($name), $args, $f);
        };
    }
    /// (first word, last word) the arguments name, or an error value.
    fn find(vm: &mut ScriptVm, args: ScriptObject, what: &str) -> Result<(std::sync::Arc<SongTiming>, usize, usize, f32), ScriptValue> {
        let a = arg_of(vm, args, id!(voice));
        let b = arg_of(vm, args, id!(text));
        // `word(@song, "x")` or, leniently, `word("x")`.
        let (q, offset, nth) = if a.as_id().is_some() {
            (b, arg_of(vm, args, id!(offset)), arg_of(vm, args, id!(nth)))
        } else {
            (a, b, arg_of(vm, args, id!(offset)))
        };
        let offset = offset.as_number().unwrap_or(0.0) as f32;
        let nth = nth.as_number().unwrap_or(0.0).max(0.0) as usize;
        // `word(@song, "x", offset, nth, after)`: from that second of the scene's clock.
        let after = arg_of(vm, args, id!(after)).as_number().map(|a| a as f32);
        let Some(song) = host(vm).and_then(|h| h.song.clone()) else {
            return Err(makepad_widgets::makepad_script::script_err_not_found!(vm.trap(), "`{what}(..)`: this scene has no song timing (render it in an edit with the song and its lyrics, or with --audio)"));
        };
        if let Some(i) = q.as_number() {
            let i = i.max(0.0) as usize;
            if i >= song.words.len() {
                return Err(makepad_widgets::makepad_script::script_err_not_found!(vm.trap(), "`{what}`: the song has {} words, not {}", song.words.len(), i + 1));
            }
            return Ok((song, i, i, offset));
        }
        let text = vm.bx.heap_mut().cast_to_owned_string(q, "word").unwrap_or_default();
        match song.find_after(&text, nth, after) {
            Some((a, z)) => Ok((song, a, z, offset)),
            None => Err(makepad_widgets::makepad_script::script_err_not_found!(vm.trap(), "`{what}`: the song has no \"{}\" (match {})", text, nth)),
        }
    }
    add!(word, script_args!(voice = NIL, text = NIL, offset = NIL, nth = NIL, after = NIL), |vm, args| match find(vm, args, "word") {
        Ok((song, a, _, off)) => ((song.words[a].start + off) as f64).into(),
        Err(e) => e,
    });
    add!(word_end, script_args!(voice = NIL, text = NIL, offset = NIL, nth = NIL, after = NIL), |vm, args| match find(vm, args, "word_end") {
        Ok((song, _, z, off)) => ((song.words[z].end + off) as f64).into(),
        Err(e) => e,
    });
    add!(words, script_args!(voice = NIL, text = NIL, offset = NIL, nth = NIL, after = NIL), |vm, args| match find(vm, args, "words") {
        Ok((song, a, z, off)) => {
            let arr = vm.bx.heap_mut().new_array();
            for w in &song.words[a..=z] {
                let v = NumericValue::vec2(Vec2f { x: w.start + off, y: w.end + off }).to_script_value(vm);
                vm.bx.heap_mut().array_push_unchecked(arr, v);
            }
            arr.into()
        }
        Err(e) => e,
    });
    fn near(vm: &mut ScriptVm, args: ScriptObject, down: bool) -> ScriptValue {
        let t = arg_of(vm, args, id!(t)).as_number().unwrap_or(0.0) as f32;
        match host(vm).and_then(|h| h.song.as_ref()).and_then(|s| s.nearest_beat(t, down)) {
            Some(b) => (b as f64).into(),
            None => (t as f64).into(),
        }
    }
    // Motion's lyric lookups (kit_pure.splash) on the song's words, so a
    // Motion document's `let L = lyric_line(voice(@song), "turn", 0)` means
    // the same in its scenes: the words ({text start end line}) of the
    // `nth` sung line containing `word`; a placeholder without the song.
    fn word_value(vm: &mut ScriptVm, text: &str, start: f32, end: f32, line: f32) -> ScriptValue {
        let o = vm.bx.heap_mut().new_object();
        let t = vm.new_string_with(|_, out| out.push_str(text));
        vm.bx.heap_mut().set_value_def(o, id_lut!(text).into(), t);
        vm.bx.heap_mut().set_value_def(o, id_lut!(start).into(), (start as f64).into());
        vm.bx.heap_mut().set_value_def(o, id_lut!(end).into(), (end as f64).into());
        vm.bx.heap_mut().set_value_def(o, id_lut!(line).into(), (line as f64).into());
        o.into()
    }
    fn nth_word(vm: &mut ScriptVm, args: ScriptObject, key: LiveId) -> (String, Option<(std::sync::Arc<SongTiming>, usize)>) {
        let text = arg_of(vm, args, key);
        let text = vm.bx.heap_mut().cast_to_owned_string(text, "lyric").unwrap_or_default();
        let nth = arg_of(vm, args, id!(nth)).as_number().unwrap_or(0.0).max(0.0) as usize;
        // `after`: the nth at or after that second of the scene's clock.
        let after = arg_of(vm, args, id!(after)).as_number().map(|a| a as f32);
        let found = host(vm).and_then(|h| h.song.clone()).and_then(|song| {
            let i = song.words.iter().enumerate().filter(|(_, w)| w.text == text && after.is_none_or(|a| w.start >= a)).nth(nth).map(|(i, _)| i)?;
            Some((song, i))
        });
        (text, found)
    }
    fn line_words(vm: &mut ScriptVm, song: &SongTiming, line: u32) -> ScriptValue {
        let arr = vm.bx.heap_mut().new_array();
        for w in song.words.iter().filter(|w| w.line == line) {
            let v = word_value(vm, &w.text, w.start, w.end, w.line as f32);
            vm.bx.heap_mut().array_push_unchecked(arr, v);
        }
        arr.into()
    }
    // The scene's own start on its clock, as `here()` is a document's.
    add!(here, script_args!(), |_, _| 0.0f64.into());
    add!(lyric_line_after, script_args!(voice = NIL, t = NIL, nth = NIL), |vm, args| {
        let t = arg_of(vm, args, id!(t)).as_number().unwrap_or(0.0) as f32;
        let nth = arg_of(vm, args, id!(nth)).as_number().unwrap_or(0.0).max(0.0) as usize;
        let song = host(vm).and_then(|h| h.song.clone());
        let line = song.as_ref().and_then(|song| {
            let mut last = None;
            song.words.iter().filter(|w| if last == Some(w.line) { false } else { last = Some(w.line); true }).filter(|w| w.start >= t).nth(nth).map(|w| w.line)
        });
        match (song, line) {
            (Some(song), Some(line)) => line_words(vm, &song, line),
            _ => {
                let arr = vm.bx.heap_mut().new_array();
                let v = word_value(vm, "", t, t + 0.5, -1.0);
                vm.bx.heap_mut().array_push_unchecked(arr, v);
                arr.into()
            }
        }
    });
    add!(lyric_line, script_args!(voice = NIL, word = NIL, nth = NIL, after = NIL), |vm, args| {
        let (text, found) = nth_word(vm, args, id!(word));
        let arr = vm.bx.heap_mut().new_array();
        match found {
            Some((song, i)) => {
                let line = song.words[i].line;
                for w in song.words.iter().filter(|w| w.line == line) {
                    let v = word_value(vm, &w.text, w.start, w.end, w.line as f32);
                    vm.bx.heap_mut().array_push_unchecked(arr, v);
                }
            }
            None => {
                let v = word_value(vm, &text, 0.0, 0.5, -1.0);
                vm.bx.heap_mut().array_push_unchecked(arr, v);
            }
        }
        arr.into()
    });
    add!(lyric_word, script_args!(voice = NIL, text = NIL, nth = NIL, after = NIL), |vm, args| {
        let (text, found) = nth_word(vm, args, id!(text));
        match found {
            Some((song, i)) => {
                let w = &song.words[i];
                word_value(vm, &w.text, w.start, w.end, w.line as f32)
            }
            None => word_value(vm, &text, 0.0, 0.5, -1.0),
        }
    });
    add!(beat_near, script_args!(t = NIL), |vm, args| near(vm, args, false));
    add!(downbeat_near, script_args!(t = NIL), |vm, args| near(vm, args, true));
}

thread_local! {
    /// Frozen arrays flattened once: (heap, array) -> numbers. A frozen
    /// array cannot change, and a freed one comes back with a new generation.
    static FROZEN: std::cell::RefCell<std::collections::HashMap<(usize, ScriptArray), std::sync::Arc<Vec<f32>>>> = Default::default();
}

/// An array's numbers for a kernel input, flattened once if it is frozen
/// (a drawing's data, `strokes_data`), else every call.
fn input_numbers(vm: &mut ScriptVm, v: ScriptValue) -> std::sync::Arc<Vec<f32>> {
    let frozen = v.as_array().filter(|a| vm.bx.heap_ref().array_is_frozen(*a)).map(|a| (vm.bx.heap_ref().heap_key(), a));
    if let Some(key) = frozen {
        if let Some(hit) = FROZEN.with(|f| f.borrow().get(&key).cloned()) {
            return hit;
        }
    }
    let mut flat = Vec::new();
    flatten_numbers(vm, v, &mut flat);
    let flat = std::sync::Arc::new(flat);
    if let Some(key) = frozen {
        FROZEN.with(|f| {
            let mut f = f.borrow_mut();
            if f.len() >= 512 {
                f.clear();
            }
            f.insert(key, flat.clone());
        });
    }
    flat
}

/// An array of numbers or vectors, flattened to f32 (a kernel input the
/// document computes); false when `v` is not an array.
pub fn flatten_numbers(vm: &mut ScriptVm, v: ScriptValue, out: &mut Vec<f32>) -> bool {
    let Some(a) = v.as_array() else { return false };
    let n = vm.bx.heap_ref().array_len(a);
    out.reserve(n * 4);
    for i in 0..n {
        let x = vm.bx.heap_ref().array_index(a, i, NoTrap);
        { let numeric = NumericValue::from_script_value_heap(vm.heap(), x, Default::default()); match numeric.kind() {
            NumericValueTag::F64 => { let n = numeric.as_f64().unwrap(); out.push(n as f32) },
            NumericValueTag::Vec2 => { let p = numeric.as_vec2().unwrap(); out.extend_from_slice(&[p.x, p.y]) },
            NumericValueTag::Vec3 => { let p = numeric.as_vec3().unwrap(); out.extend_from_slice(&[p.x, p.y, p.z]) },
            NumericValueTag::Vec4 => { let p = numeric.as_vec4().unwrap(); out.extend_from_slice(&[p.x, p.y, p.z, p.w]) },
            _ => out.push(0.0),
        } }
    }
    true
}

/// Runs kernel `id` of a document's `kernels` in any VM (a scene's, a 2D
/// Motion document's) with `spec` (its count, params' and document arrays'
/// values; crate::objects::load): a load-time kernel runs now and its
/// outputs are arrays; a per-frame kernel's outputs are handles
/// (`{op: kernel_buf kernel buffer count spec}`) the drawing runs every
/// frame ([`kernel_frame`]). An error carries the kernel's place.
pub fn kernel_run(vm: &mut ScriptVm, kernels: &mut crate::kernels::DocKernels, id: f64, spec: ScriptValue) -> Result<ScriptValue, (Loc, String)> {
    {
        if id < 0.0 || id as usize >= kernels.list.len() {
            return Ok(NIL);
        }
        let id = id as usize;
        // Inputs the document computes: its arrays, flattened to f32.
        let data_names: Vec<(String, usize)> = kernels.list[id].data.clone();
        if !data_names.is_empty() {
            let spec_o = spec.as_object();
            let mut values = Vec::new();
            for (name, _) in &data_names {
                let v = spec_o.map(|so| vm.bx.heap_ref().value(so, LiveId::from_str(name).into(), NoTrap)).unwrap_or(NIL);
                let mut flat: Vec<f32> = Vec::new();
                // A fn(t) is evaluated every frame (see `KernelRun`).
                if v.as_object().is_some_and(|o| vm.bx.heap_ref().is_fn(o)) {
                    continue;
                }
                flatten_numbers(vm, v, &mut flat);
                values.push((name.clone(), std::sync::Arc::new(flat)));
            }
            kernels.list[id].data_values = values;
        }
        let (uses_time, outputs, params, loc) = {
            let k = &kernels.list[id];
            // Physics kernels (spawns, and kernels reading contacts or
            // bodies) run when physics says, never at load.
            (k.per_frame || k.body_out || !k.physics.is_empty(), k.outputs.clone(), k.params.clone(), k.loc)
        };
        let spec_o = spec.as_object();
        // Per frame when it reads `time` or a param is keyed (not a number).
        let keyed = |p: &str| spec_o.map(|so| vm.bx.heap_ref().value(so, LiveId::from_str(p).into(), NoTrap)).is_some_and(|v| v.as_number().is_none());
        // (`active_count: fn(t)` too: how many elements run each frame.)
        let active_keyed = spec_o.map(|so| vm.bx.heap_ref().value(so, id!(active_count).into(), NoTrap)).and_then(|v| v.as_object()).is_some_and(|o| vm.bx.heap_ref().is_fn(o));
        let data_keyed = data_names.iter().any(|(n, _)| spec_o.map(|so| vm.bx.heap_ref().value(so, LiveId::from_str(n).into(), NoTrap)).and_then(|v| v.as_object()).is_some_and(|o| vm.bx.heap_ref().is_fn(o)));
        let per_frame = uses_time || active_keyed || data_keyed || params.iter().any(|p| keyed(p));
        let given = spec_o.map(|o| vm.bx.heap_ref().value(o, id!(count).into(), NoTrap)).and_then(|v| v.as_number());
        let count = given.unwrap_or(1.0).max(0.0) as usize;
        let o = vm.bx.heap_mut().new_object();
        if per_frame {
            for out in &outputs {
                let t = vm.bx.heap_mut().new_object();
                vm.bx.heap_mut().set_value_def(t, id!(__m3).into(), LiveId::from_str_with_lut("__anim").unwrap_or(LiveId::from_str("__anim")).into());
                vm.bx.heap_mut().set_value_def(t, id!(op).into(), LiveId::from_str_with_lut("kernel_buf").unwrap_or(LiveId::from_str("kernel_buf")).into());
                vm.bx.heap_mut().set_value_def(t, id!(kernel).into(), (id as f64).into());
                vm.bx.heap_mut().set_value_def(t, id!(count).into(), given.map(|n| n.max(0.0).into()).unwrap_or(NIL));
                let name = vm.new_string_with(|_, s| s.push_str(&out.name));
                vm.bx.heap_mut().set_value_def(t, id!(buffer).into(), name);
                vm.bx.heap_mut().set_value_def(t, id!(spec).into(), spec);
                vm.bx.heap_mut().set_value_def(o, LiveId::from_str(&out.name).into(), t.into());
            }
            return Ok(o.into());
        }
        let mut values = Vec::new();
        for p in &params {
            let v = spec_o.map(|so| vm.bx.heap_ref().value(so, LiveId::from_str(p).into(), NoTrap)).and_then(|v| v.as_number()).unwrap_or(0.0);
            values.push((p.clone(), v as f32));
        }
        let result = kernels.run(id, count, &values, 0.0).map_err(|e| (loc, e))?;
        for out in &outputs {
            let data = result[&out.name].clone();
            let arr = vm.bx.heap_mut().new_array();
            match out.width {
                1 => {
                    for x in data.iter() {
                        vm.bx.heap_mut().array_push_unchecked(arr, (*x as f64).into());
                    }
                }
                2..=4 => {
                    for c in data.chunks_exact(out.width) {
                        let v = match out.width {
                            2 => NumericValue::vec2(Vec2f { x: c[0], y: c[1] }),
                            3 => NumericValue::vec3(Vec3f { x: c[0], y: c[1], z: c[2] }),
                            _ => NumericValue::vec4(Vec4f { x: c[0], y: c[1], z: c[2], w: c[3] }),
                        };
                        let sv = v.to_script_value(vm);
                        vm.bx.heap_mut().array_push_unchecked(arr, sv);
                    }
                }
                _ => {
                    // Records (Sprite, mat4): flat numbers.
                    for x in data.iter() {
                        vm.bx.heap_mut().array_push_unchecked(arr, (*x as f64).into());
                    }
                }
            }
            vm.bx.heap_mut().set_value_def(o, LiveId::from_str(&out.name).into(), arr.into());
        }
        kernels.list[id].results = result;
        Ok(o.into())
    }
}

/// A per-frame kernel's drawn buffer at time `t`, for a 2D document's
/// `c.segs(k.out)`: `handle` is what [`kernel_run`] gave for the output. Its
/// params and per-frame inputs are the spec's numbers, or its `fn(t)` called
/// now; `active_count: fn(t)` runs only that many elements.
///
/// `with` (a draw's `{with: {…}}`) gives inputs, params and `count` in place
/// of the spec's: a library module's kernel drawn with the caller's data.
pub fn kernel_frame(vm: &mut ScriptVm, kernels: &crate::kernels::DocKernels, handle: ScriptObject, t: f64, with: Option<ScriptObject>) -> Result<std::sync::Arc<Vec<f32>>, String> {
    let id = vm.bx.heap_ref().value(handle, id!(kernel).into(), NoTrap).as_number().ok_or("not a kernel's output")? as usize;
    let dk = kernels.list.get(id).ok_or("the kernel is not this document's")?;
    let spec = vm.bx.heap_ref().value(handle, id!(spec).into(), NoTrap).as_object();
    let mut buffer = String::new();
    vm.bx.heap_ref().string_with(vm.bx.heap_ref().value(handle, id!(buffer).into(), NoTrap), |_, b| buffer = b.to_string());
    let field = |vm: &mut ScriptVm, name: &str| {
        let given = with.map(|w| vm.bx.heap_ref().value(w, LiveId::from_str(name).into(), NoTrap)).filter(|v| !v.is_nil() && !v.is_err());
        given.unwrap_or_else(|| spec.map(|so| vm.bx.heap_ref().value(so, LiveId::from_str(name).into(), NoTrap)).unwrap_or(NIL))
    };
    let at = |vm: &mut ScriptVm, v: ScriptValue| if v.as_object().is_some_and(|o| vm.bx.heap_ref().is_fn(o)) { vm.call(v, &[t.into()]) } else { v };
    let mut params = Vec::new();
    for p in &dk.params {
        let v = field(vm, p);
        let v = at(vm, v);
        params.push((p.clone(), v.as_number().unwrap_or(0.0) as f32));
    }
    let given = with.map(|w| vm.bx.heap_ref().value(w, id!(count).into(), NoTrap)).and_then(|v| v.as_number());
    let mut count = given.or_else(|| vm.bx.heap_ref().value(handle, id!(count).into(), NoTrap).as_number()).unwrap_or(1.0).max(0.0) as usize;
    let active = field(vm, "active_count");
    if !active.is_nil() {
        let a = at(vm, active);
        count = count.min(a.as_number().unwrap_or(count as f64).max(0.0) as usize);
    }
    let mut inputs: Vec<(String, std::sync::Arc<Vec<f32>>)> = Vec::new();
    for (name, ki, buf) in &dk.inputs {
        let data = kernels.list[*ki].results.get(buf).cloned().ok_or_else(|| format!("kernel `{}`: its input `{name}` has not run", dk.name))?;
        inputs.push((name.clone(), data));
    }
    for (name, _) in &dk.data {
        let v = field(vm, name);
        let given = with.is_some_and(|w| { let g = vm.bx.heap_ref().value(w, LiveId::from_str(name).into(), NoTrap); !g.is_nil() && !g.is_err() });
        if v.as_object().is_some_and(|o| vm.bx.heap_ref().is_fn(o)) || given {
            // Per frame (`fn(t)`), or the draw's own array.
            let a = if v.as_object().is_some_and(|o| vm.bx.heap_ref().is_fn(o)) { vm.call(v, &[t.into()]) } else { v };
            inputs.push((name.clone(), input_numbers(vm, a)));
        } else if let Some((_, data)) = dk.data_values.iter().find(|(n, _)| n == name) {
            inputs.push((name.clone(), data.clone()));
        }
    }
    if count == 0 {
        return Ok(std::sync::Arc::new(Vec::new()));
    }
    let out = crate::kernels::run_kernel(dk, count, &params, t as f32, &inputs)?;
    out.get(&buffer).cloned().ok_or_else(|| format!("kernel `{}` has no output `{buffer}`", dk.name))
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// A media reference: a library revision or a file (relative to the
/// document's directory).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MediaRef {
    Asset { id: String, rev: String },
    File(String),
}

/// Whether a value is a colour (sRGB literals become linear) or an angle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Semantic {
    Plain,
    Color,
}

/// Reads evaluated values into typed data, collecting diagnostics.
pub struct Reader<'a, 'v> {
    pub vm: &'a mut ScriptVm<'v>,
    pub diagnostics: Vec<Diagnostic>,
    /// Closures kept alive for per-frame evaluation.
    pub closures: Vec<ScriptObjectRef>,
    pub line_offset: u32,
    /// Tempo for `beat(n)` / `bar(n)` times (the scene's `bpm`).
    pub bpm: f64,
    pub beats_per_bar: f64,
    /// The file the document is written in, for edit mode's live shader
    /// literals (see `LoadOptions::origin_file`), and the lines of that file
    /// before the document's first.
    pub origin_file: Option<(String, u32)>,
    /// Columns before the document's text on its VM's first line (a Motion
    /// document's `use` prefix): taken off locations there.
    pub col_shift: u32,
    /// The tempo of the document the scene is in, when the scene sets none.
    pub default_bpm: Option<f64>,
    /// Fields the host reads (a layer's place and time), known to the scene.
    pub host_fields: &'static [&'static str],
}

/// A typed read of one field the scene reader made, as [`recording`]
/// notes it.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldRead {
    pub name: String,
    /// What it takes: `number`, `whole number`, `vec2`, `vec3`, `colour`,
    /// `bool`, `words`, `@name`, `media` or `fn`.
    pub ty: &'static str,
    /// What it reads as when not written, as Splash.
    pub default: Option<String>,
    /// The range the reader keeps a number in.
    pub range: Option<[f64; 2]>,
}

/// One type the scene reader checks: its name as `check_fields` gives it,
/// and its fields, each with the first typed read of it (none: read its
/// own way).
#[derive(Clone, Debug, PartialEq)]
pub struct SceneType {
    pub name: String,
    pub fields: Vec<(String, Option<FieldRead>)>,
}

/// What loads read while [`recording`]: every object `check_fields`
/// checked (with its type and fields) and every typed read of a field.
#[derive(Clone, Debug, Default)]
pub struct SceneReads {
    checked: Vec<(ScriptObject, String, Vec<String>)>,
    reads: Vec<(ScriptObject, FieldRead)>,
}

impl SceneReads {
    /// The types checked, each once, in the order first checked: an
    /// object's typed reads count for the type it was checked as.
    pub fn types(&self) -> Vec<SceneType> {
        let mut types: Vec<SceneType> = Vec::new();
        for (obj, name, known) in &self.checked {
            let at = match types.iter().position(|t| &t.name == name) {
                Some(at) => at,
                None => {
                    types.push(SceneType { name: name.clone(), fields: Vec::new() });
                    types.len() - 1
                }
            };
            for field in known {
                if !types[at].fields.iter().any(|(f, _)| f == field) {
                    types[at].fields.push((field.clone(), None));
                }
            }
            for (_, read) in self.reads.iter().filter(|(o, _)| o == obj) {
                if let Some((_, slot)) = types[at].fields.iter_mut().find(|(f, _)| *f == read.name) {
                    match slot {
                        None => *slot = Some(read.clone()),
                        Some(first) if first.range.is_none() => first.range = read.range,
                        Some(_) => {}
                    }
                }
            }
        }
        types
    }
}

thread_local! {
    static READS: std::cell::RefCell<Option<SceneReads>> = const { std::cell::RefCell::new(None) };
}

/// Run `load` (scene loads on this thread), noting what the reader checks
/// and reads: a registry of the fields it takes is the reader's own.
pub fn recording<R>(load: impl FnOnce() -> R) -> (R, SceneReads) {
    READS.with(|reads| *reads.borrow_mut() = Some(SceneReads::default()));
    let out = load();
    (out, READS.with(|reads| reads.borrow_mut().take().unwrap_or_default()))
}

fn note(f: impl FnOnce(&mut SceneReads)) {
    READS.with(|reads| {
        if let Some(reads) = reads.borrow_mut().as_mut() {
            f(reads);
        }
    });
}

fn note_read(obj: ScriptObject, name: &str, ty: &'static str, default: impl FnOnce() -> Option<String>, range: Option<[f64; 2]>) {
    note(|reads| reads.reads.push((obj, FieldRead { name: name.to_string(), ty, default: default(), range })));
}

/// A number as Splash: `1`, `0.25`.
fn splash_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{}", v as f32) }
}

/// `N` floats as Splash: a number, `vec2(..)`, `vec3(..)`, or (a colour,
/// linear here) `#rrggbb` as written.
fn splash_floats<const N: usize>(v: [f32; N], semantic: Semantic) -> String {
    let byte = |c: f32| (makepad_tween::linear_to_srgb(c.clamp(0.0, 1.0) as f64) * 255.0).round() as u32;
    match v.as_slice() {
        [x] => splash_num(*x as f64),
        [r, g, b] | [r, g, b, _] if semantic == Semantic::Color => {
            let alpha = v.as_slice().get(3).copied().unwrap_or(1.0);
            let rgb = byte(*r) << 16 | byte(*g) << 8 | byte(*b);
            if alpha >= 1.0 { format!("#{rgb:06x}") } else { format!("#{rgb:06x}{:02x}", (alpha.clamp(0.0, 1.0) * 255.0).round() as u32) }
        }
        [x, y] => format!("vec2({}, {})", splash_num(*x as f64), splash_num(*y as f64)),
        [x, y, z] => format!("vec3({}, {}, {})", splash_num(*x as f64), splash_num(*y as f64), splash_num(*z as f64)),
        many => format!("vec{}({})", many.len(), many.iter().map(|c| splash_num(*c as f64)).collect::<Vec<_>>().join(", ")),
    }
}

/// How field `field` of scene type `ty` is written, where the reader's
/// hints for other tools' words say it (`write `intensity` here: …`).
pub fn field_hint(ty: &str, field: &str) -> Option<&'static str> {
    crate::foreign::how(ty, field)
}

/// sRGB (0..1) to linear.
pub fn srgb_to_linear(c: f32) -> f32 {
    makepad_tween::srgb_to_linear(c as f64) as f32
}

impl<'a, 'v> Reader<'a, 'v> {
    pub fn new(vm: &'a mut ScriptVm<'v>, line_offset: u32) -> Self {
        Self { vm, diagnostics: Vec::new(), closures: Vec::new(), line_offset, bpm: 120.0, beats_per_bar: 4.0, origin_file: None, col_shift: 0, default_bpm: None, host_fields: &[] }
    }

    pub fn loc(&self, obj: ScriptObject) -> Loc {
        self.ip_loc(self.vm.bx.heap_ref().made_at(obj)).unwrap_or_default()
    }

    /// Where code is, in the document's lines and columns.
    pub fn ip_loc(&self, ip: ScriptIp) -> Option<Loc> {
        match self.vm.bx.code_ref().ip_to_loc(ip) {
            Some(l) if l.line() < 1_000_000 => Some(self.doc_loc(l.line(), l.col())),
            _ => None,
        }
    }

    fn doc_loc(&self, line: u32, col: u32) -> Loc {
        let col = if line == 0 { col.saturating_sub(self.col_shift) } else { col };
        Loc { line: (line + 1).saturating_sub(self.line_offset).max(1), col: col + 1 }
    }

    /// Where the value of `obj`'s field `name` is written: where `value`
    /// was made, unless a library made it (a form's constructor, `tween(..)`
    /// in `mod.forms`), then where the field is written.
    pub fn value_loc(&self, value: ScriptObject, obj: ScriptObject, name: &str) -> Loc {
        let made = self.vm.bx.heap_ref().made_at(value);
        let in_library = self.vm.bx.code_ref().ip_to_loc(made).is_some_and(|l| l.file().starts_with("mod.") || l.file().starts_with("graph://") || l.file() == "motion3d://kits");
        if in_library {
            let key = LiveId::from_str_with_lut(name).unwrap_or_else(|_| LiveId::from_str(name));
            if let Some(l) = self.vm.bx.code_ref().field_loc(self.vm.bx.heap_ref().made_at(obj), key) {
                if l.line() < 1_000_000 {
                    return self.doc_loc(l.line(), l.col());
                }
            }
            return self.loc(obj);
        }
        self.loc(value)
    }

    /// Where a script function's body is (for reading its text back).
    pub fn fn_loc(&self, f: ScriptObject) -> Option<Loc> {
        let ip = self.vm.bx.heap_ref().as_fn(f)?.script_ip()?;
        self.ip_loc(ip)
    }

    pub fn error(&mut self, loc: Loc, path: &str, message: impl Into<String>, hint: Option<String>) {
        let mut d = Diagnostic::error(loc, path, message);
        d.hint = hint;
        self.diagnostics.push(d);
    }

    pub fn warn(&mut self, loc: Loc, path: &str, message: impl Into<String>, hint: Option<String>) {
        let mut d = Diagnostic::warning(loc, path, message);
        d.hint = hint;
        self.diagnostics.push(d);
    }

    /// The `__m3` kind of an object made from one of [`TYPES`].
    pub fn kind(&self, v: ScriptValue) -> Option<String> {
        let o = v.as_object()?;
        let k = self.vm.bx.heap_ref().value(o, id!(__m3).into(), NoTrap);
        k.as_id().map(|id| id.to_string())
    }

    pub fn is_anim(&self, v: ScriptValue) -> Option<String> {
        let o = v.as_object()?;
        if self.kind(v).as_deref() != Some(ANIM_KIND) {
            return form_op_of(self.vm.heap(), o);
        }
        self.vm.bx.heap_ref().value(o, id!(op).into(), NoTrap).as_id().map(|id| id.to_string())
    }

    /// A field, NIL when absent.
    /// A field's value; a loaded kernel's output (`k.out`) reads as what
    /// it gave (crate::objects::resolve).
    pub fn get(&self, obj: ScriptObject, name: &str) -> ScriptValue {
        let v = self.vm.bx.heap_ref().value(obj, LiveId::from_str(name).into(), NoTrap);
        if v.is_err() {
            return NIL;
        }
        if host(self.vm).is_some_and(|h| h.adopted) && self.is_host_default(obj, LiveId::from_str(name)) {
            return NIL;
        }
        match host(self.vm) {
            Some(h) if !h.kernels.objects.is_empty() && v.as_object().is_some() => crate::objects::resolve(&h.kernels, v),
            _ => v,
        }
    }

    /// Whether the value of `key` on `obj` is written by a prototype of
    /// Motion's own (a registered type's default), not by the document's
    /// objects.
    fn is_host_default(&self, obj: ScriptObject, key: LiveId) -> bool {
        let h = self.vm.heap();
        let key: ScriptValue = key.into();
        let mut at = Some(obj);
        let mut depth = 0;
        while let Some(level) = at {
            let own = h.map_entries(level).iter().any(|e| e.key == key) || (0..h.vec_len(level)).any(|i| h.vec_key_value(level, i, NoTrap).key == key);
            if own {
                return h.map_entries(level).iter().any(|e| e.key == ScriptValue::from(id!(__host)));
            }
            depth += 1;
            if depth > 64 {
                return false;
            }
            at = h.proto(level).as_object();
        }
        false
    }

    pub fn has(&self, obj: ScriptObject, name: &str) -> bool {
        !self.get(obj, name).is_nil()
    }

    /// The object's own field names (not its prototype's).
    pub fn own_fields(&self, obj: ScriptObject) -> Vec<String> {
        let h = self.vm.heap();
        let mut out = Vec::new();
        h.map_entries(obj).iter().for_each(|e| {
            if let Some(id) = e.key.as_id() {
                out.push(id.to_string());
            }
        });
        for i in 0..h.vec_len(obj) {
            let e = h.vec_key_value(obj, i, NoTrap);
            if let Some(id) = e.key.as_id() {
                out.push(id.to_string());
            }
        }
        out
    }

    /// Warn about every field that is not in `known`, naming the closest
    /// real one (and the snake_case spelling of a camelCase name), or the
    /// field another tool's word for it means here.
    pub fn check_fields(&mut self, obj: ScriptObject, path: &str, type_name: &str, known: &[&str]) {
        note(|reads| reads.checked.push((obj, type_name.to_string(), known.iter().map(|f| f.to_string()).collect())));
        for field in self.own_fields(obj) {
            if field.starts_with("__") || known.contains(&field.as_str()) {
                continue;
            }
            let loc = self.loc(obj);
            if let Some(f) = crate::foreign::lookup(&field, type_name).filter(|f| known.contains(&f.field)) {
                let message = format!("{type_name} has no parameter `{field}` (ignored); write `{}` here: {}", f.field, f.how);
                self.warn(loc, path, message, Some(format!("{type_name} takes: {}", known.join(", "))));
                continue;
            }
            let snake = to_snake(&field);
            let hint = if snake != field && known.contains(&snake.as_str()) {
                Some(format!("parameters are snake_case: write `{snake}`"))
            } else {
                anim::closest(&field, known.iter().copied()).map(|c| format!("did you mean `{c}`?"))
            };
            self.warn(
                loc,
                path,
                format!("{type_name} has no parameter `{field}` (ignored)"),
                hint.or_else(|| Some(format!("{type_name} takes: {}", known.join(", ")))),
            );
        }
    }

    /// A number; `beat(n, offset)` and `bar(n, beat, offset)` resolve to
    /// seconds at the scene's tempo.
    pub fn number(&self, v: ScriptValue) -> Option<f64> {
        if let Some(b) = v.as_bool() {
            return Some(if b { 1.0 } else { 0.0 });
        }
        if let Some(op) = self.is_anim(v) {
            let o = v.as_object()?;
            let g = |k: &str| self.get(o, k).as_number().unwrap_or(0.0);
            let spb = 60.0 / self.bpm.max(1.0);
            return match op.as_str() {
                "beat" => Some(g("n") * spb + g("offset")),
                "bar" => Some((g("n") * self.beats_per_bar + g("beat")) * spb + g("offset")),
                _ => None,
            };
        }
        v.as_number()
    }

    pub fn f32(&mut self, obj: ScriptObject, path: &str, name: &str, default: f32) -> f32 {
        note_read(obj, name, "number", || Some(splash_num(default as f64)), None);
        let v = self.get(obj, name);
        if v.is_nil() {
            return default;
        }
        match self.number(v) {
            Some(n) if n.is_finite() => n as f32,
            _ => {
                let loc = self.loc(obj);
                self.error(loc, &format!("{path}.{name}"), format!("`{name}` must be a number here, not {}", self.describe(v)), Some(format!("write `{name}: {default}`")));
                default
            }
        }
    }

    pub fn u32(&mut self, obj: ScriptObject, path: &str, name: &str, default: u32, max: u32) -> u32 {
        note_read(obj, name, "whole number", || Some(default.to_string()), Some([0.0, max as f64]));
        let v = self.f32(obj, path, name, default as f32);
        if v.round() > max as f32 {
            let loc = self.loc(obj);
            self.warn(loc, &format!("{path}.{name}"), format!("`{name}: {v}` is above the limit {max}; clamped"), None);
        }
        v.round().clamp(0.0, max as f32) as u32
    }

    pub fn bool(&mut self, obj: ScriptObject, name: &str, default: bool) -> bool {
        note_read(obj, name, "bool", || Some(default.to_string()), None);
        let v = self.get(obj, name);
        if let Some(b) = v.as_bool() {
            return b;
        }
        self.number(v).map(|n| n != 0.0).unwrap_or(default)
    }

    pub fn string(&mut self, obj: ScriptObject, name: &str) -> Option<String> {
        note_read(obj, name, "words", || None, None);
        let v = self.get(obj, name);
        self.string_of(v)
    }

    pub fn string_of(&mut self, v: ScriptValue) -> Option<String> {
        if v.is_nil() {
            return None;
        }
        if let Some(id) = v.as_id() {
            return Some(id.to_string());
        }
        if v.is_string_like() {
            return self.vm.bx.heap_mut().cast_to_owned_string(v, "motion3d string");
        }
        None
    }

    /// An `@id` (or a string), lower-case.
    pub fn id(&mut self, obj: ScriptObject, name: &str) -> Option<String> {
        note_read(obj, name, "@name", || None, None);
        let v = self.get(obj, name);
        if let Some(id) = v.as_id() {
            return Some(id.to_string());
        }
        self.string_of(v)
    }

    /// Items of an array (or an object's vec part).
    pub fn list(&self, v: ScriptValue) -> Vec<ScriptValue> {
        let h = self.vm.heap();
        if let Some(a) = v.as_array() {
            return (0..h.array_len(a)).map(|i| h.array_index(a, i, NoTrap)).collect();
        }
        if let Some(o) = v.as_object() {
            if self.kind(v).is_none() {
                return (0..h.vec_len(o)).map(|i| h.vec_value(o, i, NoTrap)).collect();
            }
        }
        Vec::new()
    }

    pub fn describe(&self, v: ScriptValue) -> String {
        if v.is_nil() {
            return "nothing".into();
        }
        if let Some(n) = v.as_number() {
            return format!("the number {n}");
        }
        if v.as_color().is_some() {
            return "a colour".into();
        }
        if let Some(id) = v.as_id() {
            return format!("@{id}");
        }
        if v.is_string_like() {
            return "a string".into();
        }
        if let Some(k) = self.is_anim(v) {
            return format!("{k}(...)");
        }
        if let Some(k) = self.kind(v) {
            return format!("a {} object", type_name_of(&k));
        }
        if v.as_array().is_some() {
            return "a list".into();
        }
        format!("{:?}", v.value_type())
    }

    /// A plain value of N floats: a number (broadcast), vec2/3/4, colour.
    pub fn plain<const N: usize>(&self, v: ScriptValue, semantic: Semantic) -> Option<[f32; N]> {
        if let Some(b) = v.as_bool() {
            return Some([if b { 1.0 } else { 0.0 }; N]);
        }
        if let Some(n) = v.as_number() {
            return Some([n as f32; N]);
        }
        // `in(@p)` of a vec3 input.
        if self.is_anim(v).as_deref() == Some("vec3") {
            let o = v.as_object()?;
            let g = |k| self.number(self.get(o, k)).unwrap_or(0.0) as f32;
            return fit([g("x"), g("y"), g("z"), 1.0], 3);
        }
        // `[x, y, w, h]`: an array of numbers (a reframe's crop keys).
        if v.as_array().is_some() {
            let items = self.list(v);
            if items.is_empty() || items.len() > 4 {
                return None;
            }
            let mut out = [0.0f32, 0.0, 0.0, 1.0];
            for (i, it) in items.iter().enumerate() {
                out[i] = self.number(*it)? as f32;
            }
            return fit(out, items.len());
        }
        if v.as_color().is_none() && v.as_pod().is_none() && v.as_f32().is_none() && v.as_u32().is_none() && v.as_i32().is_none() {
            return None;
        }
        { let numeric = NumericValue::from_script_value_heap(self.vm.heap(), v, Default::default()); match numeric.kind() {
            NumericValueTag::F64 => { let n = numeric.as_f64().unwrap(); Some([n as f32; N]) },
            NumericValueTag::Vec2 => { let p = numeric.as_vec2().unwrap(); fit([p.x, p.y, 0.0, 1.0], 2) },
            NumericValueTag::Vec3 => { let p = numeric.as_vec3().unwrap(); fit([p.x, p.y, p.z, 1.0], 3) },
            NumericValueTag::Vec4 => { let p = numeric.as_vec4().unwrap(); fit([p.x, p.y, p.z, p.w], 4) },
            NumericValueTag::Color => { let c = numeric.as_color().unwrap();
                let c = if semantic == Semantic::Color {
                    [srgb_to_linear(c.x), srgb_to_linear(c.y), srgb_to_linear(c.z), c.w]
                } else {
                    [c.x, c.y, c.z, c.w]
                };
                fit(c, 4)
            }
            _ => None,
        } }
    }

    /// Any keyable value of N floats. Errors say what was expected.
    pub fn anim<const N: usize>(&mut self, obj: ScriptObject, path: &str, name: &str, default: [f32; N], semantic: Semantic) -> Anim<N> {
        let ty = match (N, semantic) {
            (3 | 4, Semantic::Color) => "colour",
            (1, _) => "number",
            (2, _) => "vec2",
            (3, _) => "vec3",
            _ => "numbers",
        };
        note_read(obj, name, ty, || Some(splash_floats(default, semantic)), None);
        let v = self.get(obj, name);
        if v.is_nil() {
            return Anim::Const(default);
        }
        let full = format!("{path}.{name}");
        match self.anim_of::<N>(v, &full, semantic) {
            Ok(a) => a,
            Err(message) => {
                let loc = v.as_object().map(|o| self.value_loc(o, obj, name)).unwrap_or_else(|| self.loc(obj));
                self.error(loc, &full, message, Some(expected_syntax::<N>(name, semantic)));
                Anim::Const(default)
            }
        }
    }

    pub fn anim_of<const N: usize>(&mut self, v: ScriptValue, path: &str, semantic: Semantic) -> Result<Anim<N>, String> {
        if let Some(p) = self.plain::<N>(v, semantic) {
            // Splash has no arithmetic on animations: `tween(..) + vec3(..)`
            // evaluates to NaN, which would draw nothing (or fly a body to
            // nowhere) without a word.
            // What the VM could not compute (NaN): warn where it was made,
            // and read 0 (a document that wrote it still loads).
            if p.iter().any(|x| x.is_nan()) {
                let loc = v.as_f64_traced_nan().and_then(|ip| self.ip_loc(ip)).unwrap_or_default();
                self.warn(
                    loc,
                    path,
                    "this value is not a number (NaN), read as 0",
                    Some("arithmetic on an animation works for + - * / with numbers and vectors (`tween(..) + vec3(..)`); functions of one (`sin(tween(..))`, `-tween(..)`) are not evaluated: write `fn(t) { .. }` or `mul(tween(..), -1)`".into()),
                );
                return Ok(Anim::Const(p.map(|x| if x.is_nan() { 0.0 } else { x })));
            }
            return Ok(Anim::Const(p));
        }
        if matches!(self.is_anim(v).as_deref(), Some("beat") | Some("bar")) {
            let n = self.number(v).unwrap_or(0.0) as f32;
            return Ok(Anim::Const([n; N]));
        }
        if let Some(o) = v.as_object() {
            if self.vm.bx.heap_ref().is_fn(o) {
                let index = self.closures.len() as u32;
                let r = self.vm.bx.heap_mut().new_object_ref(o);
                self.closures.push(r);
                return Ok(Anim::Script(index));
            }
        }
        let Some(op) = self.is_anim(v) else {
            return Err(format!("expected {}, got {}", expected_what::<N>(semantic), self.describe(v)));
        };
        let o = v.as_object().unwrap();
        let field = |r: &Self, k: &str| r.get(o, k);
        match op.as_str() {
            "tween" => {
                let from = self.plain::<N>(field(self, "from"), semantic).ok_or_else(|| format!("tween's `from` must be {}", expected_what::<N>(semantic)))?;
                let to = self.plain::<N>(field(self, "to"), semantic).ok_or_else(|| format!("tween's `to` must be {}", expected_what::<N>(semantic)))?;
                let start = self.number(field(self, "start")).ok_or("tween(from, to, start, end, ease) needs a start time in seconds")? as f32;
                let end = self.number(field(self, "end")).ok_or("tween(from, to, start, end, ease) needs an end time in seconds")? as f32;
                if end < start {
                    return Err(format!("tween ends ({end} s) before it starts ({start} s)"));
                }
                let ease = self.ease(field(self, "ease"), path)?;
                Ok(Anim::tween(from, to, start, end, ease))
            }
            "keys" => {
                let mut keys: Vec<Key<N>> = Vec::new();
                // `keys([[0, a], [1, b, @ease], …])`: one list of the keys
                // (made in a loop), or each key an argument. Motion's
                // `Keys{points}` holds the arguments as a list; a scene's
                // own `keys(..)` has them as `k0`, `k1`, ….
                let points = field(self, "points");
                let given: Vec<ScriptValue> = if !points.is_nil() {
                    let items = self.list(points);
                    let listed = items.len() == 1 && self.list(items[0]).first().is_some_and(|k| k.as_array().is_some());
                    if listed { self.list(items[0]) } else { items }
                } else {
                    let first = field(self, "k0");
                    let listed = field(self, "k1").is_nil() && self.list(first).first().is_some_and(|k| k.as_array().is_some());
                    if listed { self.list(first) } else { (0..64).map(|i| field(self, &format!("k{i}"))).collect() }
                };
                for (i, k) in given.into_iter().enumerate() {
                    if k.is_nil() {
                        continue;
                    }
                    let items = self.list(k);
                    if items.len() < 2 {
                        return Err(format!("key {i} must be [time, value] or [time, value, @ease]"));
                    }
                    let t = self.number(items[0]).ok_or_else(|| format!("key {i}: the time must be a number of seconds"))? as f32;
                    let v = self.plain::<N>(items[1], semantic).ok_or_else(|| format!("key {i}: the value must be {}", expected_what::<N>(semantic)))?;
                    // `@pin` (a reframe's hand-moved key) is for the solver, not the curve.
                    let pin = LiveId::from_str_with_lut("pin").unwrap_or(LiveId::from_str("pin"));
                    let ease = match items.get(2) {
                        Some(e) if e.as_id() != Some(pin) => self.ease(*e, path)?,
                        _ => Ease::Linear,
                    };
                    keys.push(Key { t, v, ease });
                }
                if keys.is_empty() {
                    return Err("keys(...) needs at least one [time, value]".into());
                }
                keys.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
                Ok(Anim::Keys(keys))
            }
            "repeat" | "pingpong" => {
                // Motion's `Loop{inner period mode count}` is `repeat(anim,
                // period, @restart | @pingpong)`; a scene's own `pingpong(..)`
                // and `repeat(..)` hold `anim`.
                let looped = field(self, "inner");
                let inner_v = if looped.is_nil() { field(self, "anim") } else { looped };
                let pingpong = op == "pingpong" || field(self, "mode").as_id().is_some_and(|m| m.to_string() == "pingpong");
                if !field(self, "count").is_nil() {
                    return Err("repeat(anim, period, mode, count): a scene's loop runs for ever (leave `count` out)".into());
                }
                let op = if pingpong { "pingpong" } else { "repeat" };
                let inner = self.anim_of::<N>(inner_v, path, semantic)?;
                let period = match self.number(field(self, "period")) {
                    Some(p) if p > 0.0 => p as f32,
                    _ => inner.end_time().filter(|e| *e > 0.0).ok_or(format!("{op}(anim, period): give the period in seconds"))?,
                };
                Ok(Anim::Loop { inner: Box::new(inner), period, mode: if op == "repeat" { LoopMode::Repeat } else { LoopMode::PingPong } })
            }
            "delay" => {
                let inner = self.anim_of::<N>(field(self, "anim"), path, semantic)?;
                let seconds = self.number(field(self, "seconds")).unwrap_or(0.0) as f32;
                Ok(Anim::Delay { inner: Box::new(inner), seconds })
            }
            "wiggle" => {
                let base = self.anim_of::<N>(field(self, "base"), path, semantic)?;
                let amount = self.plain::<N>(field(self, "amount"), Semantic::Plain).ok_or("wiggle(base, amount, frequency, seed): amount is a number or vector")?;
                let frequency = self.number(field(self, "frequency")).unwrap_or(1.0) as f32;
                let seed = self.number(field(self, "seed")).unwrap_or(1.0) as u64;
                Ok(Anim::Wiggle { base: Box::new(base), amount, frequency, seed })
            }
            "spin" => {
                // spin(rate) or spin(base, rate).
                let (base, rate) = if field(self, "rate").is_nil() {
                    (Anim::Const([0.0; N]), field(self, "base"))
                } else {
                    (self.anim_of::<N>(field(self, "base"), path, semantic)?, field(self, "rate"))
                };
                let rate = self.plain::<N>(rate, Semantic::Plain).ok_or("spin(rate) / spin(base, rate): the rate is units per second (degrees for rotations)")?;
                Ok(Anim::Spin { base: Box::new(base), rate })
            }
            "add" | "mul" => {
                let a = self.anim_of::<N>(field(self, "a"), path, semantic)?;
                let b = self.anim_of::<N>(field(self, "b"), path, semantic)?;
                Ok(if op == "add" { Anim::Add(Box::new(a), Box::new(b)) } else { Anim::Mul(Box::new(a), Box::new(b)) })
            }
            "music" | "voice" => {
                let fv = field(self, "feature");
                let name = fv.as_id().map(|i| i.to_string()).or_else(|| self.string_of(fv)).unwrap_or_else(|| if op == "voice" { "voice".into() } else { "energy".into() });
                if name == "none" {
                    // `follow_music(@none, lo, hi)`: no binding (kits' default).
                    return Ok(Anim::Const(self.plain::<N>(field(self, "lo"), semantic).unwrap_or([0.0; N])));
                }
                let feature = AudioFeature::from_id(&name).ok_or_else(|| {
                    format!(
                        "unknown audio feature `@{name}`{}; features: {}",
                        anim::closest(&name, AudioFeature::NAMES.iter().copied()).map(|c| format!(" (did you mean `@{c}`?)")).unwrap_or_default(),
                        AudioFeature::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" ")
                    )
                })?;
                let lo = self.plain::<N>(field(self, "lo"), semantic).unwrap_or([0.0; N]);
                let hi = self.plain::<N>(field(self, "hi"), semantic).unwrap_or([1.0; N]);
                let smooth = self.number(field(self, "smooth")).unwrap_or(0.0) as f32;
                let sv = field(self, "source");
                let source = sv.as_id().map(|i| i.to_string()).or_else(|| self.string_of(sv));
                Ok(Anim::Audio { source, feature, lo, hi, smooth })
            }
            other => Err(format!("{other}(...) is not a value here")),
        }
    }

    /// `@ease_out_back`, `spring(k, d)`, `bezier(x1, y1, x2, y2)`; NIL = linear.
    pub fn ease(&mut self, v: ScriptValue, _path: &str) -> Result<Ease, String> {
        if v.is_nil() {
            return Ok(Ease::Linear);
        }
        if let Some(id) = v.as_id() {
            let s = id.to_string();
            return Ease::from_id(&s).ok_or_else(|| {
                let ids = Ease::ids();
                format!(
                    "unknown ease `@{s}`{}",
                    anim::closest(&s, ids.iter().map(|x| x.as_str())).map(|c| format!("; did you mean `@{c}`?")).unwrap_or_default()
                )
            });
        }
        match self.is_anim(v).as_deref() {
            Some("spring") => {
                let o = v.as_object().unwrap();
                let k = self.number(self.get(o, "k")).unwrap_or(170.0);
                let d = self.number(self.get(o, "d")).unwrap_or(26.0);
                Ok(Ease::Spring { k, d })
            }
            Some("bezier") => {
                let o = v.as_object().unwrap();
                let g = |r: &Self, k| r.number(r.get(o, k)).unwrap_or(0.0);
                Ok(Ease::Bezier { x1: g(self, "x1"), y1: g(self, "y1"), x2: g(self, "x2"), y2: g(self, "y2") })
            }
            _ => Err(format!("an ease is `@ease_out_cubic`, `spring(k, d)` or `bezier(x1, y1, x2, y2)`, not {}", self.describe(v))),
        }
    }

    /// `asset(id, rev)`, `file("x.glb")` or a plain string path.
    pub fn media(&mut self, obj: ScriptObject, path: &str, name: &str) -> Option<MediaRef> {
        note_read(obj, name, "media", || None, None);
        let v = self.get(obj, name);
        if v.is_nil() {
            return None;
        }
        if v.is_string_like() {
            return self.string_of(v).map(MediaRef::File);
        }
        match self.is_anim(v).as_deref() {
            Some("asset") => {
                let o = v.as_object().unwrap();
                let id = self.string_of(self.get(o, "id")).unwrap_or_default();
                let rev = self.string_of(self.get(o, "rev")).unwrap_or_default();
                Some(MediaRef::Asset { id, rev })
            }
            Some("file") => {
                let o = v.as_object().unwrap();
                self.string_of(self.get(o, "path")).map(MediaRef::File)
            }
            _ => {
                let loc = self.loc(obj);
                self.error(loc, &format!("{path}.{name}"), format!("`{name}` must be asset(\"id\", \"rev\") or file(\"path\"), not {}", self.describe(v)), None);
                None
            }
        }
    }

    /// A kept closure (for `each: fn(i, n, t)` and shader hooks).
    pub fn closure(&mut self, obj: ScriptObject, name: &str) -> Option<u32> {
        note_read(obj, name, "fn", || None, None);
        let v = self.get(obj, name);
        let o = v.as_object()?;
        if !self.vm.bx.heap_ref().is_fn(o) {
            return None;
        }
        let index = self.closures.len() as u32;
        let r = self.vm.bx.heap_mut().new_object_ref(o);
        self.closures.push(r);
        Some(index)
    }
}

fn fit<const N: usize>(v: [f32; 4], have: usize) -> Option<[f32; N]> {
    if N == 1 && have > 1 {
        return None;
    }
    if N > have && !(N == 4 && have == 3) {
        return None;
    }
    let mut out = [0.0; N];
    out.copy_from_slice(&v[..N]);
    Some(out)
}

fn expected_what<const N: usize>(semantic: Semantic) -> &'static str {
    match (N, semantic) {
        (1, _) => "a number",
        (2, _) => "vec2(x, y)",
        (3, _) => "vec3(x, y, z) or a number",
        (_, Semantic::Color) => "a colour like #ff5a36",
        _ => "vec4(x, y, z, w)",
    }
}

fn expected_syntax<const N: usize>(name: &str, semantic: Semantic) -> String {
    let v = match (N, semantic) {
        (1, _) => "1.0",
        (2, _) => "vec2(1, 1)",
        (3, _) => "vec3(0, 1, 0)",
        (_, Semantic::Color) => "#ff5a36",
        _ => "vec4(1, 1, 1, 1)",
    };
    format!("write `{name}: {v}`, `{name}: tween(a, b, start, end, @ease_out_cubic)`, `{name}: keys([0, a], [1.5, b, @ease_in_out])` or `{name}: fn(t) {{ ... }}`")
}

pub fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The Splash name of a kind (the first alias).
pub fn type_name_of(kind: &str) -> &'static str {
    TYPES.iter().find(|(_, k)| *k == kind).map(|(n, _)| *n).unwrap_or("object")
}

/// Every Splash type name of a kind family, for messages.
pub fn names_where(pred: impl Fn(&str) -> bool) -> Vec<&'static str> {
    let mut seen = Vec::new();
    for (n, k) in TYPES {
        if pred(k) && !seen.contains(n) {
            seen.push(*n);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str) -> (DocVm, ScriptValue) {
        let mut doc = DocVm::new(HashMap::from([("title".to_string(), InputValue::Text("HELLO".into())), ("accent".to_string(), InputValue::Color([1.0, 0.0, 0.0, 1.0]))]));
        let v = doc.with(|vm| {
            vm.capture_errors();
            let v = vm.eval(ScriptMod::new("", "", "t.splash", 0, 0, &format!("{SCENE_PRELUDE}{src}"), &[]));
            let e = vm.take_errors();
            assert!(e.is_empty(), "{e:?}");
            v
        });
        (doc, v)
    }

    #[test]
    fn values_and_anims_read() {
        let (mut doc, v) = eval("Mesh{\n  position: vec3(1, 2, 3)\n  scale: 2\n  color: #ff0000\n  rotation: tween(vec3(0,0,0), vec3(0,90,0), 1, 2, @ease_out_back)\n  opacity: keys([0, 0], [1, 1, @ease_in], [2, 0.5])\n  y: repeat(tween(0, 1, 0, 1), 1)\n  s: music(@low, 1, 2)\n  f: fn(t) { t * 2.0 }\n  name: in(@title)\n  metalnes: 1\n}");
        doc.with(|vm| {
            let mut r = Reader::new(vm, SCENE_PRELUDE_LINES);
            let o = v.as_object().unwrap();
            assert_eq!(r.kind(v).as_deref(), Some("mesh"));
            assert_eq!(r.loc(o).line, 1);
            let p = r.anim::<3>(o, "m", "position", [0.0; 3], Semantic::Plain);
            assert_eq!(p, Anim::Const([1.0, 2.0, 3.0]));
            assert_eq!(r.anim::<3>(o, "m", "scale", [1.0; 3], Semantic::Plain), Anim::Const([2.0; 3]));
            let c = r.anim::<4>(o, "m", "color", [1.0; 4], Semantic::Color);
            assert_eq!(c, Anim::Const([1.0, 0.0, 0.0, 1.0]));
            assert!(matches!(r.anim::<3>(o, "m", "rotation", [0.0; 3], Semantic::Plain), Anim::Keys(k) if k.len() == 2 && k[1].ease == Ease::Out(anim::Curve::Back)));
            assert!(matches!(r.anim::<1>(o, "m", "opacity", [1.0], Semantic::Plain), Anim::Keys(k) if k.len() == 3));
            assert!(matches!(r.anim::<1>(o, "m", "y", [0.0], Semantic::Plain), Anim::Loop { .. }));
            assert!(matches!(r.anim::<3>(o, "m", "s", [1.0; 3], Semantic::Plain), Anim::Audio { .. }));
            assert!(matches!(r.anim::<1>(o, "m", "f", [0.0], Semantic::Plain), Anim::Script(0)));
            assert_eq!(r.string(o, "name").as_deref(), Some("HELLO"));
            r.check_fields(o, "m", "Mesh", &["position", "scale", "color", "rotation", "opacity", "y", "s", "f", "name", "metalness"]);
            assert_eq!(r.diagnostics.len(), 1, "{:?}", r.diagnostics);
            assert_eq!(r.diagnostics[0].hint.as_deref(), Some("did you mean `metalness`?"));
            assert!(r.diagnostics.iter().all(|d| !d.is_error()));
        });
    }

    #[test]
    fn bad_values_teach() {
        let (mut doc, v) = eval("Mesh{\n  rotation: tween(0, 1, 2, 1)\n  roughness: \"x\"\n  e: tween(0, 1, 0, 1, @ease_out_bak)\n  camelCase: 1\n}");
        doc.with(|vm| {
            let mut r = Reader::new(vm, SCENE_PRELUDE_LINES);
            let o = v.as_object().unwrap();
            r.anim::<3>(o, "m", "rotation", [0.0; 3], Semantic::Plain);
            r.f32(o, "m", "roughness", 0.5);
            r.anim::<1>(o, "m", "e", [0.0], Semantic::Plain);
            r.check_fields(o, "m", "Mesh", &["rotation", "roughness", "e", "camel_case"]);
            let text: Vec<String> = r.diagnostics.iter().map(|d| d.to_string()).collect();
            assert!(text[0].contains("before it starts"), "{text:?}");
            assert!(text[1].contains("must be a number"), "{text:?}");
            assert!(text[2].contains("ease_out_back"), "{text:?}");
            assert!(text[3].contains("snake_case"), "{text:?}");
            assert_eq!(r.diagnostics[0].loc.line, 2);
        });
    }

    #[test]
    fn closures_run_with_live_audio() {
        let (mut doc, v) = eval("Mesh{ f: fn(t, edit) { music(@m).beat * 10.0 + t } }");
        doc.host.std.evaluating = true;
        doc.host.std.audio_now.beat = 0.5;
        let out = doc.with(|vm| {
            let o = v.as_object().unwrap();
            let f = vm.bx.heap_ref().value(o, id!(f).into(), NoTrap);
            vm.call(f, &[2.0f64.into(), 3.0f64.into()]).as_number()
        });
        assert_eq!(out, Some(7.0));
    }
}


/// Empty the `memo()` cache: a new evaluation (frame or sub-frame).
pub(crate) fn memo_clear() {
    crate::forms::clear_memo();
}

unsafe impl makepad_script::ScriptHostCapability for M3Host {
    const ID: makepad_script::LiveId = makepad_script::LiveId::from_str("makepad.motion3d.host.v1");
}
