//! Full three.js-level Scene3D layers, drawn by lane R3's motion3d
//! (`makepad_stage_motion3d::layer::Scene3dLayer`): the scene (an object of
//! the document's own VM, read there) is loaded once per layer (with the
//! host's files), evaluated at the layer's local time with audio read on the
//! host's clock through the document's signals, and recorded inside a
//! container pass of this frame. Every motion3d pass hangs under that
//! container, so the container (which copies motion3d's premultiplied
//! output) paints after them and sits in the frame's pass order like any
//! prepared layer.

use super::{target_format, MotionDraw};
use crate::coverage::{CoverNode, SceneCoverage};
use makepad_motion::frame::{Affine, Item, ItemKind};
use makepad_motion::model::{InputValue, SceneSource};
use makepad_motion::scene::{scene_inputs, template_inputs, SceneRequest};
use makepad_motion::signals::SignalSource;
use makepad_stage_motion3d::anim::{AudioFeature, AudioSource};
use makepad_stage_motion3d::doc::{MediaResolver, Scene3dDoc};
use makepad_stage_motion3d::eval::EvalCache;
use makepad_stage_motion3d::layer::{FrameTime, Scene3dLayer};
use makepad_stage_motion3d::scene::{NodeDef, NodeKind};
use makepad_stage_motion3d::script::{InputValue as M3Input, MediaRef};
use makepad_widgets::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(crate) struct Scene {
    key: u64,
    /// What its load read (see [`LoadKey`]): it goes back to [`LOADED`]
    /// under this when it leaves.
    load: LoadKey,
    /// Its source's id and the inputs it shows (see [`take_inputs`]).
    source_id: u64,
    inputs: HashMap<String, M3Input>,
    /// Recordings in a row that did not draw it ([`SCENE_IDLE_FRAMES`]).
    idle: u32,
    layer: Option<Scene3dLayer>,
    container: RenderTarget,
    /// Every pass motion3d recorded for this layer.
    owned: HashSet<DrawPassId>,
    /// The repaint a later sub-frame (motion blur, temporal AA) was
    /// recorded in: the container must paint after it, since motion3d
    /// writes its output only with the frame's last chain.
    resampled_at: Option<u64>,
    /// The document's signals: sub-frames are evaluated as they are
    /// recorded, with the frame's audio.
    signals: Option<std::sync::Arc<dyn SignalSource>>,
}

/// Recordings a Scene3D layer keeps its renderer (and its targets) without
/// being drawn (a scene is ~30 full-size post targets, 0.5 GB at 2880x1620: a scrub or a seek
/// visits a scene per click, so a scene that left goes within a few frames; its document stays
/// for the return): a layer that left the frame gives them back, and loads
/// again if it returns.
const SCENE_IDLE_FRAMES: u32 = 3;

#[derive(Default)]
pub(crate) struct Scenes3d {
    scenes: HashMap<String, RefCell<Scene>>,
    /// Containers of scenes that went, their textures released, for the
    /// next scene (their passes stay in the pass tree).
    spare: Vec<RenderTarget>,
    /// Library files by `id@rev` (and `id`), for the scenes' `asset(…)`.
    pub files: HashMap<String, Arc<Vec<u8>>>,
    /// The scenes the last recorded frame drew.
    drawn: Vec<String>,
    /// Where each scene sits: its box → document map and box size.
    placed: HashMap<String, (Affine, [f64; 2])>,
    errors: Vec<String>,
    /// Each template scene's key for its source (by pointer: the document
    /// shares one source every frame) and inputs: hashing a large scene's
    /// source every frame was most of a preview frame's drawing time.
    keys: HashMap<String, (Arc<str>, String, u64, u64)>,
    /// Draw each scene's coverage id map ([`MotionDraw::set_scene_coverage`]).
    coverage: bool,
}

/// Everything a scene's load reads that can differ between two layers: its
/// source and inputs (the reload key), its layer path and first line (its
/// diagnostics) and its start on the edit's clock (its song timing).
#[derive(Clone, PartialEq, Eq, Hash)]
struct LoadKey {
    key: u64,
    path: String,
    first_line: u32,
    edit_offset: u64,
}

thread_local! {
    /// Scene documents loaded and not drawing: a layer that leaves (its draw
    /// dropped, or idle) puts its document back here, and [`preload`] loads
    /// ahead of the first draw; a layer that starts takes one with its key
    /// instead of loading (loading is the scene's Splash evaluation and kernel
    /// lowering: 100-800 ms on a browser's UI thread). One per scene layer of
    /// the film (its path, first line and start): a document loaded with other
    /// inputs replaces it. A kept renderer holds only its static meshes'
    /// geometry, while the host keeps it ([`keep_geometry`]).
    static LOADED: RefCell<HashMap<(String, u32, u64), Kept>> = RefCell::new(HashMap::new());
    /// Scenes (reload keys) whose preload failed: not tried again ahead
    /// (their draw loads them and reports why).
    static FAILED: RefCell<std::collections::HashSet<u64>> = RefCell::new(std::collections::HashSet::new());
    /// Scenes (slots) loading on a worker: not asked for again meanwhile.
    static LOADING: RefCell<std::collections::HashSet<(String, u32, u64)>> = RefCell::new(std::collections::HashSet::new());
}

/// A kept scene document: its reload key, source id and inputs.
struct Kept {
    key: u64,
    source_id: u64,
    inputs: HashMap<String, M3Input>,
    layer: Scene3dLayer,
}

/// Whether a scene document loaded with `old` inputs shows `new` without a
/// new load (only its per-frame closures read the inputs that differ), and
/// gives them to it if so: an animated input reloaded the scene (20 ms and
/// more on a browser's UI thread) whenever its value changed.
fn take_inputs(layer: &mut Scene3dLayer, old: &HashMap<String, M3Input>, new: &HashMap<String, M3Input>) -> bool {
    if old == new {
        return true;
    }
    if !layer.doc.takes_inputs(old, new) {
        return false;
    }
    layer.doc.set_inputs(new.clone());
    true
}

/// A scene's start on the edit's clock as a key: in microseconds. The host
/// computes it as edit time minus local time, which differs in the last bits
/// from frame to frame (29.782 and 29.781999999999996): keyed by its bits,
/// a scene where nothing changed loaded again on every sample.
fn offset_key(edit_offset: f64) -> u64 {
    (edit_offset * 1e6).round() as i64 as u64
}

fn slot(key: &LoadKey) -> (String, u32, u64) {
    (key.path.clone(), key.first_line, key.edit_offset)
}

/// The kept document of this layer when it shows `inputs` (given to it).
fn loaded_take(key: &LoadKey, source_id: u64, inputs: &HashMap<String, M3Input>) -> Option<Scene3dLayer> {
    LOADED.with(|l| {
        let mut l = l.borrow_mut();
        let slot = slot(key);
        let fits = l.get_mut(&slot).is_some_and(|kept| kept.key == key.key || kept.source_id == source_id && take_inputs(&mut kept.layer, &kept.inputs, inputs));
        if fits {
            return l.remove(&slot).map(|kept| kept.layer);
        }
        None
    })
}

fn loaded_has(key: &LoadKey, source_id: u64, inputs: &HashMap<String, M3Input>) -> bool {
    LOADED.with(|l| l.borrow().get(&slot(key)).is_some_and(|kept| kept.key == key.key || kept.source_id == source_id && kept.layer.doc.takes_inputs(&kept.inputs, inputs)))
}

/// Keep a layer's document for the next layer of its key; of its renderer
/// only the static meshes' geometry stays.
fn loaded_put(key: LoadKey, source_id: u64, inputs: HashMap<String, M3Input>, mut layer: Scene3dLayer) {
    layer.renderer.reset_keeping_geometry();
    LOADED.with(|l| {
        l.borrow_mut().insert(slot(&key), Kept { key: key.key, source_id, inputs, layer });
    });
}

/// The kept scenes whose start (on the edit's clock) `keep` refuses give
/// their geometry back (their documents stay).
pub(crate) fn keep_geometry(keep: &dyn Fn(f64) -> bool) {
    LOADED.with(|l| {
        for ((_, _, offset), kept) in l.borrow_mut().iter_mut() {
            if !keep(*offset as i64 as f64 / 1e6) {
                kept.layer.renderer.drop_geometry();
            }
        }
    });
}

/// The kept scenes whose start (on the edit's clock) `keep` refuses go,
/// document and all: their renderers here, their documents (many MB of
/// script heap each) returned, for the host to drop off the UI thread.
pub(crate) fn keep_scenes(keep: &dyn Fn(f64) -> bool) -> Vec<SceneDoc> {
    LOADED.with(|l| {
        let mut l = l.borrow_mut();
        let gone: Vec<_> = l.keys().filter(|(_, _, offset)| !keep(*offset as i64 as f64 / 1e6)).cloned().collect();
        gone.into_iter().filter_map(|k| l.remove(&k)).map(|kept| {
            let (doc, cache) = kept.layer.into_doc();
            SceneDoc(doc, cache)
        }).collect()
    })
}

/// The reload key of a template scene's source and inputs (the document's
/// tempo added as `bpm` unless the scene sets one), and those inputs.
/// Also the source's own id.
fn scene_key(source: &str, values: &[(String, InputValue)], bpm: f64) -> (Vec<(String, InputValue)>, String, u64, u64) {
    let values = template_inputs(values, bpm);
    let shown = format!("{values:?}");
    let source_id = LiveId::from_str(source);
    let key = source_id.bytes_append(shown.as_bytes()).0;
    (values, shown, key, source_id.0)
}

/// What a scene item loads and keeps by: its reload key, its source's id
/// and the inputs it shows. A scene of the document's VM is its object (the
/// document's inputs are baked into the VM, another document is another
/// object); a template scene is its source and inputs.
fn scene_identity(keys: &mut HashMap<String, (Arc<str>, String, u64, u64)>, path: &str, scene: &SceneSource, bpm: f64) -> (u64, u64, HashMap<String, M3Input>) {
    match scene {
        SceneSource::Object(object) => {
            let key = object.key();
            (key, key, HashMap::new())
        }
        SceneSource::Template { source, inputs: values } => {
            let values = template_inputs(values, bpm);
            let shown = format!("{values:?}");
            let (key, source_id) = match keys.get(path) {
                Some((src, v, key, id)) if Arc::ptr_eq(src, source) && *v == shown => (*key, *id),
                Some((src, _, _, id)) if Arc::ptr_eq(src, source) => {
                    // Only the inputs changed: the source's id stands.
                    let id = *id;
                    let key = LiveId(id).bytes_append(shown.as_bytes()).0;
                    keys.insert(path.to_string(), (source.clone(), shown, key, id));
                    (key, id)
                }
                _ => {
                    let (_, shown, key, id) = scene_key(source, &values, bpm);
                    keys.insert(path.to_string(), (source.clone(), shown, key, id));
                    (key, id)
                }
            };
            (key, source_id, scene_inputs(&values))
        }
    }
}

/// Load a scene's document (a template's modules through the document's
/// resolver).
fn load_scene(path: &str, request: &SceneRequest, files: &HashMap<String, Arc<Vec<u8>>>) -> Result<Scene3dLayer, Vec<makepad_stage_motion3d::script::Diagnostic>> {
    request.load(path, &Files(files)).map(|doc| Scene3dLayer::from_doc(doc, Default::default()))
}

/// A scene document that left (with its evaluation cache): many MB of
/// script heap, for the host to drop off the UI thread.
pub struct SceneDoc(pub Scene3dDoc, pub EvalCache);

/// A Scene3D a frame shows that is not loaded yet: its document's load and
/// first evaluation, for a worker ([`SceneLoad::run`]); the UI thread then
/// adopts it ([`crate::MotionDraw::adopt_scene`]).
pub struct SceneLoad {
    load: LoadKey,
    source_id: u64,
    shown: HashMap<String, M3Input>,
    request: SceneRequest,
    files: HashMap<String, Arc<Vec<u8>>>,
    input_t: f32,
    aspect: f32,
}

/// A scene loaded on a worker, for the UI thread to adopt.
pub struct SceneLoaded {
    load: LoadKey,
    source_id: u64,
    shown: HashMap<String, M3Input>,
    /// The document and what its first evaluation built; None: it failed
    /// (its draw loads it and says why).
    doc: Option<(Scene3dDoc, EvalCache)>,
}

impl SceneLoad {
    /// Load the document and evaluate it once: its meshes and hulls are
    /// built into its cache and its kernels run once (their first calls are
    /// the slow ones). Off the UI thread: the document's VM is used under
    /// its lock. The environment is not prepared here: the renderer
    /// prepares it off the UI thread on the first draw.
    pub fn run(self) -> SceneLoaded {
        let SceneLoad { load, source_id, shown, request, files, input_t, aspect } = self;
        let edit_offset = request.edit_offset;
        let signals = request.signals.clone();
        let doc = request.load(&load.path, &Files(&files)).ok().map(|mut doc| {
            let mut cache = EvalCache::default();
            let _ = doc.eval(&makepad_stage_motion3d::eval::EvalInput { t: input_t, aspect, audio: &Audio(&*signals), edit_offset: edit_offset as f32 }, &mut cache);
            (doc, cache)
        });
        SceneLoaded { load, source_id, shown, doc }
    }
}

/// The scenes `frame` shows that are not loaded, not loading and not
/// drawing: their loads, for workers (see [`LOADED`]); their models' files
/// are those given with [`crate::MotionDraw::set_file`].
pub(crate) fn scene_loads(md: &MotionDraw, frame: &makepad_motion::MotionFrame) -> Vec<SceneLoad> {
    let mut out = Vec::new();
    for (_, item) in frame.walk() {
        let ItemKind::Scene3dDoc { scene, first_line, size, local_time, edit_offset, bpm, .. } = &item.kind else { continue };
        let Some(request) = SceneRequest::of(&item.kind) else { continue };
        let mut keys = HashMap::new();
        let (key, source_id, shown) = scene_identity(&mut keys, &item.path, scene, *bpm);
        let load = LoadKey { key, path: item.path.clone(), first_line: *first_line, edit_offset: offset_key(*edit_offset) };
        let drawing = md.scenes3d.scenes.get(&item.path).is_some_and(|s| {
            let s = s.borrow();
            s.load == load || s.load.path == load.path && s.load.first_line == load.first_line && s.load.edit_offset == load.edit_offset && s.source_id == source_id && s.layer.as_ref().is_some_and(|l| l.doc.takes_inputs(&s.inputs, &shown))
        });
        if loaded_has(&load, source_id, &shown) || drawing || FAILED.with(|f| f.borrow().contains(&key)) || !LOADING.with(|l| l.borrow_mut().insert(slot(&load))) {
            continue;
        }
        out.push(SceneLoad {
            load,
            source_id,
            shown,
            request,
            files: md.scenes3d.files.clone(),
            input_t: *local_time as f32,
            aspect: (size[0] / size[1].max(1e-9)) as f32,
        });
    }
    out
}

/// A scene a worker loaded, kept for its first draw. It leaves the host no
/// shared CPU work (None): the environment's prefilter was that work, and
/// the renderer now prepares the environment itself, off the UI thread
/// (makepad's env_map/prepare.rs); nothing reads render-material's prefilter
/// cache any more.
pub(crate) fn adopt_loaded(loaded: SceneLoaded) -> Option<Box<dyn FnOnce() + Send>> {
    let SceneLoaded { load, source_id, shown, doc } = loaded;
    LOADING.with(|l| l.borrow_mut().remove(&slot(&load)));
    let Some((doc, cache)) = doc else {
        FAILED.with(|f| f.borrow_mut().insert(load.key));
        return None;
    };
    loaded_put(load, source_id, shown, Scene3dLayer::from_doc(doc, cache));
    None
}

/// A dropped draw's scenes keep their documents (see [`LOADED`]); their
/// renderers go (the platform retires the dropped handles).
impl Drop for Scenes3d {
    fn drop(&mut self) {
        for (_, scene) in self.scenes.drain() {
            let scene = scene.into_inner();
            if let Some(layer) = scene.layer {
                loaded_put(scene.load, scene.source_id, scene.inputs, layer);
            }
        }
    }
}

struct Files<'a>(&'a HashMap<String, Arc<Vec<u8>>>);

impl MediaResolver for Files<'_> {
    fn load(&self, media: &MediaRef) -> Result<Arc<Vec<u8>>, String> {
        match media {
            MediaRef::Asset { id, rev } => self.0.get(&format!("{id}@{rev}")).or_else(|| self.0.get(id)).cloned()
                .ok_or_else(|| format!("asset {id} rev {rev} was not given to the renderer")),
            MediaRef::File(path) => std::fs::read(path).map(Arc::new).map_err(|e| format!("{path}: {e}")),
        }
    }
}

/// motion3d's audio reads over the document's signals.
struct Audio<'a>(&'a dyn SignalSource);

impl AudioSource for Audio<'_> {
    fn sample(&self, source: Option<&str>, feature: &AudioFeature, t: f32) -> f32 {
        let s = self.0;
        let t = t as f64;
        let music = || source.or(s.default_music()).unwrap_or("").to_string();
        let v = match feature {
            AudioFeature::Energy => s.energy(&music(), t),
            AudioFeature::Band(band) => s.band(&music(), band, t),
            AudioFeature::BeatPulse => (-s.beat_phase(&music(), t) * 7.0).exp(),
            AudioFeature::BeatPhase => s.beat_phase(&music(), t),
            AudioFeature::BarPhase => s.bar_phase(&music(), t),
            AudioFeature::Onset => s.hit(&music(), t, 0.25),
            AudioFeature::BeatIndex => s.beat_index(&music(), t).floor(),
            AudioFeature::Voice => {
                let voice = source.or(s.default_voice()).unwrap_or("");
                s.amplitude(voice, t)
            }
        };
        v as f32
    }
}

impl Scenes3d {
    pub fn begin_frame(&mut self, cx: &mut Cx) {
        let drawn: HashSet<String> = self.drawn.drain(..).collect();
        let mut gone = Vec::new();
        for (path, scene) in self.scenes.iter_mut() {
            let scene = scene.get_mut();
            if drawn.contains(path) {
                scene.idle = 0;
            } else {
                scene.idle += 1;
                if scene.idle >= SCENE_IDLE_FRAMES {
                    gone.push(path.clone());
                }
            }
        }
        for path in gone {
            self.remove(cx, &path);
        }
    }

    fn remove(&mut self, cx: &mut Cx, path: &str) {
        if let Some(scene) = self.scenes.remove(path) {
            let scene = scene.into_inner();
            // Its renderer's targets go now (a dropped handle's storage
            // otherwise waits for its slot to be reused); its document is
            // kept for its return.
            if let Some(mut layer) = scene.layer {
                layer.renderer.release_targets(cx);
                loaded_put(scene.load, scene.source_id, scene.inputs, layer);
            }
            let mut container = scene.container;
            container.release(cx);
            self.spare.push(container);
        }
        self.keys.remove(path);
    }

    /// Draw each scene's coverage id map from its next record on.
    pub fn set_coverage(&mut self, on: bool) {
        self.coverage = on;
    }

    /// The coverage maps of the scenes the last recorded frame drew.
    pub fn coverage_maps(&self) -> Vec<SceneCoverage> {
        self.drawn
            .iter()
            .filter_map(|path| {
                let scene = self.scenes.get(path)?.borrow();
                let layer = scene.layer.as_ref()?;
                let (map, mesh_nodes) = layer.renderer.coverage()?;
                let (place, size) = *self.placed.get(path)?;
                let mut nodes = Vec::new();
                surface_nodes(&layer.doc.def.root, &mut nodes);
                Some(SceneCoverage { scene: path.clone(), map, mesh_nodes, nodes, place, size })
            })
            .collect()
    }

    /// Every scene goes now (targets released, documents kept for the return).
    pub(crate) fn release_all(&mut self, cx: &mut Cx) {
        for path in self.scenes.keys().cloned().collect::<Vec<_>>() {
            self.remove(cx, &path);
        }
        for container in &mut self.spare {
            container.release(cx);
        }
    }

    pub fn errors(&self) -> Vec<String> {
        let mut out = self.errors.clone();
        for (path, scene) in &self.scenes {
            if let Some(layer) = &scene.borrow().layer {
                out.extend(layer.renderer.errors.iter().map(|e| format!("{path}: {e}")));
            }
        }
        out
    }

    pub fn debug_state(&self, cx: &Cx) -> String {
        let mut out = String::new();
        for path in &self.drawn {
            let Some(scene) = self.scenes.get(path) else { continue };
            if let Some(layer) = &scene.borrow().layer {
                if !layer.painted(cx) {
                    out.push_str(&format!("; Scene3D {path}: {} (sub-frames still to record: {})", layer.renderer.painting_report(cx), layer.renderer.in_progress()));
                }
            }
        }
        out
    }

    /// A scene the last frame drew waits for a model it places (building
    /// in Stage's model host).
    pub fn waiting_for_models(&self) -> bool {
        self.drawn.iter().any(|path| self.scenes.get(path).is_some_and(|scene| scene.borrow().layer.as_ref().is_some_and(|l| l.waiting_for_models())))
    }

    /// A model a drawn scene places landed since it loaded: the frame is
    /// not exact (the next record loads the scene again, with it).
    pub fn models_changed(&self) -> bool {
        self.drawn.iter().any(|path| self.scenes.get(path).is_some_and(|scene| scene.borrow().layer.as_ref().is_some_and(|l| l.models_changed())))
    }

    /// A scene the last frame drew has its environment still preparing (the
    /// renderer builds it off the UI thread: the frame is recorded without
    /// it, and the draw after it landed adopts it).
    pub fn environment_pending(&self) -> bool {
        self.drawn.iter().any(|path| self.scenes.get(path).is_some_and(|scene| scene.borrow().layer.as_ref().is_some_and(|l| l.environment_pending())))
    }

    /// Keeps the model builds of the scenes going between frames (the
    /// pool's completions raise the UI signal); a model landing redraws, and
    /// so does a scene's environment preparing: its job's completion is a
    /// signal, and only a draw adopts the result (a full queue retries on a
    /// draw too, and the jobs filling it raise signals as they finish).
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if !matches!(event, Event::Signal | Event::NextFrame(_)) {
            return;
        }
        let places_models = self.scenes.values().any(|scene| scene.borrow().layer.as_ref().is_some_and(|l| !l.doc.models.is_empty()));
        if places_models && makepad_stage_motion3d::models::pump(cx) {
            cx.redraw_all();
        }
        if self.environment_pending() {
            cx.redraw_all();
        }
    }

    pub fn painted(&self, cx: &Cx) -> bool {
        self.drawn.iter().all(|path| self.scenes.get(path).is_none_or(|scene| {
            let scene = scene.borrow();
            let container = &cx.passes[scene.container.pass.draw_pass_id()];
            scene.layer.as_ref().is_none_or(|layer| layer.painted(cx))
                && scene.resampled_at.is_none_or(|at| !container.paint_dirty && container.painted_serial > at)
        }))
    }

    /// Attach the recorded scenes again: motion3d's chains (which may
    /// record their next motion-blur sample) back under their containers,
    /// and the containers repainted when a sample was added.
    pub fn reattach(&self, cx: &mut Cx2d) {
        for path in &self.drawn {
            let Some(scene) = self.scenes.get(path) else { continue };
            let mut scene = scene.borrow_mut();
            let scene = &mut *scene;
            let Some(layer) = &mut scene.layer else { continue };
            let before = parents(cx);
            let serial = layer.renderer.record_serial();
            match &scene.signals {
                Some(signals) => layer.reattach(cx, &Audio(&**signals)),
                None => layer.reattach(cx, &makepad_stage_motion3d::anim::NoAudio),
            }
            let container = scene.container.pass.draw_pass_id();
            let mut touched = layer.renderer.record_serial() != serial;
            if touched {
                scene.resampled_at = Some(cx.repaint_id());
            }
            for id in cx.passes.id_iter().collect::<Vec<_>>() {
                if before.get(&id) != Some(&parent_key(&cx.passes[id].parent)) {
                    scene.owned.insert(id);
                }
            }
            scene.owned.extend(layer.renderer.recorded_passes());
            adopt(cx, &scene.owned, container);
            // Only a chain that really changed repaints the container: the
            // tail motion3d attached under the host pass is back under the
            // container now, which is where it was (repainting it anyway
            // painted every pass of the frame twice whenever the host asked
            // before its first paint).
            touched |= scene.owned.iter().any(|&id| before.get(&id) != Some(&parent_key(&cx.passes[id].parent)));
            cx.make_child_pass(&scene.container.pass);
            if touched {
                cx.repaint_pass(container);
            }
        }
    }
}

#[derive(PartialEq)]
enum Parent {
    Xr,
    Window(usize),
    Pass(DrawPassId),
    None,
}

fn parent_key(parent: &CxDrawPassParent) -> Parent {
    match parent {
        CxDrawPassParent::Xr => Parent::Xr,
        CxDrawPassParent::Window(id) => Parent::Window(id.id()),
        CxDrawPassParent::DrawPass(id) => Parent::Pass(*id),
        CxDrawPassParent::None => Parent::None,
    }
}

fn parents(cx: &Cx2d) -> HashMap<DrawPassId, Parent> {
    cx.passes.id_iter().map(|id| (id, parent_key(&cx.passes[id].parent))).collect()
}

/// The nodes of a scene that draw surfaces, depth first: what a coverage
/// report lists (a group, a light or points cover nothing of their own).
fn surface_nodes(node: &NodeDef, out: &mut Vec<CoverNode>) {
    if matches!(node.kind, NodeKind::Mesh { .. } | NodeKind::Model(_) | NodeKind::Text3D(_) | NodeKind::Instances(_)) {
        out.push(CoverNode { id: node.id, name: node.name.clone(), path: node.path.clone(), line: node.loc.line });
    }
    for child in &node.children {
        surface_nodes(child, out);
    }
}

/// Hang every motion3d pass whose parent is outside motion3d's own chain
/// under `container`.
fn adopt(cx: &mut Cx2d, owned: &HashSet<DrawPassId>, container: DrawPassId) {
    for &id in owned {
        let outside = match cx.passes[id].parent {
            CxDrawPassParent::DrawPass(parent) => parent != container && !owned.contains(&parent),
            _ => true,
        };
        if outside {
            cx.cx.attach_child_pass(id, container, None);
        }
    }
}

/// Record a motion3d scene item into its container; the container's
/// texture, or `None` (the reason is in the errors).
pub(super) fn render(md: &mut MotionDraw, cx: &mut Cx2d, item: &Item, scale: f64) -> Option<Texture> {
    let ItemKind::Scene3dDoc { scene, first_line, size, local_time, edit_offset, fps, bpm, signals, textures, videos, layers, .. } = &item.kind else { return None };
    let px = ((size[0] * scale).ceil().max(1.0) as u32, (size[1] * scale).ceil().max(1.0) as u32);
    // The document's tempo, so `beat(n)` in the scene is the document's beat
    // (a `bpm:` in the scene itself still wins).
    let (key, source_id, shown_inputs) = scene_identity(&mut md.scenes3d.keys, &item.path, scene, *bpm);
    let load = LoadKey { key, path: item.path.clone(), first_line: *first_line, edit_offset: offset_key(*edit_offset) };
    let request = SceneRequest::of(&item.kind)?;
    {
        let scenes = &mut md.scenes3d;
        // A model the scene places landed since it loaded: load it again.
        let models_changed = scenes.scenes.get(&item.path).is_some_and(|scene| scene.borrow().layer.as_ref().is_some_and(|l| l.models_changed()));
        // Other inputs: the loaded document takes them unless its load read one.
        if !models_changed {
            if let Some(scene) = scenes.scenes.get_mut(&item.path) {
                let scene = scene.get_mut();
                if scene.key != key && scene.source_id == source_id && scene.load.first_line == *first_line && scene.load.edit_offset == offset_key(*edit_offset) {
                    if scene.layer.as_mut().is_some_and(|layer| take_inputs(layer, &scene.inputs, &shown_inputs)) {
                        scene.key = key;
                        scene.load.key = key;
                        scene.inputs = shown_inputs.clone();
                    }
                }
            }
        }
        let fresh = models_changed || scenes.scenes.get(&item.path).is_none_or(|scene| scene.borrow().key != key);
        if fresh {
            // A document loaded ahead (or left by an earlier draw), else loaded
            // now (a template with the Splash modules it imports, through its
            // document's resolver).
            let kept = if models_changed { None } else { loaded_take(&load, source_id, &shown_inputs) };
            let loaded = match kept {
                Some(layer) => Ok(layer),
                None => load_scene(&item.path, &request, &scenes.files),
            };
            let layer = match loaded {
                Ok(layer) => Some(layer),
                Err(diags) => {
                    for diag in diags {
                        let message = if diag.message.trim().is_empty() { "the Scene3D did not load (its loader gave no reason)" } else { diag.message.as_str() };
                        let error = format!("{}: {}:{}: {message}", item.path, diag.loc.line, diag.loc.col);
                        if !scenes.errors.contains(&error) {
                            scenes.errors.push(error);
                        }
                    }
                    None
                }
            };
            let container = match scenes.scenes.remove(&item.path) {
                Some(old) => old.into_inner().container,
                None => scenes.spare.pop().unwrap_or_else(|| RenderTarget::new(cx, "motion_scene3d")),
            };
            scenes.scenes.insert(item.path.clone(), RefCell::new(Scene { key, load, source_id, inputs: shown_inputs, idle: 0, layer, container, owned: HashSet::new(), resampled_at: None, signals: None }));
        }
    }
    // 2D layers the scene shows as textures: each drawn at this moment into
    // its own canvas of the scene's pixels (recorded before the scene, so
    // painted before it samples them).
    let mut layer_pictures: Vec<(String, Texture)> = Vec::new();
    for (name, items) in layers {
        let minified = md.scenes3d.scenes.get(&item.path).is_some_and(|scene| scene.borrow().layer.as_ref().is_some_and(|l| l.doc.samples_minified(name)));
        let key = format!("{}/layers/{name}", item.path);
        let mut target = md.layer_targets.remove(&key).unwrap_or_else(|| RenderTarget::new(cx, "motion_scene_layer"));
        target.ensure(cx, px, RenderTargetFormat::Bgra8);
        let saved = (md.base, md.pixels, md.hdr);
        // A picture on a 3D surface: an sRGB texture motion3d decodes, in an
        // HDR frame too.
        md.hdr = false;
        // Top row first, like every picture motion3d shows (images, videos).
        md.base = Affine::scale(px.0 as f64 / size[0].max(1e-9), px.1 as f64 / size[1].max(1e-9));
        md.pixels = px;
        let texture = md.canvas_into(cx, items, vec4(0.0, 0.0, 0.0, 0.0), None, Some(&mut target));
        // A picture the scene minifies (`self.<name>.sample_grad(…)` or
        // `sample_lod`, e.g. a recursive zoom) gets a mip chain to read.
        texture.set_render_mips(cx, minified);
        (md.base, md.pixels, md.hdr) = saved;
        md.layer_targets.insert(key, target);
        layer_pictures.push((name.clone(), texture));
    }
    // Host pictures the materials name: images, and this frame of each video.
    // With each picture's size, so screens crop it to their own shape
    // instead of stretching it (video frames do not carry a size).
    let size_of = |s: &[f64; 2]| (s[0].round().max(1.0) as u32, s[1].round().max(1.0) as u32);
    let pictures: Vec<(String, Texture, (u32, u32))> = textures.iter().filter_map(|(name, media)| md.images.get(media).map(|(t, s)| (name.clone(), t.clone(), size_of(s))))
        .chain(videos.iter().filter_map(|(name, _)| md.videos.get(&video_key(&item.path, name)).map(|(t, s)| (name.clone(), t.clone(), size_of(s)))))
        .chain(layer_pictures.into_iter().map(|(name, t)| (name, t, px)))
        .collect();
    let scenes = &mut md.scenes3d;
    let scene = scenes.scenes.get_mut(&item.path).unwrap().get_mut();
    scene.resampled_at = None;
    scene.signals = Some(signals.0.clone());
    let layer = scene.layer.as_mut()?;
    scenes.drawn.push(item.path.clone());
    layer.renderer.set_coverage(scenes.coverage);
    // In an HDR frame the scene hands its linear light over (and the frame
    // samples the shutter); in locked time a display frame's scene samples
    // its own.
    let hdr = md.hdr;
    layer.set_host_sampled(hdr);
    layer.set_realtime(!md.locked);
    scene.container.ensure(cx, px, target_format(hdr));
    scene.container.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
    let before = parents(cx);
    for (name, texture, size) in pictures {
        layer.set_texture(&name, texture);
        layer.set_texture_frame(&name, size, None);
    }
    layer.set_exact_time(Some(*local_time));
    let time = FrameTime { t: *local_time as f32, fps: *fps as f32, edit_offset: *edit_offset as f32 };
    let texture = layer.draw(cx, &time, (px.0 as usize, px.1 as usize), &Audio(&*signals.0));
    for id in cx.passes.id_iter().collect::<Vec<_>>() {
        if before.get(&id) != Some(&parent_key(&cx.passes[id].parent)) {
            scene.owned.insert(id);
        }
    }
    // The chain's own passes, whatever their parent was before: a scene
    // loaded again draws into passes that may take the ids (and parents) of
    // the scene it replaced, and a pass whose parent happened to stay put
    // was not seen as the chain's (the pass below it was then hung under
    // the container and the chain never finished painting).
    scene.owned.extend(layer.renderer.recorded_passes());
    let container = scene.container.pass.draw_pass_id();
    adopt(cx, &scene.owned, container);
    // The scene's `layers:` pictures are sampled inside its chain: the pass
    // recorded before the scene (its last layer) hangs under the chain's
    // deepest pass, not the container, so it paints before every pass that
    // samples it. Under the container it painted after them, and each
    // frame's chain was painted twice (a pass that sampled a picture still
    // painting is painted again).
    let deepest = if layers.is_empty() { None } else { deepest_pass(cx, layer.renderer.recorded_passes(), container) };
    if let Some(texture) = &texture {
        let pixels = [px.0 as f64, px.1 as f64];
        md.draws.quad.quad(cx, texture, None, &Affine::IDENTITY, pixels, [0.0, 0.0, 1.0, 1.0], 1.0);
    }
    // A preview shows its models' build progress over the scene until they
    // land (an export waits for them instead).
    let waiting = !md.locked && md.scenes3d.scenes.get(&item.path).is_some_and(|scene| scene.borrow().layer.as_ref().is_some_and(|l| l.waiting_for_models()));
    if let Some(line) = waiting.then(makepad_stage_motion3d::models::progress_text).flatten() {
        let s = (px.1 as f64 / 360.0).max(0.5);
        let at = dvec2(12.0 * s, px.1 as f64 - 28.0 * s);
        if let Some(face) = crate::text::face(cx.cx, "Inter", 600.0, false) {
            md.draws.text.text_style = face;
        }
        md.draws.text.text_style.font_size = (13.0 * s) as f32;
        md.draws.text.color = vec4(0.0, 0.0, 0.0, 0.75);
        md.draws.text.draw_abs(cx, at + dvec2(s, s), &line);
        md.draws.text.color = vec4(1.0, 1.0, 1.0, 0.95);
        md.draws.text.draw_abs(cx, at, &line);
    }
    let scene = md.scenes3d.scenes.get_mut(&item.path).unwrap().get_mut();
    scene.container.end(cx);
    md.note_kind(&format!("scene3d {key:016x}"));
    let (id, out) = {
        let scene = md.scenes3d.scenes[&item.path].borrow();
        (scene.container.pass.draw_pass_id(), scene.container.texture.clone())
    };
    md.spans.push((deepest.unwrap_or(id), id));
    md.passes.push(id);
    Some(out)
}

/// The pass of `chain` farthest below `container` (painted first).
fn deepest_pass(cx: &Cx2d, chain: &[DrawPassId], container: DrawPassId) -> Option<DrawPassId> {
    let depth = |mut id: DrawPassId| {
        for depth in 1..64 {
            match cx.passes[id].parent {
                CxDrawPassParent::DrawPass(parent) if parent == container => return Some(depth),
                CxDrawPassParent::DrawPass(parent) => id = parent,
                _ => return None,
            }
        }
        None
    };
    chain.iter().filter_map(|&id| depth(id).map(|d| (d, id))).max_by_key(|&(d, _)| d).map(|(_, id)| id)
}

impl Scenes3d {
    /// Where node `node` of the scene at `path` projects this frame, in
    /// document units, and whether it is in front of the camera.
    pub(super) fn anchor(&self, path: &str, node: &str) -> Option<([f64; 2], bool)> {
        let scene = self.scenes.get(path)?.borrow();
        let a = scene.layer.as_ref()?.anchors().iter().find(|a| a.name == node)?.clone();
        let (m, size) = self.placed.get(path)?;
        Some((m.apply([a.screen[0] as f64 * size[0], a.screen[1] as f64 * size[1]]), a.in_front))
    }

    pub(super) fn place(&mut self, path: &str, m: Affine, size: [f64; 2]) {
        self.placed.insert(path.to_string(), (m, size));
    }
}

/// Where the host puts a Scene3D video's frame (`MotionDraw::set_video_frame`),
/// as `Need::VideoFrame` names it: the layer's path and the texture's name.
pub(crate) fn video_key(item_path: &str, name: &str) -> String {
    format!("{item_path}/videos/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_motion::{AudioFeatures, MotionContext, MotionDoc};
    use makepad_stage_motion3d::anim::NoAudio;
    use makepad_stage_motion3d::doc::FileResolver;
    use makepad_stage_motion3d::eval::{EvalCache, EvalInput};

    /// A lyric time the document and its scene share is one value in both:
    /// the document's `let T` is the second the word is sung on the
    /// document's clock, in the scene that starts at 12 s too (its own `t`
    /// is then 1.5 at the document's 13.5).
    #[test]
    fn a_shared_lyric_time_is_one_value_in_the_document_and_a_scene_starting_at_12() {
        let features = AudioFeatures::synthetic("one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen", 0.5, 1.0, 120.0, 30.0);
        let context = MotionContext::analyzed(features, None, 0.0);
        let src = "use mod.motion.*; use mod.scene\nlet L = lyric_line(voice(@vo), \"fourteen\", 0)\nlet T = lyric_word(voice(@vo), \"fourteen\", 0).start\n\
                   Motion{ size: vec2(64, 64) duration: 20 layers: [\n  Text{text: \"x\" opacity: fn(t) { if t >= T { 1.0 } else { 0.0 } }}\n  Scene3D{ span: [12.0, 6.0]\n    camera: Camera{}\n    lights: [scene.AmbientLight{}]\n    children: [scene.Mesh{geometry: scene.BoxGeometry{} position: fn(t, edit) { vec3(T, t, L[0].start) }}]\n  }\n]}\n";
        let doc = MotionDoc::load_with(src, &[], context).unwrap_or_else(|d| panic!("{d:?}"));
        // The document's T: second 13.5 ("fourteen" is word 13 at 1 word/s from 0.5).
        let t_doc = 13.5;
        let frame = doc.eval(t_doc);
        let Some(item) = frame.items.iter().find(|i| matches!(i.kind, ItemKind::Scene3dDoc { .. })) else { panic!("no scene at {t_doc}") };
        let ItemKind::Scene3dDoc { local_time, edit_offset, .. } = &item.kind else { unreachable!() };
        assert!((*edit_offset - 12.0).abs() < 1e-9 && (*local_time - 1.5).abs() < 1e-9, "{edit_offset} {local_time}");
        let resolver = FileResolver::default();
        let mut scene = makepad_motion::scene::load_scene(&item.kind, "t", &resolver).expect("a scene item").unwrap_or_else(|e| panic!("{:?}", e.iter().map(|d| d.to_string()).collect::<Vec<_>>()));
        let mut cache = EvalCache::default();
        let f = scene.eval(&EvalInput { t: *local_time as f32, aspect: 1.0, audio: &NoAudio, edit_offset: *edit_offset as f32 }, &mut cache);
        let m = &f.meshes[0].instances[0].transform.v;
        // T is the document's second in the scene; its own t is the layer's.
        assert!((m[12] - 13.5).abs() < 1e-4, "T in the scene {}", m[12]);
        assert!((m[13] - 1.5).abs() < 1e-4, "{}", m[13]);
        // The line's first word ("one", second 0.5 of the document).
        assert!((m[14] - 0.5).abs() < 1e-4, "{}", m[14]);
    }
}
