//! FULLY-LOCAL MODE, shared: an app hosting its own Asset Server.
//!
//! One asset library per app. Every app has its own store
//! ([`app_store_root`]): Stage keeps the checkout's
//! `local/asset-library/store`, the library it always had; any other app
//! gets `local/asset-library/<app>/store`. Apps never share a store, and
//! never attach to one another's server.
//!
//! An app hosts its store in-process ([`Host`], on 127.0.0.1 only) with its
//! library publisher riding along. The one exception is a store that is
//! ALREADY served — a second instance of the same app, or the standalone
//! `makepad-asset-server --app <name>` — which the app attaches to instead:
//! the store's own lock admits one host.
//!
//! The thin-client law does not bend for this. Hosting changes WHERE the
//! store runs, never who owns the content: the app still browses, publishes
//! and fetches over HTTP, through the same `AssetClient`, with the same
//! catalog-event subscription. Nothing durable moves into the app.
//!
//! ## Loopback, deliberately
//!
//! The embedded host binds 127.0.0.1 for both planes (port 0 = OS-assigned,
//! or pinned by `<PREFIX>_ASSET_PORT`) and announces nothing: an app
//! hosting for itself has no business accepting connections from the
//! network, and announcing the store would invite other apps onto a private
//! instance of the user's library. That is also what makes reference
//! imports safe to enable here (see `makepad_asset_store::blobrefs`).
//!
//! ## Choosing
//!
//! `<PREFIX>_ASSET_EMBED` decides, `auto` by default:
//!
//! - `never`  — attach only; if nothing is reachable, behave as before.
//! - `auto`   — attach if THIS store is already served, else host.
//! - `always` — host, regardless of what else is running.
//!
//! "Already served" is a real probe of this store's identity (the
//! `server-id` its root holds), not "some server answers": `GET /v1/health`
//! at the caller's hint or the root's `listen` address must report that id,
//! failing that the root's `server.lock` is held (a held lock is proof the
//! store is hosted RIGHT NOW whatever its ports, and hosting then would
//! fail on the lock anyway). Every process that can serve this root holds
//! that lock, so nothing more is asked: no LAN beacon listen (it cost every
//! start whose previous host had exited 2.4 s, for an answer the lock
//! already gave). A server of any other store — another app's, a scratch
//! instance — is never an answer.

use crate::{Host, HostConfig};
use makepad_asset_client::util::from_hex_exact;
use makepad_asset_client::{json, paths, ApiEndpoints};
use makepad_asset_store::BlobRefPolicy;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How the app got its store this run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreMode {
    /// Attached to a store some other process runs (the usual case).
    Attached,
    /// Hosting the store in this process, on loopback.
    Hosting,
    /// Hosting it here failed and no other process serves it: there is
    /// nothing to attach to. The note says why; a caller shows that rather
    /// than wait on a server that never comes.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedPolicy {
    Never,
    Auto,
    Always,
}

/// `<PREFIX>_ASSET_EMBED`, `auto` when unset or unrecognised.
pub fn embed_policy(prefix: &str) -> EmbedPolicy {
    match std::env::var(format!("{prefix}_ASSET_EMBED"))
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "never" | "off" | "0" => EmbedPolicy::Never,
        "always" | "force" | "1" => EmbedPolicy::Always,
        _ => EmbedPolicy::Auto,
    }
}

/// `app`'s store root: see [`makepad_asset_client::paths::app_store_root`]
/// (`<APP>_ASSET_ROOT`, else Stage's library store or
/// `local/asset-library/<app>/store`).
pub fn app_store_root(app: &str) -> PathBuf {
    paths::app_store_root(app)
}

/// Stage's store root, the library this checkout always had. Kept for
/// callers from before one-library-per-app; name the app instead.
pub fn main_store_root() -> PathBuf {
    paths::app_store_root(paths::STAGE_APP)
}

/// The app an env prefix belongs to. Stage's surfaces use three prefixes
/// (`STAGE`, the VJ's `VJ`, the flow side's `FLOW`) over one library; any
/// other prefix is its app's name (`SANDBOX` -> `sandbox`).
pub fn app_for_prefix(prefix: &str) -> String {
    match prefix {
        "STAGE" | "VJ" | "FLOW" => paths::STAGE_APP.to_string(),
        _ => prefix.to_ascii_lowercase().replace('_', "-"),
    }
}

/// The store `prefix` works on: `<PREFIX>_ASSET_ROOT` when set, else its
/// app's store ([`app_for_prefix`]). The seed argument remains
/// source-compatible and never creates a second library.
pub fn default_store_root(prefix: &str, _seed_dir: &str) -> PathBuf {
    std::env::var_os(format!("{prefix}_ASSET_ROOT"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| app_store_root(&app_for_prefix(prefix)))
}

/// The identity of the store at `root` (its `server-id`), once any server
/// has ever served it.
pub fn root_server_id(root: &Path) -> Option<[u8; 16]> {
    let text = std::fs::read_to_string(root.join("server-id")).ok()?;
    from_hex_exact::<16>(text.trim())
}

/// The planes the root's `listen` file names (`ip:control:data`).
fn root_listen(root: &Path) -> Option<ApiEndpoints> {
    let text = std::fs::read_to_string(root.join("listen")).ok()?;
    let line = text.lines().next()?.trim();
    let mut parts = line.rsplitn(3, ':');
    let data: u16 = parts.next()?.trim().parse().ok()?;
    let control: u16 = parts.next()?.trim().parse().ok()?;
    let host = parts.next()?.trim();
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    let ip: IpAddr = host.parse().ok()?;
    let ip = if ip.is_unspecified() { IpAddr::V4(Ipv4Addr::LOCALHOST) } else { ip };
    (control != 0 && data != 0).then(|| ApiEndpoints {
        control: SocketAddr::new(ip, control),
        data: SocketAddr::new(ip, data),
    })
}

/// The server id an Asset Server at `addr` reports in `GET /v1/health`.
///
/// Deliberately minimal and deliberately SHORT: this runs on the startup
/// path, and its only job is to tell "this store's server is up" from "that
/// port file is stale, or that port is somebody else's now". A 400 ms budget
/// is generous for loopback and invisible to a human. Anything unexpected
/// reads as "no".
fn health_server_id(addr: SocketAddr) -> Option<[u8; 16]> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(400)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));
    let req = format!("GET /v1/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    // The answer is a few hundred bytes; the cap only bounds a stranger.
    let mut buf = Vec::with_capacity(1024);
    let _ = stream.take(16 * 1024).read_to_end(&mut buf);
    if !buf.starts_with(b"HTTP/1.1 200") {
        return None;
    }
    let body_at = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let body = json::parse(&buf[body_at..]).ok()?;
    from_hex_exact::<16>(body.get("server_id")?.as_str()?)
}

/// Is the store at `root` served by some process right now? `Some(why)`
/// when it is. Only a server with the root's own identity counts: a
/// store never ever served (no `server-id` yet) cannot be.
pub fn store_served(root: &Path, hinted: Option<ApiEndpoints>) -> Option<&'static str> {
    let id = root_server_id(root)?;
    let listed = root_listen(root);
    for endpoints in [hinted, listed].into_iter().flatten() {
        if health_server_id(endpoints.control) == Some(id) {
            return Some("its server answers");
        }
    }
    // THE STORE IS HOSTED BUT NOT ANSWERING YET (a succession handover, a
    // stale `listen` file): the lock holder is proof it exists, and hosting
    // here would fail on that lock anyway.
    lock_held(root).then_some("its lock is held")
}

/// True when another process holds `root`'s server lock — the store is
/// hosted right now, whatever its ports.
fn lock_held(root: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("server.lock"))
    else {
        return false;
    };
    // The SAME advisory lock the store takes (File::try_lock): if we can
    // take it, nobody serves that root; the file drop releases it.
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(_) => true,
    }
}

/// The in-process host (server + library publisher), held for as long as
/// the app runs.
///
/// Dropping it stops the publisher first and the server last, so the field
/// holding this must be declared AFTER anything that talks to the store —
/// the same drop-order discipline asset-ui's `AssetStore` uses.
pub struct LocalStore {
    host: Host,
    root: PathBuf,
}

impl LocalStore {
    pub fn endpoints(&self) -> ApiEndpoints {
        self.host.endpoints()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn control_addr(&self) -> SocketAddr {
        self.host.endpoints().control
    }

    pub fn data_addr(&self) -> SocketAddr {
        self.host.endpoints().data
    }

    pub fn server_id(&self) -> [u8; 16] {
        self.host.server_id()
    }

    pub fn token(&self) -> &str {
        self.host.token()
    }

    pub fn publisher_running(&self) -> bool {
        self.host.publisher_running()
    }

    pub fn sync_note(&self) -> String {
        self.host.sync_note()
    }
}

/// What [`resolve`] decided, and why — the `note` belongs in the app's
/// status line, because "which store am I on" is the first thing that
/// matters when content does not show up.
pub struct Resolved {
    pub mode: StoreMode,
    /// The embedded host, when hosting. The caller owns it for the life of
    /// the app and points its client at [`LocalStore::endpoints`] with
    /// [`LocalStore::server_id`] and [`LocalStore::token`].
    pub local: Option<LocalStore>,
    pub note: String,
    /// The store this decision is about. When attached, the caller connects
    /// to THIS store: its `listen`, `server-id` and `admin-token`, pinned to
    /// that id, never whichever server a LAN discovery turns up.
    pub root: PathBuf,
    /// When attached, those three as the root held them at the decision, read on the deciding
    /// thread (the resolver's, off a UI): the caller connects with them and reads none of the
    /// files. `None` when hosting, unavailable, or decided without a resolver thread.
    pub attached: Option<AttachedStore>,
}

/// What an attached store's root says: where its server listens, its identity and its admin
/// token ([`Resolved::attached`]). A host binds new planes every launch; a client that loses it
/// decides again, which reads them again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttachedStore {
    /// The first line of `listen`, trimmed (`ip:control:data`).
    pub listen: Option<String>,
    pub server_id: Option<[u8; 16]>,
    /// `admin-token`, trimmed; its shape is the caller's to check.
    pub admin_token: Option<String>,
}

impl AttachedStore {
    /// The files of the store at `root`, as they are now.
    pub fn read(root: &Path) -> AttachedStore {
        let read = |name: &str| std::fs::read_to_string(root.join(name)).ok();
        AttachedStore {
            listen: read("listen").and_then(|t| t.lines().next().map(|l| l.trim().to_string())).filter(|l| !l.is_empty()),
            server_id: root_server_id(root),
            admin_token: read("admin-token").map(|t| t.trim().to_string()).filter(|t| !t.is_empty()),
        }
    }
}

/// Decide between attaching and hosting, for the app `prefix` names, on its
/// store ([`default_store_root`]).
///
/// `pinned` is the caller saying "an explicit server was named" — naming a
/// server is not something you do by accident, so it always means attach.
/// `hinted` is where the caller last knew its server to live, for the
/// health probe (the root's own `listen` is probed as well).
pub fn resolve(
    prefix: &str,
    seed_dir: &str,
    pinned: bool,
    hinted: Option<ApiEndpoints>,
) -> Resolved {
    resolve_at(prefix, &default_store_root(prefix, seed_dir), pinned, hinted)
}

/// [`resolve`] on an explicit store root. `prefix` still names the app: its
/// `<PREFIX>_ASSET_EMBED` policy, `<PREFIX>_ASSET_PORT` and its library.
pub fn resolve_at(
    prefix: &str,
    root: &Path,
    pinned: bool,
    hinted: Option<ApiEndpoints>,
) -> Resolved {
    let root = root.to_path_buf();
    let policy = embed_policy(prefix);

    if pinned || policy == EmbedPolicy::Never {
        let note = if pinned {
            format!("asset server pinned by {prefix}_ASSET_SERVER")
        } else {
            format!("attach-only ({prefix}_ASSET_EMBED=never)")
        };
        let attached = Some(AttachedStore::read(&root));
        return Resolved { mode: StoreMode::Attached, local: None, note, root, attached };
    }

    if policy == EmbedPolicy::Auto {
        if let Some(why) = store_served(&root, hinted) {
            return Resolved {
                mode: StoreMode::Attached,
                local: None,
                note: format!("attached to the server of {} ({why})", root.display()),
                attached: Some(AttachedStore::read(&root)),
                root,
            };
        }
    }

    match host_at(&root, prefix) {
        Ok(local) => {
            let note = format!(
                "local store on {} over {} · library publisher {} · store sync {}",
                local.control_addr(),
                root.display(),
                if local.publisher_running() { "on" } else { "off" },
                local.sync_note()
            );
            Resolved { mode: StoreMode::Hosting, local: Some(local), note, root, attached: None }
        }
        Err(error) => {
            // Hosting failed. When another process took this root's lock
            // since the probe, it serves the store: attach to it. Otherwise
            // nobody does, and an attach would wait forever on nothing.
            let served = store_served(&root, hinted)
                .or_else(|| lock_held(&root).then_some("its lock is held"));
            match served {
                Some(why) => Resolved {
                    mode: StoreMode::Attached,
                    local: None,
                    note: format!(
                        "attached to the server of {} ({why}; hosting it here failed: {error})",
                        root.display()
                    ),
                    attached: Some(AttachedStore::read(&root)),
                    root,
                },
                None => Resolved {
                    mode: StoreMode::Unavailable,
                    local: None,
                    note: format!("could not host {} ({error}), and nothing else serves it", root.display()),
                    root,
                    attached: None,
                },
            }
        }
    }
}

/// [`resolve`] off the calling thread: the probe (a loopback health probe
/// of up to 400 ms) and any hosting run on a thread of their own, and this returns
/// at once. Poll the handle from the UI loop; see [`resolve_async_notify`]
/// to be woken when the answer lands.
pub fn resolve_async(
    prefix: &str,
    seed_dir: &str,
    pinned: bool,
    hinted: Option<ApiEndpoints>,
) -> ResolveHandle {
    resolve_async_notify(prefix, seed_dir, pinned, hinted, || {})
}

/// [`resolve_async`], calling `notify` on the resolver thread right after
/// the answer is ready (a UI passes its wake-up, e.g.
/// `SignalToUI::set_ui_signal`). The root is chosen on the calling thread
/// (env reads only), so [`ResolveHandle::root`] is known at once.
pub fn resolve_async_notify(
    prefix: &str,
    seed_dir: &str,
    pinned: bool,
    hinted: Option<ApiEndpoints>,
    notify: impl FnOnce() + Send + 'static,
) -> ResolveHandle {
    resolve_at_async_notify(prefix, &default_store_root(prefix, seed_dir), pinned, hinted, notify)
}

/// [`resolve_at`] off the calling thread; see [`resolve_async_notify`].
pub fn resolve_at_async_notify(
    prefix: &str,
    root: &Path,
    pinned: bool,
    hinted: Option<ApiEndpoints>,
    notify: impl FnOnce() + Send + 'static,
) -> ResolveHandle {
    spawn_resolver(prefix, root, notify, move |prefix, root, _cancelled| {
        Some(resolve_at(prefix, root, pinned, hinted))
    })
}

/// Attached to the server of `root`, keep the store: wait, on a thread of
/// its own, for the process hosting it to let go of it, then resolve again,
/// which hosts it here (or attaches to whichever other attached process
/// claimed it first). The wait is a blocking take of the store's
/// `server.lock`, which the host holds for its whole life and the OS
/// releases the moment it quits or dies: no polling, and no window in which
/// the store is served by nobody while an app still wants it. The answer
/// arrives like any resolution; an app that is attached again simply waits
/// again. Dropping the handle cancels it: a store freed after that is not
/// hosted for nobody.
pub fn resolve_when_freed_notify(
    prefix: &str,
    root: &Path,
    notify: impl FnOnce() + Send + 'static,
) -> ResolveHandle {
    spawn_resolver(prefix, root, notify, |prefix, root, cancelled| {
        let started = std::time::Instant::now();
        wait_for_lock_release(root);
        // A lock that was free at once, and a store still answering to us
        // (a host that never took the lock), must not become a busy loop.
        if started.elapsed() < Duration::from_millis(50) {
            std::thread::sleep(Duration::from_secs(1));
        }
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        let mut resolved = resolve_at(prefix, root, false, None);
        if resolved.mode == StoreMode::Hosting {
            resolved.note = format!("{} (took the store over: its previous host quit)", resolved.note);
        }
        Some(resolved)
    })
}

/// Block until nobody holds `root`'s server lock, then let go of it again
/// at once (the host takes it for itself). A root without a lock file has
/// never been hosted: nothing to wait for.
fn wait_for_lock_release(root: &Path) {
    let Ok(file) = std::fs::OpenOptions::new().write(true).open(root.join("server.lock")) else {
        return;
    };
    if file.lock().is_ok() {
        let _ = file.unlock();
    }
}

fn spawn_resolver(
    prefix: &str,
    root: &Path,
    notify: impl FnOnce() + Send + 'static,
    resolve: impl FnOnce(&str, &Path, &std::sync::atomic::AtomicBool) -> Option<Resolved> + Send + 'static,
) -> ResolveHandle {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread_cancelled = cancelled.clone();
    let thread_prefix = prefix.to_string();
    let thread_root = root.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name(format!("{}-asset-resolve", prefix.to_ascii_lowercase()))
        .spawn(move || {
            let Some(resolved) = resolve(&thread_prefix, &thread_root, &thread_cancelled) else {
                return;
            };
            // A handle dropped meanwhile drops the answer here, and with it
            // any store hosted for nobody.
            let _ = tx.send(resolved);
            notify();
        });
    let state = match spawned {
        Ok(_) => ResolveState::Pending(rx),
        Err(error) => ResolveState::Ready(Resolved {
            mode: StoreMode::Attached,
            local: None,
            note: format!("could not start the store resolver thread ({error}); attaching instead"),
            root: root.to_path_buf(),
            attached: None,
        }),
    };
    ResolveHandle { root: root.to_path_buf(), state, cancelled }
}

/// A [`resolve`] in flight ([`resolve_async`]).
pub struct ResolveHandle {
    root: PathBuf,
    state: ResolveState,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for ResolveHandle {
    fn drop(&mut self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::Release);
    }
}

enum ResolveState {
    Pending(std::sync::mpsc::Receiver<Resolved>),
    Ready(Resolved),
    Taken,
}

impl ResolveHandle {
    /// The store being resolved (known before the answer).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The answer, exactly once, when it is ready; `None` before that and
    /// after it was taken. Never blocks.
    pub fn poll(&mut self) -> Option<Resolved> {
        if let ResolveState::Pending(rx) = &self.state {
            match rx.try_recv() {
                Ok(resolved) => self.state = ResolveState::Ready(resolved),
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // The resolver thread died without answering (a panic).
                    self.state = ResolveState::Ready(self.failed("the store resolver stopped without an answer"));
                }
            }
        }
        match std::mem::replace(&mut self.state, ResolveState::Taken) {
            ResolveState::Ready(resolved) => Some(resolved),
            other => {
                self.state = other;
                None
            }
        }
    }

    /// True until [`poll`](Self::poll) has handed the answer out.
    pub fn is_pending(&self) -> bool {
        !matches!(self.state, ResolveState::Taken)
    }

    /// Block until the answer (for worker threads and tests). `None` when
    /// it was already taken by [`poll`](Self::poll).
    pub fn wait(mut self) -> Option<Resolved> {
        match std::mem::replace(&mut self.state, ResolveState::Taken) {
            ResolveState::Pending(rx) => Some(
                rx.recv().unwrap_or_else(|_| self.failed("the store resolver stopped without an answer")),
            ),
            ResolveState::Ready(resolved) => Some(resolved),
            ResolveState::Taken => None,
        }
    }

    fn failed(&self, why: &str) -> Resolved {
        Resolved {
            mode: StoreMode::Attached,
            local: None,
            note: format!("{why}; attaching instead"),
            root: self.root.clone(),
            attached: None,
        }
    }
}

/// Bring up the in-process host on loopback.
///
/// This is the standalone asset-server's own [`Host`] — catalog + CAS plus
/// the app's LIBRARY PUBLISHER, so a self-hosted app sees the same rows the
/// standalone server would publish for it, not a bare seed store. Loopback
/// only, like every host; store sync when the app's store-sync file says so.
fn host_at(root: &Path, prefix: &str) -> Result<LocalStore, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("create store root: {e}"))?;
    let port: u16 = std::env::var(format!("{prefix}_ASSET_PORT"))
        .ok()
        .and_then(|p| p.trim().parse().ok())
        .unwrap_or(0);
    let mut cfg = HostConfig::new(root.to_path_buf());
    cfg.control_addr = SocketAddr::from(([127, 0, 0, 1], port));
    // The data plane always takes an OS-assigned port: pinning one would
    // only create a second thing to collide.
    cfg.data_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    // Store sync with the fleet, when the app has a store-sync file: the
    // fleet TLS front is the only listener off loopback.
    cfg.sync = crate::sync_config::load(prefix, &app_for_prefix(prefix))?;
    // The app's generation library, when this checkout has one: the
    // publisher keeps turning it into catalog rows exactly as the
    // standalone server would.
    let library = paths::app_library_root(&app_for_prefix(prefix));
    cfg.library = library.join("index.json").exists().then_some(library);
    // Reference imports ON: the whole point of the local mode is pointing
    // the store at content that stays where it is. Loopback-only and no
    // prefix restriction, which is exactly the privilege this process
    // already has over the user's own files.
    cfg.blob_refs = BlobRefPolicy::local_host();
    // The app's blob limit is a store setting (Stage: 16 GiB for video);
    // an attached instance learns it from this host's health answer.
    cfg.max_blob_bytes = paths::app_max_blob_bytes(&app_for_prefix(prefix));
    // No operator progress lines inside an app (its failures still print).
    cfg.log = false;
    let host = Host::start(&cfg).map_err(|e| format!("{e}"))?;
    if host.token().is_empty() {
        return Err("admin token file empty".to_string());
    }
    Ok(LocalStore { host, root: root.to_path_buf() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_base(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "mp_asset_embed_{}_{}_{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn every_app_gets_its_own_store_and_stage_keeps_the_library() {
        assert_eq!(app_for_prefix("STAGE"), "stage");
        assert_eq!(app_for_prefix("VJ"), "stage");
        assert_eq!(app_for_prefix("FLOW"), "stage");
        assert_eq!(app_for_prefix("SANDBOX"), "sandbox");
        assert_eq!(app_store_root("stage"), main_store_root());
        if std::env::var_os("SANDBOX_ASSET_ROOT").is_none() {
            let sandbox = default_store_root("SANDBOX", "sandbox-assets");
            assert_eq!(sandbox, paths::library_root().join("sandbox").join("store"));
            assert_ne!(sandbox, main_store_root(), "Sandbox never shares Stage's store");
        }
        if std::env::var_os("STAGE_ASSET_ROOT").is_none() && std::env::var_os("AI_CONTENT_ASSET_ROOT").is_none() {
            assert_eq!(default_store_root("STAGE", "stage"), paths::library_root().join("store"));
        }
    }

    #[test]
    fn the_listen_file_names_the_planes() {
        let base = temp_base("listen");
        assert_eq!(root_listen(&base), None);
        std::fs::write(base.join("listen"), "127.0.0.1:4000:4001\n").unwrap();
        let endpoints = root_listen(&base).unwrap();
        assert_eq!(endpoints.control, "127.0.0.1:4000".parse().unwrap());
        assert_eq!(endpoints.data, "127.0.0.1:4001".parse().unwrap());
        std::fs::write(base.join("listen"), "0.0.0.0:4000:4001\n").unwrap();
        assert_eq!(root_listen(&base).unwrap().control, "127.0.0.1:4000".parse().unwrap());
        std::fs::write(base.join("listen"), "127.0.0.1:0:4001\n").unwrap();
        assert_eq!(root_listen(&base), None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A store served before whose host has exited (its `server-id` and a
    /// stale `listen` remain, nobody holds the lock) is known free at once:
    /// no LAN listen stands between an app's launch and its own store.
    #[test]
    fn a_store_whose_host_exited_is_free_without_waiting() {
        let base = temp_base("exited");
        std::fs::write(base.join("server-id"), "11".repeat(16)).unwrap();
        std::fs::write(base.join("listen"), "127.0.0.1:9:9\n").unwrap();
        std::fs::write(base.join("server.lock"), b"").unwrap();
        let started = std::time::Instant::now();
        assert_eq!(store_served(&base, None), None);
        assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// `GET /v1/auth/whoami` with `token`: the HTTP status line's code.
    fn whoami_status(addr: SocketAddr, token: &str) -> Option<u16> {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
        let req = format!(
            "GET /v1/auth/whoami HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).ok()?;
        let mut buf = Vec::new();
        let _ = stream.take(16 * 1024).read_to_end(&mut buf);
        std::str::from_utf8(buf.get(9..12)?).ok()?.parse().ok()
    }

    /// A store claimed by nobody: its catalog and CAS copied from another
    /// machine without the transport database, identity or admin token,
    /// beside a leftover `server.lock` and a `listen` naming a dead port.
    /// The stale claim is no claim, so the app hosts it, adopting the
    /// catalog's root admin with a working token. A store that cannot be
    /// hosted and that nobody serves is `Unavailable`, never an attach.
    #[test]
    fn a_stale_claim_on_a_copied_catalog_is_hosted_here() {
        let base = temp_base("copied");
        let root = base.join("store");
        let first = resolve_at("L11TESTD", &root, false, None);
        assert_eq!(first.mode, StoreMode::Hosting, "{}", first.note);
        drop(first);
        for leaf in ["transport.sqlite3", "transport.sqlite3-wal", "transport.sqlite3-shm", "admin-token", "server-id"] {
            let _ = std::fs::remove_file(root.join(leaf));
        }
        std::fs::write(root.join("server.lock"), b"").unwrap();
        std::fs::write(root.join("listen"), "127.0.0.1:9:9\n").unwrap();
        assert_eq!(store_served(&root, None), None, "a leftover lock file and listen are stale");

        let again = resolve_at("L11TESTD", &root, false, None);
        assert_eq!(again.mode, StoreMode::Hosting, "{}", again.note);
        let host = again.local.expect("hosted here");
        assert_eq!(health_server_id(host.control_addr()), Some(host.server_id()));
        assert_eq!(whoami_status(host.control_addr(), host.token()), Some(200), "the adopted root admin's token works");
        drop(host);

        let blocked = base.join("not-a-dir");
        std::fs::write(&blocked, b"").unwrap();
        let unhostable = resolve_at("L11TESTD", &blocked.join("store"), false, None);
        assert_eq!(unhostable.mode, StoreMode::Unavailable, "{}", unhostable.note);
        assert!(unhostable.local.is_none());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn one_app_shares_its_store_and_another_app_never_joins_it() {
        let base = temp_base("apps");
        let root_a = base.join("a/store");
        let root_b = base.join("b/store");

        let first = resolve_at("L11TESTA", &root_a, false, None);
        assert_eq!(first.mode, StoreMode::Hosting, "{}", first.note);
        let host_a = first.local.expect("the first instance hosts");
        assert_eq!(root_server_id(&root_a), Some(host_a.server_id()));
        assert_eq!(health_server_id(host_a.control_addr()), Some(host_a.server_id()));

        // The same app again: its store is served, so it attaches.
        let second = resolve_at("L11TESTA", &root_a, false, None);
        assert_eq!(second.mode, StoreMode::Attached, "{}", second.note);
        assert!(second.local.is_none());
        assert_eq!(second.root, root_a);
        assert!(second.note.contains("answers"), "{}", second.note);

        // Another app, hinted at app A's live server: never an answer for B.
        let other = resolve_at("L11TESTB", &root_b, false, Some(host_a.endpoints()));
        assert_eq!(other.mode, StoreMode::Hosting, "{}", other.note);
        let host_b = other.local.expect("app B hosts its own store");
        assert_ne!(host_b.server_id(), host_a.server_id());
        assert_eq!(root_server_id(&root_b), Some(host_b.server_id()));

        drop(host_b);
        drop(host_a);
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// The async form answers off the calling thread, exactly once, wakes
    /// its caller, and hands over a live host.
    /// The host half of the two-process test below: host the store the
    /// parent names, then idle until the parent kills this process.
    #[test]
    fn takeover_child_host() {
        let Some(root) = std::env::var_os("ST_TAKEOVER_ROOT") else { return };
        let resolved = resolve_at("L11TESTA", Path::new(&root), false, None);
        assert_eq!(resolved.mode, StoreMode::Hosting, "{}", resolved.note);
        // Until the parent lets go: its end of our stdin closes as it kills
        // us or dies, however long its test takes.
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
        drop(resolved);
    }

    /// Two processes: a child hosts the store, this process attaches as a
    /// client and uses it, the child is killed (no graceful shutdown), and
    /// this process hosts the same store without being asked again: its
    /// clients connect to the new host and read the same catalog.
    #[test]
    fn a_client_takes_the_store_over_when_its_host_process_dies() {
        use makepad_asset_client::{AssetClient, CatalogQuery, ClientConfig};
        let base = temp_base("takeover");
        let root = base.join("store");
        // Killed on every exit from this test, a failed assert included.
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        std::fs::create_dir_all(&base).unwrap();
        let said = base.join("child.log");
        let mut child = Child(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["embed::tests::takeover_child_host", "--exact", "--test-threads=1", "--nocapture"])
            .env("ST_TAKEOVER_ROOT", &root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(&said).unwrap())
            .spawn()
            .unwrap());
        // Up and answering, not just locked: its `listen` is written last.
        // However long its start takes (a JIT's start on a loaded box took
        // over the 30 s this wait once had, 11-24), the wait ends as it hosts
        // or as it ends, saying what it said.
        while store_served(&root, None) != Some("its server answers") {
            if let Some(status) = child.0.try_wait().unwrap() {
                panic!("the child never hosted: it ended ({status}): {}", std::fs::read_to_string(&said).unwrap_or_default());
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let attached = resolve_at("L11TESTA", &root, false, None);
        assert_eq!(attached.mode, StoreMode::Attached, "{}", attached.note);
        let id = root_server_id(&root).unwrap();
        let token = std::fs::read_to_string(root.join("admin-token")).unwrap().trim().to_string();
        let connect = |endpoints: ApiEndpoints, cache: &str| {
            let mut config = ClientConfig::new(base.join(cache));
            config.token = Some(token.clone());
            AssetClient::connect(config, endpoints, Some(id)).unwrap()
        };
        let client = connect(root_listen(&root).unwrap(), "cache-a");
        client.catalog_search(&CatalogQuery::browse(10), None).expect("the child's server answers");
        let mut watch = resolve_when_freed_notify("L11TESTA", &root, || {});
        assert!(watch.poll().is_none(), "no takeover while the host lives");

        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(client.catalog_search(&CatalogQuery::browse(10), None).is_err(), "the old host is gone");
        let took = watch.wait().unwrap();
        assert_eq!(took.mode, StoreMode::Hosting, "{}", took.note);
        let local = took.local.expect("hosted here now");
        assert_eq!(local.server_id(), id, "the same store");
        drop(client);
        let client = connect(local.endpoints(), "cache-b");
        client.catalog_search(&CatalogQuery::browse(10), None).expect("the new host answers");

        drop(client);
        drop(local);
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A takeover watch dropped before its store is freed never hosts it.
    #[test]
    fn a_dropped_takeover_watch_hosts_nothing() {
        let base = temp_base("cancel");
        let root = base.join("store");
        let host = resolve_at("L11TESTA", &root, false, None);
        assert_eq!(host.mode, StoreMode::Hosting, "{}", host.note);
        drop(resolve_when_freed_notify("L11TESTA", &root, || {}));
        drop(host);
        // Past the watcher's reaction to the freed lock.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!lock_held(&root), "nobody hosts the freed store");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn resolve_async_returns_at_once_and_answers_once() {
        let base = temp_base("async");
        let root = base.join("a/store");
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = woken.clone();
        let mut handle = resolve_at_async_notify("L11TESTA", &root, false, None, move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst)
        });
        assert_eq!(handle.root(), root.as_path());
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let resolved = loop {
            if let Some(resolved) = handle.poll() {
                break resolved;
            }
            assert!(handle.is_pending());
            assert!(std::time::Instant::now() < deadline, "the resolver never answered");
            std::thread::sleep(Duration::from_millis(10));
        };
        // The wake-up follows the answer (so a woken UI always finds it).
        while !woken.load(std::sync::atomic::Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline, "notify never ran");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!handle.is_pending());
        assert!(handle.poll().is_none(), "the answer is handed out once");
        assert_eq!(resolved.mode, StoreMode::Hosting, "{}", resolved.note);
        let local = resolved.local.expect("hosted");
        assert_eq!(health_server_id(local.control_addr()), Some(local.server_id()));

        // A second instance, blocking form over the async one: attaches.
        let second = resolve_at_async_notify("L11TESTA", &root, false, None, || {}).wait().unwrap();
        assert_eq!(second.mode, StoreMode::Attached, "{}", second.note);

        drop(local);
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A store whose `listen` went stale onto a port another store's server
    /// now answers (the ports are OS-assigned): that server reports the
    /// wrong identity, so the app hosts its own store.
    #[test]
    fn a_stale_listen_pointing_at_another_store_is_not_this_store() {
        let base = temp_base("stale");
        let root_a = base.join("a/store");
        let root_c = base.join("c/store");
        let host_a = resolve_at("L11TESTA", &root_a, false, None).local.expect("app A hosts");

        std::fs::create_dir_all(&root_c).unwrap();
        std::fs::write(root_c.join("server-id"), "00112233445566778899aabbccddeeff\n").unwrap();
        std::fs::copy(root_a.join("listen"), root_c.join("listen")).unwrap();
        assert_eq!(store_served(&root_c, None), None);

        let resolved = resolve_at("L11TESTC", &root_c, false, None);
        assert_eq!(resolved.mode, StoreMode::Hosting, "{}", resolved.note);
        let host_c = resolved.local.unwrap();
        assert_eq!(host_c.server_id(), [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);

        drop(host_c);
        drop(host_a);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
