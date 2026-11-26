//! Built-model cache: the app's asset store. A level's model program is
//! keyed by its hash; its built product is a `ModelProgram` asset at
//! `gen/built/<hash>-l4` (the GLB with its automatic LODs and collider
//! metadata as the render file; its text source only names the program's hash:
//! the program lives in the Splash that posted it). Every instance,
//! home and LAN client of the store reads it from there, so a program builds
//! once anywhere. A `l4d` / `p4d` row retains the full-detail GLB and built
//! document while distance representations are still being derived.
//! A bounded in-memory map sits in front for repeat lookups,
//! and a lock file per hash makes a second instance on this machine wait for
//! a build already running instead of repeating it.
use crate::{host, StoreSession};
use makepad_asset_client::{AssetClient, ClientConfig, ClientError, PublishBundle, PublishBundleFile, PublishExpectedHead, PublishThumbnail};
use makepad_asset_data::{AssetAlias, AssetId, AssetKind, FileRole, MediaType, ThumbnailMedia};
use std::{collections::HashMap, sync::{Arc, Mutex}, time::{Duration, Instant, SystemTime}};

/// Store alias prefix of built products (the game library leaves them out).
pub const STORE_CACHE_PREFIX: &str = "gen/built/";

/// `gen/built/<key>-l1`. A key that is not one alias segment (the store
/// allows `[a-z0-9_-]`, 48 bytes) is replaced by a stable hash of it.
pub(super) fn alias(key: &str) -> String { alias_with(key, "l1") }

/// Built products carry their images in the load-ready form (UASTC KTX2 in
/// the GLB) and their collision meshes as accessors in the GLB's binary
/// chunk (format `l4`). A product of an older format is not looked up: its
/// program builds again, once.
const PRODUCT_FORMAT: &str = "l4";
/// The same products with their PNG images kept as PNG.
const PNG_PRODUCT_FORMAT: &str = "p4";

/// `SANDBOX_PRODUCT_FORMAT=png`: read and write only products that keep
/// their PNG images, with no conversion (the before side of load-time A/B
/// measurements).
pub(super) fn png_products() -> bool {
    static PNG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PNG.get_or_init(|| std::env::var("SANDBOX_PRODUCT_FORMAT").as_deref() == Ok("png"))
}
fn product_format() -> &'static str { if png_products() { PNG_PRODUCT_FORMAT } else { PRODUCT_FORMAT } }
fn detail_format() -> String { format!("{}d", product_format()) }
pub(super) fn detail_alias(hash: &str) -> String { alias_with(hash, &detail_format()) }

/// The built product's key: its program hash and the product format. Two
/// products of one program in different formats are different GLBs.
pub fn product_key(hash: &str) -> String { format!("{hash}-{}", product_format()) }

fn alias_with(key: &str, format: &str) -> String {
    let valid = !key.is_empty() && key.len() + 1 + format.len() <= 48 && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if valid { return format!("{STORE_CACHE_PREFIX}{key}-{format}"); }
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    let head: String = key.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(24).collect::<String>().to_ascii_lowercase();
    format!("{STORE_CACHE_PREFIX}{}{hash:016x}-{format}", if head.is_empty() { String::new() } else { format!("{}-", head.trim_matches('-')) })
}

thread_local! {
    static LOAD_READY_TIMINGS: std::cell::RefCell<Option<Vec<makepad_render::texture_pack::LoadReadyImageTiming>>> = const { std::cell::RefCell::new(None) };
}

/// The freshly compiled images. Nested
/// builds restore their parent's collector; errors discard only their own.
pub(super) struct LoadReadyTimingScope(Option<Vec<makepad_render::texture_pack::LoadReadyImageTiming>>);
impl LoadReadyTimingScope {
    pub(super) fn new() -> Self { Self(LOAD_READY_TIMINGS.with(|current| current.replace(Some(Vec::new())))) }
    pub(super) fn finish(self) -> Vec<makepad_render::texture_pack::LoadReadyImageTiming> {
        LOAD_READY_TIMINGS.with(|current| current.borrow_mut().take().unwrap())
    }
}
impl Drop for LoadReadyTimingScope {
    fn drop(&mut self) { LOAD_READY_TIMINGS.with(|current| { current.replace(self.0.take()); }); }
}

/// Build time: the GLB with every embedded PNG replaced by its load-ready
/// KTX2 (see `makepad_render::texture_pack`); unchanged when there is none.
pub(super) fn load_ready(glb: Vec<u8>) -> Result<Vec<u8>, String> {
    let pool = crate::POOL.get().ok_or("model build pool is not initialized")?;
    if png_products() { return Ok(glb); }
    let run = makepad_render::texture_compression::pool_executor(pool);
    // How the encode used the pool: its calls and chunks, the wall time inside
    // them and the summed time of the chunks (their ratio is the parallelism).
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    let (calls, chunks, wall_us, work_us, longest_us) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let encode = |n: usize, work: &(dyn Fn(usize) + Sync)| {
        let started = std::time::Instant::now();
        run(n, &|index| {
            let chunk = std::time::Instant::now();
            work(index);
            let us = chunk.elapsed().as_micros() as u64;
            work_us.fetch_add(us, Relaxed);
            longest_us.fetch_max(us, Relaxed);
        });
        calls.fetch_add(1, Relaxed);
        chunks.fetch_add(n as u64, Relaxed);
        wall_us.fetch_add(started.elapsed().as_micros() as u64, Relaxed);
    };
    let started = std::time::Instant::now();
    let measured = LOAD_READY_TIMINGS.with(|current| current.borrow().is_some());
    let mut observe = |image| LOAD_READY_TIMINGS.with(|current| {
        if let Some(timings) = current.borrow_mut().as_mut() { timings.push(image); }
    });
    let ready = makepad_render::texture_pack::embed_load_ready_images_with_timings(&glb, makepad_render::makepad_texcomp::uastc::Quality::Fast, &encode,
        if measured { Some(&mut observe) } else { None })?;
    let (wall, work) = (wall_us.load(Relaxed), work_us.load(Relaxed));
    if wall > 50_000 {
        makepad_platform::log!("load-ready encode: {} ms in all; {} encodes of {} chunks, {} ms inside them for {} ms of chunk work (parallelism {:.1}), longest chunk {} ms",
            started.elapsed().as_millis(), calls.load(Relaxed), chunks.load(Relaxed), wall / 1000, work / 1000, work as f64 / wall.max(1) as f64, longest_us.load(Relaxed) / 1000);
    }
    Ok(ready.unwrap_or(glb))
}

// ── memory front ────────────────────────────────────────────────────────

struct Memory { entries: HashMap<String, (Arc<[u8]>, u64)>, bytes: usize, clock: u64 }
static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);

/// The host's budget (Sandbox: an eighth of its generated-model residency
/// budget): products stay in RAM while they are likely to be asked for again
/// (a level re-run, a split pane); the least recently used go first. The
/// store keeps everything. This is the model cache's only RAM copy of what it
/// reads: its store client fetches without keeping (`fetch_blob_bytes_unkept`).
fn memory_budget() -> usize { host().memory_budget() }

pub(super) fn memory_get(hash: &str) -> Option<Arc<[u8]>> {
    let mut memory = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    let memory = memory.as_mut()?;
    memory.clock += 1;
    let clock = memory.clock;
    memory.entries.get_mut(hash).map(|(glb, used)| { *used = clock; glb.clone() })
}

pub(super) fn memory_put(hash: &str, glb: Arc<[u8]>) {
    let budget = memory_budget();
    let mut memory = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    let memory = memory.get_or_insert_with(|| Memory { entries: HashMap::new(), bytes: 0, clock: 0 });
    memory.clock += 1;
    let clock = memory.clock;
    if let Some((old, _)) = memory.entries.insert(hash.to_string(), (glb.clone(), clock)) { memory.bytes -= old.len(); }
    memory.bytes += glb.len();
    while memory.bytes > budget && memory.entries.len() > 1 {
        let Some(oldest) = memory.entries.iter().filter(|(k, _)| k.as_str() != hash).min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone()) else { break };
        if let Some((old, _)) = memory.entries.remove(&oldest) { memory.bytes -= old.len(); }
    }
}

// ── store probes ────────────────────────────────────────────────────────

/// Store lookups ahead of the build queue. A request whose probe is still
/// out takes no compile slot; a hit lands in the memory front (and installs
/// without a compile slot); only a miss is compiled. `Detail`: no complete
/// product, but a full-detail row the build resumes from (or, without a
/// batch answer, maybe one: the build looks itself).
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Probe { Pending, Miss, Detail, Hit }
static PROBES: Mutex<Option<HashMap<String, (Probe, Duration)>>> = Mutex::new(None);

pub(super) fn probe_state(hash: &str) -> Option<Probe> {
    PROBES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(hash).map(|p| p.0))
}
/// Time the store lookup of `hash` took (for the ready log).
pub(super) fn lookup_time(hash: &str) -> Option<Duration> {
    PROBES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(hash).filter(|p| p.0 != Probe::Pending).map(|p| p.1))
}
pub(super) fn begin_probe(hash: &str) {
    PROBES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(hash.to_string(), (Probe::Pending, Duration::ZERO));
}
/// Worker body of a batch of probes (a level's worth, on first visit): one
/// `alias_status` round trip answers which products (and full-detail rows)
/// exist, then only the hits are fetched, from their known heads (no
/// per-product resolve), up to eight at once. The store serializes RPCs, so
/// fewer round trips is what makes a first visit faster.
pub(super) fn run_probe_batch(session: &StoreSession, hashes: Vec<String>) {
    let started = Instant::now();
    let (format, detail) = (product_format(), detail_format());
    let statuses = (|| -> Result<HashMap<String, makepad_asset_client::dto::AliasStatusDto>, String> {
        let mut entries = Vec::new();
        for hash in &hashes {
            entries.push((AssetAlias::new(alias_with(hash, format)).map_err(|e| e.to_string())?, None));
            entries.push((AssetAlias::new(alias_with(hash, &detail)).map_err(|e| e.to_string())?, None));
        }
        let mut answers = HashMap::new();
        for chunk in entries.chunks(makepad_asset_client::wire::MAX_ALIAS_STATUS_ITEMS) {
            let chunk = with_client(session, |client| client.alias_status(chunk, &[]).map_err(|e| format!("alias status: {e}")))?;
            answers.extend(chunk.into_iter().map(|a| (a.alias.as_str().to_string(), a)));
        }
        Ok(answers)
    })();
    let statuses = match statuses {
        Ok(statuses) => Some(statuses),
        Err(error) => { makepad_platform::log!("built models: batched lookup failed ({error}); looking up one by one"); None }
    };
    let probe = |hash: &String| {
        let head = |format: &str| statuses.as_ref().and_then(|s| s.get(&alias_with(hash, format))).filter(|c| c.present).and_then(|c| c.head_revision.clone());
        let found = match (statuses.is_some(), head(format)) {
            (true, Some(head)) => fetch_at_head(session, hash, &head, FileRole::RenderGlb).map(|b| b.filter(|b| b.starts_with(b"glTF"))),
            (true, None) => Ok(None),
            // No batch answer: one lookup.
            (false, _) => store_get(session, hash),
        };
        let state = match found {
            Ok(Some(glb)) => { memory_put(hash, Arc::from(glb)); Probe::Hit }
            Ok(None) if statuses.is_none() || head(&detail).is_some() => Probe::Detail,
            Ok(None) => Probe::Miss,
            Err(error) => { makepad_platform::log!("built model {hash}: store lookup failed, building: {error}"); Probe::Miss }
        };
        PROBES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(hash.clone(), (state, started.elapsed()));
    };
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..hashes.len().min(8) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(hash) = hashes.get(i) else { break };
                probe(hash);
            });
        }
    });
}

/// A probed hash that was built and saved is a hit from now on.
pub(super) fn note_saved(hash: &str) {
    if let Some(m) = PROBES.lock().unwrap_or_else(|e| e.into_inner()).as_mut() { if let Some(p) = m.get_mut(hash) { p.0 = Probe::Hit; } }
}

// ── store ───────────────────────────────────────────────────────────────

/// One connected client per store session; every lookup works on its own
/// lane clone (a clone shares the verified cache and opens a connection
/// per call), so no lookup waits for a pooled client. Only the first
/// connect (a health check and a credential probe) is waited on, once.
struct Base { endpoints: makepad_asset_client::ApiEndpoints, server_id: [u8; 16], client: AssetClient }
static BASE: Mutex<Option<Base>> = Mutex::new(None);

pub fn with_client<R>(session: &StoreSession, f: impl FnOnce(&mut AssetClient) -> Result<R, String>) -> Result<R, String> {
    // Store work is waiting (the first connect syncs the cache root and probes the server, the
    // calls wait for answers): a pool job holds no CPU permit over it, and is admitted again
    // after the client lock is let go.
    makepad_platform::thread::without_permit(|| with_client_waiting(session, f))
}
fn with_client_waiting<R>(session: &StoreSession, f: impl FnOnce(&mut AssetClient) -> Result<R, String>) -> Result<R, String> {
    let mut client = {
        let mut base = BASE.lock().unwrap_or_else(|e| e.into_inner());
        if !base.as_ref().is_some_and(|b| b.endpoints == session.endpoints && b.server_id == session.server_id) {
            // Drop the old client first: a cache root is single-owner.
            *base = None;
            let mut config = ClientConfig::new(host().client_root());
            config.token = session.token.clone();
            let client = AssetClient::connect(config, session.endpoints, Some(session.server_id)).map_err(|e| format!("model cache client: {e}"))?;
            *base = Some(Base { endpoints: session.endpoints, server_id: session.server_id, client });
        }
        base.as_ref().unwrap().client.lane_clone()
    };
    f(&mut client)
}

/// The app is shutting down: the store client (and with it its cache root's single-owner lock)
/// and the prepared-texture session go. A relaunch of the app in the same process found this
/// run's client still holding the root ("cache root is owned by another process").
pub fn release_store_client() {
    *BASE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Connect once the store attaches, so the first level's lookups do not pay
/// for it.
pub(super) fn prewarm(session: &StoreSession) { let _ = with_client(session, |_| Ok(())); }
/// (count, bytes) of built products fetched from the store since launch.
pub static PRODUCT_FETCH: [std::sync::atomic::AtomicUsize; 2] = [const { std::sync::atomic::AtomicUsize::new(0) }; 2];

/// The built product of `hash`, when the store has it in this format.
pub(crate) fn store_get(session: &StoreSession, hash: &str) -> Result<Option<Vec<u8>>, String> {
    let found = store_get_role(session, &alias_with(hash, product_format()), FileRole::RenderGlb)?.filter(|b| b.starts_with(b"glTF"));
    if let Some(glb) = &found {
        PRODUCT_FETCH[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        PRODUCT_FETCH[1].fetch_add(glb.len(), std::sync::atomic::Ordering::Relaxed);
    }
    Ok(found)
}

pub(super) struct StoredDetail { pub glb: Vec<u8>, pub source: Vec<u8> }

/// Both files come from one immutable revision. Decode on the build worker
/// after it has acquired the same lock as a fresh build.
pub(super) fn store_get_detail(session: &StoreSession, hash: &str) -> Result<Option<StoredDetail>, String> {
    let name = detail_alias(hash);
    let alias = AssetAlias::new(name.clone()).map_err(|e| e.to_string())?;
    with_client(session, |client| {
        let head = match client.resolve_alias(&alias) {
            Ok(head) => head.head_revision,
            Err(ClientError::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(format!("model cache lookup: {error}")),
        };
        let manifest = client.fetch_asset_manifest(&head).map_err(|e| format!("model cache manifest: {e}"))?;
        let mut fetch = |role, media| {
            let file = manifest.files.iter().find(|f| f.role == role && f.media == media)
                .ok_or_else(|| format!("missing {role:?}/{media:?} file"))?;
            client.fetch_blob_bytes_unkept(&file.blob, Some(file.byte_len)).map_err(|e| format!("model cache blob: {e}"))
        };
        let glb = fetch(FileRole::RenderGlb, MediaType::Glb)?;
        let source = fetch(FileRole::Source, MediaType::Bin)?;
        PRODUCT_FETCH[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        PRODUCT_FETCH[1].fetch_add(glb.len(), std::sync::atomic::Ordering::Relaxed);
        Ok(Some(StoredDetail { glb, source }))
    }).map_err(|error| format!("full-detail row '{name}': {error}"))
}

fn store_get_role(session: &StoreSession, name: &str, role: FileRole) -> Result<Option<Vec<u8>>, String> {
    let alias = AssetAlias::new(name.to_string()).map_err(|e| e.to_string())?;
    let head = with_client(session, |client| match client.resolve_alias(&alias) {
        Ok(head) => Ok(Some(head.head_revision)),
        Err(ClientError::NotFound { .. }) => Ok(None),
        Err(error) => Err(format!("model cache lookup: {error}")),
    })?;
    match head { Some(head) => fetch_at_head(session, name, &head, role), None => Ok(None) }
}

/// The `role` file of revision `head` (its lowest LOD).
fn fetch_at_head(session: &StoreSession, name: &str, head: &makepad_asset_data::AssetRevisionId, role: FileRole) -> Result<Option<Vec<u8>>, String> {
    let t0 = Instant::now();
    with_client(session, |client| {
        let manifest = client.fetch_asset_manifest(head).map_err(|e| format!("model cache manifest: {e}"))?;
        let Some(file) = manifest.files.iter().filter(|f| f.role == role).min_by_key(|f| f.lod) else { return Ok(None) };
        let t1 = Instant::now();
        let bytes = client.fetch_blob_bytes_unkept(&file.blob, Some(file.byte_len)).map_err(|e| format!("model cache blob: {e}"))?;
        if t0.elapsed().as_millis() > 1000 {
            makepad_platform::log!("built model {name}: slow store fetch {} ms (manifest {} ms, blob {} ms, {} KiB)",
                t0.elapsed().as_millis(), (t1 - t0).as_millis(), t1.elapsed().as_millis(), bytes.len() >> 10);
        }
        Ok(Some(bytes))
    })
}

/// Publish the built product of `hash`. An alias another instance published
/// first is success: the content is the same program's product.
pub(crate) fn store_put(session: &StoreSession, hash: &str, title: &str, glb: &[u8], source: Option<&[u8]>) -> Result<(), String> {
    let pool = crate::POOL.get().ok_or("model build pool is not initialized")?;
    let alias = AssetAlias::new(if source.is_some() { detail_alias(hash) } else { alias_with(hash, product_format()) }).map_err(|e| e.to_string())?;
    // The store holds exactly the load-ready form (a no-op for products the
    // model builder already converted).
    let converted = if png_products() { None } else {
        let encode = makepad_render::texture_compression::pool_executor(pool);
        makepad_render::texture_pack::embed_load_ready_images(glb, makepad_render::makepad_texcomp::uastc::Quality::Fast, &encode)?
    };
    let glb = converted.as_deref().unwrap_or(glb);
    let mut bundle = PublishBundle::new(
        "gen", AssetKind::ModelProgram, format!("{title} (built model)"),
        vec![
            PublishBundleFile::bytes(FileRole::Source, MediaType::Text, format!("model program {hash}: built from the Splash that posted it (game.model_new)\n").into_bytes(), None),
            PublishBundleFile::bytes(FileRole::RenderGlb, MediaType::Glb, glb.to_vec(), None),
        ],
        PublishThumbnail::plain(thumbnail()?, ThumbnailMedia::Png, 256, 256),
        makepad_asset_client::PublishRights::generated_cc0(),
    );
    if let Some(source) = source {
        // Source admits Bin, but ModelProgram also requires Source/Text.
        // Retain that text at lod 0 and use a distinct source slot for the
        // document; readers select it by role AND media, never by lowest LOD.
        let mut file = PublishBundleFile::bytes(FileRole::Source, MediaType::Bin, source.to_vec(), None);
        file.lod = 1;
        bundle.files.push(file);
    }
    let metrics = glb_metrics(glb)?;
    bundle.stats.triangles = metrics.triangles;
    bundle.stats.vertices = metrics.vertices;
    bundle.stats.joints = metrics.joints;
    bundle.stats.clips = metrics.clips;
    bundle.capabilities.rigged = metrics.joints > 0;
    bundle.capabilities.animated = metrics.clips > 0;
    let [lo, hi] = metrics.bounds;
    bundle.bounds = makepad_asset_data::Bounds { min: makepad_asset_data::Vec3::new(lo[0], lo[1], lo[2]), max: makepad_asset_data::Vec3::new(hi[0], hi[1], hi[2]) };
    bundle.alias = Some(alias);
    bundle.asset_id = Some(mint_asset_id(hash));
    bundle.expected_head = PublishExpectedHead::Absent;
    bundle.generator = "makepad-model".into();
    // A cache row, never a catalogue entry: the game's named entry
    // (model_build::catalog) carries the vehicle tags.
    bundle.tags = vec!["build-cache".into()];
    bundle.description = format!("Built product of model program {hash} (game.model_new); cached by program hash.");
    with_client(session, |client| match client.publish_bundle_with(&bundle, None, &|| false) {
        Ok(_) | Err(ClientError::Server { status: 409, .. }) => Ok(()),
        Err(error) => Err(format!("model cache publish: {error}")),
    })
}

/// What the store's content contract records for a mesh: counts summed
/// over every mesh and primitive (static or skinned), and the bounds of the
/// model as it stands: every mesh a scene node draws, through its node
/// transforms ([`model_points`]). A built model places its parts by node
/// (a car's fenders, wheels and roof are each a translated node), so the
/// union of the meshes' own spaces is not its size: a 5 m sedan measured
/// 1.7 x 2.9 x 5.3 that way and a spawned copy was fitted to that.
pub struct GlbMetrics { pub triangles: u32, pub vertices: u32, pub joints: u16, pub clips: u16, pub bounds: [[f32; 3]; 2] }

pub fn glb_metrics(glb: &[u8]) -> Result<GlbMetrics, String> {
    let loaded = makepad_gltf::load_gltf_from_bytes(glb, None).map_err(|e| format!("built GLB: {e:?}"))?;
    let (mut triangles, mut vertices) = (0u64, 0u64);
    let mut bounds = [[f32::MAX; 3], [f32::MIN; 3]];
    for (m, mesh) in loaded.document.meshes_slice().iter().enumerate() {
        for p in 0..mesh.primitives.len() {
            let decoded = makepad_gltf::decode_mesh_primitive(&loaded, m, p).map_err(|e| format!("built GLB primitive: {e:?}"))?;
            vertices += decoded.positions.len() as u64;
            triangles += if decoded.indices.is_empty() { decoded.positions.len() / 3 } else { decoded.indices.len() / 3 } as u64;
            for q in &decoded.positions { for d in 0..3 { bounds[0][d] = bounds[0][d].min(q[d]); bounds[1][d] = bounds[1][d].max(q[d]); } }
        }
    }
    if triangles == 0 || vertices < 3 { return Err("built GLB has no triangles".into()); }
    // The scene's bounds; a file whose nodes draw no mesh keeps the
    // mesh-space union above.
    let points = loaded_points(&loaded);
    if !points.is_empty() {
        bounds = [[f32::MAX; 3], [f32::MIN; 3]];
        for q in &points { for d in 0..3 { bounds[0][d] = bounds[0][d].min(q[d]); bounds[1][d] = bounds[1][d].max(q[d]); } }
    }
    let joints = loaded.document.skins.as_deref().unwrap_or(&[]).iter()
        .map(|s| match s { makepad_gltf::JsonValue::Object(o) => match o.get("joints") { Some(makepad_gltf::JsonValue::Array(j)) => j.len(), _ => 0 }, _ => 0 })
        .max().unwrap_or(0);
    let clips = loaded.document.animations.as_deref().map_or(0, |a| a.len());
    Ok(GlbMetrics {
        triangles: triangles.min(u32::MAX as u64) as u32, vertices: vertices.min(u32::MAX as u64) as u32,
        joints: joints.min(u16::MAX as usize) as u16, clips: clips.min(u16::MAX as usize) as u16, bounds,
    })
}

type M4 = [f32; 16];
fn m_mul(a: &M4, b: &M4) -> M4 {
    let mut o = [0.0; 16];
    for c in 0..4 { for r in 0..4 { o[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum(); } }
    o
}
fn node_matrix(n: &makepad_gltf::GltfNode) -> M4 {
    if let Some(m) = n.matrix { return m; }
    let t = n.translation.unwrap_or([0.0; 3]);
    let s = n.scale.unwrap_or([1.0; 3]);
    let [x, y, z, w] = n.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let r = [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + w * z), 2.0 * (x * z - w * y),
        2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x),
        2.0 * (x * z + w * y), 2.0 * (y * z - w * x), 1.0 - 2.0 * (x * x + y * y)];
    [r[0] * s[0], r[1] * s[0], r[2] * s[0], 0.0, r[3] * s[1], r[4] * s[1], r[5] * s[1], 0.0, r[6] * s[2], r[7] * s[2], r[8] * s[2], 0.0, t[0], t[1], t[2], 1.0]
}

/// Every vertex of a (static) GLB in model space, through its node
/// transforms; a skinned mesh in its bind pose. Empty when it does not load.
pub fn model_points(glb: &[u8]) -> Vec<[f32; 3]> {
    match makepad_gltf::load_gltf_from_bytes(glb, None) {
        Ok(loaded) => loaded_points(&loaded),
        Err(_) => Vec::new(),
    }
}

fn loaded_points(loaded: &makepad_gltf::LoadedGltf) -> Vec<[f32; 3]> {
    let doc = &loaded.document;
    let nodes = doc.nodes_slice();
    let mut is_child = vec![false; nodes.len()];
    for n in nodes { for c in n.children.iter().flatten() { if let Some(f) = is_child.get_mut(*c) { *f = true; } } }
    let identity = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
    let mut stack: Vec<(usize, M4)> = (0..nodes.len()).filter(|i| !is_child[*i]).map(|i| (i, identity)).collect();
    let mut out = Vec::new();
    let mut visits = 0;
    while let Some((i, parent)) = stack.pop() {
        visits += 1;
        if visits > 65536 { break; }
        let node = &nodes[i];
        let world = m_mul(&parent, &node_matrix(node));
        for c in node.children.iter().flatten() { if *c < nodes.len() { stack.push((*c, world)); } }
        let Some(mesh) = node.mesh else { continue };
        let m = if node.skin.is_some() { identity } else { world };
        let Some(gm) = doc.meshes_slice().get(mesh) else { continue };
        for p in 0..gm.primitives.len() {
            let Ok(prim) = makepad_gltf::decode_mesh_primitive(loaded, mesh, p) else { continue };
            out.extend(prim.positions.iter().map(|v| [
                m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12],
                m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13],
                m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14],
            ]));
        }
    }
    out
}

// ── saves that failed ───────────────────────────────────────────────────

/// A product whose store save failed for a transient reason (the store was
/// away, a connection dropped). The model is already installed from memory;
/// the save is retried with backoff until it lands.
/// `source`: a full-detail row (the GLB and its built document);
/// `replaces_detail`: a complete product whose save retires that row.
#[derive(Clone)]
pub(super) struct PendingSave { pub hash: String, pub title: String, pub glb: Arc<[u8]>, pub session: Option<StoreSession>, source: Option<Arc<[u8]>>, replaces_detail: bool, due: Instant, delay: Duration }
static RETRY: Mutex<Vec<PendingSave>> = Mutex::new(Vec::new());

/// This process's full-detail rows by program hash: queued or publishing,
/// in the store, or replaced by the complete product while a save was still
/// out (that save is then skipped, or its row retired once it lands). A quick
/// follow-up often saves its complete product before its detail save runs.
#[derive(Clone, Copy, PartialEq)]
enum DetailRow { Saving, Saved, Replaced }
static DETAIL_ROWS: Mutex<Option<HashMap<String, DetailRow>>> = Mutex::new(None);
fn detail_rows<R>(f: impl FnOnce(&mut HashMap<String, DetailRow>) -> R) -> R {
    f(DETAIL_ROWS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

/// The follow-up has serialized the document. The store work runs as its
/// own job, so the follow-up never waits for publication.
pub(super) fn save_detail(session: Option<StoreSession>, hash: String, title: String, glb: Arc<[u8]>, source: Vec<u8>) {
    let pending = PendingSave { hash, title, glb, session, source: Some(Arc::from(source)), replaces_detail: false, due: Instant::now(), delay: Duration::from_secs(30) };
    let job = pending.clone();
    detail_rows(|rows| rows.insert(pending.hash.clone(), DetailRow::Saving));
    let submitted = match crate::POOL.get() {
        Some(pool) => pool.submit(makepad_platform::thread::Class::Background, move || {
            makepad_platform::thread::without_permit(|| save_pending(job));
        }).map_err(|e| e.to_string()),
        None => Err("model build pool is not initialized".to_string()),
    };
    match submitted {
        Ok(task) => task.detach(),
        Err(error) => {
            makepad_platform::log!("built model {} ({}) full-detail save queued for retry: {error}", pending.hash, pending.title);
            RETRY.lock().unwrap_or_else(|e| e.into_inner()).push(pending);
        }
    }
}

/// Save now; on a transient failure queue a retry. A product the content
/// contract refuses is logged once (retrying cannot change the answer).
pub(super) fn save(session: Option<StoreSession>, hash: String, title: String, glb: Arc<[u8]>, delay: Duration, replaces_detail: bool) {
    save_pending(PendingSave { hash, title, glb, session, source: None, replaces_detail, due: Instant::now(), delay });
}

/// The complete product of `hash` is saved: its full-detail row has done its
/// job. A detail save still out is told to stand down; a waiting retry is
/// dropped; a row in the store (this run's, or an earlier run's) is retired.
fn replace_detail(store: &StoreSession, hash: &str) {
    let retire = detail_rows(|rows| match rows.get(hash) {
        Some(DetailRow::Saving) => {
            let mut retry = RETRY.lock().unwrap_or_else(|e| e.into_inner());
            let waiting = retry.len();
            retry.retain(|p| !(p.source.is_some() && p.hash == hash));
            // A dropped retry never published (unless a failed answer hid a
            // publish that landed: the retire finds that row too).
            if retry.len() < waiting { rows.remove(hash); true } else { rows.insert(hash.to_string(), DetailRow::Replaced); false }
        }
        _ => { rows.remove(hash); true }
    });
    if retire { retire_detail(store, hash); }
}

fn retire_detail(store: &StoreSession, hash: &str) {
    let name = detail_alias(hash);
    let retired = AssetAlias::new(name.clone()).map_err(|e| e.to_string()).and_then(|alias| with_client(store, |client| match client.resolve_alias(&alias) {
        Ok(found) => client.retire_asset(&found.asset_id).map(|_| true).map_err(|e| format!("retire: {e}")),
        Err(ClientError::NotFound { .. }) => Ok(false),
        Err(error) => Err(format!("lookup: {error}")),
    }));
    match retired {
        Ok(true) => makepad_platform::log!("built model {hash}: full-detail row {name} retired; the complete product replaces it"),
        Ok(false) => {}
        Err(error) => makepad_platform::log!("built model {hash}: full-detail row {name} not retired: {error}"),
    }
}

fn save_pending(mut pending: PendingSave) {
    // The complete product saved first: nothing is left to resume from.
    if pending.source.is_some() && detail_rows(|rows| rows.get(&pending.hash) == Some(&DetailRow::Replaced)) {
        detail_rows(|rows| rows.remove(&pending.hash));
        return;
    }
    // Built before the store attached (the effect models at startup): wait for it.
    let Some(store) = pending.session.as_ref() else {
        RETRY.lock().unwrap_or_else(|e| e.into_inner()).push(pending);
        return;
    };
    let (hash, title) = (&pending.hash, &pending.title);
    let detail = if pending.source.is_some() { " full detail" } else { "" };
    match store_put(store, hash, title, &pending.glb, pending.source.as_deref()) {
        Ok(()) if pending.source.is_some() => {
            makepad_platform::log!("built model {hash} ({title}) full detail saved to the asset store");
            // Replaced while this save was out: the complete product is in.
            let replaced = detail_rows(|rows| if rows.get(hash) == Some(&DetailRow::Replaced) { rows.remove(hash); true } else { rows.insert(hash.clone(), DetailRow::Saved); false });
            if replaced { retire_detail(store, hash); }
        }
        Ok(()) => {
            note_saved(hash);
            makepad_platform::log!("built model {hash} ({title}) saved to the asset store");
            if pending.replaces_detail { replace_detail(store, hash); }
        }
        Err(error) if error.contains("content contract") || error.contains("no triangles") => {
            if pending.source.is_some() { detail_rows(|rows| rows.remove(hash)); }
            makepad_platform::log!("built model {hash} ({title}){detail} not cached: {error}");
        }
        Err(error) => {
            makepad_platform::log!("built model {hash} ({title}){detail} not cached yet, retrying in {} s: {error}", pending.delay.as_secs());
            pending.due = Instant::now() + pending.delay;
            RETRY.lock().unwrap_or_else(|e| e.into_inner()).push(pending);
        }
    }
}

/// Retries whose time has come (the caller runs each on a worker); saves
/// still waiting for a store take `session` once there is one.
pub(super) fn due_saves(session: Option<&StoreSession>) -> Vec<PendingSave> {
    let mut retry = RETRY.lock().unwrap_or_else(|e| e.into_inner());
    if retry.is_empty() { return Vec::new(); }
    let now = Instant::now();
    let (mut due, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut *retry).into_iter()
        .partition(|p| p.due <= now && (p.session.is_some() || session.is_some()));
    *retry = wait;
    for p in &mut due { if p.session.is_none() { p.session = session.cloned(); } }
    due
}

pub(super) fn retry_save(mut p: PendingSave) {
    p.delay = (p.delay * 2).min(Duration::from_secs(600));
    // A wait on the store, like the first save: without the job's CPU permit.
    makepad_platform::thread::without_permit(|| save_pending(p));
}

// ── prepared textures ───────────────────────────────────────────────────

/// The store session prepared-texture lookups use from worker threads.
static SESSION: Mutex<Option<StoreSession>> = Mutex::new(None);
pub(super) fn set_session(session: Option<StoreSession>) {
    let mut slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let same = match (&*slot, &session) { (Some(a), Some(b)) => a.endpoints == b.endpoints && a.server_id == b.server_id, (None, None) => true, _ => false };
    if !same { *slot = session; }
}
fn session() -> Option<StoreSession> { SESSION.lock().unwrap_or_else(|e| e.into_inner()).clone() }

/// Basis/UASTC forms of source images (render's `basis_texture_key`) cached
/// in the store next to the models that use them: `gen/built/tex-<key>-l1`,
/// a `texture` asset whose one file is the KTX2. Saves are queued for the
/// pool. (Mipped RGBA chains are not stored: decoding and mipping a source
/// costs about what the fetch did.)
pub(super) struct TextureStore;
/// (worker-microseconds, count) of Basis store fetches since launch.
pub static BASIS_FETCH: [std::sync::atomic::AtomicUsize; 2] = [const { std::sync::atomic::AtomicUsize::new(0) }; 2];

static TEXTURE_SAVES: Mutex<Vec<(u64, Arc<[u8]>)>> = Mutex::new(Vec::new());
fn texture_key(key: u64) -> String { format!("tex-{key:016x}") }

impl makepad_render::material_surface::PreparedTextureStore for TextureStore {
    // Under its own key (the key already carries the encoder version).
    fn get_basis(&self, key: u64) -> Option<Vec<u8>> {
        let name = texture_key(key);
        if let Some(bytes) = memory_get(&name) { return Some(bytes.to_vec()); }
        let started = Instant::now();
        // A failed read is not a miss, though both end in encoding the image here.
        let fetched = store_get_role(&session()?, &alias(&name), FileRole::Texture).unwrap_or_else(|error| {
            makepad_platform::log!("prepared texture {name}: the store read failed, so it is encoded here: {error}");
            None
        });
        BASIS_FETCH[0].fetch_add(started.elapsed().as_micros() as usize, std::sync::atomic::Ordering::Relaxed);
        BASIS_FETCH[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bytes = fetched?;
        memory_put(&name, Arc::from(bytes.as_slice()));
        Some(bytes)
    }
    fn put_basis(&self, key: u64, ktx2: &[u8]) {
        let bytes: Arc<[u8]> = Arc::from(ktx2);
        memory_put(&texture_key(key), bytes.clone());
        if session().is_some() { TEXTURE_SAVES.lock().unwrap_or_else(|e| e.into_inner()).push((key, bytes)); }
    }
}

pub(super) fn take_texture_saves() -> Vec<(u64, Arc<[u8]>)> { std::mem::take(&mut *TEXTURE_SAVES.lock().unwrap_or_else(|e| e.into_inner())) }

pub(super) fn save_texture(key: u64, bytes: &[u8]) {
    let Some(session) = session() else { return };
    let name = texture_key(key);
    let description = format!("Basis/UASTC texture (encoder {}) cached by source image hash.", makepad_render::material_surface::BASIS_ENCODER_VERSION);
    if let Err(error) = derived_put(&session, &name, &format!("prepared texture {name}"), &description, bytes) {
        makepad_platform::log!("prepared texture {name} not cached: {error}");
    }
}

/// Derived bytes a build or bake produces once (a navigation graph, a
/// prepared texture), stored under `gen/built/<key>-l1` as one opaque file
/// and read back through the same client pool as built products. `key` is
/// the content key (it should carry the producer's version). Publishing a
/// key that already exists is success: the bytes are the same derivation.
pub(crate) fn derived_put(session: &StoreSession, key: &str, title: &str, description: &str, bytes: &[u8]) -> Result<(), String> {
    let alias = AssetAlias::new(alias(key)).map_err(|e| e.to_string())?;
    // (The store has no generic data kind; an opaque texture-role file is
    // what its content contract accepts for bytes it does not interpret.)
    let mut bundle = PublishBundle::new("gen", AssetKind::Texture, title.to_string(),
        vec![PublishBundleFile::bytes(FileRole::Texture, MediaType::Bin, bytes.to_vec(), None)],
        PublishThumbnail::plain(thumbnail()?, ThumbnailMedia::Png, 256, 256), makepad_asset_client::PublishRights::generated_cc0());
    bundle.alias = Some(alias);
    bundle.asset_id = Some(mint_asset_id(key));
    bundle.expected_head = PublishExpectedHead::Absent;
    bundle.generator = "makepad-sandbox".into();
    bundle.tags = vec!["build-cache".into()];
    bundle.description = description.to_string();
    with_client(session, |client| match client.publish_bundle_with(&bundle, None, &|| false) {
        Ok(_) | Err(ClientError::Server { status: 409, .. }) => Ok(()),
        Err(error) => Err(format!("{error}")),
    })
}


fn thumbnail() -> Result<Vec<u8>, String> {
    // One neutral card for every cache entry (content-addressed: stored once).
    static PNG: std::sync::OnceLock<Result<Vec<u8>, String>> = std::sync::OnceLock::new();
    PNG.get_or_init(|| makepad_asset_importer::png::encode_png_rgba(&[0x3a, 0x3f, 0x46, 0xff].repeat(256 * 256), 256, 256).map_err(|e| e.to_string())).clone()
}

fn mint_asset_id(hash: &str) -> AssetId {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write(hash.as_bytes());
        h.write_usize(i);
        h.write_u128(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    AssetId::from_bytes(bytes)
}

// ── same-machine build locks ────────────────────────────────────────────

fn lock_path(hash: &str) -> std::path::PathBuf {
    host().lock_dir().join(format!("{}.building", alias(hash).trim_start_matches(STORE_CACHE_PREFIX)))
}

/// Held while this process builds and publishes `hash`: the file
/// `<hash>.building` in the host's lock folder, with an exclusive OS lock on
/// it. The OS lets go of that lock when the process ends, cleanly or not, so
/// a build a quit cut short (a follow-up still running at exit, which runs no
/// destructor) never holds up the next launch: its file is taken over at once.
pub(super) struct BuildLock { path: std::path::PathBuf, file: Option<std::fs::File> }
impl BuildLock {
    /// `Err(())` while another live build holds the lock, in this process or another.
    pub(super) fn acquire(hash: &str) -> Result<BuildLock, ()> {
        let path = lock_path(hash);
        if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
        let opened = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(&path);
        let error = match opened.map(|file| (file.try_lock(), file)) {
            Ok((Ok(()), file)) => return Ok(BuildLock { path, file: Some(file) }),
            Ok((Err(std::fs::TryLockError::WouldBlock), _)) => return Err(()),
            Ok((Err(std::fs::TryLockError::Error(error)), _)) | Err(error) => error,
        };
        // A folder that takes no file or no lock leaves builds unguarded: two
        // instances may build one program twice, which costs time, never a
        // wrong product.
        static UNGUARDED: std::sync::Once = std::sync::Once::new();
        UNGUARDED.call_once(|| makepad_platform::log!("model build locks unavailable at {}: {error}; builds run unguarded", path.display()));
        Ok(BuildLock { path, file: None })
    }
}
/// The file goes while it is still locked, so a waiter that finds it gone
/// looks in the store again. (A waiter that opened it just before can take
/// the lock of the removed file and build once more: time, not a wrong product.)
impl Drop for BuildLock { fn drop(&mut self) { if self.file.is_some() { let _ = std::fs::remove_file(&self.path); } } }

/// True while a live build holds the lock at `path`. The test takes the lock
/// for a moment when it is free; an `acquire` meeting that waits one round more.
fn held_by_peer(path: &std::path::Path) -> bool {
    std::fs::File::open(path).is_ok_and(|file| matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
}

/// Wait while another live build holds `hash`'s lock; true when it let go
/// (it finished, or its process ended: look in the store again), false when
/// cancelled.
pub(super) fn wait_for_peer(hash: &str, cancelled: &dyn Fn() -> bool) -> bool {
    let path = lock_path(hash);
    // A wait for another build: without this job's CPU permit.
    makepad_platform::thread::without_permit(|| {
        while held_by_peer(&path) {
            if cancelled() { return false; }
            std::thread::sleep(Duration::from_millis(250));
        }
        true
    })
}
