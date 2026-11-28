use crate::table::Tree;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn local(path: &str) -> Result<(), String> {
    if path.contains([':', '\\']) || path.bytes().any(|b| b.is_ascii_control())
        || path.split('/').any(|s| matches!(s, "" | "." | "..") || s.eq_ignore_ascii_case(".git"))
    { return Err(format!("invalid relative file path {path:?}")); }
    Ok(())
}

pub(crate) fn error(path: &Path, e: io::Error) -> String { format!("{}: {e}", path.display()) }

pub(crate) fn directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| error(path, e))
}

pub(crate) struct Scratch(pub PathBuf);

static NEXT: AtomicUsize = AtomicUsize::new(0);
impl Scratch {
    pub fn new(parent: &Path, label: &str) -> Result<Self, String> {
        directory(parent)?;
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".{label}-{}-{n}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(error(&path, e)),
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

pub(crate) fn read_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => fs::read(path).map(Some).map_err(|e| error(path, e)),
        Ok(_) => Err(format!("expected regular file: {}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(error(path, e)),
    }
}

// Check each directory crossed, including those above a missing leaf.
pub(crate) fn checked(root: &Path, rel: &str) -> Result<PathBuf, String> {
    local(rel)?;
    let mut path = root.to_owned();
    for part in rel.split('/') {
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => (),
            Ok(_) => return Err(format!("expected directory: {}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(error(&path, e)),
        }
        path.push(part);
    }
    Ok(path)
}

pub(crate) fn collect(root: &Path, rel: &str, files: &mut Tree) -> Result<(), String> {
    let path = checked(root, rel)?;
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(error(&path, e)),
        Ok(m) if m.is_dir() => {
            for entry in fs::read_dir(&path).map_err(|e| error(&path, e))? {
                let name = entry.map_err(|e| error(&path, e))?.file_name().into_string()
                    .map_err(|_| format!("non-UTF-8 path below {rel}"))?;
                collect(root, &format!("{rel}/{name}"), files)?;
            }
            Ok(())
        }
        Ok(m) if m.is_file() => {
            files.insert(rel.into(), fs::read(&path).map_err(|e| error(&path, e))?);
            Ok(())
        }
        Ok(_) => Err(format!("unexpected symlink or special file: {rel}")),
    }
}

pub(crate) fn write(root: &Path, rel: &str, bytes: &[u8]) -> Result<(), String> {
    let path = checked(root, rel)?;
    directory(path.parent().unwrap())?;
    let mut file = fs::File::create_new(&path).map_err(|e| error(&path, e))?;
    file.write_all(bytes).map_err(|e| error(&path, e))
}
