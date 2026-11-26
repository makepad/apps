//! The standalone Asset Server host.
//!
//! # Why this process exists
//!
//! The asset store is inherently multiplayer. asset-ui, the VJ, the sandbox
//! and headless workers are all clients of ONE catalog, and they come and go
//! independently. When the catalog lives inside one of those apps, that
//! app's lifetime becomes everybody's lifetime: closing the Asset UI window
//! takes the store and the events hub down under every other connected
//! client, which they see as `503 state unavailable` mid-session. This
//! binary breaks that coupling — the server outlives every window.
//!
//! # What it carries
//!
//! [`Host::start`] composes two things, both existing, tested code:
//!
//! 1. [`makepad_asset_store::AssetServer`] — catalog + CAS over the control
//!    and data planes, the games publish path, the committed events hub,
//!    game rooms, the blob GC janitor, and (when configured) store sync over
//!    the fleet TLS front. The planes are loopback only: the LAN reaches a
//!    store through store sync's authenticated front, never plain HTTP, and
//!    no store announces itself unauthenticated.
//! 2. The **library publisher** (`makepad_asset_importer::watch`) — whatever
//!    the generation pipelines write into the ai-content library becomes
//!    catalog rows. Headless, so it belongs beside the server rather than
//!    inside a UI.
//!
//! # What deliberately stays client-side
//!
//! Everything that CREATES content (aicore: "the store stores, the client
//! creates"). Generation runs in the creating apps over their own ai-hub
//! fleet connections (`makepad-gen`), chat sessions live in-app,
//! and loops that derive content with resources only a UI process has — the
//! offscreen thumbnail renders, the classic-game import wizards, the
//! stems/lyrics analysis bake — stay in the app and reach the catalog as
//! ordinary clients.
//!
//! # Single-owner laws
//!
//! - One process per server root (`<root>/server.lock`). Starting this
//!   binary while the Asset UI still hosts the same root fails cleanly on
//!   the lock instead of corrupting anything; stop the other host first, or
//!   let it attach (see the README).
//! - An `AssetClient` cache root is single-owner too, so each loop gets its
//!   own child of the work root.
//!
//! # One library per app
//!
//! Every app has its own store (`paths::app_store_root`): Stage keeps the
//! checkout's `local/asset-library/store`, any other app gets
//! `local/asset-library/<app>/store`. An app hosts its store in-process
//! ([`embed`]) and attaches only to a server that serves that same store;
//! this binary is a tool for running one store out of process, named with
//! `--app` or `--root`.

pub mod backup;
pub mod embed;
pub mod sync_config;

use makepad_asset_client::{ApiEndpoints, AssetClient, ClientConfig, PublishRights};
use makepad_asset_store::{AssetServer, BlobRefPolicy, ServerConfig};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Default namespace the library publisher publishes into. Same value the
/// creator apps publish with, so rows from either path sit side by side.
pub const DEFAULT_NAMESPACE: &str = "gen";

/// Everything the host needs. Every knob is a named field: nothing here is
/// read from the environment behind the operator's back (the binary maps
/// flags and two documented env fallbacks onto it).
#[derive(Clone, Debug)]
pub struct HostConfig {
    /// Server root: catalog, CAS, `server.lock`, `server-id`, `listen`,
    /// `admin-token`.
    pub root: PathBuf,
    /// Control plane bind address: loopback only (`127.0.0.1:0` by default,
    /// an ephemeral port published in `<root>/listen`, which is what every
    /// attaching client reads). A non-loopback address refuses to start.
    pub control_addr: SocketAddr,
    pub data_addr: SocketAddr,
    /// Store sync with the fleet (the only network listener a store has):
    /// see [`sync_config`]. `None` = off.
    pub sync: Option<makepad_asset_store::SyncConfig>,
    /// ai-content library to publish continuously. `None` = no publisher.
    pub library: Option<PathBuf>,
    /// Namespace for published rows.
    pub namespace: String,
    /// Parent for the loops' single-owner client caches. Defaults to the
    /// server root's parent, which puts them exactly where the Asset UI's
    /// own hosting mode puts them.
    pub work_root: PathBuf,
    /// Reference-import policy handed to the server. The deployment default
    /// (owned blobs only); a loopback-only embedder may open this up.
    pub blob_refs: BlobRefPolicy,
    /// Largest blob the store admits; `None` = the store default (256 MiB).
    /// Per app ([`makepad_asset_client::paths::app_max_blob_bytes`]): Stage's
    /// video library takes 16 GiB. A larger limit also stretches the upload
    /// body deadline ([`upload_deadline_ms`]).
    pub max_blob_bytes: Option<u64>,
    /// Live-connection cap per plane; `None` = the store default (64). A
    /// machine-wide host serving many app instances (each keeps a pool of
    /// clients and long-polls) needs more: over the cap a client gets an
    /// immediate 503, which surfaces as failed loads.
    pub max_conns: Option<usize>,
    /// Log to stderr.
    pub log: bool,
}

/// The data-plane body deadline for a store admitting `max_blob_bytes`:
/// the default 10 minutes, or enough for the largest blob at 10 MB/s (a slow
/// LAN), whichever is longer — so a large upload is bounded, not refused
/// half way by a deadline sized for small ones.
pub fn upload_deadline_ms(default_ms: u64, max_blob_bytes: u64) -> u64 {
    default_ms.max(max_blob_bytes / 10_000)
}

impl HostConfig {
    /// Deployment defaults: ephemeral loopback planes, no sync.
    pub fn new(root: PathBuf) -> Self {
        let work_root = root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.clone());
        Self {
            root,
            control_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            data_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            sync: None,
            library: None,
            namespace: DEFAULT_NAMESPACE.to_string(),
            work_root,
            blob_refs: BlobRefPolicy::default(),
            max_blob_bytes: None,
            max_conns: None,
            log: true,
        }
    }
}

/// A running host. Dropping it (or calling [`Host::shutdown`]) stops the
/// publisher first and the server last, so nothing is still publishing into
/// a catalog that is closing.
pub struct Host {
    // Declaration order IS drop order: the loop is joined while the server
    // it talks to is still answering.
    publish: Option<BackgroundLoop>,
    server: Option<AssetServer>,
    endpoints: ApiEndpoints,
    server_id: [u8; 16],
    token: String,
    library_error: Option<String>,
}

/// One owned background thread plus the flag that stops it.
struct BackgroundLoop {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Drop for BackgroundLoop {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Host {
    /// Bring the server up, then the publisher.
    ///
    /// The SERVER is the only thing whose failure is fatal — a publisher
    /// that cannot start is reported ([`Host::library_error`]) and logged,
    /// never a reason to deny every client its catalog.
    pub fn start(config: &HostConfig) -> Result<Host, String> {
        // Opening the store upgrades an older catalog in place: copy it
        // aside first, or do not open it at all.
        // No plain-HTTP LAN listener: the network reaches a store only
        // through store sync's fleet TLS front.
        for (plane, addr) in [("control", config.control_addr), ("data", config.data_addr)] {
            if !addr.ip().is_loopback() {
                return Err(format!(
                    "the {plane} plane binds loopback only ({addr} is not); the LAN reaches a store through store sync (a store-sync file)"
                ));
            }
        }
        backup::backup_before_upgrade(&config.root, config.log)?;
        let mut cfg = ServerConfig::new(config.root.clone());
        cfg.control_addr = config.control_addr;
        cfg.data_addr = config.data_addr;
        // The host mints (or re-mints) the root admin token: its own loops
        // authenticate with it, and an attaching client reads it from the
        // root the same way it does against an Asset-UI-hosted server.
        cfg.bootstrap_admin = true;
        cfg.sync = config.sync.clone();
        cfg.blob_refs = config.blob_refs.clone();
        if let Some(max) = config.max_blob_bytes {
            cfg.budgets.max_blob_bytes = max;
            cfg.data_body_deadline_ms = upload_deadline_ms(cfg.data_body_deadline_ms, max);
        }
        if let Some(max) = config.max_conns {
            cfg.control_max_conns = max;
            cfg.data_max_conns = max;
        }
        cfg.log = config.log;
        let server = AssetServer::start(cfg).map_err(|error| format!("asset server: {error}"))?;

        let token = std::fs::read_to_string(config.root.join("admin-token"))
            .map_err(|error| format!("admin token: {error}"))?
            .trim()
            .to_string();
        if token.is_empty() {
            return Err("admin token file is empty".to_string());
        }
        let endpoints = ApiEndpoints { control: server.control_addr(), data: server.data_addr() };
        let server_id = server.server_id();

        let (publish, library_error) = match &config.library {
            None => (None, None),
            Some(dir) => match start_publisher(config, dir, endpoints, server_id, &token) {
                Ok(handle) => (Some(handle), None),
                Err(error) => {
                    // a failure: printed whatever `config.log` says
                    log(true, &format!("library publisher: {error}"));
                    (None, Some(error))
                }
            },
        };

        log(
            config.log,
            &format!(
                "asset host up: control {} data {} · publisher {}",
                endpoints.control,
                endpoints.data,
                publish.as_ref().map_or("off", |_| "on"),
            ),
        );
        Ok(Host {
            publish,
            server: Some(server),
            endpoints,
            server_id,
            token,
            library_error,
        })
    }

    /// The loopback planes, which is what `<root>/listen` advertises too.
    pub fn endpoints(&self) -> ApiEndpoints {
        self.endpoints
    }

    pub fn server_id(&self) -> [u8; 16] {
        self.server_id
    }

    /// The root admin bearer token this host bootstrapped.
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn publisher_running(&self) -> bool {
        self.publish.is_some()
    }

    /// Store sync: `serving on <addr>`, `off`, or why it is off although
    /// configured.
    pub fn sync_note(&self) -> String {
        let Some(server) = &self.server else { return "off".into() };
        match (server.sync_addr(), server.sync_error()) {
            (Some(addr), _) => format!("on ({addr})"),
            (None, Some(error)) => format!("refused: {error}"),
            (None, None) => "off".into(),
        }
    }

    /// Why the library publisher is not running, when it was asked for.
    pub fn library_error(&self) -> Option<&str> {
        self.library_error.as_deref()
    }

    /// Stop the publisher, then the server. Idempotent; also runs on drop.
    pub fn shutdown(&mut self) {
        drop(self.publish.take());
        if let Some(mut server) = self.server.take() {
            server.shutdown();
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Publish everything the pipelines write into the ai-content library.
///
/// The client connects HERE, on the caller's thread, so "the publisher could
/// not reach the server it is hosted beside" is an answer the operator (and
/// the tests) get immediately instead of a log line scrolling past.
fn start_publisher(
    config: &HostConfig,
    dir: &Path,
    endpoints: ApiEndpoints,
    server_id: [u8; 16],
    token: &str,
) -> Result<BackgroundLoop, String> {
    let mut client_config = ClientConfig::new(config.work_root.join("publish-cache"));
    client_config.token = Some(token.to_string());
    let mut client = AssetClient::connect(client_config, endpoints, Some(server_id))
        .map_err(|error| format!("cannot connect to the local server: {error}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let dir = dir.to_path_buf();
    let namespace = config.namespace.clone();
    let log_enabled = config.log;
    let join = std::thread::Builder::new()
        .name("asset-host-publish".to_string())
        .spawn(move || {
            log(
                log_enabled,
                &format!("library publisher: watching {}", dir.display()),
            );
            makepad_asset_importer::watch::run(
                &mut client,
                &dir,
                &namespace,
                &PublishRights::generated_cc0(),
                log_enabled,
                &thread_stop,
                makepad_asset_importer::watch::Timing::default(),
                |_| {},
            );
            log(log_enabled, "library publisher: stopped");
        })
        .map_err(|error| format!("cannot spawn the publisher thread: {error}"))?;
    Ok(BackgroundLoop { stop, join: Some(join) })
}

fn log(enabled: bool, message: &str) {
    if enabled {
        eprintln!("[asset-host] {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "mp_asset_host_{}_{}_{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    /// An isolated host: loopback-only ephemeral planes, no sync.
    fn isolated(name: &str) -> HostConfig {
        let base = test_root(name);
        let mut config = HostConfig::new(base.join("server"));
        config.control_addr = "127.0.0.1:0".parse().unwrap();
        config.data_addr = "127.0.0.1:0".parse().unwrap();
        config.log = false;
        config.work_root = base.join("work");
        config
    }

    fn cleanup(config: &HostConfig) {
        if let Some(parent) = config.root.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn the_host_serves_a_catalog_and_shuts_down_cleanly() {
        let config = isolated("serves");
        let mut host = Host::start(&config).expect("host starts");
        assert!(!host.token().is_empty(), "the host bootstraps an admin token");
        assert!(
            config.root.join("listen").is_file(),
            "the listen file is how every client attaches"
        );

        // A real client over real sockets, exactly as any other process
        // would reach it.
        let mut client_config = ClientConfig::new(config.work_root.join("probe-cache"));
        client_config.token = Some(host.token().to_string());
        let client =
            AssetClient::connect(client_config, host.endpoints(), Some(host.server_id()))
                .expect("client connects to the standalone host");
        assert_eq!(client.server_id(), host.server_id());

        drop(client);
        host.shutdown();
        host.shutdown();
        cleanup(&config);
    }

    #[test]
    fn a_second_host_on_one_root_is_refused_by_the_root_lock() {
        let config = isolated("locked");
        let host = Host::start(&config).expect("first host starts");
        let error = match Host::start(&config) {
            Ok(_) => panic!("the root lock is single-owner: a second host must be refused"),
            Err(error) => error,
        };
        assert!(
            error.contains("locked by another server process"),
            "the refusal must name the lock, got {error}"
        );
        drop(host);
        cleanup(&config);
    }

    /// The publisher is a CLIENT of the server beside it, and a library that
    /// is not there yet must not cost anyone their catalog: the loop starts,
    /// the server keeps serving, and shutdown still joins cleanly.
    #[test]
    fn a_missing_library_directory_never_costs_the_catalog_its_server() {
        let mut config = isolated("library");
        config.library = Some(config.work_root.join("nonexistent-library"));
        let mut host = Host::start(&config).expect("host starts");
        assert!(
            host.publisher_running(),
            "the publisher connected: {:?}",
            host.library_error()
        );
        assert_eq!(host.library_error(), None);

        let mut client_config = ClientConfig::new(config.work_root.join("probe-cache"));
        client_config.token = Some(host.token().to_string());
        AssetClient::connect(client_config, host.endpoints(), Some(host.server_id()))
            .expect("the server serves while the publisher waits for its library");

        host.shutdown();
        cleanup(&config);
    }

    /// The blob limit is a store setting: the host applies it, its health
    /// answer states it, every client (a second instance attaching over the
    /// root's `listen` included) learns it at connect and refuses a larger
    /// blob before moving a byte.
    #[test]
    fn the_blob_limit_is_the_stores_and_every_client_learns_it() {
        let mut config = isolated("limit");
        config.max_blob_bytes = Some(64 * 1024);
        let host = Host::start(&config).expect("host starts");

        let listen = std::fs::read_to_string(config.root.join("listen")).unwrap();
        let mut parts = listen.trim().rsplitn(3, ':');
        let data: u16 = parts.next().unwrap().parse().unwrap();
        let control: u16 = parts.next().unwrap().parse().unwrap();
        let attached = ApiEndpoints {
            control: SocketAddr::from(([127, 0, 0, 1], control)),
            data: SocketAddr::from(([127, 0, 0, 1], data)),
        };
        let mut client_config = ClientConfig::new(config.work_root.join("probe-cache"));
        client_config.token = Some(host.token().to_string());
        let client = AssetClient::connect(client_config, attached, Some(host.server_id()))
            .expect("an attaching client connects over the root's listen");
        assert_eq!(client.server_max_blob_bytes(), Some(64 * 1024));
        assert_eq!(client.lane_clone().server_max_blob_bytes(), Some(64 * 1024));

        let small = vec![7u8; 32 * 1024];
        client.upload_blob("gen", &small).expect("a blob under the limit uploads");
        let large = vec![7u8; 64 * 1024 + 1];
        match client.upload_blob("gen", &large) {
            Err(makepad_asset_client::ClientError::OverBudget { limit, found, .. }) => {
                assert_eq!((limit, found), (64 * 1024, 64 * 1024 + 1));
            }
            other => panic!("a blob over the store's limit is refused locally, got {other:?}"),
        }
        drop(client);
        drop(host);
        cleanup(&config);
    }

    #[test]
    fn stage_hosts_a_video_library_and_other_apps_keep_the_default() {
        use makepad_asset_client::paths;
        let mut config = isolated("stage-limit");
        config.max_blob_bytes = paths::app_max_blob_bytes(paths::STAGE_APP);
        let host = Host::start(&config).expect("host starts");
        let mut client_config = ClientConfig::new(config.work_root.join("probe-cache"));
        client_config.token = Some(host.token().to_string());
        let client = AssetClient::connect(client_config, host.endpoints(), Some(host.server_id())).unwrap();
        assert_eq!(client.server_max_blob_bytes(), Some(16 << 30));
        drop(client);
        drop(host);
        cleanup(&config);

        let config = isolated("default-limit");
        assert_eq!(config.max_blob_bytes, paths::app_max_blob_bytes("sandbox"));
        let host = Host::start(&config).expect("host starts");
        let mut client_config = ClientConfig::new(config.work_root.join("probe-cache"));
        client_config.token = Some(host.token().to_string());
        let client = AssetClient::connect(client_config, host.endpoints(), Some(host.server_id())).unwrap();
        assert_eq!(client.server_max_blob_bytes(), Some(256 * 1024 * 1024), "the store default");
        drop(client);
        drop(host);
        cleanup(&config);
    }

    #[test]
    fn a_lan_plane_is_refused() {
        let mut config = isolated("lan-plane");
        config.control_addr = "0.0.0.0:0".parse().unwrap();
        let error = Host::start(&config).err().expect("a 0.0.0.0 plane must not start");
        assert!(error.contains("loopback only"), "{error}");
        let mut config = isolated("lan-data");
        config.data_addr = "192.0.2.5:0".parse().unwrap();
        assert!(Host::start(&config).is_err());
        cleanup(&config);
    }

    #[test]
    fn a_larger_blob_limit_stretches_the_upload_deadline() {
        assert_eq!(upload_deadline_ms(600_000, 256 * 1024 * 1024), 600_000);
        // 16 GiB at 10 MB/s: about 29 minutes.
        assert_eq!(upload_deadline_ms(600_000, 16 << 30), (16u64 << 30) / 10_000);
    }

    #[test]
    fn the_work_root_defaults_beside_the_server_root() {
        let config = HostConfig::new(PathBuf::from("/checkout/local/asset-library/store"));
        assert_eq!(config.work_root, PathBuf::from("/checkout/local/asset-library"));
        assert!(config.sync.is_none(), "store sync is off unless configured");
        assert!(config.control_addr.ip().is_loopback() && config.data_addr.ip().is_loopback(), "loopback only");
        assert_eq!(config.namespace, DEFAULT_NAMESPACE);
        assert_eq!(
            config.control_addr.port(),
            0,
            "ephemeral by default: the listen file is the address of record"
        );
    }
}
