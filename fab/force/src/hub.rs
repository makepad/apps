//! Force solves on the AI hub: the Force program ([`crate::program`]) on this machine's hub, over
//! loopback with the machine token, and with networked AI hub on on the fleet's nodes too. The hub
//! runs it on the device it picks; Fab sends a study (its analysed level laid out, its load, the
//! answer it starts from) and gets back the answer the in-process solve gives, to the solve's
//! tolerance. The hub builds the coarse levels and keeps them, with the analysed level, in the
//! solve's keep slot: a load edit sends the load and the guess and solves on them at once.
//!
//! A study the person waits on is one [`Solve`] at [`Class::Interactive`] on this machine's hub,
//! its progress told as it goes and cancelled there the moment it goes stale. A batch of independent
//! solves (an infill floor's densities) is [`analyse`]d over every node at once, each job where
//! [`place`] puts it: a pure function of the jobs' sizes and the node count, never of timings or
//! load, so a batch with the same nodes places the same way every time.

use crate::grid::Grid;
use crate::material::Materials;
use crate::pieces::Loose;
use crate::program;
use crate::solve::{Level, Options};
use crate::surface::Surface;
use crate::{Outcome, Posed, V3};
use makepad_ai_hub::kernel_client::{kernel_nodes, KernelStream};
#[cfg(not(target_arch = "wasm32"))]
use makepad_ai_hub::kernel_client::machine_kernel_nodes;
use makepad_ai_hub::program_client::{RemoteConfig, RemoteSession};
use makepad_compute_program::Updates;
pub use makepad_compute_program::Class;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

/// The hubs that serve compute programs as Fab looks: this machine's first, then (networked AI
/// hub on) the fleet's nodes in address order, each with its stream open.
pub struct Hub {
    nodes: Vec<(String, Arc<KernelStream>)>,
}

impl Hub {
    /// This machine's hub and, with networked AI hub on, the fleet's nodes: the ones that answer
    /// now. Blocking: a request each (in parallel for the fleet's), then a stream each.
    pub fn find() -> Hub {
        let mut fleet = kernel_nodes();
        fleet.sort_by(|a, b| a.0.base_url.cmp(&b.0.base_url));
        let found = fleet.into_iter();
        #[cfg(not(target_arch = "wasm32"))]
        let found = machine_kernel_nodes().into_iter().chain(found);
        Hub::of(found.filter_map(|(node, _)| KernelStream::open(&node).ok().map(|s| (node.base_url.clone(), Arc::new(s)))).collect())
    }

    /// These nodes (named, their streams open), this machine's hub first.
    pub fn of(nodes: Vec<(String, Arc<KernelStream>)>) -> Hub {
        Hub { nodes }
    }

    /// The nodes' names (their addresses), this machine's hub first.
    pub fn nodes(&self) -> Vec<&str> {
        self.nodes.iter().map(|(name, _)| name.as_str()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Whether a node's stream is still open (a hub that quit closes its stream: look again with
    /// [`Hub::find`]).
    pub fn is_open(&self) -> bool {
        self.nodes.iter().any(|(_, s)| !s.is_closed())
    }

    /// Where a study the person waits on runs: the first node whose stream is open, and its name.
    pub fn first(&self) -> Result<(&str, &Arc<KernelStream>), String> {
        match self.nodes.iter().find(|(_, s)| !s.is_closed()) {
            Some((name, s)) => Ok((name, s)),
            None if self.nodes.is_empty() => Err(NO_HUB.into()),
            None => Err("The AI hub's link closed: look for it again".into()),
        }
    }
}

const NO_HUB: &str = "No AI hub serves compute programs: turn the AI hub on";

/// A hub in this process on loopback, the AI hub app's node side as it serves this machine's
/// apps (for benches and tests; Fab reaches the hub the AI hub app runs): its address. It lives
/// as long as the process, its cache in a folder `cache` names in the temp folder. Without a
/// GPU device it runs programs on the CPU: as the AI hub app does, a caller with one gives it
/// with `makepad_ai_hub::program_serve::service().set_gpu`.
#[cfg(feature = "loopback")]
pub fn loopback(cache: &str) -> Result<String, String> {
    use makepad_ai_hub::server::{start_service, ServiceConfig};
    let dir = std::env::temp_dir().join(format!("{cache}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let handle = start_service(ServiceConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        cache_dir: dir,
        registry: makepad_ai_hub::registry::Registry::embedded().map_err(|e| format!("the hub's registry: {e:?}"))?,
        downloader: makepad_ai_hub::download::Downloader::new("http://127.0.0.1:1", None).map_err(|e| format!("the hub's downloader: {e:?}"))?,
        peer: makepad_ai_hub::peer_serve::PeerOptions { serve: Some(false), sources: Some(Vec::new()), ..Default::default() },
        fleet: makepad_ai_hub::discovery::DEFAULT_FLEET.to_string(),
    })
    .map_err(|e| format!("the loopback hub: {e:?}"))?;
    let url = format!("http://{}", handle.addr);
    std::mem::forget(handle);
    Ok(url)
}

/// A running solve's progress: iterations so far, the residual's share of the load, and how
/// far it is from the start to the tolerance (0..1, on the log of the residual).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub iterations: usize,
    pub residual: f64,
    pub fraction: f64,
}

/// How often a waiting solve asks whether it went stale while the hub says nothing (a cancel
/// reaches the program however quiet it is).
const ASK_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// Where a solve on the hub ran: on its GPU engine (else its CPU engine, as where the node has
/// no GPU or its GPU could not hold the solve), what its GPU route needs in all and in its
/// largest buffer, and the device's GPU budget and largest buffer (bytes; None: no GPU).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ran {
    pub gpu: bool,
    pub gpu_need: f64,
    pub gpu_largest: f64,
    pub gpu_limits: Option<(f64, f64)>,
}

/// One study solved on the hub. Dropping it cancels the program there.
pub struct Solve {
    session: RemoteSession,
    node: String,
    tolerance: f64,
    grid: Arc<Grid>,
    held: Vec<u8>,
    near: Vec<bool>,
    loose: Loose,
    surface: Option<Arc<Surface>>,
    materials: Materials,
}

impl Solve {
    /// Sends `posed` with materials `m` to this machine's hub (else the first node that answers),
    /// to be solved from `guess` (an answer on the same grid; [`crate::solve::start`]) at `class`.
    /// The hub keeps the model and the coarse levels it builds of it in slot `keep` (empty:
    /// none) in place of the slot's last ones, so the next solve kept there sends only what
    /// changed and solves on them at once (a load edit: the load and the guess). A solve that
    /// runs once (a size no step comes back to) keeps nothing. None once `stale` says yes before
    /// the hub takes it: a hub that never answers keeps nobody waiting.
    #[allow(clippy::too_many_arguments)]
    pub fn start(hub: &Hub, posed: Posed, m: &Materials, guess: Option<&[V3]>, opts: &Options, class: Class, keep: &str, stale: &mut dyn FnMut() -> bool) -> Result<Option<Solve>, String> {
        let fine = Level::new(posed.grid.clone(), &posed.held, m);
        Solve::start_on(hub, &fine, posed, m, guess, opts, class, keep, stale)
    }

    /// [`Solve::start`] on `fine`, the level analysed for `posed`'s grid and held nodes with
    /// these materials: a study whose loads alone changed sends the one it has.
    #[allow(clippy::too_many_arguments)]
    pub fn start_on(hub: &Hub, fine: &Level, posed: Posed, m: &Materials, guess: Option<&[V3]>, opts: &Options, class: Class, keep: &str, stale: &mut dyn FnMut() -> bool) -> Result<Option<Solve>, String> {
        let (node, stream) = hub.first()?;
        let Posed { grid, held, force, near, loose, surface } = posed;
        let (params, inputs) = program::job(fine, &force, guess, opts)?;
        let config = RemoteConfig { params, inputs, limits: program::limits(), class, keep: keep.to_string(), ..RemoteConfig::default() };
        let opened = RemoteSession::open_unless(stream, &program::program(), config, stale).map_err(|e| format!("The AI hub did not take the solve: {e:?}"))?;
        Ok(opened.map(|session| Solve { session, node: node.to_string(), tolerance: opts.tolerance, grid, held, near, loose, surface, materials: m.clone() }))
    }

    /// A solve of `posed` waiting on `program` in the Force program's place.
    #[cfg(all(test, feature = "loopback"))]
    pub(crate) fn on_session(stream: &Arc<KernelStream>, program: &makepad_compute_program::Program, posed: Posed, m: &Materials) -> Solve {
        let Posed { grid, held, near, loose, surface, .. } = posed;
        let session = RemoteSession::open(stream, program, RemoteConfig::default()).unwrap();
        Solve { session, node: String::new(), tolerance: Options::default().tolerance, grid, held, near, loose, surface, materials: m.clone() }
    }

    /// The node it runs on (its address: this machine's hub is on loopback).
    pub fn node(&self) -> &str {
        &self.node
    }

    /// The device's GPU budget and its largest buffer (bytes), where the node has a GPU.
    fn gpu_limits(&self) -> Option<(f64, f64)> {
        let at = |k: &str| self.session.device().get(k).and_then(|v| v.as_number());
        Some((at("gpu_buffer_bytes")?, at("gpu_max_buffer_bytes").unwrap_or(f64::INFINITY)))
    }

    /// The outcome, waiting for it. `stale` is asked as the solve starts, with the progress
    /// when there is more of it (the question [`crate::solve::solve_until`]'s `stop` is asked),
    /// and every [`ASK_EVERY`] with None while the hub says nothing; once it says yes the
    /// program on the hub is cancelled and this is None.
    pub fn wait(self, stale: &mut dyn FnMut(Option<&Progress>) -> bool) -> Result<Option<Outcome>, String> {
        self.wait_ran(stale).map(|o| o.map(|(outcome, _)| outcome))
    }

    /// [`Solve::wait`], with where it ran.
    pub fn wait_ran(self, stale: &mut dyn FnMut(Option<&Progress>) -> bool) -> Result<Option<(Outcome, Ran)>, String> {
        let mut seen = Updates::default();
        let mut told = None;
        loop {
            if let Some(done) = program::answer(&seen) {
                let solution = done.map_err(stopped_on_hub)?;
                let (gpu, gpu_need, gpu_largest) = program::engine(&seen).unwrap_or((false, 0.0, 0.0));
                let ran = Ran { gpu, gpu_need, gpu_largest, gpu_limits: self.gpu_limits() };
                return Ok(Some((crate::outcome(self.grid, self.held, &self.near, solution, &self.materials, self.loose, self.surface.as_deref()), ran)));
            }
            let news = program::progress(&seen).filter(|&(its, _)| told != Some(its)).map(|(iterations, residual)| {
                told = Some(iterations);
                let fraction = match residual > 0.0 {
                    true => (residual.ln() / self.tolerance.ln()).clamp(0.0, 1.0),
                    false => 1.0,
                };
                Progress { iterations, residual, fraction }
            });
            if stale(news.as_ref()) {
                self.session.cancel();
                return Ok(None);
            }
            seen.merge(self.session.wait_within(ASK_EVERY));
        }
    }
}

/// A program's error on the hub, for the log: the panel says plainly that the solve stopped
/// there (a program's own words, `force.splash:3036:13: s.run(..)`, are no one's to read).
fn stopped_on_hub(e: String) -> String {
    eprintln!("fab force: the solve stopped on the AI hub: {e}");
    "The solve stopped on the AI hub before it answered (the log has why).".to_string()
}

/// `fine`'s solves of the loads `fs` alone on this machine's hub (else the first node that
/// answers), in one session (the levels set up once, the loads solved in turn), each to `opts`'
/// tolerance from rest, kept in slot `keep` as [`Solve::start`] keeps a study: the
/// displacements in the loads' order and the iterations they took in all, no stresses read. The
/// buckling stage's preconditioner ([`crate::buckle::lobpcg`], a step's residuals at once): in a
/// study's own slot the hub has the level and its coarse levels already, so only the loads are
/// sent. None once `stale` says yes (the program on the hub is cancelled).
pub fn solve_u(hub: &Hub, fine: &Level, fs: &[Vec<V3>], opts: &Options, class: Class, keep: &str, stale: &mut dyn FnMut() -> bool) -> Result<Option<(Vec<Vec<V3>>, usize)>, String> {
    let (_, stream) = hub.first()?;
    let loads: Vec<&[V3]> = fs.iter().map(Vec::as_slice).collect();
    let (params, inputs) = program::job_block(fine, &loads, None, opts, program::Solves::OneRun)?;
    let config = RemoteConfig { params, inputs, limits: program::limits(), class, keep: keep.to_string(), ..RemoteConfig::default() };
    let Some(session) = RemoteSession::open_unless(stream, &program::program(), config, stale).map_err(|e| format!("The AI hub did not take the solve: {e:?}"))? else {
        return Ok(None);
    };
    let mut seen = Updates::default();
    loop {
        if let Some(done) = program::answers(&seen) {
            let all = done.map_err(stopped_on_hub)?;
            let its = all.iter().map(|s| s.iterations).sum();
            return Ok(Some((all.into_iter().map(|s| s.u).collect(), its)));
        }
        if stale() {
            session.cancel();
            return Ok(None);
        }
        seen.merge(session.wait_within(ASK_EVERY));
    }
}

/// One solve of a batch: a study as the solvers take it, its materials, and the answer it
/// starts from.
pub struct Job {
    pub posed: Posed,
    pub materials: Materials,
    pub guess: Option<Vec<V3>>,
}

/// Which node each job of a batch runs on with `nodes` nodes, `cells[i]` job i's size (its
/// cells; a solve's time goes with them): the largest first, each to the node with the fewest
/// cells so far (on a tie the first). With the order they start in (the largest first).
pub fn place(cells: &[usize], nodes: usize) -> (Vec<usize>, Vec<usize>) {
    let mut order: Vec<usize> = (0..cells.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(cells[i]));
    let mut load = vec![0usize; nodes];
    let mut placed = vec![0; cells.len()];
    for &i in &order {
        if let Some(k) = (0..nodes).min_by_key(|&k| (load[k], k)) {
            load[k] += cells[i];
            placed[i] = k;
        }
    }
    (placed, order)
}

/// Solves every job on the node [`place`] puts it on, at `class`, all nodes at once (each runs
/// its jobs in turn, the largest first). The outcomes in job order. `stale` is asked while the
/// nodes run: once it says yes the running solves are cancelled and the rest never start (all
/// of them come back as errors).
pub fn analyse(hub: &Hub, jobs: Vec<Job>, opts: &Options, stale: &dyn Fn() -> bool, class: Class) -> Vec<Result<Outcome, String>> {
    if hub.is_empty() {
        return jobs.iter().map(|_| Err(NO_HUB.to_string())).collect();
    }
    let count = jobs.len();
    let cells: Vec<usize> = jobs.iter().map(|j| (0..j.posed.grid.cells()).filter(|&c| j.posed.grid.active(c)).count()).collect();
    let (placed, order) = place(&cells, hub.nodes.len());
    let mut queues: Vec<Vec<(usize, Job)>> = hub.nodes.iter().map(|_| Vec::new()).collect();
    let mut jobs: Vec<Option<Job>> = jobs.into_iter().map(Some).collect();
    for i in order {
        queues[placed[i]].push((i, jobs[i].take().unwrap()));
    }
    let stopped = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel::<(usize, Result<Outcome, String>)>();
    let mut out: Vec<Option<Result<Outcome, String>>> = (0..count).map(|_| None).collect();
    std::thread::scope(|scope| {
        for (queue, (name, stream)) in queues.into_iter().zip(&hub.nodes) {
            let (tx, stopped) = (tx.clone(), &stopped);
            let node = Hub::of(vec![(name.clone(), stream.clone())]);
            scope.spawn(move || {
                for (i, job) in queue {
                    let done = match stopped.load(Ordering::Relaxed) {
                        true => Err(STALE.to_string()),
                        false => Solve::start(&node, job.posed, &job.materials, job.guess.as_deref(), opts, class, "", &mut || stopped.load(Ordering::Relaxed))
                            .and_then(|s| s.ok_or_else(|| STALE.to_string()))
                            .and_then(|s| s.wait(&mut |_| stopped.load(Ordering::Relaxed)))
                            .and_then(|o| o.ok_or_else(|| STALE.to_string())),
                    };
                    let _ = tx.send((i, done));
                }
            });
        }
        drop(tx);
        // asked while the nodes run: a stale batch stops each node's solve after its iteration
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(20)) {
                Ok((i, done)) => out[i] = Some(done),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if !stopped.load(Ordering::Relaxed) && stale() {
                stopped.store(true, Ordering::Relaxed);
            }
        }
    });
    out.into_iter().map(|o| o.unwrap_or_else(|| Err(STALE.into()))).collect()
}

const STALE: &str = "stopped: the batch went stale";
