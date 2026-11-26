//! Stage's host of the model build service (KERNELS.md §3.10, PM2).
//!
//! A Scene3D document calls the same builders a Sandbox game does
//! (`model_new`, `character_new`, the shared kits): the builder records the
//! program and posts it to `makepad-model-script`'s mailbox. [`pump`], called
//! on the UI thread by whoever draws scenes, starts `makepad-model-build` on
//! first use with this app's host, drains the mailbox and installs finished
//! products here; a document reads them by alias with [`product`].
//!
//! Builds are cached in **Stage's own store** (`gen/built/<hash>-l3` in the
//! store at `STAGE_ASSET_ROOT`, else Stage's library store): a program built
//! once in Stage is fetched from there by every later run, export part or
//! instance, never rebuilt. Sandbox has its own store root; a product
//! reaches the other app only by an explicit import or publish.
//!
//! Only channel polls, the registry and this table are touched on the UI
//! thread; compiles, store lookups and saves run on the pool.

use makepad_model_build::{BuildHost, StoreSession};
use makepad_model_script::model_verbs::{self as verbs, ScriptModelRequest, ScriptModelState};
use makepad_platform::thread::{Class, TaskPool};
use makepad_platform::{log, Cx};
use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

pub use makepad_model_script::model_verbs::is_script_model;

/// Where a built model is.
#[derive(Clone, Debug)]
pub enum Built {
    /// Installed: the revision `hash` the latest evaluation asked for.
    Ready { hash: String, glb: Arc<[u8]> },
    /// Posted, not installed yet.
    Building,
    Failed(String),
    /// No evaluation asked for this alias (not a built model).
    Unknown,
}

/// Installed products by alias: (revision, GLB). Touched by [`pump`] and
/// by document loads, both on the UI thread (tools and tests that load
/// without drawing never contend).
static INSTALLED: Mutex<Option<HashMap<String, (String, Arc<[u8]>)>>> = Mutex::new(None);
/// Bumped whenever a product installs or a build fails: documents that
/// waited reload.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn installed<R>(f: impl FnOnce(&mut HashMap<String, (String, Arc<[u8]>)>) -> R) -> R {
    f(INSTALLED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

/// The built model `alias` as the latest evaluation asked for it.
pub fn product(alias: &str) -> Built {
    match verbs::script_model_state(alias) {
        ScriptModelState::Unknown => Built::Unknown,
        ScriptModelState::Failed(error) => Built::Failed(error),
        // Not built on this device (a downloaded realm over its memory).
        ScriptModelState::Withheld(reason) => Built::Failed(reason),
        ScriptModelState::Building => Built::Building,
        ScriptModelState::Ready => {
            let want = verbs::script_model_wanted(alias);
            match installed(|t| t.get(alias).cloned()) {
                Some((hash, glb)) if Some(&hash) == want.as_ref() => Built::Ready { hash, glb },
                _ => Built::Building,
            }
        }
    }
}

/// Changes whenever a product installs or a build fails.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Models posted and not installed or failed yet.
pub fn building() -> bool {
    !verbs::pending_script_models().is_empty()
}

/// (done, total, queued, the model compiling now) of the current batch.
pub fn progress() -> (usize, usize, usize, Option<String>) {
    verbs::script_build_progress()
}

/// A line for a progress card: "building models 1 / 3: stage-rio".
pub fn progress_text() -> Option<String> {
    let (done, total, _, current) = progress();
    (total > 0).then(|| match current {
        Some(name) => format!("building models {done} / {total}: {name}"),
        None => format!("building models {done} / {total}"),
    })
}

// ── the store ───────────────────────────────────────────────────────────

/// Stage's store root: `STAGE_ASSET_ROOT`, else Stage's library store.
pub fn store_root() -> PathBuf {
    makepad_asset_client::paths::app_store_root(makepad_asset_client::paths::STAGE_APP)
}

/// The session of the server serving Stage's store, once found. Set once
/// by a pool probe (the UI thread only reads it).
static SESSION: OnceLock<StoreSession> = OnceLock::new();

/// Reads the root's `listen`, `server-id` and `admin-token` and asks the
/// server's health (blocking, up to 0.4 s: a pool job).
fn probe_store(root: &std::path::Path) -> Option<StoreSession> {
    use std::io::{Read, Write};
    let listen = std::fs::read_to_string(root.join("listen")).ok()?;
    let mut parts = listen.lines().next()?.trim().rsplitn(3, ':');
    let data: u16 = parts.next()?.trim().parse().ok()?;
    let control: u16 = parts.next()?.trim().parse().ok()?;
    let host = parts.next()?.trim();
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    let ip: std::net::IpAddr = host.parse().ok()?;
    let ip = if ip.is_unspecified() { std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) } else { ip };
    let id_text = std::fs::read_to_string(root.join("server-id")).ok()?;
    let id_text = id_text.trim();
    if id_text.len() != 32 {
        return None;
    }
    let mut server_id = [0u8; 16];
    for (i, byte) in server_id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&id_text[i * 2..i * 2 + 2], 16).ok()?;
    }
    let control = std::net::SocketAddr::new(ip, control);
    let mut stream = std::net::TcpStream::connect_timeout(&control, std::time::Duration::from_millis(400)).ok()?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(400)));
    stream.write_all(format!("GET /v1/health HTTP/1.1\r\nHost: {control}\r\nConnection: close\r\n\r\n").as_bytes()).ok()?;
    let mut head = [0u8; 16];
    let mut got = 0;
    while got < head.len() {
        match stream.read(&mut head[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    if !head[..got].starts_with(b"HTTP/1.1 200") {
        return None;
    }
    let token = std::fs::read_to_string(root.join("admin-token")).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    Some(StoreSession { endpoints: makepad_asset_client::ApiEndpoints { control, data: std::net::SocketAddr::new(ip, data) }, token, server_id })
}

/// The `m.import` fetch: a store alias's model file.
fn fetch_alias(session: &StoreSession, name: &str) -> Result<Vec<u8>, String> {
    use makepad_asset_data::{AssetAlias, FileRole, MediaType};
    let alias = AssetAlias::new(name.to_string()).map_err(|e| format!("m.import(\"{name}\"): {e}"))?;
    makepad_model_build::with_client(session, |client| {
        let head = client.resolve_alias(&alias).map_err(|e| format!("m.import(\"{name}\"): {e}"))?.head_revision;
        let manifest = client.fetch_asset_manifest(&head).map_err(|e| format!("m.import(\"{name}\"): {e}"))?;
        let file = manifest
            .files
            .iter()
            .filter(|f| f.role == FileRole::RenderGlb || f.media == MediaType::Glb)
            .min_by_key(|f| (f.role != FileRole::RenderGlb, f.lod))
            .ok_or_else(|| format!("m.import(\"{name}\"): the asset has no model file"))?;
        client.fetch_blob_bytes(&file.blob, Some(file.byte_len)).map(|b| b.to_vec()).map_err(|e| format!("m.import(\"{name}\"): {e}"))
    })
}

struct StageBuildHost {
    root: PathBuf,
}

impl BuildHost for StageBuildHost {
    fn store(&self) -> (Option<StoreSession>, Option<String>) {
        (SESSION.get().cloned(), None)
    }
    fn bundled(&self, _alias: &str) -> Option<&'static [u8]> {
        None
    }
    fn fetcher(&self, session: Option<StoreSession>, _variant: Option<String>) -> Result<Box<dyn FnMut(&str) -> Result<Vec<u8>, String>>, String> {
        let session = session.ok_or("m.import needs Stage's asset store, which is not running")?;
        Ok(Box::new(move |alias: &str| fetch_alias(&session, alias)))
    }
    fn prepare(&self, _request: &ScriptModelRequest, _glb: &Arc<[u8]>) -> Result<Box<dyn Any + Send>, String> {
        // A document parses the GLB itself when it loads.
        Ok(Box::new(()))
    }
    fn wants_review(&self) -> bool {
        false
    }
    fn review(&self, _document: &makepad_model::Document, _alias: &str, _cancelled: &dyn Fn() -> bool) -> Result<Box<dyn Any + Send>, String> {
        Err("Stage renders no model reviews".into())
    }
    fn after_build(&self, _request: &ScriptModelRequest, _session: Option<StoreSession>) -> Option<Box<dyn FnOnce(Arc<[u8]>) + Send>> {
        None
    }
    fn memory_budget(&self) -> usize {
        256 << 20
    }
    fn client_root(&self) -> PathBuf {
        self.root.with_file_name("stage-model-cache-0")
    }
    fn lock_dir(&self) -> PathBuf {
        self.root.with_file_name("stage-model-build-locks")
    }
}

// ── the pump ────────────────────────────────────────────────────────────

struct Service {
    pool: TaskPool,
    root: PathBuf,
    /// When the store was last looked for (every few seconds until found).
    probed: Option<Instant>,
    /// Posted models not installed or failed at the last pump.
    pending: std::collections::HashSet<String>,
}

thread_local! {
    static SERVICE: std::cell::RefCell<Option<Service>> = const { std::cell::RefCell::new(None) };
}

/// Start the service (first call), drain the builder's mailbox, install what
/// finished. Call on the UI thread from every draw or event of a host that
/// shows scenes; cheap when nothing is posted. True when a product
/// installed or a build failed (documents waiting for it reload).
pub fn pump(cx: &mut Cx) -> bool {
    SERVICE.with(|cell| {
        let mut service = cell.borrow_mut();
        if service.is_none() {
            if verbs::pending_script_models().is_empty() {
                return false;
            }
            let root = store_root();
            let pool = cx.task_pool();
            makepad_model_build::set_host(Arc::new(StageBuildHost { root: root.clone() }));
            makepad_model_build::initialize(pool.clone());
            *service = Some(Service { pool, root, probed: None, pending: Default::default() });
        }
        let service = service.as_mut().unwrap();
        if SESSION.get().is_none() && service.probed.is_none_or(|t| t.elapsed().as_secs_f64() > 3.0) {
            service.probed = Some(Instant::now());
            let root = service.root.clone();
            {
                let _ = service.pool.submit(Class::Visible, move || {
                    if let Some(session) = probe_store(&root) {
                        if SESSION.set(session.clone()).is_ok() {
                            log!("stage models: building into the store at {}", root.display());
                            makepad_model_build::prewarm_store(session);
                        }
                    }
                }).map(|task| task.detach());
            }
        }
        let (_, finished) = makepad_model_build::poll();
        let mut changed = false;
        for done in finished {
            let request = &done.request;
            log!(
                "stage models: {} {} ({} KiB, {} ms; {})",
                request.alias,
                if done.product.cached { "from the store" } else { "built" },
                done.product.glb.len() >> 10,
                done.started.elapsed().as_millis(),
                done.product.timing
            );
            for warning in &done.product.warnings {
                log!("stage models: {}: {warning}", request.alias);
            }
            installed(|t| t.insert(request.alias.clone(), (request.hash.clone(), done.product.glb.clone())));
            verbs::mark_script_model_installed(&request.alias, &request.hash);
            changed = true;
        }
        // A model that stopped pending without installing here failed.
        let pending = verbs::pending_script_models();
        if service.pending.iter().any(|alias| !pending.contains(alias)) {
            changed = true;
        }
        service.pending = pending;
        if changed {
            GENERATION.fetch_add(1, Ordering::AcqRel);
        }
        changed
    })
}

/// Whether this process has started the service (tests, reports).
pub fn started() -> bool {
    SERVICE.with(|cell| cell.borrow().is_some())
}
