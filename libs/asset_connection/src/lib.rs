//! The product windows' asset library (Stage's and Story's: the one
//! `stage` store): where it is, the verified client every worker shares,
//! and [`host`], the in-process host of that store.
//!
//! Asset client setup for the app worker. Never call `connect` from the UI thread.

pub mod host;

use std::env::{self, VarError};
use std::fs;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use makepad_widgets::makepad_platform::thread::without_permit;
use makepad_asset_client::http::HttpLimits;
use makepad_asset_client::{paths, ApiEndpoints, AssetClient, ClientConfig};

const UNAVAILABLE: &str = "Asset server is unavailable; start it and Refresh";
/// While this Stage is still deciding whether it hosts its own store.
const STARTING: &str = "Stage's asset store is starting";

/// Where this Stage's store is, once its asset host has decided: the
/// hosted store's own endpoints (the address of record while Stage hosts,
/// never a `listen` file left by an earlier run), or the root of the store
/// it attached to. Set from the UI thread (`publish_resolution`): once at
/// start, and again when this Stage takes its store over from a host that
/// quit. Workers only read it.
#[derive(Clone)]
pub struct Resolution {
    pub hosted: Option<(ApiEndpoints, [u8; 16], String)>,
    pub root: PathBuf,
}

static RESOLUTION: std::sync::RwLock<Option<Resolution>> = std::sync::RwLock::new(None);

fn resolution() -> Option<Resolution> {
    RESOLUTION.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// Record the asset host's decision for every worker's next connect. A
/// worker verified against an earlier decision's server reconnects: its
/// origin key no longer matches.
pub fn publish_resolution(resolution: Resolution) {
    *RESOLUTION.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(resolution);
}

/// Whether the asset host has decided yet. Before it has, a `listen` file
/// may name a server from an earlier run.
pub fn resolved() -> bool {
    RESOLUTION.read().unwrap_or_else(std::sync::PoisonError::into_inner).is_some()
}

/// This process's asset hosts making their first decision: from
/// [`host::AssetHost::start`] until the decision is published, or its
/// thread ends without one. A worker's [`connect`] waits it out: the store is
/// starting, not failing.
static DECIDING: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
static DECIDED: std::sync::Condvar = std::sync::Condvar::new();

/// A first decision in the making, while this lives.
pub(crate) struct Deciding(());

impl Deciding {
    pub(crate) fn start() -> Self {
        *DECIDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        Deciding(())
    }
}

impl Drop for Deciding {
    fn drop(&mut self) {
        *DECIDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
        DECIDED.notify_all();
    }
}

/// Once no asset host of this process is making its first decision (at
/// once when none is).
fn wait_for_decision() {
    if *DECIDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner) == 0 {
        return;
    }
    without_permit(|| {
        let deciding = DECIDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(DECIDED.wait_while(deciding, |count| *count > 0).unwrap_or_else(std::sync::PoisonError::into_inner));
    });
}

fn hosted_here() -> bool {
    resolution().is_some_and(|resolution| resolution.hosted.is_some())
}

/// The one cache root both media workers share. The asset client takes a
/// single-owner lock on it, so exactly one verified client owns it and the
/// workers use lane clones of that owner: one published blob is one file,
/// whichever worker fetched it. The former per-lane `audio` and `video`
/// leaves are left on disk unused.
const SHARED_CACHE: &str = "shared";

/// Stages running at once on one Makepad home: each owns the first cache
/// root (`shared`, `shared-1`, ...) that no other process holds.
#[cfg(not(target_arch = "wasm32"))]
const CACHE_SLOTS: usize = 8;

/// The media workers' shared asset origin: the one verified client that owns
/// the cache root, with the exact configuration it was verified with. Held
/// by each media worker for its whole life ([`origin`]); a static keeps only
/// a `Weak`, so when the last worker ends the owner, its cache and its root
/// lock drop on that worker.
pub struct AssetOrigin {
    #[cfg(not(target_arch = "wasm32"))]
    root: std::sync::Mutex<OriginRoot>,
    #[cfg(not(target_arch = "wasm32"))]
    verified: std::sync::Condvar,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
enum OriginRoot {
    #[default]
    Empty,
    Verifying,
    Ready(OriginKey, AssetClient),
}

/// The sole verifier publishes its result and wakes waiters, also on error or unwind.
#[cfg(not(target_arch = "wasm32"))]
struct Verifying<'a> {
    origin: &'a AssetOrigin,
    result: OriginRoot,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Verifying<'_> {
    fn drop(&mut self) {
        *self.origin.root.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = std::mem::take(&mut self.result);
        self.origin.verified.notify_all();
    }
}

/// Everything a verified owner was verified with, compared exactly, except
/// its cache root, which is whichever slot was free. Never printed: it holds
/// the credential.
#[cfg(not(target_arch = "wasm32"))]
#[derive(PartialEq, Eq)]
struct OriginKey {
    control: SocketAddr,
    data: SocketAddr,
    server_id: [u8; 16],
    token: Option<String>,
}

static ORIGIN: std::sync::Mutex<std::sync::Weak<AssetOrigin>> = std::sync::Mutex::new(std::sync::Weak::new());

/// The process's shared origin, created by the first media worker that asks
/// and kept alive only by the workers that hold it. Media worker body only.
pub fn origin() -> std::sync::Arc<AssetOrigin> {
    let mut held = ORIGIN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(origin) = held.upgrade() {
        return origin;
    }
    let origin = std::sync::Arc::new(AssetOrigin {
        #[cfg(not(target_arch = "wasm32"))]
        root: Default::default(),
        #[cfg(not(target_arch = "wasm32"))]
        verified: Default::default(),
    });
    *held = std::sync::Arc::downgrade(&origin);
    origin
}

/// A client on the configured asset server and the shared cache root, for
/// one media worker. Runs on that worker only; it may block it for one
/// verified connect, within the client's own network limits, and first, while
/// this process's asset host makes its first decision, for that decision
/// (made on a thread of its own, never waiting for the UI).
///
/// With the configuration the owner was verified with (cache root,
/// endpoints, expected server id, credential), it returns a lane clone of the
/// owner: no network, no second check, the client's documented `lane_clone`
/// contract. The clone shares the owner's cache (bookkeeping mutex), RAM
/// cache and per-digest transfer gate, and its API endpoints, limits and
/// credential. Otherwise the old owner is dropped first and a new client is
/// genuinely connected and verified; while the other worker still holds a
/// clone of the old cache, the root lock refuses that honestly (`CacheBusy`).
pub fn connect(origin: &AssetOrigin) -> Result<AssetClient, String> {
    wait_for_decision();
    let (ApiEndpoints { control, data }, server_id, token) = store_endpoints()?;
    let mut config = ClientConfig::new(
        makepad_ai_hub::home::makepad_home().join("stage-product").join("asset-cache").join(SHARED_CACHE),
    );
    config.token = Some(token);
    // Stage's store is a video library (blobs up to STAGE_MAX_BLOB_BYTES,
    // see `paths::app_max_blob_bytes`): the media cache admits what it
    // serves. Fetches stream to disk, so this sizes disk, not memory; the
    // total stays an LRU bound.
    config.cache.max_object_bytes = paths::STAGE_MAX_BLOB_BYTES;
    config.cache.max_partial_bytes = paths::STAGE_MAX_BLOB_BYTES;
    config.cache.max_total_bytes = config.cache.max_total_bytes.max(2 * paths::STAGE_MAX_BLOB_BYTES);
    config.http = HttpLimits {
        connect_timeout_ms: 2_000,
        read_timeout_ms: 2_000,
        write_timeout_ms: 2_000,
        head_probe_ms: 5_000,
        body_deadline_ms: 10_000,
    };
    // A stalled transfer is caught by the 2 s read timeout; the whole-body
    // deadline only bounds a slow one, so it follows the largest blob at
    // 10 MB/s (about 29 min for 16 GiB) rather than failing long videos.
    config.blob_body_deadline_ms = 30_000u64.max(paths::STAGE_MAX_BLOB_BYTES / 10_000);
    config.max_transfer_attempts = 2;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let key = OriginKey { control, data, server_id, token: config.token.clone() };
        let mut verification = loop {
            let mut root = origin.root.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match &*root {
                OriginRoot::Ready(verified, owner) if *verified == key => return Ok(owner.lane_clone()),
                OriginRoot::Verifying => {
                    drop(root);
                    without_permit(|| {
                        let root = origin.root.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        drop(origin.verified.wait_while(root, |r| matches!(r, OriginRoot::Verifying))
                            .unwrap_or_else(std::sync::PoisonError::into_inner));
                    });
                }
                _ => {
                    let previous = std::mem::replace(&mut *root, OriginRoot::Verifying);
                    drop(root);
                    let verification = Verifying { origin, result: OriginRoot::Empty };
                    // Release the previous cache root before opening another.
                    drop(previous);
                    break verification;
                }
            }
        };
        let mut slot = 0;
        let owner = loop {
            let mut attempt = config.clone();
            if slot > 0 {
                attempt.cache_root = config.cache_root.with_file_name(format!("{SHARED_CACHE}-{slot}"));
            }
            match AssetClient::connect(attempt, ApiEndpoints { control, data }, Some(server_id)) {
                // Another Stage on this home owns that root: take the next.
                Err(makepad_asset_client::ClientError::CacheBusy) if slot + 1 < CACHE_SLOTS => slot += 1,
                result => break result.map_err(|error| describe(error, control))?,
            }
        };
        let lane = owner.lane_clone();
        verification.result = OriginRoot::Ready(key, owner);
        Ok(lane)
    }
    // The portable client refuses a native session on its own.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = origin;
        AssetClient::connect(config, ApiEndpoints { control, data }, Some(server_id)).map_err(|error| describe(error, control))
    }
}

/// The one way a worker holds its store client: `client`, reconnected
/// first when the store has moved since it was made (this Stage took the
/// store over from a host that quit, or attached to the Stage that did), or
/// made when there is none. Every cached client goes through here, so a
/// takeover is invisible: the next job after it simply talks to the new
/// server. Checking is a read of the current decision (and, while attached,
/// of the store's few small files); no network.
pub fn connected<'a>(client: &'a mut Option<AssetClient>, origin: &AssetOrigin) -> Result<&'a mut AssetClient, String> {
    if client.as_ref().is_some_and(|client| !is_current(client.endpoints())) {
        *client = None;
    }
    if client.is_none() {
        *client = Some(connect(origin)?);
    }
    Ok(client.as_mut().unwrap())
}

/// Whether something made for `endpoints` still talks to this Stage's
/// store. When the store's location cannot be read right now it is not
/// called stale: the request itself says what is wrong.
pub fn is_current(endpoints: ApiEndpoints) -> bool {
    store_endpoints().map_or(true, |(now, _, _)| now == endpoints)
}

/// Where Stage's own store answers, which server it must be, and its
/// credential: the `STAGE_*` (then `VJ_*`) settings, else the files of
/// Stage's library root. Reads a few small files; call it off the UI thread.
/// Shared by the media workers' client and the FLOW studio's publisher.
pub fn store_endpoints() -> Result<(ApiEndpoints, [u8; 16], String), String> {
    let pinned = env_pair("STAGE_ASSET_SERVER", "VJ_ASSET_SERVER")?.is_some();
    let resolution = resolution();
    // Hosted here: the host's own sockets, never a stale `listen`.
    if let Some((endpoints, server_id, token)) = resolution.as_ref().and_then(|resolution| resolution.hosted.clone()).filter(|_| !pinned) {
        return Ok((endpoints, server_id, token));
    }
    let root = match env_pair("STAGE_ASSET_ROOT", "VJ_ASSET_ROOT")? {
        Some(path) => PathBuf::from(path),
        None => match &resolution {
            Some(resolution) => resolution.root.clone(),
            // Stage's own library, never whatever server answers.
            None => paths::app_store_root(paths::STAGE_APP),
        },
    };
    let missing = if pinned || resolved() { UNAVAILABLE } else { STARTING };
    let listen = pick("STAGE_ASSET_SERVER", "VJ_ASSET_SERVER", &root.join("listen"), true)?
        .ok_or_else(|| missing.to_string())?;
    let (control, data) = parse_listen(&listen)?;
    let server_id = parse_server_id(
        &pick("STAGE_ASSET_SERVER_ID", "VJ_ASSET_SERVER_ID", &root.join("server-id"), true)?
            .ok_or_else(|| "asset server id is not configured".to_string())?,
    )?;
    Ok((ApiEndpoints { control, data }, server_id, token(&root)?))
}

fn describe(error: makepad_asset_client::ClientError, control: SocketAddr) -> String {
    match error {
        // The store names a server that is not up: its `listen` outlives it.
        makepad_asset_client::ClientError::Io { kind: ErrorKind::ConnectionRefused, .. } if hosted_here() => {
            format!("Stage's own asset store does not answer at {control}")
        }
        makepad_asset_client::ClientError::Io { kind: ErrorKind::ConnectionRefused, .. } if !resolved() => {
            format!("{STARTING} (nothing answers yet at {control})")
        }
        makepad_asset_client::ClientError::Io { kind: ErrorKind::ConnectionRefused, .. } => {
            format!("Asset server is not running (nothing answers at {control}); start it and Refresh")
        }
        makepad_asset_client::ClientError::CacheBusy => {
            "every asset cache on this Makepad home is in use (by the other media worker until it reconnects, or by other Stages); Refresh again".into()
        }
        error => format!("asset server: {error}"),
    }
}

fn token(root: &Path) -> Result<String, String> {
    if let Some(token) = env_pair("STAGE_ASSET_TOKEN", "VJ_ASSET_TOKEN")? {
        return Ok(token);
    }
    if let Some(path) = env_pair("STAGE_ASSET_ADMIN_TOKEN_FILE", "VJ_ASSET_ADMIN_TOKEN_FILE")? {
        let text = fs::read_to_string(&path).map_err(|err| format!("asset token file: {err}"))?;
        let text = text.trim();
        if text.is_empty() {
            return Err("asset token file is empty".into());
        }
        return Ok(text.to_string());
    }
    store_text(&root.join("admin-token"), false)?
        .ok_or_else(|| "asset server admin token is not configured".to_string())
}

/// `STAGE_*`, then the `VJ_*` alias, then the store file. A set-but-invalid env is an error.
fn pick(primary: &str, legacy: &str, file: &Path, first_line: bool) -> Result<Option<String>, String> {
    if let Some(value) = env_pair(primary, legacy)? {
        return Ok(Some(value));
    }
    store_text(file, first_line)
}

fn env_pair(primary: &str, legacy: &str) -> Result<Option<String>, String> {
    match env_one(primary)? {
        Some(value) => Ok(Some(value)),
        None => env_one(legacy),
    }
}

fn env_one(key: &str) -> Result<Option<String>, String> {
    match env::var(key) {
        Ok(value) if value.trim().is_empty() => Err(format!("{key} is set but empty")),
        Ok(value) => Ok(Some(value.trim().to_string())),
        Err(VarError::NotPresent) => Ok(None),
        Err(_) => Err(format!("{key} is not valid UTF-8")),
    }
}

fn store_text(path: &Path, first_line: bool) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let text = if first_line { text.lines().next().unwrap_or("") } else { &text };
            let text = text.trim();
            Ok((!text.is_empty()).then(|| text.to_string()))
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("reading asset store: {err}")),
    }
}

fn parse_listen(line: &str) -> Result<(SocketAddr, SocketAddr), String> {
    let mut parts = line.trim().rsplitn(3, ':');
    let data = parts.next().unwrap_or("");
    let control = parts.next().unwrap_or("");
    let host = parts.next().unwrap_or("").trim();
    let host = host.strip_prefix('[').and_then(|host| host.strip_suffix(']')).unwrap_or(host);
    if host.is_empty() {
        return Err("invalid asset server listen; expected ip:control:data".into());
    }
    let ip: IpAddr = host.parse().map_err(|_| "asset server listen host must be an IP address".to_string())?;
    Ok((SocketAddr::new(ip, port(control)?), SocketAddr::new(ip, port(data)?)))
}

fn port(text: &str) -> Result<u16, String> {
    let port: u16 = text.trim().parse().map_err(|_| "asset server listen port is invalid".to_string())?;
    if port == 0 { Err("asset server listen port must be non-zero".into()) } else { Ok(port) }
}

fn parse_server_id(text: &str) -> Result<[u8; 16], String> {
    let text = text.trim();
    if text.len() != 32 || !text.is_ascii() {
        return Err("asset server id must be 32 hex characters".into());
    }
    let mut id = [0u8; 16];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| "asset server id must be 32 hex characters".to_string())?;
    }
    Ok(id)
}

/// Two processes, as the user met it: another process hosts Stage's store,
/// this Stage is attached and every kind of store client it keeps has
/// talked to it; the host is killed, this Stage takes the store over, and
/// every one of those same clients succeeds against the new host without
/// being made again by hand.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use makepad_app_asset_server::embed;
    use makepad_asset_client::api::CatalogQuery;
    use makepad_asset_client::publish::{PublishFile, PublishRequest, PublishThumbnail};
    use makepad_asset_data::{AssetKind, FileRole, MediaType, ThumbnailMedia};
    use makepad_flow_assets::{AssetListQuery, AssetStoreConfig, AssetWorker, StoreLocator};
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// An app prefix whose library folder does not exist: no publisher.
    const PREFIX: &str = "STTAKEOVER";

    fn child(test: &str, root: &Path, home: Option<&Path>) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([test, "--exact", "--nocapture", "--test-threads=1"]).env("ST_TAKEOVER_ROOT", root);
        if let Some(home) = home {
            command.env("MAKEPAD_HOME", home);
        }
        command
    }

    /// Killed on every exit from the test, a failed assert included.
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn takeover_host_child() {
        let Some(root) = std::env::var_os("ST_TAKEOVER_ROOT") else { return };
        let resolved = embed::resolve_at(PREFIX, Path::new(&root), false, None);
        assert_eq!(resolved.mode, embed::StoreMode::Hosting, "{}", resolved.note);
        std::thread::sleep(Duration::from_secs(60));
        drop(resolved);
    }

    /// The Stage half: attached, every client kind used, then the takeover.
    #[test]
    fn takeover_stage_child() {
        let Some(root) = std::env::var_os("ST_TAKEOVER_ROOT").map(PathBuf::from) else { return };
        publish_resolution(Resolution { hosted: None, root: root.clone() });
        let origin = origin();

        // The workers' cached clients (media, music bake, video assets, the
        // cut store and media, generation, storyboard, studio API, flow
        // studio all hold one through `connected`).
        let mut worker: Option<AssetClient> = None;
        let client = connected(&mut worker, &origin).expect("attached client");
        let png = makepad_ai_hub::testpattern::encode_png_rgba(&vec![128u8; 256 * 256 * 4], 256, 256).unwrap();
        let source = b"Edit{duration: 1.0}\n".to_vec();
        let file = PublishFile { bytes: source.clone(), media: MediaType::Text, role: FileRole::Source, media_millis: 0, dims: None };
        client
            .publish_artifact(&PublishRequest::new("stage", AssetKind::Cut, "takeover", file, PublishThumbnail::plain(png, ThumbnailMedia::Png, 256, 256)))
            .expect("publish a cut on the first host");
        // The flow studio's publish worker.
        let mut config = AssetStoreConfig::new(root.parent().unwrap().join("flow-cache"));
        config.discovery_wait_ms = 0;
        config.locate = Some(StoreLocator(std::sync::Arc::new(store_endpoints)));
        let flow = AssetWorker::start(config).unwrap();
        let list = || flow.handle().list(AssetListQuery { text: String::new(), namespace: Some("stage".into()), limit: 10, cursor: None });
        assert_eq!(list().expect("flow worker on the first host").len(), 1);
        let first = store_endpoints().unwrap().0;

        // Stage's own watch (AssetHost), then tell the parent to kill the host.
        let watch = embed::resolve_when_freed_notify(PREFIX, &root, || {});
        println!("ATTACHED");
        let took = watch.wait().unwrap();
        assert_eq!(took.mode, embed::StoreMode::Hosting, "{}", took.note);
        let local = took.local.unwrap();
        publish_resolution(Resolution { hosted: Some((local.endpoints(), local.server_id(), local.token().to_string())), root: root.clone() });
        assert_ne!(store_endpoints().unwrap().0, first, "the store moved");

        // The SAME clients, no hand-made reconnect.
        let client = connected(&mut worker, &origin).expect("the worker client follows");
        client.catalog_search(&CatalogQuery::browse(10), None).expect("catalog on the new host");
        assert_eq!(list().expect("flow worker on the new host").len(), 1);
        // A client made fresh (score, generate) goes to the new host too.
        connect(&origin).unwrap().catalog_search(&CatalogQuery::browse(10), None).expect("fresh client");
        drop(flow);
        println!("FOLLOWED");
        drop(local);
    }

    #[test]
    fn every_store_client_follows_a_takeover() {
        let base = std::env::temp_dir().join(format!("stage-takeover-{}-{}", std::process::id(), makepad_asset_client::util::now_ms()));
        let root = base.join("store");
        std::fs::create_dir_all(base.join("home")).unwrap();
        let mut host = Kill(child("tests::takeover_host_child", &root, None).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(30);
        while embed::store_served(&root, None) != Some("its server answers") {
            assert!(Instant::now() < deadline, "the host never came up");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut stage = Kill(child("tests::takeover_stage_child", &root, Some(&base.join("home")))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap());
        let mut lines = BufReader::new(stage.0.stdout.take().unwrap()).lines();
        let mut saw = |want: &str| loop {
            match lines.next() {
                Some(Ok(line)) if line.contains(want) => return,
                Some(Ok(_)) => continue,
                other => panic!("the Stage process ended before {want}: {other:?}"),
            }
        };
        saw("ATTACHED");
        host.0.kill().unwrap();
        host.0.wait().unwrap();
        saw("FOLLOWED");
        let status = stage.0.wait().unwrap();
        assert!(status.success(), "the Stage process failed: {status}");
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// An asset error as the UI and the logs show it.
pub fn redact(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// Pixel limits do not bound compressed PNG metadata. Reject those optional
/// chunks before either decoder parses headers; no colour/text metadata is
/// needed for a catalogue preview. Chunk traversal itself is byte-bounded.
pub fn bounded_png_metadata(bytes: &[u8]) -> Option<()> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") { return None; }
    let mut offset = 8usize;
    while offset < bytes.len() {
        let header = bytes.get(offset..offset.checked_add(8)?)?;
        let len = u32::from_be_bytes(header[..4].try_into().ok()?) as usize;
        let kind = &header[4..8];
        let end = offset.checked_add(12)?.checked_add(len)?;
        bytes.get(offset..end)?;
        if matches!(kind, b"iCCP" | b"zTXt" | b"iTXt" | b"acTL") { return None; }
        if kind == b"IEND" { return (len == 0 && end == bytes.len()).then_some(()); }
        offset = end;
    }
    None
}
