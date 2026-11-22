//! Pinned radio packs for Fab. Downloads run synchronously on the caller's worker.

mod fetch;
mod install;
mod manifest;

pub use fetch::{fetch, Progress};
pub use install::install;
pub use makepad_network::blocking_http::CancelToken;
pub use manifest::{espressif_s3, Manifest, PackFile, Repository};

use makepad_core_util::sha256::{to_hex, Sha256};
use makepad_network::blocking_http;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug)]
pub enum Error {
    InvalidManifest(&'static str),
    Missing(PathBuf),
    WrongType(PathBuf),
    Size {
        file: PathBuf,
        expected: u64,
        actual: u64,
    },
    Hash(PathBuf),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Http {
        file: PathBuf,
        source: blocking_http::Error,
    },
    Status {
        file: PathBuf,
        status: u16,
    },
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidManifest(reason) => write!(f, "invalid pack manifest: {reason}"),
            Self::Missing(path) => write!(f, "{} is missing", path.display()),
            Self::WrongType(path) => write!(f, "{} has the wrong file type", path.display()),
            Self::Size {
                file,
                expected,
                actual,
            } => write!(
                f,
                "{} has the wrong size (expected {expected} bytes, found {actual})",
                file.display()
            ),
            Self::Hash(file) => write!(f, "{} has the wrong SHA-256 hash", file.display()),
            Self::Io { path, source } => write!(f, "could not access {}: {source}", path.display()),
            Self::Http { file, source } => {
                write!(f, "could not fetch {}: {source}", file.display())
            }
            Self::Status { file, status } => {
                write!(f, "server returned HTTP {status} for {}", file.display())
            }
            Self::Cancelled => f.write_str("pack download cancelled"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Http { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn io_error(path: &Path, source: io::Error) -> Error {
    if source.kind() == io::ErrorKind::NotFound {
        Error::Missing(path.into())
    } else {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().expect("path has a file name").to_owned();
    name.push(suffix);
    path.with_file_name(name)
}

fn remove_file_if_present(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_error(path, e)),
        _ => Ok(()),
    }
}

// A second worker must not unlink the first worker's live .part file, or
// install its directory while the first worker is still writing to it.
static ACTIVE_STAGING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

struct StagingDir(PathBuf);

impl StagingDir {
    fn claim(path: &Path) -> Result<Self, Error> {
        let path = fs::canonicalize(path).map_err(|e| io_error(path, e))?;
        let mut active = ACTIVE_STAGING.lock().unwrap_or_else(|e| e.into_inner());
        if active.contains(&path) {
            return Err(io_error(
                &path,
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another worker is using this staging directory",
                ),
            ));
        }
        active.push(path.clone());
        Ok(Self(path))
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        let mut active = ACTIVE_STAGING.lock().unwrap_or_else(|e| e.into_inner());
        active.retain(|path| path != &self.0);
    }
}

// Check each directory before descending. Links into or out of staging could
// otherwise verify before the install rename and break afterwards.
fn pack_path(root: &Path, file: &PackFile, create_dirs: bool) -> Result<PathBuf, Error> {
    let relative = Path::new(&file.repository).join(&file.path);
    let mut path = root.to_owned();
    for component in relative.components() {
        if create_dirs {
            fs::create_dir_all(&path).map_err(|e| io_error(&path, e))?;
        }
        let meta = fs::symlink_metadata(&path).map_err(|e| io_error(&root.join(&relative), e))?;
        if !meta.is_dir() {
            return Err(Error::WrongType(path));
        }
        path.push(component);
    }
    Ok(path)
}

struct HashingWriter<W, F> {
    inner: W,
    hash: Sha256,
    size: u64,
    progress: F,
}

impl<W: Write, F: FnMut(u64)> HashingWriter<W, F> {
    fn new(inner: W, progress: F) -> Self {
        Self {
            inner,
            hash: Sha256::new(),
            size: 0,
            progress,
        }
    }

    fn verify(self, file: &PackFile, path: &Path) -> Result<(), Error> {
        check_size(file, path, self.size)?;
        if !file
            .sha256
            .eq_ignore_ascii_case(&to_hex(&self.hash.finalize()))
        {
            return Err(Error::Hash(path.into()));
        }
        Ok(())
    }
}

impl<W: Write, F: FnMut(u64)> Write for HashingWriter<W, F> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        self.size += n as u64;
        (self.progress)(self.size);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn check_size(file: &PackFile, path: &Path, actual: u64) -> Result<(), Error> {
    if actual != file.size {
        return Err(Error::Size {
            file: path.into(),
            expected: file.size,
            actual,
        });
    }
    Ok(())
}

fn verify_file(file: &PackFile, path: &Path) -> Result<(), Error> {
    let meta = fs::symlink_metadata(path).map_err(|e| io_error(path, e))?;
    if !meta.is_file() {
        return Err(Error::WrongType(path.into()));
    }
    check_size(file, path, meta.len())?;
    let mut input = File::open(path).map_err(|e| io_error(path, e))?;
    let mut digest = HashingWriter::new(io::sink(), |_| {});
    io::copy(&mut input, &mut digest).map_err(|e| io_error(path, e))?;
    digest.verify(file, path)
}

/// Verify every listed file, reporting the first missing or damaged file.
pub fn verify(manifest: &Manifest, dir: &Path) -> Result<(), Error> {
    manifest.validate()?;
    for file in &manifest.files {
        verify_file(file, &pack_path(dir, file, false)?)?;
    }
    Ok(())
}
