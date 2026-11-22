//! The files a played document reads, by path, on every target: the one
//! reader of document trees and their media (the folder reader, the
//! resolvers, the decoders).
//!
//! Natively a path is a file on disk and these are `std::fs`. In the
//! browser there is no disk: the page fills this store, and every reader
//! reads the same paths from it. The store is filled from wherever the page
//! has its files -- fetched over HTTP (a player served from a folder: the
//! trace host, a development server) or the data the web export packs into
//! the wasm -- and the readers never know which.
//!
//! Paths in the store are page-relative (`pdoom/media/song.mp3`), `/`
//! separated, normalised ([`canonicalize`]). A directory is known by its
//! listing (`store::install_dir`). A path the store does not hold yet is
//! recorded as wanted (`store::take_wanted`) and the page fetches it, so
//! what is fetched is exactly what was read. On the page's own thread such
//! a read fails with [`std::io::ErrorKind::WouldBlock`] (the page runs the
//! read again once it has come); on any other thread (a load worker) it
//! wakes the page (`store::set_waker`) and waits until the path has come.

use std::io;
use std::path::{Path, PathBuf};

/// What a directory holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// Natively: a symlink (a tree refuses them).
    Symlink,
    Other,
}

/// A directory entry: its name (one path component) and kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
}

impl Entry {
    pub fn is_file(&self) -> bool {
        self.kind == EntryKind::File
    }

    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::{Entry, EntryKind};
    use std::io;
    use std::path::{Path, PathBuf};

    pub fn read(path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    pub fn read_head(path: &Path, len: usize) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let mut head = Vec::with_capacity(len);
        std::fs::File::open(path)?.take(len as u64).read_to_end(&mut head)?;
        Ok(head)
    }

    pub fn read_dir(dir: &Path) -> io::Result<Vec<Entry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let kind = if ty.is_symlink() {
                EntryKind::Symlink
            } else if ty.is_dir() {
                EntryKind::Dir
            } else if ty.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            let Ok(name) = entry.file_name().into_string() else { continue };
            out.push(Entry { name, kind });
        }
        Ok(out)
    }

    pub fn is_file(path: &Path) -> bool {
        path.is_file()
    }

    pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{Entry, EntryKind};
    use std::collections::{BTreeSet, HashMap};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

    #[derive(Default)]
    pub struct Store {
        pub files: HashMap<String, Arc<[u8]>>,
        pub dirs: HashMap<String, Vec<Entry>>,
        /// Paths asked for that the page has no file or directory at.
        pub absent: BTreeSet<String>,
        pub wanted: BTreeSet<(String, bool)>,
        /// The page's thread (it never waits) and how a waiting thread wakes it.
        pub page: Option<std::thread::ThreadId>,
        pub waker: Option<Box<dyn Fn() + Send>>,
    }

    pub struct Shared {
        pub store: Mutex<Store>,
        /// Signalled whenever something is installed.
        pub came: Condvar,
    }

    pub fn shared() -> &'static Shared {
        static SHARED: OnceLock<Shared> = OnceLock::new();
        SHARED.get_or_init(|| Shared { store: Mutex::new(Store::default()), came: Condvar::new() })
    }

    pub fn store() -> MutexGuard<'static, Store> {
        shared().store.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `(key, is a directory)` is wanted: on the page's thread the read fails
    /// now; elsewhere the page is woken and the store comes back once
    /// something was installed (the caller looks again).
    fn want(mut store: MutexGuard<'static, Store>, key: &str, dir: bool) -> Result<MutexGuard<'static, Store>, io::Error> {
        store.wanted.insert((key.to_string(), dir));
        let on_page = store.page.is_none_or(|page| page == std::thread::current().id());
        if on_page {
            return Err(pending(key));
        }
        if let Some(waker) = &store.waker {
            waker();
        }
        Ok(shared().came.wait(store).unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    pub fn key(path: &Path) -> String {
        super::normalise(path).to_string_lossy().into_owned()
    }

    fn parent_listing<'a>(store: &'a Store, key: &str) -> Option<&'a Vec<Entry>> {
        let (dir, _) = key.rsplit_once('/').unwrap_or(("", key));
        store.dirs.get(dir)
    }

    fn name_of(key: &str) -> &str {
        key.rsplit('/').next().unwrap_or(key)
    }

    fn not_found(key: &str) -> io::Error {
        io::Error::new(io::ErrorKind::NotFound, format!("{key}: no such file"))
    }

    fn pending(key: &str) -> io::Error {
        io::Error::new(io::ErrorKind::WouldBlock, format!("{key}: not fetched yet"))
    }

    fn file(path: &Path) -> io::Result<Arc<[u8]>> {
        let key = key(path);
        let mut store = store();
        loop {
            if let Some(bytes) = store.files.get(&key) {
                return Ok(bytes.clone());
            }
            let listed = parent_listing(&store, &key).map(|entries| entries.iter().any(|e| e.is_file() && e.name == name_of(&key)));
            if store.absent.contains(&key) || listed == Some(false) {
                return Err(not_found(&key));
            }
            store = want(store, &key, false)?;
        }
    }

    pub fn read(path: &Path) -> io::Result<Vec<u8>> {
        file(path).map(|bytes| bytes.to_vec())
    }

    pub fn read_head(path: &Path, len: usize) -> io::Result<Vec<u8>> {
        file(path).map(|bytes| bytes[..len.min(bytes.len())].to_vec())
    }

    pub fn read_dir(dir: &Path) -> io::Result<Vec<Entry>> {
        let key = key(dir);
        let mut store = store();
        loop {
            if let Some(entries) = store.dirs.get(&key) {
                return Ok(entries.clone());
            }
            if store.absent.contains(&key) {
                return Err(io::Error::new(io::ErrorKind::NotFound, format!("{key}: no such directory")));
            }
            store = want(store, &key, true)?;
        }
    }

    /// Answered by the parent's listing (on the page's thread: false until
    /// it has come, and the run is made again).
    pub fn is_file(path: &Path) -> bool {
        let key = key(path);
        let (dir, _) = key.rsplit_once('/').unwrap_or(("", &key));
        let dir = dir.to_string();
        let mut store = store();
        loop {
            if store.files.contains_key(&key) {
                return true;
            }
            if let Some(entries) = parent_listing(&store, &key) {
                return entries.iter().any(|e| e.kind == EntryKind::File && e.name == name_of(&key));
            }
            if store.absent.contains(&key) || store.absent.contains(&dir) {
                return false;
            }
            store = match want(store, &dir, true) {
                Ok(store) => store,
                Err(_) => return false,
            };
        }
    }

    pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
        Ok(super::normalise(path))
    }
}

/// A file's bytes (`std::fs::read` natively).
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    imp::read(path)
}

/// A file's text (`std::fs::read_to_string` natively).
pub fn read_to_string(path: &Path) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{}: not UTF-8", path.display())))
}

/// At most the first `len` bytes of a file (a format sniffed by its head).
pub fn read_head(path: &Path, len: usize) -> io::Result<Vec<u8>> {
    imp::read_head(path, len)
}

/// What a directory holds, in no particular order.
pub fn read_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    imp::read_dir(dir)
}

/// Whether `path` is a file (false when it is not, or cannot be known).
pub fn is_file(path: &Path) -> bool {
    imp::is_file(path)
}

/// The path made absolute and normal (natively the file system's own
/// answer; in the browser the page-relative path without `.`, `..` or
/// doubled separators).
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    imp::canonicalize(path)
}

/// `path` without `.` components, `..` taken back, no leading `/`.
pub fn normalise(path: &Path) -> PathBuf {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.to_str().unwrap_or("").split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    PathBuf::from(parts.join("/"))
}

/// The store a page fills (the browser only).
#[cfg(target_arch = "wasm32")]
pub mod store {
    pub use super::imp::key;
    use super::Entry;
    use std::path::Path;

    /// `path` holds `bytes`.
    pub fn install_file(path: &Path, bytes: Vec<u8>) {
        let key = key(path);
        let mut store = super::imp::store();
        store.absent.remove(&key);
        store.files.insert(key, bytes.into());
        super::imp::shared().came.notify_all();
    }

    /// The directory `path` holds `entries`.
    pub fn install_dir(path: &Path, entries: Vec<Entry>) {
        let key = key(path);
        let mut store = super::imp::store();
        store.absent.remove(&key);
        store.dirs.insert(key, entries);
        super::imp::shared().came.notify_all();
    }

    /// The page has nothing at `path`.
    pub fn install_absent(path: &Path) {
        super::imp::store().absent.insert(key(path));
        super::imp::shared().came.notify_all();
    }

    /// The calling thread is the page's (its reads never wait), and `waker`
    /// wakes it when another thread wants a path.
    pub fn set_page(waker: Box<dyn Fn() + Send>) {
        let mut store = super::imp::store();
        store.page = Some(std::thread::current().id());
        store.waker = Some(waker);
    }

    /// The paths read since the last call that the store did not hold:
    /// `(path, is a directory)`. A run that wanted any is run again once
    /// they are installed.
    pub fn take_wanted() -> Vec<(String, bool)> {
        std::mem::take(&mut super::imp::store().wanted).into_iter().collect()
    }
}
