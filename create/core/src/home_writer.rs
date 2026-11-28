//! Ordered private-file writes for one locked Town home.
use crate::{event::TownId, home::Home};
use std::collections::{HashMap, LinkedList};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::{thread, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveFailure {
    Io(io::ErrorKind),
    WorkerStopped,
}

/// Standalone stores use disk directly. Stores in an attached home share its queued view.
#[derive(Clone, Debug, Default)]
pub struct PrivateFiles(Option<Weak<HomeWriter>>);

impl PrivateFiles {
    pub(crate) fn queued(writer: &Arc<HomeWriter>) -> Self { Self(Some(Arc::downgrade(writer))) }

    fn writer(&self) -> io::Result<Option<Arc<HomeWriter>>> {
        self.0.as_ref().map(|w| w.upgrade().ok_or_else(|| io::Error::other("home writer closed"))).transpose()
    }

    pub fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.read_capped(path, u64::MAX)
    }

    pub fn read_capped(&self, path: &Path, max: u64) -> io::Result<Vec<u8>> {
        let view = self.writer()?.and_then(|w| w.files.lock().unwrap().get(path).map(|(_, v)| v.clone()));
        let read_disk = |path: &Path| -> io::Result<Vec<u8>> {
            let mut out = Vec::new();
            File::open(path)?.take(max).read_to_end(&mut out)?;
            Ok(out)
        };
        match view {
            Some(FileView::Bytes(bytes)) => Ok(bytes[..bytes.len().min(usize::try_from(max).unwrap_or(usize::MAX))].to_vec()),
            Some(FileView::Absent) => Err(io::ErrorKind::NotFound.into()),
            Some(FileView::Archive(from)) => match read_disk(path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    // The worker can finish the rename and reuse its source during the read.
                    let old = read_disk(&from);
                    match read_disk(path) {
                        Err(err) if err.kind() == io::ErrorKind::NotFound => old,
                        moved => moved,
                    }
                }
                moved => moved,
            },
            None => read_disk(path),
        }
    }

    pub fn exists(&self, path: &Path) -> io::Result<bool> {
        if let Some(writer) = self.writer()? {
            if let Some((_, view)) = writer.files.lock().unwrap().get(path) { return Ok(!matches!(view, FileView::Absent)) }
        }
        match path.symlink_metadata() {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let Some(writer) = self.writer()? else { return std::fs::rename(from, to) };
        writer.file(FileOp::Rename(from.to_owned(), to.to_owned()));
        Ok(())
    }

    pub fn replace(&self, path: &Path, bytes: Vec<u8>) -> io::Result<()> {
        self.replace_with(path, bytes, false)
    }

    /// A failed checkpoint or UI preference must let later durable writes proceed.
    pub fn replace_rebuildable(&self, path: &Path, bytes: Vec<u8>) -> io::Result<()> {
        self.replace_with(path, bytes, true)
    }

    fn replace_with(&self, path: &Path, bytes: Vec<u8>, rebuildable: bool) -> io::Result<()> {
        let Some(writer) = self.writer()? else { return crate::home::write_private(path, &bytes) };
        writer.file(FileOp::Replace { path: path.to_owned(), bytes: Arc::new(bytes), rebuildable });
        Ok(())
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn settle(&self) {
        let Ok(Some(writer)) = self.writer() else { return };
        loop {
            let generation = writer.wake_gen();
            writer.tick(false, |_| {});
            if writer.is_drained() || writer.is_dead() { break }
            writer.wait_wake(generation);
        }
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn hold(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        struct Hold { entered: Option<mpsc::Sender<()>>, release: mpsc::Receiver<()> }
        impl Drop for Hold {
            fn drop(&mut self) {
                if let Some(entered) = self.entered.take() { let _ = entered.send(()); }
                let _ = self.release.recv();
            }
        }
        let (in_tx, in_rx) = mpsc::channel();
        let (out_tx, out_rx) = mpsc::channel();
        if let Ok(Some(writer)) = self.writer() {
            writer.release(Hold { entered: Some(in_tx), release: out_rx });
        }
        (in_rx, out_tx)
    }
}

#[derive(Clone)]
enum FileView { Bytes(Arc<Vec<u8>>), Absent, Archive(PathBuf) }

enum FileOp {
    Replace { path: PathBuf, bytes: Arc<Vec<u8>>, rebuildable: bool },
    Rename(PathBuf, PathBuf),
}

impl FileOp {
    fn rebuildable(&self) -> bool { matches!(self, Self::Replace { rebuildable: true, .. }) }

    fn persist(&self) -> io::Result<()> {
        match self {
            Self::Replace { path, bytes, .. } => crate::home::write_private(path, bytes),
            Self::Rename(from, to) => std::fs::rename(from, to),
        }
    }
}

enum Completion { File(usize, PathBuf, Option<PathBuf>), Released }

enum Command {
    File(usize, FileOp, Arc<Home>),
    Release(Box<dyn Send>),
}

#[derive(Default)]
struct Outbox {
    front: Option<Command>,
    fifo: LinkedList<Command>,
    congested: bool,
}

#[cfg(any(test, feature = "test-hooks"))]
pub enum HookPoint<'a> {
    BeforeFileOp { index: usize, path: &'a Path, rebuildable: bool },
}

#[cfg(any(test, feature = "test-hooks"))]
pub enum HookAction { Continue, Fail(io::ErrorKind), Crash, Panic }

#[cfg(any(test, feature = "test-hooks"))]
pub struct TestHooks {
    pub capacity: usize,
    pub on_op: Box<dyn FnMut(HookPoint<'_>) -> HookAction + Send>,
}

/// The worker and the dispatcher can both be first to observe a disconnect.
struct WorkerState {
    failure: Mutex<Option<SaveFailure>>,
    dead: AtomicBool,
    abandon: AtomicBool,
    closing: AtomicBool,
    retry: AtomicUsize,
    files_pending: AtomicUsize,
    wake: Mutex<Box<dyn Fn() + Send>>,
    #[cfg(any(test, feature = "test-hooks"))]
    wake_state: (Mutex<u64>, std::sync::Condvar),
}

impl WorkerState {
    fn wake(&self) {
        #[cfg(any(test, feature = "test-hooks"))]
        {
            let (generation, changed) = &self.wake_state;
            *generation.lock().unwrap() += 1;
            changed.notify_all();
        }
        (self.wake.lock().unwrap())();
    }

    fn stopped(&self) {
        let mut failure = self.failure.lock().unwrap();
        if *failure == Some(SaveFailure::WorkerStopped) { return }
        *failure = Some(SaveFailure::WorkerStopped);
        self.dead.store(true, Ordering::Release);
        drop(failure);
        eprintln!("Town home writer: worker stopped");
        self.wake();
    }

    fn shutting_down(&self) -> bool {
        self.closing.load(Ordering::Acquire) || self.abandon.load(Ordering::Acquire)
    }
}

struct AliveGuard(Arc<WorkerState>);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        if thread::panicking() || !self.0.shutting_down() { self.0.stopped() }
    }
}

pub struct HomeWriter {
    pub(crate) home: Arc<Home>,
    tx: Option<mpsc::SyncSender<Command>>,
    tail: Option<mpsc::SyncSender<LinkedList<Command>>>,
    outbox: Mutex<Outbox>,
    completed: Mutex<mpsc::Receiver<Completion>>,
    files: Mutex<HashMap<PathBuf, (usize, FileView)>>,
    sequence: AtomicUsize,
    state: Arc<WorkerState>,
    outstanding: AtomicUsize,
    thread: Option<thread::JoinHandle<()>>,
}

impl HomeWriter {
    pub fn start(home: Arc<Home>, wake: impl Fn() + Send + 'static) -> io::Result<Arc<Self>> {
        Self::spawn(home, wake, mpsc::sync_channel(16), #[cfg(any(test, feature = "test-hooks"))] None)
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn start_with_hooks(home: Arc<Home>, wake: impl Fn() + Send + 'static, hooks: TestHooks) -> io::Result<Arc<Self>> {
        Self::spawn(home, wake, mpsc::sync_channel(hooks.capacity), Some(hooks))
    }

    fn spawn(home: Arc<Home>, wake: impl Fn() + Send + 'static,
        (tx, rx): (mpsc::SyncSender<Command>, mpsc::Receiver<Command>),
        #[cfg(any(test, feature = "test-hooks"))] mut hooks: Option<TestHooks>,
    ) -> io::Result<Arc<Self>> {
        let (tail, last) = mpsc::sync_channel::<LinkedList<Command>>(1);
        let (done, completed) = mpsc::channel();
        let state = Arc::new(WorkerState {
            failure: Default::default(), dead: Default::default(), abandon: Default::default(),
            closing: Default::default(), files_pending: Default::default(), retry: Default::default(),
            wake: Mutex::new(Box::new(wake)),
            #[cfg(any(test, feature = "test-hooks"))]
            wake_state: Default::default(),
        });
        let worker = state.clone();
        let thread = thread::Builder::new().name("Town home writer".into()).spawn(move || {
            let _alive = AliveGuard(worker.clone());
            #[cfg(any(test, feature = "test-hooks"))]
            let mut index = 0;
            for command in rx.into_iter().chain(last.into_iter().flatten()) {
                if worker.abandon.load(Ordering::Acquire) { return }
                let completion = match command {
                    Command::Release(value) => {
                        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value))).is_err() {
                            eprintln!("Town home writer: a released value panicked during drop");
                        }
                        Completion::Released
                    }
                    Command::File(seq, op, _home) => {
                        let mut delay_ms = 250;
                        let mut retrying = false;
                        loop {
                            #[cfg(any(test, feature = "test-hooks"))]
                            let injected = hooks.as_mut().map(|hooks| {
                                let path = match &op { FileOp::Replace { path, .. } | FileOp::Rename(path, _) => path };
                                let action = (hooks.on_op)(HookPoint::BeforeFileOp { index, path, rebuildable: op.rebuildable() });
                                index += 1;
                                action
                            });
                            #[cfg(any(test, feature = "test-hooks"))]
                            let result = match injected {
                                Some(HookAction::Crash) => return,
                                Some(HookAction::Panic) => panic!("home writer test hook"),
                                Some(HookAction::Fail(kind)) => Err(kind.into()),
                                _ => op.persist(),
                            };
                            #[cfg(not(any(test, feature = "test-hooks")))]
                            let result = op.persist();
                            let Err(err) = result else { break };
                            if !retrying { eprintln!("Town home writer: {err}"); }
                            if op.rebuildable() { break }
                            retrying = true;
                            worker.retry.store(delay_ms, Ordering::Release);
                            *worker.failure.lock().unwrap() = Some(SaveFailure::Io(err.kind()));
                            worker.wake();
                            delay_ms = (delay_ms * 2).min(5000);
                            while worker.retry.load(Ordering::Acquire) != 0 {
                                if worker.shutting_down() { return }
                                thread::park();
                            }
                            if worker.abandon.load(Ordering::Acquire) { return }
                        }
                        if retrying { *worker.failure.lock().unwrap() = None; }
                        if !op.rebuildable() { worker.files_pending.fetch_sub(1, Ordering::AcqRel); }
                        match op {
                            FileOp::Replace { path, .. } => Completion::File(seq, path, None),
                            FileOp::Rename(from, to) => Completion::File(seq, from, Some(to)),
                        }
                    }
                };
                let _ = done.send(completion);
                worker.wake();
            }
        })?;
        let writer = Arc::new(Self { home, tx: Some(tx), tail: Some(tail), outbox: Mutex::default(),
            completed: Mutex::new(completed), files: Mutex::default(), sequence: AtomicUsize::default(),
            state, outstanding: AtomicUsize::default(), thread: Some(thread) });
        writer.home.attach_writer(&writer);
        Ok(writer)
    }

    fn file(&self, op: FileOp) {
        // The link worker can submit alongside the UI. Publish the view and enqueue
        // the command in the same order, even when both producers replace one path.
        let mut outbox = self.outbox.lock().unwrap();
        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut files = self.files.lock().unwrap();
        match &op {
            FileOp::Replace { path, bytes, .. } => { files.insert(path.clone(), (seq, FileView::Bytes(bytes.clone()))); }
            FileOp::Rename(from, to) => {
                let old = files.insert(from.clone(), (seq, FileView::Absent));
                let view = old.map(|(_, view)| view).unwrap_or_else(|| FileView::Archive(from.clone()));
                files.insert(to.clone(), (seq, view));
            }
        }
        drop(files);
        self.submit(&mut outbox, Command::File(seq, op, self.home.clone()));
    }

    pub fn failure(&self) -> Option<SaveFailure> { *self.state.failure.lock().unwrap() }
    pub fn unsaved_changes(&self) -> usize { self.state.files_pending.load(Ordering::Acquire) }
    pub fn abandoned(&self) -> bool { self.state.abandon.load(Ordering::Acquire) }
    pub fn abandon(&self) {
        self.state.abandon.store(true, Ordering::Release);
        self.thread.as_ref().unwrap().thread().unpark();
    }

    pub fn release(&self, value: impl Send + 'static) {
        self.submit(&mut self.outbox.lock().unwrap(), Command::Release(Box::new(value)));
    }

    fn submit(&self, outbox: &mut Outbox, cmd: Command) {
        if let Command::File(_, FileOp::Replace { path, rebuildable, .. }, _) = &cmd {
            if let Some(last) = outbox.fifo.back_mut().or(outbox.front.as_mut()) {
                if matches!(last, Command::File(_, FileOp::Replace { path: prev, rebuildable: mode, .. }, _)
                    if prev == path && mode == rebuildable) {
                    *last = cmd;
                    return
                }
            }
        }
        if matches!(&cmd, Command::File(_, op, _) if !op.rebuildable()) {
            self.state.files_pending.fetch_add(1, Ordering::Relaxed);
        }
        self.outstanding.fetch_add(1, Ordering::Relaxed);
        if outbox.front.is_some() {
            outbox.fifo.push_back(cmd);
        } else {
            outbox.front = Some(cmd);
            self.dispatch(outbox);
        }
    }

    fn dispatch(&self, outbox: &mut Outbox) {
        while let Some(cmd) = outbox.front.take() {
            match self.tx.as_ref().unwrap().try_send(cmd) {
                Ok(()) => outbox.front = outbox.fifo.pop_front(),
                Err(mpsc::TrySendError::Full(cmd)) => {
                    outbox.front = Some(cmd);
                    if !outbox.congested {
                        eprintln!("Town home writer: the queue is backed up; retaining writes");
                        outbox.congested = true;
                    }
                    return
                }
                Err(mpsc::TrySendError::Disconnected(cmd)) => {
                    outbox.front = Some(cmd);
                    if !self.state.shutting_down() { self.state.stopped(); }
                    return
                }
            }
        }
        outbox.congested = false;
    }

    pub fn tick(&self, timer_fired: bool, _completed: impl FnMut(TownId)) -> Option<Duration> {
        if timer_fired && self.state.retry.swap(0, Ordering::AcqRel) != 0 {
            self.thread.as_ref().unwrap().thread().unpark();
        }
        let completed = self.completed.lock().unwrap();
        let mut made_space = false;
        loop {
            match completed.try_recv() {
                Ok(completion) => {
                    made_space = true;
                    if let Completion::File(seq, first, second) = completion {
                        let mut files = self.files.lock().unwrap();
                        for path in Some(first).into_iter().chain(second) {
                            if files.get(&path).is_some_and(|(n, _)| *n == seq) { files.remove(&path); }
                        }
                    }
                    self.outstanding.fetch_sub(1, Ordering::Relaxed);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !self.state.shutting_down() { self.state.stopped(); }
                    break
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        drop(completed);
        if made_space { self.dispatch(&mut self.outbox.lock().unwrap()); }
        let delay_ms = self.state.retry.load(Ordering::Acquire);
        (delay_ms != 0).then(|| Duration::from_millis(delay_ms as u64))
    }

    pub fn is_drained(&self) -> bool { self.outstanding.load(Ordering::Relaxed) == 0 }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn wake_gen(&self) -> u64 { *self.state.wake_state.0.lock().unwrap() }
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn wait_wake(&self, seen: u64) {
        let (generation, changed) = &self.state.wake_state;
        drop(changed.wait_while(generation.lock().unwrap(), |g| *g == seen).unwrap());
    }
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn is_dead(&self) -> bool { self.state.dead.load(Ordering::Acquire) }
}

impl Drop for HomeWriter {
    fn drop(&mut self) {
        let outbox = self.outbox.get_mut().unwrap();
        if let Some(cmd) = outbox.front.take() { outbox.fifo.push_front(cmd); }
        if !self.abandoned() {
            let outbox = self.outbox.get_mut().unwrap();
            let _ = self.tail.as_ref().unwrap().try_send(std::mem::take(&mut outbox.fifo));
        }
        self.state.closing.store(true, Ordering::Release);
        drop(self.tail.take());
        drop(self.tx.take());
        let handle = self.thread.take().unwrap();
        handle.thread().unpark();
        if !self.abandoned() { let _ = handle.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn home() -> Arc<Home> {
        let key = crate::key::DeviceKey::generate().unwrap().public();
        let dir = std::env::temp_dir().join(format!("town-writer-{}", crate::hex(&key[1..9])));
        Arc::new(Home::open(&dir).unwrap())
    }

    #[test]
    fn rebuildable_failure_does_not_stall_durable_writes_and_clears_read_through() {
        let home = home();
        let dir = home.dir.clone();
        let writer = HomeWriter::start(home.clone(), || {}).unwrap();
        let files = home.files();
        let checkpoint = home.town_checkpoint(&[1; 32]);
        fs::create_dir(checkpoint.with_extension("new")).unwrap();
        files.replace_rebuildable(&checkpoint, b"checkpoint".to_vec()).unwrap();
        assert_eq!(files.read_capped(&checkpoint, 5).unwrap(), b"check", "queued reads respect the caller's limit");
        assert_eq!(writer.unsaved_changes(), 0, "a checkpoint does not count as an unsaved change");
        files.replace(&home.record_path(), b"Kit".to_vec()).unwrap();
        files.settle();
        assert_eq!(fs::read(home.record_path()).unwrap(), b"Kit", "durable writes pass a failed checkpoint");
        assert_eq!(writer.failure(), None, "a checkpoint cannot start a save-failure spell");
        assert_eq!(files.read(&checkpoint).unwrap_err().kind(), io::ErrorKind::NotFound, "failed checkpoint leaves no queued bytes");
        drop((writer, home));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn release_panic_is_isolated_and_worker_death_sets_failure() {
        struct PanicOnDrop;
        impl Drop for PanicOnDrop { fn drop(&mut self) { panic!("release test"); } }
        let home = home();
        let dir = home.dir.clone();
        let writer = HomeWriter::start_with_hooks(home.clone(), || {}, TestHooks {
            capacity: 1,
            on_op: Box::new(|HookPoint::BeforeFileOp { index, .. }| {
                if index == 1 { HookAction::Panic } else { HookAction::Continue }
            }),
        }).unwrap();
        writer.release(PanicOnDrop);
        let files = home.files();
        files.replace(&home.record_path(), b"Ana".to_vec()).unwrap();
        files.settle();
        assert_eq!(fs::read(home.record_path()).unwrap(), b"Ana", "the writer survived the released value's panic");
        assert_eq!(writer.failure(), None, "a release panic does not mark writes failed");
        files.replace(&home.record_path(), b"Ben".to_vec()).unwrap();
        files.settle();
        assert!(writer.is_dead(), "an unexpected worker exit wakes the caller");
        assert_eq!(writer.failure(), Some(SaveFailure::WorkerStopped), "the panic is recorded as a stopped worker");
        files.replace(&home.record_path(), b"Rik".to_vec()).unwrap();
        writer.tick(false, |_| {});
        assert_eq!(writer.failure(), Some(SaveFailure::WorkerStopped), "later submission preserves the death notice");
        assert!(!writer.is_drained(), "unwritten changes remain outstanding");
        drop((writer, home));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn drop_joins_and_releases_home_lock_deterministically() {
        let home = home();
        let dir = home.dir.clone();
        let (hold, held) = mpsc::channel();
        let writer = HomeWriter::start_with_hooks(home.clone(), || {}, TestHooks {
            capacity: 1,
            on_op: Box::new(move |HookPoint::BeforeFileOp { index, .. }| {
                if index == 0 { held.recv().unwrap(); }
                HookAction::Continue
            }),
        }).unwrap();
        let paths = [home.record_path(), home.peers_path(), home.layout_path(), home.call_devices_path()];
        for (i, path) in paths.iter().enumerate() { home.files().replace(path, vec![i as u8]).unwrap(); }
        hold.send(()).unwrap();
        drop(writer);
        drop(home);
        let reopened = Home::open(&dir).expect("drop joins the writer and releases the home lock on the first attempt");
        for (i, path) in paths.iter().enumerate() {
            assert_eq!(fs::read(path).unwrap(), [i as u8], "queued file {i} reached disk during drop");
        }
        drop(reopened);
        fs::remove_dir_all(dir).unwrap();
    }
}
