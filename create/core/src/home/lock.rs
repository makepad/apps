//! The home lock: one process, one owner of a home.
//!
//! `local/lock` is opened by this module and by nothing else: closing any descriptor of a file
//! drops a POSIX record lock the process holds on it, so a stray open of `local/lock` that
//! closes would release the home.
//!
//! On Unix the lock is a POSIX `fcntl(F_SETLK)` write lock. It belongs to the process, a forked
//! child does not inherit it, and it goes away when the process exits. (A `flock` belongs to the
//! open file description, which a child between `fork` and `exec` still holds: the home would
//! refuse to reopen until that child execs.) A second `F_SETLK` from the same process succeeds,
//! so a process-wide registry of lock-file paths says `InUse` for the second opener instead.
//!
//! On Windows `LockFileEx` locks the handle; std opens files non-inheritable, so no child holds
//! it. The registry is used there too, so a second open in one process says `InUse` everywhere.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

pub(super) enum LockError {
    InUse,
    Io(io::Error),
}

impl From<io::Error> for LockError {
    fn from(e: io::Error) -> Self {
        LockError::Io(e)
    }
}

static HELD: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

fn held() -> MutexGuard<'static, Option<HashSet<PathBuf>>> {
    // The set is only inserted into and removed from, so a poisoned guard is still consistent.
    HELD.lock().unwrap_or_else(|e| e.into_inner())
}

/// The exclusive lock on a home's `local/lock`, released when dropped.
pub(super) struct HomeLock {
    file: Option<File>,
    key: PathBuf,
}

impl HomeLock {
    /// Locks `<dir>/local/lock`, creating it. `InUse` at once when another holder, in this
    /// process or another, has it. Never waits.
    pub(super) fn acquire(dir: &Path) -> Result<HomeLock, LockError> {
        let key = std::fs::canonicalize(dir)?.join("local/lock");
        if !held().get_or_insert_with(HashSet::new).insert(key.clone()) {
            return Err(LockError::InUse);
        }
        // From here the entry is ours: any failure gives it back.
        let mut lock = HomeLock { file: None, key };
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&lock.key)?;
        lock_file(&file)?;
        lock.file = Some(file);
        Ok(lock)
    }
}

impl Drop for HomeLock {
    fn drop(&mut self) {
        // Close first: that releases the lock. Only then may another opener in this process take it.
        drop(self.file.take());
        if let Some(set) = held().as_mut() {
            set.remove(&self.key);
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux", target_os = "android"))]
pub(super) mod sys {
    use std::ffi::c_int;

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub const F_SETLK: c_int = 8;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub const F_WRLCK: i16 = 3;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub const F_SETLK: c_int = 6;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub const F_WRLCK: i16 = 1;
    pub const SEEK_SET: i16 = 0;

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[repr(C)]
    pub struct Flock {
        pub l_start: i64,
        pub l_len: i64,
        pub l_pid: i32,
        pub l_type: i16,
        pub l_whence: i16,
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[repr(C)]
    pub struct Flock {
        pub l_type: i16,
        pub l_whence: i16,
        pub l_start: i64,
        pub l_len: i64,
        pub l_pid: i32,
    }

    extern "C" {
        pub fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    }
}

/// Write-locks the whole file for this process.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux", target_os = "android"))]
fn lock_file(file: &File) -> Result<(), LockError> {
    use std::os::unix::io::AsRawFd;
    let mut request = sys::Flock { l_start: 0, l_len: 0, l_pid: 0, l_type: sys::F_WRLCK, l_whence: sys::SEEK_SET };
    if unsafe { sys::fcntl(file.as_raw_fd(), sys::F_SETLK, &mut request as *mut sys::Flock) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // Another process holds a conflicting lock: EACCES or EAGAIN.
        Some(13) | Some(11) | Some(35) => Err(LockError::InUse),
        _ => Err(LockError::Io(error)),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux", target_os = "android")))]
fn lock_file(file: &File) -> Result<(), LockError> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Err(LockError::InUse),
        Err(std::fs::TryLockError::Error(e)) => Err(LockError::Io(e)),
    }
}
