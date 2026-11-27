//! The model build service: script-authored models (`model_new`,
//! `character_new` in a level or a scene). The script records a model program
//! and posts it (`makepad-model-script`); this service compiles it on the warm
//! pool, far from the script's frame budget, and caches the GLB in the app's
//! asset store by program hash (`model_cache`). The host prepares each
//! product for install on the worker and installs it (Sandbox: a live
//! library alias through its prepared-publication path). Only channel polls
//! happen on the UI thread.
//!
//! The host (Sandbox, Stage) supplies a [`BuildHost`]: its store session,
//! the models it ships, a store fetcher for `m.import`, and its install
//! preparation. Each app builds into its own store.
use makepad_model::{Operation, ProgramOp};
use makepad_model_script::model_verbs::{self as verbs, ScriptModelRequest};
use makepad_platform::log;
use makepad_platform::thread::{Class, TaskPool};
use std::{any::Any, cell::RefCell, collections::VecDeque, path::PathBuf, sync::{Arc, OnceLock, atomic::{AtomicBool, AtomicUsize, Ordering}, mpsc::{sync_channel, Receiver, TryRecvError}}, time::Instant};

mod model_cache;
use model_cache as cache;
pub use cache::{glb_metrics, model_points, product_key, release_store_client, with_client, GlbMetrics, BASIS_FETCH, PRODUCT_FETCH, STORE_CACHE_PREFIX};

/// Where the app's asset store lives: enough for a blocking client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreSession {
    pub endpoints: makepad_asset_client::ApiEndpoints,
    pub token: Option<String>,
    pub server_id: [u8; 16],
}

/// What the service needs from the app hosting it.
pub trait BuildHost: Send + Sync + 'static {
    /// The store session and asset variant builds use now (UI thread; no
    /// session until the store attaches).
    fn store(&self) -> (Option<StoreSession>, Option<String>);
    /// A model the app ships itself, for `m.import` without a store.
    fn bundled(&self, alias: &str) -> Option<&'static [u8]>;
    /// A store fetcher for `m.import` (worker thread; made on first use).
    fn fetcher(&self, session: Option<StoreSession>, variant: Option<String>) -> Result<Box<dyn FnMut(&str) -> Result<Vec<u8>, String>>, String>;
    /// Worker: prepare a finished product for install (the app's own form).
    fn prepare(&self, request: &ScriptModelRequest, glb: &Arc<[u8]>) -> Result<Box<dyn Any + Send>, String>;
    /// Review contact sheets are wanted: builds bypass the cache and review.
    fn wants_review(&self) -> bool;
    /// Worker: prepare the review of a freshly built document.
    fn review(&self, document: &makepad_model::Document, alias: &str, cancelled: &dyn Fn() -> bool) -> Result<Box<dyn Any + Send>, String>;
    /// UI thread, as a build is submitted: work for the worker once the
    /// product is saved (it gets the GLB).
    fn after_build(&self, request: &ScriptModelRequest, session: Option<StoreSession>) -> Option<Box<dyn FnOnce(Arc<[u8]>) + Send>>;
    /// UI thread, as the service takes a request into its queue: the app may
    /// ask for what the model will need beside it, so that builds alongside
    /// it instead of on its first use.
    fn requested(&self, _request: &ScriptModelRequest) {}
    /// UI thread, as a build is submitted: the person waits for this product now (a load that
    /// holds the world until it is in). Its compile then takes the pool's Visible class instead of
    /// Background.
    fn waited_on(&self, _request: &ScriptModelRequest) -> bool { false }
    /// Bytes of built products kept in memory in front of the store, and the working memory each
    /// build may hold beside its document (its share).
    fn memory_budget(&self) -> usize;
    /// Bytes of working memory a build or follow-up may hold when its share refused it and it runs alone:
    /// the app's whole budget for generated models.
    fn alone_budget(&self) -> usize { self.memory_budget() }
    /// The cache's store client root (single owner).
    fn client_root(&self) -> PathBuf;
    /// The folder of the same-machine build locks.
    fn lock_dir(&self) -> PathBuf;
}

static HOST: OnceLock<Arc<dyn BuildHost>> = OnceLock::new();
static POOL: OnceLock<TaskPool> = OnceLock::new();
/// The app's host, once per process (before `initialize` or `build`).
pub fn set_host(host: Arc<dyn BuildHost>) { let _ = HOST.set(host); }
fn host() -> &'static dyn BuildHost { &**HOST.get().expect("makepad_model_build::set_host first") }

/// Work steps the service finished (a store lookup batch, a program run,
/// its LODs, its load-ready images, a prepare, an impostor bake): the
/// host's load watchdog reads it, so a world whose models are still being
/// built step by step (a headset compiles one at a time, each for tens of
/// seconds) is loading, not stalled.
static PROGRESS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn step_done() { PROGRESS.fetch_add(1, Ordering::Relaxed); }
/// The count of finished work steps (see `PROGRESS`); it only grows.
pub fn progress() -> u64 { PROGRESS.load(Ordering::Relaxed) }

/// One finished build, prepared for install by the host.
pub struct Product {
    pub glb: Arc<[u8]>,
    /// What `BuildHost::prepare` made.
    pub prepared: Box<dyn Any + Send>,
    /// Found in memory or the store, not compiled.
    pub cached: bool,
    pub warnings: Vec<String>,
    /// What `BuildHost::review` made (review builds only).
    pub review: Option<Box<dyn Any + Send>>,
    /// Where the time went (queue, lookup, program, LODs, KTX2, prepare).
    pub timing: String,
    /// The revision's last product: its follow-up, or a product with none planned. A first
    /// product (false) is older than its revision's settled one and never replaces it.
    pub settled: bool,
    publish: Option<PublishLater>,
    /// Derived distance representations, built after this product went out.
    follow_up: Option<BuildLater>,
}
/// One follow-up adds both automatic LODs and an optional impostor. The
/// full-detail model already draws correctly at every distance and has its
/// own store row. The final product keeps the original build lock/revision.
struct BuildLater {
    document: makepad_model::Document,
    compiled: makepad_model::CompileCache,
    /// The program's own compile (no automatic LODs): what the bake renders.
    /// A detail hit compiles its retained document here after installation.
    glb: Option<Vec<u8>>,
    /// A fresh build's installed GLB: saved with the document as the
    /// full-detail row before any distance work (a detail hit has its row).
    detail: Option<Arc<[u8]>>,
    plan: FollowUpPlan,
    lock: cache::BuildLock,
    session: Option<StoreSession>,
}
struct FollowUpPlan {
    lods: Vec<Operation>,
    impostor: Option<(f64, usize)>,
}
impl FollowUpPlan {
    fn new(document: &makepad_model::Document, impostor: Option<(f64, usize)>, view_model: bool) -> Option<Self> {
        let lods = if view_model { Vec::new() } else { auto_lod_ops(document) };
        (!lods.is_empty() || impostor.is_some()).then_some(Self { lods, impostor })
    }
}
/// Finished follow-ups, for `poll` to hand over like any product.
static FOLLOW_UPS: std::sync::Mutex<Vec<(ScriptModelRequest, Instant, Result<Product, String>)>> = std::sync::Mutex::new(Vec::new());
/// Follow-ups building on a worker, waiting to run alone, or finished and not yet handed over
/// by `poll`.
static FOLLOW_UPS_PENDING: AtomicUsize = AtomicUsize::new(0);
/// Follow-ups building (from when their full detail goes out until they finish or wait), and the
/// ones waiting to run alone: their share of working memory refused them (see `poll`).
static FOLLOW_UP_LOAD: std::sync::Mutex<FollowUpLoad> = std::sync::Mutex::new(FollowUpLoad { building: 0, waiting: VecDeque::new() });
struct FollowUpLoad { building: usize, waiting: VecDeque<(FollowUp, Derive)> }
fn follow_up_load() -> std::sync::MutexGuard<'static, FollowUpLoad> { FOLLOW_UP_LOAD.lock().unwrap_or_else(|e| e.into_inner()) }
/// The aliases of those follow-ups, one entry per follow-up.
static FOLLOW_UP_ALIASES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Planned follow-ups (a product's distance representations: automatic LODs
/// and its impostor) still building or not yet handed over by [`poll`]. Each
/// replaces its full-detail product shortly after that installs. The app
/// installs the full detail first and never waits for these; a host that
/// wants the settled model (a review render) waits for 0.
pub fn follow_ups_pending() -> usize { FOLLOW_UPS_PENDING.load(Ordering::Acquire) }
/// The aliases whose follow-ups [`follow_ups_pending`] counts: models installed at full detail
/// that will be replaced by a product with their distance representations.
pub fn visit_follow_ups_pending(visit: &mut dyn FnMut(&str)) {
    for alias in FOLLOW_UP_ALIASES.lock().unwrap_or_else(|e| e.into_inner()).iter() { visit(alias); }
}
/// A product of the revision the latest eval asked for, ready to install.
pub struct Finished { pub request: ScriptModelRequest, pub started: Instant, pub product: Product }
/// A fresh product goes to the store after the app has it (the lock is held until then).
/// `replaces_detail`: the product of a model with a full-detail row, retired once this saves.
struct PublishLater { lock: cache::BuildLock, session: Option<StoreSession>, glb: Arc<[u8]>, replaces_detail: bool }
/// Keep the original store inputs and publication callback across a memory wait.
struct PrimaryJob {
    request: ScriptModelRequest,
    session: Option<StoreSession>,
    variant: Option<String>,
    after: Option<Box<dyn FnOnce(Arc<[u8]>) + Send>>,
    started: Instant,
}
impl PrimaryJob {
    fn new(request: ScriptModelRequest) -> Self {
        let (session, variant) = host().store();
        let after = host().after_build(&request, session.clone());
        Self { request, session, variant, after, started: Instant::now() }
    }
}
struct Running { request: ScriptModelRequest, cancel: Arc<AtomicBool>, receiver: Receiver<Result<Primary<Product, (PrimaryJob, String)>, String>>, started: Instant, cached: bool }
#[derive(Default)]
struct Builds {
    /// When the service started (compiles wait briefly for the store).
    started: StartInstant,
    pool: Option<TaskPool>,
    waiting: VecDeque<ScriptModelRequest>,
    waiting_alone: VecDeque<(PrimaryJob, String)>,
    running: Vec<Running>,
    /// A primary or follow-up runs alone: new compiles wait until it is done.
    alone: bool,
}
#[derive(Clone, Copy)]
struct StartInstant(Instant);
impl Default for StartInstant { fn default() -> Self { StartInstant(Instant::now()) } }
impl StartInstant { fn elapsed(&self) -> std::time::Duration { self.0.elapsed() } }
thread_local! { static BUILDS: RefCell<Builds> = RefCell::new(Builds::default()); }

/// Start the service on the app's warm pool (after `set_host`); prepared
/// textures are cached in the same store from now on.
pub fn initialize(pool: TaskPool) {
    let _ = POOL.set(pool.clone());
    // The model generators (characters, imports, primitives) give way to the pool through this.
    // A stale build is reported by the cancel callback's next poll: a stale result is never consumed.
    makepad_model::mesh::install_pace(pace);
    makepad_render::material_surface::set_prepared_texture_store(Box::new(cache::TextureStore));
    BUILDS.with(|b| b.borrow_mut().pool = Some(pool));
}

fn pace() { let _ = makepad_platform::thread::checkpoint_when_due(); }

thread_local! { static FOCUS: std::cell::Cell<Option<[f32; 3]>> = const { std::cell::Cell::new(None) }; }
/// Where the viewer starts (the local player's spawn, or the debug camera):
/// placed models nearest it build and install first. The host sets it after
/// a level evaluates; without it, placement order is used.
pub fn set_build_focus(focus: Option<[f32; 3]>) { FOCUS.with(|f| f.set(focus)); }

/// Connect the built-model cache's client once a store attaches (a worker
/// thread; the first level's lookups then find it connected).
pub fn prewarm_store(session: StoreSession) {
    let Some(pool) = POOL.get() else {
        log!("model store prewarm: task pool is not initialized");
        return;
    };
    match pool.submit(Class::Background, move || cache::prewarm(&session)) {
        Ok(task) => task.detach(),
        Err(error) => log!("model store prewarm: {error}"),
    }
}

/// The built product (the load-ready GLB) of a script model's program `hash`, as the store keeps
/// it: what a review tool reads to inspect a model a level built. `None` until it is saved.
pub fn stored_product(session: &StoreSession, hash: &str) -> Result<Option<Vec<u8>>, String> { cache::store_get(session, hash) }

/// Reference for automatic LODs, the renderer's LOD screen reference
/// (`makepad_render::lod_view`): a 2160 px (4K) output at 62 degrees
/// vertical FOV, the race's chase camera. In model units (metres),
/// p(d) = 2*d*tan(FOV/2)/H; drop sub-pixel parts and bound certified two-sided
/// surface deviation to half a reference pixel to avoid a visible pop at 4K. Each
/// view scales its switch distances to its own FOV and height, so the rule
/// holds for narrower cameras and other targets too.
const REFERENCE_HEIGHT_PX: f64 = makepad_render::lod_view::LOD_REFERENCE_HEIGHT_PX as f64;
/// Vertical field of view used with [`REFERENCE_HEIGHT_PX`].
const REFERENCE_FOV_DEG: f64 = makepad_render::lod_view::LOD_REFERENCE_FOV_DEG as f64;

/// Every static part shares model-wide distances; authored LODs override only
/// their owner, except a lone empty level keeps nearer automatic levels.
/// Automatic levels certify two-sided surface deviation to half a reference pixel.
/// Delivery keeps a part's previous geometry when it cannot reduce it.
/// `SANDBOX_AUTO_LOD=0` disables automatic levels.
fn auto_lod_ops(doc: &makepad_model::Document) -> Vec<Operation> {
    use makepad_model::{LodLevel, Operation, SceneOperation, transform::transform_point};
    if std::env::var_os("SANDBOX_AUTO_LOD").is_some_and(|v| v == "0") { return Vec::new(); }
    if doc.skeleton().is_some() { return Vec::new(); }
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    let mut parts = Vec::new();
    for (name, geometry) in doc.geometries() {
        let world = match doc.scene().world_matrix(name) {
            Ok(world) => world,
            Err(error) => { log!("automatic LOD part '{}': {}", name, error); continue; }
        };
        let (mut part_lo, mut part_hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        let mut include = |p| {
            let position = transform_point(world, p);
            for d in 0..3 { part_lo[d] = part_lo[d].min(position[d]); part_hi[d] = part_hi[d].max(position[d]); }
        };
        match geometry {
            makepad_model::ObjectGeometry::Editable(m) => m.vertices().iter().for_each(|v| include(v.position)),
            makepad_model::ObjectGeometry::Baked(m) => m.positions.iter().for_each(|p| include(p.map(f64::from))),
        }
        if part_lo.iter().chain(&part_hi).any(|v| !v.is_finite()) { continue; }
        for d in 0..3 { lo[d] = lo[d].min(part_lo[d]); hi[d] = hi[d].max(part_hi[d]); }
        if matches!(geometry, makepad_model::ObjectGeometry::Baked(_)) || name.starts_with("__") { continue; }
        let hide = match doc.scene().lods.get(name).map(Vec::as_slice) {
            Some([level]) if level.target_faces == 0 => Some(level),
            Some(_) => continue,
            None => None,
        };
        let size = (0..3).map(|d| (part_hi[d] - part_lo[d]).powi(2)).sum::<f64>().sqrt();
        parts.push((name, size, hide));
    }
    let radius = (0..3).map(|d| (hi[d] - lo[d]).powi(2)).sum::<f64>().sqrt() * 0.5;
    if !radius.is_finite() || radius <= 0.0 { return Vec::new(); }
    let pixel_per_metre = 2.0 * (REFERENCE_FOV_DEG.to_radians() * 0.5).tan() / REFERENCE_HEIGHT_PX;
    parts.into_iter().map(|(name, size, hide)| {
        let mut levels = Vec::new();
        for distance in [(radius * 12.0).max(15.0), (radius * 35.0).max(40.0), (radius * 100.0).max(115.0)] {
            if hide.is_some_and(|level| distance >= level.distance) { break; }
            let pixel = pixel_per_metre * distance;
            // Four triangle faces is the minimum closed solid; otherwise the
            // half-pixel surface bound, not a percentage target, stops work.
            let target_faces = if size < pixel { 0 } else { 4 };
            // Only the final level may be empty; preserve an authored hide
            // distance instead of adding an earlier automatic empty level.
            if target_faces == 0 && hide.is_some() { break; }
            levels.push(LodLevel { target_faces, max_error: 0.5 * pixel, distance });
            if target_faces == 0 { break; }
        }
        if let Some(level) = hide { levels.push(level.clone()); }
        Operation::Scene(SceneOperation::Lods { object: name.into(), levels })
    }).collect()
}

/// A program's `m.impostor` request (its distance and view size).
fn impostor_request(program: &[ProgramOp]) -> Option<(f64, usize)> {
    program.iter().rev().find_map(|op| match op { ProgramOp::Impostor { distance, cell } => Some((*distance, (*cell).clamp(32, 256) as usize)), _ => None })
}

/// The first material index the built document leaves free.
fn next_material(doc: &makepad_model::Document) -> u32 {
    let faces = doc.objects().flat_map(|(_, m)| m.faces().iter().map(|f| f.material)).max();
    let declared = doc.materials().keys().chain(doc.surface().materials.keys()).copied().max();
    faces.max(declared).map_or(1, |m| m + 1)
}

/// The baked stand-in as model ops: one cut-out, double-sided material
/// (`makepadShading.impostor` = the distance) with the albedo atlas as its
/// base colour, the normal atlas as its normal layer and the depth atlas as
/// its occlusion layer (a slot reuse: impostor.rs `ImpostorBake::depth`, the
/// occlusion in its blue), a crown material when the tree's leaves are crowns, on one
/// quad that carries the bounding sphere: positions = centre + corner x
/// radius in the XY plane, uvs = corner x radius (libs/render impostor.rs;
/// the shader turns the quad toward the camera and picks the frames).
fn impostor_ops(b: &makepad_render::impostor::ImpostorBake, distance: f64, material: u32, limits: &makepad_model::Limits) -> Result<Vec<Operation>, String> {
    use makepad_model::{BakedMesh, Material, RgbaImage, SurfaceAlpha, SurfaceChannel, SurfaceLayer, SurfaceMaterial, SurfaceOperation};
    let image = |px: &[u8]| { let side = ((px.len() / 4) as f64).sqrt() as u32; RgbaImage { width: side, height: side, pixels: px.to_vec() } };
    let [cx, cy, cz] = b.center;
    let r = b.radius;
    let crown = if b.crown { 1.0 } else { 0.0 };
    let shading = makepad_gltf::GlbShading { wind: 0.0, clearcoat: 0.0, flake: 0.0, impostor: distance, rim: 0.0, crown, plate: 0.0, lamp: None, ..Default::default() };
    let surface = SurfaceMaterial { base_color: [1.0; 4], metallic: 0.0, roughness: 0.9, alpha: SurfaceAlpha::Mask, alpha_cutoff: 0.5, double_sided: true, shading: Some(shading), ..SurfaceMaterial::default() };
    let layer = |channel: SurfaceChannel, name: &str, px: &[u8]| Operation::Surface(SurfaceOperation::Layer { material, channel, layer: SurfaceLayer::new(name, image(px)) });
    let mips = |channel: SurfaceChannel, name: &str| Operation::Surface(SurfaceOperation::Mips { material, channel, layer: name.into() });
    let quad = Operation::Baked { object: "impostor-quad".into(), mesh: Arc::new(BakedMesh {
        positions: vec![[cx-r, cy+r, cz], [cx-r, cy-r, cz], [cx+r, cy-r, cz], [cx+r, cy+r, cz]],
        normals: vec![[0., 0., 1.]; 4], uvs: Some(vec![[-r,r], [-r,-r], [r,-r], [r,r]]),
        material_bytes: None, indices: vec![0, 1, 2, 2, 3, 0], runs: vec![(material, 0..6)],
    }) };
    quad.check(limits).map_err(|e| e.to_string())?;
    Ok(vec![
        Operation::SetMaterial { material, value: Material { color: [1.0; 3], base_color_png: Vec::new() } },
        Operation::Surface(SurfaceOperation::Material { material, value: surface }),
        layer(SurfaceChannel::BaseColor, "impostor", &b.albedo),
        mips(SurfaceChannel::BaseColor, "impostor"),
        layer(SurfaceChannel::Normal, "impostor-normal", &b.normal),
        mips(SurfaceChannel::Normal, "impostor-normal"),
        // No authored mips: the loader's Data16 mips average the 16-bit
        // depth whole and the occlusion byte (material_surface.rs).
        layer(SurfaceChannel::Occlusion, "impostor-depth", &b.depth),
        quad,
    ])
}

enum CachedBuild { Complete(Arc<[u8]>), Detail(cache::StoredDetail) }

enum Primary<T, W = String> { Built(T), WaitAlone(W) }

#[derive(Clone, Copy)]
enum BuildLimit { Share(usize), Alone(usize), Uncapped }
impl BuildLimit {
    fn limits(self) -> makepad_model::Limits {
        let max_bytes = match self { Self::Share(bytes) | Self::Alone(bytes) => bytes, Self::Uncapped => usize::MAX };
        makepad_model::Limits { mesh: makepad_model::mesh::Limits { max_bytes }, ..Default::default() }
    }
}

fn compile_primary(alias: &str, ops: &[ProgramOp], mut limit: BuildLimit, fetch: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>, cancelled: &dyn Fn() -> bool)
    -> Result<Primary<(makepad_model::BuiltProgram, makepad_model::CompileCache)>, String> {
    loop {
        let mut compiled = makepad_model::CompileCache::default();
        match makepad_model::build_program_with_cache(ops, limit.limits(), fetch, Some(cancelled), &mut compiled) {
            Ok(built) => return Ok(Primary::Built((built, compiled))),
            Err(error) if error.memory_refusal => match limit {
                BuildLimit::Share(bytes) => {
                    log!("script model '{alias}' waits to run alone, over a build's share of {:.1} MiB ({error})", mib(bytes));
                    return Ok(Primary::WaitAlone(error.to_string()));
                }
                BuildLimit::Alone(bytes) => {
                    log!("script model '{alias}' runs uncapped, over the whole budget of {:.1} MiB ({error})", mib(bytes));
                    limit = BuildLimit::Uncapped;
                }
                BuildLimit::Uncapped => return Err(error.to_string()),
            },
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Worker body: a cached product (memory, then the asset store) or a fresh
/// compile, then the host's preparation. Public for headless tools (one
/// build off the queue, after `set_host` and `initialize`).
pub fn build(request: &ScriptModelRequest, session: Option<StoreSession>, variant: Option<String>, cancel: &AtomicBool) -> Result<Product, String> {
    POOL.get().ok_or("model build pool is not initialized")?;
    match build_with_limit(request, session, variant, cancel, BuildLimit::Alone(host().alone_budget()), None)? {
        Primary::Built(product) => Ok(product),
        Primary::WaitAlone(_) => unreachable!("only a shared compile waits to run alone"),
    }
}

fn build_with_limit(request: &ScriptModelRequest, session: Option<StoreSession>, variant: Option<String>, cancel: &AtomicBool,
    limit: BuildLimit, cached_glb: Option<Arc<[u8]>>) -> Result<Primary<Product>, String> {
    let build_started = Instant::now();
    let mut detail = makepad_model::ProgramBuildDetail::default();
    let image_timing_scope = cache::LoadReadyTimingScope::new();
    let host = host();
    let review_wanted = cached_glb.is_none() && host.wants_review();
    let cancelled = || cancel.load(Ordering::Acquire) || makepad_platform::thread::checkpoint_when_due().is_err();
    let mut warnings = Vec::new();
    let mut review = None;
    let mut timing = String::new();
    // Memory, then the complete row, then the full-detail row; a hit stops
    // there. What the probe batch just answered is not asked again: on a busy
    // store those resolves were 0.3-1 s of every cold build. Review bypasses all.
    let lookup = |probe: Option<cache::Probe>| -> Option<CachedBuild> {
        if review_wanted { return None; }
        if let Some(glb) = cache::memory_get(&request.hash).or_else(|| cached_glb.clone()) { return Some(CachedBuild::Complete(glb)); }
        if probe == Some(cache::Probe::Miss) { return None; }
        let session = session.as_ref()?;
        if probe != Some(cache::Probe::Detail) {
            match cache::store_get(session, &request.hash) {
                Ok(Some(glb)) => return Some(CachedBuild::Complete(glb.into())),
                Ok(None) => {}
                Err(error) => log!("script model '{}': {error}", request.alias),
            }
        }
        cache::store_get_detail(session, &request.hash).unwrap_or_else(|error| { log!("script model '{}': {error}", request.alias); None }).map(CachedBuild::Detail)
    };
    let mut lock = None;
    // A peer could have published while a refused compile waited with its lock released.
    let probe = if matches!(limit, BuildLimit::Share(_)) { cache::probe_state(&request.hash) } else { None };
    let mut found = lookup(probe);
    while !matches!(found, Some(CachedBuild::Complete(_))) && lock.is_none() {
        match cache::BuildLock::acquire(&request.hash) {
            Ok(held) => lock = Some(held),
            Err(()) => {
                if !cache::wait_for_peer(&request.hash, &cancelled) { return Err("superseded by a newer revision".into()); }
                // The peer may have published the complete row meanwhile.
                found = lookup(None);
            }
        }
    }
    let mut restored = None;
    if let Some(CachedBuild::Detail(stored)) = &found {
        let started = Instant::now();
        let decoded = cache::glb_metrics(&stored.glb).and_then(|_| makepad_model::Document::from_bytes(&stored.source, limit.limits(), Some(&cancelled)).map_err(|e| e.to_string()));
        if cancelled() { return Err("superseded by a newer revision".into()); }
        match decoded {
            Ok(document) => {
                restored = Some(document);
                timing = format!("full detail from the asset store, restore {} ms, ", started.elapsed().as_millis());
                step_done();
            }
            Err(error) => {
                log!("script model '{}': full-detail row '{}': {error}; building the program", request.alias, cache::detail_alias(&request.hash));
                found = None;
            }
        }
    }
    let mut publish = None;
    let mut follow_up = None;
    let (glb, cached) = match found {
        Some(CachedBuild::Complete(bytes)) => (bytes, true),
        Some(CachedBuild::Detail(stored)) => {
            let document = restored.expect("a detail hit has a decoded document");
            let lock = lock.take().expect("a detail hit holds its build lock");
            let view_model = request.alias.starts_with("gen/mdl/arms-");
            let plan = FollowUpPlan::new(&document, impostor_request(&request.program), view_model);
            log!("script model '{}': full detail from the asset store; distance representations follow", request.alias);
            match plan {
                Some(plan) => follow_up = Some(BuildLater { document, compiled: makepad_model::CompileCache::default(), glb: None, detail: None, plan, lock, session: session.clone() }),
                None => publish = Some((lock, true)),
            }
            (Arc::from(stored.glb), true)
        }
        None => {
            let lock = lock.take().expect("a build holds its lock");
            let mut fetcher = None;
            let mut fetch = |alias: &str| -> Result<Vec<u8>, String> {
                if let Some(bytes) = host.bundled(alias) { return Ok(bytes.to_vec()); }
                if fetcher.is_none() { fetcher = Some(host.fetcher(session.clone(), variant.clone())?); }
                fetcher.as_mut().unwrap()(alias)
            };
            // `m.impostor`: not a model op (the program skips it); baked after this
            // product is out (`BuildLater`).
            let impostor = impostor_request(&request.program);
            let program_started = Instant::now();
            let (built, compiled) = match compile_primary(&request.alias, &request.program, limit, &mut fetch, &cancelled)? {
                Primary::Built(work) => work,
                // The lock drops here on the worker, before any row is published or the request queues again.
                Primary::WaitAlone(why) => return Ok(Primary::WaitAlone(why)),
            };
            let t_program = program_started.elapsed();
            step_done();
            makepad_platform::thread::checkpoint().map_err(|_| "superseded by a newer revision")?;
            warnings = built.warnings;
            if review_wanted {
                match host.review(&built.document, &request.alias, &cancelled) {
                    Ok(prepared) => review = Some(prepared),
                    Err(error) => warnings.push(format!("review not rendered: {error}")),
                }
            }
            // First-person arms only ever draw at arm's length: no LODs.
            let view_model = request.alias.starts_with("gen/mdl/arms-");
            let plan = FollowUpPlan::new(&built.document, impostor, view_model);
            let base = built.compiled.glb;
            // Images in the load-ready form (UASTC mips), encoded once here.
            let t_images = Instant::now();
            let glb = cache::load_ready(base.clone())?;
            step_done();
            timing = format!("program {} ms (geometry {} ms, weathering {} ms, textures {} ms, compile {} ms), lods 0 ms, ktx2 {} ms, ", t_program.as_millis(), built.timings.geometry.as_millis(), built.timings.weathering.as_millis(), built.timings.textures.as_millis(), built.timings.compile.as_millis(), t_images.elapsed().as_millis());
            makepad_platform::thread::checkpoint().map_err(|_| "superseded by a newer revision")?;
            detail = built.detail;
            let glb: Arc<[u8]> = Arc::from(glb);
            // The complete row is published after install, under this lock;
            // the follow-up first saves full detail under its own row.
            match plan {
                Some(plan) => follow_up = Some(BuildLater { document: built.document, compiled, glb: Some(base), detail: Some(glb.clone()), plan, lock, session: session.clone() }),
                None => publish = Some((lock, false)),
            }
            (glb, false)
        }
    };
    if cancelled() { return Err("superseded by a newer revision".into()); }
    cache::memory_put(&request.hash, glb.clone());
    let publish = publish.map(|(lock, replaces_detail)| PublishLater { lock, session, glb: glb.clone(), replaces_detail });
    if let Some(lookup) = cache::lookup_time(&request.hash) { timing = format!("lookup {} ms, {timing}", lookup.as_millis()); }
    makepad_platform::thread::checkpoint().map_err(|_| "superseded by a newer revision")?;
    let t_prepare = Instant::now();
    let prepared = host.prepare(request, &glb)?;
    step_done();
    timing.push_str(&format!("prepare {} ms", t_prepare.elapsed().as_millis()));
    let image_timings = image_timing_scope.finish();
    if build_started.elapsed() > std::time::Duration::from_millis(300) {
        log!("model build detail '{}': {}", request.alias, build_detail(&detail, &request.texture_timings, &image_timings, cached));
    }
    Ok(Primary::Built(Product { glb, prepared, cached, warnings, review, timing, settled: follow_up.is_none(), publish, follow_up }))
}

/// Formatting and its allocations happen only once a successful build is slow.
fn build_detail(detail: &makepad_model::ProgramBuildDetail, script_textures: &[makepad_model::TextureTiming], images: &[makepad_render::texture_pack::LoadReadyImageTiming], cached: bool) -> String {
    let ms = |time: std::time::Duration| time.as_secs_f64() * 1000.;
    let operations = detail.operations.iter().map(|(kind, timing)| format!("{kind} x{}: {:.3} ms", timing.count, ms(timing.elapsed))).collect::<Vec<_>>().join(", ");
    let mut textures = detail.textures.clone();
    for script in script_textures {
        if let Some(texture) = textures.iter_mut().find(|t| t.material == script.material && t.channel == script.channel) {
            texture.elapsed += script.elapsed;
        } else { textures.push(script.clone()); }
    }
    let textures = textures.iter().map(|texture| {
        let script = script_textures.iter().filter(|t| t.material == texture.material && t.channel == texture.channel).map(|t| t.elapsed).sum::<std::time::Duration>();
        let script = if script.is_zero() { String::new() } else { format!(" (script {:.3} ms)", ms(script)) };
        let owner = texture.material.map_or_else(|| "shared".to_string(), |material| format!("material {material}"));
        format!("{owner} {} {} {}x{}: {:.3} ms{script}", texture.channel.name(), texture.kind, texture.width, texture.height, ms(texture.elapsed))
    }).collect::<Vec<_>>().join(", ");
    let images = images.iter().map(|image| format!("image {} {}x{}, {} mip levels: {:.3} ms ({})", image.image, image.width, image.height, image.mip_levels, ms(image.elapsed), image.format)).collect::<Vec<_>>().join(", ");
    let w = &detail.weathering;
    let c = &detail.compile;
    let g = &detail.geometry;
    format!("cached {cached}; geometry [{operations}; flush/apply {:.3} ms, validate {:.3} ms, history {:.3} ms, clone {:.3} ms, other {:.3} ms; {} vertices, {} triangles]; weathering [{} vertices, {} rays; setup {:.3} ms, bvh {:.3} ms, queries+wear {:.3} ms]; textures [{textures}]; compile [evaluation {:.3} ms, mesh packing {:.3} ms, tangents {:.3} ms, PNG {:.3} ms, GLB assembly {:.3} ms, texture work {:.3} ms, other {:.3} ms]; load-ready (ktx2) [{images}]",
        ms(g.flush_apply), ms(g.validate), ms(g.history), ms(g.clone), ms(detail.geometry_other), detail.vertices, detail.triangles, w.vertices, w.rays, ms(w.setup), ms(w.acceleration), ms(w.queries),
        ms(c.evaluation), ms(c.mesh_packing), ms(c.tangents), ms(c.png), ms(c.glb_assembly), ms(c.texture_work), ms(c.other))
}

/// A follow-up's last compile: its document at its last use (no undo history or checkpoint
/// beside the state: the full detail has its own row) and the automatic levels it still applies.
struct LastCompile { last: makepad_model::LastUse, lods: Vec<Operation>, limit: usize }
impl LastCompile {
    fn new(document: makepad_model::Document, lods: Vec<Operation>) -> Self {
        let limit = document.limits().mesh.max_bytes;
        Self { last: document.into_last_use(), lods, limit }
    }
    fn set_limit(&mut self, bytes: usize) { self.limit = bytes; self.last.set_memory_limit(bytes); }
    /// Compile only new export inputs. Delivery reports failures per part while preserving all
    /// other levels; never discard the complete automatic plan. Every part's levels go in one
    /// transaction (it has no operation count bound), applied once: a refusal leaves the work as
    /// it was, to compile again under a larger limit.
    fn compile(&mut self, compiled: &mut makepad_model::CompileCache, cancelled: &dyn Fn() -> bool) -> Result<makepad_model::CompiledModel, makepad_model::Error> {
        if !self.lods.is_empty() {
            self.last.apply("auto-lod", self.lods.clone(), Some(cancelled))?;
            self.lods.clear();
        }
        self.last.compile_with_cache(compiled, Some(cancelled))
    }
    /// A refusal a larger limit lets through, said for the log (what the refused step needed);
    /// None for any other error. The model's other byte budgets (`Budget`, by name) count too:
    /// the build checked its counts and dimensions, and the levels add none. An operation's own
    /// storage is refused beside what the document holds (the rest of the limit).
    fn refusal(&self, error: &makepad_model::Error) -> Option<String> {
        use makepad_model::{Error, mesh::MeshError};
        if !error.is_memory_refusal() { return None; }
        Some(match error {
            Error::Mesh(MeshError::Budget { requested, limit, .. }) => format!("mesh work: needs {:.1} MiB, limit {:.1} MiB",
                mib(self.limit.saturating_sub(*limit as usize).saturating_add(*requested as usize)), mib(self.limit)),
            Error::Budget(what) => format!("{what}: over the limit of {:.1} MiB", mib(self.limit)),
            _ => error.to_string(),
        })
    }
}

/// A follow-up's last compile under its limit. Under its share (`alone` None) a refusal for
/// working memory hands the work back with what it needed (`Ok(Err(why))`), to wait and run
/// alone. Alone it holds the whole budget (`alone`), and when even that is short, any (logged).
fn compile_last(alias: &str, work: &mut LastCompile, compiled: &mut makepad_model::CompileCache, alone: Option<usize>, cancelled: &dyn Fn() -> bool) -> Result<Result<makepad_model::CompiledModel, String>, String> {
    if let Some(whole) = alone { work.set_limit(whole); }
    match work.compile(compiled, cancelled) {
        Ok(product) => Ok(Ok(product)),
        Err(error) => match (work.refusal(&error), alone) {
            (Some(why), None) => Ok(Err(why)),
            (Some(why), Some(whole)) => {
                // A slower build that finishes beats a refused one.
                log!("script model '{alias}' follow-up runs uncapped, over the whole budget of {:.1} MiB ({why})", mib(whole));
                work.set_limit(usize::MAX);
                work.compile(compiled, cancelled).map(Ok).map_err(|e| e.to_string())
            }
            (None, _) => Err(error.to_string()),
        },
    }
}
fn mib(bytes: usize) -> f64 { bytes as f64 / 1048576.0 }

/// A follow-up past its full-detail save and its impostor: the last compile, and what goes with
/// its product.
struct Derive {
    work: LastCompile,
    compiled: makepad_model::CompileCache,
    warnings: Vec<String>,
    bake: std::time::Duration,
    lock: cache::BuildLock,
    session: Option<StoreSession>,
}
/// A follow-up on a worker: its request, and what it hands over after its product (`hand_over`).
struct FollowUp {
    job: ScriptModelRequest,
    flag: Arc<AtomicBool>,
    publish: Option<PublishLater>,
    named: Option<Arc<[u8]>>,
    after: Option<Box<dyn FnOnce(Arc<[u8]>) + Send>>,
}
enum Stage { Begin(BuildLater), Derive(Derive) }

/// Runs a follow-up to its product, or, when its share refused it, to the queue of follow-ups
/// waiting to run alone (`poll` starts them).
fn run_follow_up(mut f: FollowUp, stage: Stage, alone: Option<usize>) {
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let derive = match stage { Stage::Begin(later) => begin_follow_up(&f.job, later, &f.flag)?, Stage::Derive(derive) => derive };
        finish_follow_up(&f.job, derive, &f.flag, alone)
    })).unwrap_or_else(|_| Err("model follow-up panicked".into()));
    let result = match result {
        Ok(Err((derive, why))) => {
            log!("script model '{}' follow-up waits to run alone, over a build's share of {:.1} MiB ({why})", f.job.alias, mib(derive.work.limit));
            let mut load = follow_up_load();
            load.building -= 1;
            load.waiting.push_back((f, derive));
            drop(load);
            makepad_platform::thread::SignalToUI::set_ui_signal();
            return;
        }
        Ok(Ok((product, later_publish))) => { f.publish = Some(later_publish); f.named = Some(product.glb.clone()); Ok(product) }
        Err(error) => Err(error),
    };
    FOLLOW_UPS.lock().unwrap_or_else(|e| e.into_inner()).push((f.job.clone(), started, result));
    follow_up_load().building -= 1;
    makepad_platform::thread::SignalToUI::set_ui_signal();
    hand_over(&f.job, f.publish, f.named, f.after);
}

/// One worker follow-up for all distance representations, installed under
/// the same revision. Unchanged mesh parts, tangents and encoded surface
/// images come from the first compile; load-ready images hit the encode memo.
fn begin_follow_up(request: &ScriptModelRequest, later: BuildLater, cancel: &AtomicBool) -> Result<Derive, String> {
    use makepad_model::Transaction;
    let cancelled = || cancel.load(Ordering::Acquire) || makepad_platform::thread::checkpoint_when_due().is_err();
    let t0 = Instant::now();
    let mut warnings = Vec::new();
    // Full detail gets its own store row before any distance work: a quit
    // during the follow-up resumes from it instead of building again.
    if let Some(glb) = later.detail {
        match later.document.to_bytes(Some(&cancelled)) {
            Ok(source) => cache::save_detail(later.session.clone(), request.hash.clone(), request.title.clone(), glb, source),
            Err(error) => log!("built model {} ({}): full detail not saved: {error}", request.hash, request.title),
        }
    }
    let mut document = later.document;
    let mut compiled = later.compiled;
    let mut lods = later.plan.lods;
    if let Some((distance, cell)) = later.plan.impostor {
        let glb = match later.glb {
            Some(glb) => glb,
            None => {
                // A resumed build already installed its load-ready GLB. The
                // bake needs original images: compile the retained document,
                // without running any of the program's geometry operations.
                let base = document.compile_with_cache(&mut compiled, Some(&cancelled)).map_err(|e| e.to_string())?;
                warnings.extend(base.warnings);
                step_done();
                base.glb
            }
        };
        match makepad_render::impostor::bake(&glb, cell) {
            Some(bake) => {
                // SANDBOX_IMPOSTOR_DUMP=dir: each bake's atlases as PNGs
                // (`<name>-albedo.png`, `<name>-normal.png`, `<name>-depth.png`), to look at.
                if let Ok(dir) = std::env::var("SANDBOX_IMPOSTOR_DUMP") {
                    let name = request.alias.rsplit('/').next().unwrap_or("model").to_string();
                    let n = (bake.normal.len() / 4) as f64;
                    for (kind, px, w) in [("albedo", &bake.albedo, bake.size as u32), ("normal", &bake.normal, n.sqrt() as u32), ("depth", &bake.depth, n.sqrt() as u32)] {
                        if let Ok(png) = makepad_platform::Cx::encode_rgba_as_png(w, w, px) {
                            let _ = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(std::path::Path::new(&dir).join(format!("{name}-{kind}.png")), png));
                        }
                    }
                }
                let limits = makepad_model::Limits { mesh: makepad_model::mesh::Limits { max_bytes: host().memory_budget() }, ..Default::default() };
                let operations = impostor_ops(&bake, distance, next_material(&document), &limits).map_err(|e| format!("impostor: {e}"))?;
                let head = document.head();
                document.apply(Transaction { request_id: "impostor".into(), expected: head, operations }, Some(&cancelled)).map_err(|e| format!("impostor: {e}"))?;
            }
            None => warnings.push("impostor: the model has nothing to draw from the side".into()),
        }
        // Preserve the former combined build's distance calculation, which
        // includes the impostor quad in the bounds used for automatic LODs.
        if !lods.is_empty() { lods = auto_lod_ops(&document); }
    }
    let bake = t0.elapsed();
    step_done();
    let mut work = LastCompile::new(document, lods);
    // A primary that needed the whole budget (or more) gives the follow-up its own share.
    work.set_limit(host().memory_budget());
    Ok(Derive { work, compiled, warnings, bake, lock: later.lock, session: later.session })
}

/// The follow-up's last compile and its product, or the work back with what it needed when its
/// share refused it (`Ok(Err(..))`, see `compile_last`).
fn finish_follow_up(request: &ScriptModelRequest, mut derive: Derive, cancel: &AtomicBool, alone: Option<usize>) -> Result<Result<(Product, PublishLater), (Derive, String)>, String> {
    let cancelled = || cancel.load(Ordering::Acquire) || makepad_platform::thread::checkpoint_when_due().is_err();
    if cancel.load(Ordering::Acquire) || makepad_platform::thread::checkpoint().is_err() {
        return Err("superseded by a newer revision".into());
    }
    let t0 = Instant::now();
    let product = match compile_last(&request.alias, &mut derive.work, &mut derive.compiled, alone, &cancelled)? {
        Ok(product) => product,
        Err(why) => return Ok(Err((derive, why))),
    };
    let mut warnings = derive.warnings;
    warnings.extend(product.warnings);
    let glb = product.glb;
    let t_lods = t0.elapsed();
    step_done();
    let glb = cache::load_ready(glb)?;
    let glb: Arc<[u8]> = Arc::from(glb);
    cache::memory_put(&request.hash, glb.clone());
    let t_prepare = Instant::now();
    let prepared = host().prepare(request, &glb)?;
    let timing = format!("distance representations after install: bake {} ms, lods {} ms, ktx2 {} ms, prepare {} ms", derive.bake.as_millis(), t_lods.as_millis(), (t_prepare - t0 - t_lods).as_millis(), t_prepare.elapsed().as_millis());
    let publish = PublishLater { lock: derive.lock, session: derive.session, glb: glb.clone(), replaces_detail: true };
    Ok(Ok((Product { glb, prepared, cached: false, warnings, review: None, timing, settled: true, publish: None, follow_up: None }, publish)))
}

fn submit(pool: &TaskPool, job: PrimaryJob, limit: BuildLimit, cached_glb: Option<Arc<[u8]>>) -> Result<Running, ScriptModelRequest> {
    // The lane says who waits for the product.
    // - A product the memory front holds only prepares: Interactive, so it never queues behind
    //   compiles. (A headset has one heavy worker: a cached car sat 5 min behind three tree
    //   compiles.)
    // - A compile the person waits for (`BuildHost::waited_on`: a held load): Visible, the class
    //   of the person's other programs. At Background (macOS utility QoS, Linux nice +8), a cargo
    //   build in a terminal took the cores. On a 16-core M-series Mac a fresh load took 14.9 s
    //   quiet and 64.7 s under 16 default-QoS threads, against 30.6 s with the same threads at the
    //   build's own QoS.
    // - Any other compile, and the follow-up of every compile: Background.
    // Pin a prepare-only job's bytes so eviction cannot turn it into a concurrent compile.
    let cached = cached_glb.is_some();
    let request = job.request.clone();
    let started = job.started;
    let lane = if cached { Class::Interactive } else if host().waited_on(&request) { Class::Visible } else { Class::Background };
    let (sender, receiver) = sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let submitted = Instant::now();
    let follow_pool = pool.clone();
    pool.submit(lane, move || {
        let send = |reply| {
            let _ = sender.try_send(reply);
            makepad_platform::thread::SignalToUI::set_ui_signal();
        };
        let queued = submitted.elapsed();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_with_limit(&job.request, job.session.clone(), job.variant.clone(), &flag, limit, cached_glb)))
            .unwrap_or_else(|_| Err("model build panicked".into()));
        let mut output = match result {
            Ok(Primary::Built(output)) => output,
            Ok(Primary::WaitAlone(why)) => return send(Ok(Primary::WaitAlone((job, why)))),
            Err(error) => return send(Err(error)),
        };
        output.timing = format!("queued {} ms on {lane:?}, {}", queued.as_millis(), output.timing);
        let PrimaryJob { request: job, after, .. } = job;
        let publish = output.publish.take();
        let follow_up = output.follow_up.take();
        let named = Some(output.glb.clone());
        // Counted before the product goes out: whoever installs it sees its follow-up coming.
        if follow_up.is_some() {
            FOLLOW_UP_ALIASES.lock().unwrap_or_else(|e| e.into_inner()).push(job.alias.clone());
            FOLLOW_UPS_PENDING.fetch_add(1, Ordering::AcqRel);
            follow_up_load().building += 1;
        }
        send(Ok(Primary::Built(output)));
        let Some(later) = follow_up else { return hand_over(&job, publish, named, after) };
        // Full detail is out. Its distance representations are no one's wait: at Background,
        // on this worker when it is a Background one, else as their own Background job, which
        // frees this lane's worker for the next build.
        let follow = move || run_follow_up(FollowUp { job, flag, publish, named, after }, Stage::Begin(later), None);
        if lane == Class::Background { follow() } else { at_background(&follow_pool, follow) }
    }).map_err(|_| request.clone())?.detach();
    Ok(Running { request, cancel, receiver, started, cached })
}

/// The product is out. What is left waits on the store's answers: no CPU permit is held
/// meanwhile (a dozen saves behind a busy store kept every Background permit for seconds, and
/// the level's texture rows and the next compiles waited for one).
fn hand_over(job: &ScriptModelRequest, publish: Option<PublishLater>, named: Option<Arc<[u8]>>, after: Option<Box<dyn FnOnce(Arc<[u8]>) + Send>>) {
    makepad_platform::thread::without_permit(|| {
        if let Some(PublishLater { lock, session, glb, replaces_detail }) = publish {
            cache::save(session, job.hash.clone(), job.title.clone(), glb, std::time::Duration::from_secs(30), replaces_detail);
            drop(lock);
        }
        if let (Some(glb), Some(after)) = (named, after) { after(glb); }
    });
}

/// `work` as its own Background job. When the pool takes no more jobs (it is closing), it runs
/// here and now instead, so a follow-up's count and its save still settle.
fn at_background(pool: &TaskPool, work: impl FnOnce() + Send + 'static) {
    let slot = Arc::new(std::sync::Mutex::new(Some(work)));
    let queued = slot.clone();
    let submitted = pool.submit(Class::Background, move || {
        let work = queued.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(work) = work { work() }
    });
    match submitted {
        Ok(task) => task.detach(),
        Err(_) => {
            let work = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(work) = work { work() }
        }
    }
}

/// Called every frame by the host: (progress, the products of current
/// revisions that finished since the last call, for the host to install).
pub fn poll() -> (bool, Vec<Finished>) {
    BUILDS.with(|cell| {
        let mut b = cell.borrow_mut();
        let Some(pool) = b.pool.clone() else { return (false, Vec::new()) };
        let mut changed = false;
        let (store, _) = host().store();
        cache::set_session(store.clone());
        {
            // Prepared-texture GPU footprint, logged once it settles (for
            // compression before/after measurements).
            thread_local! { static SEEN: std::cell::Cell<([usize; 5], Option<Instant>)> = const { std::cell::Cell::new(([0; 5], None)) }; }
            let now = makepad_render::material_surface::prepared_texture_upload_stats();
            SEEN.with(|seen| {
                let (last, since) = seen.get();
                if now != last { seen.set((now, Some(Instant::now()))); }
                else if since.is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(2)) {
                    let mib = |b: usize| b as f64 / 1048576.0;
                    let work = makepad_render::material_surface::texture_work_stats();
                    use std::sync::atomic::Ordering::Relaxed;
                    log!("prepared textures on the GPU: {} ({} block-compressed), {:.1} MiB; compressed {:.1} MiB; large uncompressed {:.1} MiB; basis fetch {} x {} ms total, transcode {} ms total; work: decode+mip {} x {} ms, ktx2 remap {} x {} ms, hash {} x {} ms; products fetched {} x {:.1} MiB", now[0], now[1], mib(now[2]), mib(now[3]), mib(now[4]),
                        cache::BASIS_FETCH[1].load(Relaxed), cache::BASIS_FETCH[0].load(Relaxed) / 1000, makepad_render::material_surface::TRANSCODE_MICROS.load(Relaxed) / 1000,
                        work[0].0, work[0].1, work[1].0, work[1].1, work[2].0, work[2].1,
                        cache::PRODUCT_FETCH[0].load(Relaxed), mib(cache::PRODUCT_FETCH[1].load(Relaxed)));
                    seen.set((now, None));
                }
            });
        }
        for (key, bytes) in cache::take_texture_saves() {
            if let Err(error) = pool.submit(Class::Background, move || cache::save_texture(key, &bytes)) { log!("texture save: {error}"); }
        }
        for pending in cache::due_saves(store.as_ref()) {
            if let Err(error) = pool.submit(Class::Visible, move || cache::retry_save(pending)) { log!("model save: {error}"); }
        }
        for request in verbs::take_script_model_requests() {
            host().requested(&request);
            // A newer revision of the same alias replaces a queued one and
            // cancels a running one at its next checkpoint.
            b.waiting.retain(|w| w.alias != request.alias);
            for running in b.running.iter().filter(|r| r.request.alias == request.alias) { running.cancel.store(true, Ordering::Release); }
            b.waiting.push_back(request);
            changed = true;
        }
        let mut finished = Vec::new();
        // Snapshot follow-ups before reading the initial channels. Every
        // follow-up in this snapshot was queued after its initial send, so
        // that initial product is either already installed or received below.
        // Snapshotting afterwards could race a send between the two polls.
        let follow_ups = std::mem::take(&mut *FOLLOW_UPS.lock().unwrap_or_else(|e| e.into_inner()));
        if !follow_ups.is_empty() {
            let mut aliases = FOLLOW_UP_ALIASES.lock().unwrap_or_else(|e| e.into_inner());
            for (request, _, _) in &follow_ups {
                if let Some(at) = aliases.iter().position(|alias| *alias == request.alias) { aliases.swap_remove(at); }
            }
        }
        FOLLOW_UPS_PENDING.fetch_sub(follow_ups.len(), Ordering::AcqRel);
        b.running.retain_mut(|running| match running.receiver.try_recv() {
            Ok(result) => { finished.push((running.request.clone(), running.started, result)); false }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => { finished.push((running.request.clone(), running.started, Err("model build worker stopped".into()))); false }
        });
        // A primary counts its follow-up before sending the first product.
        if b.alone && b.running.iter().all(|r| r.cached) && follow_up_load().building == 0 { b.alone = false; }
        // Drain initial products first: a quick follow-up may already be here,
        // and must never be overwritten by its earlier single-level product.
        for (request, started, result) in follow_ups {
            match result {
                Ok(product) => finished.push((request, started, Ok(Primary::Built(product)))),
                Err(error) => {
                    // Full detail remains installed and ready if optional
                    // distance representations fail; do not fail that revision.
                    if verbs::script_model_wanted(&request.alias).as_deref() == Some(request.hash.as_str()) {
                        log!("script model '{}' follow-up failed: {error}", request.alias);
                    }
                }
            }
        }
        let mut ready = Vec::new();
        for (request, started, result) in finished {
            changed = true;
            // Only the revision the latest eval asked for installs.
            let current = verbs::script_model_wanted(&request.alias).as_deref() == Some(request.hash.as_str());
            match result {
                Ok(Primary::Built(product)) if current => ready.push(Finished { request, started, product }),
                Ok(Primary::WaitAlone(waiting)) if current => b.waiting_alone.push_back(waiting),
                Ok(_) => {}
                Err(error) if current => {
                    log!("script model '{}' failed: {error}", request.alias);
                    verbs::mark_script_model_failed(&request.alias, &request.hash, &error);
                }
                Err(_) => {}
            }
        }
        // Player-facing models first, then placed ones nearest the focus
        // (spawn or debug camera), then models nothing has placed yet.
        if b.waiting.len() > 1 {
            let focus = FOCUS.with(|f| f.get());
            b.waiting.make_contiguous().sort_by_cached_key(|r| verbs::script_model_priority(&r.alias, focus));
        }
        // Look every waiting program up in the store first, off the UI
        // thread: all not yet probed in one batch (one round trip says which
        // exist; only hits are fetched).
        let review_wanted = host().wants_review();
        if let (Some(store), false) = (store.as_ref(), review_wanted) {
            let hashes: Vec<String> = b.waiting.iter()
                .filter(|r| cache::probe_state(&r.hash).is_none() && cache::memory_get(&r.hash).is_none())
                .map(|r| r.hash.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().take(256).collect();
            if !hashes.is_empty() {
                {
                    for hash in &hashes { cache::begin_probe(hash); }
                    let session = store.clone();
                    let _ = pool.submit(Class::Background, move || { cache::run_probe_batch(&session, hashes); step_done(); makepad_platform::thread::SignalToUI::set_ui_signal(); });
                }
            }
        }
        // Products the memory front holds (store hits) only prepare: they
        // start at once instead of queueing behind heavy compiles.
        let mut index = 0;
        while !review_wanted && index < b.waiting.len() {
            if let Some(glb) = cache::memory_get(&b.waiting[index].hash) {
                let request = b.waiting.remove(index).unwrap();
                match submit(&pool, PrimaryJob::new(request), BuildLimit::Share(host().memory_budget()), Some(glb)) {
                    Ok(running) => { b.running.push(running); changed = true; }
                    Err(request) => { verbs::mark_script_model_failed(&request.alias, &request.hash, "model worker pool is closed"); changed = true; }
                }
            } else { index += 1; }
        }
        // Until the store attaches (a few seconds after launch) a build cannot
        // look up a cached product; hold new compiles briefly instead of
        // repeating work the store already has.
        let store_pending = store.is_none() && b.started.elapsed() < std::time::Duration::from_secs(10);
        while !store_pending && !b.alone && b.running.iter().filter(|r| !r.cached).count() < pool.worker_count().max(1) {
            // Compile slots go to programs the store does not have (probed
            // misses, or anything when there is no store).
            let Some(position) = b.waiting.iter().position(|r| review_wanted || store.is_none() || match cache::probe_state(&r.hash) {
                // A full-detail row resumes with the follow-up's work: a compile slot.
                Some(cache::Probe::Miss | cache::Probe::Detail) => true,
                // A hit the memory front has since evicted: the build's own lookup refetches it.
                Some(cache::Probe::Hit) => cache::memory_get(&r.hash).is_none(),
                _ => false,
            }) else { break };
            let request = b.waiting.remove(position).unwrap();
            match submit(&pool, PrimaryJob::new(request), BuildLimit::Share(host().memory_budget()), None) {
                Ok(running) => { b.running.push(running); changed = true; }
                Err(request) => {
                    verbs::mark_script_model_failed(&request.alias, &request.hash, "model worker pool is closed");
                    changed = true;
                }
            }
        }
        // A refused compile runs alone once ordinary compiles and active follow-ups drain.
        // A first product goes first, unless a waiting follow-up holds that hash's build lock.
        b.waiting_alone.retain(|(job, _)| verbs::script_model_wanted(&job.request.alias).as_deref() == Some(job.request.hash.as_str()));
        if !b.alone && b.waiting.is_empty() && b.running.iter().all(|r| r.cached) {
            let mut load = follow_up_load();
            if load.building == 0 {
                let follow_index = match b.waiting_alone.front() {
                    Some((job, _)) => load.waiting.iter().position(|(f, _)| f.job.hash == job.request.hash),
                    None => Some(0),
                };
                if let Some((follow, derive)) = follow_index.and_then(|i| load.waiting.remove(i)) {
                    load.building += 1;
                    drop(load);
                    b.alone = true;
                    let whole = host().alone_budget();
                    if verbs::script_model_wanted(&follow.job.alias).as_deref() == Some(follow.job.hash.as_str()) {
                        log!("script model '{}' follow-up runs alone, under the whole budget of {:.1} MiB", follow.job.alias, mib(whole));
                    } else { follow.flag.store(true, Ordering::Release); }
                    at_background(&pool, move || run_follow_up(follow, Stage::Derive(derive), Some(whole)));
                } else if let Some((job, why)) = b.waiting_alone.pop_front() {
                    drop(load);
                    let whole = host().alone_budget();
                    log!("script model '{}' runs alone, under the whole budget of {:.1} MiB ({why})", job.request.alias, mib(whole));
                    match submit(&pool, job, BuildLimit::Alone(whole), None) {
                        Ok(running) => { b.alone = true; b.running.push(running); }
                        Err(request) => verbs::mark_script_model_failed(&request.alias, &request.hash, "model worker pool is closed"),
                    }
                    changed = true;
                }
            }
        }
        verbs::set_script_models_running(b.running.iter().map(|r| r.request.alias.clone()).collect());
        (changed, ready)
    })
}

#[cfg(test)]
mod auto_lod_tests {
    #[test]
    fn overrides_are_per_part_and_world_space_subpixel_parts_drop() {
        use makepad_model::{CompileCache, Document, Limits, LodLevel, SceneNode, SceneOperation, Transaction, Transform};
        let mut doc = Document::new(Limits::default()).unwrap();
        let authored = vec![
            LodLevel { target_faces: 4, max_error: 0., distance: 150. },
            LodLevel { target_faces: 0, max_error: 0., distance: 200. },
        ];
        let operations = vec![
            super::Operation::Cube { object: "body".into(), size: [2.; 3] },
            super::Operation::Cube { object: "tiny".into(), size: [0.01; 3] },
            super::Operation::Cube { object: "authored".into(), size: [0.2; 3] },
            super::Operation::Scene(SceneOperation::Node { object: "body".into(), node: SceneNode { transform: Transform { translation: [5., 2., 0.], scale: [2.; 3], ..Default::default() }, ..Default::default() } }),
            super::Operation::Scene(SceneOperation::Node { object: "tiny".into(), node: SceneNode { parent: Some("body".into()), ..Default::default() } }),
            super::Operation::Scene(SceneOperation::Node { object: "authored".into(), node: SceneNode { parent: Some("body".into()), ..Default::default() } }),
            super::Operation::Scene(SceneOperation::Lods { object: "authored".into(), levels: authored.clone() }),
        ];
        doc.apply(Transaction { request_id: "parts".into(), expected: doc.head(), operations }, None).unwrap();
        let lods = super::auto_lod_ops(&doc);
        assert_eq!(lods.len(), 2, "an authored part leaves all other parts eligible");
        let radius = 12f64.sqrt();
        for op in &lods {
            let super::Operation::Scene(SceneOperation::Lods { object, levels }) = op else { panic!("LOD operation") };
            assert!((levels[0].distance - radius * 12.).abs() < 1e-10, "model radius includes parent scale and translations");
            assert!((levels[1].distance - radius * 35.).abs() < 1e-10);
            assert_eq!(levels[0].target_faces, 4);
            assert_eq!(levels[1].target_faces, if object == "tiny" { 0 } else { 4 });
            assert!((levels[1].max_error - levels[1].distance * (62f64.to_radians() / 2.).tan() / 2160.).abs() < 1e-12);
            assert_eq!(levels.len(), if object == "tiny" { 2 } else { 3 }, "automatic levels stop at the first empty level");
            if object == "body" {
                assert!((levels[2].distance - radius * 100.).abs() < 1e-10, "third level uses 100 model radii");
                assert_eq!(levels[2].target_faces, 4);
            }
        }
        let product = super::LastCompile::new(doc.clone(), lods).compile(&mut CompileCache::default(), &|| false).unwrap();
        assert!(product.warnings.is_empty(), "a protected cube that cannot reduce keeps its mesh and the build succeeds quietly");
        assert_eq!(doc.scene().lods["authored"], authored, "the authored source remains unchanged");
        let mut plan = makepad_render::asset_lod::AssetLodPlan::parse(&product.glb, 1024 * 1024).unwrap().unwrap();
        let far = plan.variant_glb(1, 1024 * 1024).unwrap();
        let model = makepad_render::StaticModel::parse_glb(&far).unwrap();
        assert_eq!(model.indices.len() / 3, 24, "the tiny part disappears while both larger parts retain geometry");

        for (scale, distances) in [(2., [radius * 12., radius * 35., radius * 100.]), (0.5, [15., 40., 115.])] {
            doc.apply(Transaction { request_id: format!("scale-{scale}"), expected: doc.head(), operations: vec![
                super::Operation::Scene(SceneOperation::Node { object: "body".into(), node: SceneNode { transform: Transform { translation: [5., 2., 0.], scale: [scale; 3], ..Default::default() }, ..Default::default() } }),
            ] }, None).unwrap();
            for (distance, automatic_count) in [
                (distances[0] * 0.5, 0), (distances[0], 0), (distances[1], 1),
                ((distances[1] + distances[2]) * 0.5, 2), (distances[2], 2), (distances[2] + 1., 3),
            ] {
                let hide = LodLevel { target_faces: 0, max_error: 0.123, distance };
                doc.apply(Transaction { request_id: format!("hide-{scale}-{distance}"), expected: doc.head(), operations: vec![
                    super::Operation::Scene(SceneOperation::Lods { object: "authored".into(), levels: vec![hide.clone()] }),
                ] }, None).unwrap();
                let lods = super::auto_lod_ops(&doc);
                assert_eq!(lods.len(), 3, "a lone empty level keeps its part eligible");
                for op in &lods {
                    let super::Operation::Scene(SceneOperation::Lods { object, levels }) = op else { panic!("LOD operation") };
                    let automatic = if object == "authored" {
                        assert_eq!(levels.len(), automatic_count + 1, "only automatic distances strictly below the authored hide distance survive");
                        assert_eq!(levels.last(), Some(&hide), "authored hide distance and error stay unchanged");
                        &levels[..automatic_count]
                    } else {
                        assert_eq!(levels.len(), if object == "tiny" { 2 } else { 3 }, "three levels unless a subpixel part stops early");
                        levels.as_slice()
                    };
                    for (index, level) in automatic.iter().enumerate() {
                        assert!((level.distance - distances[index]).abs() < 1e-10, "automatic distance at scale {scale}, level {index}");
                        assert_eq!(level.target_faces, if object == "tiny" && index == 1 { 0 } else { 4 }, "subpixel target at scale {scale}, level {index}");
                        assert!((level.max_error - distances[index] * (62f64.to_radians() / 2.).tan() / 2160.).abs() < 1e-12, "half-pixel error at scale {scale}, level {index}");
                    }
                }
                let mut applied = doc.clone();
                applied.apply(Transaction { request_id: "auto-lod".into(), expected: applied.head(), operations: lods }, None).unwrap();
                assert_eq!(doc.scene().lods["authored"], vec![hide], "planning and applying automatic levels leave the authored source unchanged");
            }
        }

        let hide = LodLevel { target_faces: 0, max_error: 0.456, distance: 200. };
        for (scale, automatic_count) in [(1., 1), (0.1, 0)] {
            doc.apply(Transaction { request_id: format!("tiny-hide-{scale}"), expected: doc.head(), operations: vec![
                super::Operation::Scene(SceneOperation::Node { object: "tiny".into(), node: SceneNode { parent: Some("body".into()), transform: Transform { scale: [scale; 3], ..Default::default() }, ..Default::default() } }),
                super::Operation::Scene(SceneOperation::Lods { object: "tiny".into(), levels: vec![hide.clone()] }),
            ] }, None).unwrap();
            let lods = super::auto_lod_ops(&doc);
            let levels = lods.iter().find_map(|op| match op {
                super::Operation::Scene(SceneOperation::Lods { object, levels }) if object == "tiny" => Some(levels),
                _ => None,
            }).unwrap();
            assert_eq!(levels.len(), automatic_count + 1, "subpixel cutoff stops automatic levels before the authored empty level");
            assert!(levels[..automatic_count].iter().all(|level| level.target_faces == 4 && level.distance == 15.), "only the nearer non-empty level survives");
            assert_eq!(levels.last(), Some(&hide), "the authored empty level is the sole empty level, unchanged");
            let mut applied = doc.clone();
            applied.apply(Transaction { request_id: "auto-lod".into(), expected: applied.head(), operations: lods }, None).unwrap();
        }
    }

    #[test]
    fn automatic_switches_stay_under_one_reference_pixel() {
        use makepad_gltf::{GltfContainerKind, GltfNode, LoadedGltf};
        use makepad_model::{transform::{self, Matrix4}, CompileCache, Document, Limits, LodLevel, Material,
            Operation, RgbaImage, SceneNode, SceneOperation, SurfaceChannel, SurfaceLayer,
            SurfaceMaterial, SurfaceOperation, Transaction, Transform};
        use std::collections::BTreeMap;

        fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
            std::array::from_fn(|i| a[i] - b[i])
        }
        fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
            a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
        }
        fn bounds(points: impl IntoIterator<Item = [f64; 3]>) -> ([f64; 3], [f64; 3]) {
            let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
            for p in points {
                for axis in 0..3 { lo[axis] = lo[axis].min(p[axis]); hi[axis] = hi[axis].max(p[axis]); }
            }
            (lo, hi)
        }
        fn diagonal(lo: [f64; 3], hi: [f64; 3]) -> f64 {
            let size = sub(hi, lo);
            dot(size, size).sqrt()
        }
        fn world_matrix(nodes: &[GltfNode], parents: &[Option<usize>], mut node: usize) -> Matrix4 {
            let mut world = transform::IDENTITY_MATRIX;
            loop {
                let n = &nodes[node];
                let local = if let Some(m) = n.matrix {
                    // glTF matrices are column-major; the model math is row-major.
                    std::array::from_fn(|r| std::array::from_fn(|c| m[c * 4 + r] as f64))
                } else {
                    Transform {
                        translation: n.translation.unwrap_or([0.; 3]).map(f64::from),
                        rotation: n.rotation.unwrap_or([0., 0., 0., 1.]).map(f64::from),
                        scale: n.scale.unwrap_or([1.; 3]).map(f64::from),
                    }.matrix().unwrap()
                };
                world = transform::matrix_mul(local, world);
                match parents[node] { Some(parent) => node = parent, None => return world }
            }
        }

        // Independent Voronoi-region oracle: no simplifier grid or certification.
        struct Triangle {
            p: [[f64; 3]; 3],
            lo: [f64; 3],
            hi: [f64; 3],
            ab: [f64; 3],
            ac: [f64; 3],
            aa: f64,
            ab_ac: f64,
            cc: f64,
        }
        impl Triangle {
            fn new(p: [[f64; 3]; 3]) -> Self {
                let (lo, hi) = bounds(p);
                let (ab, ac) = (sub(p[1], p[0]), sub(p[2], p[0]));
                Self { p, lo, hi, ab, ac, aa: dot(ab, ab), ab_ac: dot(ab, ac), cc: dot(ac, ac) }
            }
            fn point(&self, w: [f64; 3]) -> [f64; 3] {
                std::array::from_fn(|a| self.p[0][a] * w[0] + self.p[1][a] * w[1] + self.p[2][a] * w[2])
            }
            fn distance_sq(&self, p: [f64; 3]) -> f64 {
                let ap = sub(p, self.p[0]);
                let d1 = dot(self.ab, ap);
                let d2 = dot(self.ac, ap);
                let (v, w);
                if d1 <= 0. && d2 <= 0. { return dot(ap, ap); }
                let d3 = d1 - self.aa;
                let d4 = d2 - self.ab_ac;
                if d3 >= 0. && d4 <= d3 {
                    let bp = sub(p, self.p[1]);
                    return dot(bp, bp);
                }
                let vc = d1 * d4 - d3 * d2;
                if vc <= 0. && d1 >= 0. && d3 <= 0. {
                    v = d1 / (d1 - d3);
                    w = 0.;
                } else {
                    let d5 = d1 - self.ab_ac;
                    let d6 = d2 - self.cc;
                    if d6 >= 0. && d5 <= d6 {
                        let cp = sub(p, self.p[2]);
                        return dot(cp, cp);
                    }
                    let vb = d5 * d2 - d1 * d6;
                    if vb <= 0. && d2 >= 0. && d6 <= 0. {
                        v = 0.;
                        w = d2 / (d2 - d6);
                    } else {
                        let va = d3 * d6 - d5 * d4;
                        if va <= 0. && d4 >= d3 && d5 >= d6 {
                            w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
                            v = 1. - w;
                        } else {
                            let inv = 1. / (va + vb + vc);
                            v = vb * inv;
                            w = vc * inv;
                        }
                    }
                }
                let delta = std::array::from_fn(|a| ap[a] - v * self.ab[a] - w * self.ac[a]);
                dot(delta, delta)
            }
        }
        fn triangles(g: &LoadedGltf, node: usize, world: Matrix4) -> Vec<Triangle> {
            let Some(mesh) = g.document.nodes_slice()[node].mesh else { return Vec::new(); };
            let mut out = Vec::new();
            for primitive in &g.document.meshes_slice()[mesh].primitives {
                assert_eq!(primitive.mode(), 4, "node {node}: triangle primitive");
                let positions = makepad_gltf::read_accessor_f32x3(g, primitive.attributes["POSITION"]).unwrap();
                let indices = makepad_gltf::read_accessor_indices_u32(g, primitive.indices.unwrap()).unwrap();
                assert_eq!(indices.len() % 3, 0, "node {node}: complete triangles");
                for ids in indices.chunks_exact(3) {
                    out.push(Triangle::new(std::array::from_fn(|i|
                        transform::transform_point(world, positions[ids[i] as usize].map(f64::from)))));
                }
            }
            out
        }

        let mut doc = Document::new(Limits::default()).unwrap();
        let hide = LodLevel { target_faces: 0, max_error: 0., distance: 50. };
        let mut operations = vec![
            Operation::sphere("body", 1., 24, 12, true, doc.limits()).unwrap(),
            Operation::cylinder("tyre_l", 0.34, 0.3, 32, true, doc.limits()).unwrap(),
            Operation::cylinder("tyre_r", 0.34, 0.3, 32, true, doc.limits()).unwrap(),
            Operation::Cube { object: "wing".into(), size: [1.6, 0.02, 0.3] },
            Operation::Cube { object: "mirror".into(), size: [0.02; 3] },
            Operation::sphere("decal", 0.3, 16, 8, true, doc.limits()).unwrap(),
            Operation::Cube { object: "interior".into(), size: [1., 0.5, 1.] },
            Operation::Scene(SceneOperation::Lods { object: "interior".into(), levels: vec![hide.clone()] }),
        ];
        let axle = transform::quat_axis_angle([0., 0., 1.], -std::f64::consts::FRAC_PI_2).unwrap();
        for (object, transform) in [
            ("body", Transform { translation: [0., 0.7, 0.], scale: [2.3, 0.6, 1.],
                rotation: transform::quat_axis_angle([0., 1., 0.], 0.5).unwrap() }),
            ("tyre_l", Transform { translation: [-1.4, 0.34, 0.8], rotation: axle, ..Default::default() }),
            ("tyre_r", Transform { translation: [1.4, 0.34, 0.8], rotation: axle, ..Default::default() }),
            ("wing", Transform { translation: [0., 1.2, -2.], ..Default::default() }),
            ("mirror", Transform { translation: [1., 1., 0.9], ..Default::default() }),
        ] {
            operations.push(Operation::Scene(SceneOperation::Node { object: object.into(),
                node: SceneNode { transform, ..Default::default() } }));
        }
        doc.apply(Transaction { request_id: "car".into(), expected: doc.head(), operations }, None).unwrap();
        let faces = doc.object("decal").unwrap().faces().iter().map(|f| f.id).collect();
        doc.apply(Transaction { request_id: "decal-paint".into(), expected: doc.head(), operations: vec![
            Operation::SetMaterial { material: 1, value: Material::default() },
            Operation::Surface(SurfaceOperation::Material { material: 1, value: SurfaceMaterial::default() }),
            Operation::Surface(SurfaceOperation::Layer { material: 1, channel: SurfaceChannel::BaseColor,
                layer: SurfaceLayer::new("paint", RgbaImage { width: 2, height: 2, pixels: [120, 80, 40, 255].repeat(4) }) }),
            Operation::AssignMaterial { object: "decal".into(), faces, material: 1 },
        ] }, None).unwrap();

        let (lo, hi) = bounds(doc.objects().flat_map(|(name, mesh)| {
            let world = doc.scene().world_matrix(name).unwrap();
            mesh.vertices().iter().map(move |v| transform::transform_point(world, v.position))
        }));
        let center: [f64; 3] = std::array::from_fn(|i| (lo[i] + hi[i]) * 0.5);
        let radius = diagonal(lo, hi) * 0.5;
        let pixel_per_metre = 2. * 31f64.to_radians().tan() / 2160.;
        let pixel = |depth: f64| depth * pixel_per_metre;
        let expected_distances = [(12. * radius).max(15.), (35. * radius).max(40.), (100. * radius).max(115.)];
        let lods = super::auto_lod_ops(&doc);
        let planned: BTreeMap<_, _> = lods.iter().map(|op| {
            let Operation::Scene(SceneOperation::Lods { object, levels }) = op else { panic!("LOD operation") };
            (object.clone(), levels.clone())
        }).collect();
        let names = ["body", "tyre_l", "tyre_r", "wing", "mirror", "decal", "interior"];
        assert_eq!(planned.len(), names.len(), "every fixture part has a plan");
        assert_eq!(planned["mirror"].len(), 2, "mirror: retained at D1 and hidden at D2");
        assert_eq!(planned["mirror"][0].target_faces, 4, "mirror: first level is non-empty");
        assert_eq!(planned["mirror"][1].target_faces, 0, "mirror: plan ends with an empty level");
        let mirror_size = diagonal([0.; 3], [0.02; 3]);
        assert!(pixel(expected_distances[0]) < mirror_size && mirror_size < pixel(expected_distances[1]),
            "mirror: {mirror_size} m lies between the first two reference pixels");
        for name in names {
            let levels = &planned[name];
            let automatic = if name == "interior" {
                assert_eq!(levels.last(), Some(&hide), "interior: authored hide is preserved");
                &levels[..levels.len() - 1]
            } else {
                assert_eq!(levels.len(), if name == "mirror" { 2 } else { 3 }, "{name}: automatic level count");
                levels.as_slice()
            };
            for (index, level) in automatic.iter().enumerate() {
                let switch = index + 1;
                assert!((level.distance / expected_distances[index] - 1.).abs() <= 1e-12,
                    "{name} switch {switch}: planned distance {} != {}", level.distance, expected_distances[index]);
                assert!((level.max_error / (0.5 * pixel(level.distance)) - 1.).abs() <= 1e-12,
                    "{name} switch {switch}: planned error {} m is not half a reference pixel", level.max_error);
                assert_eq!(level.target_faces, if name == "mirror" && index == 1 { 0 } else { 4 },
                    "{name} switch {switch}: automatic face target");
            }
        }

        let product = super::LastCompile::new(doc.clone(), lods).compile(&mut CompileCache::default(), &|| false).unwrap();
        assert!(product.warnings.is_empty(), "{:?}", product.warnings);
        let parsed = makepad_gltf::parse_glb_bytes(&product.glb).unwrap();
        let gltf = LoadedGltf { kind: GltfContainerKind::Glb, document: parsed.document,
            buffers: vec![parsed.bin_chunk.unwrap()], source_path: None, base_dir: None };
        let nodes = gltf.document.nodes_slice();
        let mut parents = vec![None; nodes.len()];
        for (parent, node) in nodes.iter().enumerate() {
            for &child in node.children.as_deref().unwrap_or(&[]) {
                assert!(parents[child].replace(parent).is_none(), "node {child}: a single parent");
            }
        }
        let mut weights = Vec::new();
        // Eleven subdivisions give 78 samples, including all edges and corners.
        for i in 0..=11 {
            for j in 0..=11 - i {
                weights.push([(11 - i - j) as f64 / 11., i as f64 / 11., j as f64 / 11.]);
            }
        }
        assert_eq!(weights.len(), 78);
        let axis = transform::normalized([1., 0.35, 1.]).unwrap();
        let forward = axis.map(|v| -v);
        let mut three_non_empty_reduced = false;
        let mut decal_reduced = false;
        for name in names {
            let owner = nodes.iter().position(|n| n.name.as_deref() == Some(name)).unwrap();
            let node = &nodes[owner];
            let ids = node.extensions.as_ref().unwrap().key("MSFT_lod").unwrap().key("ids").unwrap().as_array().unwrap();
            let distances = node.extras.as_ref().unwrap().key("MAKEPAD_lod_distances").unwrap().as_array().unwrap();
            let levels = &planned[name];
            assert_eq!(ids.len(), levels.len(), "{name}: every planned level is delivered");
            assert_eq!(distances.len(), levels.len(), "{name}: every switch has a distance");
            let world = world_matrix(nodes, &parents, owner);
            let mut previous = triangles(&gltf, owner, world);
            let base_count = previous.len();
            assert!(base_count > 0, "{name}: non-empty base mesh");
            for (index, ((id, distance), level)) in ids.iter().zip(distances).zip(levels).enumerate() {
                let switch = index + 1;
                let target = usize::try_from(id.as_u64().unwrap()).unwrap();
                let distance = distance.as_f64().unwrap();
                // Delivery stores distances at f32 render precision; compare exactly there.
                assert_eq!(distance, f64::from(level.distance as f32), "{name} switch {switch}: delivered distance");
                let next = &nodes[target];
                assert_eq!((next.matrix, next.translation, next.rotation, next.scale),
                    (node.matrix, node.translation, node.rotation, node.scale), "{name} switch {switch}: owner transform");
                assert_eq!(next.mesh.is_none(), level.target_faces == 0, "{name} switch {switch}: empty geometry matches plan");
                // Level nodes are detached: render their mesh with the owner's complete world matrix.
                let current = triangles(&gltf, target, world);
                assert_eq!(current.is_empty(), level.target_faces == 0, "{name} switch {switch}: delivered triangle count");
                if name == "decal" {
                    for mesh in [node.mesh.unwrap(), next.mesh.unwrap()] {
                        for primitive in &gltf.document.meshes_slice()[mesh].primitives {
                            let material = &gltf.document.materials_slice()[primitive.material.unwrap()];
                            assert!(material.pbr_metallic_roughness.as_ref().unwrap().base_color_texture.is_some(),
                                "decal switch {switch}: image-sampling material keeps the group UV-bound");
                            let uv = makepad_gltf::read_accessor_f32x2(&gltf, primitive.attributes["TEXCOORD_0"]).unwrap();
                            assert!(!uv.is_empty(), "decal switch {switch}: delivered texture coordinates");
                        }
                    }
                    decal_reduced |= current.len() < base_count;
                }
                if name != "interior" {
                    assert!(!previous.is_empty(), "{name} switch {switch}: previous level is non-empty");
                    if current.is_empty() {
                        let (lo, hi) = bounds(previous.iter().flat_map(|t| t.p));
                        let px = diagonal(lo, hi) / pixel(distance);
                        assert!(px < 1., "{name} switch {switch} previous->empty: world bounding-box diagonal {px:.12} px");
                    } else {
                        let eye: [f64; 3] = std::array::from_fn(|i| center[i] + distance * axis[i]);
                        let reach = pixel(distance + radius);
                        for (direction, from, to) in [
                            ("previous->current", previous.as_slice(), current.as_slice()),
                            ("current->previous", current.as_slice(), previous.as_slice()),
                        ] {
                            let mut maximum = 0f64;
                            let mut candidates: Vec<&Triangle> = Vec::new();
                            for triangle in from {
                                candidates.clear();
                                // Prune once per triangle, on all three axes. This is exact for
                                // distances <= reach; removing farther triangles can only raise
                                // a distance beyond reach, never hide a one-pixel violation.
                                candidates.extend(to.iter().filter(|other| (0..3).all(|a|
                                    other.lo[a] <= triangle.hi[a] + reach && other.hi[a] >= triangle.lo[a] - reach)));
                                for &w in &weights {
                                    let p = triangle.point(w);
                                    let depth = dot(sub(p, eye), forward);
                                    assert!(depth.is_finite() && depth > 0., "{name} switch {switch} {direction}: depth {depth}");
                                    let mut nearest = f64::INFINITY;
                                    for other in &candidates {
                                        let distance_sq = other.distance_sq(p);
                                        assert!(distance_sq.is_finite(), "{name} switch {switch} {direction}: non-finite oracle distance");
                                        nearest = nearest.min(distance_sq);
                                    }
                                    maximum = maximum.max(nearest.sqrt() / pixel(depth));
                                }
                            }
                            assert!(maximum <= 1., "{name} switch {switch} {direction}: {maximum:.12} px at D={distance:.9} m");
                        }
                    }
                    if switch == 3 && levels.iter().all(|l| l.target_faces != 0) && current.len() < base_count {
                        three_non_empty_reduced = true;
                    }
                }
                previous = current;
            }
            if name == "mirror" { assert!(previous.is_empty(), "mirror: delivered final level is empty"); }
        }
        assert!(three_non_empty_reduced, "at least one part has three non-empty automatic levels and a reduced far mesh");
        assert!(decal_reduced, "decal: at least one UV-bound level reduces the base mesh");
    }

    #[test]
    fn full_detail_precedes_one_combined_follow_up_with_identical_final_bytes() {
        use makepad_model::{CompileCache, Operation, ProgramOp, SurfaceChannel, SurfaceLayer, SurfaceMaterial, SurfaceOperation, Transaction};
        let build = |segments: u32, cache: &mut CompileCache| {
            let ops = [ProgramOp::Mesh(makepad_model::MeshPrimitive::Sphere { object: "ball".into(), radius: 1.0, segments, rings: segments / 2, smooth: true }),
                ProgramOp::Op(Operation::Cube { object: "small".into(), size: [0.2; 3] }),
                ProgramOp::Op(Operation::Surface(SurfaceOperation::Material { material: 0, value: SurfaceMaterial::default() })),
                ProgramOp::Op(Operation::Surface(SurfaceOperation::Layer { material: 0, channel: SurfaceChannel::BaseColor,
                    layer: SurfaceLayer::new("paint", makepad_model::RgbaImage { width: 2, height: 2, pixels: [120, 80, 40, 255].repeat(4) }) })),
            ];
            let mut fetch = |_: &str| -> Result<Vec<u8>, String> { Err("no fetch".into()) };
            makepad_model::build_program_with_cache(&ops, makepad_model::Limits::default(), &mut fetch, None, cache).map_err(|e| e.message).unwrap()
        };
        let mut cache = CompileCache::default();
        let dense = build(8, &mut cache);
        assert!(!String::from_utf8_lossy(&dense.compiled.glb).contains("MSFT_lod"), "first full-detail product has no automatic levels");
        let plan = super::FollowUpPlan::new(&dense.document, Some((100.0, 32)), false).expect("one combined follow-up");
        assert!(plan.impostor.is_some() && !plan.lods.is_empty(), "one plan owns both distance representations");
        let mut reference = dense.document.clone();
        reference.apply(Transaction { request_id: "auto-lod".into(), expected: reference.head(), operations: plan.lods.clone() }, None).unwrap();
        let expected = reference.compile(None).unwrap().glb;
        let glb = super::LastCompile::new(dense.document.clone(), plan.lods).compile(&mut cache, &|| false).unwrap().glb;
        let text = String::from_utf8_lossy(&glb);
        assert!(text.contains("MSFT_lod") && text.contains("MAKEPAD_lod_distances"));
        assert_eq!(glb, expected, "reuse must preserve the old single-step GLB bytes");
        assert!(super::FollowUpPlan::new(&reference, None, false).is_none(), "the completed model has no further follow-up");
        // A tiny deterministic bake exercises the combined document path
        // without a GPU, atlas rendering or another asynchronous product.
        let mut combined = dense.document.clone();
        let bake = makepad_render::impostor::ImpostorBake {
            albedo: [180, 100, 60, 255].repeat(4), normal: vec![128, 128, 255, 255], depth: vec![128, 0, 128, 255],
            size: 2, center: [0.; 3], radius: 1.0, crown: false,
        };
        let operations = super::impostor_ops(&bake, 100.0, super::next_material(&combined), combined.limits()).unwrap();
        combined.apply(Transaction { request_id: "impostor".into(), expected: combined.head(), operations }, None).unwrap();
        let lods = super::auto_lod_ops(&combined);
        let mut expected = combined.clone();
        expected.apply(Transaction { request_id: "auto-lod".into(), expected: expected.head(), operations: lods.clone() }, None).unwrap();
        let combined_glb = super::LastCompile::new(combined.clone(), lods).compile(&mut cache, &|| false).unwrap().glb;
        assert_eq!(combined_glb, expected.compile(None).unwrap().glb, "one combined follow-up keeps both impostor and LOD output bytes");
        let small = build(8, &mut CompileCache::default());
        assert!(super::FollowUpPlan::new(&small.document, None, false).is_some(), "small parts receive automatic levels");
    }

    /// Primary and follow-up compiles hand back a share refusal and produce identical bytes
    /// when they run alone, including when the whole budget also refuses them.
    #[test]
    fn a_follow_up_its_share_refuses_waits_then_runs_alone_to_the_same_bytes() {
        use makepad_model::{CompileCache, ProgramOp};
        use super::{BuildLimit, Primary, compile_primary};
        let ops = [ProgramOp::Mesh(makepad_model::MeshPrimitive::Sphere { object: "ball".into(), radius: 1.0, segments: 16, rings: 8, smooth: true })];
        let mut fetch = |_: &str| -> Result<Vec<u8>, String> { Err("no fetch".into()) };
        let whole = makepad_model::Limits::default().mesh.max_bytes;
        let built = makepad_model::build_program_with_cache(&ops, BuildLimit::Uncapped.limits(), &mut fetch, None, &mut CompileCache::default()).map_err(|e| e.message).unwrap();
        let Primary::WaitAlone(why) = compile_primary("ball", &ops, BuildLimit::Share(1), &mut fetch, &|| false).expect("a share refusal hands the primary back") else {
            panic!("one byte cannot build the primary");
        };
        // A fetcher can return the exact same text as a memory refusal.
        let image = ProgramOp::ImageLayer { material: 0, channel: makepad_model::SurfaceChannel::BaseColor, layer: "paint".into(), image: "paint.png".into() };
        let fetched = compile_primary("ball", &[image], BuildLimit::Share(1), &mut |_| Err(why.clone()), &|| false);
        assert!(matches!(fetched, Err(message) if message.ends_with(&why)), "a fetch error fails with its original diagnostic");
        let plan = super::FollowUpPlan::new(&built.document, None, false).expect("the sphere gets automatic levels");
        let expected = super::LastCompile::new(built.document.clone(), plan.lods.clone()).compile(&mut CompileCache::default(), &|| false).unwrap().glb;
        for alone in [whole, 1] {
            let Primary::Built((primary, _)) = compile_primary("ball", &ops, BuildLimit::Alone(alone), &mut fetch, &|| false).unwrap() else {
                panic!("an exclusive primary must finish under {alone} bytes or uncapped");
            };
            assert!(primary.compiled.glb == built.compiled.glb, "the primary that waited makes the uncapped GLB under {alone} bytes");
            assert_eq!(primary.document.limits().mesh.max_bytes, if alone == 1 { usize::MAX } else { whole }, "only the refused whole budget escalates");
            let mut work = super::LastCompile::new(built.document.clone(), plan.lods.clone());
            work.set_limit(1);
            let mut compiled = CompileCache::default();
            let why = match super::compile_last("ball", &mut work, &mut compiled, None, &|| false) {
                Ok(Err(why)) => why,
                other => panic!("a share of one byte hands the follow-up back: {:?}", other.map(|r| r.map(|p| p.glb.len()))),
            };
            assert!(why.starts_with("document working bytes: needs ") && why.ends_with(" MiB, limit 0.0 MiB"), "the refusal says what the follow-up needed: {why}");
            let glb = match super::compile_last("ball", &mut work, &mut compiled, Some(alone), &|| false) {
                Ok(Ok(product)) => product.glb,
                other => panic!("alone under {alone} bytes, the follow-up runs (uncapped past it): {:?}", other.map(|r| r.map(|p| p.glb.len()))),
            };
            assert!(glb == expected, "the follow-up that waited makes the product, byte for byte (alone under {alone} bytes)");
        }
    }

    /// A wide, short model (a bush) gets a far stand-in whose quad carries the bounding
    /// sphere, so it reaches below the base. A copy is grounded by its model's bounds when it
    /// is placed: against the full-detail product (a fresh build, before its follow-up lands)
    /// or against the settled product (a later load: the memory front or the store). Either
    /// way it must stand at the same height and then draw the same level, near and far.
    #[test]
    fn a_copy_stands_and_draws_alike_whichever_product_it_was_placed_against() {
        use makepad_model::{CompileCache, Operation, ProgramOp, SurfaceChannel, SurfaceLayer, SurfaceMaterial, SurfaceOperation, Transaction};
        use makepad_render::{StaticModel, asset_lod::{AssetLodPlan, selected_level}};
        let ops = [
            ProgramOp::Op(Operation::Cube { object: "bush".into(), size: [4.0, 1.0, 4.0] }),
            ProgramOp::Op(Operation::Surface(SurfaceOperation::Material { material: 0, value: SurfaceMaterial::default() })),
            ProgramOp::Op(Operation::Surface(SurfaceOperation::Layer { material: 0, channel: SurfaceChannel::BaseColor,
                layer: SurfaceLayer::new("leaves", makepad_model::RgbaImage { width: 2, height: 2, pixels: [60, 120, 40, 255].repeat(4) }) })),
        ];
        let mut fetch = |_: &str| -> Result<Vec<u8>, String> { Err("no fetch".into()) };
        let mut cache = CompileCache::default();
        let built = makepad_model::build_program_with_cache(&ops, makepad_model::Limits::default(), &mut fetch, None, &mut cache).map_err(|e| e.message).unwrap();
        let full = StaticModel::parse_glb(&built.compiled.glb).unwrap();
        // The follow-up's work (finish_follow_up): the stand-in on the bounding sphere, then
        // automatic levels. A one-texel bake: no GPU.
        let bake = makepad_render::impostor::ImpostorBake {
            albedo: [60, 120, 40, 255].repeat(4), normal: vec![128, 128, 255, 255], depth: vec![128, 0, 128, 255],
            size: 2, center: [0.; 3], radius: 8.25f32.sqrt(), crown: false,
        };
        let mut document = built.document.clone();
        let operations = super::impostor_ops(&bake, 100.0, super::next_material(&document), document.limits()).unwrap();
        document.apply(Transaction { request_id: "impostor".into(), expected: document.head(), operations }, None).unwrap();
        let lods = super::auto_lod_ops(&document);
        let settled_glb = super::LastCompile::new(document.clone(), lods).compile(&mut cache, &|| false).unwrap().glb;
        let settled = StaticModel::parse_glb(&settled_glb).unwrap();
        let bounds = |m: &StaticModel| [m.min.x, m.min.y, m.min.z, m.max.x, m.max.y, m.max.z];
        assert!((full.min.y + 0.5).abs() < 1e-5 && (full.max.y - 0.5).abs() < 1e-5, "the full-detail product is the body: {:?}", bounds(&full));
        assert!(bounds(&full).iter().zip(bounds(&settled)).all(|(a, b)| (a - b).abs() < 1e-5),
            "the settled product's bounds are its body's, not its stand-in's: {:?} against {:?}", bounds(&settled), bounds(&full));
        // Grounded as the host grounds a copy, its base on the ground (y = 0), then drawn
        // from the settled product on both paths.
        let (fresh, cached) = (-full.min.y, -settled.min.y);
        assert!((fresh - cached).abs() < 1e-5, "a copy stands at {fresh} placed against full detail, at {cached} against the settled product");
        let mut plan = AssetLodPlan::parse(&settled_glb, 64 << 20).unwrap().expect("the settled product has automatic levels");
        let distances = plan.distances().to_vec();
        let last = distances.len();
        for (what, across, level) in [("near", 4.0f32, 0), ("far", distances[last - 1] + 10.0, last)] {
            let eye = 1.7f32;
            let picks = [fresh, cached].map(|y| selected_level(&distances, (across * across + (eye - y) * (eye - y)).sqrt()));
            assert_eq!(picks, [level; 2], "{what}: each copy draws the same level");
        }
        // Variant k is the level drawn from distances[k] (level k + 1).
        let far_glb = plan.variant_glb(last - 1, 64 << 20).unwrap();
        let loaded = makepad_gltf::load_gltf_from_bytes(&far_glb, None).unwrap();
        let stand_in = loaded.document.meshes_slice().iter().position(|m| m.name.as_deref() == Some("impostor-quad")).unwrap();
        let quad = makepad_gltf::decode_mesh_primitive(&loaded, stand_in, 0).unwrap();
        let r = bake.radius;
        assert_eq!(quad.texcoords0.unwrap(), [[-r,r], [-r,-r], [r,-r], [r,r]], "far TEXCOORD_0 keeps signed sphere radius");
        let far = StaticModel::parse_glb(&far_glb).unwrap();
        assert!(far.draw_layers.iter().any(|l| l.pbr.surface.as_ref().is_some_and(|s| s.impostor > 0.0)), "the far level draws the stand-in");
    }

    #[test]
    fn a_follow_up_resumed_from_the_full_detail_row_makes_the_fresh_product() {
        use makepad_model::{CompileCache, Construction, ConstructionOperation, Document, Limits, Operation, ProgramOp, SelectionOperation, SelectionQuery, Shape, SurfaceChannel, SurfaceLayer, SurfaceMaterial, SurfaceOperation};
        // A mirrored tyre (a cylinder under x -> -x, like a car's left and
        // right parts): negating its stored normals yields -0.0 components,
        // which the source codec writes as +0.0. Export keys vertices by
        // normal bits, so the built document must hold the decoded values.
        let mirror = [[-1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        let ops = [ProgramOp::Op(Operation::Construction(ConstructionOperation { object: "tyre".into(),
                kind: Construction::Shape { shape: Shape::Cone { bottom: 0.3, top: 0.3, height: 0.2, segments: 12, smooth: true }, material: 0 } })),
            ProgramOp::Op(Operation::Selection(SelectionOperation::Query { object: "tyre".into(), name: "all".into(), query: SelectionQuery::All, element: "vertices".into(), min: 1, max: 1000 })),
            ProgramOp::Op(Operation::Selection(SelectionOperation::Use { object: "tyre".into(), name: "all".into(), element: "vertices".into(),
                operation: Box::new(Operation::Transform { object: "tyre".into(), vertices: Vec::new(), matrix: mirror }) })),
            ProgramOp::Op(Operation::Surface(SurfaceOperation::Material { material: 0, value: SurfaceMaterial::default() })),
            ProgramOp::Op(Operation::Surface(SurfaceOperation::Layer { material: 0, channel: SurfaceChannel::BaseColor,
                layer: SurfaceLayer::new("paint", makepad_model::RgbaImage { width: 2, height: 2, pixels: [120, 80, 40, 255].repeat(4) }) })),
        ];
        let mut fetch = |_: &str| -> Result<Vec<u8>, String> { Err("no fetch".into()) };
        let mut cache = CompileCache::default();
        let built = makepad_model::build_program_with_cache(&ops, Limits::default(), &mut fetch, None, &mut cache).map_err(|e| e.message).unwrap();
        // A later session decodes the full-detail row's source.
        let restored = Document::from_bytes(&built.document.to_bytes(None).unwrap(), Limits::default(), None).unwrap();
        // No plan: the program's product is the one its source compiles (empty cache).
        let reopened = restored.compile(None).unwrap().glb;
        assert!(built.compiled.glb == reopened, "the built product is its source's product, byte for byte ({} vs {} B)", built.compiled.glb.len(), reopened.len());
        // A plan: the follow-up right after the build (in-memory document, the
        // build's cache) and the resumed one (decoded source, empty cache).
        let plan = super::FollowUpPlan::new(&built.document, None, false).expect("the fixture gets automatic levels");
        let fresh = super::LastCompile::new(built.document.clone(), plan.lods).compile(&mut cache, &|| false).unwrap().glb;
        let plan = super::FollowUpPlan::new(&restored, None, false).expect("the resumed build gets the same levels");
        let resumed = super::LastCompile::new(restored.clone(), plan.lods).compile(&mut CompileCache::default(), &|| false).unwrap().glb;
        assert!(String::from_utf8_lossy(&fresh).contains("MSFT_lod"), "the fixture gets automatic levels");
        assert!(fresh == resumed, "a resumed build makes the fresh build's product, byte for byte ({} vs {} B)", fresh.len(), resumed.len());
        let bake = makepad_render::impostor::ImpostorBake {
            albedo: [60, 120, 40, 255].repeat(4), normal: vec![128, 128, 255, 255], depth: vec![128, 0, 128, 255],
            size: 2, center: [0.; 3], radius: 3., crown: false,
        };
        let mut quad = super::impostor_ops(&bake, 100., 0, &Limits::default()).unwrap().pop().unwrap();
        let key = |op| {
            let (_, captured) = super::verbs::capture_programs(|| super::verbs::request_builtin_model("stand-in", "prop", vec![ProgramOp::Op(op)]));
            super::cache::product_key(&captured[0].hash)
        };
        let original = key(quad.clone());
        let Operation::Baked { mesh, .. } = &mut quad else { panic!("stand-in geometry") };
        let x = &mut std::sync::Arc::make_mut(mesh).positions[0][0];
        *x = f32::from_bits(x.to_bits() ^ 1);
        assert_ne!(key(quad), original, "one position byte changes the store product key");
        let with_quad = |mut doc: Document, cache: &mut CompileCache| {
            let ops = super::impostor_ops(&bake, 100., super::next_material(&doc), doc.limits()).unwrap();
            doc.apply(makepad_model::Transaction { expected: doc.head(), request_id: "stand-in".into(), operations: ops }, None).unwrap();
            assert!(doc.geometries().any(|(n, g)| n == "impostor-quad" && matches!(g, makepad_model::ObjectGeometry::Baked(_))));
            let lods = super::auto_lod_ops(&doc);
            assert!(matches!(&lods[..], [Operation::Scene(makepad_model::SceneOperation::Lods { object, levels })]
                if object == "tyre" && levels[0].distance > 45.), "the stand-in affects distances but only the tyre gets levels");
            super::LastCompile::new(doc, lods).compile(cache, &|| false).unwrap().glb
        };
        let fresh = with_quad(built.document, &mut cache);
        assert_eq!(fresh, with_quad(restored, &mut CompileCache::default()), "the resumed stand-in and its material match the fresh one");
    }
}
