//! A model-worker notification owns the bytes read on open. The first producer publishes
//! them before its own record. An idle demo never claims a directory in the plate store.
use super::*;
use fab_elec_design::By;
use fab_elec_host::{JournalStep, JournalEvent, Ticket};
use makepad_widgets::makepad_platform::thread::{Class, TaskPool, ThreadOptions, ThreadSpawner};
use std::sync::{Condvar, OnceLock, Weak};
use std::time::Instant;

#[derive(Default)]
struct State {
    paused: bool,
    retired: bool,
    active: bool,
    recovery_writers: usize,
    thread: Option<std::thread::Thread>,
    discarded: u64,
    reported: bool,
}

pub(super) struct Gate {
    dir: PathBuf,
    state: Mutex<State>,
    changed: Condvar,
    committing: AtomicBool,
}

static GATES: Mutex<BTreeMap<PathBuf, Weak<Gate>>> = Mutex::new(BTreeMap::new());

impl State {
    fn report(&mut self, log: &mut impl FnMut(String)) {
        if self.discarded != 0 && !self.reported {
            self.reported = true;
            log(format!("journal: {} records for a deleted plate were not kept", self.discarded));
        }
    }
}

impl Gate {
    fn for_dir(dir: PathBuf) -> Arc<Self> {
        let mut gates = GATES.lock().unwrap();
        if let Some(gate) = gates.get(&dir).and_then(Weak::upgrade) { return gate }
        gates.retain(|_, gate| gate.strong_count() != 0);
        let gate = Arc::new(Self { dir: dir.clone(), state: Mutex::default(), changed: Condvar::new(), committing: false.into() });
        gates.insert(dir, Arc::downgrade(&gate));
        gate
    }

    pub(super) fn paused(&self) -> bool { self.state.lock().unwrap().paused }

    fn pipe(self: &Arc<Self>) -> Option<Arc<Pipe>> {
        pipes().lock().unwrap().get(&self.dir).and_then(Weak::upgrade).filter(|p| Arc::ptr_eq(&p.gate, self))
    }
}

// Remove runs a short synchronous recovery writer without a producer pipe.
pub(super) struct RecoveryWriter(pub(super) Arc<Gate>);

impl RecoveryWriter {
    pub(super) fn enter(dir: &Path) -> io::Result<Self> {
        let gate = Gate::for_dir(dir.to_owned());
        let mut state = gate.state.lock().unwrap();
        if state.paused || state.retired { return Err(io::ErrorKind::WouldBlock.into()) }
        state.recovery_writers += 1;
        drop(state);
        Ok(Self(gate))
    }
}

impl Drop for RecoveryWriter {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().recovery_writers -= 1;
        self.0.changed.notify_all();
    }
}

impl Gate {
    // A contended kernel lock has no cancellation primitive. Only live
    // writers use this interruptible wait before retrying the row lock.
    pub(super) fn wait_for_row(&self) -> io::Result<()> {
        let state = self.state.lock().unwrap();
        let (state, _) = self.changed.wait_timeout_while(state, Duration::from_millis(10), |s| !s.paused && !s.retired).unwrap();
        if state.paused || state.retired { Err(io::ErrorKind::Interrupted.into()) } else { Ok(()) }
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.state.get_mut().unwrap().report(&mut |line| makepad_widgets::log!("{}", line));
    }
}

pub struct Pause { gate: Arc<Gate> }

/// Called on the model worker. A lazy opening and discovery both register at
/// the same in-memory gate before they can start their first filesystem access.
pub fn pause_plate(plate: &Path) -> io::Result<Pause> {
    let gate = Gate::for_dir(plate.join("journal"));
    let mut state = gate.state.lock().unwrap();
    if state.paused || state.retired { return Err(io::ErrorKind::WouldBlock.into()) }
    state.paused = true;
    if let Some(thread) = &state.thread { thread.unpark(); }
    gate.changed.notify_all();
    drop(gate.changed.wait_while(state, |s| s.active || s.recovery_writers != 0).unwrap());
    Ok(Pause { gate })
}

impl Pause {
    pub fn resume(self) { drop(self); }

    /// Transfer existing notes while the model can still refuse Delete and resume.
    pub fn prepare(self) -> io::Result<PreparedPause> {
        self.prepare_notes().map_err(prepare_error)?;
        Ok(PreparedPause(self))
    }

    fn prepare_notes(&self) -> io::Result<()> {
        let gate = &self.gate;
        let plate = gate.dir.parent().ok_or_else(invalid)?;
        let settings = plate.parent().and_then(Path::parent).ok_or_else(invalid)?;
        gate.committing.store(true, Ordering::Release);
        // Quit publishes its dispatch under the same orders lock that we
        // sample here. The IO and this wait are both outside producer locks.
        if let Some(pipe) = gate.pipe() {
            let quit = pipe.journal.progress.orders.lock().unwrap().quit_cut.clone();
            if let Some((_, attempt)) = quit {
                #[cfg(test)]
                tests::probe("waiting for quit note")?;
                attempt.wait()?;
            }
        }
        #[cfg(test)]
        tests::probe("before fallback transfer")?;
        loss::transfer(plate, settings)
    }
}

fn prepare_error(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), "Fab couldn't prepare this plate's journal for the Trash. It is kept.")
}

/// Dropping this guard resumes the writers even after their fallback notes moved.
pub struct PreparedPause(Pause);

impl PreparedPause {
    pub fn commit(self) -> io::Result<RetiredPlate> {
        self.retire().map_err(prepare_error)?;
        Ok(RetiredPlate { gate: self.0.gate.clone(), pipe: self.0.gate.pipe().as_ref().map(Arc::downgrade) })
    }

    fn retire(&self) -> io::Result<()> {
        let gate = &self.0.gate;
        let plate = gate.dir.parent().ok_or_else(invalid)?;
        let mut published = loss::DeleteNotes::default();
        let mut through = None;
        let (outstanding, waiters) = loop {
            let mut state = gate.state.lock().unwrap();
            let Some(pipe) = gate.pipe() else {
                state.retired = true;
                break Default::default()
            };
            let mut orders = pipe.journal.progress.orders.lock().unwrap();
            let after = *through.get_or_insert_with(|| orders.quit_cut.as_ref().map_or(0, |(cut, _)| *cut));
            if orders.outstanding.last().is_none_or(|n| *n <= after) {
                // The last cut and retirement share the producer's lock. A row
                // arriving during note I/O therefore gets either a note or a
                // discarded count, exactly once.
                state.retired = true;
                pipe.journal.progress.pending.store(0, Ordering::Release);
                break (std::mem::take(&mut orders.outstanding), std::mem::take(&mut orders.waiters))
            }
            let notes = loss::Note::outstanding(&gate.dir, &orders, after).unwrap_or_default();
            through = orders.outstanding.last().copied();
            drop(orders);
            drop(state);
            // Publish Delete's own notes directly in the primary location.
            for note in notes { published.publish(plate, note)?; }
        };
        drop(outstanding);
        if let Some(pipe) = gate.pipe() {
            // A failed spawn may have left an unstarted writer and its receiver
            // here. Claim it exactly as start() does. Retired producers can no
            // longer enqueue, so payloads can be freed off their threads.
            if !pipe.started.swap(true, Ordering::AcqRel) { pipe.writer.lock().unwrap().take(); }
            let backlog = std::mem::take(&mut pipe.journal.sender.backlog.lock().unwrap().records);
            drop(backlog);
        }
        for waiter in waiters {
            if let Some(notify) = waiter.notify.upgrade() { notify(0); }
        }
        Ok(())
    }
}

/// Keep discovery on the retired gate through the last filesystem operation of Delete.
pub struct RetiredPlate {
    gate: Arc<Gate>,
    pipe: Option<Weak<Pipe>>,
}

impl Drop for RetiredPlate {
    fn drop(&mut self) {
        // Publish both removals together. A new lookup can now start a fresh
        // writer while old calls still own (and discard through) their old gate.
        let mut gates = GATES.lock().unwrap();
        let mut entries = pipes().lock().unwrap();
        let dir = &self.gate.dir;
        if let Some(old) = &self.pipe {
            if entries.get(dir).is_some_and(|entry| entry.ptr_eq(old)) { entries.remove(dir); }
        }
        if gates.get(dir).is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&self.gate))) {
            gates.remove(dir);
        }
    }
}

impl Drop for Pause {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        state.paused = false;
        self.gate.committing.store(false, Ordering::Release);
        self.gate.changed.notify_all();
    }
}

pub(super) struct Pipe {
    gate: Arc<Gate>,
    journal: Journal,
    writer: Mutex<Option<Writer>>,
    started: std::sync::atomic::AtomicBool,
    dir: PathBuf,
    settings: Option<PathBuf>,
}

fn pipes() -> &'static Mutex<BTreeMap<PathBuf, Weak<Pipe>>> {
    static PIPES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Pipe>>>> = OnceLock::new();
    PIPES.get_or_init(Default::default)
}

/// Observe an existing writer without starting one or touching the filesystem.
pub fn writer_progress(dir: &Path) -> Option<Arc<Progress>> {
    pipes().lock().unwrap().get(dir)?.upgrade().map(|pipe| pipe.journal.progress.clone())
}

pub(crate) type SettleNotice = Arc<dyn Fn(usize) + Send + Sync>;

pub(super) struct SettleWaiter {
    through: u64,
    remaining: usize,
    notify: Weak<dyn Fn(usize) + Send + Sync>,
}

/// Subscribe for a record the reader has already found on disk. Until restart has
/// published its prefix, every such record predates this writer's reservations.
pub(crate) fn watch_settlement(dir: &Path, order: u64, notify: &SettleNotice) {
    let remaining = writer_progress(dir).map_or(0, |p| subscribe(&p, order, notify));
    notify(remaining);
}

pub(super) fn subscribe(progress: &Progress, order: u64, notify: &SettleNotice) -> usize {
    let mut orders = progress.orders.lock().unwrap();
    let Some(through) = orders.segments.through(order) else { return 0 };
    // An order in a higher foreign life can be ahead of locals not reserved yet.
    // A later producer must not satisfy this subscription in place of an earlier one.
    let through = through.min(orders.outstanding.last().copied().unwrap_or(0));
    let remaining = orders.outstanding.range(..=through).count();
    if remaining > 0 {
        orders.waiters.retain(|w| w.notify.strong_count() > 0);
        orders.waiters.push(SettleWaiter { through, remaining, notify: Arc::downgrade(notify) });
    }
    remaining
}

// Called after sync. Deliver outside the writer's lock. Dropping the subscription
// cancels a wait without waking a thread or waiting for a failed disk.
pub(super) fn settled(orders: &mut Orders, order: u64) -> Vec<(SettleNotice, usize)> {
    let mut notices = Vec::new();
    orders.waiters.retain_mut(|w| {
        let Some(notify) = w.notify.upgrade() else { return false };
        if order <= w.through {
            w.remaining -= 1;
            notices.push((notify, w.remaining));
        }
        w.remaining > 0
    });
    notices
}

#[derive(Clone, Copy)]
pub(super) struct ReadBoundary {
    // Both consumers stop at complete lines within this byte limit.
    pub end: u64,
    pub settled: bool,
    pub repairing: bool,
}

impl ReadBoundary {
    /// A rollback can replace the bytes underneath a contended scan. If a
    /// repair started or the log grew during that scan, finish the new prefix
    /// before publishing any of its rows (including a provisional separator).
    pub fn still_current(self, dir: &Path, file: &File) -> io::Result<bool> {
        Ok(self.settled || (Repair::pending(dir)?.is_none() && file.metadata()?.len() == self.end))
    }
}

/// Sample only; release the sidecar before scanning the log. An idle local
/// writer's last sync says nothing about how far a peer has since written.
pub(super) fn read_boundary(dir: &Path, file: &File, known: Option<u64>) -> io::Result<ReadBoundary> {
    loop {
        let mut observed = file.metadata()?.len();
        if known == Some(observed) {
            return Ok(ReadBoundary { end: observed, settled: true, repairing: false })
        }
        let lock = open_regular(&dir.join(LOCK));
        let settled = match &lock {
            Ok(Some(f)) if f.try_lock_shared().is_ok() => {
                observed = file.metadata()?.len();
                true
            }
            // A cooperating writer creates the sidecar before appending. The
            // length sampled BEFORE this lookup is safe in a legacy journal.
            Ok(None) => true,
            _ => false,
        };
        if let Some(repair) = Repair::pending(dir)? {
            return Ok(ReadBoundary { end: file.metadata()?.len().min(repair.end), settled: true, repairing: true })
        }
        // If the sample included just the separator, the pending file existed
        // then. Its removal implies the marker grew the log beyond that sample.
        if observed == file.metadata()?.len() { return Ok(ReadBoundary { end: observed, settled, repairing: false }) }
    }
}

impl Pipe {
    /// All openings of the same directory share its writer, even if a previously
    /// unnamed plate was adopted while an old engine still had a commit in flight.
    fn open(gate: &Arc<Gate>, settings: Option<&Path>, create_plate: bool) -> Arc<Self> {
        let dir = gate.dir.clone();
        let mut entries = pipes().lock().unwrap();
        if let Some(pipe) = entries.get(&dir).and_then(Weak::upgrade) { return pipe }
        let (journal, mut writer) = Journal::channel(dir.clone(), Arc::default(), 1,
            Arc::new(|n| makepad_widgets::log!("journal: {} records queued", n)));
        let settings = settings.map(Path::to_owned);
        writer.settings = settings.clone();
        writer.gate = Some(gate.clone());
        writer.create_plate = create_plate;
        let pipe = Arc::new(Self { gate: gate.clone(), settings, journal, writer: Mutex::new(Some(writer)), started: Default::default(), dir: dir.clone() });
        entries.insert(dir, Arc::downgrade(&pipe));
        pipe
    }

    fn start(self: &Arc<Self>, spawner: &ThreadSpawner) {
        if self.started.swap(true, Ordering::AcqRel) { return }
        let owner = self.clone();
        match spawner.spawn_worker(ThreadOptions { name: Some("Fab plate journal".into()), ..Default::default() }, move || {
            owner.run();
        }) {
            Ok(task) => task.detach(),
            Err(_) => { self.started.store(false, Ordering::Release); }
        }
    }
    fn stop_if_idle(self: &Arc<Self>, writer: &mut Writer) -> bool {
        if Arc::strong_count(self) != 1 { return false }
        writer.close_handles();
        let mut state = self.gate.state.lock().unwrap();
        let mut entries = pipes().lock().unwrap();
        if Arc::strong_count(self) != 1 { return false }
        if entries.get(&self.dir).is_some_and(|p| p.ptr_eq(&Arc::downgrade(self))) { entries.remove(&self.dir); }
        state.active = false;
        state.thread = None;
        self.gate.changed.notify_all();
        true
    }

    fn run(self: Arc<Self>) {
        let mut writer = self.writer.lock().unwrap().take().unwrap();
        let gate = &self.gate;
        let mut state = gate.state.lock().unwrap();
        state.thread = Some(std::thread::current());
        let mut batch = VecDeque::new();
        let mut prepare = true;
        let mut retry = false;
        let mut log = |line| makepad_widgets::log!("{}", line);
        loop {
            if state.paused || state.retired {
                drop(state);
                writer.close_handles();
                state = gate.state.lock().unwrap();
                state.active = false;
                gate.changed.notify_all();
                state = gate.changed.wait_while(state, |s| s.paused && !s.retired).unwrap();
                if state.retired { break }
                retry = false;
            }
            if retry {
                #[cfg(test)]
                let _ = tests::probe("writer retry wait");
                let (next, _) = gate.changed.wait_timeout_while(state, Duration::from_secs(1), |s| !s.paused && !s.retired).unwrap();
                state = next;
                if !state.paused && !state.retired && batch.is_empty() {
                    drop(state);
                    if self.stop_if_idle(&mut writer) { return }
                    state = gate.state.lock().unwrap();
                }
                retry = false;
                continue
            }
            state.active = true;
            drop(state);
            if batch.is_empty() {
                if let Ok(row) = writer.rx.try_recv() {
                    let mut backlog = writer.backlog.lock().unwrap();
                    batch = std::mem::take(&mut backlog.records);
                    batch.push_front(row);
                }
            }
            if let Some(row) = batch.front() {
                if writer.try_write(row, &mut log) {
                    batch.pop_front();
                    if batch.is_empty() {
                        let mut backlog = writer.backlog.lock().unwrap();
                        if backlog.records.is_empty() { backlog.reported = false; }
                    }
                } else { retry = true; }
                prepare = false;
            } else if prepare {
                let result = if writer.progress.pending() != 0 { writer.prepare(&mut log).map(|_| ()) }
                    else { writer.recover_unpaused(&mut log) };
                retry = result.is_err();
                prepare = retry;
            } else {
                if self.stop_if_idle(&mut writer) { return }
                #[cfg(test)]
                let _ = tests::probe("writer idle");
                std::thread::park_timeout(Duration::from_secs(1));
            }
            state = gate.state.lock().unwrap();
        }
        state.active = false;
        state.thread = None;
        gate.changed.notify_all();
    }

}

/// Discover unopened plates on the pool. Concurrent plate opens share the
/// same writer through the pipe registry.
pub(crate) fn recover_loss_notes(store: fab_ai::plates::PlateStore, spawner: ThreadSpawner, pool: &TaskPool) {
    let discover = move || {
        let settings = store.root.parent();
        loss::discover_for_recovery(&store, &mut |line| makepad_widgets::log!("{}", line), |dir, create_plate| {
            let gate = Gate::for_dir(dir);
            let state = gate.state.lock().unwrap();
            if !state.retired { Pipe::open(&gate, settings, create_plate).start(&spawner); }
        });
    };
    match pool.submit_named(Class::Background, "Fab journal loss discovery", discover) {
        Ok(task) => task.detach(),
        Err(e) => makepad_widgets::log!("journal: loss note discovery could not be queued ({e:?})"),
    }
}

#[derive(Default)]
struct Disk {
    docs: fab_model::Docs,
    assets: fab_model::ElecAssets,
}

struct Opening {
    gate: Arc<Gate>,
    dir: Option<PathBuf>,
    pipe: OnceLock<Arc<Pipe>>,
    spawner: OnceLock<ThreadSpawner>,
    saved_plate: AtomicBool,
    presence: OnceLock<Option<super::presence::Watch>>,
    manifest: String,
    checkpoint: Mutex<Option<Arc<Files>>>,
    checkpoint_order: OnceLock<u64>,
    prefix: String,
    /// Electronics files at the last publication. Only the strand compares contents.
    disk: Mutex<Disk>,
    mechanical: Mutex<super::mechanical::Saved>,
}

impl Opening {
    fn watch_presence(&self, spawner: &ThreadSpawner) {
        if let Some(plate) = self.gate.dir.parent() {
            self.presence.get_or_init(|| super::presence::Watch::start(plate.to_owned(), spawner));
        }
    }
}

#[derive(Clone)]
pub(crate) struct Plate {
    opening: Arc<Opening>,
    store: fab_ai::plates::PlateStore,
    identity: Arc<fab_ai::WorkingId>,
}

impl std::fmt::Debug for Plate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("Journal plate") }
}

type Beds = Vec<(Weak<fab_ai::WorkingId>, Arc<Opening>)>;
fn beds() -> &'static Mutex<Beds> {
    static BEDS: OnceLock<Mutex<Beds>> = OnceLock::new();
    BEDS.get_or_init(Default::default)
}

/// Close this process's presence file for the plate folder `plate`, before the folder moves
/// (Windows refuses to rename a folder with a handle open inside). Entries whose working id is
/// gone but not yet swept count too; a released opening starts no new watch.
pub(crate) fn release_presence(plate: &Path) {
    let openings: Vec<_> = beds().lock().unwrap().iter().map(|(_, o)| o.clone()).collect();
    for opening in openings {
        if opening.gate.dir.parent() == Some(plate) {
            // an opening not yet attached takes no watch after this
            if let Some(watch) = opening.presence.get_or_init(|| None) { watch.release(); }
        }
    }
}

impl Plate {
    pub(crate) fn opened(session: &mut fab_ai::Session) -> Option<Self> {
        let loaded = session.model.opened_files.take();
        let store = session.store.clone()?;
        let identity = session.working_id.clone();
        let key = Arc::downgrade(&identity);
        let beds = beds();
        {
            let mut beds = beds.lock().unwrap();
            beds.retain(|(k, _)| k.strong_count() != 0);
            if let Some((_, opening)) = beds.iter().find(|(k, _)| k.ptr_eq(&key)) {
                if session.plate.is_some() {
                    opening.saved_plate.store(true, Ordering::Release);
                    if let Some(spawner) = opening.spawner.get() { opening.watch_presence(spawner); }
                }
                return Some(Self { opening: opening.clone(), store, identity })
            }
        }
        let loaded = loaded.unwrap_or_default();
        let manifest = if loaded.manifest.is_empty() { "model.fab.splash" } else { &loaded.manifest };
        let prefix = fab_model::project::elec_dir(Path::new(manifest)).to_string_lossy().into_owned();
        let dir = session.plate.as_ref().map(|id| store.dir(id).join("journal"));
        let mechanical = super::mechanical::Saved::opened(manifest, &loaded.files, session.model.state());
        let gate = Gate::for_dir(dir.clone().unwrap_or_else(|| {
            let id = identity.id();
            debug_assert_eq!(reader::sessions::working_id(id).map(|(_, pid)| pid), Some(std::process::id()));
            store.dir(id).join("journal")
        }));
        let opening = Arc::new(Opening { saved_plate: session.plate.is_some().into(), gate, manifest: manifest.to_owned(), mechanical: Mutex::new(mechanical), dir, pipe: OnceLock::new(), spawner: OnceLock::new(), presence: OnceLock::new(), checkpoint_order: OnceLock::new(), checkpoint: Mutex::new(Some(Arc::new(loaded.files))), prefix,
            disk: Mutex::new(Disk { docs: crate::elec_host::electronics(session.model.docs()), assets: (**session.model.elec_assets()).clone() }) });
        beds.lock().unwrap().push((key, opening.clone()));
        Some(Self { opening, store, identity })
    }

    pub(crate) fn attach(&self, spawner: &ThreadSpawner) {
        self.opening.spawner.get_or_init(|| spawner.clone());
        if self.opening.saved_plate.load(Ordering::Acquire) { self.opening.watch_presence(spawner); }
        self.retry_worker();
    }

    fn pipe(&self) -> &Arc<Pipe> {
        self.opening.pipe.get_or_init(|| Pipe::open(&self.opening.gate, self.store.root.parent(), self.opening.dir.is_none()))
    }

    /// Another Fab process has this plate open. One atomic read.
    pub(crate) fn elsewhere(&self) -> bool {
        self.opening.presence.get().and_then(Option::as_ref).is_some_and(|w| w.elsewhere())
    }

    pub(crate) fn retry_worker(&self) {
        if let (Some(pipe), Some(spawner)) = (self.opening.pipe.get(), self.opening.spawner.get()) {
            if !pipe.started.load(Ordering::Acquire) && pipe.journal.progress.pending.load(Ordering::Acquire) != 0 {
                pipe.start(spawner);
            }
        }
    }

    pub(crate) fn checkpoint(&self) -> u64 {
        let mut state = self.opening.gate.state.lock().unwrap();
        if state.retired { state.discarded += 1; return 0 }
        self.checkpoint_locked()
    }

    fn checkpoint_locked(&self) -> u64 {
        let pipe = self.pipe();
        let order = *self.opening.checkpoint_order.get_or_init(|| {
            let files = self.opening.checkpoint.lock().unwrap().take().expect("first record of this opening");
            pipe.journal.send(Record::Checkpoint { manifest: Some(self.opening.manifest.clone()), files })
        });
        self.retry_worker();
        order
    }

    fn reserve(&self) -> Reservation {
        let mut state = self.opening.gate.state.lock().unwrap();
        if state.retired {
            state.discarded += 1;
            return Reservation { order: 0, time_ms: 0 }
        }
        self.checkpoint_locked();
        self.pipe().journal.reserve()
    }

    fn publish(&self, reservation: Reservation, record: Record) {
        let state = self.opening.gate.state.lock().unwrap();
        if state.retired { return }
        let checkpoint = self.opening.checkpoint_order.get().copied();
        self.pipe().journal.sender.push(Ordered { reservation, checkpoint, record });
        if let Some(thread) = &state.thread { thread.unpark(); }
    }

    pub(super) fn send(&self, record: Record) -> u64 {
        let reservation = self.reserve();
        self.publish(reservation, record);
        reservation.order
    }

    pub(super) fn mechanical_changes(&self, old: &fab_model::State, model: &fab_model::FabModel) -> Vec<Change> {
        self.opening.mechanical.lock().unwrap().changes(old, model)
    }

    /// The store and folder the reader may watch. Reserving a working id is memory-only.
    pub(crate) fn reader_location(&self) -> (&Path, &str) {
        let folder = self.opening.dir.as_deref().and_then(Path::parent).and_then(Path::file_name)
            .and_then(|name| name.to_str()).unwrap_or_else(|| self.identity.id());
        (&self.store.root, folder)
    }

    pub(crate) fn store(&self) -> &fab_ai::plates::PlateStore { &self.store }

    pub(super) fn folder(&self) -> String {
        // Production directories are always <plate id>/journal.
        self.opening.gate.dir.parent().and_then(Path::file_name).unwrap_or_default().to_string_lossy().into_owned()
    }

    pub(super) fn effect(&self, call: Option<&Call>, seq: u64, by: By, label: &str, changes: Vec<Change>) {
        let mut local = None;
        let mut foreign = None;
        let mut action = None;
        if let Some(call) = call {
            let mut state = call.state.lock().unwrap();
            let seqs = if Arc::ptr_eq(&self.opening.gate, &call.plate.opening.gate) {
                local = Some(call.id);
                action = Some(call.action.clone());
                &mut state.seqs
            } else {
                foreign = Some((call.reference.clone(), call.plate.folder()));
                state.elsewhere.entry(self.folder()).or_default()
            };
            if seqs.last() != Some(&seq) { seqs.push(seq); }
        }
        self.send(Record::Effect { call: local, foreign, seq, actor: actor(by), label: label.into(), action, changes });
    }

    fn name(&self, name: &str) -> String { format!("{}/{name}", self.opening.prefix) }
}

fn actor(by: By) -> Value {
    match by {
        By::Person => obj(vec![("kind", s("person"))]),
        By::Ai { turn, call } => obj(vec![("kind", s("ai")), ("turn", number(turn)), ("call", number(call))]),
        By::Agent { call } => obj(vec![("kind", s("agent")), ("call", number(call))]),
        By::Disk => obj(vec![("kind", s("disk"))]),
    }
}

pub(crate) fn arguments(args: &[(String, String)]) -> Value {
    Value::Arr(args.iter().map(|(k,v)| obj(vec![("name", s(k)), ("value", s(v))])).collect())
}

#[cfg(test)]
pub(crate) fn text_args(ask: &crate::elec_host::TextAsk) -> Value {
    use fab_elec_host::doc_actions::TextChange;
    let mut fields = vec![("doc", s(&ask.doc)), ("rev", number(ask.rev))];
    fields.push(match &ask.change {
        TextChange::Whole(text) => ("text", s(text)),
        TextChange::Replace(pairs) => ("replace", Value::Arr(pairs.iter().map(|(old,new)| obj(vec![("old",s(old)),("new",s(new))])).collect())),
    });
    obj(fields)
}

pub(crate) struct Call {
    plate: Plate,
    id: u64,
    reference: String,
    action: Value,
    state: Mutex<CallState>,
}

/// Holds completion until submitted work (including a job cancelled in flight) settles.
pub(crate) struct Binding(pub(crate) Arc<Call>);

impl Binding {
    pub(crate) fn new(call: Arc<Call>) -> Arc<Self> {
        call.state.lock().unwrap().tickets += 1;
        Arc::new(Self(call))
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().tickets -= 1;
        self.0.publish_done();
    }
}

#[derive(Clone)]
pub(crate) struct Work {
    pub(crate) by: By,
    pub(crate) person: Option<Arc<PersonReply>>,
    pub(crate) journal: Option<Arc<Binding>>,
}

impl Work {
    pub(crate) fn new(by: By, call: Option<&Arc<Call>>) -> Self {
        Self { by, person: None, journal: call.cloned().map(Binding::new) }
    }
}

#[derive(Default)]
struct CallState {
    tickets: usize,
    seqs: Vec<u64>,
    elsewhere: BTreeMap<String, Vec<u64>>,
    finished: bool,
    /// Reset can deliver the original refusal while the old strand finishes a commit.
    /// Reserve the completion at delivery, publish once its final effects are known.
    done: Option<(Reservation, &'static str, String)>,
}

impl Call {
    pub(crate) fn refused(plate: &Plate, by: By, name: &str, wire_id: &str, raw: &str, reason: &str) {
        let mut who = actor(by);
        if let Value::Obj(fields) = &mut who { fields.push(("wire_id".into(), s(wire_id))); }
        Self::begin_as(plate, who, name, s(raw), true).done("refused", reason.into());
    }

    pub(crate) fn begin(plate: &Plate, by: By, name: &str, args: Value) -> Arc<Self> {
        Self::begin_as(plate, actor(by), name, args, true)
    }

    pub(super) fn decision(plate: &Plate, name: &str, args: &[(String, String)]) -> Arc<Self> {
        Self::begin_as(plate, actor(By::Person), name, arguments(args), false)
    }

    fn begin_as(plate: &Plate, actor: Value, name: &str, args: Value, replay: bool) -> Arc<Self> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let reference = format!("call-{nanos:x}-{:x}-{:x}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed));
        let id = plate.send(Record::Call { reference: reference.clone(), actor, action: name.to_owned(), args: args.clone(), replay });
        Arc::new(Self { plate: plate.clone(), id, reference, action: obj(vec![("name", s(name)), ("args", args)]), state: Default::default() })
    }

    pub(crate) fn done(&self, outcome: &'static str, reply: String) {
        let mut state = self.state.lock().unwrap();
        if state.finished { return }
        state.finished = true;
        state.done = Some((self.plate.reserve(), outcome, reply));
        drop(state);
        self.publish_done();
    }

    fn publish_done(&self) {
        let mut state = self.state.lock().unwrap();
        if state.tickets != 0 { return }
        if let Some((order, outcome, reply)) = state.done.take() {
            let seqs = std::mem::take(&mut state.seqs);
            let elsewhere = std::mem::take(&mut state.elsewhere);
            drop(state);
            self.plate.publish(order, Record::Done { call: self.id, outcome, reply, seqs, elsewhere });
        }
    }
}

#[derive(Clone)]
pub(crate) struct Electronics {
    plate: Plate,
    tickets: Arc<Mutex<std::collections::HashMap<Ticket, Arc<Call>>>>,
}

impl Electronics {
    pub(crate) fn new(plate: Plate) -> Self { Self { plate, tickets: Default::default() } }
    pub(crate) fn plate(&self) -> &Plate { &self.plate }
    pub(crate) fn bind(&self, ticket: Ticket, call: &Arc<Call>) {
        call.state.lock().unwrap().tickets += 1;
        self.tickets.lock().unwrap().insert(ticket, call.clone());
    }
    pub(crate) fn unbind(&self, ticket: Ticket) {
        if let Some(call) = self.tickets.lock().unwrap().remove(&ticket) {
            call.state.lock().unwrap().tickets -= 1;
            call.publish_done();
        }
    }

    /// On the engine strand, after the previous engine's last commit, before taking
    /// commands on the new one. A publication of the engine's own files has no delta.
    pub(crate) fn loaded(&self, docs: &fab_model::Docs, assets: &fab_model::ElecAssets) -> bool {
        let mut disk = self.plate.opening.disk.lock().unwrap();
        let next = crate::elec_host::electronics(docs);
        let mut changes = Vec::new();
        for (name, old) in &disk.docs {
            if !next.contains_key(name) { changes.push(Change::whole(self.plate.name(name), Some(old.as_bytes()), None)); }
        }
        for (name, text) in &next {
            let old = disk.docs.get(name);
            if old != Some(text) {
                changes.push(Change::whole(self.plate.name(name), old.map(|t| t.as_bytes()), Some(Arc::from(text.as_bytes()))));
            }
        }
        for (name, old) in &disk.assets {
            if !assets.contains_key(name) { changes.push(Change::whole(self.plate.name(name), Some(old), None)); }
        }
        Self::added(&self.plate, &mut disk, assets, &mut changes);
        disk.docs = next;
        disk.assets.clone_from(assets);
        if changes.is_empty() { return false }
        self.plate.send(Record::Reload(changes));
        true
    }

    fn added(plate: &Plate, disk: &mut Disk, added: &fab_model::ElecAssets, changes: &mut Vec<Change>) {
        for (name, bytes) in added {
            let old = disk.assets.get(name);
            if old != Some(bytes) {
                changes.push(Change::whole(plate.name(name), old.map(|b| &b[..]), Some(bytes.clone())));
                disk.assets.insert(name.clone(), bytes.clone());
            }
        }
    }

    pub(crate) fn assets_added(&self, assets: fab_model::ElecAssets) {
        let mut disk = self.plate.opening.disk.lock().unwrap();
        let mut changes = Vec::new();
        Self::added(&self.plate, &mut disk, &assets, &mut changes);
        if !changes.is_empty() { self.plate.send(Record::Reload(changes)); }
    }

    /// Merged typing and placement can extend an earlier undo step. Diff against
    /// the immediately preceding publication, including any outside reload.
    fn changes(&self, step: JournalStep<'_>) -> Vec<Change> {
        let mut disk = self.plate.opening.disk.lock().unwrap();
        let mut changes = Vec::new();
        for (name, after) in step.texts() {
            let before = disk.docs.get(name);
            if before == after { continue }
            let path = self.plate.name(name);
            changes.push(match (before, after) {
                (Some(before), Some(after)) => Change::text(path, before, after),
                _ => Change::whole(path, before.map(|t| t.as_bytes()), after.map(|t| Arc::from(t.as_bytes()))),
            });
            if let Some(after) = after { disk.docs.insert(name.to_owned(), after.clone()); } else { disk.docs.remove(name); }
        }
        for asset in step.assets() {
            let after = asset.available.then(|| asset.bytes.0.clone());
            let before = disk.assets.get(&asset.name);
            if before == after.as_ref() { continue }
            changes.push(Change::whole(self.plate.name(&asset.name), before.map(|b| &b[..]), after.clone()));
            if let Some(after) = after { disk.assets.insert(asset.name.clone(), after); } else { disk.assets.remove(&asset.name); }
        }
        changes
    }

}

/// Cancelling a plate while a released edit is in flight closes its journal call too.
struct GestureBinding(Electronics, Ticket, Arc<Call>);

impl Drop for GestureBinding {
    fn drop(&mut self) {
        self.2.done("cancelled", "cancelled".into());
        self.0.unbind(self.1);
    }
}

impl Electronics {
    fn bind_press(&self, ticket: Ticket, call: Arc<Call>) -> Box<dyn FnOnce(Result<u64, String>) + Send + Sync> {
        self.bind(ticket, &call);
        let guard = GestureBinding(self.clone(), ticket, call);
        Box::new(move |result| {
            match result {
                Ok(seq) => guard.2.done("ok", format!("{{seq: {seq}}}")),
                Err(reason) => guard.2.done("refused", reason),
            }
            drop(guard);
        })
    }
}

impl fab_elec_host::JournalSink for Electronics {
    fn gesture(&self, ticket: Ticket, name: &str, args: &[(String, String)]) -> Option<Box<dyn FnOnce(Result<u64, String>) + Send + Sync>> {
        Some(self.bind_press(ticket, Call::begin(&self.plate, By::Person, name, arguments(args))))
    }

    fn decision(&self, ticket: Ticket, name: &str, args: &[(String, String)]) -> Option<Box<dyn FnOnce(Result<u64, String>) + Send + Sync>> {
        Some(self.bind_press(ticket, Call::decision(&self.plate, name, args)))
    }

    fn receive(&self, event: JournalEvent<'_>) {
        let record = match event {
            JournalEvent::Effect { ticket, seq, by, label, changes: rows } => {
                let call = ticket.and_then(|t| self.tickets.lock().unwrap().get(&t).cloned());
                return self.plate.effect(call.as_deref(), seq, by, label, self.changes(rows));
            }
            JournalEvent::History { undo, before, after, changes: rows } => Record::History { undo, before, after, changes: self.changes(rows) },
        };
        self.plate.send(record);
    }
}

#[derive(Default)]
enum Save {
    #[default]
    Waiting,
    Answered,
    HostGone { since: Instant, reported: bool },
}

// Give the confirmed failure time on the caption even if there are no notes.
// This is a presentation interval; a pending save has no deadline.
const SHOW_FAILURE: Duration = Duration::from_secs(3);
const HOST_GONE: &str = "Model host stopped. Last edits unsaved. Journal entries kept.";

#[derive(Default)]
pub(crate) struct Quit {
    repeats: u8,
    save: Save,
    asked: Option<Instant>,
    shown_seconds: Option<u64>,
    writing: Option<(Arc<[Arc<loss::Attempt>]>, Receiver<()>)>,
    exited: bool,
}

impl Quit {
    pub(crate) fn repeat(&mut self) { self.repeats = self.repeats.saturating_add(1); }

    pub(crate) fn begin(&mut self, now: Instant) { self.asked = Some(now); }

    pub(crate) fn saving_line(&mut self, now: Instant) -> Option<String> {
        if !matches!(self.save, Save::Waiting) { return None }
        let seconds = now.saturating_duration_since(self.asked?).as_secs();
        if self.shown_seconds == Some(seconds) { return None }
        self.shown_seconds = Some(seconds);
        Some(format!("Saving the plate… {seconds} s"))
    }

    pub(crate) fn answered(&mut self) { self.save = Save::Answered; }

    pub(crate) fn host_gone(&mut self, now: Instant) -> &'static str {
        self.save = Save::HostGone { since: now, reported: false };
        // Start the loss-note phase even if presses arrived during the save.
        // A further press after the failure is visible can leave a stuck writer.
        self.repeats = 1;
        HOST_GONE
    }

    pub(crate) fn ready(&mut self, cx: &makepad_widgets::Cx, now: Instant) -> bool {
        let writers = || pipes().lock().unwrap().values().filter_map(Weak::upgrade).collect();
        let ready = self.poll(now, writers, |attempts, done| {
            // The model's pool may itself have dropped the quit. This last
            // file operation has an independent worker, just like each pipe.
            let options = ThreadOptions { name: Some("Fab journal loss note".into()), ..Default::default() };
            let queued = attempts.clone();
            if let Ok(task) = cx.spawn_worker_with(options, move || {
                for attempt in attempts.iter() { attempt.write(); }
                let _ = done.send(());
            }) { task.detach(); } else { for attempt in queued.iter() { attempt.finish(false); } }
        }, |msg| makepad_widgets::log!("{}", msg));
        if ready {
            // Beds can retain an opening until process exit, when statics have
            // no destructor. Report its final discarded count at graceful Quit.
            let gates: Vec<_> = beds().lock().unwrap().iter().map(|(_, o)| o.gate.clone()).collect();
            for gate in gates { gate.state.lock().unwrap().report(&mut |line| makepad_widgets::log!("{}", line)); }
        }
        ready
    }

    fn poll(&mut self, now: Instant, writers: impl FnOnce() -> Vec<Arc<Pipe>>, start: impl FnOnce(Arc<[Arc<loss::Attempt>]>, mpsc::Sender<()>),
        mut log: impl FnMut(String)) -> bool {
        let gone = matches!(self.save, Save::HostGone { .. });
        let waiting = matches!(self.save, Save::Waiting);
        if self.exited || (waiting && (self.repeats != 1 || self.writing.is_some())) { return false }

        let mut queued = false;
        if self.writing.is_none() {
            let writers = writers();
            if !gone && self.repeats == 0 {
                if writers.iter().any(|p| p.journal.progress.pending() != 0) { return false }
            } else {
                // Take one cut across writers. No file operation holds these locks.
                let mut all: Vec<_> = writers.iter().map(|p| p.journal.progress.orders.lock().unwrap()).collect();
                if !gone && self.repeats == 1 && writers.iter().zip(&all).any(|(p, orders)|
                    !orders.outstanding.is_empty() && !p.journal.progress.write_error()) { return false }
                if writers.iter().any(|p| p.gate.committing.load(Ordering::Acquire)) { return false }
                let notes: Arc<[_]> = writers.iter().zip(&mut all).filter_map(|(p, orders)| {
                    let notes = loss::Note::cut(&p.dir, orders)?;
                    let attempt = Arc::new(loss::Attempt::new(notes, &p.dir, p.settings.as_deref()));
                    if gone || self.repeats == 1 {
                        attempt.keep_pipe(p.clone());
                        orders.quit_cut = Some((p.journal.counter.load(Ordering::Relaxed), attempt.clone()));
                    }
                    Some(attempt)
                }).collect();
                drop(all);
                if gone || !notes.is_empty() {
                    let (done, receipt) = mpsc::channel();
                    self.writing = Some((notes.clone(), receipt));
                    if !notes.is_empty() && (gone || self.repeats == 1) {
                        start(notes, done);
                        queued = true;
                    }
                }
            }
        }
        if waiting { return false }
        if let Some((notes, done)) = &self.writing {
            if let Save::HostGone { since, reported } = &mut self.save {
                if !*reported {
                    let count: u64 = notes.iter().map(|a| a.count()).sum();
                    log(format!("journal: model host stopped before save; journals={}, outstanding={count}", notes.len()));
                    *reported = true;
                }
                if self.repeats < 2 && now.saturating_duration_since(*since) < SHOW_FAILURE { return false }
            }
            if queued || (self.repeats < 2 && matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty))) { return false }
            if !gone {
                log(format!("journal: unwritten records: {}", notes.iter().map(|a| a.summary()).collect::<Vec<_>>().join("; ")));
            }
        }
        self.exited = true;
        true
    }
}

#[cfg(test)]
impl Plate {
    pub(crate) fn testing(dir: &Path, manifest: &str, docs: &fab_model::Docs) -> Self {
        // The fixture represents an already opened, saved plate.
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        let prefix = fab_model::project::elec_dir(Path::new(manifest)).to_str().unwrap().to_owned();
        let mut files: Files = docs.iter().map(|(name, text)| (format!("{prefix}/{name}"), Arc::from(text.as_bytes()))).collect();
        files.insert(manifest.into(), Arc::from(b"// fixture\nuse mod.fab.*\n".as_slice()));
        let opening = Opening { saved_plate: true.into(), gate: Gate::for_dir(dir.to_owned()), manifest: manifest.to_owned(), mechanical: Default::default(), dir: Some(dir.to_owned()), pipe: OnceLock::new(), spawner: OnceLock::new(), presence: OnceLock::new(), checkpoint_order: OnceLock::new(), checkpoint: Mutex::new(Some(Arc::new(files))), prefix,
            disk: Mutex::new(Disk { docs: docs.clone(), assets: Default::default() }) };
        Self { opening: Arc::new(opening), store: fab_ai::plates::PlateStore::under(dir, dir), identity: Default::default() }
    }

    pub(super) fn worker(&self) -> impl FnOnce() + Send + use<> {
        let pipe = self.pipe().clone();
        assert!(!pipe.started.swap(true, Ordering::AcqRel));
        move || pipe.run()
    }

    pub(super) fn discarded(&self) -> u64 { self.opening.gate.state.lock().unwrap().discarded }

    pub(super) fn take_writer(&self) -> Writer {
        let pipe = self.pipe();
        assert!(!pipe.started.swap(true, Ordering::AcqRel));
        pipe.writer.lock().unwrap().take().unwrap()
    }

    pub(crate) fn pending(&self) -> u64 { self.opening.pipe.get().map_or(0, |p| p.journal.progress.pending()) }
}

#[cfg(test)]
pub(super) fn quit_for(quit: &mut Quit, plate: &Plate, start: impl FnOnce(Arc<[Arc<loss::Attempt>]>, mpsc::Sender<()>),
    log: impl FnMut(String)) -> bool {
    quit_for_at(quit, plate, Instant::now(), start, log)
}

#[cfg(test)]
pub(super) fn quit_for_at(quit: &mut Quit, plate: &Plate, now: Instant,
    start: impl FnOnce(Arc<[Arc<loss::Attempt>]>, mpsc::Sender<()>), log: impl FnMut(String)) -> bool {
    quit.poll(now, || plate.opening.pipe.get().cloned().into_iter().collect(), start, log)
}

#[cfg(test)]
impl Plate {
    /// Hold the writer off the producer's clock, then measure its real durable writes
    /// against exactly the same trace. The checkpoint and content blobs count too.
    pub(crate) fn measured_writer(&self) -> impl FnOnce() -> (u64, Duration) {
        let writer = self.take_writer();
        move || {
            let n = writer.progress.pending.load(Ordering::Acquire);
            let now = std::time::Instant::now();
            while writer.progress.pending.load(Ordering::Acquire) != 0 { writer.batch(writer.rx.recv().unwrap()); }
            (n, now.elapsed())
        }
    }
}
