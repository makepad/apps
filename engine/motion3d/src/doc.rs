//! Loading a Scene3D document: evaluate the Splash (written in `mod.scene`), read it
//! into a [`SceneDef`] (building geometry, loading models, fonts, images and
//! environments once), and keep the VM for the document's closures.

use makepad_core_util::ui_thread::WorkerOnly;
use makepad_script_compute_sched::kernel::KernelExt;
use crate::anim::{Anim, ScriptEval};
use makepad_render_material::ibl::{EnvMap, EnvPreset};
use makepad_geom3d::*;
use makepad_text_mesh::letters::*;
use crate::glb::{AlphaMode, GlbModel};
use crate::layout::{InstanceBase, Layout};
use makepad_geom3d::mesh::Mesh;
use crate::particles::{Emitter, ParticleSystem, Turbulence};
use crate::scene::*;
use crate::script::*;
// Explicit: makepad_widgets exports a MediaRef too (with its full features).
use crate::script::MediaRef;
use makepad_widgets::makepad_script::EvalLimits;
use makepad_widgets::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Where `asset(id, rev)` and `file("...")` bytes come from.
pub trait MediaResolver {
    fn load(&self, media: &MediaRef) -> Result<Arc<Vec<u8>>, String>;
}

/// Files relative to a base directory; assets from a table (`id` or
/// `id@rev` → path), for the CLI and tests. Stage supplies its library.
#[derive(Clone, Debug, Default)]
pub struct FileResolver {
    pub base: PathBuf,
    pub assets: HashMap<String, PathBuf>,
    /// A library directory where asset `id` is the file `<id>.<ext>`.
    pub library: Option<PathBuf>,
}

/// The largest media file a document may load (bytes).
const MAX_MEDIA_BYTES: u64 = 1 << 30;

/// The engine's own read-only resource directories, which a document may
/// name by absolute path: the bundled fonts (Makepad's `widgets/resources`,
/// or `MOTION3D_FONTS_DIR`) and the directories hosts name with
/// [`trust_resource_root`] (Motion names the resources of every crate whose
/// fonts it draws, whose Text3D layers lower to Scene3D source naming those
/// files). Nothing else outside the document's directory is reachable.
pub fn trusted_resource_roots() -> Vec<makepad_platform::resource_resolver::ResourceResolver> {
    use makepad_platform::resource_resolver::ResourceResolver;
    let mut dirs = vec![makepad_text_mesh::letters::fonts_dir()];
    dirs.extend(TRUSTED.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned());
    dirs.iter().filter_map(|dir| ResourceResolver::new(dir).ok()).collect()
}

static TRUSTED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Let documents name files under `dir` by absolute path (read only).
pub fn trust_resource_root(dir: impl Into<PathBuf>) {
    let dir = dir.into();
    let mut trusted = TRUSTED.lock().unwrap_or_else(|e| e.into_inner());
    if !trusted.contains(&dir) {
        trusted.push(dir);
    }
}

impl MediaResolver for FileResolver {
    /// Document names are untrusted: `file(..)` resolves beneath `base` only
    /// (no `..` or symlinks; an absolute path only inside the engine's
    /// trusted resource roots), and an asset id is checked
    /// against the id grammar before it names a library file. The media
    /// table's paths come from the host and are read as given.
    fn load(&self, media: &MediaRef) -> Result<Arc<Vec<u8>>, String> {
        use makepad_platform::resource_resolver::{validate_asset_id, ResourceResolver};
        let confined = |dir: &std::path::Path, name: &str| ResourceResolver::new(dir).and_then(|r| r.read(name, MAX_MEDIA_BYTES)).map_err(|e| e.to_string());
        let bytes = match media {
            MediaRef::File(p) if std::path::Path::new(p).is_absolute() => {
                ResourceResolver::read_within(&trusted_resource_roots(), std::path::Path::new(p), MAX_MEDIA_BYTES).map_err(|e| e.to_string())?
            }
            MediaRef::File(p) => confined(&self.base, p)?,
            MediaRef::Asset { id, rev } => match self.assets.get(&format!("{id}@{rev}")).or_else(|| self.assets.get(id)) {
                Some(p) => std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?,
                None => {
                    validate_asset_id(id).map_err(|e| e.to_string())?;
                    let (dir, name) = self
                        .library
                        .as_ref()
                        .and_then(|dir| ["glb", "gltf", "hdr", "png", "jpg", "svg", "ttf", "otf"].iter().map(|e| format!("{id}.{e}")).find(|n| dir.join(n).exists()).map(|n| (dir, n)))
                        .ok_or_else(|| format!("asset {id} rev {rev} is not in the media table"))?;
                    confined(dir, &name)?
                }
            },
        };
        Ok(Arc::new(bytes))
    }
}

pub struct LoadOptions<'a> {
    /// For messages and the VM's module name.
    pub name: String,
    pub inputs: HashMap<String, InputValue>,
    pub resolver: &'a dyn MediaResolver,
    /// The host line the given source starts on (1 = its own first line).
    pub first_line: u32,
    /// The song's words and beats on the scene's clock (`word(..)`,
    /// `beat_near(..)`), when the host has them.
    pub song: Option<Arc<SongTiming>>,
    /// The file the source is written in, by the name edit mode's shader
    /// literal sites give it (`Cx::set_shader_live_literals`): its
    /// ShaderMaterial hooks' literals then compile as slots an editor
    /// patches. `None`: they fold.
    pub origin_file: Option<String>,
}

/// A Motion document's VM, as its scenes live in it: whoever draws the
/// document locks it (one thread at a time) and lends it to the scene's
/// calls.
pub trait SceneVmHost: Send + Sync {
    fn with_vm(&self, f: &mut dyn FnMut(&mut ScriptVm));
}

/// A `Scene3D{..}` object a Motion document's VM evaluated: the VM and the
/// object, compared by identity.
#[derive(Clone)]
pub struct SceneObject {
    host: Arc<dyn SceneVmHost>,
    object: ScriptObject,
    /// The columns the document's first line has before the document's own
    /// text in its VM (the `use` prefix it evaluates).
    prefix_cols: u32,
}

impl SceneObject {
    pub fn new(host: Arc<dyn SceneVmHost>, object: ScriptObject, prefix_cols: u32) -> Self {
        Self { host, object, prefix_cols }
    }

    /// Run `f` with the document's VM (locked meanwhile).
    pub fn with_vm<R>(&self, f: impl FnOnce(&mut ScriptVm) -> R) -> R {
        let mut f = Some(f);
        let mut out = None;
        self.host.with_vm(&mut |vm| out = Some((f.take().expect("one call"))(vm)));
        out.expect("the document's VM ran the call")
    }

    pub fn object(&self) -> ScriptObject {
        self.object
    }

    /// A number naming this scene among the document's (the VM, the object):
    /// equal for equal scenes.
    pub fn key(&self) -> u64 {
        LiveId::from_str(&format!("{:p}/{:?}", Arc::as_ptr(&self.host) as *const u8, self.object)).0
    }
}

impl PartialEq for SceneObject {
    fn eq(&self, other: &Self) -> bool {
        Arc::as_ptr(&self.host) as *const u8 == Arc::as_ptr(&other.host) as *const u8 && self.object == other.object
    }
}

impl Eq for SceneObject {}

impl std::fmt::Debug for SceneObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SceneObject({:?})", self.object)
    }
}

/// What reading a scene object from its document's VM needs.
pub struct ObjectOptions<'a> {
    /// For messages.
    pub name: String,
    pub resolver: &'a dyn MediaResolver,
    /// The document's tempo, for `beats(n)` times unless the scene sets `bpm`.
    pub bpm: f64,
    /// The file the scene is written in, by the name edit mode's shader
    /// literal sites give it (see [`LoadOptions::origin_file`]).
    pub origin_file: Option<String>,
    /// The fields of the layer itself (its place and time), which the
    /// document's loader reads: not the scene's, and not unknown.
    pub host_fields: &'static [&'static str],
}

/// What the shared half of a load needs (see [`Scene3dDoc::read_from`]).
struct ReadOptions<'a> {
    source: &'a str,
    resolver: &'a dyn MediaResolver,
    bpm: Option<f64>,
    origin_file: Option<(String, u32)>,
    host_fields: &'static [&'static str],
    line_offset: u32,
    col_shift: u32,
    shift: u32,
    read_at_load: Option<std::collections::HashSet<String>>,
}

/// The VM a document's closures run in: its own (a scene-only source), or
/// its Motion document's. Its [`M3Host`] is lent the scene's audio for the
/// length of each call.
enum SceneVm {
    Own(DocVm),
    Doc(Arc<dyn SceneVmHost>),
}

thread_local! {
    static SCENE_VM_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(debug_assertions)]
struct SceneVmCall;

#[cfg(debug_assertions)]
impl Drop for SceneVmCall {
    fn drop(&mut self) {
        SCENE_VM_DEPTH.set(SCENE_VM_DEPTH.get() - 1);
    }
}

impl SceneVm {
    /// Run `f` with the VM; `audio`: closures read `music(@m)` as these live
    /// levels (while `f` runs).
    fn with<R>(&mut self, audio: Option<AudioNow>, f: impl FnOnce(&mut ScriptVm) -> R) -> R {
        let run = move |vm: &mut ScriptVm| {
            #[cfg(debug_assertions)]
            let _call = {
                SCENE_VM_DEPTH.set(SCENE_VM_DEPTH.get() + 1);
                SceneVmCall
            };
            if let Some(h) = crate::script::host_mut(vm) {
                h.evaluating = audio.is_some();
                if let Some(now) = audio {
                    h.audio_now = now;
                }
            }
            let out = f(vm);
            if let Some(h) = crate::script::host_mut(vm) {
                h.evaluating = false;
            }
            out
        };
        match self {
            SceneVm::Own(vm) => vm.with(run),
            SceneVm::Doc(cell) => {
                let mut run = Some(run);
                let mut out = None;
                cell.with_vm(&mut |vm| out = Some((run.take().expect("one call"))(vm)));
                out.expect("the document's VM ran the call")
            }
        }
    }

    /// The host state of a scene-only source's own VM (outside a call).
    fn own_host(&mut self) -> Option<&mut M3Host> {
        match self {
            SceneVm::Own(vm) => Some(&mut vm.host.std),
            SceneVm::Doc(_) => None,
        }
    }
}

/// Loads the `Kernel{}` and `Sim{}` objects of the bodies the VM
/// evaluated from `first_body` on into its host's kernels (crate::objects).
fn load_kernels(vm: &mut ScriptVm, first_body: usize) -> Vec<crate::objects::KernelError> {
    let Some(h) = crate::script::host_mut(vm) else { return Vec::new() };
    let Some(entries) = h.kernel_entries.clone() else { return Vec::new() };
    let mut kernels = std::mem::take(&mut h.kernels);
    let errors = crate::objects::load(vm, &mut kernels, &entries, first_body);
    if let Some(h) = crate::script::host_mut(vm) {
        h.kernels = kernels;
    }
    errors
}

/// A loaded document: the typed scene plus the VM that runs its closures.
pub struct Scene3dDoc {
    pub def: SceneDef,
    pub warnings: Vec<Diagnostic>,
    vm: SceneVm,
    closures: Vec<ScriptObjectRef>,
    _root: Option<ScriptObjectRef>,
    /// Script errors of per-frame calls since the host last took them.
    eval_errors: Vec<String>,
    /// The frame's time exactly (the host's seconds): a closure called at
    /// this time (as f32) gets it, so `t >= 36.6` holds on frame 36.6.
    exact_t: Option<(f32, f64)>,
    /// What `music(@m)` gives the closures of the evaluation in progress.
    audio: Option<AudioNow>,
    /// Effect physics: the stepper, seeked to every evaluated time.
    pub(crate) physics: Option<crate::physics::PhysicsRuntime>,
    /// The built models (`mod.model`) it places, as they were at load.
    pub models: Vec<crate::models::ModelUse>,
    /// The inputs its load read: a change to one of them needs a new load;
    /// any other reaches it through [`Self::set_inputs`]. None: all of them
    /// (a Seq3D, lowered from what its template read; a scene a document's
    /// VM evaluated, whose inputs are the document's).
    read_at_load: Option<std::collections::HashSet<String>>,
}

/// Its references into the VM go under the VM's lock: a document's heap may
/// be in use by another thread.
impl Drop for Scene3dDoc {
    fn drop(&mut self) {
        let closures = std::mem::take(&mut self.closures);
        let root = self._root.take();
        self.vm.with(None, |_| {
            drop(closures);
            drop(root);
        });
    }
}

// SAFETY: as motion's document VM: everything not `Send` in a document (its
// VM's `Rc`s and `RefCell`s, the natives' closures, the references into its
// heap) is reached only through the document, and a Motion document's VM
// only under its lock (`SceneVmHost`), so a document loaded on a worker is
// moved to the thread that draws it like any owned value.
unsafe impl Send for Scene3dDoc {}

impl ScriptEval for Scene3dDoc {
    fn eval_script_array(&mut self, index: u32, t: f32, edit: f32, out: &mut Vec<f32>) -> bool {
        let Some(f) = self.closures.get(index as usize).map(|r| r.as_object()) else { return false };
        let errors = &mut self.eval_errors;
        let exact = self.exact_t;
        self.vm.with(self.audio, |vm| {
            let t = match exact {
                Some((t32, t64)) if t32 == t => t64,
                _ => t as f64,
            };
            let Some(v) = frame_call(vm, errors, f, &[t.into(), (edit as f64).into()]) else { return false };
            crate::script::flatten_numbers(vm, v, out)
        })
    }
    fn eval_script(&mut self, index: u32, t: f32, edit: f32, out: &mut [f32]) -> bool {
        let Some(f) = self.closures.get(index as usize).map(|r| r.as_object()) else { return false };
        let errors = &mut self.eval_errors;
        let exact = self.exact_t;
        self.vm.with(self.audio, |vm| {
            let t = match exact {
                Some((t32, t64)) if t32 == t => t64,
                _ => t as f64,
            };
            let Some(v) = frame_call(vm, errors, f, &[t.into(), (edit as f64).into()]) else { return false };
            let r = Reader::new(vm, 0);
            // A number, a colour, a vec4, vec3 or vec2 (a vec2 fills x, y).
            let got: Option<[f32; 4]> = r
                .plain::<4>(v, Semantic::Color)
                .or_else(|| r.plain::<3>(v, Semantic::Plain).map(|p| [p[0], p[1], p[2], 1.0]))
                .or_else(|| r.plain::<2>(v, Semantic::Plain).map(|p| [p[0], p[1], 0.0, 1.0]));
            match got {
                Some(p) => {
                    for (k, o) in out.iter_mut().enumerate() {
                        *o = if v.as_number().is_some() { p[0] } else { p[k.min(3)] };
                    }
                    true
                }
                None => false,
            }
        })
    }
}

/// What one instance's `each: fn(i, n, t)` returned.
#[derive(Clone, Copy, Debug, Default)]
pub struct EachOut {
    pub position: Option<[f32; 3]>,
    pub rotation: Option<[f32; 3]>,
    pub scale: Option<[f32; 3]>,
    pub color: Option<[f32; 4]>,
}

impl Scene3dDoc {
    /// Load a scene-only source (a `Scene3D{...}` or any source whose value
    /// is one) in a VM of its own.
    pub fn load(source: &str, opts: LoadOptions) -> Result<Scene3dDoc, Vec<Diagnostic>> {
        Self::load_with_modules(source, opts, &[])
    }

    /// Load with the host document's Splash modules (resolved, dependencies
    /// first): the scene imports them as the document does.
    pub fn load_with_modules(source: &str, opts: LoadOptions, modules: &[makepad_script_imports::LoadedModule]) -> Result<Scene3dDoc, Vec<Diagnostic>> {
        let lowered = source;
        let shift = opts.first_line.saturating_sub(1);
        let at_doc = |e: String| Diagnostic::error(Loc { line: opts.first_line.max(1), col: 1 }, "", e);
        // A kernel error at its place: the document's own lines (after the
        // prelude), else a module's file named.
        let kernel_diag = |e: crate::objects::KernelError| -> Diagnostic {
            if e.file.starts_with("motion3d://") {
                Diagnostic::error(Loc { line: e.line.saturating_sub(PRELUDE_LINES).max(1) + shift, col: e.col }, &e.kernel, e.message)
            } else {
                at_doc(format!("{}:{}:{}: {}", e.file, e.line, e.col, e.message))
            }
        };
        let hash = makepad_geom3d::math::hash64(source.bytes().fold(0u64, |h, b| h.wrapping_mul(0x100_0000_01b3) ^ b as u64));
        let mut own = DocVm::new(opts.inputs.clone());
        own.host.std.song = opts.song.clone();
        let first_body = own.with(|vm| vm.body_count());
        let mut vm = SceneVm::Own(own);
        if !modules.is_empty() {
            let errors = vm.with(None, |vm| crate::script::load_modules(vm, modules));
            if !errors.is_empty() {
                return Err(errors.into_iter().map(at_doc).collect());
            }
        }
        // `mod.libs.<name>`: the shared model kits, evaluated first.
        vm.with(None, |vm| crate::models::load_kits(vm, lowered)).map_err(|mut e| {
            for d in &mut e {
                d.hint = Some("kits are the `<name>.splash` files in the kit folders the host installed; write `use mod.libs.<name>.*`".into());
            }
            e
        })?;
        let (value, errors) = vm.with(None, |vm| {
            vm.capture_errors();
            let (v, _) = vm.eval_bounded(&EvalLimits::DOC_LOAD, |vm| vm.eval(ScriptMod::new("", "", &(format!("motion3d://{}/{hash:016x}", opts.name)), 0, 0, &(format!("{PRELUDE}{lowered}")), &[])));
            (v, vm.take_errors())
        });
        // The inputs the evaluation read are baked into what it built; the
        // closures the reader keeps read theirs per frame (it may call one
        // to learn its shape: that read is not a load's). The tempo is read
        // below.
        let mut read_at_load: std::collections::HashSet<String> = vm.own_host().and_then(|h| h.read_at_load.take()).unwrap_or_default();
        read_at_load.insert("bpm".into());
        let line_offset = PRELUDE_LINES;
        if !errors.is_empty() || value.is_err() {
            let list = if errors.is_empty() { vec!["the document did not evaluate".to_string()] } else { errors };
            return Err(list.into_iter().map(|e| script_error(&e, line_offset, shift)).collect());
        }
        // Its `Kernel{..}` and `Sim{..}` objects (and its modules'), compiled
        // and loaded for the nodes that draw them.
        let kernel_errors: Vec<Diagnostic> = vm.with(None, |vm| load_kernels(vm, first_body)).into_iter().map(kernel_diag).collect();
        if !kernel_errors.is_empty() {
            return Err(kernel_errors);
        }
        // A `Seq3D{…}` is the Scene3D its template (seq3d.splash) builds.
        let seq = vm.with(None, |vm| {
            let mut r = Reader::new(vm, line_offset);
            let obj = value.as_object().filter(|_| r.kind(value).as_deref() == Some("seq3d"))?;
            let spec = crate::seq3d::read_spec(&mut r, obj);
            let loc = r.loc(obj);
            let diags = r.diagnostics;
            Some(if diags.iter().any(|d| d.is_error()) { Err(diags) } else { crate::seq3d::scene(vm, &spec).map_err(|e| vec![Diagnostic::error(loc, "template", e)]) })
        });
        let value = match seq {
            Some(Ok(scene)) => scene,
            Some(Err(diags)) => return Err(diags),
            None => value,
        };
        let read = ReadOptions { source, resolver: opts.resolver, bpm: None, origin_file: opts.origin_file.clone().map(|file| (file, shift)), host_fields: &[], line_offset, col_shift: 0, shift, read_at_load: Some(read_at_load) };
        let object = value.as_object();
        Self::read_from(vm, object, read)
    }

    /// Read a scene a Motion document's VM evaluated: the second half of a
    /// load, over the document's own VM instead of one of its own. Errors
    /// and warnings are at the document's lines.
    pub fn from_object(scene: &SceneObject, opts: ObjectOptions) -> Result<Scene3dDoc, Vec<Diagnostic>> {
        let read = ReadOptions { source: "", resolver: opts.resolver, bpm: Some(opts.bpm), origin_file: opts.origin_file.clone().map(|file| (file, 0)), host_fields: opts.host_fields, line_offset: 0, col_shift: scene.prefix_cols, shift: 0, read_at_load: None };
        Self::read_from(SceneVm::Doc(scene.host.clone()), Some(scene.object), read)
    }

    /// Read `object` (a `Scene3D{..}`) in `vm`; the shared half of the loads.
    fn read_from(mut vm: SceneVm, object: Option<ScriptObject>, o: ReadOptions) -> Result<Scene3dDoc, Vec<Diagnostic>> {
        let ReadOptions { source, resolver, bpm, origin_file, host_fields, line_offset, col_shift, shift, read_at_load } = o;
        let mut cx = LoadCx { resolver, source, glbs: HashMap::new(), node_names: Vec::new(), models: Vec::new(), budget: Default::default(), hdri_site: None };
        // Reading runs document code too (builders, parametric functions):
        // bounded like the evaluation.
        let (result, report) = vm.with(None, |vm| vm.eval_bounded(&EvalLimits::DOC_LOAD, |vm| {
            let mut r = Reader::new(vm, line_offset);
            r.col_shift = col_shift;
            r.default_bpm = bpm;
            r.host_fields = host_fields;
            r.origin_file = origin_file;
            let Some(obj) = object else {
                r.error(Loc { line: 1, col: 1 }, "", "a Scene3D document is one `Scene3D{...}` value", Some("write `Scene3D{ camera: Camera{...} children: [...] }`".into()));
                return Err(r.diagnostics);
            };
            if r.kind(obj.into()).as_deref() != Some("scene") {
                let loc = r.loc(obj);
                r.error(loc, "", format!("the document's value is {}, not a Scene3D", r.describe(obj.into())), Some("the last expression must be `Scene3D{...}`".into()));
                return Err(r.diagnostics);
            }
            let root = r.vm.bx.heap_mut().new_object_ref(obj);
            let def = parse_scene(&mut r, &mut cx, obj);
            let closures = std::mem::take(&mut r.closures);
            // Collect this parse's model messages before another scene uses the VM.
            let said = crate::script::host(r.vm).map(|h| std::mem::take(&mut *h.model_messages.borrow_mut())).unwrap_or_default();
            Ok((def, r.diagnostics, closures, root, said))
        }));
        if let Some(limit) = report.limit() {
            vm.with(None, |_| drop(result));
            return Err(vec![script_error(&limit, line_offset, shift)]);
        }
        let (def, warnings, closures, root, said) = result?;
        // The document owns its VM roots during map resolution, including on a
        // failed file read or an unwind (its Drop releases them under the lock).
        let mut doc = Scene3dDoc { def, warnings, vm, closures, _root: Some(root), eval_errors: Vec::new(), exact_t: None, audio: None, physics: None, models: std::mem::take(&mut cx.models), read_at_load };
        if let Some(env) = &mut doc.def.environment {
            match environment_map(&env.source, &mut cx) {
                Ok(map) => {
                    env.key = env_key(&env.source, &map);
                    env.map = map;
                }
                Err(diagnostic) => doc.warnings.push(diagnostic),
            }
        }
        for d in &mut doc.warnings {
            d.loc.line += shift;
        }
        doc.warnings.extend(said.into_iter().map(|m| Diagnostic::warning(Loc { line: 1, col: 1 }, "mod.model", m)));
        if doc.warnings.iter().any(|d| d.is_error()) {
            return Err(std::mem::take(&mut doc.warnings));
        }
        // A standalone VM is frozen from here on: a frame's closures read the
        // document but cannot keep state in it, so frames are pure functions
        // of their arguments and can be evaluated in any order. (A Motion
        // document's VM was frozen as it loaded: the scene's closures are
        // among what it holds.)
        if let SceneVm::Own(_) = &doc.vm {
            let roots: Vec<ScriptValue> = doc._root.iter().chain(&doc.closures).map(|r| r.as_object().into()).collect();
            doc.vm.with(None, |vm| {
                vm.freeze_document(&roots);
                // What `memo()` kept while it loaded refers into this VM: it
                // goes now, under the VM's lock (a worker loads, the UI thread
                // draws).
                crate::script::memo_clear();
            });
        }
        doc.physics = doc.def.physics.clone().map(crate::physics::PhysicsRuntime::new);
        Ok(doc)
    }

    /// Whether this document, loaded with `old` inputs, shows `new` by
    /// [`Self::set_inputs`] alone: every input that differs was read only by
    /// its per-frame closures (none at load).
    pub fn takes_inputs(&self, old: &HashMap<String, InputValue>, new: &HashMap<String, InputValue>) -> bool {
        old.keys().chain(new.keys()).all(|k| old.get(k) == new.get(k) || self.read_at_load.as_ref().is_some_and(|read| !read.contains(k)))
    }

    /// The host inputs its closures read from now on (see [`Self::takes_inputs`]).
    pub fn set_inputs(&mut self, inputs: HashMap<String, InputValue>) {
        if let Some(h) = self.vm.own_host() {
            h.inputs = inputs;
        }
    }

    /// A built model it places is still building: the document loaded
    /// without it (a realtime host shows the build's progress; locked time
    /// waits, see [`crate::layer::Scene3dLayer::painted`]).
    pub fn waiting_for_models(&self) -> bool {
        crate::models::waiting(&self.models)
    }

    /// A built model it places installed, failed or changed since it
    /// loaded: the host loads it again.
    pub fn models_changed(&self) -> bool {
        !self.models.is_empty() && crate::models::changed(&self.models)
    }

    /// Run an instance closure `fn(i, n, t)` returning `{position rotation
    /// scale color}` (any subset).
    pub fn eval_each(&mut self, index: u32, i: usize, n: usize, t: f32, edit: f32) -> EachOut {
        let Some(f) = self.closures.get(index as usize).map(|r| r.as_object()) else { return EachOut::default() };
        let errors = &mut self.eval_errors;
        self.vm.with(self.audio, |vm| {
            let Some(v) = frame_call(vm, errors, f, &[(i as f64).into(), (n as f64).into(), (t as f64).into(), (edit as f64).into()]) else { return EachOut::default() };
            let r = Reader::new(vm, 0);
            let Some(o) = v.as_object() else { return EachOut::default() };
            EachOut {
                position: r.plain::<3>(r.get(o, "position"), Semantic::Plain),
                rotation: r.plain::<3>(r.get(o, "rotation"), Semantic::Plain),
                scale: r.plain::<3>(r.get(o, "scale"), Semantic::Plain),
                color: r.plain::<4>(r.get(o, "color"), Semantic::Color),
            }
        })
    }

    /// Script errors of per-frame calls (a limit, a write into the frozen
    /// document, a type error) since the last take, oldest first.
    /// The frame's time exactly, for the closures called at it (see
    /// `exact_t`); `None` when the host has only the f32 clock.
    pub fn set_exact_time(&mut self, t: Option<f64>) {
        self.exact_t = t.map(|t| (t as f32, t));
    }

    pub fn take_eval_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.eval_errors)
    }

    /// Interactive preview: physics steps at most `budget` per evaluation
    /// and shows its newest state until it reaches the asked time. `None`
    /// (the default) is locked time: every evaluation reaches its time, as
    /// exports need.
    pub fn set_physics_budget(&mut self, budget: Option<std::time::Duration>) {
        if let Some(p) = self.physics.as_mut() {
            p.set_realtime(budget);
        }
    }

    /// Set what `music(@m)` returns inside closures for the next calls.
    pub fn set_audio_now(&mut self, now: AudioNow) {
        self.audio = Some(now);
    }

    /// Whether a shader material's hooks, or a post pass that reads it,
    /// sample the 2D layer `name` with gradients or at a level
    /// (`self.name.sample_grad(` / `.sample_lod(`): the layer's picture then
    /// carries a mip chain.
    pub fn samples_minified(&self, name: &str) -> bool {
        fn calls(text: &str, name: &str) -> bool {
            [".sample_grad(", ".sample_lod("].iter().any(|call| text.contains(&format!("self.{name}{call}")))
        }
        fn hooks_sample(m: &MaterialDef, name: &str) -> bool {
            m.shader.as_ref().is_some_and(|h| h.fields.iter().any(|(_, text)| calls(text, name)))
        }
        fn node_samples(n: &NodeDef, name: &str) -> bool {
            let own = match &n.kind {
                NodeKind::Mesh { material, .. } => hooks_sample(material, name),
                NodeKind::Instances(i) => hooks_sample(&i.material, name),
                NodeKind::Model(m) => m.base_materials.iter().any(|m| hooks_sample(m, name)) || m.materials.iter().any(|o| hooks_sample(&o.def, name)),
                _ => false,
            };
            own || n.children.iter().any(|c| node_samples(c, name))
        }
        // `post: [Pass{reads: ["name"] pixel: fn() -> vec4 {..}}]`, its pixel fn or a helper.
        let in_post = self.def.post.graph.iter().any(|p| {
            p.decl.reads.iter().any(|r| r == name) && (calls(p.decl.pixel.text(), name) || p.decl.helpers.iter().any(|h| calls(h.code.text(), name)))
        });
        node_samples(&self.def.root, name) || in_post
    }

    /// A new evaluation (frame or sub-frame) begins: what `memo()` kept for
    /// the last one is forgotten.
    pub(crate) fn begin_evaluation(&mut self) {
        match &self.vm {
            SceneVm::Own(_) => crate::script::memo_clear(),
            SceneVm::Doc(_) => {
                self.vm.with(None, |vm| {
                    if let Some(clear) = crate::script::host(vm).and_then(|h| h.new_evaluation) {
                        clear(vm);
                    }
                });
            }
        }
    }
}

/// Errors a document keeps between takes (the rest are dropped).
const MAX_EVAL_ERRORS: usize = 32;

/// One per-frame call of a document closure under the frame limits. A
/// limit fails the call (None); every script error is kept for the host.
fn frame_call(vm: &mut ScriptVm, errors: &mut Vec<String>, f: ScriptObject, args: &[ScriptValue]) -> Option<ScriptValue> {
    let (v, report) = vm.eval_bounded(&EvalLimits::DOC_FRAME, |vm| vm.call(f.into(), args));
    for e in vm.take_errors() {
        if errors.len() < MAX_EVAL_ERRORS {
            errors.push(e);
        }
    }
    report.limit().is_none().then_some(v)
}

/// `file:line:col: message` from the VM, shifted into document lines.
fn script_error(text: &str, line_offset: u32, shift: u32) -> Diagnostic {
    // Errors read "motion3d://name/hash:12:5: message (platform/script/..)",
    // the line and column counted from 0 in the evaluated text (the
    // prelude's `line_offset` lines, then the document).
    let mut loc = Loc { line: 1, col: 1 };
    let mut message = text.trim().to_string();
    if let Some(sep) = text.find(": ") {
        let mut head = text[..sep].rsplitn(3, ':');
        if let (Some(c), Some(l), Some(_file)) = (head.next(), head.next(), head.next()) {
            if let (Ok(l), Ok(c)) = (l.trim().parse::<u32>(), c.trim().parse::<u32>()) {
                loc = Loc { line: (l + 1).saturating_sub(line_offset).max(1) + shift, col: c + 1 };
                message = text[sep + 2..].trim().to_string();
            }
        }
    }
    // The interpreter's own origin (" (…/file.rs:123)") is not the
    // author's business.
    if let Some(at) = message.rfind(" (") {
        let origin = &message[at + 2..];
        if origin.ends_with(')') && origin.contains(".rs:") && !origin.contains(' ') {
            message.truncate(at);
        }
    }
    if let Some(at) = message.find("motion3d://").filter(|at| *at > 0) {
        message.truncate(at);
    }
    Diagnostic::error(loc, "", message.trim_end().to_string()).hint("the scene is read after its Splash evaluates: fix this first")
}

pub(crate) struct LoadCx<'a> {
    pub(crate) resolver: &'a dyn MediaResolver,
    source: &'a str,
    glbs: HashMap<MediaRef, Arc<GlbModel>>,
    node_names: Vec<String>,
    /// Built models (`mod.model`) the document placed.
    pub(crate) models: Vec<crate::models::ModelUse>,
    /// Decoded bytes of this load, charged by the decoders before they
    /// allocate.
    budget: makepad_platform::resource_resolver::DecodeBudget,
    /// Source of a file environment's diagnostic after the VM has been released.
    hdri_site: Option<(Loc, String)>,
}

/// Number `node` and the nodes below it depth first, from `next`.
fn number_nodes(node: &mut NodeDef, next: &mut u32) {
    node.id = *next;
    *next += 1;
    for child in &mut node.children {
        number_nodes(child, next);
    }
}

const NODE_FIELDS: &[&str] = &[
    "name", "position", "rotation", "scale", "look_at", "pivot", "visible", "opacity", "span", "cast_shadow", "receive_shadow", "outline", "children",
    "translate", "rotate",
];

pub(crate) fn with_node_fields(extra: &[&'static str]) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = NODE_FIELDS.to_vec();
    v.extend_from_slice(extra);
    v
}

fn parse_scene(r: &mut Reader, cx: &mut LoadCx, obj: ScriptObject) -> SceneDef {
    let path = "Scene3D";
    let mut known = vec![
        "size", "duration", "background", "environment", "fog", "camera", "tone_mapping", "exposure", "post", "children", "lights", "model", "nodes",
        "materials", "music", "animation", "name", "antialias", "temporal_aa", "bpm", "beats_per_bar", "look", "physics",
    ];
    known.extend_from_slice(r.host_fields);
    r.check_fields(obj, path, "Scene3D", &known);
    // The tempo first: `beat(n)` / `bar(n)` times below resolve with it.
    let input_bpm =
        crate::script::host(r.vm)
            .and_then(|h| match h.inputs.get("bpm") {
                Some(InputValue::Number(n)) => Some(*n),
                _ => None,
            });
    r.bpm = r
        .number(r.get(obj, "bpm"))
        .or(input_bpm)
        .or(r.default_bpm)
        .unwrap_or(120.0)
        .max(1.0);
    r.beats_per_bar = r
        .number(r.get(obj, "beats_per_bar"))
        .unwrap_or(4.0)
        .max(1.0);
    let size = r.plain::<2>(r.get(obj, "size"), Semantic::Plain);
    let duration = r.number(r.get(obj, "duration")).map(|d| d as f32);
    let background = if r.has(obj, "background") { Some(r.anim::<4>(obj, path, "background", [0.0, 0.0, 0.0, 1.0], Semantic::Color)) } else { None };
    let environment = { let v = r.get(obj, "environment"); if v.is_nil() { None } else { parse_environment(r, cx, v, "Scene3D.environment") } };
    let fog = { let v = r.get(obj, "fog"); if v.is_nil() { None } else { parse_fog(r, v, "Scene3D.fog") } };
    let tone_mapping = match r.id(obj, "tone_mapping") {
        None => ToneMapping::Aces,
        Some(s) => ToneMapping::by_name(&s).unwrap_or_else(|| {
            let loc = r.loc(obj);
            r.warn(loc, "Scene3D.tone_mapping", format!("unknown tone mapping `@{s}`, using @aces"), Some(format!("one of {}", ToneMapping::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
            ToneMapping::Aces
        }),
    };
    let exposure = r.anim::<1>(obj, path, "exposure", [1.0], Semantic::Plain);
    let mut post = { let v = r.get(obj, "post"); parse_post(r, v, "Scene3D.post") };
    post.antialias = r.u32(obj, path, "antialias", 2, 4).max(1);
    post.temporal_aa = r.u32(obj, path, "temporal_aa", 1, 32).max(1);
    let music = r.id(obj, "music");

    // Children and lights, then M1's shorthand: a top-level model with
    // node and material overrides.
    let mut root = NodeDef {
        name: Some("scene".into()),
        id: 0,
        path: path.into(),
        loc: r.loc(obj),
        transform: TransformDef::default(),
        visible: Anim::Const([1.0]),
        opacity: Anim::Const([1.0]),
        span: None,
        cast_shadow: true,
        receive_shadow: true,
        outline: false,
        kind: NodeKind::Group,
        children: Vec::new(),
    };
    let kids = r.list(r.get(obj, "children"));
    for (i, v) in kids.into_iter().enumerate() {
        if let Some(n) = parse_node(r, cx, v, &format!("children[{i}]")) {
            root.children.push(n);
        }
    }
    let lights = r.list(r.get(obj, "lights"));
    for (i, v) in lights.into_iter().enumerate() {
        if let Some(n) = parse_node(r, cx, v, &format!("lights[{i}]")) {
            root.children.push(n);
        }
    }
    if r.has(obj, "model") {
        // M1's Scene3D: `model: asset(...)` with `nodes:`/`materials:` overrides.
        if let Some(n) = parse_model(r, cx, obj, "Scene3D", true) {
            root.children.push(n);
        }
    }
    let look = match r.id(obj, "look") {
        None => None,
        Some(s) => match Look::by_name(&s) {
            Some(l) => Some(l),
            None => {
                let loc = r.loc(obj);
                r.error(loc, "Scene3D.look", format!("no look `@{s}`"), Some(format!("one of {}", Look::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
                None
            }
        },
    };
    let (mut background, mut environment, mut post) = (background, environment, post);
    if let Some(l) = look {
        // What the look brings, where the author has not set it.
        let (bg, env, rig, bloom): (u32, Option<EnvPreset>, &str, Option<f32>) = match l {
            Look::Studio => (0x1b1d22, Some(EnvPreset::Studio), "LightRig{preset: @product}", None),
            Look::NeonNight => (0x06060c, Some(EnvPreset::Neon), "LightRig{preset: @neon}", Some(0.8)),
            Look::Clay => (0xd9d5cd, Some(EnvPreset::Softbox), "LightRig{preset: @product intensity: 0.9}", None),
            Look::Blueprint => (0x0a2744, None, "AmbientLight{intensity: 1}", Some(0.4)),
            Look::Holographic => (0x020409, Some(EnvPreset::Night), "AmbientLight{intensity: 0.4}", Some(1.0)),
            Look::Toon => (0xf3e9d2, None, "Group3D{children: [HemisphereLight{sky_color: #ffffff ground_color: #b8a88a intensity: 0.8} DirectionalLight{position: vec3(3, 6, 4) intensity: 2.2 cast_shadow: true}]}", None),
            Look::Ink => (0xf1ece0, None, "Group3D{children: [HemisphereLight{sky_color: #ffffff ground_color: #9a9a9a intensity: 0.9} DirectionalLight{position: vec3(-3, 6, 4) intensity: 2.0 cast_shadow: true}]}", None),
        };
        // Toon and ink bring the paper, unless the author set one.
        if matches!(l, Look::Toon | Look::Ink) && post.paper.is_none() {
            let (amount, halftone, color) = if l == Look::Ink { (0.9, 0.8, 0xf1ece0) } else { (0.6, 0.35, 0xf6ecd4) };
            post.paper = Some((Anim::Const([amount]), Anim::Const([halftone]), 7.0, Anim::Const(lin(color))));
        }
        if background.is_none() && !r.has(obj, "background") {
            background = Some(Anim::Const(lin(bg)));
        }
        if environment.is_none() {
            environment = env.map(|p| pending_environment(EnvSource::Preset(p)));
        }
        if !contains_light(&root) {
            let v = r.vm.eval(ScriptMod::new("", "", &(format!("motion3d://look/{l:?}")), 0, 0, &(format!("{SCENE_PRELUDE}{rig}")), &[]));
            if let Some(n) = parse_node(r, cx, v, "Scene3D.look") {
                root.children.push(n);
            }
        }
        if let (Some(s), None) = (bloom, &post.bloom) {
            post.bloom = Some((Anim::Const([s]), Anim::Const([0.6]), Anim::Const([0.9])));
        }
    }
    let has_light = contains_light(&root);
    let camera = { let v = r.get(obj, "camera"); if v.is_nil() { CameraDef::default() } else { parse_camera(r, v, "Scene3D.camera") } };
    let doc_hash = cx.source.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    let physics = { let v = r.get(obj, "physics"); crate::physics::parse_physics(r, cx, v, doc_hash, &root).map(Arc::new) };
    number_nodes(&mut root, &mut 0);
    let mut def = SceneDef { size, duration, background, environment, fog, camera, tone_mapping, exposure, post, root, music, look, physics };
    if !has_light && def.environment.is_none() {
        // A scene with nothing lighting it renders black: give it the
        // studio environment and say so.
        let loc = r.loc(obj);
        r.warn(loc, "Scene3D", "no lights and no environment: using Environment{preset: @studio}", Some("add `environment: Environment{preset: @studio}` or lights: [DirectionalLight{...}]".into()));
        def.environment = Some(pending_environment(EnvSource::Preset(EnvPreset::Studio)));
    }
    def
}

fn contains_light(n: &NodeDef) -> bool {
    matches!(n.kind, NodeKind::Light(_)) || n.children.iter().any(contains_light)
}

fn env_key(source: &EnvSource, map: &EnvMap) -> u64 {
    let tag = match source {
        EnvSource::Preset(p) => format!("{p:?}"),
        EnvSource::HdriPreset(name) => name.to_string(),
        EnvSource::Hdri(m) => format!("{m:?}"),
    };
    makepad_geom3d::math::hash64(tag.bytes().fold(map.width as u64, |h, b| h.wrapping_mul(0x100_0000_01b3) ^ b as u64))
}

/// The environment a Scene3D `@name` stands for: one of render-material's
/// looks (`@studio` .. `@gradient`, as always), else an HDRI generator
/// preset by its Scene3D name (`@hdri_golden_hour`,
/// `makepad_hdri_engine::PRESET_ENVIRONMENTS`). Nothing is made here: its
/// map is resolved by [`environment_map`] after parsing.
fn environment_source(name: &str) -> Option<EnvSource> {
    if let Some(p) = EnvPreset::by_name(name) {
        return Some(EnvSource::Preset(p));
    }
    let name = makepad_hdri_engine::PRESET_ENVIRONMENTS.iter().copied().find(|n| *n == name)?;
    generator_preset(name).map(|_| EnvSource::HdriPreset(name))
}

/// A generator preset's params by its Scene3D name (`hdri_golden_hour`).
fn generator_preset(name: &str) -> Option<makepad_hdri_engine::HdriParams> {
    makepad_hdri_engine::presets::preset(name.strip_prefix(makepad_hdri_engine::HDRI_PREFIX)?)
}

/// The parser records the source. Only `read_from` sees this empty map.
fn pending_environment(source: EnvSource) -> EnvironmentDef {
    static EMPTY: std::sync::OnceLock<Arc<EnvMap>> = std::sync::OnceLock::new();
    let map = EMPTY.get_or_init(|| Arc::new(EnvMap { width: 0, height: 0, data: Vec::new() })).clone();
    EnvironmentDef { source, map, key: 0, intensity: Anim::Const([1.0]), rotation: Anim::Const([0.0]), background: false, blur: 0.5, background_intensity: Anim::Const([1.0]) }
}

fn environment_map(source: &EnvSource, cx: &mut LoadCx) -> Result<Arc<EnvMap>, Diagnostic> {
    debug_assert!(SCENE_VM_DEPTH.get() == 0, "an environment map made under the document's VM lock");
    match source {
        EnvSource::Preset(preset) => Ok(Arc::new(EnvMap::procedural(preset, 512, 1.0, 0.0))),
        EnvSource::HdriPreset(name) => Ok(hdri_preset_map(name, &generator_preset(name).expect("a resolved generator preset"))),
        EnvSource::Hdri(media) => cx.resolver.load(media).and_then(|bytes| load_hdr_engine(&bytes, &mut cx.budget)).map(Arc::new).map_err(|e| {
            let (loc, path) = cx.hdri_site.take().expect("a parsed HDRI file has a source location");
            Diagnostic::error(loc, path, format!("the HDRI did not load: {e}")).hint("give a Radiance .hdr file (file(\"studio.hdr\") or asset(id, rev)), or use `preset: @studio`")
        }),
    }
}

/// A generator preset's map, baked once per process at
/// `PROCEDURAL_HDRI_WIDTH`: a document loads again on every edit and a
/// weather preset can take minutes to bake. The whole map, key
/// and all, as the generator exports it: Scene3D lights through its
/// environment and its own lights and declares no `Environment.sun`, so the
/// renderer lights with all of it, as it does a `.hdr`.
fn hdri_preset_map(name: &'static str, params: &makepad_hdri_engine::HdriParams) -> Arc<EnvMap> {
    static BAKED: std::sync::Mutex<Option<HashMap<&'static str, Arc<EnvMap>>>> = std::sync::Mutex::new(None);
    static PRESET_BAKE: WorkerOnly = WorkerOnly::new("a generator preset's environment bake");
    if let Some(map) = BAKED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|baked| baked.get(name)) {
        return map.clone();
    }
    PRESET_BAKE.check();
    // Spread over the pool whose worker loads the document (a scene's load
    // job): a weather preset bakes for minutes on one core. Elsewhere (a
    // test, a tool) the bake runs on the caller.
    let pool = makepad_widgets::makepad_platform::thread::TaskPool::of_current_worker();
    let spread = |n: usize, f: &(dyn Fn(usize) + Sync)| match &pool {
        Some(pool) => pool.fan_out(n, f),
        None => (0..n).for_each(f),
    };
    let (map, _key) = makepad_hdri_engine::envmap::bake_env_map(params, makepad_hdri_engine::PROCEDURAL_HDRI_WIDTH, spread);
    // Two loads that baked at once keep the first map, so every document
    // hands the renderer the same one.
    BAKED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).entry(name).or_insert_with(|| Arc::new(map)).clone()
}

/// A Radiance `.hdr` in the engine's convention. Files (Blender, three.js,
/// the HDRI generator's export) put +X at the image centre and the engine
/// puts -Z there (`ibl::dir_to_equirect_uv`), so the columns roll a quarter
/// turn (`hdri::image::roll_quarter`, the generator's own loader's rule) and
/// a map lights and shows from where its author put it. Documents written
/// before this turn a quarter (MOTION3D.md, Environment). The turned copy is
/// decoded data like the file: it is charged to the load's budget while the
/// file is still held.
fn load_hdr_engine(bytes: &[u8], budget: &mut makepad_platform::resource_resolver::DecodeBudget) -> Result<EnvMap, String> {
    let file = makepad_render_material::ibl::load_hdr(bytes, budget)?;
    // What load_hdr keeps charged for the image: 16 bytes a texel.
    let held = file.data.len() as u64 * 16;
    if let Err(e) = budget.charge(held, "the .hdr turned to the engine's convention") {
        budget.release(held);
        return Err(e.to_string());
    }
    let engine = makepad_hdri_engine::image::roll_quarter(&file, false);
    drop(file);
    budget.release(held);
    Ok(engine)
}

fn parse_environment(r: &mut Reader, cx: &mut LoadCx, v: ScriptValue, path: &str) -> Option<EnvironmentDef> {
    let Some(o) = v.as_object().filter(|_| r.kind(v).as_deref() == Some("environment")) else {
        // `environment: @studio` shorthand.
        if let Some(id) = v.as_id() {
            if let Some(source) = environment_source(&id.to_string()) {
                return Some(pending_environment(source));
            }
        }
        let loc = v.as_object().map(|o| r.loc(o)).unwrap_or_default();
        r.error(loc, path, format!("environment must be Environment{{...}}, not {}", r.describe(v)), Some("write `environment: Environment{preset: @studio intensity: 1}`".into()));
        return None;
    };
    r.check_fields(o, path, "Environment", &["preset", "hdri", "src", "intensity", "rotation", "background", "blur", "background_intensity", "top", "horizon", "bottom"]);
    let intensity = r.anim::<1>(o, path, "intensity", [1.0], Semantic::Plain);
    let rotation = r.anim::<1>(o, path, "rotation", [0.0], Semantic::Plain);
    let background = r.bool(o, "background", false);
    let blur = r.f32(o, path, "blur", 0.0).clamp(0.0, 1.0);
    let background_intensity = r.anim::<1>(o, path, "background_intensity", [1.0], Semantic::Plain);
    let hdri = r.media(o, path, "hdri").or_else(|| r.media(o, path, "src"));
    let source = if let Some(m) = hdri {
        cx.hdri_site = Some((r.loc(o), format!("{path}.hdri")));
        EnvSource::Hdri(m)
    } else {
        let name = r.id(o, "preset").unwrap_or_else(|| "studio".into());
        if r.has(o, "top") || name == "gradient" {
            let c = |r: &mut Reader, k: &str, d: [f32; 4]| { let v = r.get(o, k); r.plain::<4>(v, Semantic::Color).unwrap_or(d) };
            let top = c(r, "top", [0.2, 0.3, 0.6, 1.0]);
            let horizon = c(r, "horizon", [0.8, 0.8, 0.9, 1.0]);
            let bottom = c(r, "bottom", [0.1, 0.1, 0.1, 1.0]);
            let preset = EnvPreset::Gradient { top: [top[0], top[1], top[2]], horizon: [horizon[0], horizon[1], horizon[2]], bottom: [bottom[0], bottom[1], bottom[2]] };
            EnvSource::Preset(preset)
        } else {
            match environment_source(&name) {
                Some(found) => found,
                None => {
                    let loc = r.loc(o);
                    let names: Vec<String> = makepad_render::PROCEDURAL_ENVIRONMENTS.iter().chain(makepad_hdri_engine::PRESET_ENVIRONMENTS).map(|n| format!("@{n}")).collect();
                    r.error(loc, &format!("{path}.preset"), format!("no environment preset `@{name}`"), Some(format!("one of {}", names.join(" "))));
                    EnvSource::Preset(EnvPreset::Studio)
                }
            }
        }
    };
    Some(EnvironmentDef { intensity, rotation, background, blur, background_intensity, ..pending_environment(source) })
}

fn parse_fog(r: &mut Reader, v: ScriptValue, path: &str) -> Option<FogDef> {
    let kind = r.kind(v);
    let Some(o) = v.as_object().filter(|_| matches!(kind.as_deref(), Some("fog") | Some("fog_exp2"))) else {
        let loc = v.as_object().map(|o| r.loc(o)).unwrap_or_default();
        r.error(loc, path, format!("fog must be Fog{{color near far}} or FogExp2{{color density}}, not {}", r.describe(v)), None);
        return None;
    };
    let exp = kind.as_deref() == Some("fog_exp2");
    r.check_fields(o, path, if exp { "FogExp2" } else { "Fog" }, &["color", "near", "far", "density"]);
    Some(FogDef {
        color: r.anim::<4>(o, path, "color", [0.5, 0.5, 0.5, 1.0], Semantic::Color),
        near: r.anim::<1>(o, path, "near", [5.0], Semantic::Plain),
        far: r.anim::<1>(o, path, "far", [50.0], Semantic::Plain),
        density: if exp || r.has(o, "density") { Some(r.anim::<1>(o, path, "density", [0.05], Semantic::Plain)) } else { None },
    })
}

fn parse_post(r: &mut Reader, v: ScriptValue, path: &str) -> PostDef {
    let mut post = PostDef::default();
    for (i, item) in r.list(v).into_iter().enumerate() {
        let p = format!("{path}[{i}]");
        let kind = r.kind(item);
        let Some(o) = item.as_object() else { continue };
        // `on: false`: bypassed (an editor's switch), kept as written, not drawn.
        if r.get(o, "on").as_bool() == Some(false) {
            continue;
        }
        match kind.as_deref() {
            Some("bloom") => {
                r.check_fields(o, &p, "Bloom", &["strength", "radius", "threshold"]);
                post.bloom = Some((r.anim::<1>(o, &p, "strength", [0.6], Semantic::Plain), r.anim::<1>(o, &p, "radius", [0.5], Semantic::Plain), r.anim::<1>(o, &p, "threshold", [1.0], Semantic::Plain)));
            }
            Some("ssao") => {
                r.check_fields(o, &p, "SSAO", &["radius", "intensity"]);
                post.ssao = Some((r.anim::<1>(o, &p, "radius", [0.5], Semantic::Plain), r.anim::<1>(o, &p, "intensity", [1.0], Semantic::Plain)));
            }
            Some("dof") => {
                r.check_fields(o, &p, "DepthOfField", &["focus", "aperture", "max_blur", "f_stop"]);
                if r.has(o, "f_stop") {
                    post.f_stop = Some(r.anim::<1>(o, &p, "f_stop", [2.8], Semantic::Plain));
                }
                let fv = r.get(o, "focus");
                let (focus, node) = if fv.is_string_like() {
                    (Anim::Const([5.0]), r.string_of(fv))
                } else {
                    (r.anim::<1>(o, &p, "focus", [5.0], Semantic::Plain), None)
                };
                post.dof = Some((focus, node, r.anim::<1>(o, &p, "aperture", [8.0], Semantic::Plain), r.f32(o, &p, "max_blur", 16.0).clamp(0.0, 64.0)));
            }
            Some("motion_blur") => {
                r.check_fields(o, &p, "MotionBlur", &["shutter", "samples", "tol"]);
                // `samples`: the most sub-frames (`@auto` = 324); the frame
                // adds sub-frames until it converges within `tol` levels.
                let samples = if r.id(o, "samples").as_deref() == Some("auto") { 324 } else { r.u32(o, &p, "samples", 324, 324).max(1) };
                let tol = r.f32(o, &p, "tol", 3.0).clamp(0.25, 64.0);
                post.motion_blur = Some((r.anim::<1>(o, &p, "shutter", [180.0], Semantic::Plain), samples, tol));
                post.velocity_blur = velocity_blur_pass(r);
            }
            Some(kind) if kind == "pass" || GRAPH_KITS.contains(&kind) => {
                for g in parse_graph_item(r, o, item, kind, &p, i) {
                    post.graph.push(g);
                }
            }
            Some("outline") => {
                r.check_fields(o, &p, "Outline", &["color", "thickness"]);
                post.outline = Some((r.anim::<4>(o, &p, "color", [1.0; 4], Semantic::Color), r.anim::<1>(o, &p, "thickness", [2.0], Semantic::Plain)));
            }
            Some("paper") => {
                r.check_fields(o, &p, "Paper", &["amount", "halftone", "cell", "color"]);
                post.paper = Some((
                    r.anim::<1>(o, &p, "amount", [0.6], Semantic::Plain),
                    r.anim::<1>(o, &p, "halftone", [0.35], Semantic::Plain),
                    r.f32(o, &p, "cell", 7.0).max(2.0),
                    r.anim::<4>(o, &p, "color", lin(0xf6ecd4), Semantic::Color),
                ));
            }
            Some("post_pass") => {
                r.check_fields(o, &p, "PostPass", &["code", "uniforms"]);
                // `code: fn(uv: vec2, c: vec3) -> vec3 { ... }`: a shader
                // function in the document, the final pass's `post_<k>`.
                let code_v = r.get(o, "code");
                let Some(code) = makepad_render_graph::script::fn_code(r.vm, code_v) else {
                    let loc = r.loc(o);
                    r.error(loc, &p, "PostPass needs `code: fn(uv: vec2, c: vec3) -> vec3 { ... }`, a shader function", None);
                    continue;
                };
                let mut uniforms = Vec::new();
                let uv = r.get(o, "uniforms");
                if let Some(uo) = uv.as_object() {
                    for name in r.own_fields(uo).into_iter().take(4) {
                        uniforms.push(r.anim::<1>(uo, &format!("{p}.uniforms"), &name, [0.0], Semantic::Plain));
                    }
                }
                if post.passes.len() == 4 {
                    let loc = r.loc(o);
                    r.warn(loc, &p, "at most four PostPass passes; this one is ignored", None);
                } else {
                    post.passes.push(PostPassDef { code, uniforms });
                }
            }
            Some("light_shafts") => {
                r.check_fields(o, &p, "LightShafts", &["position", "node", "strength", "color", "decay", "threshold"]);
                post.shafts = Some(ShaftsDef {
                    position: r.anim::<3>(o, &p, "position", [0.0, 3.0, -5.0], Semantic::Plain),
                    node: r.string(o, "node"),
                    strength: r.anim::<1>(o, &p, "strength", [0.8], Semantic::Plain),
                    color: r.anim::<4>(o, &p, "color", [1.0, 0.92, 0.8, 1.0], Semantic::Color),
                    decay: r.f32(o, &p, "decay", 0.9).clamp(0.5, 1.0),
                    threshold: r.f32(o, &p, "threshold", 0.8).max(0.0),
                });
            }
            Some("zoom_blur") => {
                r.check_fields(o, &p, "ZoomBlur", &["amount"]);
                post.zoom_blur = Some(r.anim::<1>(o, &p, "amount", [0.3], Semantic::Plain));
            }
            Some("vignette") => {
                r.check_fields(o, &p, "Vignette", &["amount", "softness"]);
                post.vignette = Some((r.anim::<1>(o, &p, "amount", [0.3], Semantic::Plain), r.f32(o, &p, "softness", 0.5)));
            }
            Some("grain") => {
                r.check_fields(o, &p, "FilmGrain", &["amount"]);
                post.grain = Some(r.anim::<1>(o, &p, "amount", [0.04], Semantic::Plain));
            }
            Some("grade") => {
                r.check_fields(o, &p, "ColorGrade", &["contrast", "saturation", "temperature", "tint", "lift", "gamma", "gain"]);
                post.grade = Some(GradeDef {
                    contrast: r.anim::<1>(o, &p, "contrast", [1.0], Semantic::Plain),
                    saturation: r.anim::<1>(o, &p, "saturation", [1.0], Semantic::Plain),
                    temperature: r.anim::<1>(o, &p, "temperature", [0.0], Semantic::Plain),
                    tint: r.anim::<1>(o, &p, "tint", [0.0], Semantic::Plain),
                    lift: r.anim::<4>(o, &p, "lift", [0.0, 0.0, 0.0, 1.0], Semantic::Plain),
                    gamma: r.anim::<4>(o, &p, "gamma", [1.0; 4], Semantic::Plain),
                    gain: r.anim::<4>(o, &p, "gain", [1.0; 4], Semantic::Plain),
                });
            }
            Some("contact") => {
                r.check_fields(o, &p, "ContactShadows", &["opacity", "blur", "far", "size"]);
                post.contact = Some(ContactDef {
                    opacity: r.anim::<1>(o, &p, "opacity", [0.8], Semantic::Plain),
                    blur: r.anim::<1>(o, &p, "blur", [0.15], Semantic::Plain),
                    far: r.f32(o, &p, "far", 1.0).max(0.01),
                    size: r.u32(o, &p, "size", 1024, 4096).max(128),
                });
            }
            _ => {
                let loc = r.loc(o);
                let names = names_where(|k| matches!(k, "bloom" | "ssao" | "dof" | "motion_blur" | "zoom_blur" | "light_shafts" | "post_pass" | "paper" | "outline" | "vignette" | "grain" | "grade" | "contact" | "pass") || GRAPH_KITS.contains(&k));
                r.error(loc, &p, format!("{} is not a post effect", r.describe(item)), Some(format!("post effects: {}", names.join(", "))));
            }
        }
    }
    post.graph_decls = Arc::new(post.graph.iter().map(|g| g.decl.clone()).collect());
    post
}

/// The render graph's kits a Scene3D document can use (the graph's other
/// kits share names with this renderer's own nodes: Vignette, FilmGrain,
/// DepthOfField, Outline).
pub const GRAPH_KITS: &[&str] = &["glow", "halation", "aberration", "frame_post", "lut", "upsample", "velocity_blur", "shoulder"];

/// The module that has the kit templates (`kit_glow`): `mod.post`, the
/// render graph's (`None` without it; the caller reports).
fn kit_module(r: &Reader) -> Option<ScriptObject> {
    r.vm.bx.heap_ref().value(r.vm.modules(), id!(post).into(), NoTrap).as_object()
}

/// The realtime form of motion blur: the graph's VelocityBlur kit built by
/// its Splash template (its `amount` is set per frame from the shutter).
fn velocity_blur_pass(r: &mut Reader) -> Option<makepad_render_graph::PassDecl> {
    let module = kit_module(r)?;
    let proto = r.vm.bx.heap_ref().value(module, LiveId::from_str("VelocityBlur").into(), NoTrap).as_object()?;
    let f = r.vm.bx.heap_ref().value(module, LiveId::from_str("kit_velocity_blur").into(), NoTrap);
    let object = r.vm.bx.heap_mut().new_with_proto(proto.into());
    r.vm.capture_errors();
    let built = r.vm.call(f, &[object.into()]);
    let errors = r.vm.take_errors();
    if !errors.is_empty() || built.is_err() {
        return None;
    }
    let item = r.list(built).into_iter().next()?;
    let read = makepad_render_graph::script::read_pass(r.vm, item, "MotionBlur (realtime)").ok()?;
    let mut decl = read.decl;
    for (name, _) in read.uniforms {
        decl.uniforms.push(makepad_render_graph::UniformDecl { name, width: 1 });
    }
    decl.name = None;
    Some(decl)
}

/// The width of what a pass uniform's `fn(t)` gives (1 for a number, 2..4
/// for a vector), from one call at t = 0; None when `v` is no function or
/// the call fails (the frame's own call reports that).
/// `Err` with the call's error when the probe fails: the uniform's width
/// cannot be known, and guessing 1 makes a vector uniform's pass fail to
/// compile with nothing said.
fn fn_uniform_width(r: &mut Reader, v: ScriptValue) -> Result<Option<u8>, String> {
    let Some(f) = v.as_object().filter(|o| r.vm.bx.heap_ref().is_fn(*o)) else { return Ok(None) };
    let mut errors = Vec::new();
    let Some(got) = frame_call(r.vm, &mut errors, f, &[0.0f64.into(), 0.0f64.into()]) else {
        return Err(format!("its fn(t) did not finish at t = 0 (it ran over its limits){}", errors.first().map_or(String::new(), |e| format!(": {e}"))));
    };
    if let Some(e) = errors.first() {
        return Err(format!("its fn(t) failed at t = 0, so its width (a number or a vec2/3/4) is not known: {e}"));
    }
    if got.as_number().is_some() {
        return Ok(Some(1));
    }
    Ok(Some({ let numeric = makepad_widgets::makepad_script::numeric::NumericValue::from_script_value_heap(r.vm.heap(), got, Default::default()); match numeric.kind() {
        makepad_widgets::makepad_script::numeric::NumericValueTag::Vec2 => 2,
        makepad_widgets::makepad_script::numeric::NumericValueTag::Vec3 => 3,
        makepad_widgets::makepad_script::numeric::NumericValueTag::Vec4 => 4,
        _ => 1,
    } }))
}

/// A `Pass{}` or a graph kit in `post`: the passes it makes (read the one
/// way, render_graph::script::read_post_entry: a kit's template, installed
/// into `mod.post`, builds them from the author's object, and their names get
/// the entry's place as a prefix so two kits never collide), their uniforms
/// keyable.
fn parse_graph_item(r: &mut Reader, o: ScriptObject, item: ScriptValue, kind: &str, path: &str, at: usize) -> Vec<GraphPassDef> {
    let type_name = crate::script::type_name_of(kind);
    if kind == "pass" {
        r.check_fields(o, path, "Pass", makepad_render_graph::script::PASS_FIELDS);
    } else {
        let params = makepad_render_graph::kits::kit(kind).map(|k| k.params).unwrap_or(&[]);
        r.check_fields(o, path, type_name, params);
    }
    let Some(module) = kit_module(r) else { return Vec::new() };
    let reads = match makepad_render_graph::script::read_post_entry(r.vm, module, kind, item, path, &format!("p{at}_")) {
        Ok(makepad_render_graph::script::PostEntry::Passes(reads)) => reads,
        Ok(makepad_render_graph::script::PostEntry::Host(_)) => return Vec::new(),
        Err(e) => {
            let loc = r.loc(o);
            let hint = if kind == "pass" { "write `Pass{at: @hdr reads: [@color] pixel: fn() -> vec4 { return self.color.sample(self.uv()) }}`" } else { "check the kit's parameters (numbers and colours; `fn(t)` for keyed values)" };
            r.error(loc, path, e, Some(hint.into()));
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for read in reads {
        let label = read.decl.label.clone();
        let mut decl = read.decl;
        let mut uniforms = Vec::new();
        for (name, value) in read.uniforms {
            let upath = format!("{path}.uniforms.{name}");
            let (width, u) = if value.is_nil() {
                (1, GraphUniform::One(Anim::Const([0.0])))
            } else if value.as_number().is_some() || value.as_bool().is_some() {
                (1, GraphUniform::One(r.anim_of::<1>(value, &upath, Semantic::Plain).unwrap_or(Anim::Const([0.0]))))
            } else if let Some(c) = r.plain::<4>(value, Semantic::Color) {
                let width = { let numeric = makepad_widgets::makepad_script::numeric::NumericValue::from_script_value_heap(r.vm.heap(), value, Default::default()); match numeric.kind() {
                    makepad_widgets::makepad_script::numeric::NumericValueTag::Vec2 => 2,
                    makepad_widgets::makepad_script::numeric::NumericValueTag::Vec3 => 3,
                    _ => 4,
                } };
                (width, GraphUniform::Many(Anim::Const(c)))
            } else if let Some(c) = r.plain::<2>(value, Semantic::Plain) {
                (2, GraphUniform::Many(Anim::Const([c[0], c[1], 0.0, 0.0])))
            } else if let Some(width) = match fn_uniform_width(r, value) {
                Ok(w) => w.filter(|w| *w > 1),
                Err(e) => {
                    let loc = r.loc(o);
                    r.error(loc, &upath, e, None);
                    None
                }
            } {
                // A `fn(t)` giving a vector is a vector uniform (its width
                // read from one call at load); every frame calls it again.
                match r.anim_of::<4>(value, &upath, Semantic::Plain) {
                    Ok(a) => (width, GraphUniform::Many(a)),
                    Err(_) => (1, GraphUniform::One(Anim::Const([0.0]))),
                }
            } else {
                match r.anim_of::<1>(value, &upath, Semantic::Plain) {
                    Ok(a) => (1, GraphUniform::One(a)),
                    Err(_) => match r.anim_of::<4>(value, &upath, Semantic::Color) {
                        Ok(a) => (4, GraphUniform::Many(a)),
                        Err(e) => {
                            let loc = r.loc(o);
                            r.error(loc, &upath, e, None);
                            (1, GraphUniform::One(Anim::Const([0.0])))
                        }
                    },
                }
            };
            decl.uniforms.push(makepad_render_graph::UniformDecl { name, width });
            uniforms.push(u);
        }
        if let Err(e) = decl.validate() {
            let loc = r.loc(o);
            r.error(loc, path, e, None);
            return Vec::new();
        }
        if decl.stage != makepad_render_graph::Stage::Hdr && decl.format() != makepad_render_graph::Format::Rgba8 {
            let loc = r.loc(o);
            r.error(loc, path, format!("{label}: passes after the tone map (@display, @final) write 8-bit display colour"), Some("leave `format` out, or place the pass `at: @hdr`".into()));
            return Vec::new();
        }
        out.push(GraphPassDef { decl, uniforms });
    }
    out
}

fn parse_camera(r: &mut Reader, v: ScriptValue, path: &str) -> CameraDef {
    let kind = r.kind(v);
    let mut cam = CameraDef::default();
    let Some(o) = v.as_object().filter(|_| matches!(kind.as_deref(), Some("camera") | Some("ortho_camera"))) else {
        let loc = v.as_object().map(|o| r.loc(o)).unwrap_or_default();
        r.error(loc, path, format!("camera must be Camera{{...}} or OrthographicCamera{{...}}, not {}", r.describe(v)), Some("write `camera: Camera{position: vec3(0, 2, 8) target: vec3(0, 0, 0) fov: 35}`".into()));
        return cam;
    };
    r.check_fields(o, path, "Camera", &[
        "position", "target", "look_at", "up", "fov", "height", "zoom", "roll", "near", "far", "orbit", "yaw", "pitch", "distance", "path", "along", "shake",
        "shake_frequency", "shake_seed", "type", "focal_length", "sensor_height", "preset", "move", "move_start", "move_end", "move_amount", "move_ease", "moves", "shots",
    ]);
    // Framing presets: an orbit around the target (+ orthographic for
    // isometric/top/front/side); the fields written below override them.
    if let Some(preset) = r.id(o, "preset") {
        let orbit = |yaw: f32, pitch: f32, dist: f32| (Anim::Const([yaw]), Anim::Const([pitch]), Anim::Const([dist]));
        match preset.as_str() {
            "isometric" => {
                cam.orthographic = true;
                cam.orbit = Some(orbit(45.0, 35.264, 20.0));
            }
            "top" => {
                cam.orthographic = true;
                cam.orbit = Some(orbit(0.0, 89.0, 20.0));
            }
            "front" | "side" => {
                cam.orthographic = true;
                cam.orbit = Some(orbit(if preset == "side" { 90.0 } else { 0.0 }, 0.0, 20.0));
            }
            "hero" => cam.orbit = Some(orbit(-30.0, 12.0, 7.0)),
            "low_angle" => cam.orbit = Some(orbit(-20.0, -8.0, 6.0)),
            "overhead" => cam.orbit = Some(orbit(0.0, 70.0, 9.0)),
            "three_quarter" => cam.orbit = Some(orbit(-40.0, 25.0, 8.0)),
            other => {
                let loc = r.loc(o);
                r.warn(loc, &format!("{path}.preset"), format!("no camera preset `@{other}`"), Some("one of @isometric @top @front @side @hero @low_angle @overhead @three_quarter".into()));
            }
        }
    }
    if r.has(o, "focal_length") {
        cam.focal_length = Some(r.anim::<1>(o, path, "focal_length", [50.0], Semantic::Plain));
    }
    cam.sensor = r.f32(o, path, "sensor_height", 24.0).max(1.0);
    cam.orthographic = cam.orthographic || kind.as_deref() == Some("ortho_camera") || r.id(o, "type").as_deref() == Some("orthographic");
    cam.position = r.anim::<3>(o, path, "position", [0.0, 1.5, 6.0], Semantic::Plain);
    let tv = if r.has(o, "target") { r.get(o, "target") } else { r.get(o, "look_at") };
    if tv.is_string_like() {
        cam.aim = CameraAim::Node(r.string_of(tv).unwrap_or_default());
    } else if !tv.is_nil() {
        let name = if r.has(o, "target") { "target" } else { "look_at" };
        cam.aim = CameraAim::Target(r.anim::<3>(o, path, name, [0.0; 3], Semantic::Plain));
    }
    cam.up = r.anim::<3>(o, path, "up", [0.0, 1.0, 0.0], Semantic::Plain);
    cam.fov = r.anim::<1>(o, path, "fov", [35.0], Semantic::Plain);
    cam.height = r.anim::<1>(o, path, "height", [5.0], Semantic::Plain);
    cam.zoom = r.anim::<1>(o, path, "zoom", [1.0], Semantic::Plain);
    cam.roll = r.anim::<1>(o, path, "roll", [0.0], Semantic::Plain);
    cam.near = r.f32(o, path, "near", 0.05).max(1e-4);
    cam.far = r.f32(o, path, "far", 500.0).max(cam.near * 2.0);
    let orbit = r.get(o, "orbit");
    if orbit.as_bool() == Some(true) || r.has(o, "yaw") || r.has(o, "pitch") || r.has(o, "distance") || cam.orbit.is_some() {
        // Written fields override a preset's orbit one by one.
        let (py, pp, pd) = cam.orbit.take().unwrap_or((Anim::Const([0.0]), Anim::Const([15.0]), Anim::Const([6.0])));
        let pick = |r: &mut Reader, name: &str, preset: Anim<1>| if r.has(o, name) { r.anim::<1>(o, path, name, [0.0], Semantic::Plain) } else { preset };
        let yaw = pick(r, "yaw", py);
        let pitch = pick(r, "pitch", pp);
        let distance = pick(r, "distance", pd);
        cam.orbit = Some((yaw, pitch, distance));
    }
    let pv = r.get(o, "path");
    if !pv.is_nil() {
        if let Some(curve) = parse_curve(r, pv, &format!("{path}.path")) {
            let along = r.anim::<1>(o, path, "along", [0.0], Semantic::Plain);
            let aim_along = tv.is_nil();
            cam.path = Some((curve, along, aim_along));
        }
    }
    if r.has(o, "shake") {
        cam.shake = Some((r.anim::<1>(o, path, "shake", [0.0], Semantic::Plain), r.f32(o, path, "shake_frequency", 1.5), r.f32(o, path, "shake_seed", 1.0) as u64));
    }
    // `move: @push_in move_start move_end move_amount move_ease`, or
    // `moves: [CameraMove{type start end amount ease} ...]`.
    if let Some(kind) = r.id(o, "move") {
        let (s, e) = (r.f32(o, path, "move_start", 0.0), r.f32(o, path, "move_end", 4.0));
        let amount = r.number(r.get(o, "move_amount")).map(|a| a as f32);
        let ease = r.get(o, "move_ease");
        if let Some(m) = camera_move(r, o, path, &kind, s, e, amount, ease) {
            cam.moves.push(m);
        }
    }
    // `shots: [CameraShot{at hold position target roll fov}]`: one flight.
    for (i, sv) in r.list(r.get(o, "shots")).into_iter().enumerate() {
        let p = format!("{path}.shots[{i}]");
        let Some(so) = sv.as_object().filter(|_| r.kind(sv).as_deref() == Some("camera_shot")) else {
            let loc = sv.as_object().map(|x| r.loc(x)).unwrap_or_default();
            r.error(loc, &p, format!("expected CameraShot{{at position target roll fov hold}}, got {}", r.describe(sv)), None);
            continue;
        };
        r.check_fields(so, &p, "CameraShot", &["at", "hold", "position", "target", "roll", "fov"]);
        let v3 = |r: &mut Reader, k: &str, d: [f32; 3]| r.plain::<3>(r.get(so, k), Semantic::Plain).unwrap_or(d);
        cam.shots.push(ShotDef {
            at: r.number(r.get(so, "at")).unwrap_or(i as f64 * 2.0) as f32,
            hold: r.number(r.get(so, "hold")).unwrap_or(0.0).max(0.0) as f32,
            position: v3(r, "position", [0.0, 1.5, 6.0]),
            target: v3(r, "target", [0.0; 3]),
            roll: r.f32(so, &p, "roll", 0.0),
            fov: r.f32(so, &p, "fov", 0.0),
        });
    }
    cam.shots.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(std::cmp::Ordering::Equal));
    for (i, mv) in r.list(r.get(o, "moves")).into_iter().enumerate() {
        let p = format!("{path}.moves[{i}]");
        let Some(mo) = mv.as_object().filter(|_| r.kind(mv).as_deref() == Some("camera_move")) else {
            let loc = mv.as_object().map(|x| r.loc(x)).unwrap_or_default();
            r.error(loc, &p, format!("expected CameraMove{{type: @push_in start end amount ease}}, got {}", r.describe(mv)), None);
            continue;
        };
        r.check_fields(mo, &p, "CameraMove", &["type", "start", "end", "amount", "ease"]);
        let kind = r.id(mo, "type").unwrap_or_else(|| "push_in".into());
        let (s, e) = (r.f32(mo, &p, "start", 0.0), r.f32(mo, &p, "end", 4.0));
        let amount = r.number(r.get(mo, "amount")).map(|a| a as f32);
        let ease = r.get(mo, "ease");
        if let Some(m) = camera_move(r, mo, &p, &kind, s, e, amount, ease) {
            cam.moves.push(m);
        }
    }
    cam
}

fn parse_transform(r: &mut Reader, o: ScriptObject, path: &str) -> TransformDef {
    let pos_name = if r.has(o, "translate") && !r.has(o, "position") { "translate" } else { "position" };
    let rot_name = if r.has(o, "rotate") && !r.has(o, "rotation") { "rotate" } else { "rotation" };
    TransformDef {
        position: r.anim::<3>(o, path, pos_name, [0.0; 3], Semantic::Plain),
        rotation: r.anim::<3>(o, path, rot_name, [0.0; 3], Semantic::Plain),
        scale: r.anim::<3>(o, path, "scale", [1.0; 3], Semantic::Plain),
        look_at: if r.has(o, "look_at") && r.id(o, "look_at").as_deref() != Some("camera") { Some(r.anim::<3>(o, path, "look_at", [0.0; 3], Semantic::Plain)) } else { None },
        face_camera: r.id(o, "look_at").as_deref() == Some("camera"),
        pivot: r.anim::<3>(o, path, "pivot", [0.0; 3], Semantic::Plain),
    }
}

pub(crate) fn base_node(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str, kind: NodeKind) -> NodeDef {
    let name = r.string(o, "name");
    if let Some(n) = &name {
        cx.node_names.push(n.clone());
    }
    let span = r.plain::<2>(r.get(o, "span"), Semantic::Plain).map(|s| (s[0], s[1].max(0.0)));
    let mut node = NodeDef {
        name,
        id: 0,
        path: path.to_string(),
        loc: r.loc(o),
        transform: parse_transform(r, o, path),
        visible: r.anim::<1>(o, path, "visible", [1.0], Semantic::Plain),
        opacity: r.anim::<1>(o, path, "opacity", [1.0], Semantic::Plain),
        span,
        cast_shadow: r.bool(o, "cast_shadow", true),
        receive_shadow: r.bool(o, "receive_shadow", true),
        outline: r.bool(o, "outline", false),
        kind,
        children: Vec::new(),
    };
    for (i, v) in r.list(r.get(o, "children")).into_iter().enumerate() {
        if let Some(n) = parse_node(r, cx, v, &format!("{path}.children[{i}]")) {
            node.children.push(n);
        }
    }
    node
}

fn parse_node(r: &mut Reader, cx: &mut LoadCx, v: ScriptValue, path: &str) -> Option<NodeDef> {
    let kind = r.kind(v);
    let Some(o) = v.as_object().filter(|_| kind.is_some()) else {
        let loc = v.as_object().map(|o| r.loc(o)).unwrap_or_default();
        let names = names_where(|k| matches!(k, "group" | "mesh" | "model" | "text3d" | "instances" | "points" | "particles" | "line") || k.ends_with("_light"));
        r.error(loc, path, format!("{} is not a scene node", r.describe(v)), Some(format!("children are {}", names.join(", "))));
        return None;
    };
    let kind = kind.unwrap();
    let node = match kind.as_str() {
        "group" => {
            r.check_fields(o, path, "Group3D", NODE_FIELDS);
            base_node(r, cx, o, path, NodeKind::Group)
        }
        "mesh" => {
            r.check_fields(o, path, "Mesh", &with_node_fields(&["geometry", "material", "displace", "displace_frequency", "displace_speed", "twist", "bend", "ripple", "ripple_frequency", "ripple_speed", "explode", "curl", "curl_radius", "wrap_radius"]));
            let geometry = match parse_geometry(r, cx, r.get(o, "geometry"), &format!("{path}.geometry")) {
                Some(g) => g,
                None => GeometrySrc::Static(SharedMesh::new(BoxGeo::default().build())),
            };
            let mut material = parse_material(r, cx, r.get(o, "material"), &format!("{path}.material"));
            if let GeometrySrc::Static(m) = &geometry {
                if !m.mesh.colors.is_empty() && !r.has(o, "material") {
                    material.vertex_colors = true;
                }
            }
            let deform = parse_deform(r, o, path);
            // Explode needs every triangle on its own.
            let geometry = match geometry {
                GeometrySrc::Static(m) if deform.explode.is_some() => GeometrySrc::Static(SharedMesh::new(explodable(&m.mesh))),
                g => g,
            };
            // Displacement moves split vertices along their own normals and
            // would open cracks at hard edges: weld the normals first.
            let geometry = match geometry {
                GeometrySrc::Static(m) if deform.explode.is_some() => GeometrySrc::Static(m),
                GeometrySrc::Static(m) if deform.displace.is_some() => {
                    let mut mesh = (*m.mesh).clone();
                    welded_normals(&mut mesh);
                    GeometrySrc::Static(SharedMesh::new(mesh))
                }
                g => g,
            };
            base_node(r, cx, o, path, NodeKind::Mesh { geometry, material, deform })
        }
        "model" => return parse_model(r, cx, o, path, false),
        "text3d" => return crate::text3d_node::parse(r, cx, o, path),
        "instances" => return parse_instances(r, cx, o, path),
        "points" => return parse_points(r, cx, o, path, false),
        "particles" => return parse_points(r, cx, o, path, true),
        "line" => return parse_line(r, cx, o, path),
        "light_rig" => return parse_light_rig(r, cx, o, path),
        "device" | "turntable" | "field" | "shape_set" | "fx" | "crowd" | "spotlights" | "callout" => return parse_kit(r, cx, o, path, &kind),
        "ground" => return parse_ground(r, cx, o, path),
        k if k.ends_with("_light") || k == "light3d" => return parse_light(r, cx, o, path, k),
        _ => {
            let loc = r.loc(o);
            r.error(loc, path, format!("{} is not a scene node", r.describe(v)), Some("put geometries inside `Mesh{geometry: ...}` and materials in its `material:`".into()));
            return None;
        }
    };
    Some(node)
}

pub(crate) fn parse_deform(r: &mut Reader, o: ScriptObject, path: &str) -> DeformDef {
    DeformDef {
        displace: if r.has(o, "displace") {
            Some((
                r.anim::<1>(o, path, "displace", [0.0], Semantic::Plain),
                r.anim::<1>(o, path, "displace_frequency", [1.5], Semantic::Plain),
                r.anim::<1>(o, path, "displace_speed", [0.3], Semantic::Plain),
            ))
        } else {
            None
        },
        twist: if r.has(o, "twist") { Some(r.anim::<1>(o, path, "twist", [0.0], Semantic::Plain)) } else { None },
        wrap: if r.has(o, "wrap_radius") { Some(r.anim::<1>(o, path, "wrap_radius", [0.0], Semantic::Plain)) } else { None },
        bend: if r.has(o, "bend") { Some(r.anim::<1>(o, path, "bend", [0.0], Semantic::Plain)) } else { None },
        ripple: if r.has(o, "ripple") {
            Some((
                r.anim::<1>(o, path, "ripple", [0.0], Semantic::Plain),
                r.anim::<1>(o, path, "ripple_frequency", [3.0], Semantic::Plain),
                r.anim::<1>(o, path, "ripple_speed", [1.0], Semantic::Plain),
            ))
        } else {
            None
        },
        explode: if r.has(o, "explode") { Some(r.anim::<1>(o, path, "explode", [0.0], Semantic::Plain)) } else { None },
        curl: if r.has(o, "curl") { Some((r.anim::<1>(o, path, "curl", [0.0], Semantic::Plain), r.f32(o, path, "curl_radius", 0.15).max(0.01))) } else { None },
    }
}

/// Split a mesh per triangle with each vertex's tangent carrying its
/// triangle's centre (w = 2 marks it): what `explode` moves.
pub(crate) fn explodable(mesh: &Mesh) -> Mesh {
    let mut m = mesh.flat_shaded();
    let mut tangents = Vec::with_capacity(m.positions.len());
    for tri in m.positions.chunks_exact(3) {
        let c = [(tri[0][0] + tri[1][0] + tri[2][0]) / 3.0, (tri[0][1] + tri[1][1] + tri[2][1]) / 3.0, (tri[0][2] + tri[1][2] + tri[2][2]) / 3.0];
        tangents.extend_from_slice(&[[c[0], c[1], c[2], 2.0]; 3]);
    }
    m.tangents = tangents;
    m
}

// ---------------------------------------------------------------------------
// Materials
// ---------------------------------------------------------------------------

const MATERIAL_FIELDS: &[&str] = &[
    "name", "color", "opacity", "metalness", "roughness", "emissive", "emissive_intensity", "clearcoat", "clearcoat_roughness", "transmission", "ior",
    "thickness", "attenuation_color", "sheen", "sheen_color", "iridescence", "specular_intensity", "env_intensity", "env_map_intensity", "rim_color",
    "rim_power", "steps", "matcap", "map", "vertex_colors", "flat_shading", "wireframe", "side", "blending", "transparent", "depth_write", "shininess",
    "alpha_test", "uniforms", "base_color", "metallic", "preset", "flakes", "flake_scale", "brushed", "brush_scale", "reflectivity", "reflection_blur",
    "refraction", "fade", "texture", "hooks", "fit", "crop", "aspect", "ink", "ink_color", "wire_fill", "wire_width",
];

pub(crate) fn parse_material(r: &mut Reader, cx: &mut LoadCx, v: ScriptValue, path: &str) -> MaterialDef {
    let mut m = MaterialDef::default();
    if v.is_nil() {
        return m;
    }
    // `material: #ff5a36` shorthand: a standard material of that colour.
    if let Some(c) = r.plain::<4>(v, Semantic::Color) {
        m.color = Anim::Const(c);
        return m;
    }
    let kind = r.kind(v);
    let Some(o) = v.as_object().filter(|_| kind.is_some()) else {
        let loc = v.as_object().map(|o| r.loc(o)).unwrap_or_default();
        let names = names_where(|k| matches!(k, "standard" | "physical" | "basic" | "lambert" | "phong" | "toon" | "matcap" | "normal" | "depth" | "shader"));
        r.error(loc, path, format!("material must be a material object, not {}", r.describe(v)), Some(format!("materials: {}", names.join(", "))));
        return m;
    };
    let kind = kind.unwrap();
    m.loc = r.loc(o);
    m.model = match kind.as_str() {
        "standard" | "physical" | "phong" | "material3d" | "shader" => ShadingModel::Standard,
        "basic" => ShadingModel::Basic,
        "lambert" => ShadingModel::Lambert,
        "toon" => ShadingModel::Toon,
        "matcap" => ShadingModel::Matcap,
        "normal" => ShadingModel::Normal,
        "depth" => ShadingModel::Depth,
        _ => {
            let loc = r.loc(o);
            r.error(loc, path, format!("{} is not a material", r.describe(v)), None);
            return m;
        }
    };
    let type_name = type_name_of(&kind);
    let mut known: Vec<&str> = MATERIAL_FIELDS.to_vec();
    let hooks = if kind == "shader" || r.has(o, "hooks") {
        // The hook functions, as written, for the GPU to compile.
        let (fields, origins) = material_hooks(r, cx, o);
        for (name, _) in &fields {
            known.push(Box::leak(name.clone().into_boxed_str()));
        }
        Some((fields, origins))
    } else {
        None
    };
    r.check_fields(o, path, type_name, &known);
    apply_material_fields(r, cx, o, path, &mut m, None);
    if let Some((fields, origins)) = hooks {
        // The text, and where it is written when that names its literals.
        let mut key = makepad_geom3d::math::hash64(fields.iter().flat_map(|(a, b)| a.bytes().chain(b.bytes())).fold(0u64, |h, b| h.wrapping_mul(0x100_0000_01b3) ^ b as u64));
        for o in &origins {
            key = makepad_geom3d::math::hash64(key ^ ((o.line as u64) << 32 | o.col as u64));
        }
        if fields.is_empty() {
            let loc = r.loc(o);
            r.warn(loc, path, "ShaderMaterial has no hook functions; it draws as a standard material", Some("add `surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 {...}`".into()));
        }
        m.shader = Some(ShaderHooks { fields, key, origins });
        let uv = r.get(o, "uniforms");
        if let Some(uo) = uv.as_object() {
            for name in r.own_fields(uo) {
                if m.uniforms.len() == 8 {
                    let loc = r.loc(uo);
                    r.warn(loc, &format!("{path}.uniforms"), format!("at most eight uniforms (self.u_custom.xyzw, self.u_custom2.xyzw); `{name}` is ignored"), None);
                    break;
                }
                let a = r.anim::<1>(uo, &format!("{path}.uniforms"), &name, [0.0], Semantic::Plain);
                m.uniforms.push((name, a));
            }
        }
    }
    m
}

/// The hook functions of a ShaderMaterial, as `name: fn(..) {..}` text for
/// the GPU to compile: the functions of its `hooks: {..}` object (a kit's
/// set, which may derive from another), or the hook functions written on
/// the material itself.
fn material_hooks(r: &mut Reader, _cx: &LoadCx, o: ScriptObject) -> (Vec<(String, String)>, Vec<crate::scene::HookOrigin>) {
    let loc = r.loc(o);
    let mut origins = Vec::new();
    // Where a function's text is written in the document (edit mode's
    // live literals name its literals by it): its `fn`, zero-based.
    let origin = |r: &Reader, field: &str, at: &makepad_render_graph::pass::CodeAt| -> Option<crate::scene::HookOrigin> {
        let (file, shift) = r.origin_file.clone()?;
        let line = at.line.checked_sub(r.line_offset)? + shift;
        // (A document's first line carries its VM's `use` prefix.)
        let col = if at.line == 0 { at.col.saturating_sub(r.col_shift) } else { at.col };
        Some(crate::scene::HookOrigin { field: field.to_string(), file, line, col })
    };
    let hooks_v = r.get(o, "hooks");
    if !hooks_v.is_nil() && !hooks_v.is_err() {
        return match makepad_render_graph::script::helper_fns(r.vm, hooks_v) {
            Ok(fns) => {
                let fields = fns.iter().map(|(name, _, text)| (name.clone(), format!("{name}: {text}"))).collect();
                origins.extend(fns.iter().filter_map(|(name, at, _)| origin(r, name, at)));
                (fields, origins)
            }
            Err(e) => {
                r.error(loc, "hooks", e.replace("`helpers`", "`hooks`").replace("helpers: ", "hooks: "), None);
                (Vec::new(), origins)
            }
        };
    }
    const HOOKS: &[&str] = &["surface", "vertex_displace", "emissive_fn"];
    let mut out = Vec::new();
    for name in r.own_fields(o) {
        let v = r.get(o, &name);
        let Some((at, text)) = makepad_render_graph::script::fn_text(r.vm, v) else { continue };
        if HOOKS.contains(&name.as_str()) {
            out.push((name.clone(), format!("{name}: {text}")));
            origins.extend(origin(r, &name, &at));
        } else {
            r.warn(loc, "ShaderMaterial", format!("`{name}` is not a hook"), Some(format!("hooks: {}", HOOKS.join(", "))));
        }
    }
    (out, origins)
}

/// Read material fields onto `m`; with `set`, record which were written.
fn apply_material_fields(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str, m: &mut MaterialDef, mut set: Option<&mut Vec<String>>) {
    macro_rules! field {
        ($name:literal, $slot:expr, $n:literal, $sem:expr) => {
            if r.has(o, $name) {
                let d = [0.0f32; $n];
                $slot = r.anim::<$n>(o, path, $name, d, $sem);
                if let Some(s) = set.as_deref_mut() {
                    s.push($name.to_string());
                }
            }
        };
    }
    m.name = r.string(o, "name").or(m.name.take());
    if let Some(preset) = r.id(o, "preset") {
        if let Some(sl) = set.as_deref_mut() {
            sl.push("preset".to_string());
        }
        if !apply_preset(m, &preset) {
            let loc = r.loc(o);
            r.warn(loc, &format!("{path}.preset"), format!("no material preset `@{preset}`"), Some(format!("one of {}", MATERIAL_PRESETS.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
        }
    }
    field!("flakes", m.flakes, 1, Semantic::Plain);
    field!("brushed", m.brushed, 1, Semantic::Plain);
    field!("reflectivity", m.reflectivity, 1, Semantic::Plain);
    field!("reflection_blur", m.reflection_blur, 1, Semantic::Plain);
    if r.has(o, "flake_scale") {
        m.flake_scale = r.f32(o, path, "flake_scale", 400.0).max(1.0);
    }
    if r.has(o, "brush_scale") {
        m.brush_scale = r.f32(o, path, "brush_scale", 300.0).max(1.0);
    }
    if let Some(mode) = r.id(o, "refraction") {
        m.refraction_env = mode == "environment";
    }
    if let Some(name) = r.string(o, "texture") {
        m.external = Some(name);
        m.map_space = MapSpace::Picture;
    }
    if let Some(fit) = r.id(o, "fit") {
        match MapFit::by_name(&fit) {
            Some(f) => m.fit = f,
            None => {
                let loc = r.loc(o);
                r.warn(loc, &format!("{path}.fit"), format!("no fit `@{fit}`"), Some("@cover (crop to the surface), @contain (letterbox) or @fill (stretch)".into()));
            }
        }
    }
    if r.has(o, "crop") && r.id(o, "crop").as_deref() != Some("none") {
        m.crop = Some(r.anim::<4>(o, path, "crop", [0.0, 0.0, 0.0, 0.0], Semantic::Plain));
    }
    if r.has(o, "aspect") {
        m.aspect = r.f32(o, path, "aspect", 0.0).max(0.0);
    }
    if r.has(o, "wire_fill") {
        m.wire_fill = Some(r.anim::<4>(o, path, "wire_fill", [0.0, 0.0, 0.0, 1.0], Semantic::Color));
    }
    if r.has(o, "wire_width") {
        m.wire_width = r.f32(o, path, "wire_width", 1.0).max(0.1);
    }
    if r.has(o, "ink") {
        m.ink = r.anim::<1>(o, path, "ink", [0.0], Semantic::Plain);
    }
    if r.has(o, "ink_color") {
        m.ink_color = r.anim::<4>(o, path, "ink_color", [0.0, 0.0, 0.0, 1.0], Semantic::Color);
    }
    if let Some(f) = r.plain::<2>(r.get(o, "fade"), Semantic::Plain) {
        m.fade = Some((f[0], f[1].max(f[0] + 1e-3)));
    }
    field!("color", m.color, 4, Semantic::Color);
    field!("base_color", m.color, 4, Semantic::Color);
    field!("opacity", m.opacity, 1, Semantic::Plain);
    field!("metalness", m.metalness, 1, Semantic::Plain);
    field!("metallic", m.metalness, 1, Semantic::Plain);
    field!("roughness", m.roughness, 1, Semantic::Plain);
    field!("emissive", m.emissive, 4, Semantic::Color);
    field!("emissive_intensity", m.emissive_intensity, 1, Semantic::Plain);
    field!("clearcoat", m.clearcoat, 1, Semantic::Plain);
    field!("clearcoat_roughness", m.clearcoat_roughness, 1, Semantic::Plain);
    field!("transmission", m.transmission, 1, Semantic::Plain);
    field!("ior", m.ior, 1, Semantic::Plain);
    field!("thickness", m.thickness, 1, Semantic::Plain);
    field!("attenuation_color", m.attenuation_color, 4, Semantic::Color);
    field!("sheen", m.sheen, 1, Semantic::Plain);
    field!("sheen_color", m.sheen_color, 4, Semantic::Color);
    field!("iridescence", m.iridescence, 1, Semantic::Plain);
    field!("specular_intensity", m.specular_intensity, 1, Semantic::Plain);
    field!("env_intensity", m.env_intensity, 1, Semantic::Plain);
    field!("env_map_intensity", m.env_intensity, 1, Semantic::Plain);
    field!("rim_color", m.rim_color, 4, Semantic::Color);
    field!("rim_power", m.rim_power, 1, Semantic::Plain);
    if r.has(o, "shininess") {
        // Phong shininess → roughness (Blinn-Phong exponent to GGX alpha).
        let s = r.f32(o, path, "shininess", 30.0).max(0.0);
        m.roughness = Anim::Const([(2.0 / (s + 2.0)).sqrt().powf(0.5).clamp(0.02, 1.0)]);
    }
    if r.has(o, "steps") {
        m.steps = r.u32(o, path, "steps", 3, 16).max(2);
    }
    if let Some(name) = r.id(o, "matcap") {
        m.model = ShadingModel::Matcap;
        m.matcap = MatcapPreset::by_name(&name).unwrap_or_else(|| {
            let loc = r.loc(o);
            r.warn(loc, &format!("{path}.matcap"), format!("no matcap `@{name}`, using @clay"), Some(format!("one of {}", MatcapPreset::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
            MatcapPreset::Clay
        });
    }
    if let Some(media) = r.media(o, path, "map") {
        match cx.resolver.load(&media).and_then(|b| ImageData::decode(&b, true)) {
            Ok(img) => {
                m.maps.base = Some(img);
                m.map_space = MapSpace::Picture;
            }
            Err(e) => {
                let loc = r.loc(o);
                r.error(loc, &format!("{path}.map"), format!("the image did not load: {e}"), Some("give a PNG or JPEG: map: file(\"wood.jpg\") or asset(id, rev)".into()));
            }
        }
    }
    m.vertex_colors = r.bool(o, "vertex_colors", m.vertex_colors);
    m.flat_shading = r.bool(o, "flat_shading", m.flat_shading);
    m.wireframe = r.bool(o, "wireframe", m.wireframe);
    if let Some(side) = r.id(o, "side") {
        m.side = match side.as_str() {
            "front" => FaceSide::Front,
            "back" => FaceSide::Back,
            "double" | "both" => FaceSide::Double,
            other => {
                let loc = r.loc(o);
                r.warn(loc, &format!("{path}.side"), format!("side `@{other}` is not @front, @back or @double"), None);
                m.side
            }
        };
    }
    if let Some(b) = r.id(o, "blending") {
        m.blending = match b.as_str() {
            "normal" => Blending::Normal,
            "additive" | "add" => Blending::Additive,
            "multiply" => Blending::Multiply,
            other => {
                let loc = r.loc(o);
                r.warn(loc, &format!("{path}.blending"), format!("blending `@{other}` is not @normal, @additive or @multiply"), None);
                m.blending
            }
        };
    }
    m.transparent = r.bool(o, "transparent", m.transparent);
    m.depth_write = r.bool(o, "depth_write", m.depth_write);
    if r.has(o, "alpha_test") {
        m.alpha_cutoff = Some(r.f32(o, path, "alpha_test", 0.5));
    }
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

fn parse_geometry(r: &mut Reader, cx: &mut LoadCx, v: ScriptValue, path: &str) -> Option<GeometrySrc> {
    if v.is_nil() {
        let loc = Loc::default();
        r.warn(loc, path, "a Mesh without geometry draws a unit box", Some("add `geometry: BoxGeometry{width: 1 height: 1 depth: 1}`".into()));
        return None;
    }
    let kind = r.kind(v);
    let Some(o) = v.as_object().filter(|_| kind.is_some()) else {
        let loc = v.as_object().map(|o| r.loc(o)).unwrap_or_default();
        r.error(loc, path, format!("geometry must be a geometry object, not {}", r.describe(v)), Some(format!("geometries: {}", geometry_names().join(", "))));
        return None;
    };
    let kind = kind.unwrap();
    let modifier_fields = ["noise", "noise_frequency", "noise_octaves", "noise_seed", "twist", "bend", "taper", "subdivide", "flat"];
    macro_rules! fields {
        ($name:literal, [$($f:literal),*]) => {{
            let mut k: Vec<&str> = vec![$($f),*];
            k.extend_from_slice(&modifier_fields);
            r.check_fields(o, path, $name, &k);
        }};
    }
    let f = |r: &mut Reader, name: &str, d: f32| r.f32(o, path, name, d);
    let u = |r: &mut Reader, name: &str, d: u32| r.u32(o, path, name, d, 4096);
    let mesh: Mesh = match kind.as_str() {
        "box" => {
            fields!("BoxGeometry", ["width", "height", "depth", "size", "width_segments", "height_segments", "depth_segments", "radius", "radius_segments"]);
            let size = r.plain::<3>(r.get(o, "size"), Semantic::Plain).unwrap_or([f(r, "width", 1.0), f(r, "height", 1.0), f(r, "depth", 1.0)]);
            BoxGeo { size, segments: [u(r, "width_segments", 1), u(r, "height_segments", 1), u(r, "depth_segments", 1)], radius: f(r, "radius", 0.0), radius_segments: u(r, "radius_segments", 4) }.build()
        }
        "sphere" => {
            fields!("SphereGeometry", ["radius", "width_segments", "height_segments", "phi_start", "phi_length", "theta_start", "theta_length"]);
            let d = SphereGeo::default();
            SphereGeo {
                radius: f(r, "radius", 1.0),
                width_segments: u(r, "width_segments", 48),
                height_segments: u(r, "height_segments", 32),
                phi_start: f(r, "phi_start", d.phi_start),
                phi_length: f(r, "phi_length", d.phi_length),
                theta_start: f(r, "theta_start", d.theta_start),
                theta_length: f(r, "theta_length", d.theta_length),
            }
            .build()
        }
        "torus" => {
            fields!("TorusGeometry", ["radius", "tube", "radial_segments", "tubular_segments", "arc"]);
            TorusGeo { radius: f(r, "radius", 1.0), tube: f(r, "tube", 0.4), radial_segments: u(r, "radial_segments", 24), tubular_segments: u(r, "tubular_segments", 96), arc: f(r, "arc", 360.0) }.build()
        }
        "torus_knot" => {
            fields!("TorusKnotGeometry", ["radius", "tube", "tubular_segments", "radial_segments", "p", "q"]);
            TorusKnotGeo { radius: f(r, "radius", 1.0), tube: f(r, "tube", 0.3), tubular_segments: u(r, "tubular_segments", 256), radial_segments: u(r, "radial_segments", 24), p: u(r, "p", 2), q: u(r, "q", 3) }.build()
        }
        "cylinder" | "cone" => {
            let cone = kind == "cone";
            if cone {
                fields!("ConeGeometry", ["radius", "height", "radial_segments", "height_segments", "open_ended", "theta_start", "theta_length"]);
            } else {
                fields!("CylinderGeometry", ["radius_top", "radius_bottom", "radius", "height", "radial_segments", "height_segments", "open_ended", "theta_start", "theta_length"]);
            }
            let radius = f(r, "radius", 1.0);
            CylinderGeo {
                radius_top: if cone { 0.0 } else { f(r, "radius_top", radius) },
                radius_bottom: if cone { radius } else { f(r, "radius_bottom", radius) },
                height: f(r, "height", 1.0),
                radial_segments: u(r, "radial_segments", 48),
                height_segments: u(r, "height_segments", 1),
                open_ended: r.bool(o, "open_ended", false),
                theta_start: f(r, "theta_start", 0.0),
                theta_length: f(r, "theta_length", 360.0),
            }
            .build()
        }
        "plane" => {
            fields!("PlaneGeometry", ["width", "height", "width_segments", "height_segments"]);
            PlaneGeo { width: f(r, "width", 1.0), height: f(r, "height", 1.0), width_segments: u(r, "width_segments", 1), height_segments: u(r, "height_segments", 1) }.build()
        }
        "circle" => {
            fields!("CircleGeometry", ["radius", "segments", "theta_start", "theta_length"]);
            CircleGeo { radius: f(r, "radius", 1.0), segments: u(r, "segments", 48), theta_start: f(r, "theta_start", 0.0), theta_length: f(r, "theta_length", 360.0) }.build()
        }
        "ring" => {
            fields!("RingGeometry", ["inner_radius", "outer_radius", "theta_segments", "phi_segments", "theta_start", "theta_length"]);
            RingGeo {
                inner: f(r, "inner_radius", 0.5),
                outer: f(r, "outer_radius", 1.0),
                theta_segments: u(r, "theta_segments", 48),
                phi_segments: u(r, "phi_segments", 1),
                theta_start: f(r, "theta_start", 0.0),
                theta_length: f(r, "theta_length", 360.0),
            }
            .build()
        }
        "capsule" => {
            fields!("CapsuleGeometry", ["radius", "length", "cap_segments", "radial_segments"]);
            CapsuleGeo { radius: f(r, "radius", 0.5), length: f(r, "length", 1.0), cap_segments: u(r, "cap_segments", 8), radial_segments: u(r, "radial_segments", 24) }.build()
        }
        "tetrahedron" | "octahedron" | "icosahedron" | "dodecahedron" => {
            fields!("PolyhedronGeometry", ["radius", "detail"]);
            let pk = match kind.as_str() {
                "tetrahedron" => PolyKind::Tetra,
                "octahedron" => PolyKind::Octa,
                "icosahedron" => PolyKind::Icosa,
                _ => PolyKind::Dodeca,
            };
            PolyhedronGeo { kind: pk, radius: f(r, "radius", 1.0), detail: r.u32(o, path, "detail", 0, 6) }.build()
        }
        "shape_geo" => {
            fields!("ShapeGeometry", ["shape", "shapes"]);
            let shape = shapes_of(r, o, path)?;
            ShapeGeo { shape }.build()
        }
        "extrude" => {
            fields!("ExtrudeGeometry", ["shape", "shapes", "depth", "bevel_size", "bevel_thickness", "bevel_segments", "steps", "center", "smooth_angle"]);
            let shape = shapes_of(r, o, path)?;
            let d = ExtrudeGeo::default();
            ExtrudeGeo {
                shape,
                depth: f(r, "depth", d.depth),
                bevel_size: f(r, "bevel_size", d.bevel_size),
                bevel_thickness: f(r, "bevel_thickness", d.bevel_thickness),
                bevel_segments: u(r, "bevel_segments", d.bevel_segments),
                steps: u(r, "steps", d.steps),
                center: r.bool(o, "center", true),
                smooth_angle: f(r, "smooth_angle", d.smooth_angle),
            }
            .build()
        }
        "svg_geo" => {
            fields!("SvgGeometry", ["svg", "src", "path", "depth", "bevel_size", "bevel_thickness", "bevel_segments", "size", "center", "tolerance"]);
            let tolerance = f(r, "tolerance", 0.25);
            let text = if let Some(s) = r.string(o, "svg") {
                s
            } else if let Some(m) = r.media(o, path, "src") {
                match cx.resolver.load(&m) {
                    Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                    Err(e) => {
                        let loc = r.loc(o);
                        r.error(loc, &format!("{path}.src"), format!("the SVG did not load: {e}"), None);
                        return None;
                    }
                }
            } else if let Some(d) = r.string(o, "path") {
                format!("<svg xmlns=\"http://www.w3.org/2000/svg\"><path d=\"{d}\" fill=\"#ffffff\"/></svg>")
            } else {
                let loc = r.loc(o);
                r.error(loc, path, "SvgGeometry needs `svg: \"<svg ...>\"`, `src: file(...)` or `path: \"M ...\"`", None);
                return None;
            };
            let shapes = svg_document_shapes(&text, tolerance);
            if shapes.is_empty() {
                let loc = r.loc(o);
                r.error(loc, path, "the SVG has no filled shapes to extrude", Some("give its paths a fill".into()));
                return None;
            }
            // Fit into `size` world units (the larger side), centred.
            let mut all = Shape2D::default();
            for (_, s, _) in &shapes {
                all.extend(s);
            }
            let (mn, mx) = all.bounds().unwrap_or(([0.0, 0.0], [1.0, 1.0]));
            let span = (mx[0] - mn[0]).max(mx[1] - mn[1]).max(1e-6);
            let size = f(r, "size", 2.0);
            let k = size / span;
            let (cx0, cy0) = ((mn[0] + mx[0]) * 0.5, (mn[1] + mx[1]) * 0.5);
            let depth = f(r, "depth", 0.2);
            let d = ExtrudeGeo::default();
            let (bs, bt, bseg) = (f(r, "bevel_size", 0.0), f(r, "bevel_thickness", 0.0), u(r, "bevel_segments", d.bevel_segments));
            let mut out = Mesh::new();
            for (_, s, color) in shapes {
                let mut s = s.clone();
                s.translate(-cx0, -cy0);
                s.scale(k, k);
                let mut m = ExtrudeGeo { shape: s, depth, bevel_size: bs, bevel_thickness: bt, bevel_segments: bseg, center: true, ..d.clone() }.build();
                let lin = [srgb_to_linear(color[0]), srgb_to_linear(color[1]), srgb_to_linear(color[2]), color[3]];
                m.colors = vec![lin; m.vertex_count()];
                out.append(&m);
            }
            out
        }
        "lathe" => {
            fields!("LatheGeometry", ["points", "segments", "phi_start", "phi_length"]);
            let pts: Vec<[f32; 2]> = r.list(r.get(o, "points")).into_iter().filter_map(|p| r.plain::<2>(p, Semantic::Plain)).collect();
            if pts.len() < 2 {
                let loc = r.loc(o);
                r.error(loc, &format!("{path}.points"), "LatheGeometry needs at least two vec2(radius, y) points", Some("points: [vec2(0, -1), vec2(0.8, -0.5), vec2(0.6, 1)]".into()));
                return None;
            }
            LatheGeo { points: pts, segments: u(r, "segments", 48), phi_start: f(r, "phi_start", 0.0), phi_length: f(r, "phi_length", 360.0) }.build()
        }
        "tube" => {
            fields!("TubeGeometry", ["path", "tubular_segments", "radius", "radial_segments", "closed", "radius_end", "caps", "draw_start", "draw_end"]);
            let pv = r.get(o, "path");
            let curve = parse_curve(r, pv, &format!("{path}.path"))?;
            let tube = TubeGeo {
                curve,
                tubular_segments: u(r, "tubular_segments", 128),
                radius: f(r, "radius", 0.1),
                radial_segments: u(r, "radial_segments", 12),
                closed: if r.has(o, "closed") { Some(r.bool(o, "closed", false)) } else { None },
                radius_end: f(r, "radius_end", 0.0),
                caps: r.bool(o, "caps", true),
            };
            if r.has(o, "draw_start") || r.has(o, "draw_end") {
                let start = r.anim::<1>(o, path, "draw_start", [0.0], Semantic::Plain);
                let end = r.anim::<1>(o, path, "draw_end", [1.0], Semantic::Plain);
                if !(start.is_static() && end.is_static()) {
                    return Some(GeometrySrc::Tube { tube, start, end });
                }
            }
            tube.build()
        }
        "parametric" => {
            fields!("ParametricGeometry", ["func", "slices", "stacks", "amp", "freq", "width", "amount", "seed"]);
            let slices = u(r, "slices", 64);
            let stacks = u(r, "stacks", 64);
            if let Some(fi) = r.closure(o, "func") {
                // A Splash `fn(u, v) { vec3(...) }`, run once per vertex at load.
                let f = r.closures[fi as usize].as_object();
                let vm = std::cell::RefCell::new(&mut *r.vm);
                let failed = std::cell::Cell::new(false);
                let mesh = makepad_geom3d::parametric(
                    &|a, b| {
                        let mut vm = vm.borrow_mut();
                        vm_call(&mut vm, f, a, b).unwrap_or_else(|| {
                            failed.set(true);
                            [0.0; 3]
                        })
                    },
                    slices,
                    stacks,
                );
                if failed.get() {
                    let loc = r.loc(o);
                    r.error(loc, &format!("{path}.func"), "`func: fn(u, v)` must return vec3(x, y, z) for every u, v in 0..1", None);
                    return None;
                }
                mesh
            } else {
                let name = r.id(o, "func").unwrap_or_else(|| "klein".into());
                let func = match name.as_str() {
                    "plane" => Parametric::Plane,
                    "sphere" => Parametric::Sphere,
                    "klein" => Parametric::Klein,
                    "mobius" => Parametric::Mobius { width: f(r, "width", 0.5) },
                    "wave" => Parametric::Wave { amp: f(r, "amp", 0.2), freq: f(r, "freq", 2.0) },
                    "blob" => Parametric::Blob { amount: f(r, "amount", 0.3), freq: f(r, "freq", 1.5), seed: f(r, "seed", 1.0) as u64 },
                    "helicoid" => Parametric::Helicoid,
                    other => {
                        let loc = r.loc(o);
                        r.error(loc, &format!("{path}.func"), format!("no built-in surface `@{other}`"), Some("one of @plane @sphere @klein @mobius @wave @blob @helicoid, or `func: fn(u, v) { vec3(...) }`".into()));
                        return None;
                    }
                };
                ParametricGeo { func, slices, stacks }.build()
            }
        }
        "engine_geo" => {
            fields!("EngineGeometry", ["engine", "seed", "count", "size", "iterations", "speed", "width"]);
            let name = r.id(o, "engine").unwrap_or_else(|| "tree".into());
            let Some(kind) = crate::engines::EngineKind::by_name(&name) else {
                let loc = r.loc(o);
                r.error(loc, &format!("{path}.engine"), format!("no engine `@{name}`"), Some(format!("one of {}", crate::engines::EngineKind::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
                return None;
            };
            let d = crate::engines::EngineParams::default();
            let p = crate::engines::EngineParams {
                kind,
                seed: f(r, "seed", 1.0) as u64,
                count: u(r, "count", if kind == crate::engines::EngineKind::Tree { 1 } else { 6 }) as usize,
                size: f(r, "size", d.size),
                iterations: u(r, "iterations", d.iterations as u32) as usize,
                speed: f(r, "speed", d.speed),
                width: f(r, "width", d.width),
            };
            if kind.animated() {
                return Some(GeometrySrc::Engine(p));
            }
            crate::engines::build(&p, 0.0)
        }
        "ribbon_geo" => {
            fields!("RibbonGeometry", ["path", "width", "segments", "twist", "width_end"]);
            let pv = r.get(o, "path");
            let curve = parse_curve(r, pv, &format!("{path}.path"))?;
            ribbon_mesh(&curve, f(r, "width", 0.2), f(r, "width_end", -1.0), u(r, "segments", 200).max(2), f(r, "twist", 0.0))
        }
        "field_geo" => {
            // `FieldGeometry{field: fn(x, z) { height } size: vec2 segments: vec2}`
            // or `heights: [...] cols rows`: a heightfield from a function or
            // data, normals from its slopes.
            fields!("FieldGeometry", ["field", "heights", "cols", "rows", "size", "segments"]);
            let size = r.plain::<2>(r.get(o, "size"), Semantic::Plain).unwrap_or([10.0, 10.0]);
            let seg = r.plain::<2>(r.get(o, "segments"), Semantic::Plain).unwrap_or([128.0, 128.0]);
            let (nx, nz) = if r.has(o, "heights") {
                (r.u32(o, path, "cols", 2, 4096).max(2), r.u32(o, path, "rows", 2, 4096).max(2))
            } else {
                ((seg[0].clamp(1.0, 2048.0) as u32) + 1, (seg[1].clamp(1.0, 2048.0) as u32) + 1)
            };
            let mut h = vec![0.0f32; (nx * nz) as usize];
            if r.has(o, "heights") {
                let vals: Vec<f32> = r.list(r.get(o, "heights")).into_iter().filter_map(|v| r.number(v).map(|x| x as f32)).collect();
                for (i, v) in vals.into_iter().take(h.len()).enumerate() {
                    h[i] = v;
                }
            } else {
                let fv = r.get(o, "field");
                let Some(fo) = fv.as_object().filter(|x| r.vm.bx.heap_ref().is_fn(*x)) else {
                    let loc = r.loc(o);
                    r.error(loc, &format!("{path}.field"), "FieldGeometry needs `field: fn(x, z) { return height }` or `heights: [...]`", None);
                    return None;
                };
                // The field fn is compiled as a vertex kernel (one height per
                // vertex, native and on every core), from the fn object.
                let floc = r.fn_loc(fo).unwrap_or_else(|| r.loc(o));
                let compiled = match crate::script::host(r.vm).and_then(|h| h.kernel_entries.clone()) {
                    Some(entries) => crate::objects::field_kernel(r.vm, &entries, fo).map_err(|errors| {
                        errors
                            .into_iter()
                            .map(|e| {
                                let loc = if e.file.starts_with("motion3d://") { Loc { line: e.line.saturating_sub(r.line_offset).max(1), col: e.col } } else { floc };
                                Diagnostic::error(loc, &format!("{path}.field"), e.message)
                            })
                            .collect::<Vec<_>>()
                    }),
                    None => Err(vec![Diagnostic::error(floc, &format!("{path}.field"), "FieldGeometry's field: no kernel host in this VM")]),
                };
                let k = match compiled {
                    Ok(k) => k,
                    Err(diags) => {
                        for mut d in diags {
                            d.path = format!("{path}.field");
                            d.message = format!("{} (a field runs as a kernel: numbers, vec2/3/4, let/var, if, for and helper functions)", d.message);
                            r.diagnostics.push(d);
                        }
                        return None;
                    }
                };
                let mut c = k.call();
                for (n, v) in [("nx", nx as f32), ("nz", nz as f32), ("sx", size[0]), ("sz", size[1])] {
                    c.set_param(n, v);
                }
                let n = h.len();
                let run = c.output("h", &mut h).and_then(|_| if k.parallel_safe() { c.run_parallel(n, crate::kernels::threads()) } else { c.run(n) });
                if let Err(e) = run {
                    r.error(floc, &format!("{path}.field"), format!("kernel: {e}"), None);
                    return None;
                }
            }
            makepad_geom3d::grid_mesh(&h, nx as usize, nz as usize, size)
        }
        "heightfield" => {
            fields!("HeightfieldGeometry", ["width", "depth", "segments", "height", "frequency", "octaves", "seed"]);
            let d = HeightfieldGeo::default();
            HeightfieldGeo {
                width: f(r, "width", d.width),
                depth: f(r, "depth", d.depth),
                segments: u(r, "segments", d.segments),
                height: f(r, "height", d.height),
                frequency: f(r, "frequency", d.frequency),
                octaves: u(r, "octaves", d.octaves),
                seed: f(r, "seed", d.seed as f32) as u64,
            }
            .build()
        }
        _ => {
            let loc = r.loc(o);
            r.error(loc, path, format!("{} is not a geometry", r.describe(v)), Some(format!("geometries: {}", geometry_names().join(", "))));
            return None;
        }
    };
    // Static modifiers, in a fixed order.
    let mut mesh = mesh;
    if r.has(o, "subdivide") {
        for _ in 0..r.u32(o, path, "subdivide", 1, 4) {
            mesh = subdivide(&mesh);
        }
    }
    if r.has(o, "noise") {
        let amount = f(r, "noise", 0.1);
        let freq = f(r, "noise_frequency", 1.5);
        let oct = r.u32(o, path, "noise_octaves", 3, 8);
        let seed = f(r, "noise_seed", 1.0) as u64;
        displace_noise(&mut mesh, amount, freq, oct, seed, [0.0; 3]);
    }
    if r.has(o, "twist") {
        twist(&mut mesh, 1, f(r, "twist", 0.0));
    }
    if r.has(o, "bend") {
        bend(&mut mesh, 1, f(r, "bend", 0.0));
    }
    if r.has(o, "taper") {
        taper(&mut mesh, 1, f(r, "taper", 0.0));
    }
    if r.bool(o, "flat", false) {
        mesh = mesh.flat_shaded();
    }
    if let Err(e) = mesh.validate() {
        let loc = r.loc(o);
        r.error(loc, path, format!("the geometry came out invalid: {e}"), Some("check its parameters (zero or negative sizes?)".into()));
        return None;
    }
    if mesh.is_empty() {
        let loc = r.loc(o);
        r.warn(loc, path, "the geometry is empty", None);
    }
    Some(GeometrySrc::Static(SharedMesh::new(mesh)))
}

fn vm_call(vm: &mut ScriptVm, f: ScriptObject, a: f32, b: f32) -> Option<[f32; 3]> {
    let v = vm.call(f.into(), &[(a as f64).into(), (b as f64).into()]);
    let r = Reader::new(vm, 0);
    r.plain::<3>(v, Semantic::Plain)
}

pub fn geometry_names() -> Vec<&'static str> {
    names_where(|k| {
        matches!(
            k,
            "box" | "sphere" | "torus" | "torus_knot" | "cylinder" | "cone" | "plane" | "circle" | "ring" | "capsule" | "tetrahedron" | "octahedron" | "icosahedron"
                | "dodecahedron" | "shape_geo" | "extrude" | "lathe" | "tube" | "parametric" | "heightfield" | "field_geo" | "svg_geo"
        )
    })
}

fn shapes_of(r: &mut Reader, o: ScriptObject, path: &str) -> Option<Shape2D> {
    let mut out = Shape2D::default();
    let mut vals = r.list(r.get(o, "shapes"));
    let single = r.get(o, "shape");
    if !single.is_nil() {
        vals.push(single);
    }
    for (i, v) in vals.into_iter().enumerate() {
        if let Some(s) = parse_shape(r, v, &format!("{path}.shape[{i}]")) {
            out.extend(&s);
        }
    }
    if out.contours.is_empty() {
        let loc = r.loc(o);
        r.error(loc, &format!("{path}.shape"), "needs a shape to extrude", Some("shape: StarShape{points: 5 inner: 0.5 outer: 1}, RectShape{...}, CircleShape{...}, PolygonShape{...} or PathShape{d: \"M 0 0 L 1 0 L 0 1 Z\"}".into()));
        return None;
    }
    Some(out)
}

fn parse_shape(r: &mut Reader, v: ScriptValue, path: &str) -> Option<Shape2D> {
    // A plain string is SVG path data.
    if v.is_string_like() {
        let d = r.string_of(v)?;
        return Some(Shape2D::from_svg_path(&d, 0.01));
    }
    let kind = r.kind(v)?;
    let o = v.as_object()?;
    let f = |r: &mut Reader, name: &str, d: f32| r.f32(o, path, name, d);
    let mut s = match kind.as_str() {
        "rect_shape" => {
            r.check_fields(o, path, "RectShape", &["width", "height", "radius", "segments", "holes", "offset"]);
            let (w, h, rad) = (f(r, "width", 1.0), f(r, "height", 1.0), f(r, "radius", 0.0));
            if rad > 0.0 { Shape2D::rounded_rect(w, h, rad, r.u32(o, path, "segments", 6, 64)) } else { Shape2D::rect(w, h) }
        }
        "circle_shape" => {
            r.check_fields(o, path, "CircleShape", &["radius", "segments", "holes", "offset"]);
            Shape2D::circle(f(r, "radius", 1.0), r.u32(o, path, "segments", 64, 1024))
        }
        "star_shape" => {
            r.check_fields(o, path, "StarShape", &["points", "inner", "outer", "holes", "offset"]);
            Shape2D::star(r.u32(o, path, "points", 5, 256).max(2), f(r, "inner", 0.5), f(r, "outer", 1.0))
        }
        "polygon_shape" => {
            r.check_fields(o, path, "PolygonShape", &["sides", "radius", "holes", "offset"]);
            Shape2D::polygon(r.u32(o, path, "sides", 6, 1024).max(3), f(r, "radius", 1.0))
        }
        "path_shape" => {
            r.check_fields(o, path, "PathShape", &["d", "scale", "tolerance", "center", "holes", "offset"]);
            let d = r.string(o, "d").unwrap_or_default();
            let mut s = Shape2D::from_svg_path(&d, f(r, "tolerance", 0.25));
            let k = f(r, "scale", 1.0);
            s.scale(k, k);
            if r.bool(o, "center", true) {
                s = s.centered();
            }
            if s.contours.is_empty() {
                let loc = r.loc(o);
                r.error(loc, path, "PathShape's `d` has no closed contour", Some("SVG path data like \"M 0 0 L 1 0 L 0.5 1 Z\"".into()));
                return None;
            }
            s
        }
        _ => {
            let loc = r.loc(o);
            r.error(loc, path, format!("{} is not a shape", r.describe(v)), Some("RectShape, CircleShape, StarShape, PolygonShape, PathShape or a path string".into()));
            return None;
        }
    };
    if let Some(off) = r.plain::<2>(r.get(o, "offset"), Semantic::Plain) {
        s.translate(off[0], off[1]);
    }
    for (i, h) in r.list(r.get(o, "holes")).into_iter().enumerate() {
        if let Some(hole) = parse_shape(r, h, &format!("{path}.holes[{i}]")) {
            s.cut(&hole);
        }
    }
    Some(s)
}

pub(crate) fn parse_curve(r: &mut Reader, v: ScriptValue, path: &str) -> Option<Curve3> {
    // A plain list of points is a Catmull-Rom curve.
    if v.as_array().is_some() {
        let pts: Vec<[f32; 3]> = r.list(v).into_iter().filter_map(|p| r.plain::<3>(p, Semantic::Plain)).collect();
        if pts.len() < 2 {
            return None;
        }
        return Some(Curve3::CatmullRom { points: pts, closed: false, tension: 0.5 });
    }
    let Some(kind) = r.kind(v) else {
        let loc = v.as_object().map(|o| r.loc(o)).unwrap_or_default();
        r.error(loc, path, format!("expected a curve, got {}", r.describe(v)), Some("CatmullRomCurve3{points: [vec3(...), ...]}, CubicBezierCurve3, LineCurve3, CircleCurve, HelixCurve, LissajousCurve, TorusKnotCurve".into()));
        return None;
    };
    let o = v.as_object().unwrap();
    let f = |r: &mut Reader, name: &str, d: f32| r.f32(o, path, name, d);
    let points = |r: &mut Reader| -> Vec<[f32; 3]> { r.list(r.get(o, "points")).into_iter().filter_map(|p| r.plain::<3>(p, Semantic::Plain)).collect() };
    let c = match kind.as_str() {
        "catmull_rom" => {
            r.check_fields(o, path, "CatmullRomCurve3", &["points", "closed", "tension"]);
            Curve3::CatmullRom { points: points(r), closed: r.bool(o, "closed", false), tension: f(r, "tension", 0.5) }
        }
        "bezier_curve" => {
            r.check_fields(o, path, "CubicBezierCurve3", &["points"]);
            Curve3::Bezier(points(r))
        }
        "polyline" => {
            r.check_fields(o, path, "LineCurve3", &["points"]);
            Curve3::Polyline(points(r))
        }
        "circle_curve" => {
            r.check_fields(o, path, "CircleCurve", &["radius"]);
            Curve3::Circle { radius: f(r, "radius", 1.0) }
        }
        "helix_curve" => {
            r.check_fields(o, path, "HelixCurve", &["radius", "height", "turns"]);
            Curve3::Helix { radius: f(r, "radius", 1.0), height: f(r, "height", 2.0), turns: f(r, "turns", 3.0) }
        }
        "lissajous_curve" => {
            r.check_fields(o, path, "LissajousCurve", &["a", "b", "c", "freq", "phase"]);
            let freq = r.plain::<3>(r.get(o, "freq"), Semantic::Plain).unwrap_or([3.0, 2.0, 5.0]);
            Curve3::Lissajous { a: f(r, "a", 1.0), b: f(r, "b", 1.0), c: f(r, "c", 1.0), freq, phase: f(r, "phase", 90.0) }
        }
        "fn_curve" => {
            // `FnCurve{func: fn(s) { vec3(...) } samples closed}`: any curve,
            // sampled at load (s in 0..1) into a smooth spline.
            r.check_fields(o, path, "FnCurve", &["func", "samples", "closed"]);
            let n = r.u32(o, path, "samples", 256, 20_000).max(2);
            let fv = r.get(o, "func");
            let Some(fo) = fv.as_object().filter(|x| r.vm.bx.heap_ref().is_fn(*x)) else {
                let loc = r.loc(o);
                r.error(loc, &format!("{path}.func"), "FnCurve needs `func: fn(s) { return vec3(...) }` (s runs 0..1)", None);
                return None;
            };
            let mut pts = Vec::with_capacity(n as usize);
            for i in 0..n {
                let s = i as f64 / (n - 1) as f64;
                let v = r.vm.call(fo.into(), &[s.into()]);
                match r.plain::<3>(v, Semantic::Plain) {
                    Some(p) => pts.push(p),
                    None => {
                        let loc = r.loc(o);
                        r.error(loc, &format!("{path}.func"), format!("FnCurve's func must return vec3(x, y, z); at s = {s:.3} it gave {}", r.describe(v)), None);
                        return None;
                    }
                }
            }
            Curve3::CatmullRom { points: pts, closed: r.bool(o, "closed", false), tension: 0.5 }
        }
        "paperclip_curve" => {
            r.check_fields(o, path, "PaperclipCurve", &["length", "width"]);
            Curve3::CatmullRom { points: makepad_geom3d::paperclip_points(f(r, "length", 1.0), f(r, "width", 0.32)), closed: false, tension: 0.5 }
        }
        "braid_curve" => {
            r.check_fields(o, path, "BraidCurve", &["strand", "strands", "radius", "turns", "length"]);
            let strands = r.u32(o, path, "strands", 3, 8).max(2);
            Curve3::CatmullRom {
                points: makepad_geom3d::braid_points(r.u32(o, path, "strand", 0, 7), strands, f(r, "radius", 0.3), f(r, "turns", 3.0), f(r, "length", 4.0)),
                closed: false,
                tension: 0.5,
            }
        }
        "torus_knot_curve" => {
            r.check_fields(o, path, "TorusKnotCurve", &["p", "q", "radius", "tube_offset"]);
            Curve3::TorusKnot { p: r.u32(o, path, "p", 2, 64), q: r.u32(o, path, "q", 3, 64), radius: f(r, "radius", 1.0), tube_offset: f(r, "tube_offset", 1.0) }
        }
        _ => {
            let loc = r.loc(o);
            r.error(loc, path, format!("{} is not a curve", r.describe(v)), None);
            return None;
        }
    };
    let enough = match &c {
        Curve3::CatmullRom { points, .. } | Curve3::Polyline(points) => points.len() >= 2,
        Curve3::Bezier(p) => p.len() >= 4,
        _ => true,
    };
    if !enough {
        let loc = r.loc(o);
        r.error(loc, &format!("{path}.points"), "the curve needs more points (two, or 3n+1 for Béziers)", None);
        return None;
    }
    Some(c)
}

// ---------------------------------------------------------------------------
// Nodes with payloads
// ---------------------------------------------------------------------------

fn load_glb(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str, field: &str) -> Option<(MediaRef, Arc<GlbModel>)> {
    let media = r.media(o, path, field)?;
    // A built model (`m.build()`, `character_new`): Stage's model host has it,
    // or it is building (the node waits; the document reloads when it lands).
    if let MediaRef::File(alias) = &media {
        if let Some(built) = crate::models::built_glb(&mut cx.models, alias) {
            return match built {
                crate::models::BuiltGlb::Ready(model, _) => Some((media, model)),
                crate::models::BuiltGlb::Pending => None,
                crate::models::BuiltGlb::Failed(e) => {
                    let loc = r.loc(o);
                    r.error(loc, &format!("{path}.{field}"), format!("the model `{alias}` did not build: {e}"), Some("fix its program (model_new / character_new) and save again".into()));
                    None
                }
            };
        }
    }
    if let Some(m) = cx.glbs.get(&media) {
        return Some((media, m.clone()));
    }
    let loaded = cx.resolver.load(&media).and_then(|b| GlbModel::load(&b, None));
    match loaded {
        Ok(m) => {
            for w in &m.warnings {
                let loc = r.loc(o);
                r.warn(loc, &format!("{path}.{field}"), w.clone(), None);
            }
            let m = Arc::new(m);
            cx.glbs.insert(media.clone(), m.clone());
            Some((media, m))
        }
        Err(e) => {
            let loc = r.loc(o);
            r.error(loc, &format!("{path}.{field}"), format!("the model did not load: {e}"), Some("give a .glb: asset(\"id\", \"rev\") or file(\"model.glb\")".into()));
            None
        }
    }
}

/// The glTF material as a MaterialDef (textures decoded by the loader).
pub fn material_from_glb(model: &GlbModel, index: usize) -> MaterialDef {
    let g = &model.materials[index];
    let tex = |t: &Option<crate::glb::TexRef>, srgb: bool| -> Option<ImageData> {
        let t = t.as_ref()?;
        let texture = model.textures.get(t.texture)?;
        let img = model.images.get(texture.image?)?.as_ref()?;
        Some(ImageData::new(img.width, img.height, img.rgba.clone(), srgb))
    };
    let mut m = MaterialDef {
        name: Some(g.name.clone()),
        model: if g.unlit { ShadingModel::Basic } else { ShadingModel::Standard },
        color: Anim::Const(g.base_color),
        metalness: Anim::Const([g.metallic]),
        roughness: Anim::Const([g.roughness]),
        emissive: Anim::Const([g.emissive[0], g.emissive[1], g.emissive[2], 1.0]),
        emissive_intensity: Anim::Const([g.emissive_strength]),
        clearcoat: Anim::Const([g.clearcoat]),
        clearcoat_roughness: Anim::Const([g.clearcoat_roughness]),
        transmission: Anim::Const([g.transmission]),
        ior: Anim::Const([g.ior]),
        thickness: Anim::Const([g.thickness]),
        attenuation_color: Anim::Const([g.attenuation_color[0], g.attenuation_color[1], g.attenuation_color[2], 1.0]),
        sheen: Anim::Const([if g.sheen_color.iter().any(|c| *c > 0.0) { 1.0 } else { 0.0 }]),
        sheen_color: Anim::Const([g.sheen_color[0], g.sheen_color[1], g.sheen_color[2], 1.0]),
        specular_intensity: Anim::Const([g.specular]),
        side: if g.double_sided { FaceSide::Double } else { FaceSide::Front },
        normal_scale: g.normal_scale,
        occlusion_strength: g.occlusion_strength,
        ..MaterialDef::default()
    };
    m.maps.base = tex(&g.base_color_texture, true);
    m.maps.metal_rough = tex(&g.metallic_roughness_texture, false);
    m.maps.normal = tex(&g.normal_texture, false);
    m.maps.emissive = tex(&g.emissive_texture, true);
    m.maps.occlusion = tex(&g.occlusion_texture, false);
    match g.alpha_mode {
        AlphaMode::Opaque => {}
        AlphaMode::Mask(c) => m.alpha_cutoff = Some(c),
        AlphaMode::Blend => m.transparent = true,
    }
    m
}

fn parse_model(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str, scene_level: bool) -> Option<NodeDef> {
    if !scene_level {
        r.check_fields(o, path, "Model", &with_node_fields(&["src", "animation", "animations", "speed", "loop", "nodes", "materials", "morphs", "fit", "ground", "material", "displace", "displace_frequency", "displace_speed", "twist", "bend", "ripple", "ripple_frequency", "ripple_speed", "explode", "curl", "curl_radius", "wrap_radius"]));
    }
    let field = if scene_level { "model" } else { "src" };
    let (src, model) = load_glb(r, cx, o, path, field)?;
    let base_materials: Vec<MaterialDef> = (0..model.materials.len()).map(|i| material_from_glb(&model, i)).collect();
    let clip_names: Vec<&str> = model.clips.iter().map(|c| c.name.as_str()).collect();
    let find_clip = |r: &mut Reader, name: &str, loc: Loc, p: &str| -> Option<usize> {
        match model.clip_by_name(name) {
            Some(c) => Some(c),
            None => {
                let hint = crate::glb::closest_name(name, clip_names.iter().copied()).map(|c| format!("did you mean \"{c}\"? ")).unwrap_or_default();
                r.error(loc, p, format!("the model has no animation \"{name}\""), Some(format!("{hint}its clips: {}", if clip_names.is_empty() { "(none)".to_string() } else { clip_names.join(", ") })));
                None
            }
        }
    };
    let mut clips = Vec::new();
    let speed = r.f32(o, path, "speed", 1.0);
    let looping = r.bool(o, "loop", true);
    let av = r.get(o, "animation");
    if !av.is_nil() {
        let name = r.string_of(av).unwrap_or_default();
        let loc = r.loc(o);
        if let Some(c) = find_clip(r, &name, loc, &format!("{path}.animation")) {
            clips.push(ClipDef { clip: c, time: None, speed, looping, weight: Anim::Const([1.0]), offset: 0.0 });
        }
    }
    for (i, cv) in r.list(r.get(o, "animations")).into_iter().enumerate() {
        let p = format!("{path}.animations[{i}]");
        let Some(co) = cv.as_object().filter(|_| r.kind(cv).as_deref() == Some("clip")) else {
            let loc = cv.as_object().map(|x| r.loc(x)).unwrap_or_default();
            r.error(loc, &p, format!("expected AnimationClip{{name: \"Walk\" ...}}, got {}", r.describe(cv)), None);
            continue;
        };
        r.check_fields(co, &p, "AnimationClip", &["name", "time", "speed", "loop", "weight", "offset"]);
        let name = r.string(co, "name").unwrap_or_default();
        let loc = r.loc(co);
        if let Some(c) = find_clip(r, &name, loc, &p) {
            clips.push(ClipDef {
                clip: c,
                time: if r.has(co, "time") { Some(r.anim::<1>(co, &p, "time", [0.0], Semantic::Plain)) } else { None },
                speed: r.f32(co, &p, "speed", speed),
                looping: r.bool(co, "loop", looping),
                weight: r.anim::<1>(co, &p, "weight", [1.0], Semantic::Plain),
                offset: r.f32(co, &p, "offset", 0.0),
            });
        }
    }
    let node_names: Vec<String> = model.nodes.iter().map(|n| n.name.clone()).collect();
    let mut nodes = Vec::new();
    for (i, nv) in r.list(r.get(o, "nodes")).into_iter().enumerate() {
        let p = format!("{path}.nodes[{i}]");
        let Some(no) = nv.as_object().filter(|_| r.kind(nv).as_deref() == Some("node_override")) else {
            let loc = nv.as_object().map(|x| r.loc(x)).unwrap_or_default();
            r.error(loc, &p, format!("expected NodeOverride{{name: \"Wheel\" rotation: ...}}, got {}", r.describe(nv)), None);
            continue;
        };
        r.check_fields(no, &p, "NodeOverride", &["name", "position", "rotation", "scale", "visible", "translate", "rotate"]);
        let name = r.string(no, "name").unwrap_or_default();
        let Some(node) = model.node_by_name(&name) else {
            let loc = r.loc(no);
            let hint = crate::glb::closest_name(&name, node_names.iter().map(|s| s.as_str())).map(|c| format!("did you mean \"{c}\"?"));
            r.error(loc, &p, format!("the model has no node \"{name}\""), hint);
            continue;
        };
        let pos = if r.has(no, "translate") { "translate" } else { "position" };
        let rot = if r.has(no, "rotate") { "rotate" } else { "rotation" };
        nodes.push(NodeOverride {
            node,
            position: if r.has(no, pos) { Some(r.anim::<3>(no, &p, pos, [0.0; 3], Semantic::Plain)) } else { None },
            rotation: if r.has(no, rot) { Some(r.anim::<3>(no, &p, rot, [0.0; 3], Semantic::Plain)) } else { None },
            scale: if r.has(no, "scale") { Some(r.anim::<3>(no, &p, "scale", [1.0; 3], Semantic::Plain)) } else { None },
            visible: if r.has(no, "visible") { Some(r.anim::<1>(no, &p, "visible", [1.0], Semantic::Plain)) } else { None },
        });
    }
    let mut materials = Vec::new();
    let mat_names: Vec<String> = model.materials.iter().map(|m| m.name.clone()).collect();
    for (i, mv) in r.list(r.get(o, "materials")).into_iter().enumerate() {
        let p = format!("{path}.materials[{i}]");
        let kind = r.kind(mv);
        let Some(mo) = mv.as_object().filter(|_| kind.is_some()) else { continue };
        let name = r.string(mo, "name");
        let material = match &name {
            Some(n) => match model.material_by_name(n) {
                Some(m) => Some(m),
                None => {
                    let loc = r.loc(mo);
                    let hint = crate::glb::closest_name(n, mat_names.iter().map(|s| s.as_str())).map(|c| format!("did you mean \"{c}\"?"));
                    r.error(loc, &p, format!("the model has no material \"{n}\""), hint.or_else(|| Some(format!("its materials: {}", mat_names.join(", ")))));
                    continue;
                }
            },
            None => None,
        };
        let mut def = material.map(|m| base_materials[m].clone()).unwrap_or_default();
        let mut set = Vec::new();
        r.check_fields(mo, &p, "MaterialOverride", MATERIAL_FIELDS);
        apply_material_fields(r, cx, mo, &p, &mut def, Some(&mut set));
        materials.push(MaterialOverride { material, def, set });
    }
    if let Some(mv) = Some(r.get(o, "material")).filter(|v| !v.is_nil()) {
        // One material for every part of the model.
        let def = parse_material(r, cx, mv, &format!("{path}.material"));
        materials.push(MaterialOverride { material: None, def, set: vec!["*".into()] });
    }
    let mut morphs = Vec::new();
    if let Some(mo) = r.get(o, "morphs").as_object() {
        for name in r.own_fields(mo) {
            let mut found = false;
            for (node, mesh) in model.scene_nodes() {
                if let Some(t) = model.target_by_name(mesh, &name) {
                    morphs.push((node, t, r.anim::<1>(mo, &format!("{path}.morphs"), &name, [0.0], Semantic::Plain)));
                    found = true;
                }
            }
            if !found {
                let loc = r.loc(mo);
                let all: Vec<String> = model.meshes.iter().flat_map(|m| m.target_names.iter().cloned()).collect();
                let hint = crate::glb::closest_name(&name, all.iter().map(|s| s.as_str())).map(|c| format!("did you mean `{c}`?"));
                r.error(loc, &format!("{path}.morphs.{name}"), format!("no morph target named `{name}`"), hint.or_else(|| Some(format!("targets: {}", if all.is_empty() { "(none)".into() } else { all.join(", ") }))));
            }
        }
    }
    let fit = r.f32(o, path, "fit", 0.0);
    let ground = r.bool(o, "ground", false);
    let deform = if scene_level { DeformDef::default() } else { parse_deform(r, o, path) };
    let def = ModelDef { model, src, clips, nodes, materials, morphs, fit, ground, base_materials, deform };
    if scene_level {
        let mut node = NodeDef {
            name: Some("model".into()),
            id: 0,
            path: format!("{path}.model"),
            loc: r.loc(o),
            transform: TransformDef::default(),
            visible: Anim::Const([1.0]),
            opacity: Anim::Const([1.0]),
            span: None,
            cast_shadow: true,
            receive_shadow: true,
            outline: false,
            kind: NodeKind::Model(Box::new(def)),
            children: Vec::new(),
        };
        node.transform = TransformDef::default();
        return Some(node);
    }
    Some(base_node(r, cx, o, path, NodeKind::Model(Box::new(def))))
}

pub(crate) fn parse_each(r: &mut Reader, o: ScriptObject, path: &str) -> EachDef {
    let mut each = EachDef::default();
    let Some(e) = r.get(o, "each").as_object().filter(|eo| !r.vm.bx.heap_ref().is_fn(*eo)) else { return each };
    let p = format!("{path}.each");
    r.check_fields(e, &p, "each", &["position", "rotation", "scale", "opacity", "color", "emissive"]);
    if r.has(e, "position") {
        each.position = Some(r.anim::<3>(e, &p, "position", [0.0; 3], Semantic::Plain));
    }
    if r.has(e, "rotation") {
        each.rotation = Some(r.anim::<3>(e, &p, "rotation", [0.0; 3], Semantic::Plain));
    }
    if r.has(e, "scale") {
        each.scale = Some(r.anim::<3>(e, &p, "scale", [1.0; 3], Semantic::Plain));
    }
    if r.has(e, "opacity") {
        each.opacity = Some(r.anim::<1>(e, &p, "opacity", [1.0], Semantic::Plain));
    }
    if r.has(e, "color") {
        each.color = Some(r.anim::<4>(e, &p, "color", [1.0; 4], Semantic::Color));
    }
    if r.has(e, "emissive") {
        each.emissive = Some(r.anim::<4>(e, &p, "emissive", [0.0; 4], Semantic::Color));
    }
    each
}

pub(crate) fn parse_order(r: &mut Reader, o: ScriptObject, path: &str) -> Order {
    match r.id(o, "order") {
        None => Order::Forward,
        Some(s) => Order::by_name(&s).unwrap_or_else(|| {
            let loc = r.loc(o);
            r.warn(loc, &format!("{path}.order"), format!("no order `@{s}`"), Some(format!("one of {}", Order::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
            Order::Forward
        }),
    }
}

fn parse_layout(r: &mut Reader, cx: &mut LoadCx, v: ScriptValue, path: &str, count_hint: Option<u32>) -> Option<Vec<InstanceBase>> {
    if v.is_nil() {
        let n = count_hint.unwrap_or(100).max(1);
        let side = (n as f32).cbrt().ceil() as u32;
        return Some(Layout::Grid { count: [side, side, side], spacing: [1.0; 3] }.generate(None).into_iter().take(n as usize).collect());
    }
    if v.as_array().is_some() {
        let pts: Vec<[f32; 3]> = r.list(v).into_iter().filter_map(|p| r.plain::<3>(p, Semantic::Plain)).collect();
        return Some(Layout::Points(pts).generate(None));
    }
    let kind = r.kind(v)?;
    let o = v.as_object()?;
    let count = |r: &mut Reader| r.u32(o, path, "count", count_hint.unwrap_or(100), 1_000_000).max(1);
    let f = |r: &mut Reader, n: &str, d: f32| r.f32(o, path, n, d);
    let seed = |r: &mut Reader| r.f32(o, path, "seed", 1.0) as u64;
    let layout = match kind.as_str() {
        "grid_layout" => {
            r.check_fields(o, path, "GridLayout", &["count", "size", "spacing"]);
            let c = r.plain::<3>(r.get(o, "count"), Semantic::Plain).unwrap_or([10.0, 10.0, 1.0]);
            let spacing = r.plain::<3>(r.get(o, "spacing"), Semantic::Plain).unwrap_or([1.0; 3]);
            Layout::Grid { count: [c[0].max(1.0) as u32, c[1].max(1.0) as u32, c[2].max(1.0) as u32], spacing }
        }
        "line_layout" => {
            r.check_fields(o, path, "LineLayout", &["count", "start", "end"]);
            let start = r.plain::<3>(r.get(o, "start"), Semantic::Plain).unwrap_or([-1.0, 0.0, 0.0]);
            let end = r.plain::<3>(r.get(o, "end"), Semantic::Plain).unwrap_or([1.0, 0.0, 0.0]);
            Layout::Line { count: count(r), start, end }
        }
        "circle_layout" => {
            r.check_fields(o, path, "CircleLayout", &["count", "radius"]);
            Layout::Circle { count: count(r), radius: f(r, "radius", 2.0) }
        }
        "sphere_layout" => {
            r.check_fields(o, path, "SphereLayout", &["count", "radius"]);
            Layout::Sphere { count: count(r), radius: f(r, "radius", 2.0) }
        }
        "cube_layout" => {
            r.check_fields(o, path, "CubeLayout", &["count", "size", "seed"]);
            let size = r.plain::<3>(r.get(o, "size"), Semantic::Plain).unwrap_or([4.0; 3]);
            Layout::Cube { count: count(r), size, seed: seed(r) }
        }
        "ball_layout" => {
            r.check_fields(o, path, "BallLayout", &["count", "radius", "seed", "surface"]);
            Layout::Ball { count: count(r), radius: f(r, "radius", 2.0), seed: seed(r), surface: r.bool(o, "surface", false) }
        }
        "spiral_layout" => {
            r.check_fields(o, path, "SpiralLayout", &["count", "radius", "turns", "height"]);
            Layout::Spiral { count: count(r), radius: f(r, "radius", 3.0), turns: f(r, "turns", 4.0), height: f(r, "height", 0.0) }
        }
        "helix_layout" => {
            r.check_fields(o, path, "HelixLayout", &["count", "radius", "turns", "height"]);
            Layout::Helix { count: count(r), radius: f(r, "radius", 1.5), turns: f(r, "turns", 3.0), height: f(r, "height", 4.0) }
        }
        "phyllotaxis_layout" => {
            r.check_fields(o, path, "PhyllotaxisLayout", &["count", "spacing"]);
            Layout::Phyllotaxis { count: count(r), spacing: f(r, "spacing", 0.1) }
        }
        "surface_layout" => {
            r.check_fields(o, path, "SurfaceLayout", &["count", "seed", "geometry"]);
            let g = r.get(o, "geometry");
            let mesh = match parse_geometry(r, cx, g, &format!("{path}.geometry"))? {
                GeometrySrc::Static(m) => m.mesh.clone(),
                GeometrySrc::Tube { tube, .. } => Arc::new(tube.build()),
                GeometrySrc::Engine(p) => Arc::new(crate::engines::build(&p, 0.0)),
            };
            return Some(Layout::Surface { count: count(r), seed: seed(r) }.generate(Some(&mesh)));
        }
        "curve_layout" => {
            r.check_fields(o, path, "CurveLayout", &["count", "curve"]);
            let n = count(r);
            let cv = r.get(o, "curve");
            let curve = parse_curve(r, cv, &format!("{path}.curve"))?;
            let arc = curve.arc();
            let pts: Vec<[f32; 3]> = (0..n).map(|i| arc.point_at(if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 })).collect();
            let mut bases = Layout::Points(pts).generate(None);
            for (i, b) in bases.iter_mut().enumerate() {
                b.normal = arc.tangent_at(if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 });
            }
            return Some(bases);
        }
        "text_layout" => {
            r.check_fields(o, path, "TextLayout", &["count", "text", "font", "size", "depth", "seed"]);
            let mesh = text_points_mesh(r, cx, o, path)?;
            return Some(Layout::Surface { count: count(r), seed: seed(r) }.generate(Some(&mesh)));
        }
        "voxel_layout" => {
            r.check_fields(o, path, "VoxelLayout", &["geometry", "resolution", "text", "font", "size", "depth"]);
            let res = r.u32(o, path, "resolution", 16, 128).max(2);
            let mesh = if r.has(o, "text") {
                text_points_mesh(r, cx, o, path)?
            } else {
                let g = r.get(o, "geometry");
                match parse_geometry(r, cx, g, &format!("{path}.geometry"))? {
                    GeometrySrc::Static(m) => m.mesh.clone(),
                    GeometrySrc::Tube { tube, .. } => Arc::new(tube.build()),
                    GeometrySrc::Engine(p) => Arc::new(crate::engines::build(&p, 0.0)),
                }
            };
            let mut pts = voxelize(&mesh, res);
            if let Some(n) = count_hint {
                // Keep the count stable (for morphs): evenly thin or repeat.
                let n = n as usize;
                if !pts.is_empty() && pts.len() != n {
                    pts = (0..n).map(|i| pts[i * pts.len() / n]).collect();
                }
            }
            return Some(Layout::Points(pts).generate(None));
        }
        "crowd_layout" => {
            use crate::crowd::*;
            r.check_fields(o, path, "CrowdLayout", &["count", "shape", "radius", "spacing", "tier", "arc", "seed", "variant", "variants", "pick", "side", "offset"]);
            let d = CrowdParams::default();
            let shape = match r.id(o, "shape") {
                Some(s) => CrowdShape::by_name(&s).unwrap_or_else(|| {
                    let loc = r.loc(o);
                    r.warn(loc, &format!("{path}.shape"), format!("no crowd shape `@{s}`"), Some("@arena, @block or @ring".into()));
                    d.shape
                }),
                None => d.shape,
            };
            let variants = r.u32(o, path, "variants", 1, 64).max(1);
            let p = CrowdParams {
                count: r.u32(o, path, "count", d.count, 100_000).max(1),
                shape,
                radius: f(r, "radius", d.radius),
                spacing: f(r, "spacing", d.spacing),
                tier: f(r, "tier", d.tier),
                arc: f(r, "arc", d.arc),
                seed: seed(r),
                variant: if r.has(o, "variant") { Some((r.u32(o, path, "variant", 0, 63), variants)) } else { None },
                pick: if r.has(o, "pick") { Some(f(r, "pick", 1.0).clamp(0.0, 1.0)) } else { None },
                side: match r.id(o, "side").as_deref() {
                    Some("left") => Side::Left,
                    Some("right") => Side::Right,
                    _ => Side::All,
                },
                offset: r.plain::<3>(r.get(o, "offset"), Semantic::Plain).unwrap_or([0.0; 3]),
            };
            return Some(crowd_layout(&p));
        }
        "maze_layout" => {
            r.check_fields(o, path, "MazeLayout", &["cols", "rows", "cell", "seed", "open"]);
            let cols = r.u32(o, path, "cols", 10, 200);
            let rows = r.u32(o, path, "rows", 10, 200);
            return Some(crate::crowd::maze_layout(cols, rows, f(r, "cell", 1.0).max(0.01), seed(r), r.bool(o, "open", true)));
        }
        "points_layout" => {
            r.check_fields(o, path, "PointsLayout", &["points"]);
            let pts: Vec<[f32; 3]> = r.list(r.get(o, "points")).into_iter().filter_map(|p| r.plain::<3>(p, Semantic::Plain)).collect();
            Layout::Points(pts)
        }
        _ => {
            let loc = r.loc(o);
            let names = names_where(|k| k.ends_with("_layout"));
            r.error(loc, path, format!("{} is not a layout", r.describe(v)), Some(format!("layouts: {}", names.join(", "))));
            return None;
        }
    };
    Some(layout.generate(None))
}

fn parse_instances(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str) -> Option<NodeDef> {
    r.check_fields(o, path, "Instances", &with_node_fields(&[
        "geometry", "material", "count", "layout", "stagger", "order", "each", "align", "color_jitter", "scale_jitter", "rotation_jitter", "displace",
        "displace_frequency", "displace_speed", "twist", "bend", "ripple", "ripple_frequency", "ripple_speed", "morph_to", "morph", "scatter", "scatter_radius",
        "heights", "wrap_radius", "source", "model",
    ]));
    // `model: <built alias or .glb>`: the model's meshes are the instance
    // geometry, one Instances per material (or all of it in `material`).
    // A model still building leaves the node out until it lands.
    let model_parts = if r.has(o, "model") {
        let (_, model) = load_glb(r, cx, o, path, "model")?;
        let parts = crate::models::meshes_by_material(&model);
        if parts.is_empty() {
            let loc = r.loc(o);
            r.error(loc, &format!("{path}.model"), "the model has no meshes to instance", None);
            return None;
        }
        Some((model, parts))
    } else {
        None
    };
    let geometry = match &model_parts {
        Some((_, parts)) => SharedMesh::new(parts[0].1.clone()),
        None => match parse_geometry(r, cx, r.get(o, "geometry"), &format!("{path}.geometry"))? {
            GeometrySrc::Static(m) => m,
            GeometrySrc::Tube { tube, .. } => SharedMesh::new(tube.build()),
            GeometrySrc::Engine(p) => SharedMesh::new(crate::engines::build(&p, 0.0)),
        },
    };
    let own_material = r.has(o, "material");
    let material = parse_material(r, cx, r.get(o, "material"), &format!("{path}.material"));
    let count = if r.has(o, "count") { Some(r.u32(o, path, "count", 100, 1_000_000)) } else { None };
    // `source: bodies("g")`: the instances are a physics group's bodies.
    let bodies = match r.get(o, "source") {
        v if v.is_nil() => None,
        v if r.is_anim(v).as_deref() == Some("bodies") => Some(r.string_of(r.get(v.as_object()?, "group")).unwrap_or_default()),
        v => {
            let loc = r.loc(o);
            r.error(loc, &format!("{path}.source"), format!("an Instances source is bodies(\"group\"), not {}", r.describe(v)), Some("or place instances with `layout`".into()));
            return None;
        }
    };
    let mut bases = if bodies.is_some() { Vec::new() } else { parse_layout(r, cx, r.get(o, "layout"), &format!("{path}.layout"), count)? };
    if let Some(n) = count {
        bases.truncate(n as usize);
    }
    let each_fn = r.closure(o, "each");
    let def = InstancesDef {
        geometry,
        material,
        bases,
        stagger: r.f32(o, path, "stagger", 0.0),
        order: parse_order(r, o, path),
        each: parse_each(r, o, path),
        each_fn,
        align: r.bool(o, "align", false),
        color_jitter: r.f32(o, path, "color_jitter", 0.0),
        scale_jitter: r.f32(o, path, "scale_jitter", 0.0),
        rotation_jitter: r.f32(o, path, "rotation_jitter", 0.0),
        deform: parse_deform(r, o, path),
        morph: None,
        scatter: if r.has(o, "scatter") { Some((r.anim::<1>(o, path, "scatter", [0.0], Semantic::Plain), r.f32(o, path, "scatter_radius", 4.0))) } else { None },
        heights: parse_heights(r, o, path),
        bodies,
    };
    let mut def = def;
    if r.has(o, "morph_to") {
        let n = def.bases.len() as u32;
        if let Some(target) = parse_layout(r, cx, r.get(o, "morph_to"), &format!("{path}.morph_to"), Some(n)) {
            if !target.is_empty() {
                def.morph = Some((target, r.anim::<1>(o, path, "morph", [0.0], Semantic::Plain)));
            }
        }
    }
    let Some((model, parts)) = model_parts else {
        return Some(base_node(r, cx, o, path, NodeKind::Instances(Box::new(def))));
    };
    if own_material {
        let mut all = makepad_geom3d::mesh::Mesh::default();
        for (_, mesh) in &parts {
            all.append(mesh);
        }
        def.geometry = SharedMesh::new(all);
        return Some(base_node(r, cx, o, path, NodeKind::Instances(Box::new(def))));
    }
    // One Instances per material of the model, sharing everything else.
    let mut node = base_node(r, cx, o, path, NodeKind::Group);
    let loc = node.loc;
    let (cast_shadow, receive_shadow, outline) = (node.cast_shadow, node.receive_shadow, node.outline);
    let kids: Vec<NodeDef> = parts
        .into_iter()
        .enumerate()
        .map(|(k, (material, mesh))| {
            let mut part = def.clone();
            part.geometry = SharedMesh::new(mesh);
            part.material = material.map(|m| material_from_glb(&model, m)).unwrap_or_else(|| def.material.clone());
            NodeDef {
                name: None,
                id: 0,
                path: format!("{path}.model[{k}]"),
                loc,
                transform: TransformDef::default(),
                visible: Anim::Const([1.0]),
                opacity: Anim::Const([1.0]),
                span: None,
                cast_shadow,
                receive_shadow,
                outline,
                kind: NodeKind::Instances(Box::new(part)),
                children: Vec::new(),
            }
        })
        .collect();
    node.children.splice(0..0, kids);
    Some(node)
}

/// `heights: Heightfield{kind amount cell speed every seed}`.
fn parse_heights(r: &mut Reader, o: ScriptObject, path: &str) -> Option<HeightfieldDef> {
    let v = r.get(o, "heights");
    let h = v.as_object().filter(|_| r.kind(v).as_deref() == Some("heightfield"))?;
    let p = format!("{path}.heights");
    r.check_fields(h, &p, "Heightfield", &["kind", "amount", "cell", "speed", "every", "seed"]);
    let kind = match r.id(h, "kind") {
        Some(k) => HeightKind::by_name(&k).unwrap_or_else(|| {
            let loc = r.loc(h);
            r.warn(loc, &format!("{p}.kind"), format!("no heightfield `@{k}`"), Some(format!("one of {}", HeightKind::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
            HeightKind::BlocksWave
        }),
        None => HeightKind::BlocksWave,
    };
    let cell = r.plain::<2>(r.get(h, "cell"), Semantic::Plain).unwrap_or([1.0, 0.7]);
    let beat = 60.0 / r.bpm.max(1.0) as f32;
    let every = match r.number(r.get(h, "every")) {
        Some(e) if e > 0.0 => e as f32,
        _ => beat,
    };
    Some(HeightfieldDef {
        kind,
        amount: r.anim::<1>(h, &p, "amount", [0.5], Semantic::Plain),
        cell: [cell[0].max(0.01), cell[1].max(0.01)],
        speed: r.f32(h, &p, "speed", 0.25),
        every,
        seed: r.f32(h, &p, "seed", 1.0) as u64,
    })
}

fn parse_points(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str, particles: bool) -> Option<NodeDef> {
    let common = ["size", "size_attenuation", "color", "opacity", "shape", "blending", "stagger", "each", "geometry", "material"];
    let source = if particles {
        let mut k: Vec<&str> = with_node_fields(&common);
        k.extend_from_slice(&[
            "count", "rate", "start", "lifetime", "lifetime_jitter", "emitter", "direction", "spread", "speed", "speed_jitter", "gravity", "drag", "turbulence",
            "turbulence_frequency", "swirl", "size_start", "size_end", "color_start", "color_end", "gradient", "fade_in", "fade_out", "spin", "seed", "looping", "path", "path_duration",
        ]);
        r.check_fields(o, path, "Particles", &k);
        let d = ParticleSystem::default();
        let f = |r: &mut Reader, n: &str, dv: f32| r.f32(o, path, n, dv);
        let v3 = |r: &mut Reader, n: &str, dv: [f32; 3]| { let v = r.get(o, n); r.plain::<3>(v, Semantic::Plain).unwrap_or(dv) };
        let c4 = |r: &mut Reader, n: &str, dv: [f32; 4]| { let v = r.get(o, n); r.plain::<4>(v, Semantic::Color).unwrap_or(dv) };
        let ev = r.get(o, "emitter");
        let mut emitter_points = None;
        let emitter = match r.kind(ev).as_deref() {
            None => Emitter::Point,
            Some(k) => {
                let eo = ev.as_object().unwrap();
                let ep = format!("{path}.emitter");
                let g = |r: &mut Reader, n: &str, dv: f32| r.f32(eo, &ep, n, dv);
                match k {
                    "point_emitter" => Emitter::Point,
                    "sphere_emitter" => Emitter::Sphere { radius: g(r, "radius", 1.0), surface: r.bool(eo, "surface", false) },
                    "box_emitter" => Emitter::Box { size: r.plain::<3>(r.get(eo, "size"), Semantic::Plain).unwrap_or([1.0; 3]) },
                    "disc_emitter" => Emitter::Disc { radius: g(r, "radius", 1.0) },
                    "ring_emitter" => Emitter::Ring { radius: g(r, "radius", 1.0) },
                    "line_emitter" => Emitter::Line { a: r.plain::<3>(r.get(eo, "a"), Semantic::Plain).unwrap_or([-1.0, 0.0, 0.0]), b: r.plain::<3>(r.get(eo, "b"), Semantic::Plain).unwrap_or([1.0, 0.0, 0.0]) },
                    "curve_emitter" => {
                        let cv = r.get(eo, "curve");
                        let n = r.u32(eo, &ep, "count", 512, 200_000).max(2);
                        if let Some(curve) = parse_curve(r, cv, &format!("{ep}.curve")) {
                            let arc = curve.arc();
                            let pts: Vec<[f32; 3]> = (0..n).map(|i| arc.point_at(i as f32 / (n - 1) as f32)).collect();
                            emitter_points = Some(Layout::Points(pts).generate(None));
                        }
                        Emitter::Points
                    }
                    "text_emitter" => {
                        let n = r.u32(eo, &ep, "count", 4096, 200_000);
                        if let Some(mesh) = text_points_mesh(r, cx, eo, &ep) {
                            emitter_points = Some(Layout::Surface { count: n, seed: 7 }.generate(Some(&mesh)));
                        }
                        Emitter::Points
                    }
                    "surface_emitter" => {
                        let lv = r.get(eo, "geometry");
                        let mesh = match parse_geometry(r, cx, lv, &format!("{ep}.geometry")) {
                            Some(GeometrySrc::Static(m)) => m.mesh.clone(),
                            _ => Arc::new(SphereGeo::default().build()),
                        };
                        let n = r.u32(eo, &ep, "count", 2048, 200_000);
                        emitter_points = Some(Layout::Surface { count: n, seed: 7 }.generate(Some(&mesh)));
                        Emitter::Points
                    }
                    _ => {
                        let loc = r.loc(eo);
                        let names = names_where(|k| k.ends_with("_emitter"));
                        r.error(loc, &ep, format!("{} is not an emitter", r.describe(ev)), Some(format!("emitters: {}", names.join(", "))));
                        Emitter::Point
                    }
                }
            }
        };
        let mut gradient = Vec::new();
        for g in r.list(r.get(o, "gradient")) {
            let items = r.list(g);
            if items.len() == 2 {
                if let (Some(x), Some(c)) = (r.number(items[0]), r.plain::<4>(items[1], Semantic::Color)) {
                    gradient.push((x as f32, c));
                }
            }
        }
        let system = ParticleSystem {
            count: r.u32(o, path, "count", d.count, 500_000),
            rate: f(r, "rate", d.rate),
            start: f(r, "start", d.start),
            lifetime: f(r, "lifetime", d.lifetime),
            lifetime_jitter: f(r, "lifetime_jitter", d.lifetime_jitter),
            emitter,
            direction: v3(r, "direction", d.direction),
            spread: f(r, "spread", d.spread),
            speed: f(r, "speed", d.speed),
            speed_jitter: f(r, "speed_jitter", d.speed_jitter),
            gravity: v3(r, "gravity", d.gravity),
            drag: f(r, "drag", d.drag),
            turbulence: Turbulence { amount: f(r, "turbulence", 0.0), frequency: f(r, "turbulence_frequency", 0.5), seed: f(r, "seed", 1.0) as u64 },
            swirl: f(r, "swirl", d.swirl),
            size_start: f(r, "size_start", d.size_start),
            size_end: f(r, "size_end", d.size_end),
            color_start: c4(r, "color_start", d.color_start),
            color_end: c4(r, "color_end", d.color_end),
            gradient,
            fade_in: f(r, "fade_in", d.fade_in),
            fade_out: f(r, "fade_out", d.fade_out),
            spin: f(r, "spin", d.spin),
            seed: f(r, "seed", d.seed as f32) as u64,
            looping: r.bool(o, "looping", d.looping),
        };
        let path = match r.get(o, "path") {
            v if v.is_nil() => None,
            v => parse_curve(r, v, &format!("{path}.path")).map(|c| (c, r.f32(o, path, "path_duration", 3.0).max(0.01))),
        };
        PointSource::Particles { system, emitter_points, path }
    } else if r.has(o, "source") {
        let mut k: Vec<&str> = with_node_fields(&common);
        k.push("source");
        r.check_fields(o, path, "Points", &k);
        let sv = r.get(o, "source");
        let sim_name = |r: &mut Reader| -> Option<String> {
            let id = r.number(r.get(sv.as_object()?, "id"))? as usize;
            crate::script::host(r.vm)?
                .kernels
                .sims
                .get(id)
                .map(|s| s.name.clone())
        };
        if let Some(run) = kernel_source(r, sv, &format!("{path}.source")) {
            PointSource::Kernel(run)
        } else if r.is_anim(sv).as_deref() == Some("bodies") {
            PointSource::Bodies(sv.as_object().and_then(|so| r.string_of(r.get(so, "group"))).unwrap_or_default())
        } else if r.is_anim(sv).as_deref() == Some("sim_ref") {
            match sim_name(r) {
                Some(n) => PointSource::Sim(n),
                None => return None,
            }
        } else {
            // A load-time kernel's (or any) array of positions.
            let pts: Vec<[f32; 3]> = r.list(sv).into_iter().filter_map(|p| r.plain::<3>(p, Semantic::Plain)).collect();
            if pts.is_empty() {
                let loc = r.loc(o);
                r.error(loc, &format!("{path}.source"), format!("Points' source is a kernel output or an array of vec3, not {}", r.describe(sv)), None);
                return None;
            }
            PointSource::Layout(crate::layout::Layout::Points(pts).generate(None))
        }
    } else {
        let mut k: Vec<&str> = with_node_fields(&common);
        k.extend_from_slice(&["count", "layout"]);
        r.check_fields(o, path, "Points", &k);
        let count = if r.has(o, "count") { Some(r.u32(o, path, "count", 1000, 2_000_000)) } else { None };
        PointSource::Layout(parse_layout(r, cx, r.get(o, "layout"), &format!("{path}.layout"), count)?)
    };
    let shape = match r.id(o, "shape") {
        None => SpriteShape::Soft,
        Some(s) => SpriteShape::by_name(&s).unwrap_or_else(|| {
            let loc = r.loc(o);
            r.warn(loc, &format!("{path}.shape"), format!("no sprite shape `@{s}`"), Some(format!("one of {}", SpriteShape::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
            SpriteShape::Soft
        }),
    };
    let blending = match r.id(o, "blending").as_deref() {
        Some("normal") => Blending::Normal,
        Some("multiply") => Blending::Multiply,
        _ => Blending::Additive,
    };
    let mesh = if r.has(o, "geometry") {
        match parse_geometry(r, cx, r.get(o, "geometry"), &format!("{path}.geometry")) {
            Some(GeometrySrc::Static(m)) => Some((m, parse_material(r, cx, r.get(o, "material"), &format!("{path}.material")))),
            _ => None,
        }
    } else {
        None
    };
    // Particles and kernel points carry their own sizes: `size` scales them.
    let size_default = if particles || matches!(source, PointSource::Kernel(_)) { 1.0 } else { 0.05 };
    let def = PointsDef {
        source,
        size: r.anim::<1>(o, path, "size", [size_default], Semantic::Plain),
        size_attenuation: r.bool(o, "size_attenuation", true),
        color: r.anim::<4>(o, path, "color", [1.0; 4], Semantic::Color),
        opacity: r.anim::<1>(o, path, "opacity", [1.0], Semantic::Plain),
        shape,
        blending,
        stagger: r.f32(o, path, "stagger", 0.0),
        each: parse_each(r, o, path),
        mesh,
    };
    Some(base_node(r, cx, o, path, NodeKind::Points(Box::new(def))))
}

/// A per-frame kernel output (`k.out` of a `Kernel{...}` that reads `time`
/// or a param), ready to run each frame; `None` when `v` is not one.
fn kernel_source(r: &mut Reader, v: ScriptValue, path: &str) -> Option<crate::kernels::KernelRun> {
    if r.is_anim(v).as_deref() != Some("kernel_buf") {
        return None;
    }
    let o = v.as_object()?;
    let id = r.number(r.get(o, "kernel"))? as usize;
    let given = r.number(r.get(o, "count")).map(|n| n.max(0.0) as usize);
    let buffer = r.string(o, "buffer")?;
    let spec = r.get(o, "spec").as_object();
    let (name, kernel, outputs, params, mut inputs, physics, data) = {
        let h = crate::script::host(r.vm)?;
        let dk = h.kernels.list.get(id)?;
        let inputs: Vec<(String, Arc<Vec<f32>>)> = dk
            .inputs
            .iter()
            .map(|(n, ki, b)| (n.clone(), h.kernels.list[*ki].results.get(b).cloned().unwrap_or_default()))
            .collect();
        (dk.name.clone(), dk.kernel.clone(), dk.outputs.clone(), dk.params.clone(), inputs, dk.physics.clone(), dk.data_values.clone())
    };
    // A kernel over physics records runs over as many as there are, unless
    // it says how many.
    let count = if physics.is_empty() { Some(given.unwrap_or(1)) } else { given };
    if let Some((n, _)) = inputs.iter().find(|(_, d)| d.is_empty()) {
        let loc = r.loc(o);
        r.error(loc, path, format!("kernel `{name}`: its input `{n}` is empty (the kernel it reads must run at load: no `time`, no params)"), None);
        return None;
    }
    // The document's own arrays (`pts: input(vec3, PTS)`), and those it
    // computes every frame (`cams: input(vec4, fn(t) {..})`).
    inputs.extend(data);
    let data_names: Vec<String> = {
        let h = crate::script::host(r.vm)?;
        h.kernels
            .list
            .get(id)?
            .data
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    };
    let mut frame_data = Vec::new();
    if let Some(so) = spec {
        for n in data_names {
            if let Some(index) = r.closure(so, &n) {
                frame_data.push((n, index));
            }
        }
    }
    let params = params
        .iter()
        .map(|p| (p.clone(), match spec {
            Some(so) => r.anim::<1>(so, &format!("{path}.{p}"), p, [0.0], Semantic::Plain),
            None => Anim::Const([0.0]),
        }))
        .collect();
    let buffer = outputs.iter().position(|b| b.name == buffer)?;
    // `active_count: fn(t)`: how many elements run each frame (at most
    // `count`).
    let active = spec
        .filter(|so| {
            let v = r.get(*so, "active_count");
            v.as_number().is_some() || v.as_object().is_some_and(|o| r.vm.bx.heap_ref().is_fn(o))
        })
        .map(|so| r.anim::<1>(so, &format!("{path}.active_count"), "active_count", [0.0], Semantic::Plain));
    Some(crate::kernels::KernelRun { name, kernel, outputs, buffer, count, params, inputs, physics, active, frame_data })
}

fn parse_line(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str) -> Option<NodeDef> {
    r.check_fields(o, path, "Line3D", &with_node_fields(&[
        "points", "curve", "closed", "segments", "width", "world_width", "color", "color_end", "opacity", "trim_start", "trim_end", "dash", "gap", "dash_offset",
        "blending", "polylines", "intensity", "tail", "fade", "source",
    ]));
    let source = if r.has(o, "source") { kernel_source(r, r.get(o, "source"), &format!("{path}.source")) } else { None };
    if r.has(o, "source") && source.is_none() {
        let loc = r.loc(o);
        r.error(loc, &format!("{path}.source"), "Line3D's source is a kernel output of `Segment` records (a, b, color, width)", Some("`let sparks = Kernel{out: emit_buffer(Segment, 64) element: fn(i) { .. }}` then `Line3D{source: sparks.out blending: @additive}`".into()));
        return None;
    }
    let polylines: Vec<Vec<[f32; 3]>> = r
        .list(r.get(o, "polylines"))
        .into_iter()
        .map(|l| r.list(l).into_iter().filter_map(|p| r.plain::<3>(p, Semantic::Plain)).collect::<Vec<_>>())
        .filter(|l| l.len() >= 2)
        .collect();
    let curve = if r.has(o, "curve") {
        parse_curve(r, r.get(o, "curve"), &format!("{path}.curve"))?
    } else {
        let pts: Vec<[f32; 3]> = r.list(r.get(o, "points")).into_iter().filter_map(|p| r.plain::<3>(p, Semantic::Plain)).collect();
        if pts.len() < 2 && polylines.is_empty() && source.is_none() {
            let loc = r.loc(o);
            r.error(loc, path, "Line3D needs `points: [vec3(...), vec3(...)]`, `curve: CatmullRomCurve3{...}` or `polylines: [[...], ...]`", None);
            return None;
        }
        if pts.len() < 2 {
            Curve3::Polyline(Vec::new())
        } else if r.bool(o, "closed", false) {
            let mut p = pts.clone();
            p.push(pts[0]);
            Curve3::Polyline(p)
        } else {
            Curve3::Polyline(pts)
        }
    };
    let def = LineDef {
        curve,
        segments: r.u32(o, path, "segments", 256, 100_000).max(1),
        width: r.anim::<1>(o, path, "width", [3.0], Semantic::Plain),
        world_width: r.bool(o, "world_width", false),
        color: r.anim::<4>(o, path, "color", [1.0; 4], Semantic::Color),
        color_end: if r.has(o, "color_end") { Some(r.anim::<4>(o, path, "color_end", [1.0; 4], Semantic::Color)) } else { None },
        opacity: r.anim::<1>(o, path, "opacity", [1.0], Semantic::Plain),
        trim_start: r.anim::<1>(o, path, "trim_start", [0.0], Semantic::Plain),
        trim_end: r.anim::<1>(o, path, "trim_end", [1.0], Semantic::Plain),
        dash: r.f32(o, path, "dash", 0.0),
        gap: r.f32(o, path, "gap", 0.0),
        dash_offset: r.anim::<1>(o, path, "dash_offset", [0.0], Semantic::Plain),
        blending: if r.id(o, "blending").as_deref() == Some("additive") { Blending::Additive } else { Blending::Normal },
        polylines,
        intensity: r.anim::<1>(o, path, "intensity", [1.0], Semantic::Plain),
        tail: r.anim::<1>(o, path, "tail", [0.0], Semantic::Plain),
        fade: r.plain::<2>(r.get(o, "fade"), Semantic::Plain).map(|f| (f[0], f[1])),
        source,
    };
    Some(base_node(r, cx, o, path, NodeKind::Line(Box::new(def))))
}

fn parse_light(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str, kind: &str) -> Option<NodeDef> {
    let lk = match kind {
        "ambient_light" => LightKind::Ambient,
        "hemisphere_light" => LightKind::Hemisphere,
        "directional_light" => LightKind::Directional,
        "point_light" => LightKind::Point,
        "spot_light" => LightKind::Spot,
        // M1's Light3D{point: true|false}.
        _ => {
            if r.bool(o, "point", false) {
                LightKind::Point
            } else {
                LightKind::Directional
            }
        }
    };
    let type_name = type_name_of(kind);
    let mut known = with_node_fields(&["color", "intensity"]);
    match lk {
        LightKind::Ambient => {}
        LightKind::Hemisphere => known.extend_from_slice(&["sky_color", "ground_color", "ground"]),
        _ => known.extend_from_slice(&[
            "target", "direction", "distance", "decay", "angle", "penumbra", "cast_shadow", "shadow_size", "shadow_bias", "shadow_normal_bias", "shadow_softness",
            "shadow_extent", "point",
        ]),
    }
    r.check_fields(o, path, type_name, &known);
    let color_name = if lk == LightKind::Hemisphere && r.has(o, "sky_color") { "sky_color" } else { "color" };
    let ground_name = if r.has(o, "ground_color") { "ground_color" } else { "ground" };
    let default_intensity = match lk {
        LightKind::Ambient | LightKind::Hemisphere => 0.3,
        LightKind::Directional => 2.5,
        _ => 20.0,
    };
    let casts = r.bool(o, "cast_shadow", false);
    let shadow = if casts && matches!(lk, LightKind::Directional | LightKind::Spot | LightKind::Point) {
        let d = ShadowDef::default();
        Some(ShadowDef {
            size: r.u32(o, path, "shadow_size", d.size, 8192).max(256),
            bias: r.f32(o, path, "shadow_bias", d.bias),
            normal_bias: r.f32(o, path, "shadow_normal_bias", d.normal_bias),
            softness: r.anim::<1>(o, path, "shadow_softness", [2.0], Semantic::Plain),
            extent: r.f32(o, path, "shadow_extent", 0.0),
        })
    } else {
        None
    };
    let light = LightDef {
        kind: lk,
        color: r.anim::<4>(o, path, color_name, [1.0; 4], Semantic::Color),
        ground: r.anim::<4>(o, path, ground_name, [0.25, 0.2, 0.15, 1.0], Semantic::Color),
        intensity: r.anim::<1>(o, path, "intensity", [default_intensity], Semantic::Plain),
        target: r.anim::<3>(o, path, "target", [0.0; 3], Semantic::Plain),
        direction: if r.has(o, "direction") { Some(r.anim::<3>(o, path, "direction", [0.0, -1.0, 0.0], Semantic::Plain)) } else { None },
        distance: r.anim::<1>(o, path, "distance", [0.0], Semantic::Plain),
        decay: r.f32(o, path, "decay", 2.0),
        angle: r.anim::<1>(o, path, "angle", [30.0], Semantic::Plain),
        penumbra: r.anim::<1>(o, path, "penumbra", [0.2], Semantic::Plain),
        shadow,
    };
    let mut node = base_node(r, cx, o, path, NodeKind::Light(light));
    // A directional light without a position shines from above-front.
    if lk == LightKind::Directional && !r.has(o, "position") && !r.has(o, "direction") {
        node.transform.position = Anim::Const([3.0, 6.0, 4.0]);
    }
    Some(node)
}

/// The anims a node owns that are not constant (for static detection).
pub fn node_is_static(n: &NodeDef) -> bool {
    let t = &n.transform;
    t.position.is_static() && t.rotation.is_static() && t.scale.is_static() && n.visible.is_static() && n.opacity.is_static() && n.children.iter().all(node_is_static)
}

// ---------------------------------------------------------------------------
// Material presets, light rigs and the ground
// ---------------------------------------------------------------------------

pub const MATERIAL_PRESETS: &[&str] =
    &["car_paint", "chrome", "brushed_metal", "gold", "copper", "rubber", "plastic", "glass", "frosted_glass", "ceramic", "velvet", "matte"];

fn lin(hex: u32) -> [f32; 4] {
    let c = |s: u32| srgb_to_linear(((hex >> s) & 0xff) as f32 / 255.0);
    [c(16), c(8), c(0), 1.0]
}

/// A physically plausible starting point; the material's own fields
/// (read after it) override any of it.
pub fn apply_preset(m: &mut MaterialDef, name: &str) -> bool {
    let c = |v: f32| Anim::Const([v]);
    m.model = ShadingModel::Standard;
    match name {
        "car_paint" => {
            m.color = Anim::Const(lin(0xa3101c));
            m.metalness = c(0.55);
            m.roughness = c(0.38);
            m.clearcoat = c(1.0);
            m.clearcoat_roughness = c(0.03);
            m.flakes = c(0.35);
            m.flake_scale = 1400.0;
        }
        "chrome" => {
            m.color = Anim::Const(lin(0xf4f4f6));
            m.metalness = c(1.0);
            m.roughness = c(0.06);
        }
        "brushed_metal" => {
            m.color = Anim::Const(lin(0xc9cacd));
            m.metalness = c(1.0);
            m.roughness = c(0.28);
            m.brushed = c(0.3);
            m.brush_scale = 500.0;
        }
        "gold" => {
            m.color = Anim::Const([1.0, 0.71, 0.29, 1.0]);
            m.metalness = c(1.0);
            m.roughness = c(0.18);
        }
        "copper" => {
            m.color = Anim::Const([0.95, 0.64, 0.54, 1.0]);
            m.metalness = c(1.0);
            m.roughness = c(0.25);
        }
        "rubber" => {
            m.color = Anim::Const(lin(0x151515));
            m.roughness = c(0.88);
            m.specular_intensity = c(0.5);
        }
        "plastic" => {
            m.roughness = c(0.32);
            m.clearcoat = c(0.25);
            m.clearcoat_roughness = c(0.2);
        }
        "glass" => {
            m.color = Anim::Const([1.0; 4]);
            m.transmission = c(1.0);
            m.roughness = c(0.03);
            m.ior = c(1.5);
            m.thickness = c(0.6);
            m.side = FaceSide::Double;
        }
        "frosted_glass" => {
            m.color = Anim::Const([1.0; 4]);
            m.transmission = c(1.0);
            m.roughness = c(0.35);
            m.ior = c(1.5);
            m.thickness = c(0.6);
        }
        "ceramic" => {
            m.color = Anim::Const(lin(0xf3f0ea));
            m.roughness = c(0.2);
            m.clearcoat = c(0.8);
            m.clearcoat_roughness = c(0.05);
        }
        "velvet" => {
            m.roughness = c(1.0);
            m.sheen = c(1.0);
            m.sheen_color = m.color.clone();
        }
        "matte" => {
            m.roughness = c(1.0);
            m.specular_intensity = c(0.4);
        }
        _ => return false,
    }
    true
}

fn light_node(path: &str, loc: Loc, name: &str, position: [f32; 3], light: LightDef) -> NodeDef {
    NodeDef {
        name: Some(name.into()),
        id: 0,
        path: format!("{path}.{name}"),
        loc,
        transform: TransformDef { position: Anim::Const(position), ..TransformDef::default() },
        visible: Anim::Const([1.0]),
        opacity: Anim::Const([1.0]),
        span: None,
        cast_shadow: false,
        receive_shadow: false,
        outline: false,
        kind: NodeKind::Light(light),
        children: Vec::new(),
    }
}

/// `LightRig{preset intensity rotation target distance shadows}`: a studio
/// lighting setup as a group of lights (at most 3 direct lights plus a
/// hemisphere), turned about y by `rotation`.
fn parse_light_rig(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str) -> Option<NodeDef> {
    r.check_fields(o, path, "LightRig", &with_node_fields(&["preset", "intensity", "target", "distance", "shadows", "warmth"]));
    let preset = r.id(o, "preset").unwrap_or_else(|| "product".into());
    let k = r.f32(o, path, "intensity", 1.0);
    let d = r.f32(o, path, "distance", 1.0);
    let target = r.plain::<3>(r.get(o, "target"), Semantic::Plain).unwrap_or([0.0, 0.5, 0.0]);
    let shadows = r.bool(o, "shadows", true);
    let warmth = r.f32(o, path, "warmth", 0.0).clamp(-1.0, 1.0);
    let loc = r.loc(o);
    let warm = [1.0, 1.0 - 0.12 * warmth.max(0.0), 1.0 - 0.3 * warmth.max(0.0), 1.0];
    let cool = [1.0 - 0.25 * (-warmth).max(0.0) - 0.1, 1.0 - 0.05, 1.0, 1.0];
    let shadow = |soft: f32| if shadows { Some(ShadowDef { softness: Anim::Const([soft]), ..ShadowDef::default() }) } else { None };
    let spot = |color: [f32; 4], intensity: f32, angle: f32, penumbra: f32, sh: Option<ShadowDef>| LightDef {
        kind: LightKind::Spot,
        color: Anim::Const(color),
        ground: Anim::Const([0.0; 4]),
        intensity: Anim::Const([intensity * k]),
        target: Anim::Const(target),
        direction: None,
        distance: Anim::Const([0.0]),
        decay: 2.0,
        angle: Anim::Const([angle]),
        penumbra: Anim::Const([penumbra]),
        shadow: sh,
    };
    let dir = |color: [f32; 4], intensity: f32, sh: Option<ShadowDef>| LightDef { kind: LightKind::Directional, ..spot(color, intensity / k * k, 30.0, 0.0, sh) };
    let hemi = |sky: [f32; 4], ground: [f32; 4], intensity: f32| LightDef {
        kind: LightKind::Hemisphere,
        ground: Anim::Const(ground),
        ..spot(sky, intensity, 30.0, 0.0, None)
    };
    let point = |color: [f32; 4], intensity: f32, sh: Option<ShadowDef>| LightDef { kind: LightKind::Point, distance: Anim::Const([12.0 * d]), ..spot(color, intensity, 30.0, 0.0, sh) };
    let p = |x: f32, y: f32, z: f32| [x * d + target[0], y * d + target[1], z * d + target[2]];
    let lights: Vec<NodeDef> = match preset.as_str() {
        // Soft key from the front right, cool fill, strong rim from behind.
        "product" | "studio" | "three_point" => vec![
            light_node(path, loc, "key", p(3.5, 5.0, 4.0), dir(warm, 2.2, shadow(4.0))),
            light_node(path, loc, "fill", p(-6.0, 3.5, 3.0), spot(cool, 35.0, 40.0, 1.0, None)),
            light_node(path, loc, "rim", p(-2.5, 4.0, -5.5), spot([1.0; 4], 160.0, 30.0, 0.8, None)),
            light_node(path, loc, "sky", p(0.0, 10.0, 0.0), hemi([0.9, 0.95, 1.0, 1.0], [0.25, 0.22, 0.2, 1.0], 0.25 * k)),
        ],
        // A hard side key, a cold rim and almost no fill.
        "dramatic" => vec![
            light_node(path, loc, "key", p(5.0, 3.0, 1.5), spot(warm, 260.0, 26.0, 0.25, shadow(1.5))),
            light_node(path, loc, "rim", p(-3.0, 3.5, -4.5), spot([0.6, 0.75, 1.0, 1.0], 220.0, 28.0, 0.6, None)),
            light_node(path, loc, "sky", p(0.0, 10.0, 0.0), hemi([0.15, 0.17, 0.22, 1.0], [0.02, 0.02, 0.02, 1.0], 0.12 * k)),
        ],
        // Two rims outlining the silhouette, a faint front fill.
        "rim" => vec![
            light_node(path, loc, "rim_left", p(-4.0, 3.0, -4.0), spot([1.0; 4], 200.0, 30.0, 0.7, None)),
            light_node(path, loc, "rim_right", p(4.0, 3.0, -4.0), spot([1.0; 4], 200.0, 30.0, 0.7, None)),
            light_node(path, loc, "fill", p(0.0, 2.0, 6.0), dir([1.0; 4], 0.35, shadow(3.0))),
            light_node(path, loc, "sky", p(0.0, 10.0, 0.0), hemi([0.2, 0.22, 0.26, 1.0], [0.03, 0.03, 0.03, 1.0], 0.1 * k)),
        ],
        // Magenta and cyan practicals with point-light shadows.
        "neon" => vec![
            light_node(path, loc, "magenta", p(-2.5, 1.6, 1.8), point([1.0, 0.1, 0.6, 1.0], 45.0, shadow(2.0))),
            light_node(path, loc, "cyan", p(2.5, 1.8, -1.5), point([0.1, 0.8, 1.0, 1.0], 45.0, None)),
            light_node(path, loc, "sky", p(0.0, 10.0, 0.0), hemi([0.05, 0.05, 0.1, 1.0], [0.0, 0.0, 0.0, 1.0], 0.2 * k)),
        ],
        other => {
            r.error(loc, &format!("{path}.preset"), format!("no light rig `@{other}`"), Some("one of @product @three_point @dramatic @rim @neon".into()));
            return None;
        }
    };
    let mut node = base_node(r, cx, o, path, NodeKind::Group);
    node.children.extend(lights);
    Some(node)
}

/// `Ground{y radius color roughness metalness reflectivity reflection_blur fade}`:
/// a large disc floor that fades into the background towards its edge,
/// optionally reflecting the scene.
fn parse_ground(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str) -> Option<NodeDef> {
    r.check_fields(o, path, "Ground", &with_node_fields(&["y", "radius", "color", "roughness", "metalness", "reflectivity", "reflection_blur", "fade", "material", "shadow_only", "shadow_opacity"]));
    let radius = r.f32(o, path, "radius", 30.0).max(0.1);
    let y = r.f32(o, path, "y", 0.0);
    let mut material = if r.has(o, "material") {
        parse_material(r, cx, r.get(o, "material"), &format!("{path}.material"))
    } else {
        // Floors take less of the environment than objects: they read as
        // the dark stage they sit on, lit by the rig.
        MaterialDef { roughness: Anim::Const([0.55]), color: Anim::Const(lin(0x202226)), env_intensity: Anim::Const([0.35]), ..MaterialDef::default() }
    };
    if r.has(o, "color") {
        material.color = r.anim::<4>(o, path, "color", [0.1, 0.1, 0.1, 1.0], Semantic::Color);
    }
    if r.has(o, "roughness") {
        material.roughness = r.anim::<1>(o, path, "roughness", [0.55], Semantic::Plain);
    }
    if r.has(o, "metalness") {
        material.metalness = r.anim::<1>(o, path, "metalness", [0.0], Semantic::Plain);
    }
    if r.has(o, "reflectivity") {
        material.reflectivity = r.anim::<1>(o, path, "reflectivity", [0.0], Semantic::Plain);
    }
    if r.has(o, "reflection_blur") {
        material.reflection_blur = r.anim::<1>(o, path, "reflection_blur", [0.2], Semantic::Plain);
    }
    let fade = r.plain::<2>(r.get(o, "fade"), Semantic::Plain).unwrap_or([radius * 0.12, radius * 0.6]);
    material.fade = Some((fade[0], fade[1].max(fade[0] + 1e-3)));
    if r.bool(o, "shadow_only", false) {
        // A shadow catcher: invisible except for the shadows on it.
        material.shadow_catcher = true;
        material.opacity = r.anim::<1>(o, path, "shadow_opacity", [0.6], Semantic::Plain);
    }
    let mut disc = CircleGeo { radius, segments: 128, ..CircleGeo::default() }.build();
    disc.transform(&makepad_geom3d::math::mul(&makepad_geom3d::math::translation([0.0, y, 0.0]), &makepad_geom3d::math::quat_to_mat4(makepad_geom3d::math::quat_from_euler_deg([-90.0, 0.0, 0.0]))));
    let mut node = base_node(r, cx, o, path, NodeKind::Mesh { geometry: GeometrySrc::Static(SharedMesh::new(disc)), material, deform: DeformDef::default() });
    node.cast_shadow = false;
    Some(node)
}

#[allow(clippy::too_many_arguments)]
fn camera_move(r: &mut Reader, o: ScriptObject, path: &str, kind: &str, start: f32, end: f32, amount: Option<f32>, ease: ScriptValue) -> Option<CameraMoveDef> {
    let Some(k) = MoveKind::by_name(kind) else {
        let loc = r.loc(o);
        r.error(loc, &format!("{path}.move"), format!("no camera move `@{kind}`"), Some(format!("one of {}", MoveKind::NAMES.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(" "))));
        return None;
    };
    let default_amount = match k {
        MoveKind::OrbitLeft | MoveKind::OrbitRight => 60.0,
        MoveKind::Roll => 12.0,
        MoveKind::DollyZoom => 0.5,
        MoveKind::Dive => 1.0,
        _ => 0.3,
    };
    let ease = if ease.is_nil() { crate::anim::Ease::InOut(crate::anim::Curve::Cubic) } else {
        match r.ease(ease, path) {
            Ok(e) => e,
            Err(m) => {
                let loc = r.loc(o);
                r.error(loc, &format!("{path}.ease"), m, None);
                crate::anim::Ease::Linear
            }
        }
    };
    Some(CameraMoveDef { kind: k, start, end: end.max(start), amount: amount.unwrap_or(default_amount), ease })
}

/// A scene kit (`Device`, `Turntable`, `Field`, `ShapeSet`): its Splash
/// template (`kits.rs`) builds ordinary nodes from the author's object,
/// wrapped in a group carrying the author's own placement.
fn parse_kit(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str, kind: &str) -> Option<NodeDef> {
    let (fn_name, known) = match crate::kits::template(r, o, kind) {
        Ok(x) => x,
        Err(message) => {
            let loc = r.loc(o);
            r.error(loc, path, message, None);
            return None;
        }
    };
    let mut fields = with_node_fields(known);
    fields.push("preset");
    r.check_fields(o, path, crate::script::type_name_of(kind), &fields);
    let modules = r.vm.modules();
    let module = r.vm.bx.heap_ref().value(modules, id!(scene).into(), NoTrap).as_object()?;
    let f = r.vm.bx.heap_ref().value(module, LiveId::from_str(&fn_name).into(), NoTrap);
    // Kit templates do arithmetic on times (`p.start + p.duration`): give
    // them seconds for `beat(n)` / `bar(n)` at the scene's tempo.
    // (On an object made from the author's: the document's own are frozen.)
    let mut given = o;
    for name in r.own_fields(o) {
        let v = r.get(o, &name);
        if matches!(r.is_anim(v).as_deref(), Some("beat") | Some("bar")) {
            if let Some(sec) = r.number(v) {
                if given == o {
                    given = r.vm.bx.heap_mut().new_with_proto(o.into());
                }
                let k = LiveId::from_str_with_lut(&name).unwrap_or(LiveId::from_str(&name));
                r.vm.bx.heap_mut().set_value_def(given, k.into(), sec.into());
            }
        }
    }
    r.vm.capture_errors();
    let built = r.vm.call(f, &[given.into()]);
    let errors = r.vm.take_errors();
    if !errors.is_empty() || built.is_err() {
        let loc = r.loc(o);
        r.error(loc, path, format!("the {} kit did not build: {}", crate::script::type_name_of(kind), errors.join("; ")), Some("check the kit's parameters (numbers where numbers go)".into()));
        return None;
    }
    let inner = parse_node(r, cx, built, &format!("{path}.kit"))?;
    // The author's placement and timing, without re-reading `children`
    // (the kit already placed them).
    let mut node = base_node(r, cx, o, path, NodeKind::Group);
    node.children = vec![inner];
    Some(node)
}

/// A flat band along a curve (a ribbon), `width` wide (tapering to
/// `width_end` when ≥ 0), turning `twist` degrees over its length.
fn ribbon_mesh(curve: &Curve3, width: f32, width_end: f32, segments: u32, twist: f32) -> Mesh {
    let arc = curve.arc();
    let frames = arc.frames(segments as usize + 1);
    let mut m = Mesh::new();
    let n = frames.len().max(2);
    for (i, fr) in frames.iter().enumerate() {
        let u = i as f32 / (n - 1) as f32;
        let w = if width_end >= 0.0 { width + (width_end - width) * u } else { width } * 0.5;
        let a = twist * u * makepad_geom3d::math::DEG;
        let (s, c) = a.sin_cos();
        let side = makepad_geom3d::mesh::add3(makepad_geom3d::mesh::scale3(fr.binormal, c), makepad_geom3d::mesh::scale3(fr.normal, s));
        let up = makepad_geom3d::mesh::normalize_or_up(makepad_geom3d::mesh::cross3(fr.tangent, side));
        m.vertex(makepad_geom3d::mesh::add3(fr.position, makepad_geom3d::mesh::scale3(side, -w)), up, [u, 0.0]);
        m.vertex(makepad_geom3d::mesh::add3(fr.position, makepad_geom3d::mesh::scale3(side, w)), up, [u, 1.0]);
        if i > 0 {
            let b = (i as u32) * 2;
            m.quad(b - 2, b - 1, b + 1, b);
        }
    }
    m
}

/// The surface of some text (for text layouts and emitters): `text font
/// size depth` on the object.
fn text_points_mesh(r: &mut Reader, cx: &mut LoadCx, o: ScriptObject, path: &str) -> Option<Arc<Mesh>> {
    let text = r.string(o, "text").unwrap_or_else(|| "TEXT".into());
    let font = match r.id(o, "font") {
        Some(name) => FontSource::Bundled(name),
        None => FontSource::Bundled("bold".into()),
    };
    let _ = cx;
    let params = Text3dParams { text, font, size: r.f32(o, path, "size", 1.0), depth: r.f32(o, path, "depth", 0.1), ..Text3dParams::default() };
    match build_text3d(&params) {
        Ok(t) => Some(Arc::new(t.merged())),
        Err(e) => {
            let loc = r.loc(o);
            r.error(loc, path, format!("the text did not build: {e}"), None);
            None
        }
    }
}

/// Points on a lattice inside a closed mesh, `res` cells along its longest
/// side (inside = an odd number of crossings along +x).
pub(crate) fn voxelize(mesh: &Mesh, res: u32) -> Vec<[f32; 3]> {
    let Some((mn, mx)) = mesh.bounds() else { return Vec::new() };
    let span = (mx[0] - mn[0]).max(mx[1] - mn[1]).max(mx[2] - mn[2]).max(1e-6);
    let cell = span / res as f32;
    let tris: Vec<[[f32; 3]; 3]> = mesh.indices.chunks_exact(3).map(|t| [mesh.positions[t[0] as usize], mesh.positions[t[1] as usize], mesh.positions[t[2] as usize]]).collect();
    let mut out = Vec::new();
    let steps = |a: f32, b: f32| (((b - a) / cell).floor() as i32).max(0) + 1;
    let (nx, ny, nz) = (steps(mn[0], mx[0]), steps(mn[1], mx[1]), steps(mn[2], mx[2]));
    for iz in 0..nz {
        for iy in 0..ny {
            let y = mn[1] + (iy as f32 + 0.5) * cell;
            let z = mn[2] + (iz as f32 + 0.5) * cell;
            // Crossings of the +x ray from (-inf, y, z) with every triangle.
            let mut xs: Vec<f32> = Vec::new();
            for t in &tris {
                let (a, b, c) = (t[0], t[1], t[2]);
                let d = (b[1] - a[1]) * (c[2] - a[2]) - (c[1] - a[1]) * (b[2] - a[2]);
                if d.abs() < 1e-12 {
                    continue;
                }
                let l1 = ((b[1] - y) * (c[2] - z) - (c[1] - y) * (b[2] - z)) / d;
                let l2 = ((c[1] - y) * (a[2] - z) - (a[1] - y) * (c[2] - z)) / d;
                let l3 = 1.0 - l1 - l2;
                if l1 >= 0.0 && l2 >= 0.0 && l3 >= 0.0 {
                    xs.push(l1 * a[0] + l2 * b[0] + l3 * c[0]);
                }
            }
            xs.sort_by(|p, q| p.partial_cmp(q).unwrap());
            xs.dedup_by(|p, q| (*p - *q).abs() < 1e-5);
            for ix in 0..nx {
                let x = mn[0] + (ix as f32 + 0.5) * cell;
                if xs.iter().filter(|&&v| v < x).count() % 2 == 1 {
                    out.push([x, y, z]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod resolver_tests {
    use super::*;

    #[test]
    fn file_resolver_keeps_documents_inside_their_directory() {
        let base = std::env::temp_dir().join(format!("motion3d_file_resolver_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("doc/lib")).unwrap();
        std::fs::write(base.join("doc/tex.png"), b"png").unwrap();
        std::fs::write(base.join("doc/lib/abc.hdr"), b"hdr").unwrap();
        std::fs::write(base.join("secret.txt"), b"secret").unwrap();
        let r = FileResolver { base: base.join("doc"), assets: Default::default(), library: Some(base.join("doc/lib")) };
        assert_eq!(&**r.load(&MediaRef::File("tex.png".into())).unwrap(), b"png");
        let outside = base.join("secret.txt").display().to_string();
        for p in ["../secret.txt", outside.as_str(), "lib/../../secret.txt", "/etc/passwd"] {
            assert!(r.load(&MediaRef::File(p.into())).is_err(), "{p}");
        }
        assert_eq!(&**r.load(&MediaRef::Asset { id: "abc".into(), rev: "1".into() }).unwrap(), b"hdr");
        for id in ["../secret", "../../secret", ".hidden", "a/b"] {
            assert!(r.load(&MediaRef::Asset { id: id.into(), rev: "1".into() }).is_err(), "{id}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("secret.txt"), base.join("doc/link.png")).unwrap();
            assert!(r.load(&MediaRef::File("link.png".into())).is_err());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn bundled_fonts_load_by_absolute_path_and_nothing_else_does() {
        // How Motion's Text3D lowering names a font: an absolute path into
        // the engine's font directories (spelled with `..`).
        let r = FileResolver { base: PathBuf::from("/"), assets: Default::default(), library: None };
        let bundled = makepad_text_mesh::letters::fonts_dir().join("IBMPlexSans-SemiBold.ttf");
        assert!(r.load(&MediaRef::File(bundled.display().to_string())).unwrap().len() > 1000);
        let motion_fonts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fonts/resources/fonts");
        trust_resource_root(&motion_fonts);
        let motion = motion_fonts.join("anton/Anton-Regular.ttf");
        assert!(r.load(&MediaRef::File(motion.display().to_string())).unwrap().len() > 1000);
        // Outside the trusted roots an absolute path stays refused.
        let beside = makepad_text_mesh::letters::fonts_dir().join("../Cargo.toml");
        assert!(r.load(&MediaRef::File(beside.display().to_string())).is_err());
        assert!(r.load(&MediaRef::File("/etc/hosts".into())).is_err());
    }
}

#[cfg(test)]
mod pass_uniform_tests {
    use super::*;
    use crate::eval::{EvalCache, EvalInput};

    #[test]
    fn a_pass_uniform_fn_giving_a_vector_is_a_vector_uniform() {
        let resolver = FileResolver::default();
        let src = "use mod.scene.*; use mod.post; Scene3D{ post: [post.Pass{uniforms: {k: fn(t) { return t * 2.0 } v: fn(t) { return vec4(t, 1.0, 2.0, 3.0) } w: fn(t) { vec3(t, t, 5.0) }} pixel: fn() -> vec4 { return self.color.sample(self.uv()) * self.k + self.v + vec4(self.w, 0.0) }}] children: [Mesh{geometry: BoxGeometry{}}] }";
        let mut doc = Scene3dDoc::load(src, LoadOptions { name: "test".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None }).unwrap_or_else(|e| panic!("{e:?}"));
        let mut cache = EvalCache::default();
        let f = doc.eval(&EvalInput { t: 1.5, aspect: 16.0 / 9.0, audio: &crate::anim::NoAudio, edit_offset: 0.0 }, &mut cache);
        let (decls, values) = f.post.graph.clone().expect("graph passes");
        let widths: Vec<u8> = decls[0].uniforms.iter().map(|u| u.width).collect();
        assert_eq!(widths, vec![1, 4, 3]);
        assert_eq!(values[0][0][0], 3.0);
        assert_eq!(&values[0][1][..4], &[1.5, 1.0, 2.0, 3.0]);
        assert_eq!(&values[0][2][..3], &[1.5, 1.5, 5.0]);
    }

    #[test]
    fn documents_load_on_workers_and_draw_here() {
        // Loaded on two other threads, drawn on this one, and a third
        // dropped on a worker while this one draws.
        let scene = |k: &str| format!("use mod.scene.*; use mod.post; let base = 2.0\nlet world = [base, 3.0, 4.0]\nlet f = fn(t) {{ t * world[0] }}\nScene3D{{ post: [post.Pass{{uniforms: {{k: fn(t) {{ return f(t) * in(@k, 1.0) + {k} }}}} pixel: fn() -> vec4 {{ return self.color.sample(self.uv()) * self.k }}}}] children: [Mesh{{geometry: BoxGeometry{{}}}}] }}");
        let load = |src: String| {
            std::thread::spawn(move || {
                let resolver = FileResolver::default();
                let inputs = HashMap::from([("k".to_string(), InputValue::Number(3.0))]);
                Scene3dDoc::load(&src, LoadOptions { name: "test".into(), inputs, resolver: &resolver, first_line: 1, origin_file: None, song: None }).unwrap_or_else(|e| panic!("{e:?}"))
            })
            .join()
            .unwrap()
        };
        let k = |doc: &mut Scene3dDoc| {
            let mut cache = EvalCache::default();
            let f = doc.eval(&EvalInput { t: 0.5, aspect: 1.0, audio: &crate::anim::NoAudio, edit_offset: 0.0 }, &mut cache);
            f.post.graph.clone().expect("graph passes").1[0][0][0]
        };
        let mut one = load(scene("1.0"));
        let mut two = load(scene("11.0"));
        assert_eq!(k(&mut one), 4.0);
        assert_eq!(k(&mut two), 14.0);
        let three = load(scene("21.0"));
        let dropped = std::thread::spawn(move || drop(three));
        for _ in 0..50 {
            assert_eq!(k(&mut one), 4.0);
        }
        dropped.join().unwrap();
        assert_eq!(k(&mut two), 14.0);
    }

    #[test]
    fn inputs_read_only_by_closures_reach_the_loaded_document() {
        let resolver = FileResolver::default();
        let src = "use mod.scene.*; use mod.post; let s = in(@s, 1.0)\nScene3D{ post: [post.Pass{uniforms: {k: fn(t) { return in(@k, 1.0) * s }} pixel: fn() -> vec4 { return self.color.sample(self.uv()) * self.k }}] children: [Mesh{geometry: BoxGeometry{}}] }";
        let with = |k: f64, s: f64| HashMap::from([("k".to_string(), InputValue::Number(k)), ("s".to_string(), InputValue::Number(s))]);
        let mut doc = Scene3dDoc::load(src, LoadOptions { name: "test".into(), inputs: with(2.0, 3.0), resolver: &resolver, first_line: 1, origin_file: None, song: None }).unwrap_or_else(|e| panic!("{e:?}"));
        let k_at = |doc: &mut Scene3dDoc| {
            let mut cache = EvalCache::default();
            let f = doc.eval(&EvalInput { t: 0.5, aspect: 1.0, audio: &crate::anim::NoAudio, edit_offset: 0.0 }, &mut cache);
            f.post.graph.clone().expect("graph passes").1[0][0][0]
        };
        assert_eq!(k_at(&mut doc), 6.0);
        // `k` is read per frame: the loaded document takes a new value.
        assert!(doc.takes_inputs(&with(2.0, 3.0), &with(5.0, 3.0)));
        doc.set_inputs(with(5.0, 3.0));
        assert_eq!(k_at(&mut doc), 15.0);
        // `s` was read at load: a new value needs a new load.
        assert!(!doc.takes_inputs(&with(5.0, 3.0), &with(5.0, 4.0)));
    }

    #[test]
    fn a_kernel_of_segments_is_a_line_batch() {
        let resolver = FileResolver::default();
        let src = "use mod.scene.*; use mod.kernel; let k = Kernel{count: 3 out: kernel.output(kernel.Segment) element: fn(i) { out[i].a = vec3(float(i), 0.0, 0.0) out[i].b = vec3(float(i), time, 0.0) out[i].color = vec4(2.0, 1.0, 0.5, if i == 1 { 0.0 } else { 1.0 }) out[i].width = 0.5 }}\nScene3D{ children: [Line3D{source: k.out width: 4 blending: @additive}] }";
        let mut doc = Scene3dDoc::load(src, LoadOptions { name: "test".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None }).unwrap_or_else(|e| panic!("{e:?}"));
        let mut cache = EvalCache::default();
        let f = doc.eval(&EvalInput { t: 2.0, aspect: 16.0 / 9.0, audio: &crate::anim::NoAudio, edit_offset: 0.0 }, &mut cache);
        assert!(f.errors.is_empty(), "{:?}", f.errors);
        let l = &f.lines[0];
        // Two segments (the transparent one is dropped): ends, HDR colour, width 4 x 0.5.
        assert_eq!(l.segments.len(), 2);
        assert_eq!((l.segments[0].a, l.segments[0].b), ([0.0, 0.0, 0.0], [0.0, 2.0, 0.0]));
        assert_eq!(l.segments[0].color_a, [2.0, 1.0, 0.5, 1.0]);
        assert_eq!(l.segments[0].width_a, 2.0);
        assert_eq!(l.segments[1].a[0], 2.0);
    }
}

#[cfg(test)]
mod environment_tests {
    //! N6: Scene3D takes the HDRI generator's presets by name and its
    //! exported files the way it made them.
    use super::*;
    use makepad_platform::resource_resolver::DecodeBudget;
    use makepad_hdri_engine::{self as hdri, export, presets, HDRI_PREFIX, PRESET_ENVIRONMENTS, PROCEDURAL_HDRI_WIDTH};
    use makepad_render::PROCEDURAL_ENVIRONMENTS;

    const MESH: &str = "children: [Mesh{geometry: BoxGeometry{}}]";
    /// A scene names the scene module's types once it imports them.
    const SCENE3D: &str = "use mod.scene.*; Scene3D";

    fn load_in(base: PathBuf, src: &str) -> Result<Scene3dDoc, String> {
        let resolver = FileResolver { base, ..Default::default() };
        Scene3dDoc::load(src, LoadOptions { name: "test".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None })
            .map_err(|e| e.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"))
    }

    fn serial(n: usize, f: &(dyn Fn(usize) + Sync)) {
        (0..n).for_each(f)
    }

    fn angle_deg(a: [f32; 3], b: [f32; 3]) -> f32 {
        let len = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        let cos = (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]) / (len(a) * len(b));
        cos.clamp(-1.0, 1.0).acos().to_degrees()
    }

    fn spellings(name: &str) -> [String; 2] {
        [format!("{SCENE3D}{{environment: @{name} {MESH}}}"), format!("{SCENE3D}{{environment: Environment{{preset: @{name}}} {MESH}}}")]
    }

    #[test]
    fn every_environment_name_resolves() {
        // Each name stands for its own environment, by the load's own lookup:
        // a look, or the generator preset of that name. Nothing is baked: a
        // generator preset's map is checked once, for one preset, in
        // a_generator_preset_loads_as_the_generators_map (a document's two
        // spellings read every name the same way).
        let mut sources = std::collections::HashSet::new();
        for &name in PROCEDURAL_ENVIRONMENTS.iter().chain(PRESET_ENVIRONMENTS) {
            let source = environment_source(name).unwrap_or_else(|| panic!("@{name} does not resolve"));
            match (&source, name.strip_prefix(HDRI_PREFIX)) {
                // A look (render_materials_looks_keep_their_names_and_maps).
                (EnvSource::Preset(_), None) => {}
                (EnvSource::HdriPreset(n), Some(_)) => assert_eq!(*n, name),
                (source, _) => panic!("@{name}: {source:?}"),
            }
            // The renderer holds each map under a key made from its source:
            // no two names share one.
            assert!(sources.insert(format!("{source:?}")), "@{name} is another name's environment: {source:?}");
        }
    }

    #[test]
    fn a_generator_preset_loads_as_the_generators_map() {
        // One preset for all of them (every_environment_name_resolves): a
        // studio, whose bake is the quickest.
        let name = "hdri_three_point";
        let reference = hdri::envmap::bake_env_map(&presets::preset(name.strip_prefix(HDRI_PREFIX).unwrap()).unwrap(), PROCEDURAL_HDRI_WIDTH, serial).0;
        let seen: Vec<EnvironmentDef> = spellings(name).iter().map(|src| {
            let doc = load_in(PathBuf::new(), src).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert!(doc.warnings.is_empty(), "{src}: {:?}", doc.warnings);
            let env = doc.def.environment.clone().unwrap_or_else(|| panic!("{src}: no environment"));
            assert!(matches!(env.source, EnvSource::HdriPreset(n) if n == name), "{src}: {:?}", env.source);
            // The generator's map at `PROCEDURAL_HDRI_WIDTH`.
            assert!(*env.map == reference, "{src}: not the preset's {PROCEDURAL_HDRI_WIDTH}-wide bake");
            env
        }).collect();
        // Both spellings are one environment, and a document loaded again
        // bakes nothing: the map is the one baked before.
        assert_eq!(seen[0].key, seen[1].key, "two keys");
        assert!(Arc::ptr_eq(&seen[0].map, &seen[1].map), "baked twice");
    }

    #[test]
    fn render_materials_looks_keep_their_names_and_maps() {
        // The table starts with the looks, by the names they always had.
        assert_eq!(&PROCEDURAL_ENVIRONMENTS[..EnvPreset::NAMES.len()], EnvPreset::NAMES);
        for &name in EnvPreset::NAMES {
            let look = EnvPreset::by_name(name).unwrap();
            // `Environment{preset: @gradient}` takes its own top, horizon and
            // bottom (their defaults here), as it always did.
            let long = if name == "gradient" { EnvPreset::Gradient { top: [0.2, 0.3, 0.6], horizon: [0.8, 0.8, 0.9], bottom: [0.1, 0.1, 0.1] } } else { look.clone() };
            for (src, want, blur) in spellings(name).into_iter().zip([look, long]).zip([0.5, 0.0]).map(|((s, w), b)| (s, w, b)) {
                let env = load_in(PathBuf::new(), &src).unwrap_or_else(|e| panic!("{src}: {e}")).def.environment.clone().expect("an environment");
                assert!(matches!(&env.source, EnvSource::Preset(p) if *p == want), "{src}: {:?}", env.source);
                assert!(*env.map == EnvMap::procedural(&want, 512, 1.0, 0.0), "{src}: not the look's map");
                assert_eq!(env.blur, blur, "{src}");
            }
        }
    }

    #[test]
    fn an_unknown_environment_name_offers_every_name() {
        let err = load_in(PathBuf::new(), &format!("{SCENE3D}{{environment: Environment{{preset: @golden_hour}} {MESH}}}")).err().expect("an unknown name is an error");
        assert!(err.contains("no environment preset `@golden_hour`"), "{err}");
        let offered: Vec<&str> = err.lines().find_map(|l| l.trim().strip_prefix("hint: one of ")).unwrap_or_else(|| panic!("no list of names: {err}")).split(' ').collect();
        let every: Vec<String> = PROCEDURAL_ENVIRONMENTS.iter().chain(PRESET_ENVIRONMENTS).map(|n| format!("@{n}")).collect();
        assert_eq!(offered, every);
    }

    #[test]
    fn a_generator_export_loads_with_its_sun_where_the_generator_put_it() {
        let params = presets::preset("Golden hour").unwrap();
        let (baked, sun) = hdri::envmap::bake_env_map(&params, 64, serial);
        let sun = sun.expect("golden hour has a sun").dir;
        let bytes = export::encode_hdr(&baked);
        let dir = std::env::temp_dir().join(format!("motion3d_hdr_round_trip_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("golden.hdr"), &bytes).unwrap();
        let doc = load_in(dir.clone(), &format!("{SCENE3D}{{environment: Environment{{hdri: file(\"golden.hdr\")}} {MESH}}}"));
        let _ = std::fs::remove_dir_all(&dir);
        let mut doc = doc.unwrap_or_else(|e| panic!("{e}"));
        let env = doc.def.environment.take().expect("an environment");
        assert!(matches!(env.source, EnvSource::Hdri(_)));
        // The sun where the generator put it, to within one texel.
        let texel = 360.0 / 64.0;
        let seen = env.map.brightest_direction();
        let off = angle_deg(seen, [sun.x, sun.y, sun.z]);
        assert!(off < texel, "the sun is {off} degrees from the generator's ({seen:?} for {sun:?})");
        // And the whole map, texel for texel, to the file's RGBE precision
        // (a mantissa of 8 bits under the texel's largest channel).
        assert_eq!((env.map.width, env.map.height), (baked.width, baked.height));
        for (i, (got, want)) in env.map.data.iter().zip(&baked.data).enumerate() {
            let m = want[0].max(want[1]).max(want[2]);
            for c in 0..3 {
                assert!((got[c] - want[c]).abs() <= m / 100.0 + 1.0e-30, "texel ({}, {}): {got:?}, the generator's {want:?}", i % 64, i / 64);
            }
        }
        // MOTION3D.md's note for a document written before: Scene3D read a
        // file's columns as they were, so its sun stood a quarter turn off,
        // and `rotation: 90` (the engine's turn, `hdri::rotate_y`) puts the
        // new reading back where the old one was.
        let before = makepad_render_material::ibl::load_hdr(&bytes, &mut DecodeBudget::default()).unwrap();
        let turned = hdri::rotate_y(makepad_widgets::vec3f(seen[0], seen[1], seen[2]), 90.0);
        let off = angle_deg([turned.x, turned.y, turned.z], before.brightest_direction());
        assert!(off < texel, "rotation 90 leaves the sun {off} degrees from where documents had it");
    }

    #[test]
    fn the_turned_copy_is_charged_while_the_file_is_held() {
        // A file that fits the load's budget once, but not twice, does not
        // turn: the copy is decoded data like the file. With room for both,
        // only the turned map stays charged.
        let (map, _) = hdri::envmap::bake_env_map(&presets::preset("Three-point").unwrap(), 16, serial);
        let bytes = export::encode_hdr(&map);
        let mut once = DecodeBudget::default();
        makepad_render_material::ibl::load_hdr(&bytes, &mut once).unwrap();
        let image = once.used();
        assert_eq!(image, 16 * 8 * 16);
        // load_hdr's own staging (4 B a texel and a 4 B scanline) comes and
        // goes before the copy.
        let staging = 16 * 8 * 4 + 16 * 4;
        let mut tight = DecodeBudget::new(image + staging);
        assert!(load_hdr_engine(&bytes, &mut tight).is_err(), "turned without room for the copy");
        assert_eq!(tight.used(), 0, "a refused turn gives the file back too");
        let mut room = DecodeBudget::new(2 * image);
        load_hdr_engine(&bytes, &mut room).unwrap();
        assert_eq!(room.used(), image);
    }
}
