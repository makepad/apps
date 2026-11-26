//! Host-facing asynchronous scope tools. Only workers wait; the handle retains full-queue calls
//! and retries in order on the next method call. Dropping it cancels work without joining.

/// Catalog entries and registration.
pub mod actions;
/// Checked Splash literal calls.
pub mod call;
/// Immutable records and their acquisition intake.
pub mod capture;
/// Splash replies and written source sites.
pub mod reply;
mod compare;
mod decode;
mod expect;
mod features;
mod query;
mod worker;

/// Catalog call accepted by the handle.
pub use call::ToolCall;
/// Returned object or refusal and the host-written statement location.
pub use reply::{Site, ToolReply};
use std::collections::VecDeque;
use std::time::Instant;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TrySendError};
use std::sync::Arc;
use fab_elec_bench::source::{Intake, Quantity};
use fab_elec_design::BenchOp;
use makepad_widgets_core::{Cx, SpawnError};
use super::acquire::SimChannel;
use super::fw_events::FwEvents;

/// Identity of one invocation, independent of capture ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CallId(pub u64);

/// The host's trace writer: a `captures/<name>.trace` path and its bytes into the project's
/// folder, answering where it went. The tools worker calls it for a capture, the host for the
/// Scope's take, so both land in one folder.
pub type SaveWriter = Arc<dyn Fn(&str, &[u8]) -> Result<String, String> + Send + Sync>;

/// The host's trace reader: a saved trace's bytes from its `captures/<name>.trace` path in the
/// same folder the writer writes, or why not (naming the traces saved there when it is missing).
pub type TraceReader = Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>;

/// A current bench probe or a target the host has resolved against the live circuit.
#[derive(Clone, Debug)]
pub struct ProbeInfo {
    /// Stable probe id; empty for an available target without a probe.
    pub id: String,
    /// User's channel label.
    pub label: String,
    /// Resolved simulator net or branch-current unknown name.
    pub target: String,
    /// Volt for a net, Amp only for a source or inductor's branch-current unknown.
    pub quantity: Quantity,
    /// Physical unit abbreviation.
    pub unit: String,
    /// Scope lane, zero based.
    pub lane: usize,
    /// Known signal swing, when available.
    pub range: Option<(f64, f64)>,
}

/// Snapshot used for target checks, simulator capabilities and trigger selection.
#[derive(Clone)]
pub struct ProbeSnapshot {
    /// Current bench probes in lane order.
    pub probes: Vec<ProbeInfo>,
    /// Available nets, pin aliases and branch-current unknowns, resolved by the host.
    pub targets: Vec<ProbeInfo>,
    /// The live simulator's configured maximum accepted step, in seconds.
    pub max_step: f64,
    /// The store drawn by the bench's firmware lane, when the run has firmware.
    pub firmware: Option<Arc<dyn FwEvents>>,
}

impl std::fmt::Debug for ProbeSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("ProbeSnapshot");
        out.field("probes", &self.probes).field("targets", &self.targets).field("max_step", &self.max_step);
        if let Some(events) = &self.firmware { out.field("firmware", &format_args!("{} events", events.len())); }
        else { out.field("firmware", &None::<usize>); }
        out.finish()
    }
}

/// One bench expectation as the host resolved it, with its live state.
#[derive(Clone, Debug)]
pub struct ExpectRow {
    /// The bench expectation's name.
    pub name: String,
    /// The probe as written in bench terms: `v(VOUT)`, `v(A, B)`, `i(R1.1)`, `p(U1)`.
    pub probe: String,
    /// The measured value's unit.
    pub unit: String,
    /// The measure's bench name (`mean`, `freq`, `width`, `uart`, ...).
    pub measure: String,
    /// Numeric or text bounds from the bench.
    pub bound: fab_elec_bench::expect::Bound,
    /// A span is final once done; a trailing check is final for a tool run once judged.
    pub when: fab_elec_bench::expect::When,
    /// Live state, including failures retained after a later passing judgment.
    pub state: fab_elec_bench::expect::CheckState,
    /// The `expect` statement.
    pub site: Site,
}

/// The host's expectations for its current simulation run, cloned without waiting.
/// Keep the run id, transport state and check states from the same run together.
#[derive(Clone, Debug)]
pub struct ExpectSnapshot {
    /// Identity shared with the restart reply; changes on every restart.
    pub run: u64,
    /// Current simulation time in seconds.
    pub t: f64,
    /// Whether this run is still advancing.
    pub running: bool,
    /// Expectations in bench order.
    pub rows: Vec<ExpectRow>,
}

/// Operations which only the owning document or live simulator host can perform.
/// A timed-out call drops its reply receiver; hosts must tolerate disconnected reply senders.
pub enum HostRequest {
    /// Return the bench's current probes, available targets, simulator step and firmware store.
    Probes(Sender<Result<ProbeSnapshot, String>>),
    /// Return the current run's expectations and live states without waiting for simulation.
    Expects(Sender<Result<ExpectSnapshot, String>>),
    /// Request the transport's power cycle (a restart from t = 0), replying immediately with its new run id.
    /// Until its reset states are published, Expects may still return the preceding run id.
    Restart(Sender<Result<u64, String>>),
    /// Apply one bench operation as one undo step, returning its written statement site.
    Edit(BenchOp, Sender<Result<Site, String>>),
    /// Attach the intake to the next live simulation run, through SimIntake's Sink adapter.
    SimCapture {
        /// Channels in intake layout order.
        channels: Vec<SimChannel>,
        /// Acquisition owned by the simulation worker after the host accepts it.
        intake: Box<dyn Intake>,
        /// Set once the capture takes no more points (it completed, was dropped, or the tools
        /// stopped): the host detaches the intake then.
        ended: Arc<AtomicBool>,
        /// Whether the attachment succeeded, or the source's refusal.
        reply: Sender<Result<(), String>>,
    },
    /// Show this opened trace in the Scope tab in place of the live run (`elec.capture.open`);
    /// nothing is answered.
    Show(Arc<capture::OpenedTrace>),
}

struct Invocation {
    id: CallId,
    call: ToolCall,
}

/// UI-side handle: no waits, joins, driver calls or worker-owned data locks.
pub struct ScopeTools {
    commands: SyncSender<Invocation>,
    pending: VecDeque<Invocation>,
    replies: Receiver<(CallId, ToolReply)>,
    hosts: Receiver<HostRequest>,
    failed: Vec<(CallId, ToolReply)>,
    next: u64,
    stopped: Arc<AtomicBool>,
}
impl ScopeTools {
    /// Spawn one long-lived tools worker. Synth opens its normal device/acquisition workers lazily.
    pub fn new(cx: &mut Cx, cap: usize, save: SaveWriter, read: TraceReader) -> Result<Self, SpawnError> {
        Self::with_clock(cx, cap, save, read, Instant::now)
    }

    /// The same worker with its deadlines (each call's `timeout:`) read
    /// from `now`: a test passes a clock it moves, so a deadline falls where the test puts it.
    pub fn with_clock(cx: &mut Cx, cap: usize, save: SaveWriter, read: TraceReader, now: fn() -> Instant) -> Result<Self, SpawnError> {
        let (commands, receive) = sync_channel(16);
        let (send, replies) = channel();
        let (host, hosts) = channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let spawner = cx.thread_spawner();
        cx.spawn_worker(move || worker::run(receive, send, host, stop, spawner, cap, save, read, now))?.detach();
        Ok(Self { commands, pending: VecDeque::new(), replies, hosts, failed: Vec::new(), next: 0, stopped })
    }
    fn retry(&mut self) {
        while let Some(call) = self.pending.pop_front() {
            match self.commands.try_send(call) {
                Ok(()) => (),
                Err(TrySendError::Full(call)) => { self.pending.push_front(call); break; }
                Err(TrySendError::Disconnected(call)) => self.failed.push((call.id, reply::failed("tools worker stopped"))),
            }
        }
    }
    /// Queue a call without waiting; a full channel retains every call in FIFO order.
    pub fn call(&mut self, call: ToolCall) -> CallId {
        self.next += 1;
        let id = CallId(self.next);
        self.pending.push_back(Invocation { id, call });
        self.retry();
        id
    }
    /// Drain completed replies without waiting and retry queued calls.
    pub fn poll(&mut self) -> Vec<(CallId, ToolReply)> {
        self.retry();
        let mut replies = std::mem::take(&mut self.failed);
        replies.extend(self.replies.try_iter());
        replies
    }
    /// Drain host operations without waiting; reply senders are unbounded and never wait for space.
    pub fn host_requests(&mut self) -> Vec<HostRequest> {
        self.retry();
        self.hosts.try_iter().collect()
    }
}
impl Drop for ScopeTools {
    fn drop(&mut self) { self.stopped.store(true, Ordering::Release); }
}
