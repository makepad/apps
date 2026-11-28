//! The assembled runtime and its embedding boundary.
//!
//! `AssetServer::start(config)` brings up a complete standalone service —
//! state thread over the headless core, bounded HTTP control and data
//! planes, lease janitor, optional UDP discovery beacon — and returns a
//! handle whose `shutdown()` (also run on drop) tears everything down and
//! joins every thread. Nothing is global: embedders (the Asset Store, tests)
//! run several instances in one process on distinct roots.
//!
//! Single-writer law: one process serves one root. A `server.lock` advisory
//! file lock enforces it — the transport's job-routing metadata assumes it is
//! the only enqueuer, and two processes over one WAL catalog would be two
//! sources of recovery truth.

use super::config::ServerConfig;
use super::discovery::load_or_create_server_id;
use super::http::{Conn, HeadError, Method, Resp};
use super::routes::{serve_request, Outcome, Plane, RouteCtx};
use super::state::{spawn_state, StateHandle};
use super::util::{log, now_ms, rand32, to_hex};
use crate::{
    token_hash, Capability, PrincipalId, RecoverReport, Scope, ServerError, ServerResult,
};
use std::fs::File;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct AssetServer {
    control_addr: SocketAddr,
    data_addr: SocketAddr,
    /// Connections accepted per plane since start. A keep-alive client
    /// serves many requests per connection, so this is how an operator (and
    /// the tests) can SEE that reuse is really happening rather than assume
    /// it: thirty thumbnails should cost one accept, not thirty.
    accepted: [Arc<std::sync::atomic::AtomicU64>; 2],
    /// Requests served per plane since start (see `RouteCtx::requests`).
    requests: [Arc<std::sync::atomic::AtomicU64>; 2],
    server_id: [u8; 16],
    recover: RecoverReport,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
    janitor: Option<(mpsc::Sender<()>, JoinHandle<()>)>,
    state: Option<StateHandle>,
    state_join: Option<JoinHandle<()>>,
    events: Arc<super::events::EventHub>,
    /// Store sync's front and pullers, when configured.
    sync: Option<super::sync::SyncHost>,
    /// Why sync is off although it was configured.
    sync_error: Option<String>,
    /// Held for the server's whole life; the OS releases it on process death.
    _root_lock: File,
    log_enabled: bool,
}

impl AssetServer {
    pub fn start(cfg: ServerConfig) -> ServerResult<AssetServer> {
        Self::start_inner(
            cfg,
            #[cfg(test)]
            None,
        )
    }

    fn start_inner(
        cfg: ServerConfig,
        #[cfg(test)] mut control_probe: Option<tests::ShutdownProbe>,
    ) -> ServerResult<AssetServer> {
        cfg.validate()?;
        std::fs::create_dir_all(&cfg.root)
            .map_err(|e| ServerError::Io { op: "create server root", kind: e.kind() })?;
        let root_lock = lock_root(&cfg)?;
        super::draft_spool::DraftFile::recover(&cfg.root)?;

        let (state, recover, state_join) =
            spawn_state(cfg.root.clone(), cfg.budgets)?;
        if cfg.bootstrap_admin {
            bootstrap_admin(&state, &cfg)?;
        }
        let server_id = load_or_create_server_id(&cfg.root)?;
        // The head log stamps this store's changes with its own identity (a
        // catalog copied from another machine takes this one's).
        super::sync::set_origin(&state, server_id)?;

        let control =
            bind(cfg.control_addr).map_err(|e| ServerError::Io { op: "bind control plane", kind: e.kind() })?;
        let data =
            bind(cfg.data_addr).map_err(|e| ServerError::Io { op: "bind data plane", kind: e.kind() })?;
        let control_addr = control
            .local_addr()
            .map_err(|e| ServerError::Io { op: "control local addr", kind: e.kind() })?;
        let data_addr = data
            .local_addr()
            .map_err(|e| ServerError::Io { op: "data local addr", kind: e.kind() })?;

        let stop = Arc::new(AtomicBool::new(false));
        let cfg = Arc::new(cfg);
        // The journal epoch is fresh randomness per process life: a resume
        // cursor from a previous life can only ever produce an explicit
        // `gap`, never silently alias onto this life's sequences.
        let epoch: [u8; 8] = rand32()?[..8].try_into().expect("8 bytes");
        let events = Arc::new(super::events::EventHub::new(
            epoch,
            cfg.event_journal_cap,
            cfg.event_max_waiters,
            cfg.preview_meta,
            cfg.log,
        ));
        let requests: [Arc<std::sync::atomic::AtomicU64>; 2] = [
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
        ];
        let rc = RouteCtx {
            requests: requests[0].clone(),
            state: state.clone(),
            cfg: cfg.clone(),
            server_id,
            events: events.clone(),
            cas: std::sync::Arc::new(crate::FsCas::open(
                &cfg.root.join("cas"),
                &cfg.budgets,
            )?),
        };

        // One acceptor per plane; each connection gets its own thread up to
        // the plane's hard cap. The acceptor joins its connection threads
        // before exiting, so shutdown only has to join the acceptors.
        let mut workers = Vec::with_capacity(2);
        let accepted: [Arc<std::sync::atomic::AtomicU64>; 2] = [
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
        ];
        for (n, (plane, listener, max_conns)) in [
            (Plane::Control, control, cfg.control_max_conns),
            (Plane::Data, data, cfg.data_max_conns),
        ]
        .into_iter()
        .enumerate()
        {
            let mut rc = rc.clone();
            rc.requests = requests[n].clone();
            let stop = stop.clone();
            let counter = accepted[n].clone();
            #[cfg(test)]
            let probe = if plane == Plane::Control { control_probe.take() } else { None };
            let name = format!(
                "asset-server-{}-accept",
                if plane == Plane::Control { "control" } else { "data" }
            );
            workers.push(
                std::thread::Builder::new()
                    .name(name)
                    .spawn(move || {
                        accept_loop(
                            listener, rc, plane, max_conns, &stop, counter,
                            #[cfg(test)]
                            probe,
                        )
                    })
                    .map_err(|e| ServerError::Io { op: "spawn plane acceptor", kind: e.kind() })?,
            );
        }

        let janitor = Some(spawn_janitor(state.clone(), events.clone(),
            cfg.janitor_interval_ms,
            cfg.gc_janitor_steps,
        )?);

        // Sync is never a reason to deny the catalog: a refusal (no fleet
        // key, a non-LAN address) is logged and kept for status lines.
        let (sync, sync_error) = match &cfg.sync {
            None => (None, None),
            Some(sync_cfg) => match super::sync::mint_inner_token(&state, cfg.max_token_ttl_ms).and_then(|token| {
                super::sync::start(
                    sync_cfg,
                    super::sync::PullCtx {
                        state: super::sync::SharedState::new(state.clone()),
                        events: events.clone(),
                        cas: rc.cas.clone(),
                        server_id,
                        log: cfg.log,
                        exclude: Arc::new(sync_cfg.exclude.clone()),
                    },
                    control_addr,
                    data_addr,
                    token,
                )
            }) {
                Ok(host) => (Some(host), None),
                Err(error) => {
                    log(true, &format!("store sync off: {error}"));
                    (None, Some(error.to_string()))
                }
            },
        };

        write_listen_file(&cfg.root, control_addr, data_addr);
        log(
            cfg.log,
            &format!(
                "asset server up: control {control_addr}, data {data_addr}, server_id {}, recovered {} cas temps / {} leases",
                to_hex(&server_id),
                recover.cas_temps_removed,
                recover.leases_expired
            ),
        );
        Ok(AssetServer {
            control_addr,
            data_addr,
            accepted,
            requests,
            server_id,
            recover,
            stop,
            workers,
            janitor,
            state: Some(state),
            state_join: Some(state_join),
            events,
            sync,
            sync_error,
            _root_lock: root_lock,
            log_enabled: cfg.log,
        })
    }

    pub fn control_addr(&self) -> SocketAddr {
        self.control_addr
    }

    pub fn data_addr(&self) -> SocketAddr {
        self.data_addr
    }

    /// Connections accepted on the control plane since start.
    pub fn control_connections_accepted(&self) -> u64 {
        self.accepted[0].load(Ordering::Relaxed)
    }

    /// Connections accepted on the data plane since start.
    pub fn data_connections_accepted(&self) -> u64 {
        self.accepted[1].load(Ordering::Relaxed)
    }

    /// Requests served on the control plane since start.
    pub fn control_requests_served(&self) -> u64 {
        self.requests[0].load(Ordering::Relaxed)
    }

    /// Requests served on the data plane since start.
    pub fn data_requests_served(&self) -> u64 {
        self.requests[1].load(Ordering::Relaxed)
    }

    pub fn server_id(&self) -> [u8; 16] {
        self.server_id
    }

    /// The store sync front's address, when sync runs.
    pub fn sync_addr(&self) -> Option<SocketAddr> {
        self.sync.as_ref().map(|s| s.addr())
    }

    /// Why sync is off although it was configured.
    pub fn sync_error(&self) -> Option<&str> {
        self.sync_error.as_deref()
    }

    /// What restart recovery found when this instance opened its root.
    pub fn recover_report(&self) -> RecoverReport {
        self.recover
    }

    /// Stop accepting, finish in-flight requests, and join every thread.
    /// Idempotent; also runs on drop.
    pub fn shutdown(&mut self) {
        // Sync first: its pullers write through the state thread and its
        // front proxies to the planes.
        if let Some(mut sync) = self.sync.take() {
            sync.stop();
        }
        self.stop.store(true, Ordering::SeqCst);
        // Wake parked `/v1/events` long-polls FIRST: their connection
        // threads must answer and exit before the acceptor joins them.
        self.events.shutdown();
        for j in self.workers.drain(..) {
            let _ = j.join();
        }
        if let Some((tx, j)) = self.janitor.take() {
            let _ = tx.send(());
            let _ = j.join();
        }
        // Workers are gone, so this drop releases the last handle clones and
        // the state thread's task channel closes.
        drop(self.state.take());
        if let Some(j) = self.state_join.take() {
            let _ = j.join();
            log(self.log_enabled, "asset server stopped");
        }
    }
}

impl Drop for AssetServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Bind addresses for a live server (`ip:control:data`). Clients that lose
/// the root lock read this instead of treating the catalog as unusable.
pub const LISTEN_FILE: &str = "listen";

fn write_listen_file(root: &std::path::Path, control: SocketAddr, data: SocketAddr) {
    let ip = if control.ip().is_unspecified() {
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    } else {
        control.ip()
    };
    let _ = std::fs::write(
        root.join(LISTEN_FILE),
        format!("{ip}:{}:{}\n", control.port(), data.port()),
    );
}

/// Take the exclusive advisory lock on `<root>/server.lock`.
fn lock_root(cfg: &ServerConfig) -> ServerResult<File> {
    let path = cfg.root.join("server.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| ServerError::Io { op: "open root lock", kind: e.kind() })?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(_) => Err(ServerError::InvalidState {
            what: "server root",
            state: "locked by another server process",
        }),
    }
}

fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let listener = TcpListener::bind(addr)?;
    // Nonblocking accept + short sleep lets workers notice the stop flag
    // without a wake-up connection dance.
    listener.set_nonblocking(true)?;
    Ok(listener)
}

struct ConnectionWorker {
    join: JoinHandle<()>,
    wake: Weak<TcpStream>,
}

fn accept_loop(
    listener: TcpListener,
    rc: RouteCtx,
    plane: Plane,
    max_conns: usize,
    stop: &Arc<AtomicBool>,
    accepted: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)] mut probe: Option<tests::ShutdownProbe>,
) {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut workers: Vec<ConnectionWorker> = Vec::new();
    // The listener is non-blocking so shutdown never has to interrupt a
    // parked accept. That makes the idle wait a LATENCY budget: a flat 10 ms
    // nap meant every new connection could wait 10 ms to be accepted, which
    // on a localhost store is an order of magnitude more than serving the
    // request. So poll fast right after activity — connections arrive in
    // bursts, one per worker lane — and back off to a cheap idle poll.
    let mut idle: u32 = 0;
    while !stop.load(Ordering::Relaxed) {
        // Reap finished connection threads so the handle list stays bounded
        // by the live-connection cap.
        workers.retain(|worker| !worker.join.is_finished());
        match listener.accept() {
            Ok((stream, _peer)) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                idle = 0;
                if active.load(Ordering::Relaxed) >= max_conns {
                    refuse_over_capacity(stream);
                    continue;
                }
                active.fetch_add(1, Ordering::Relaxed);
                accepted.fetch_add(1, Ordering::Relaxed);
                let rc = rc.clone();
                let stop = stop.clone();
                let conn_active = active.clone();
                let wake_stream = match stream.try_clone() {
                    Ok(socket) => Some(Arc::new(socket)),
                    Err(error) => {
                        log(rc.cfg.log, &format!("connection wake socket clone failed: {error}"));
                        None
                    }
                };
                // Only the worker owns the duplicate, so an unreaped handle cannot delay EOF.
                let wake = wake_stream.as_ref().map(Arc::downgrade).unwrap_or_default();
                #[cfg(test)]
                let connection_probe = probe.as_mut().and_then(tests::ShutdownProbe::next_connection);
                let spawned = std::thread::Builder::new()
                    .name("asset-server-conn".into())
                    .spawn(move || {
                        serve_conn(
                            stream, &rc, plane, &stop,
                            #[cfg(test)]
                            connection_probe,
                        );
                        drop(wake_stream);
                        conn_active.fetch_sub(1, Ordering::Relaxed);
                    });
                match spawned {
                    Ok(join) => workers.push(ConnectionWorker { join, wake }),
                    Err(_) => {
                        active.fetch_sub(1, Ordering::Relaxed);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                idle = idle.saturating_add(1);
                // ~40 ms of fast polling after the last accept, then idle.
                if idle < 200 {
                    std::thread::sleep(Duration::from_micros(200));
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Err(_) => {
                // Transient accept failure (fd pressure, aborted handshake):
                // back off instead of spinning.
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    for worker in &workers {
        if let Some(stream) = worker.wake.upgrade() {
            match makepad_bounded_http::wake_idle_read(&stream) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {}
                Err(error) => log(rc.cfg.log, &format!("connection read wake failed: {error}")),
            }
        }
    }
    #[cfg(test)]
    if let Some(probe) = probe {
        probe.wake_pass_finished();
    }
    for ConnectionWorker { join: j, .. } in workers {
        let _ = j.join();
    }
}

/// The over-capacity refusal: explicit and immediate, never a silent queue.
fn refuse_over_capacity(stream: TcpStream) {
    let _ = stream.set_nonblocking(false);
    let mut conn = match makepad_bounded_http::Conn::new(stream, 1000, 1000) {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = conn.write_resp(false, &Resp::error(503, "connection capacity").closing());
}

fn serve_conn(
    stream: TcpStream,
    rc: &RouteCtx,
    plane: Plane,
    stop: &AtomicBool,
    #[cfg(test)] probe: Option<tests::ConnectionProbe>,
) {
    // The accepted socket may inherit the listener's nonblocking mode on some
    // platforms; timeouts require blocking mode.
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let cfg = &rc.cfg;
    #[cfg(not(test))]
    let connection = Conn::new(stream, cfg.read_timeout_ms, cfg.write_timeout_ms);
    #[cfg(test)]
    let connection = tests::ProbeTransport::connection(stream, cfg, probe);
    let mut conn = match connection {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut served: u32 = 0;
    loop {
        let mut head = match conn.next_request(cfg.head_deadline_ms, cfg.keepalive_idle_ms, stop) {
            Ok(head) => head,
            Err(HeadError::Closed) | Err(HeadError::Io) => return,
            Err(HeadError::Timeout) => {
                let _ = conn.write_resp(false, &Resp::error(408, "head timeout").closing());
                return;
            }
            Err(HeadError::Bad(status, msg)) => {
                let _ = conn.write_resp(false, &Resp::error(status, msg).closing());
                return;
            }
        };
        served += 1;
        rc.requests.fetch_add(1, Ordering::Relaxed);
        let force_close =
            served >= cfg.max_requests_per_conn || stop.load(Ordering::Relaxed);
        match serve_request(&mut conn, &mut head, rc, plane, force_close) {
            Outcome::Hangup => return,
            Outcome::Streamed { close } => {
                // Streamed responses only exist on GET/HEAD, whose requests
                // carry no body; the boundary is already clean.
                if close || !conn.finish_request(&mut head) {
                    return;
                }
            }
            Outcome::Resp(mut resp) => {
                let keep = conn.finish_request(&mut head) && !resp.close && !force_close;
                resp.close = !keep;
                let is_head = head.method == Method::Head;
                if conn.write_resp(is_head, &resp).is_err() || resp.close {
                    return;
                }
            }
        }
    }
}

fn spawn_janitor(
    state: StateHandle,
    events: Arc<super::events::EventHub>,
    interval_ms: u64,
    gc_steps: u32,
) -> ServerResult<(mpsc::Sender<()>, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel::<()>();
    let interval = Duration::from_millis(interval_ms.max(1));
    let join = std::thread::Builder::new()
        .name("asset-server-janitor".into())
        .spawn(move || loop {
            match rx.recv_timeout(interval) {
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    events.expire_model_previews();
                    let now = now_ms();
                    // Poisoned state returns None; keep ticking, health
                    // reports the outage.
                    // A GC run finishes even if nobody polls it: a bounded
                    // number of steps per tick, each one transaction over
                    // one batch, interleaved with every other state call.
                    if gc_steps > 0 {
                        let _ = state.call(move |ctx| ctx.core.gc_advance(gc_steps, now));
                    }
                }
                _ => break,
            }
        })
        .map_err(|e| ServerError::Io { op: "spawn janitor", kind: e.kind() })?;
    Ok((tx, join))
}

/// Ensure the bootstrap admin principal exists with a wildcard `auth_admin`
/// grant, and that `<root>/admin-token` holds a working secret for it. The
/// secret only ever exists in that 0600 file and in issuance responses; the
/// stores hold hashes.
fn bootstrap_admin(state: &StateHandle, cfg: &ServerConfig) -> ServerResult<()> {
    let now = now_ms();
    let minted = PrincipalId(super::util::rand16()?);
    let root = state
        .call(move |ctx| -> ServerResult<PrincipalId> {
            if let Some(existing) = ctx.root_admin_get()? {
                return Ok(existing);
            }
            // A catalog without its transport database (copied from another
            // machine's store, or restored without it) still holds the root
            // admin this store bootstrapped. Only a root admin creates
            // principals, so that name is always this one: adopt it rather
            // than fail on its unique name and leave the store unhosted.
            if let Some(existing) = ctx.core.auth().principal_named("root-admin")? {
                ctx.core.auth().grant(&existing, Capability::AuthAdmin, Scope::All, now)?;
                ctx.root_admin_set(&existing)?;
                return Ok(existing);
            }
            // One catalog commit for both rows on a fresh root.
            ctx.core.catalog().db.tx(|_| {
                ctx.core.auth().create_principal(&minted, "root-admin", now)?;
                ctx.core.auth().grant(&minted, Capability::AuthAdmin, Scope::All, now)
            })?;
            ctx.root_admin_set(&minted)?;
            Ok(minted)
        })
        .ok_or(ServerError::InvalidState { what: "state thread", state: "unavailable" })??;

    let token_path = cfg.root.join("admin-token");
    if let Ok(existing) = std::fs::read_to_string(&token_path) {
        let candidate = existing.trim().to_string();
        if !candidate.is_empty() {
            let probe = candidate.clone();
            let still_valid = state
                .call(move |ctx| -> ServerResult<bool> {
                    match ctx.core.auth().authenticate(probe.as_bytes(), now) {
                        Ok(p) => Ok(p == root),
                        Err(ServerError::Unauthenticated) => Ok(false),
                        Err(e) => Err(e),
                    }
                })
                .ok_or(ServerError::InvalidState { what: "state thread", state: "unavailable" })??;
            if still_valid {
                return Ok(());
            }
            log(cfg.log, "admin-token no longer valid; minting a replacement");
        }
    }

    let secret = format!("{}{}", super::api::TOKEN_PREFIX, to_hex(&rand32()?));
    let hash = token_hash(secret.as_bytes());
    let expires = now
        .checked_add(cfg.admin_token_ttl_ms)
        .ok_or(ServerError::InvalidInput { what: "admin token ttl overflow" })?;
    state
        .call(move |ctx| ctx.core.auth().register_token(&root, &hash, expires, now))
        .ok_or(ServerError::InvalidState { what: "state thread", state: "unavailable" })??;
    write_secret_file(&token_path, &secret)?;
    log(cfg.log, &format!("admin token written to {}", token_path.display()));
    Ok(())
}

/// Write a secret to a file only the owner can read.
fn write_secret_file(path: &std::path::Path, secret: &str) -> ServerResult<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| ServerError::Io { op: "open admin-token", kind: e.kind() })?;
    f.write_all(format!("{secret}\n").as_bytes())
        .map_err(|e| ServerError::Io { op: "write admin-token", kind: e.kind() })?;
    f.sync_all()
        .map_err(|e| ServerError::Io { op: "sync admin-token", kind: e.kind() })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use makepad_bounded_http::Transport;
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::sync::mpsc::Sender;

    const EMPTY_ASSETS: &[u8] = br#"{"assets":[],"cursor":null}"#;

    #[derive(Debug)]
    enum Event {
        IdlePending(Sender<()>),
        IdleResult(Result<usize, io::ErrorKind>),
        TailPending(Sender<()>),
        WakePassFinished,
    }

    enum Watch {
        Idle { response_written: bool },
        Tail,
        TailByte,
    }

    pub(super) struct ConnectionProbe {
        watch: Watch,
        events: Sender<Event>,
    }

    pub(super) struct ShutdownProbe {
        connections: std::vec::IntoIter<Option<ConnectionProbe>>,
        events: Sender<Event>,
    }

    impl ShutdownProbe {
        pub(super) fn next_connection(&mut self) -> Option<ConnectionProbe> {
            self.connections.next().flatten()
        }

        pub(super) fn wake_pass_finished(self) {
            self.events.send(Event::WakePassFinished).unwrap();
        }
    }

    pub struct ProbeTransport {
        stream: TcpStream,
        probe: Option<ConnectionProbe>,
    }

    impl ProbeTransport {
        pub(super) fn connection(
            stream: TcpStream,
            cfg: &ServerConfig,
            probe: Option<ConnectionProbe>,
        ) -> io::Result<Conn> {
            stream.set_nodelay(true)?;
            stream.set_write_timeout(Some(Duration::from_millis(cfg.write_timeout_ms.max(1))))?;
            Ok(Conn::over(Self { stream, probe }, cfg.read_timeout_ms))
        }
    }

    impl Read for ProbeTransport {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if let Some(ConnectionProbe { watch: Watch::Idle { response_written: true }, events }) = &self.probe {
                self.stream.set_nonblocking(true)?;
                assert_eq!(self.stream.read(buf).unwrap_err().kind(), io::ErrorKind::WouldBlock);
                let (resume, gate) = mpsc::channel();
                events.send(Event::IdlePending(resume)).unwrap();
                gate.recv().unwrap();
                let result = self.stream.read(buf);
                events.send(Event::IdleResult(result.as_ref().copied().map_err(io::Error::kind))).unwrap();
                self.stream.set_nonblocking(false)?;
                return result;
            }
            self.stream.read(buf)
        }
    }

    impl Write for ProbeTransport {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if let Some(probe) = &mut self.probe {
                match &mut probe.watch {
                    Watch::Idle { response_written } => {
                        let written = self.stream.write(buf)?;
                        *response_written = written == buf.len();
                        return Ok(written);
                    }
                    Watch::Tail => {
                        assert!(
                            buf.strip_suffix(EMPTY_ASSETS).is_some_and(|head| {
                                head.starts_with(b"HTTP/1.1 ") && head.ends_with(b"\r\n\r\n")
                            }),
                            "tail hold needs head and body in one write"
                        );
                        let prefix = &buf[..buf.len() - 1];
                        self.stream.write_all(prefix)?;
                        probe.watch = Watch::TailByte;
                        return Ok(prefix.len());
                    }
                    Watch::TailByte => {
                        let (resume, gate) = mpsc::channel();
                        probe.events.send(Event::TailPending(resume)).unwrap();
                        gate.recv().unwrap();
                    }
                }
            }
            self.stream.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.stream.flush()
        }
    }

    impl Transport for ProbeTransport {
        fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.stream.set_read_timeout(timeout)
        }

        fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
            self.stream.set_nonblocking(nonblocking)
        }

        fn peer_addr(&self) -> io::Result<SocketAddr> {
            self.stream.peer_addr()
        }
    }

    fn request_assets(control: SocketAddr, token: &str) -> BufReader<TcpStream> {
        let mut client = TcpStream::connect(control).unwrap();
        write!(
            client,
            "GET /v1/assets HTTP/1.1\r\nHost: example.test\r\nAuthorization: Bearer {token}\r\n\r\n"
        ).unwrap();
        let mut reader = BufReader::new(client);
        let headers: Vec<_> = (&mut reader).lines().map(Result::unwrap)
            .take_while(|line| !line.is_empty()).collect();
        assert_eq!(headers[0].split_whitespace().nth(1), Some("200"));
        let length = format!("Content-Length: {}", EMPTY_ASSETS.len());
        assert!(headers.iter().any(|line| line.eq_ignore_ascii_case(&length)), "{headers:?}");
        reader
    }

    #[test]
    fn shutdown_wakes_idle_read_and_finishes_partial_response() {
        let root = std::env::temp_dir().join(format!("asset-shutdown-{}", to_hex(&rand32().unwrap())));
        let mut cfg = ServerConfig::new(root.clone());
        cfg.control_addr.set_port(0);
        cfg.data_addr.set_port(0);
        cfg.bootstrap_admin = true;

        // On a setup assertion, drop the gate channels before the server joins its workers.
        let mut server;
        let (idle_tx, idle_events) = mpsc::channel();
        let (tail_tx, tail_events) = mpsc::channel();
        let (wake_tx, wake_events) = mpsc::channel();
        // Windows and other non-Unix platforms check only the partial response.
        // Windows still relies on the idle slice until the stop-event wake is implemented.
        let probe = ShutdownProbe {
            connections: vec![
                cfg!(unix).then_some(ConnectionProbe {
                    watch: Watch::Idle { response_written: false },
                    events: idle_tx,
                }),
                Some(ConnectionProbe { watch: Watch::Tail, events: tail_tx }),
            ].into_iter(),
            events: wake_tx,
        };
        server = AssetServer::start_inner(cfg, Some(probe)).unwrap();
        let control = server.control_addr();
        let token = std::fs::read_to_string(root.join("admin-token")).unwrap();
        let mut idle = request_assets(control, token.trim());
        let mut idle_body = [0; EMPTY_ASSETS.len()];
        idle.read_exact(&mut idle_body).unwrap();
        assert_eq!(&idle_body, EMPTY_ASSETS);
        let idle_gate = cfg!(unix).then(|| match idle_events.recv().unwrap() {
            Event::IdlePending(resume) => resume,
            event => panic!("expected a pending idle read, got {event:?}"),
        });

        let mut client = request_assets(control, token.trim());
        let prefix = &EMPTY_ASSETS[..EMPTY_ASSETS.len() - 1];
        let mut body = vec![0; prefix.len()];
        client.read_exact(&mut body).unwrap();
        assert_eq!(body, prefix);
        let Event::TailPending(tail_gate) = tail_events.recv().unwrap() else {
            panic!("expected a pending response tail");
        };
        let shutdown = std::thread::spawn(move || server.shutdown());
        assert!(matches!(wake_events.recv().unwrap(), Event::WakePassFinished));
        if let Some(resume) = idle_gate {
            resume.send(()).unwrap();
        }
        tail_gate.send(()).unwrap();
        shutdown.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();

        client.read_to_end(&mut body).unwrap();
        assert_eq!(body, EMPTY_ASSETS, "shutdown interrupted the response");
        if cfg!(unix) {
            let event = idle_events.try_recv().expect("idle read exited without a result");
            assert!(
                matches!(event, Event::IdleResult(Ok(0) | Err(io::ErrorKind::NotConnected))),
                "expected shutdown to end the idle read, got {event:?}"
            );
        }
    }
}
