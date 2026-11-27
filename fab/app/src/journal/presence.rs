//! Who else has this plate open. Each open plate holds a locked file of its own in
//! `<plate dir>/presence/`, beside `journal/`; a file whose lock another process can take belongs to no live
//! Fab (a crash left it) and is swept. Runs only on a worker; the UI reads one flag.
use super::{invalid, open_regular};
use makepad_widgets::makepad_platform::thread::{ThreadOptions, ThreadSpawner};
use makepad_widgets::SignalToUI;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::{Path, PathBuf},
    sync::{atomic::{AtomicBool, AtomicU64, Ordering}, mpsc::{self, RecvTimeoutError, Sender}, Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const DIR: &str = "presence";
/// One directory listing and one `try_lock` per other file, on a worker.
const INTERVAL: Duration = Duration::from_secs(2);

pub(super) struct Presence {
    dir: PathBuf,
    path: PathBuf,
    file: Option<File>,
}

impl Presence {
    /// Claims nothing yet. Only an existing plate folder gets a `presence/`; `journal/` is never made.
    pub(super) fn new(plate: &Path) -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let name = format!("{}-{nanos:x}-{:x}.lock", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed));
        let dir = plate.join(DIR);
        Self { path: dir.join(name), dir, file: None }
    }

    /// Hold our file if the plate folder exists, then count the other live holders.
    /// A symlinked `presence` folder is refused.
    pub(super) fn others(&mut self) -> io::Result<usize> {
        if self.file.is_some() && fs::symlink_metadata(&self.path).is_err() { self.file = None }
        if self.file.is_none() {
            match fs::symlink_metadata(&self.dir) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return Err(invalid()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    match fs::create_dir(&self.dir) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                        // no plate folder: nothing to claim
                        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
                        Err(e) => return Err(e),
                    }
                    if !fs::symlink_metadata(&self.dir)?.is_dir() { return Err(invalid()) }
                }
                Err(e) => return Err(e),
            }
            let file = OpenOptions::new().read(true).write(true).create_new(true).open(&self.path)?;
            match file.try_lock() {
                Ok(()) => self.file = Some(file),
                // a sweeper holds it for a moment; the next check claims a fresh one
                Err(TryLockError::WouldBlock) => { drop(file); let _ = fs::remove_file(&self.path); }
                Err(TryLockError::Error(e)) => return Err(e),
            }
        }
        live_holders(&self.dir, std::process::id(), Some(&self.path))
    }
}

pub(crate) fn other_presence_holders(plate: &Path, own_pid: u32) -> io::Result<usize> {
    live_holders(&plate.join(DIR), own_pid, None)
}

/// A presence watch excludes only its own file and sweeps stale names. Delete
/// samples existing holders without sweeping, excluding all files of its process.
pub(super) fn live_holders(dir: &Path, own_pid: u32, watch: Option<&Path>) -> io::Result<usize> {
    let prefix = format!("{own_pid}-");
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => (),
        Ok(_) => return Err(invalid()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    }
    let mut live = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let own = match watch {
            Some(own) => path == own,
            None => entry.file_name().to_str().is_some_and(|name| name.starts_with(&prefix)),
        };
        if own { continue }
        let file = match open_regular(&path) {
            Ok(Some(file)) => file,
            Ok(None) => continue,
            Err(_) if watch.is_some() => continue,
            Err(e) => return Err(e),
        };
        match file.try_lock() {
            Err(TryLockError::WouldBlock) => live += 1,
            Ok(()) if watch.is_some() => {
                let _ = file.unlock();
                drop(file);
                let _ = fs::remove_file(&path);
            }
            Ok(()) => (),
            Err(TryLockError::Error(e)) => return Err(e),
        }
    }
    Ok(live)
}

impl Drop for Presence {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
            drop(file);
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// What the worker and `Watch::release` share. The worker holds `presence` only around one
/// `others()` call, so a release waits for at most one directory listing.
struct Shared {
    presence: Mutex<Option<Presence>>,
    elsewhere: AtomicBool,
}

impl Shared {
    fn new(plate: &Path) -> Self { Self { presence: Mutex::new(Some(Presence::new(plate))), elsewhere: AtomicBool::new(false) } }

    /// One round. False once released.
    fn tick(&self, warned: &mut bool) -> bool {
        let mut slot = self.presence.lock().unwrap();
        let Some(presence) = slot.as_mut() else { return false };
        match presence.others() {
            Ok(n) => {
                if self.elsewhere.swap(n > 0, Ordering::AcqRel) != (n > 0) { SignalToUI::set_ui_signal(); }
            }
            Err(e) => {
                if !*warned {
                    *warned = true;
                    makepad_widgets::log!("journal: {}: {:?}; no second-window notice", presence.dir.display(), e.kind());
                }
                if self.elsewhere.swap(false, Ordering::AcqRel) { SignalToUI::set_ui_signal(); }
            }
        }
        true
    }

    /// Unlock, close and remove our file before returning; the worker ends at its next round.
    fn release(&self) {
        drop(self.presence.lock().unwrap().take());
        if self.elsewhere.swap(false, Ordering::AcqRel) { SignalToUI::set_ui_signal(); }
    }
}

/// Dropping this ends the watch and removes the presence file.
pub(super) struct Watch {
    shared: Arc<Shared>,
    _stop: Sender<()>,
}

impl Watch {
    pub(super) fn start(plate: PathBuf, spawner: &ThreadSpawner) -> Option<Self> {
        let shared = Arc::new(Shared::new(&plate));
        let (stop, rx) = mpsc::channel::<()>();
        let worker = shared.clone();
        let task = spawner.spawn_worker(ThreadOptions { name: Some("Fab plate presence".into()), ..Default::default() }, move || {
            let mut warned = false;
            while worker.tick(&mut warned) {
                match rx.recv_timeout(INTERVAL) {
                    Err(RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            }
        }).ok()?;
        task.detach();
        Some(Self { shared, _stop: stop })
    }

    pub(super) fn elsewhere(&self) -> bool { self.shared.elsewhere.load(Ordering::Acquire) }

    /// Close this process's presence file now (the plate folder is about to move). Runs on the
    /// model host thread, never the UI thread.
    pub(super) fn release(&self) { self.shared.release() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presences_see_each_other_sweep_stale_files_and_refuse_a_linked_folder() {
        let journal = super::super::tests::temp();
        let mut a = Presence::new(&journal);
        assert_eq!(a.others().unwrap(), 0, "alone on the plate");
        assert!(!journal.join("journal").exists(), "presence never makes journal/");
        let mut b = Presence::new(&journal);
        assert_eq!(b.others().unwrap(), 1, "b sees a");
        assert_eq!(a.others().unwrap(), 1, "a sees b");
        drop(b);
        assert_eq!(a.others().unwrap(), 0, "a closed presence is no longer seen");
        let stale = journal.join(DIR).join("1-0-0.lock");
        File::create(&stale).unwrap();
        assert_eq!(a.others().unwrap(), 0, "a file nobody holds is not counted");
        assert!(!stale.exists(), "and it is swept");
        let own = a.path.clone();
        drop(a);
        assert!(!own.exists(), "closing removes the file");

        let other = super::super::tests::temp();
        let linked = other.join("journal");
        fs::create_dir(&linked).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&journal, linked.join(DIR)).unwrap();
            let err = Presence::new(&linked).others().unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "a symlinked presence folder is refused");
        }
        assert_eq!(Presence::new(&other.join("missing")).others().unwrap(), 0, "no plate folder: nothing claimed");
        assert!(!other.join("missing").exists(), "a plate without a folder takes no presence");
        fs::remove_dir_all(journal).unwrap();
        fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn release_closes_the_file_before_returning_so_the_plate_folder_can_move() {
        let plate = super::super::tests::temp();
        let shared = Shared::new(&plate);
        let mut warned = false;
        assert!(shared.tick(&mut warned), "the first round claims the file");
        let held = fs::read_dir(plate.join(DIR)).unwrap().count();
        assert_eq!(held, 1, "the watch holds one presence file");
        shared.release();
        assert_eq!(fs::read_dir(plate.join(DIR)).unwrap().count(), 0, "release removed the file before returning");
        let moved = plate.with_extension("moved");
        fs::rename(&plate, &moved).expect("with no handle inside, the plate folder moves (on Windows this proves the handle is closed)");
        assert!(!shared.tick(&mut warned), "after a release the worker's next round ends it");
        fs::remove_dir_all(moved).unwrap();
    }
}
