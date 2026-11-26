//! Store sync: every store that has it on pulls the head log
//! ([`crate::sync_log`]) of the peers it names and replicates what it lacks —
//! revisions and their blobs first, then the head moves — through its own
//! catalog paths on its own state thread. Nothing on the network writes a
//! store: a peer only ever READS another.
//!
//! ## Wire (served on the control plane, reached through the fleet front)
//!
//! - `GET /v1/sync/log?since=<seq>&limit=<n>&wait=<ms>`: rows after `since`,
//!   long-polled (parked on the events hub, so an idle peer costs one parked
//!   connection and no CPU).
//! - `GET /v1/sync/revision/<arev>`: the canonical manifest, its asset's
//!   namespace and annotation.
//! - `GET /v1/sync/history?alias=<alias>`: one alias head's history.
//! - Blobs: the data plane's `GET /v1/blobs/<id>`, verified on admission.
//!
//! ## Security
//!
//! The store's planes stay on loopback. Sync adds ONE listener: the fleet
//! TLS front (`makepad_network::fleet::front`) on a private LAN address,
//! refusing to start without this machine's fleet identity
//! (`makepad_network::fleet::id`, tools/fleet-identity.md): its certificate
//! carries this machine's credential, and it admits fleet machines (a node,
//! or the admin Mac) proving their own credential for that certificate, on
//! the read routes above only. Pulls reach peers through `id::connect`,
//! which checks the peer's certificate against the fleet root. The front
//! hands requests on with a read-only token this host minted (a principal
//! with no grants).

use super::api::{parse_limit, Fail, RouteResult};
use super::config::SyncConfig;
use super::events::{self, EventBody, EventHub};
use super::http::{Conn, Head, Method, Resp};
use super::json::{self, obj, s, Value};
use super::routes::{call_state, read_state, secret_of, Outcome, RouteCtx};
use super::state::{StateCtx, StateHandle};
use super::util::{from_hex_bounded, log, now_ms, to_hex};
use crate::search::{kind_name, kind_parse};
use crate::sync_log::{self, Applying, Stamp, SyncRow, KIND_ALIAS, KIND_ASSET};
use crate::{AssetAnnotation, CandidateState, FsCas, ServerError, ServerResult, Visibility};
use makepad_asset_data::{AssetAlias, AssetId, AssetManifest, AssetRevisionId, BlobId};
use makepad_network::fleet::id;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Rows per log page (pull and serve).
const PAGE: u32 = 128;
/// How long a pull parks on an idle peer.
const PULL_WAIT_MS: u64 = 25_000;
/// The longest a sync request may take end to end (a blob is streamed).
const IO_TIMEOUT: Duration = Duration::from_secs(60);
/// Largest JSON answer a peer may send.
const MAX_JSON: usize = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// serving
// ---------------------------------------------------------------------------

/// The sync routes of the control plane (any valid token; the fleet front
/// in front of them is what decides who reaches them from the network).
pub fn dispatch(conn: &mut Conn, head: &mut Head, rc: &RouteCtx, seg: &[&str]) -> Option<RouteResult<Outcome>> {
    let read = super::routes::is_read(head.method);
    match seg {
        ["v1", "sync", "ensure"] if head.method == Method::Post => Some(ensure_route(conn, head, rc)),
        ["v1", "sync", "log"] if read => Some(log_route(head, rc)),
        ["v1", "sync", "revision", r] if read => Some(revision_route(head, rc, r)),
        ["v1", "sync", "history"] if read => Some(history_route(head, rc)),
        ["v1", "sync", ..] => Some(Err(Fail::Http(405, "method not allowed"))),
        _ => None,
    }
}

/// Which routes a fleet peer reaches through the sync front: fleet machines
/// only, the sync reads and single-blob reads, nothing else.
pub fn front_allows(peer: &id::Peer, method: &str, path: &str) -> bool {
    if !peer.machine() || !(method == "GET" || method == "HEAD") {
        return false;
    }
    let path = path.split('?').next().unwrap_or(path);
    if path == "/v1/sync/log" || path == "/v1/sync/history" {
        return true;
    }
    if let Some(rev) = path.strip_prefix("/v1/sync/revision/") {
        return !rev.is_empty() && !rev.contains('/');
    }
    if let Some(blob) = path.strip_prefix("/v1/blobs/") {
        return !blob.is_empty() && !blob.contains('/') && !matches!(blob, "fetch" | "batch" | "ref");
    }
    false
}

fn row_json(row: &SyncRow) -> Value {
    obj(vec![
        ("seq", Value::Int(row.seq as i64)),
        ("kind", Value::Int(row.kind)),
        ("key", s(row.key.clone())),
        ("asset_id", s(row.asset_id.to_string())),
        ("revision", row.revision.map(|r| s(r.to_string())).unwrap_or(Value::Null)),
        ("retired", Value::Bool(row.retired)),
        ("clock", Value::Int(row.stamp.clock)),
        ("origin", s(to_hex(&row.stamp.origin))),
        ("applied", Value::Bool(row.applied)),
    ])
}

fn parse_u64(head: &Head, key: &str) -> RouteResult<u64> {
    match head.query_get(key) {
        None => Ok(0),
        Some(t) if !t.is_empty() && t.len() <= 20 && t.bytes().all(|b| b.is_ascii_digit()) => {
            t.parse().map_err(|_| Fail::Http(400, "malformed number"))
        }
        Some(_) => Err(Fail::Http(400, "malformed number")),
    }
}

fn log_route(head: &Head, rc: &RouteCtx) -> RouteResult<Outcome> {
    let secret = secret_of(head)?;
    let since = parse_u64(head, "since")?;
    let wait_ms = parse_u64(head, "wait")?.min(rc.cfg.event_max_wait_ms);
    let limit = parse_limit(head.query_get("limit"), PAGE as u64, PAGE as u64)? as u32;
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    let mut authed = Some(secret);
    loop {
        // The events tail BEFORE the read: a change committed after the read
        // publishes its event after this cursor and wakes the wait below.
        let tail = rc.events.tail_cursor();
        let secret = authed.take();
        let now = now_ms();
        let (rows, last) = read_state(rc, |ctx| {
            if let Some(secret) = &secret {
                ctx.core.auth().authenticate(secret.as_bytes(), now)?;
            }
            let db = ctx.core.catalog().db;
            Ok((sync_log::page(db, since, limit)?, sync_log::last_seq(db)?))
        })?;
        if !rows.is_empty()
            || wait_ms == 0
            || Instant::now() >= deadline
            || !rc.events.wait_beyond(tail.seq, deadline)
        {
            return Ok(Outcome::Resp(Resp::json(
                200,
                &obj(vec![
                    ("store", s(to_hex(&rc.server_id))),
                    ("last_seq", Value::Int(last as i64)),
                    ("rows", Value::Arr(rows.iter().map(row_json).collect())),
                ]),
            )));
        }
    }
}

fn annotation_json(a: &AssetAnnotation) -> Value {
    let list = |v: &[String]| Value::Arr(v.iter().map(|t| s(t.clone())).collect());
    obj(vec![
        ("title", s(a.title.clone())),
        ("description", s(a.description.clone())),
        ("kind", a.kind.map(|k| s(kind_name(k))).unwrap_or(Value::Null)),
        ("categories", list(&a.categories)),
        ("tags", list(&a.tags)),
        ("creator", s(a.creator.clone())),
        ("artist", s(a.artist.clone())),
        ("artist_url", s(a.artist_url.clone())),
        ("album", s(a.album.clone())),
        ("source_url", s(a.source_url.clone())),
        ("license", s(a.license.clone())),
        ("license_url", s(a.license_url.clone())),
        ("generator", s(a.generator.clone())),
        ("backend", s(a.backend.clone())),
        ("model", s(a.model.clone())),
        ("prompt", s(a.prompt.clone())),
        ("provenance", s(a.provenance.clone())),
        ("visibility", s(if a.visibility == Visibility::Private { "private" } else { "public" })),
    ])
}

fn revision_route(head: &Head, rc: &RouteCtx, rev: &str) -> RouteResult<Outcome> {
    let secret = secret_of(head)?;
    let revision: AssetRevisionId = rev.parse().map_err(|_| Fail::Http(400, "malformed revision"))?;
    let now = now_ms();
    let body = read_state(rc, |ctx| {
        ctx.core.auth().authenticate(secret.as_bytes(), now)?;
        let manifest = ctx
            .core
            .catalog()
            .asset_revision_manifest(&revision)?
            .ok_or(ServerError::NotFound { what: "revision" })?;
        let asset_id = AssetManifest::from_canonical_bytes(&manifest)?.asset_id;
        let namespace = ctx.core.catalog().asset_namespace(&asset_id)?.ok_or(ServerError::NotFound { what: "asset" })?;
        let annotation = ctx.core.search().annotation(&asset_id)?;
        Ok(obj(vec![
            ("asset_id", s(asset_id.to_string())),
            ("namespace", s(namespace)),
            ("manifest", s(to_hex(&manifest))),
            ("annotation", annotation.as_ref().map(annotation_json).unwrap_or(Value::Null)),
        ]))
    })?;
    Ok(Outcome::Resp(Resp::json(200, &body)))
}

fn history_route(head: &Head, rc: &RouteCtx) -> RouteResult<Outcome> {
    let secret = secret_of(head)?;
    let alias = head.query_get("alias").ok_or(Fail::Http(400, "missing alias"))?.to_string();
    AssetAlias::new(alias.clone()).map_err(|_| Fail::Http(400, "malformed alias"))?;
    let now = now_ms();
    let rows = read_state(rc, |ctx| {
        ctx.core.auth().authenticate(secret.as_bytes(), now)?;
        sync_log::history(ctx.core.catalog().db, KIND_ALIAS, &alias, 256)
    })?;
    Ok(Outcome::Resp(Resp::json(200, &obj(vec![("rows", Value::Arr(rows.iter().map(row_json).collect()))]))))
}

/// `POST /v1/sync/ensure {"peer": "ip:port", "revision": "arev_…"}` (root
/// only, loopback: the sync front never routes a POST): pulls the revision,
/// its dependency revisions and their blobs from that peer's sync front
/// into this store, verified and published. Moves no alias. Blocks for the
/// transfer; idempotent (all zeros when everything is here already).
fn ensure_route(conn: &mut Conn, head: &mut Head, rc: &RouteCtx) -> RouteResult<Outcome> {
    let secret = secret_of(head)?;
    let body = match super::routes_control::read_json_body(conn, head, rc) {
        Err(outcome) => return Ok(outcome),
        Ok(r) => r?,
    };
    let peer_text = body.get("peer").and_then(Value::as_str).ok_or(Fail::Http(400, "missing peer"))?.to_string();
    let revision: AssetRevisionId = body
        .get("revision")
        .and_then(Value::as_str)
        .ok_or(Fail::Http(400, "missing revision"))?
        .parse()
        .map_err(|_| Fail::Http(400, "malformed revision"))?;
    let now = now_ms();
    call_state(&rc.state, move |ctx| {
        let p = ctx.core.auth().authenticate(secret.as_bytes(), now)?;
        if !ctx.is_root(&p)? {
            return Err(ServerError::Denied { capability: "auth_admin" });
        }
        Ok(())
    })?;
    let peer = Peer::parse(&peer_text).map_err(|e| Fail::Http(400, peer_refusal(e)))?;
    let pull = PullCtx {
        state: SharedState::new(rc.state.clone()),
        events: rc.events.clone(),
        cas: rc.cas.clone(),
        server_id: rc.server_id,
        log: rc.cfg.log,
        exclude: Arc::new(Vec::new()),
    };
    let mut stats = EnsureStats::default();
    let mut seen = std::collections::HashSet::new();
    match ensure_revision(&peer, &pull, revision, &mut seen, &mut stats) {
        Ok(()) => {}
        Err(e) => {
            log(rc.cfg.log, &format!("store sync: ensure {revision} from {}: {e}", peer.label));
            return Ok(Outcome::Resp(Resp::error(502, &e)));
        }
    }
    let asset = call_state(&rc.state, move |ctx| {
        let bytes = ctx.core.catalog().asset_revision_manifest(&revision)?.ok_or(ServerError::NotFound { what: "revision" })?;
        Ok(AssetManifest::from_canonical_bytes(&bytes)?.asset_id)
    })?;
    Ok(Outcome::Resp(Resp::json(
        200,
        &obj(vec![
            ("revision", s(revision.to_string())),
            ("asset_id", s(asset.to_string())),
            ("revisions_fetched", Value::Int(stats.revisions as i64)),
            ("blobs_fetched", Value::Int(stats.blobs as i64)),
            ("bytes", Value::Int(stats.bytes as i64)),
        ]),
    )))
}

/// A refusal text for a static-str error slot.
fn peer_refusal(text: String) -> &'static str {
    match text.as_str() {
        t if t.contains("not a private LAN") => "peer is not a private LAN address",
        _ => "peer is not ip:port",
    }
}

/// What one ensure moved.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnsureStats {
    pub revisions: u32,
    pub blobs: u32,
    pub bytes: u64,
}

// ---------------------------------------------------------------------------
// the front and the pullers
// ---------------------------------------------------------------------------

/// Sync's running parts, owned by the [`super::AssetServer`].
pub struct SyncHost {
    front: makepad_network::fleet::front::FrontHandle,
    stop: Arc<Stop>,
    state: SharedState,
}

/// The pullers' way to the state thread, revoked on stop: a puller parked in
/// a peer's long-poll holds no state handle, so stopping never waits for it
/// (it finds the handle gone and exits after its current request).
#[derive(Clone)]
pub struct SharedState(Arc<Mutex<Option<StateHandle>>>);

impl SharedState {
    pub fn new(state: StateHandle) -> SharedState {
        SharedState(Arc::new(Mutex::new(Some(state))))
    }

    fn get(&self) -> Option<StateHandle> {
        self.0.lock().unwrap().clone()
    }

    fn revoke(&self) {
        self.0.lock().unwrap().take();
    }
}

/// The pullers' stop flag, with a wake-up for their back-off waits.
struct Stop {
    flag: AtomicBool,
    lock: Mutex<()>,
    cv: Condvar,
}

impl Stop {
    fn stopped(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Sleeps up to `d`; `true` when stopped meanwhile.
    fn sleep(&self, d: Duration) -> bool {
        let guard = self.lock.lock().unwrap();
        let _ = self.cv.wait_timeout_while(guard, d, |_| !self.stopped());
        self.stopped()
    }
}

impl SyncHost {
    pub fn addr(&self) -> SocketAddr {
        self.front.local_addr()
    }

    pub fn stop(&mut self) {
        self.stop.flag.store(true, Ordering::SeqCst);
        self.stop.cv.notify_all();
        self.state.revoke();
        self.front.stop();
    }
}

/// What a puller needs from its host.
#[derive(Clone)]
pub struct PullCtx {
    pub state: SharedState,
    pub events: Arc<EventHub>,
    pub cas: Arc<FsCas>,
    pub server_id: [u8; 16],
    pub log: bool,
    pub exclude: Arc<Vec<String>>,
}

/// Starts the front and one puller per peer. Refuses (and starts nothing)
/// without this machine's fleet identity or on a non-LAN address.
pub fn start(
    cfg: &SyncConfig,
    pull: PullCtx,
    control: SocketAddr,
    data: SocketAddr,
    inner_token: String,
) -> ServerResult<SyncHost> {
    use makepad_network::fleet::{front, lan};
    let refuse = |what: String| {
        log(true, &format!("store sync: {what}"));
        ServerError::InvalidState { what: "store sync", state: "refused (see log)" }
    };
    lan::check_lan_bind("store sync", cfg.listen).map_err(refuse)?;
    let (identity, verifier) = id::server_identity("makepad-store-sync").map_err(refuse)?;
    let fingerprint = identity.fingerprint_hex();
    let tls = makepad_network::tls::TlsServer::new(identity).map_err(|e| refuse(format!("TLS server: {e}")))?;
    let mut secret = [0u8; 32];
    makepad_network::tls::os_random(&mut secret).map_err(|e| refuse(format!("random: {e}")))?;
    let handle = front::start_front(front::FrontConfig {
        listen: cfg.listen,
        inner: Arc::new(move |path: &str| if path.starts_with("/v1/blobs/") { data } else { control }),
        inner_headers: vec![format!("Authorization: Bearer {inner_token}")],
        tls,
        verifier: Arc::new(verifier),
        front_secret: to_hex(&secret),
        policy: Arc::new(|peer: &id::Peer, method: &str, path: &str| front_allows(peer, method, path)),
        max_total: front::LAN_MAX_TOTAL,
        max_per_ip: front::LAN_MAX_PER_IP,
        label: "store-sync",
    })
    .map_err(|e| refuse(format!("bind {}: {e}", cfg.listen)))?;
    log(
        pull.log,
        &format!("store sync: serving on {} (certificate {fingerprint}), pulling from {}", handle.local_addr(), cfg.peers.join(", ")),
    );
    let stop = Arc::new(Stop { flag: AtomicBool::new(false), lock: Mutex::new(()), cv: Condvar::new() });
    let peers: Vec<Peer> = cfg.peers.iter().map(|p| Peer::parse(p)).collect::<Result<_, _>>().map_err(refuse)?;
    for peer in peers {
        let pull = pull.clone();
        let stop = stop.clone();
        // Detached: it exits on its own once stopped (see SharedState).
        std::thread::Builder::new()
            .name(format!("store-sync-{}", peer.label))
            .spawn(move || pull_loop(&peer, &pull, &stop))
            .map_err(|e| ServerError::Io { op: "spawn sync puller", kind: e.kind() })?;
    }
    Ok(SyncHost { front: handle, stop, state: pull.state })
}

// ---------------------------------------------------------------------------
// a fleet HTTPS client for the pulls
// ---------------------------------------------------------------------------

struct Peer {
    host: String,
    port: String,
    label: String,
}

impl Peer {
    fn parse(text: &str) -> Result<Peer, String> {
        let addr: SocketAddr = text.trim().parse().map_err(|_| format!("peer '{text}' is not ip:port"))?;
        if !makepad_network::fleet::lan::is_private_lan(addr.ip()) {
            return Err(format!("peer {addr} is not a private LAN address"));
        }
        let host = match addr.ip() {
            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
            ip => ip.to_string(),
        };
        let port = addr.port().to_string();
        let label = format!("{host}:{port}");
        makepad_network::tls::mark_fleet_endpoint(&label);
        Ok(Peer { host, port, label })
    }

    /// One GET; the body goes to `sink` as it arrives. Returns the status.
    fn get(&self, path: &str, sink: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<u16, String> {
        let (mut stream, _server, authorization) = id::connect(&self.host, &self.port).map_err(|e| format!("connect: {e}"))?;
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: {authorization}\r\nConnection: close\r\n\r\n",
            self.label
        );
        stream.write_all(request.as_bytes()).map_err(|e| format!("send: {e}"))?;
        let mut buf = Vec::with_capacity(16 * 1024);
        let mut chunk = vec![0u8; 64 * 1024];
        let head_end = loop {
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break end;
            }
            if buf.len() > 64 * 1024 {
                return Err("response head too large".into());
            }
            let n = stream.read(&mut chunk).map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                return Err("connection closed before a response".into());
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| "response head not UTF-8")?.to_string();
        let mut rest = buf.split_off(head_end + 4);
        let mut lines = head.split("\r\n");
        let status: u16 = lines
            .next()
            .and_then(|l| l.split(' ').nth(1))
            .and_then(|c| c.parse().ok())
            .ok_or("malformed status line")?;
        let mut length: Option<u64> = None;
        let mut chunked = false;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                let name = name.trim().to_ascii_lowercase();
                if name == "content-length" {
                    length = value.trim().parse().ok();
                } else if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
                    chunked = true;
                }
            }
        }
        let mut read_more = |rest: &mut Vec<u8>| -> Result<bool, String> {
            let n = stream.read(&mut chunk).map_err(|e| format!("read: {e}"))?;
            rest.extend_from_slice(&chunk[..n]);
            Ok(n > 0)
        };
        if chunked {
            loop {
                let line_end = loop {
                    if let Some(p) = rest.windows(2).position(|w| w == b"\r\n") {
                        break p;
                    }
                    if !read_more(&mut rest)? {
                        return Err("truncated chunked body".into());
                    }
                };
                let size_text = std::str::from_utf8(&rest[..line_end]).map_err(|_| "bad chunk size")?;
                let size = u64::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
                    .map_err(|_| "bad chunk size")? as usize;
                rest.drain(..line_end + 2);
                if size == 0 {
                    return Ok(status);
                }
                while rest.len() < size + 2 {
                    if !read_more(&mut rest)? {
                        return Err("truncated chunk".into());
                    }
                }
                sink(&rest[..size])?;
                rest.drain(..size + 2);
            }
        }
        let mut seen = 0u64;
        loop {
            if !rest.is_empty() {
                let take = match length {
                    Some(len) => rest.len().min((len - seen) as usize),
                    None => rest.len(),
                };
                sink(&rest[..take])?;
                seen += take as u64;
                rest.clear();
            }
            if length.is_some_and(|len| seen >= len) {
                return Ok(status);
            }
            if !read_more(&mut rest)? {
                return match length {
                    Some(len) if seen < len => Err("truncated body".into()),
                    _ => Ok(status),
                };
            }
        }
    }

    fn get_json(&self, path: &str) -> Result<Value, String> {
        let mut body = Vec::new();
        let status = self.get(path, &mut |b| {
            if body.len() + b.len() > MAX_JSON {
                return Err("answer too large".into());
            }
            body.extend_from_slice(b);
            Ok(())
        })?;
        let value = json::parse(&body).map_err(|e| format!("{path}: bad JSON ({e})"))?;
        if status != 200 {
            let why = value.get("error").and_then(Value::as_str).unwrap_or("").to_string();
            return Err(format!("{path}: HTTP {status} {why}"));
        }
        Ok(value)
    }
}

// ---------------------------------------------------------------------------
// pulling
// ---------------------------------------------------------------------------

fn state_call<T: Send + 'static>(
    state: &SharedState,
    f: impl FnOnce(&mut StateCtx) -> ServerResult<T> + Send + 'static,
) -> Result<T, String> {
    let Some(state) = state.get() else { return Err("store sync stopped".into()) };
    match state.call(f) {
        None => Err("state thread unavailable".into()),
        Some(r) => r.map_err(|e| e.to_string()),
    }
}

fn parse_row(v: &Value) -> Result<SyncRow, String> {
    let str_of = |k: &str| v.get(k).and_then(Value::as_str).ok_or_else(|| format!("row: missing {k}"));
    let origin_hex = str_of("origin")?;
    let origin_bytes = from_hex_bounded(origin_hex, 16).ok_or("row: bad origin")?;
    let origin: [u8; 16] = origin_bytes.as_slice().try_into().map_err(|_| "row: bad origin")?;
    let revision = match v.get("revision") {
        None | Some(Value::Null) => None,
        Some(r) => Some(r.as_str().ok_or("row: bad revision")?.parse::<AssetRevisionId>().map_err(|_| "row: bad revision")?),
    };
    let kind = v.get("kind").and_then(Value::as_i64).ok_or("row: missing kind")?;
    if kind != KIND_ALIAS && kind != KIND_ASSET {
        return Err(format!("row: unknown kind {kind}"));
    }
    Ok(SyncRow {
        seq: v.get("seq").and_then(Value::as_u64).ok_or("row: missing seq")?,
        kind,
        key: str_of("key")?.to_string(),
        asset_id: str_of("asset_id")?.parse().map_err(|_| "row: bad asset id")?,
        revision,
        retired: v.get("retired").and_then(Value::as_bool).unwrap_or(false),
        stamp: Stamp { clock: v.get("clock").and_then(Value::as_i64).ok_or("row: missing clock")?, origin },
        applied: v.get("applied").and_then(Value::as_bool).unwrap_or(true),
    })
}

fn pull_loop(peer: &Peer, pull: &PullCtx, stop: &Stop) {
    let mut backoff = Duration::from_secs(1);
    let peer_key = peer.label.clone();
    while !stop.stopped() {
        match pull_once(peer, pull, &peer_key) {
            Ok(moved) => {
                backoff = Duration::from_secs(1);
                // Printed inside apps too: rare once caught up, and the
                // one line that says the fleet's content arrived.
                if moved > 0 {
                    log(true, &format!("store sync: {moved} change(s) from {}", peer.label));
                }
            }
            Err(_) if stop.stopped() => return,
            Err(error) => {
                let key = peer_key.clone();
                let note = error.clone();
                let _ = state_call(&pull.state, move |ctx| {
                    let db = ctx.core.catalog().db;
                    let mut st = sync_log::peer_state(db, &key)?;
                    st.last_error = Some(note);
                    sync_log::set_peer_state(db, &key, &st)
                });
                log(pull.log, &format!("store sync: {}: {error} (retry in {} s)", peer.label, backoff.as_secs()));
                if stop.sleep(backoff) {
                    return;
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// One long-poll page from the peer, applied. Returns how many rows moved
/// something here.
fn pull_once(peer: &Peer, pull: &PullCtx, peer_key: &str) -> Result<usize, String> {
    let key = peer_key.to_string();
    let mut st = state_call(&pull.state, move |ctx| sync_log::peer_state(ctx.core.catalog().db, &key))?;
    let page = peer.get_json(&format!("/v1/sync/log?since={}&limit={PAGE}&wait={PULL_WAIT_MS}", st.cursor))?;
    let store_hex = page.get("store").and_then(Value::as_str).ok_or("log: missing store")?;
    let store: [u8; 16] = from_hex_bounded(store_hex, 16)
        .and_then(|b| b.as_slice().try_into().ok())
        .ok_or("log: bad store id")?;
    if store == pull.server_id {
        return Err("that peer is this store".into());
    }
    if st.peer_store != Some(store) {
        // Another store answers there now: its seqs are another log's.
        st.cursor = 0;
        st.peer_store = Some(store);
    }
    let rows = page.get("rows").and_then(Value::as_arr).ok_or("log: missing rows")?;
    let rows: Vec<SyncRow> = rows.iter().map(parse_row).collect::<Result<_, _>>()?;
    // The page's objects first, in parallel (they are independent and
    // content-verified); then the heads, one by one in log order.
    prefetch(peer, pull, &rows)?;
    let mut moved = 0;
    for row in &rows {
        if apply_row(peer, pull, row)? {
            moved += 1;
        }
        st.cursor = row.seq;
    }
    st.last_ok_ms = Some(now_ms());
    st.last_error = None;
    let key = peer_key.to_string();
    state_call(&pull.state, move |ctx| sync_log::set_peer_state(ctx.core.catalog().db, &key, &st))?;
    Ok(moved)
}

/// Parallel fetchers per page: the bootstrap of a fresh store is bound by
/// round trips (a TLS connection per object), not by bandwidth.
const FETCHERS: usize = 8;

/// Makes every revision the page names present here (with its blobs),
/// FETCHERS at a time. The first error stops the page (it is retried).
fn prefetch(peer: &Peer, pull: &PullCtx, rows: &[SyncRow]) -> Result<(), String> {
    let wanted: Vec<AssetRevisionId> =
        rows.iter().filter(|r| !excluded(pull, r)).filter_map(|r| r.revision).collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..FETCHERS.min(wanted.len()) {
            scope.spawn(|| loop {
                if failed.lock().unwrap().is_some() {
                    return;
                }
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(revision) = wanted.get(i) else { return };
                let mut seen = std::collections::HashSet::new();
                if let Err(e) = ensure_revision(peer, pull, *revision, &mut seen, &mut EnsureStats::default()) {
                    failed.lock().unwrap().get_or_insert(e);
                    return;
                }
            });
        }
    });
    match failed.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn excluded(pull: &PullCtx, row: &SyncRow) -> bool {
    row.kind == KIND_ALIAS && pull.exclude.iter().any(|p| row.key.starts_with(p.as_str()))
}

/// Brings one row in. `Ok(true)` when it changed a head here.
fn apply_row(peer: &Peer, pull: &PullCtx, row: &SyncRow) -> Result<bool, String> {
    if excluded(pull, row) {
        return Ok(false);
    }
    let probe = row.clone();
    let known = state_call(&pull.state, move |ctx| {
        sync_log::has_row(ctx.core.catalog().db, probe.kind, &probe.key, &probe.stamp)
    })?;
    if known {
        return Ok(false);
    }
    // Every revision a row names is kept here, winner or not (history).
    if let Some(revision) = row.revision {
        let mut seen = std::collections::HashSet::new();
        ensure_revision(peer, pull, revision, &mut seen, &mut EnsureStats::default())?;
    }
    let row = row.clone();
    let hub = pull.events.clone();
    state_call(&pull.state, move |ctx| apply_in_state(ctx, &hub, &row))
}

/// Decides and applies one row on the state thread: the newest stamp is the
/// head, every other row stays as history.
fn apply_in_state(ctx: &mut StateCtx, hub: &EventHub, row: &SyncRow) -> ServerResult<bool> {
    let db = ctx.core.catalog().db;
    if sync_log::has_row(db, row.kind, &row.key, &row.stamp)? {
        return Ok(false);
    }
    let wins = sync_log::head_stamp(db, row.kind, &row.key)?.is_none_or(|head| row.stamp > head);
    if !wins {
        sync_log::record_history(db, row)?;
        return Ok(false);
    }
    let now = now_ms();
    if row.kind == KIND_ASSET {
        return apply_asset_state(ctx, hub, row, now);
    }
    let alias = AssetAlias::new(row.key.clone())?;
    let current = ctx.core.catalog().resolve_asset_alias(&alias)?;
    match row.target() {
        Some(target) => {
            if current == Some(target) {
                sync_log::record_head(db, row)?;
                return Ok(false);
            }
            let live = ctx.core.catalog().asset_retired_ms(&target.asset_id)?.is_none()
                && ctx.core.catalog().asset_candidate_state(&target.asset_id, &target.revision)?
                    == Some(CandidateState::Published);
            if !live {
                // A head on an asset this store holds deleted: kept as
                // history; a restore brings the asset (and its heads) back.
                sync_log::record_history(db, row)?;
                return Ok(false);
            }
            {
                let _applying = Applying::new(row.stamp);
                ctx.core.catalog().set_asset_alias(&alias, &target, now)?;
            }
            hub.publish(
                EventBody::asset(events::KIND_ALIAS_SET, alias.namespace(), target.asset_id.to_string(), now)
                    .with_revision(target.revision.to_string())
                    .with_alias(alias.as_str().to_string())
                    .with_content_kind(content_kind(ctx, &target.asset_id)),
            );
            Ok(true)
        }
        None => {
            let Some(old) = current else {
                sync_log::record_head(db, row)?;
                return Ok(false);
            };
            {
                let _applying = Applying::new(row.stamp);
                ctx.core.catalog().clear_asset_alias(&alias, now)?;
            }
            hub.publish(
                EventBody::asset(events::KIND_ALIAS_CLEARED, alias.namespace(), old.asset_id.to_string(), now)
                    .with_revision(old.revision.to_string())
                    .with_alias(alias.as_str().to_string())
                    .with_content_kind(content_kind(ctx, &old.asset_id)),
            );
            Ok(true)
        }
    }
}

fn content_kind(ctx: &StateCtx, asset: &AssetId) -> Option<&'static str> {
    ctx.core.search().annotation(asset).ok().flatten().and_then(|a| a.kind).map(kind_name)
}

/// A delete (retire) or restore of a whole asset, as the routes do it.
fn apply_asset_state(ctx: &mut StateCtx, hub: &EventHub, row: &SyncRow, now: u64) -> ServerResult<bool> {
    let db = ctx.core.catalog().db;
    let asset = row.asset_id;
    let Some(ns) = ctx.core.catalog().asset_namespace(&asset)? else {
        // Never seen here: nothing to hide or bring back.
        sync_log::record_head(db, row)?;
        return Ok(false);
    };
    let retired_here = ctx.core.catalog().asset_retired_ms(&asset)?.is_some();
    if row.retired == retired_here {
        sync_log::record_head(db, row)?;
        return Ok(false);
    }
    if row.retired {
        let kind = content_kind(ctx, &asset);
        let report = {
            let _applying = Applying::new(row.stamp);
            ctx.core.catalog().retire_asset(&asset, now)?
        };
        ctx.asset_mark_retired(asset.as_bytes(), now)?;
        if !report.already_retired {
            hub.publish(EventBody::asset(events::KIND_ASSET_RETIRED, &ns, asset.to_string(), now).with_content_kind(kind));
        }
        return Ok(true);
    }
    let report = {
        let _applying = Applying::new(row.stamp);
        ctx.core.catalog().restore_asset(&asset, now, &mut |_| Ok(()))?
    };
    for restored in &report.restored {
        ctx.asset_mark_live(restored.asset_id.as_bytes())?;
        let row_ns = ctx.core.catalog().asset_namespace(&restored.asset_id)?.unwrap_or_default();
        let kind = content_kind(ctx, &restored.asset_id);
        if let Some(rev) = restored.revisions.last() {
            hub.publish(
                EventBody::asset(events::KIND_ASSET_PUBLISHED, &row_ns, restored.asset_id.to_string(), now)
                    .with_revision(rev.to_string())
                    .with_content_kind(kind),
            );
        }
        if restored.annotation_restored {
            hub.publish(EventBody::asset(events::KIND_ANNOTATION_SET, &row_ns, restored.asset_id.to_string(), now).with_content_kind(kind));
        }
    }
    Ok(true)
}

fn parse_annotation(v: &Value) -> Result<AssetAnnotation, String> {
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let list = |k: &str| {
        v.get(k)
            .and_then(Value::as_arr)
            .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    Ok(AssetAnnotation {
        title: text("title"),
        description: text("description"),
        kind: match v.get("kind").and_then(Value::as_str) {
            None => None,
            Some(k) => Some(kind_parse(k).ok_or_else(|| format!("annotation: unknown kind {k}"))?),
        },
        categories: list("categories"),
        tags: list("tags"),
        creator: text("creator"),
        artist: text("artist"),
        artist_url: text("artist_url"),
        album: text("album"),
        source_url: text("source_url"),
        license: text("license"),
        license_url: text("license_url"),
        owner: None,
        generator: text("generator"),
        backend: text("backend"),
        model: text("model"),
        prompt: text("prompt"),
        provenance: text("provenance"),
        visibility: if text("visibility") == "private" { Visibility::Private } else { Visibility::Public },
    })
}

/// Makes `revision` (and every revision it depends on) published here, with
/// its blobs, fetched from the peer.
fn ensure_revision(
    peer: &Peer,
    pull: &PullCtx,
    revision: AssetRevisionId,
    seen: &mut std::collections::HashSet<AssetRevisionId>,
    stats: &mut EnsureStats,
) -> Result<(), String> {
    if !seen.insert(revision) {
        return Ok(());
    }
    let have = state_call(&pull.state, move |ctx| {
        Ok(match ctx.core.catalog().asset_revision_manifest(&revision)? {
            None => false,
            Some(bytes) => {
                let asset = AssetManifest::from_canonical_bytes(&bytes)?.asset_id;
                ctx.core.catalog().asset_candidate_state(&asset, &revision)?.is_some()
            }
        })
    })?;
    if have {
        return Ok(());
    }
    let doc = peer.get_json(&format!("/v1/sync/revision/{revision}"))?;
    let manifest_hex = doc.get("manifest").and_then(Value::as_str).ok_or("revision: missing manifest")?;
    let manifest_bytes = from_hex_bounded(manifest_hex, MAX_JSON).ok_or("revision: bad manifest hex")?;
    if AssetRevisionId::hash_of(&manifest_bytes) != revision {
        return Err(format!("revision {revision}: the manifest does not hash to it"));
    }
    let manifest = AssetManifest::from_canonical_bytes(&manifest_bytes).map_err(|e| format!("revision {revision}: {e}"))?;
    let namespace = doc.get("namespace").and_then(Value::as_str).ok_or("revision: missing namespace")?.to_string();
    let annotation = match doc.get("annotation") {
        None | Some(Value::Null) => None,
        Some(a) => Some(parse_annotation(a)?),
    };
    for dep in &manifest.dependencies {
        ensure_revision(peer, pull, dep.revision, seen, stats)?;
    }
    let mut blobs: Vec<(BlobId, u64)> = manifest.files.iter().map(|f| (f.blob, f.byte_len)).collect();
    if let Some(t) = &manifest.thumbnail {
        blobs.push((t.blob, t.byte_len));
    }
    blobs.sort();
    blobs.dedup();
    let wanted = blobs.clone();
    let missing: Vec<(BlobId, u64)> = state_call(&pull.state, move |ctx| {
        let mut out = Vec::new();
        for (blob, size) in wanted {
            if !ctx.core.catalog().has_blob(&blob)? {
                out.push((blob, size));
            }
        }
        Ok(out)
    })?;
    let mut fetched = Vec::with_capacity(missing.len());
    for (blob, _) in &missing {
        let mut writer = pull.cas.begin().map_err(|e| e.to_string())?;
        let mut write_error = None;
        let status = peer.get(&format!("/v1/blobs/{blob}"), &mut |bytes| {
            writer.write(bytes).map_err(|e| {
                let text = e.to_string();
                write_error = Some(text.clone());
                text
            })
        })?;
        if status != 200 {
            return Err(format!("blob {blob}: HTTP {status}"));
        }
        if let Some(e) = write_error {
            return Err(e);
        }
        // Committed under the digest the manifest names, or refused.
        let commit = pull.cas.commit(writer, Some(*blob)).map_err(|e| format!("blob {blob}: {e}"))?;
        stats.blobs += 1;
        stats.bytes += commit.size;
        fetched.push((commit.blob_id, commit.size));
    }
    stats.revisions += 1;
    let item_manifest = manifest_bytes.clone();
    let hub = pull.events.clone();
    let asset_id = manifest.asset_id;
    state_call(&pull.state, move |ctx| {
        let now = now_ms();
        if !fetched.is_empty() {
            ctx.core.record_blobs(&fetched, now)?;
        }
        // The annotation travels with the asset; one already here stays
        // (a peer row that wins moves only the head).
        let local = ctx.core.search().annotation(&asset_id)?;
        let owner = ctx.root_admin_get()?;
        let annotation = match (local, annotation) {
            (Some(local), _) => local,
            (None, Some(mut remote)) => {
                remote.owner = owner;
                remote
            }
            (None, None) => AssetAnnotation {
                title: asset_id.to_string(),
                description: String::new(),
                kind: Some(manifest.kind),
                categories: Vec::new(),
                tags: Vec::new(),
                creator: String::new(),
                artist: String::new(),
                artist_url: String::new(),
                album: String::new(),
                source_url: String::new(),
                license: String::new(),
                license_url: String::new(),
                owner,
                generator: String::new(),
                backend: String::new(),
                model: String::new(),
                prompt: String::new(),
                provenance: String::new(),
                visibility: Visibility::Public,
            },
        };
        let kind = annotation.kind.map(kind_name);
        let outcome = ctx.core.publish_batch(
            &[crate::PublishBatchItem { namespace: namespace.clone(), manifest_bytes: item_manifest, annotation, alias: None }],
            now,
        )?;
        ctx.tdb.tx(|_| ctx.asset_index_insert(asset_id.as_bytes(), &namespace, now))?;
        if let Some(o) = outcome.first() {
            if !o.already_published {
                hub.publish(EventBody::asset(events::KIND_ANNOTATION_SET, &namespace, asset_id.to_string(), now).with_content_kind(kind));
                hub.publish(
                    EventBody::asset(events::KIND_ASSET_PUBLISHED, &namespace, asset_id.to_string(), now)
                        .with_revision(o.revision.to_string())
                        .with_content_kind(kind),
                );
            }
        }
        Ok(())
    })
}

/// The read-only principal the sync front's requests run as: no grants, so
/// every write route refuses it even if a policy ever let one through.
pub fn mint_inner_token(state: &StateHandle, ttl_ms: u64) -> ServerResult<String> {
    let now = now_ms();
    let principal = crate::PrincipalId(super::util::rand16()?);
    let secret = format!("{}{}", super::api::TOKEN_PREFIX, to_hex(&super::util::rand32()?));
    let hash = crate::token_hash(secret.as_bytes());
    state
        .call(move |ctx| -> ServerResult<()> {
            // One principal per store, kept across restarts; a fresh token
            // per process life.
            let mut st = ctx.tdb.prepare("sync principal get", "SELECT value FROM transport_meta WHERE key='sync_principal'")?;
            let known = if st.step()? { Some(crate::catalog::fixed16(&st.column_blob(0), "sync principal")?) } else { None };
            drop(st);
            let principal = match known {
                Some(id) => crate::PrincipalId(id),
                None => {
                    ctx.core.auth().create_principal(&principal, "store-sync", now)?;
                    let mut st = ctx.tdb.prepare(
                        "sync principal set",
                        "INSERT INTO transport_meta(key, value) VALUES('sync_principal', ?1)",
                    )?;
                    st.bind_blob(1, &principal.0)?;
                    st.run()?;
                    principal
                }
            };
            ctx.core.auth().register_token(&principal, &hash, now + ttl_ms, now)
        })
        .ok_or(ServerError::InvalidState { what: "state thread", state: "unavailable" })??;
    Ok(secret)
}

/// The store's identity in its log (a copied catalog takes this machine's).
pub fn set_origin(state: &StateHandle, server_id: [u8; 16]) -> ServerResult<()> {
    state
        .call(move |ctx| ctx.core.catalog().db.tx(|db| sync_log::set_origin(db, &server_id)))
        .ok_or(ServerError::InvalidState { what: "state thread", state: "unavailable" })?
}

/// What `makepad inspect`-style status lines show about the pulls.
pub fn status(state: &StateHandle, peers: &[String]) -> Vec<String> {
    let peers = peers.to_vec();
    state
        .call(move |ctx| -> ServerResult<Vec<String>> {
            let db = ctx.core.catalog().db;
            let mut out = Vec::new();
            for peer in &peers {
                let st = sync_log::peer_state(db, peer)?;
                out.push(format!(
                    "{peer}: cursor {}, last ok {}, {}",
                    st.cursor,
                    st.last_ok_ms.map(|m| format!("{} s ago", now_ms().saturating_sub(m) / 1000)).unwrap_or("never".into()),
                    st.last_error.unwrap_or_else(|| "ok".into())
                ));
            }
            Ok(out)
        })
        .and_then(Result::ok)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_front_reaches_only_sync_reads() {
        use makepad_network::fleet::id::{Peer, Role};
        let peer = |role| Peer { name: "n".into(), role, addr: "192.0.2.57".parse().unwrap(), from: None, point: [4; 65], this_machine: false };
        for (role, method, path, ok) in [
            (Role::Node, "GET", "/v1/sync/log?since=3&wait=100", true),
            (Role::Device, "GET", "/v1/sync/revision/arev_x", true),
            (Role::Node, "GET", "/v1/sync/history?alias=sandbox/games/a", true),
            (Role::Node, "GET", "/v1/blobs/blob_abc", true),
            (Role::Paired, "GET", "/v1/sync/log", false),
            (Role::Node, "POST", "/v1/sync/log", false),
            (Role::Node, "POST", "/v1/blobs", false),
            (Role::Node, "GET", "/v1/blobs/fetch", false),
            (Role::Device, "PUT", "/v1/aliases/sandbox/games/a", false),
            (Role::Device, "GET", "/v1/assets", false),
            (Role::Node, "GET", "/v1/auth/whoami", false),
            (Role::Node, "POST", "/v1/assets/ast_x/retire", false),
            (Role::Node, "GET", "/v1/sync/revision/a/b", false),
        ] {
            assert_eq!(front_allows(&peer(role), method, path), ok, "{role:?} {method} {path}");
        }
    }
}
