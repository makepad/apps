use crate::job::{JobStatus, PrintJob};
#[cfg(not(target_arch = "wasm32"))]
use crate::job::JobStage;
use crate::model::PrinterState;
use crate::secrets::valid_name;
use makepad_network::SignalToUI;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::Thread;
#[cfg(not(target_arch = "wasm32"))]
use crate::{job::{self, Effect, Tracker}, model::{self, Reports}, mqtt::{self, ByteStream, Request, Session}, secrets::{Access, PrinterConfig, Secret}, tls};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum ConnectionStatus {
    #[default]
    Connecting,
    Connected,
    WrongAccessCode,
    WrongApiKey,
    PinMismatch,
    Offline(&'static str),
}

impl ConnectionStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Connecting => "Connecting", Self::Connected => "Connected", Self::WrongAccessCode => "Access code wrong or changed",
            Self::WrongApiKey => "The printer wants an API key, or the key is wrong",
            Self::PinMismatch => "Certificate changed: connection refused", Self::Offline(reason) => reason,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub name: String,
    pub model: String,
    /// The trays and heads come from Fab's virtual printer layout.
    pub virtual_printer: bool,
    /// How fast a virtual printer's job runs ([`PrinterLink::set_virtual_speed`]).
    pub virtual_speed: crate::virtual_run::Speed,
    pub serial_suffix: Option<String>,
    pub status: ConnectionStatus,
    pub state: Arc<PrinterState>,
    /// Fab's current or last print job on this printer.
    pub job: Option<JobStatus>,
    /// The print commands the link has taken: `job` is about the last of them.
    pub job_sends: u64,
    /// The last control command (pause, resume, stop, clear error) and its answer.
    pub command: Option<(&'static str, CommandState)>,
    /// The last storage listing (Check storage).
    pub storage_files: Option<Result<Vec<crate::ftps::Entry>, String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandState { Pending, Done, Failed(String) }

/// What the UI asks of the printer. Printing is confirmed in the UI before it is sent.
#[derive(Clone, Debug)]
pub enum Command {
    Print(PrintJob),
    Pause,
    Resume,
    /// Stops the print (or cancels the upload).
    Stop,
    /// Clears the error the printer shows.
    ClearError,
    /// Lists the printer's storage (read-only).
    CheckStorage,
}

pub(crate) struct Shared {
    pub(crate) slot: Mutex<Arc<Snapshot>>,
    commands: Mutex<VecDeque<Command>>,
    revision: AtomicUsize,
    clients: AtomicUsize,
    /// The printer model the app last spoke for and the heads its profile has: a link whose
    /// printer reports each head on its own watches these while it links that model.
    heads: Mutex<Option<(String, usize)>>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) pending_virtual: Mutex<Option<crate::VirtualSetup>>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) pending_speed: Mutex<Option<crate::virtual_run::Speed>>,
    pub(crate) stop: Arc<AtomicBool>,
    thread: Mutex<Option<Thread>>,
}

impl Shared {
    fn new(snapshot: Snapshot) -> Self {
        Self {
            slot: Mutex::new(Arc::new(snapshot)), commands: Mutex::new(VecDeque::new()),
            revision: AtomicUsize::new(1), clients: AtomicUsize::new(1), heads: Mutex::new(None),
            #[cfg(not(target_arch = "wasm32"))]
            pending_virtual: Mutex::new(None),
            #[cfg(not(target_arch = "wasm32"))]
            pending_speed: Mutex::new(None),
            stop: Arc::new(AtomicBool::new(false)), thread: Mutex::new(None),
        }
    }
}

#[derive(Default)]
struct NamedLink {
    current: Weak<Shared>,
    // A cancelled replacement can disappear while an earlier worker still holds its leases.
    running: bool,
    waiter: Option<Thread>,
}

fn registry() -> &'static Mutex<HashMap<String, NamedLink>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, NamedLink>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// One worker at a time may enter the protocol code for this configuration name.
struct RunningWorker(String);

impl RunningWorker {
    fn enter(name: &str, shared: &Shared) -> Option<Self> {
        loop {
            {
                let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
                if shared.stop.load(Ordering::Acquire) { return None; }
                let entry = registry.get_mut(name).expect("printer worker is registered");
                if !entry.running {
                    entry.running = true;
                    return Some(Self(name.to_owned()));
                }
                entry.waiter = Some(std::thread::current());
            }
            std::thread::park();
        }
    }
}

impl Drop for RunningWorker {
    fn drop(&mut self) {
        let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        let entry = registry.get_mut(&self.0).expect("running printer worker is registered");
        entry.running = false;
        if let Some(waiter) = entry.waiter.take() { waiter.unpark(); }
    }
}

/// Multiple handles share one session. The last drop requests stop without joining the UI.
pub struct PrinterLink { shared: Arc<Shared>, seen: usize }

impl PrinterLink {
    pub fn start(conf_name: &str) -> Self {
        crate::entries::START.enter();
        Self::start_with(conf_name, run)
    }

    fn start_with(conf_name: &str, run: impl FnOnce(Arc<Shared>, String) + Send + 'static) -> Self {
        let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(shared) = registry.get(conf_name).and_then(|e| e.current.upgrade()).filter(|s| !s.stop.load(Ordering::Acquire)) {
            shared.clients.fetch_add(1, Ordering::Relaxed);
            return Self { shared, seen: 0 };
        }
        let shared = Arc::new(Shared::new(Snapshot {
            name: if valid_name(conf_name) { conf_name.into() } else { "Printer".into() }, ..Default::default()
        }));
        registry.retain(|_, entry| entry.running || entry.current.strong_count() != 0);
        registry.entry(conf_name.to_string()).or_default().current = Arc::downgrade(&shared);
        let worker = shared.clone();
        let name = conf_name.to_string();
        match std::thread::Builder::new().name("fab-printer".into()).spawn(move || {
            if let Some(_running) = RunningWorker::enter(&name, &worker) { run(worker, name); }
        }) {
            Ok(handle) => *shared.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle.thread().clone()),
            Err(_) => {
                let mut snapshot = (**shared.slot.lock().unwrap_or_else(|e| e.into_inner())).clone();
                snapshot.status = ConnectionStatus::Offline("cannot start printer worker");
                publish(&shared, &snapshot);
            }
        }
        Self { shared, seen: 0 }
    }

    /// A simulated printer fed from a file of status reports ([`crate::sim`]): nothing leaves the
    /// machine. Never looks at the printer configurations.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn simulate(path: std::path::PathBuf) -> Self {
        let model = std::env::var("FAB_PRINTER_SIM_MODEL").unwrap_or_else(|_| "Simulated".into());
        Self::simulate_model(path, model)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn simulate_model(path: std::path::PathBuf, model: String) -> Self {
        let family = makepad_fab_settings::family::of_model(&model);
        if family.is_some_and(|f| f.link == makepad_fab_settings::family::Link::Moonraker) {
            return Self::simulate_moonraker(path, &model);
        }
        let shared = Arc::new(Shared::new(Snapshot { name: "Simulated printer".into(), model, ..Default::default() }));
        if family.is_none() {
            let mut snapshot = (**shared.slot.lock().unwrap_or_else(|e| e.into_inner())).clone();
            snapshot.status = ConnectionStatus::Offline(makepad_fab_settings::family::UNSUPPORTED);
            publish(&shared, &snapshot);
            return Self { shared, seen: 0 };
        }
        let worker = shared.clone();
        match std::thread::Builder::new().name("fab-printer-sim".into()).spawn(move || run_sim(worker, path)) {
            Ok(handle) => *shared.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle.thread().clone()),
            Err(_) => {
                let mut snapshot = (**shared.slot.lock().unwrap_or_else(|e| e.into_inner())).clone();
                snapshot.status = ConnectionStatus::Offline("cannot start the simulated printer");
                publish(&shared, &snapshot);
            }
        }
        Self { shared, seen: 0 }
    }

    /// A Moonraker printer in memory, following reports without reading any configuration.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn simulate_moonraker(path: std::path::PathBuf, model: &str) -> Self {
        let shared = Arc::new(Shared::new(Snapshot { name: "Simulated printer".into(), model: model.into(), ..Default::default() }));
        let worker = shared.clone();
        match std::thread::Builder::new().name("fab-printer-sim".into()).spawn(move || crate::moonraker_link::run_sim(worker, path)) {
            Ok(handle) => *shared.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle.thread().clone()),
            Err(_) => {
                let mut snapshot = (**shared.slot.lock().unwrap_or_else(|e| e.into_inner())).clone();
                snapshot.status = ConnectionStatus::Offline("cannot start the simulated printer");
                publish(&shared, &snapshot);
            }
        }
        Self { shared, seen: 0 }
    }

    /// An independent printer with the heads and spools in the supplied layout.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn virtual_printer(setup: crate::VirtualSetup) -> Self {
        let mut snapshot = Snapshot { name: setup.name.clone(), model: setup.model.clone(), virtual_printer: true, ..Default::default() };
        let shared = Arc::new(Shared::new(snapshot.clone()));
        let worker = shared.clone();
        match std::thread::Builder::new().name("fab-printer-virtual".into()).spawn(move || crate::virtual_link::run(worker, setup)) {
            Ok(handle) => *shared.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle.thread().clone()),
            Err(_) => {
                snapshot.status = ConnectionStatus::Offline("cannot start the virtual printer");
                publish(&shared, &snapshot);
            }
        }
        Self { shared, seen: 0 }
    }

    /// Replace this virtual printer's layout. Only the latest pending layout is needed.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_virtual(&self, setup: crate::VirtualSetup) {
        if !self.shared.slot.lock().unwrap_or_else(|e| e.into_inner()).virtual_printer { return; }
        let old = self.shared.pending_virtual.lock().unwrap_or_else(|e| e.into_inner()).replace(setup);
        drop(old);
        if let Some(thread) = &*self.shared.thread.lock().unwrap_or_else(|e| e.into_inner()) { thread.unpark(); }
    }

    /// Set how fast this virtual printer's job runs. Only the latest pending speed is needed.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_virtual_speed(&self, speed: crate::virtual_run::Speed) {
        if !self.shared.slot.lock().unwrap_or_else(|e| e.into_inner()).virtual_printer { return; }
        *self.shared.pending_speed.lock().unwrap_or_else(|e| e.into_inner()) = Some(speed);
        if let Some(thread) = &*self.shared.thread.lock().unwrap_or_else(|e| e.into_inner()) { thread.unpark(); }
    }

    /// Queues a command for the link worker (it answers in the next snapshots).
    pub fn send(&self, command: Command) {
        self.shared.commands.lock().unwrap_or_else(|e| e.into_inner()).push_back(command);
        if let Some(thread) = &*self.shared.thread.lock().unwrap_or_else(|e| e.into_inner()) { thread.unpark(); }
    }

    /// The profile for printer `model` has `heads` heads: a printer of that model that reports
    /// each head on its own is watched for that many.
    pub fn set_heads(&self, model: &str, heads: usize) {
        let told = Some((model.to_string(), heads));
        let changed = std::mem::replace(&mut *self.shared.heads.lock().unwrap_or_else(|e| e.into_inner()), told.clone()) != told;
        if changed {
            if let Some(thread) = &*self.shared.thread.lock().unwrap_or_else(|e| e.into_inner()) { thread.unpark(); }
        }
    }

    /// One atomic check per UI event, then a brief Arc swap under the slot lock.
    pub fn poll(&mut self) -> Option<Arc<Snapshot>> {
        let revision = self.shared.revision.load(Ordering::Acquire);
        if revision == self.seen { return None; }
        let snapshot = self.shared.slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.seen = revision;
        Some(snapshot)
    }
}

impl Drop for PrinterLink {
    fn drop(&mut self) {
        // Serialize last-drop with start so a handle cannot revive a stopped worker.
        let _registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        if self.shared.clients.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shared.stop.store(true, Ordering::Release);
            if let Some(thread) = &*self.shared.thread.lock().unwrap_or_else(|e| e.into_inner()) { thread.unpark(); }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Shared {
    /// The heads to watch on a printer of `model`: what the app said for that model, else none.
    pub(crate) fn heads_of(&self, model: &str) -> usize {
        match &*self.heads.lock().unwrap_or_else(|e| e.into_inner()) {
            Some((told, heads)) if told == model => *heads,
            _ => 0,
        }
    }
    pub(crate) fn queued(&self) -> Vec<Command> { self.commands.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect() }
}

static PUBLISHED: AtomicUsize = AtomicUsize::new(0);

/// The UI checks this counter once per event and calls [`PrinterLink::poll`] on each link only when it changes.
pub fn published() -> usize { PUBLISHED.load(Ordering::Acquire) }

pub(crate) fn publish(shared: &Shared, snapshot: &Snapshot) {
    let next = Arc::new(snapshot.clone());
    let old = std::mem::replace(&mut *shared.slot.lock().unwrap_or_else(|e| e.into_inner()), next);
    drop(old);
    shared.revision.fetch_add(1, Ordering::Release);
    PUBLISHED.fetch_add(1, Ordering::Release);
    SignalToUI::set_ui_signal();
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct SessionLease(String);

#[cfg(not(target_arch = "wasm32"))]
fn active_sessions() -> &'static Mutex<std::collections::HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

#[cfg(not(target_arch = "wasm32"))]
impl SessionLease {
    pub(crate) fn for_printer(key: String) -> Result<Self, ConnectionStatus> {
        Self::take(key).ok_or(ConnectionStatus::Offline("printer already linked in this process"))
    }

    fn take(key: String) -> Option<Self> {
        let inserted = active_sessions().lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone());
        if inserted { Some(Self(key)) } else { None }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for SessionLease {
    fn drop(&mut self) { active_sessions().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0); }
}

/// One background storage transfer (upload or listing): progress and result.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub(crate) struct Transfer {
    pub(crate) sent: std::sync::atomic::AtomicU64,
    pub(crate) cancel: AtomicBool,
    pub(crate) upload: Mutex<Option<Result<(), String>>>,
    pub(crate) stored: Mutex<Option<Result<String, crate::moonraker::Error>>>,
    pub(crate) listing: Mutex<Option<Result<Vec<crate::ftps::Entry>, String>>>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn storage_session(name: &str) -> Result<crate::ftps::Ftps<crate::ftps::PrinterDialer>, String> {
    crate::entries::STORAGE_SESSION.enter();
    use crate::{ftps::{Ftps, PrinterDialer}, secrets::PrinterConfig};
    let config = PrinterConfig::load(name).map_err(str::to_string)?;
    let leaf_pin = crate::tls::pinned_leaf(name).ok_or("the printer's certificate is not pinned yet (connect first)")?;
    let Access::Code(code) = &config.access else { return Err("the printer uses another storage protocol".into()); };
    Ftps::login(PrinterDialer { host: config.host.clone(), leaf_pin }, code).map_err(|e| e.message())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn_transfer(name: &str, work: impl FnOnce(&str, &Transfer) + Send + 'static) -> Option<Arc<Transfer>> {
    let transfer = Arc::new(Transfer::default());
    let (t, name) = (transfer.clone(), name.to_string());
    let worker = std::thread::current();
    std::thread::Builder::new().name("fab-printer-ftps".into()).spawn(move || { work(&name, &t); worker.unpark(); SignalToUI::set_ui_signal(); }).ok()?;
    Some(transfer)
}

/// The printer's storage: one background transfer per call.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) trait Storage {
    fn upload(&self, file: String, job: crate::safety::ValidatedJob) -> Option<Arc<Transfer>>;
    fn list(&self) -> Option<Arc<Transfer>>;
}

/// FTPS to the configured printer.
#[cfg(not(target_arch = "wasm32"))]
struct PrinterStorage(String);

#[cfg(not(target_arch = "wasm32"))]
impl Storage for PrinterStorage {
    fn upload(&self, file: String, job: crate::safety::ValidatedJob) -> Option<Arc<Transfer>> {
        // One upload per printer in this process, through a restart of the link too: an ending
        // upload's cleanup deletes its file by name, which may be the next one's.
        let lease = SessionLease::take(format!("upload:{}", self.0))?;
        spawn_transfer(&self.0, move |name, transfer| {
            let _lease = lease;
            let result = storage_session(name).and_then(|mut ftp| {
                let sent = ftp.upload(&file, &job, &mut |n| {
                    transfer.sent.store(n, Ordering::Relaxed);
                    !transfer.cancel.load(Ordering::Relaxed)
                }).map_err(|e| e.message());
                ftp.quit();
                sent
            });
            *transfer.upload.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        })
    }

    fn list(&self) -> Option<Arc<Transfer>> {
        spawn_transfer(&self.0, |name, transfer| {
            let result = storage_session(name).and_then(|mut ftp| {
                let list = ftp.list("/").map_err(|e| e.message());
                ftp.quit();
                list
            });
            *transfer.listing.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn lost(error: mqtt::Error) -> ConnectionStatus {
    match error {
        mqtt::Error::WrongAccessCode => ConnectionStatus::WrongAccessCode,
        mqtt::Error::Timeout => ConnectionStatus::Offline("printer session timed out"),
        mqtt::Error::Rejected => ConnectionStatus::Offline("printer rejected the session"),
        mqtt::Error::Protocol => ConnectionStatus::Offline("invalid printer MQTT response"),
        mqtt::Error::Transport => ConnectionStatus::Offline("printer connection lost"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
const NOT_CONNECTED: &str = "the printer is not connected";

/// The control command waiting for its answer.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy)]
struct Waiting {
    command: &'static str,
    sequence: u64,
    sent: Instant,
    /// The printer echoed it without saying what became of it.
    echoed: bool,
}

/// One connection's state; it ends with the connection.
#[cfg(not(target_arch = "wasm32"))]
struct Live<S: ByteStream> {
    session: Session<S>,
    code: Secret,
    reports: Reports,
    report_at: Instant,
    resync_at: Option<Instant>,
    waiting: Option<Waiting>,
}

#[cfg(not(target_arch = "wasm32"))]
impl<S: ByteStream> Live<S> {
    fn new(session: Session<S>, code: &Secret, now: Instant) -> Self {
        Self { session, code: Secret::new(code.bytes().to_vec()), reports: Reports::default(), report_at: now, resync_at: None, waiting: None }
    }
}

/// What a link keeps across connections, and what it does with commands, answers and the job.
#[cfg(not(target_arch = "wasm32"))]
struct Worker<T: Storage> {
    snapshot: Snapshot,
    /// Seconds to the next connection attempt.
    backoff: u64,
    /// The snapshot has news besides the job and the last command.
    changed: bool,
    /// Survive reconnects: an upload or a running job keeps its tracker.
    tracker: Option<Tracker>,
    /// The confirmed job, kept for the live re-check right before the start command.
    printing: Option<PrintJob>,
    upload: Option<Arc<Transfer>>,
    listing: Option<Arc<Transfer>>,
    storage: T,
}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Storage> Worker<T> {
    fn new(snapshot: Snapshot, storage: T) -> Self {
        Self { snapshot, backoff: 1, changed: false, tracker: None, printing: None, upload: None, listing: None, storage }
    }

    fn print(&mut self, print: PrintJob) {
        // Each press gets its own answer: what the last one was told no longer holds.
        if matches!(self.snapshot.command, Some(("print", _))) { self.snapshot.command = None; }
        if self.tracker.as_ref().is_some_and(|t| t.status.stage.active()) {
            self.snapshot.command = Some(("print", CommandState::Failed("a job is already on its way".into())));
            return;
        }
        // One transfer at a time: a cancelled upload's cleanup deletes its file by name.
        if self.upload.is_some() {
            self.snapshot.command = Some(("print", CommandState::Failed("the cancelled upload is still closing: press it again in a moment".into())));
            return;
        }
        let refused = if self.snapshot.status != ConnectionStatus::Connected { Some(NOT_CONNECTED.to_string()) } else { job::preflight(&self.snapshot.state, &print) };
        if let Some(why) = refused {
            self.snapshot.job = Some(JobStatus { name: print.job.name().to_string(), file: String::new(), stage: JobStage::Failed(why) });
            return;
        }
        // A new job: the last one's answers are not about it.
        self.snapshot.command = None;
        let file = crate::ftps::job_file_name(print.job.name());
        let mut t = Tracker::new(&print, file.clone());
        self.upload = self.storage.upload(file, print.job.clone());
        if self.upload.is_none() { t.uploaded(Err("cannot start the upload".into())); }
        self.tracker = Some(t);
        self.printing = Some(print);
    }

    /// A command's part that needs no connection. Some: the request to send the printer for it.
    fn command(&mut self, command: Command) -> Option<Request> {
        match command {
            Command::Print(print) => {
                self.snapshot.job_sends += 1;
                self.print(print);
                None
            }
            Command::CheckStorage => {
                if self.listing.is_none() {
                    self.listing = self.storage.list();
                    if self.listing.is_none() { self.snapshot.storage_files = Some(Err("cannot start the listing".into())); self.changed = true; }
                }
                None
            }
            Command::Stop => {
                if let Some(t) = self.tracker.as_mut().filter(|t| matches!(t.status.stage, JobStage::Uploading { .. })) {
                    if let Some(u) = &self.upload { u.cancel.store(true, Ordering::Relaxed); }
                    t.cancel();
                    return None;
                }
                Some(Request::Stop)
            }
            Command::Pause => Some(Request::Pause),
            Command::Resume => Some(Request::Resume),
            Command::ClearError => {
                let error = self.snapshot.state.print_error.filter(|e| *e != 0);
                if error.is_none() { self.snapshot.command = Some(("clean_print_error", CommandState::Failed("not sent: the printer shows no error to clear".into()))); }
                error.map(|print_error| Request::CleanPrintError { print_error })
            }
        }
    }

    /// The storage transfers' progress and results. Some: the start command of a finished upload.
    fn transfers(&mut self) -> Option<crate::mqtt::ProjectFile> {
        if let Some(l) = self.listing.clone() {
            if let Some(result) = l.listing.lock().unwrap_or_else(|e| e.into_inner()).take() {
                self.listing = None;
                self.snapshot.storage_files = Some(result);
                self.changed = true;
            }
        }
        let u = self.upload.clone()?;
        let t = self.tracker.as_mut()?;
        t.uploading(u.sent.load(Ordering::Relaxed));
        let result = {
            let mut result = u.upload.lock().unwrap_or_else(|e| e.into_inner());
            // A good upload starts on this connection's report, never the last one's: it waits.
            if matches!(*result, Some(Ok(()))) && matches!(t.status.stage, JobStage::Uploading { .. }) && self.snapshot.status != ConnectionStatus::Connected { return None; }
            result.take()?
        };
        self.upload = None;
        // Told it was still closing, or already on its way: not any more.
        if matches!(self.snapshot.command, Some(("print", _))) { self.snapshot.command = None; }
        // The printer's live state again, right before the start: still idle, no
        // error, its nozzles those of the checked file, the trays loaded.
        let result = result.and_then(|()| match self.printing.as_ref().map(|p| job::preflight(&self.snapshot.state, p)) {
            Some(None) => Ok(()),
            Some(Some(why)) => Err(format!("uploaded, not started: {why}")),
            None => Err("uploaded, not started: the job is gone".into()),
        });
        match t.uploaded(result) { Effect::Start(project) => Some(project), _ => None }
    }

    /// Shows the job, and lets go of one that ended once its transfer has.
    fn show_job(&mut self) {
        if let Some(t) = self.tracker.as_ref() {
            self.snapshot.job = Some(t.status.clone());
            if !t.status.stage.active() && self.upload.is_none() { self.tracker = None; self.printing = None; }
        }
    }

    /// Between connections nothing reaches the printer: a command is answered now, never sent
    /// when the printer is back. Cancelling an upload needs no printer connection.
    fn offline(&mut self, commands: Vec<Command>) {
        for command in commands {
            if let Some(control) = self.command(command) {
                self.snapshot.command = Some((control.command(), CommandState::Failed(NOT_CONNECTED.into())));
            }
        }
        // Only an upload that failed or was cancelled ends here: `transfers` holds a good one.
        self.transfers();
        self.show_job();
    }

    /// The connection ended, perhaps with a command unanswered.
    fn session_lost(&mut self, waiting: Option<Waiting>) {
        if let Some(w) = waiting {
            self.snapshot.command = Some((w.command, CommandState::Failed("the connection dropped before the printer answered: it may not have arrived".into())));
        }
        // The plate was confirmed clear of the print the printer showed then. What it shows
        // after an outage may be another's, so a job still on its way asks again.
        if let Some(print) = self.printing.as_mut() { print.options.plate_clear = false; }
    }

    /// One pass of a live connection: the UI's commands, the transfers, the printer's next message.
    fn step<S: ByteStream>(&mut self, live: &mut Live<S>, commands: Vec<Command>, now: Instant) -> Result<(), ConnectionStatus> {
        // Every command gets its answer here, whatever becomes of the connection.
        let mut gone = None;
        let mut stop_failed: Option<String> = None;
        for command in commands {
            let Some(control) = self.command(command) else { continue };
            let stop = control == Request::Stop;
            // Nothing goes out over a stop, and what was pressed says so. A waiting stop's end
            // takes its place when it comes; a failed stop's has no later: this carries it.
            let over_a_stop = match &stop_failed {
                _ if stop => None,
                Some(why) => Some(format!("not sent: the stop before it failed: {why}")),
                None => live.waiting.filter(|w| w.command == "stop").map(|_| "not sent: a stop is waiting for the printer's answer".to_string()),
            };
            if let Some(why) = over_a_stop {
                self.snapshot.command = Some((control.command(), CommandState::Failed(why)));
                continue;
            }
            // The session being up is enough for a stop; clearing an error names what the
            // printer reports, so it needs this connection's report.
            let refused = if !crate::printing_supported() { Some(crate::UNSUPPORTED_PRINTING) }
                else if gone.is_some() || !live.session.ready() || (matches!(control, Request::CleanPrintError { .. }) && self.snapshot.status != ConnectionStatus::Connected) { Some(NOT_CONNECTED) }
                else { None };
            if let Some(why) = refused {
                self.snapshot.command = Some((control.command(), CommandState::Failed(why.into())));
                if stop { stop_failed = Some(why.into()); }
                continue;
            }
            match live.session.publish(&control, now) {
                Ok(sequence) => {
                    live.waiting = Some(Waiting { command: control.command(), sequence, sent: now, echoed: false });
                    self.snapshot.command = Some((control.command(), CommandState::Pending));
                    if stop { if let Some(t) = self.tracker.as_mut() { t.stop_requested(); } }
                }
                Err(e) => {
                    live.waiting = None;
                    let why = "the connection dropped: it did not reach the printer";
                    self.snapshot.command = Some((control.command(), CommandState::Failed(why.into())));
                    if stop { stop_failed = Some(why.into()); }
                    let status = lost(e);
                    self.snapshot.status = status.clone();
                    gone = Some(status);
                }
            }
        }
        if let Some(status) = gone { return Err(status); }
        if now.duration_since(live.report_at) >= Duration::from_secs(10) { return Err(ConnectionStatus::Offline("no printer report for 10 seconds")); }
        if let Some(project) = self.transfers() {
            let t = self.tracker.as_mut().expect("a finished upload has its job");
            match live.session.publish(&Request::StartPrint(project), now) {
                Ok(sequence) => t.sent(sequence, now),
                Err(e) => { t.fail("printer connection lost before the start command".into()); return Err(lost(e)); }
            }
        }
        if let Some(w) = live.waiting {
            if now.duration_since(w.sent) >= Duration::from_secs(10) {
                live.waiting = None;
                self.snapshot.command = Some((w.command, CommandState::Failed(if w.echoed { "the printer received it but did not say whether it took it" } else { "no answer from the printer" }.into())));
            }
        }
        match live.session.poll(now) {
            // Subscribed is not reported: the state is still the last connection's until this
            // one's first report, and only that makes the link connected.
            Ok(mqtt::Event::Ready) => {}
            Ok(mqtt::Event::Report(payload)) => {
                let age = live.session.age(now);
                // What the printer says goes to the log, the screen and the job's files without
                // its access code and serial. After JSON decoding: only then is every spelling
                // of them what it is.
                let serial = live.session.serial();
                let private = |s: &str| live.code.redact(s).replace(serial, "***");
                if let Some(shape) = model::answer_shape(&payload, private) {
                    makepad_network::makepad_error_log::log!("fab printer: +{age:.3} s answer {shape}");
                }
                if let Some(mut ack) = model::ack(&payload) {
                    ack.redact(private);
                    if let Some(t) = self.tracker.as_mut() { t.ack(&ack); }
                    if let Some(w) = live.waiting.filter(|w| w.sequence == ack.sequence && w.command == ack.command) {
                        match ack.ok {
                            // Still waiting: a verdict may follow, and otherwise the wait says so.
                            None => live.waiting = Some(Waiting { echoed: true, ..w }),
                            Some(true) => {
                                live.waiting = None;
                                self.snapshot.command = Some((w.command, CommandState::Done));
                            }
                            Some(false) => {
                                live.waiting = None;
                                self.snapshot.command = Some((w.command, CommandState::Failed(ack.reason.clone().unwrap_or_else(|| "refused".into()))));
                                if w.command == "stop" { if let Some(t) = self.tracker.as_mut() { t.stop_refused(); } }
                            }
                        }
                    }
                }
                match live.reports.apply(&payload) {
                    Ok(Some(mut state)) => {
                        state.redact(private);
                        live.report_at = now;
                        self.backoff = 1;
                        self.snapshot.state = Arc::new(state);
                        self.snapshot.status = ConnectionStatus::Connected;
                        self.changed = true;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        // Coalesce malformed reports; the watchdog reconnects if resync fails.
                        if live.resync_at.is_none_or(|at| now.duration_since(at) >= Duration::from_secs(1)) {
                            live.session.publish(&Request::PushAll, now).map_err(lost)?;
                            live.resync_at = Some(now);
                        }
                    }
                }
            }
            Ok(mqtt::Event::Idle) => {}
            Err(e) => return Err(lost(e)),
        }
        // Only this connection's reports say what became of the job.
        if self.snapshot.status == ConnectionStatus::Connected {
            if let Some(t) = self.tracker.as_mut() {
                if t.advance(&self.snapshot.state, now) == Effect::PushAll {
                    live.session.publish(&Request::PushAll, now).map_err(lost)?;
                }
            }
        }
        self.show_job();
        Ok(())
    }
}

/// One connection, from its session to its end.
#[cfg(not(target_arch = "wasm32"))]
fn connected<S: ByteStream, T: Storage>(shared: &Shared, worker: &mut Worker<T>, session: Session<S>, code: &Secret) -> Result<(), ConnectionStatus> {
    let mut live = Live::new(session, code, Instant::now());
    let outcome = loop {
        if shared.stop.load(Ordering::Acquire) { break Ok(()); }
        let before = (worker.snapshot.job.clone(), worker.snapshot.command.clone());
        if let Err(status) = worker.step(&mut live, shared.queued(), Instant::now()) { break Err(status); }
        if std::mem::take(&mut worker.changed) || (worker.snapshot.job.clone(), worker.snapshot.command.clone()) != before { publish(shared, &worker.snapshot); }
    };
    worker.session_lost(live.waiting.take());
    let _ = live.session.disconnect();
    outcome
}

/// The shared reconnect loop, with each protocol answering its own offline commands.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) trait Driven {
    fn snapshot(&mut self) -> &mut Snapshot;
    fn backoff(&mut self) -> &mut u64;
    fn take_changed(&mut self) -> bool;
    fn offline(&mut self, commands: Vec<Command>);
    fn cancel_transfer(&self);
    fn poll_interval(&self) -> Option<Duration> { None }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Storage> Driven for Worker<T> {
    fn snapshot(&mut self) -> &mut Snapshot { &mut self.snapshot }
    fn backoff(&mut self) -> &mut u64 { &mut self.backoff }
    fn take_changed(&mut self) -> bool { std::mem::take(&mut self.changed) }
    fn offline(&mut self, commands: Vec<Command>) { Worker::offline(self, commands); }
    fn cancel_transfer(&self) {
        if let Some(u) = &self.upload { u.cancel.store(true, Ordering::Relaxed); }
    }
}

/// A link's life, whatever it links to: an attempt at a connection, then the wait for the next.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn drive<W: Driven>(shared: &Shared, worker: &mut W, mut attempt: impl FnMut(&mut W) -> Result<(), ConnectionStatus>) {
    while !shared.stop.load(Ordering::Acquire) {
        worker.snapshot().status = ConnectionStatus::Connecting;
        worker.offline(shared.queued());
        publish(shared, worker.snapshot());
        let outcome = attempt(worker);
        if shared.stop.load(Ordering::Acquire) { break; }
        if let Err(status) = outcome { worker.snapshot().status = status; }
        worker.offline(shared.queued());
        publish(shared, worker.snapshot());
        // The wait before the next attempt; what is pressed in it gets its answer at once.
        let retry = Instant::now() + Duration::from_secs(*worker.backoff());
        loop {
            let left = retry.saturating_duration_since(Instant::now());
            if left.is_zero() || shared.stop.load(Ordering::Acquire) { break; }
            std::thread::park_timeout(worker.poll_interval().map_or(left, |interval| left.min(interval)));
            let before = (worker.snapshot().job.clone(), worker.snapshot().command.clone());
            worker.offline(shared.queued());
            if worker.take_changed() || (worker.snapshot().job.clone(), worker.snapshot().command.clone()) != before { publish(shared, worker.snapshot()); }
        }
        *worker.backoff() = (*worker.backoff() * 2).min(30);
    }
    worker.cancel_transfer();
}

#[cfg(not(target_arch = "wasm32"))]
fn run(shared: Arc<Shared>, name: String) {
    if PrinterConfig::load(&name).is_ok_and(|config| matches!(config.access, Access::Moonraker { .. })) {
        return crate::moonraker_link::run(shared, name);
    }
    let snapshot = (**shared.slot.lock().unwrap_or_else(|e| e.into_inner())).clone();
    let mut worker = Worker::new(snapshot, PrinterStorage(name.clone()));
    drive(&shared, &mut worker, |worker| {
        let config = PrinterConfig::load(&name).map_err(ConnectionStatus::Offline)?;
        let Access::Code(code) = &config.access else { return Err(ConnectionStatus::Offline("printer configuration changed protocol")); };
        worker.snapshot.model = config.model.clone();
        worker.snapshot.name = code.redact(&name);
        publish(&shared, &worker.snapshot);
        // Reserve both aliases and certificate identity before MQTT authentication.
        let _host = SessionLease::for_printer(format!("host:{}", config.host.to_ascii_lowercase()))?;
        let (stream, serial) = tls::connect(&name, &config).map_err(|e| match e {
            tls::ConnectError::PinMismatch => ConnectionStatus::PinMismatch,
            tls::ConnectError::Offline(reason) => ConnectionStatus::Offline(reason),
        })?;
        if shared.stop.load(Ordering::Acquire) { return Ok(()); }
        let _serial = SessionLease::for_printer(format!("serial:{serial}"))?;
        worker.snapshot.serial_suffix = Some(code.redact(&serial).chars().rev().take(4).collect::<String>().chars().rev().collect());
        let session = Session::connect(stream, &serial, code, Instant::now()).map_err(lost)?;
        connected(&shared, worker, session, code)
    });
}

/// The simulated printer's link: the real one, over a connection and a storage in memory
/// ([`crate::sim`]).
#[cfg(not(target_arch = "wasm32"))]
fn run_sim(shared: Arc<Shared>, path: std::path::PathBuf) {
    use crate::sim::{SimPrinter, SimStorage, SimStream};
    let snapshot = (**shared.slot.lock().unwrap_or_else(|e| e.into_inner())).clone();
    let printer = std::rc::Rc::new(std::cell::RefCell::new(SimPrinter::new(path)));
    let mut worker = Worker::new(snapshot, SimStorage(printer.clone()));
    let code = Secret::new(b"SIMULATED".to_vec());
    drive(&shared, &mut worker, |worker| {
        let stream = SimStream::connect(&printer).map_err(ConnectionStatus::Offline)?;
        let session = Session::connect(stream, mqtt::fake::SERIAL, &code, Instant::now()).map_err(lost)?;
        connected(&shared, worker, session, &code)
    });
}

#[cfg(target_arch = "wasm32")]
fn run(shared: Arc<Shared>, _name: String) {
    let mut snapshot = (**shared.slot.lock().unwrap_or_else(|e| e.into_inner())).clone();
    snapshot.status = ConnectionStatus::Offline("local printer sockets are unavailable in the browser");
    publish(&shared, &snapshot);
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::model::TrayId;
    use crate::mqtt::fake::{FakePrinter, SERIAL};
    use makepad_strict_json::{parse, Value};
    use std::cell::RefCell;

    fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(Instant::now() < deadline, "waiting for {what}");
            std::thread::park_timeout(Duration::from_millis(1));
        }
    }

    #[test]
    fn either_printer_publishes_without_changing_the_other() {
        let start = |name: &str| {
            let mut link = PrinterLink::virtual_printer(crate::VirtualSetup {
                name: name.into(), model: "Virtual".into(), numbered_heads: false, nozzle_diameters: vec![0.4], trays: Vec::new(),
            });
            // Drain the worker's first publication, after the initial Connecting revision.
            wait_until("first publication", || link.shared.revision.load(Ordering::Acquire) != 1);
            assert_eq!(link.poll().unwrap().status, ConnectionStatus::Connected);
            link
        };
        let mut first = start("First");
        let mut second = start("Second");
        let check = |changed: &mut PrinterLink, quiet: &mut PrinterLink| {
            let before = crate::published();
            changed.send(Command::CheckStorage);
            wait_until("published storage answer", || {
                crate::published() != before && changed.poll().is_some_and(|s| s.storage_files.is_some())
            });
            assert!(quiet.poll().is_none());
        };
        check(&mut first, &mut second);
        check(&mut second, &mut first);
    }

    #[test]
    fn only_the_real_link_enters_transport() {
        // Other unit tests use the memory transports in parallel. Count in a process of our own.
        const CHILD: &str = "FAB_LINK_ENTRY_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "link::tests::only_the_real_link_enters_transport", "--nocapture"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success(), "isolated entry counter test failed");
            return;
        }

        const NAME: &str = "entry-fixture";
        let before = crate::entry_counts();
        let mut peer = PrinterLink::start_with(NAME, |shared, _| {
            let printer = FakePrinter::default();
            let _session = Session::connect(printer, SERIAL, &Secret::new(CODE.to_vec()), Instant::now()).unwrap();
            publish(&shared, &Snapshot { status: ConnectionStatus::Connected, ..Default::default() });
        });
        // The public real constructor reuses the registered memory worker.
        let real = PrinterLink::start(NAME);
        wait_until("memory connection", || peer.poll().is_some_and(|s| s.status == ConnectionStatus::Connected));
        let after = crate::entry_counts();
        assert_eq!(after.start, before.start + 1);
        assert_eq!(after.mqtt_connect, before.mqtt_connect + 1);
        drop((real, peer));

        let mut link = PrinterLink::virtual_printer(crate::VirtualSetup {
            name: "Virtual fixture".into(), model: "Fixture".into(), numbered_heads: false, nozzle_diameters: vec![0.4], trays: Vec::new(),
        });
        link.send(Command::CheckStorage);
        wait_until("virtual storage answer", || link.poll().is_some_and(|s| s.virtual_printer && s.storage_files.is_some()));
        assert_eq!(crate::entry_counts(), after, "a virtual link entered a real printer path");
    }

    #[test]
    fn restarting_the_same_name_waits_for_the_retiring_worker() {
        use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
        const NAME: &str = "relink-fixture";
        fn start_sim(closing: Option<Receiver<()>>) -> PrinterLink {
            PrinterLink::start_with(NAME, move |shared, _| {
                let code = Secret::new(CODE.to_vec());
                let mut worker = Worker::new(Snapshot::default(), FakeStorage::default());
                drive(&shared, &mut worker, |worker| {
                    let _host = SessionLease::for_printer("host:relink.invalid".into())?;
                    let _serial = SessionLease::for_printer(format!("serial:{SERIAL}"))?;
                    let printer = FakePrinter::default();
                    let session = Session::connect(printer.clone(), SERIAL, &code, Instant::now()).map_err(lost)?;
                    printer.report(&report("IDLE", ""));
                    let result = connected(&shared, worker, session, &code);
                    // Let the test finish the retiring session while it still holds both leases.
                    if let Some(closing) = &closing { let _ = closing.recv_timeout(Duration::from_secs(10)); }
                    result
                });
            })
        }
        fn wait_for_turn(link: &PrinterLink) {
            let id = link.shared.thread.lock().unwrap().as_ref().unwrap().id();
            wait_until("replacement to wait for the old worker", || {
                registry().lock().unwrap()[NAME].waiter.as_ref().is_some_and(|w| w.id() == id)
            });
        }
        let (release, closing) = channel();
        let mut first = start_sim(Some(closing));
        let other_handle = PrinterLink::start(NAME);
        wait_until("first connection", || first.poll().is_some_and(|s| s.status == ConnectionStatus::Connected));
        let old_worker = Arc::downgrade(&first.shared);
        drop((first, other_handle));

        let (entered, ran) = channel();
        let cancelled = PrinterLink::start_with(NAME, move |_, _| { let _ = entered.send(()); });
        let cancelled_worker = Arc::downgrade(&cancelled.shared);
        wait_for_turn(&cancelled);
        drop(cancelled);
        assert_eq!(ran.recv_timeout(Duration::from_secs(10)), Err(RecvTimeoutError::Disconnected),
            "dropping the waiting link must discard its connection routine");
        wait_until("cancelled worker to exit", || cancelled_worker.strong_count() == 0);

        let mut next = start_sim(None);
        wait_for_turn(&next);
        assert_eq!(next.poll().unwrap().status, ConnectionStatus::Connecting);
        release.send(()).unwrap();
        wait_until("replacement connection", || next.poll().is_some_and(|s| {
            assert_ne!(s.status, ConnectionStatus::Offline("printer already linked in this process"));
            s.status == ConnectionStatus::Connected
        }));
        assert_eq!(old_worker.strong_count(), 0, "the replacement connected before the old worker exited");
    }

    #[test]
    fn rejected_duplicate_cannot_release_an_active_session() {
        let key = "serial:TESTSERIAL0001".to_string();
        let first = SessionLease::take(key.clone()).unwrap();
        assert!(SessionLease::take(key.clone()).is_none());
        assert!(SessionLease::take(key.clone()).is_none());
        drop(first);
        assert!(SessionLease::take(key).is_some());
    }

    const CODE: &[u8] = b"TESTCODE";

    fn report(state: &str, job: &str) -> String {
        format!(r#"{{"print":{{"command":"push_status","msg":0,"gcode_state":"{state}","subtask_name":"{job}","sdcard":true,"device":{{"nozzle":{{"info":[{{"id":0,"diameter":0.4}},{{"id":1,"diameter":0.4}}]}}}},"ams":{{"tray_exist_bits":"1","ams":[{{"id":"0","info":"1003","tray":[{{"id":"0","tray_type":"PLA","tray_color":"FF0000FF"}},{{"id":"1"}}]}}]}}}}}}"#)
    }

    fn cube() -> Command {
        Command::Print(PrintJob { job: crate::safety::tests::checked_square("cube"), trays: vec![Some(TrayId { ams: 0, slot: 0 })], used: vec![0], options: Default::default() })
    }

    /// Transfers that do nothing until a test ends them.
    #[derive(Default)]
    struct FakeStorage { uploads: RefCell<Vec<Arc<Transfer>>> }

    impl Storage for FakeStorage {
        fn upload(&self, _file: String, _job: crate::safety::ValidatedJob) -> Option<Arc<Transfer>> {
            let transfer = Arc::new(Transfer::default());
            self.uploads.borrow_mut().push(transfer.clone());
            Some(transfer)
        }
        fn list(&self) -> Option<Arc<Transfer>> { None }
    }

    /// The worker on a connection to a memory printer, with the clock in the test's hands.
    struct Rig { worker: Worker<FakeStorage>, printer: FakePrinter, live: Live<FakePrinter>, now: Instant }

    fn live(printer: &FakePrinter, code: &[u8], now: Instant) -> Live<FakePrinter> {
        let code = Secret::new(code.to_vec());
        Live::new(Session::connect(printer.clone(), SERIAL, &code, now).unwrap(), &code, now)
    }

    impl Rig {
        /// Subscribed, with no report yet.
        fn with_code(code: &[u8]) -> Self {
            let (now, printer) = (Instant::now(), FakePrinter::default());
            let live = live(&printer, code, now);
            let mut rig = Self { worker: Worker::new(Snapshot::default(), FakeStorage::default()), printer, live, now };
            rig.settle();
            rig
        }
        /// Connected to a printer that has reported `state`; None where printing is unsupported.
        fn reporting(state: &str, job: &str) -> Option<Self> {
            if !crate::printing_supported() { return None; }
            let mut rig = Self::with_code(CODE);
            rig.report(&report(state, job));
            Some(rig)
        }
        fn step(&mut self, commands: Vec<Command>) -> Result<(), ConnectionStatus> { self.worker.step(&mut self.live, commands, self.now) }
        fn press(&mut self, command: Command) { self.step(vec![command]).unwrap(); }
        /// Reads what the printer has said so far.
        fn settle(&mut self) { for _ in 0..8 { self.step(Vec::new()).unwrap(); } }
        fn report(&mut self, json: &str) { self.printer.report(json); self.settle(); }
        /// The connection breaks; the worker is between connections.
        fn disconnect(&mut self) {
            self.printer.drop_link();
            self.worker.session_lost(self.live.waiting.take());
            self.worker.snapshot.status = ConnectionStatus::Offline("printer connection lost");
        }
        /// A new connection, subscribed, with no report yet.
        fn reconnect(&mut self) {
            self.worker.snapshot.status = ConnectionStatus::Connecting;
            self.printer = FakePrinter::default();
            self.live = live(&self.printer, CODE, self.now);
            self.settle();
        }
        /// The sequence ids `command` went out with on this connection.
        fn sent(&self, command: &str) -> Vec<String> {
            self.printer.published().iter().filter_map(|json| {
                let request = parse(json.as_bytes()).unwrap();
                let print = request.get("print")?;
                (print.get("command").and_then(Value::as_str) == Some(command)).then(|| print.get("sequence_id").and_then(Value::as_str).unwrap().to_string())
            }).collect()
        }
        /// The printer answers the last `command` with these extra fields.
        fn answer(&mut self, command: &str, fields: &str) {
            let sequence = self.sent(command).pop().expect("the command went out");
            self.report(&format!(r#"{{"print":{{"command":"{command}","sequence_id":"{sequence}"{fields}}}}}"#));
        }
        fn shown(&self) -> (&'static str, CommandState) { self.worker.snapshot.command.clone().expect("a command is shown") }
        fn stage(&self) -> JobStage { self.worker.snapshot.job.as_ref().expect("a job").stage.clone() }
        fn upload(&self, i: usize) -> Arc<Transfer> { self.worker.storage.uploads.borrow()[i].clone() }
        fn upload_ends(&self, i: usize, result: Result<(), String>) { *self.upload(i).upload.lock().unwrap() = Some(result); }
        /// A job up to the printer running it.
        fn printing(&mut self) {
            self.press(cube());
            self.upload_ends(0, Ok(()));
            self.settle();
            self.answer("project_file", r#","result":"success""#);
            self.report(&report("RUNNING", "cube"));
            assert_eq!(self.stage(), JobStage::Printing);
        }
    }

    fn failed(state: CommandState) -> String {
        match state { CommandState::Failed(why) => why, other => panic!("not a failure: {other:?}") }
    }

    // ---- Stop (and the other controls) always get an answer ----

    #[test]
    fn a_stop_that_cannot_go_out_says_so_and_the_rest_of_the_batch_too() {
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.printer.drop_link();
        assert!(rig.step(vec![Command::Stop, Command::Pause]).is_err(), "the session ends");
        let (command, state) = rig.shown();
        assert_eq!(command, "pause");
        let why = failed(state);
        assert!(why.contains("not sent") && why.contains("did not reach the printer"), "it says what became of the stop too: {why}");
        assert!(rig.sent("pause").is_empty());
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.printer.drop_link();
        assert!(rig.step(vec![Command::Pause, Command::Stop, cube()]).is_err());
        let (command, state) = rig.shown();
        assert_eq!(command, "stop");
        assert!(failed(state).contains("not connected"));
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("not connected")), "a print pressed with it is answered too");
    }

    #[test]
    fn a_stop_unanswered_when_the_connection_drops_does_not_wait_forever() {
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.press(Command::Stop);
        assert_eq!(rig.sent("stop").len(), 1);
        assert_eq!(rig.shown(), ("stop", CommandState::Pending));
        rig.disconnect();
        assert!(failed(rig.shown().1).contains("connection dropped"));
        rig.reconnect();
        rig.report(&report("RUNNING", "cube"));
        assert!(matches!(rig.shown().1, CommandState::Failed(_)), "it stays said");
        assert!(rig.sent("stop").is_empty(), "a lost stop is not sent again by itself");
    }

    #[test]
    fn a_report_watchdog_loses_no_command() {
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.now += Duration::from_secs(10);
        assert!(rig.step(vec![Command::Stop]).is_err(), "no report for 10 seconds ends the session");
        assert_eq!(rig.sent("stop").len(), 1, "the stop still went out");
        rig.worker.session_lost(rig.live.waiting.take());
        assert!(matches!(rig.shown(), ("stop", CommandState::Failed(_))));
    }

    #[test]
    fn between_connections_commands_are_answered_now_and_never_sent_later() {
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.disconnect();
        rig.worker.offline(vec![Command::Stop]);
        let (command, state) = rig.shown();
        assert_eq!(command, "stop");
        assert!(failed(state).contains("not connected"));
        rig.worker.offline(vec![cube()]);
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("not connected")));
        rig.reconnect();
        rig.report(&report("RUNNING", "cube"));
        assert!(rig.sent("stop").is_empty());
        assert!(rig.worker.storage.uploads.borrow().is_empty(), "a print pressed while offline never starts later");
    }

    #[test]
    fn an_upload_cancels_without_the_printer_connection() {
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        assert!(matches!(rig.stage(), JobStage::Uploading { .. }));
        rig.disconnect();
        rig.worker.offline(vec![Command::Stop]);
        assert!(rig.upload(0).cancel.load(Ordering::Relaxed));
        assert_eq!(rig.stage(), JobStage::Cancelled);
        rig.upload_ends(0, Err("upload cancelled".into()));
        rig.worker.offline(Vec::new());
        assert!(rig.worker.upload.is_none() && rig.worker.tracker.is_none(), "the transfer ended; nothing waits for a connection");
    }

    #[test]
    fn what_is_pressed_over_a_waiting_stop_says_it_was_not_sent() {
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.report(&report("RUNNING", "cube").replace("\"msg\":0", "\"msg\":0,\"print_error\":7"));
        rig.press(Command::Pause);
        rig.press(Command::Stop);
        assert_eq!(rig.shown(), ("stop", CommandState::Pending), "a stop replaces a waiting pause");
        for (press, name) in [(Command::Resume, "resume"), (Command::Pause, "pause"), (Command::ClearError, "clean_print_error")] {
            let before = rig.sent(name).len();
            rig.press(press);
            assert_eq!(rig.sent(name).len(), before, "nothing goes out over a waiting stop");
            let (command, state) = rig.shown();
            assert_eq!(command, name);
            assert!(failed(state).contains("not sent: a stop is waiting"));
        }
        // The stop's own end takes its place, however it ends.
        rig.answer("stop", r#","result":"fail","reason":"busy""#);
        assert_eq!(rig.shown(), ("stop", CommandState::Failed("busy".into())));
        rig.press(Command::Pause);
        assert_eq!(rig.shown(), ("pause", CommandState::Pending), "no stop waits now: it goes out");

        rig.press(Command::Stop);
        rig.press(Command::Pause);
        rig.answer("stop", r#","result":"success""#);
        assert_eq!(rig.shown(), ("stop", CommandState::Done));

        rig.press(Command::Stop);
        rig.press(Command::Pause);
        rig.now += Duration::from_secs(5);
        rig.report(&report("RUNNING", "cube"));
        rig.now += Duration::from_secs(5);
        rig.report(&report("RUNNING", "cube"));
        assert!(matches!(rig.shown(), ("stop", CommandState::Failed(why)) if why.contains("no answer")));

        rig.press(Command::Stop);
        rig.press(Command::Pause);
        rig.disconnect();
        assert!(matches!(rig.shown(), ("stop", CommandState::Failed(why)) if why.contains("connection dropped")));
    }

    #[test]
    fn a_clear_with_no_error_to_clear_says_so() {
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.press(Command::ClearError);
        assert!(rig.sent("clean_print_error").is_empty());
        assert_eq!(rig.shown().0, "clean_print_error");
        assert!(failed(rig.shown().1).contains("no error to clear"));
    }

    #[test]
    fn only_a_stop_that_went_out_unrefused_makes_the_end_a_stop() {
        let failed_report = report("FAILED", "cube");
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.printing();
        rig.press(Command::Stop);
        rig.answer("stop", r#","result":"fail""#);
        rig.report(&failed_report);
        assert!(matches!(rig.stage(), JobStage::Failed(_)), "refused, then the print failed by itself: {:?}", rig.stage());

        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.printing();
        rig.printer.drop_link();
        assert!(rig.step(vec![Command::Stop]).is_err());
        rig.reconnect();
        rig.report(&failed_report);
        assert!(matches!(rig.stage(), JobStage::Failed(_)), "never sent: {:?}", rig.stage());

        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.printing();
        rig.press(Command::Stop);
        rig.answer("stop", r#","result":"success""#);
        rig.report(&failed_report);
        assert_eq!(rig.stage(), JobStage::Cancelled);
    }

    // ---- An answer counts for what it says ----

    #[test]
    fn an_error_answer_without_a_result_is_a_refusal() {
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.press(Command::Stop);
        rig.answer("stop", r#","err_code":42"#);
        assert_eq!(failed(rig.shown().1), "42");

        // An echo says neither: still waiting, then plainly not confirmed.
        rig.press(Command::Stop);
        rig.answer("stop", "");
        assert_eq!(rig.shown(), ("stop", CommandState::Pending));
        rig.now += Duration::from_secs(5);
        rig.report(&report("RUNNING", "cube"));
        rig.now += Duration::from_secs(5);
        rig.report(&report("RUNNING", "cube"));
        assert!(failed(rig.shown().1).contains("did not say"));

        rig.press(Command::Stop);
        rig.answer("stop", r#","result":"SUCCESS","reason":"SUCCESS""#);
        assert_eq!(rig.shown(), ("stop", CommandState::Done));

        // The start command the same way.
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.upload_ends(0, Ok(()));
        rig.settle();
        assert_eq!(rig.sent("project_file").len(), 1);
        rig.answer("project_file", "");
        assert_eq!(rig.stage(), JobStage::Starting, "an echo is no acceptance");
        rig.answer("project_file", r#","err_code":42"#);
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("42")));
    }

    #[test]
    fn an_echoed_start_is_taken_when_the_printer_reports_it() {
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.upload_ends(0, Ok(()));
        rig.settle();
        rig.answer("project_file", "");
        rig.report(&report("RUNNING", "cube"));
        assert_eq!(rig.stage(), JobStage::Printing);
    }

    // ---- Whose answer it is ----

    #[test]
    fn an_answer_to_the_last_connections_command_does_not_count_for_this_ones() {
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.press(Command::Stop);
        let old = rig.sent("stop").pop().unwrap();
        rig.disconnect();
        rig.reconnect();
        rig.report(&report("RUNNING", "cube"));
        rig.press(Command::Stop);
        let new = rig.sent("stop").pop().unwrap();
        assert_ne!(new, old, "a new connection's command has an id of its own");
        rig.report(&format!(r#"{{"print":{{"command":"stop","sequence_id":"{old}","result":"success"}}}}"#));
        assert_eq!(rig.shown(), ("stop", CommandState::Pending), "the old stop's answer says nothing about this one");
        rig.answer("stop", r#","result":"success""#);
        assert_eq!(rig.shown(), ("stop", CommandState::Done));
    }

    #[test]
    fn another_clients_answers_say_nothing_about_ours() {
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.upload_ends(0, Ok(()));
        rig.settle();
        let start = rig.sent("project_file").pop().unwrap();
        // The ids its own commands went out with are not ours, even next to ours.
        let other = start.parse::<u64>().unwrap() + 1_000_000;
        rig.report(&format!(r#"{{"print":{{"command":"project_file","sequence_id":"{other}","result":"fail","reason":"another client's"}}}}"#));
        assert_eq!(rig.stage(), JobStage::Starting);
        rig.answer("project_file", r#","result":"success""#);
        assert_eq!(rig.stage(), JobStage::Accepted);
        rig.report(&report("RUNNING", "cube"));
        rig.press(Command::Stop);
        rig.report(&format!(r#"{{"print":{{"command":"stop","sequence_id":"{other}","result":"success"}}}}"#));
        assert_eq!(rig.shown(), ("stop", CommandState::Pending));
    }

    // ---- Readiness is this connection's ----

    #[test]
    fn after_a_reconnect_nothing_starts_on_the_last_connections_report() {
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.disconnect();
        rig.upload_ends(0, Ok(()));
        rig.worker.offline(Vec::new());
        assert!(matches!(rig.stage(), JobStage::Uploading { .. }), "a finished upload waits");
        rig.reconnect();
        assert_ne!(rig.worker.snapshot.status, ConnectionStatus::Connected, "subscribed is not reported");
        assert!(rig.sent("project_file").is_empty(), "the old IDLE starts nothing");
        rig.report(&report("PAUSE", "another"));
        assert!(rig.sent("project_file").is_empty());
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("busy")), "the fresh report refuses it");

        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.disconnect();
        rig.upload_ends(0, Ok(()));
        rig.reconnect();
        rig.report(&report("IDLE", ""));
        assert_eq!(rig.sent("project_file").len(), 1, "a fresh IDLE starts it, once");
        rig.settle();
        assert_eq!(rig.sent("project_file").len(), 1);
    }

    #[test]
    fn a_plate_confirmed_before_an_outage_does_not_count_after_it() {
        // Up to the upload ending: the last print's plate confirmed clear for this printer.
        fn confirmed() -> Option<Rig> {
            let mut rig = Rig::reporting("FINISH", "last")?;
            let Command::Print(mut print) = cube() else { unreachable!() };
            print.options.plate_clear = true;
            rig.press(Command::Print(print));
            assert!(matches!(rig.stage(), JobStage::Uploading { .. }));
            Some(rig)
        }
        // With no outage the confirmation stands.
        let Some(mut rig) = confirmed() else { return };
        rig.upload_ends(0, Ok(()));
        rig.settle();
        assert_eq!(rig.sent("project_file").len(), 1);

        // After one the printer's FINISH may be another print's: the plate is unknown again.
        let Some(mut rig) = confirmed() else { return };
        rig.disconnect();
        rig.upload_ends(0, Ok(()));
        rig.reconnect();
        rig.report(&report("FINISH", "last"));
        rig.settle();
        assert!(rig.sent("project_file").is_empty(), "a confirmation from before the outage starts nothing");
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("uploaded, not started") && why.contains("confirm the plate is clear")), "{:?}", rig.stage());

        // A failed print's the same way, and a dropped report stream is an outage too.
        let Some(mut rig) = confirmed() else { return };
        rig.now += Duration::from_secs(10);
        assert!(rig.step(Vec::new()).is_err(), "no report for 10 seconds ends the session");
        rig.worker.session_lost(rig.live.waiting.take());
        rig.upload_ends(0, Ok(()));
        rig.reconnect();
        rig.report(&report("FAILED", "last"));
        assert!(rig.sent("project_file").is_empty());
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("confirm the plate is clear")));

        // An idle printer asks for no confirmation: what an outage does to that upload is unchanged.
        let Some(mut rig) = confirmed() else { return };
        rig.disconnect();
        rig.upload_ends(0, Ok(()));
        rig.reconnect();
        rig.report(&report("IDLE", ""));
        assert_eq!(rig.sent("project_file").len(), 1);
    }

    #[test]
    fn a_print_pressed_before_the_first_report_is_refused() {
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.disconnect();
        rig.reconnect();
        rig.press(cube());
        assert!(rig.worker.storage.uploads.borrow().is_empty());
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("not connected")));
    }

    #[test]
    fn a_stop_goes_out_before_the_first_report_but_a_clear_error_does_not() {
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.report(&report("RUNNING", "cube").replace("\"msg\":0", "\"msg\":0,\"print_error\":7"));
        rig.disconnect();
        rig.reconnect();
        rig.press(Command::ClearError);
        assert!(rig.sent("clean_print_error").is_empty(), "the error it would clear is the last connection's");
        assert!(failed(rig.shown().1).contains("not connected"));
        rig.press(Command::Stop);
        assert_eq!(rig.sent("stop").len(), 1, "the session is up: a stop need not wait");
    }

    #[test]
    fn a_long_outage_fails_no_job_before_the_printer_says() {
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.upload_ends(0, Ok(()));
        rig.settle();
        assert_eq!(rig.stage(), JobStage::Starting);
        rig.disconnect();
        rig.now += Duration::from_secs(100);
        rig.reconnect();
        assert_eq!(rig.stage(), JobStage::Starting, "no report yet: nothing is known");
        rig.report(&report("RUNNING", "cube"));
        assert_eq!(rig.stage(), JobStage::Printing);
    }

    // ---- One upload at a time ----

    #[test]
    fn a_retry_waits_for_the_cancelled_upload_to_end() {
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.press(Command::Stop);
        assert!(rig.upload(0).cancel.load(Ordering::Relaxed));
        assert_eq!(rig.stage(), JobStage::Cancelled);
        rig.press(cube());
        assert_eq!(rig.worker.storage.uploads.borrow().len(), 1, "its cleanup deletes the file by name: no second upload yet");
        let (command, state) = rig.shown();
        assert_eq!(command, "print");
        assert!(failed(state).contains("still closing"));
        rig.upload_ends(0, Err("upload cancelled".into()));
        rig.settle();
        assert_eq!(rig.worker.snapshot.command, None, "it has closed: nothing says it is still closing");
        rig.press(cube());
        assert_eq!(rig.worker.storage.uploads.borrow().len(), 2);
        assert!(matches!(rig.stage(), JobStage::Uploading { .. }));
    }

    #[test]
    fn what_a_print_was_told_does_not_outlast_it() {
        // Refused again for another reason: only that reason shows.
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.press(cube());
        assert!(failed(rig.shown().1).contains("already on its way"));
        rig.upload_ends(0, Ok(()));
        rig.settle();
        rig.answer("project_file", r#","result":"fail","reason":"no""#);
        assert!(matches!(rig.stage(), JobStage::Failed(_)));
        rig.settle();
        rig.report(&report("FINISH", "cube"));
        rig.press(cube());
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("confirm the plate")));
        assert_eq!(rig.worker.snapshot.command, None);

        // A stop's failure stays through a refused print, and a new job does not inherit it.
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.press(Command::Stop);
        rig.answer("stop", r#","result":"fail","reason":"busy""#);
        rig.press(cube());
        assert_eq!(rig.shown(), ("stop", CommandState::Failed("busy".into())), "still about the print that runs");
        rig.report(&report("IDLE", ""));
        rig.press(cube());
        assert!(matches!(rig.stage(), JobStage::Uploading { .. }));
        assert_eq!(rig.worker.snapshot.command, None, "the last print's answers are not about this one");
    }

    // ---- What a real printer really answers ----

    #[test]
    fn what_goes_out_and_every_answer_go_in_the_log_with_the_secrets_out() {
        use makepad_network::makepad_error_log::{set_log_tap, LogLevel};
        static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
        fn tap(line: &str, _: LogLevel) { LINES.lock().unwrap_or_else(|e| e.into_inner()).push(line.to_string()); }
        // A warm test process runs this again with its statics as they were.
        LINES.lock().unwrap_or_else(|e| e.into_inner()).clear();
        set_log_tap(Some(tap));
        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.upload_ends(0, Ok(()));
        rig.settle();
        rig.now += Duration::from_millis(250);
        rig.answer("project_file", &format!(r#","result":"fail","reason":"logged \u0054ESTCODE {SERIAL}","url":"ftp://TESTCODE@{SERIAL}""#));
        let start = rig.sent("project_file").pop().unwrap();
        rig.report(&report("IDLE", ""));
        set_log_tap(None);
        let lines = LINES.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert!(lines.iter().any(|l| l.starts_with("fab printer: +") && l.ends_with(&format!(" s sent project_file id {start}"))), "{lines:?}");
        let answer = lines.iter().find(|l| l.contains("logged")).expect("the answer is in the log");
        assert!(answer.starts_with("fab printer: +0.250 s answer print{command=\"project_file\" "), "{answer}");
        assert!(answer.contains(&format!("sequence_id=\"{start}\"")) && answer.contains("result=\"fail\"") && answer.contains("url:str"), "{answer}");
        assert!(lines.iter().all(|l| !l.contains("TESTCODE") && !l.contains(SERIAL)), "{lines:?}");
        assert!(lines.iter().all(|l| !l.contains("push_status") && !l.contains("sent pushall")), "reports and their query stay out: {lines:?}");
    }

    // ---- The access code ----

    #[test]
    fn an_escaped_access_code_stays_out_of_answers() {
        let refusal = r#","result":"fail","reason":"bad \u0054ESTCODE""#;
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.press(Command::Stop);
        rig.answer("stop", refusal);
        assert_eq!(failed(rig.shown().1), "bad ***");

        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.upload_ends(0, Ok(()));
        rig.settle();
        rig.answer("project_file", refusal);
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("bad ***") && !why.contains("TESTCODE")));
    }

    #[test]
    fn the_serial_stays_out_of_answers_and_reports() {
        let refusal = format!(r#","result":"fail","reason":"bad {SERIAL}""#);
        let Some(mut rig) = Rig::reporting("RUNNING", "cube") else { return };
        rig.press(Command::Stop);
        rig.answer("stop", &refusal);
        assert_eq!(failed(rig.shown().1), "bad ***");

        let Some(mut rig) = Rig::reporting("IDLE", "") else { return };
        rig.press(cube());
        rig.upload_ends(0, Ok(()));
        rig.settle();
        rig.answer("project_file", &refusal);
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("bad ***") && !why.contains(SERIAL)));

        rig.report(&format!(r#"{{"print":{{"command":"push_status","msg":0,"gcode_state":"IDLE","subtask_name":"for {SERIAL}","mc_note":"at {SERIAL}","fail_reason":"{SERIAL} is out"}}}}"#));
        let shown = format!("{:?}", rig.worker.snapshot.state);
        assert!(shown.contains("for ***") && !shown.contains(SERIAL), "{shown}");
    }

    #[test]
    fn digits_of_a_code_inside_a_number_lose_no_report() {
        let mut rig = Rig::with_code(b"12345678");
        rig.report(r#"{"print":{"command":"push_status","msg":0,"gcode_state":"IDLE","task_id":9123456789}}"#);
        assert_eq!(rig.worker.snapshot.state.gcode_state.as_deref(), Some("IDLE"));
    }
}
