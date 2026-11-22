//! Part meshes read once per file content: a body's `.glb` is parsed the first time a body needs it
//! and shared (`Arc<Mesh>`) by every view that draws or checks it, until the file's bytes change.
//!
//! The key is the path plus the file's SHA-256. A call stats the file; when its length and modified
//! time match the entry, it returns the entry without reading. Otherwise it reads and hashes the
//! bytes: the same hash (a rewrite with the same content) keeps the parsed mesh, a new hash parses
//! again and replaces the entry. A failed read or parse drops the entry and returns the issue, so a
//! deleted or broken file is never served from the cache.
//!
//! Cost: called on a worker or the pool, never per frame. A hit is one `stat` and a short lock; a
//! changed file is one read, one hash (linear in its size) and one parse. The lock is held only to
//! look up or store an entry, never across I/O or parsing; two workers missing the same file at once
//! both parse it and the later store wins. Memory is one mesh per distinct file in use.
//!
//! Plate assets use virtual `asset:<sha256 hex>/<name>` paths: [`MeshCache::put`] parses once per
//! distinct key, even on concurrent puts, and [`MeshCache::get`] reads them without a file stat.
//! Asset entries (including failures) stay for the process, bounded by distinct assets. Parsing
//! runs on the caller's worker, outside the map lock; an asset is ready when `put` returns.

use crate::glb::read_glb;
use crate::{Issue, Mesh};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// A parsed file or asset: the mesh, the reader's warnings, and its content SHA-256 (hex).
#[derive(Clone, Debug)]
pub struct CachedMesh {
    pub mesh: Arc<Mesh>,
    pub warnings: Arc<[Issue]>,
    pub sha256: Arc<str>,
}

struct Entry {
    len: u64,
    modified: Option<SystemTime>,
    mesh: CachedMesh,
}

#[derive(Default)]
pub struct MeshCache {
    entries: Mutex<HashMap<PathBuf, Entry>>,
    assets: Mutex<HashMap<PathBuf, Arc<OnceLock<Result<CachedMesh, Issue>>>>>,
}

impl MeshCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The process's one cache: the 3D view, the case checks and the slicer's part bodies share it.
    pub fn shared() -> &'static MeshCache {
        static SHARED: OnceLock<MeshCache> = OnceLock::new();
        SHARED.get_or_init(MeshCache::new)
    }

    /// Register immutable `.glb` bytes under `asset:<sha256 hex>/<name>`. The caller supplies the
    /// content hash; another put of that key reuses its result without parsing or hashing again.
    /// One parse per distinct asset, retained for the process so all readers share the same mesh.
    pub fn put(&self, key: &str, bytes: &[u8]) -> Result<(), Issue> {
        let Some((sha256, _)) = key.strip_prefix("asset:").and_then(|s| s.split_once('/')) else {
            return Err(Issue::new(0, key, "a mesh asset key must be asset:<sha256 hex>/<name>"));
        };
        let entry = self.assets.lock().unwrap_or_else(|poison| poison.into_inner())
            .entry(PathBuf::from(key)).or_default().clone();
        entry.get_or_init(|| read_glb(bytes).map(|read| CachedMesh {
            mesh: Arc::new(read.mesh), warnings: read.warnings.into(), sha256: sha256.into(),
        }).map_err(|issue| Issue { field: format!("{key}: {}", issue.field), ..issue }))
            .as_ref().map(|_| ()).map_err(Clone::clone)
    }

    /// The mesh in the `.glb` at `path`, or a registered virtual asset without filesystem I/O.
    /// The issue names the path in its field; an absent or still registering asset is unavailable.
    pub fn get(&self, path: &Path) -> Result<CachedMesh, Issue> {
        let field = || path.display().to_string();
        if path.to_str().is_some_and(|s| s.starts_with("asset:")) {
            let entry = self.assets.lock().unwrap_or_else(|poison| poison.into_inner()).get(path).cloned();
            return entry.and_then(|entry| entry.get().cloned())
                .unwrap_or_else(|| Err(Issue::new(0, field(), "mesh asset is not registered")));
        }
        let (len, modified) = match std::fs::metadata(path) {
            Ok(m) => (m.len(), m.modified().ok()),
            Err(e) => {
                self.forget(path);
                return Err(Issue::new(0, field(), e.to_string()));
            }
        };
        let cached = self.lock().get(path).map(|e| (e.len == len && e.modified == modified, e.mesh.clone()));
        if let Some((true, mesh)) = cached {
            return Ok(mesh);
        }
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.forget(path);
                return Err(Issue::new(0, field(), e.to_string()));
            }
        };
        let sha256: Arc<str> = makepad_core_util::sha256::sha256_hex(&bytes).into();
        let mesh = match cached {
            Some((_, mesh)) if mesh.sha256 == sha256 => mesh,
            _ => match read_glb(&bytes) {
                Ok(read) => CachedMesh { mesh: Arc::new(read.mesh), warnings: read.warnings.into(), sha256 },
                Err(issue) => {
                    self.forget(path);
                    return Err(Issue { field: format!("{}: {}", field(), issue.field), ..issue });
                }
            },
        };
        // The stat taken before the read: a write during it shows as a changed file on the next call.
        self.lock().insert(path.to_path_buf(), Entry { len, modified, mesh: mesh.clone() });
        Ok(mesh)
    }

    /// The number of file and asset entries held, including failed asset registrations.
    pub fn len(&self) -> usize {
        let files = self.lock().len();
        files + self.assets.lock().unwrap_or_else(|poison| poison.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn forget(&self, path: &Path) {
        self.lock().remove(path);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Entry>> {
        // A panic while holding the lock leaves only whole entries behind: keep using them.
        self.entries.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}
