use crate::{io_error, remove_file_if_present, verify, with_suffix, Error, Manifest, StagingDir};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn write_provenance(manifest: &Manifest, staging: &Path) -> Result<(), Error> {
    let path = staging.join("PROVENANCE.txt");
    remove_file_if_present(&path)?;
    let write = || -> io::Result<()> {
        let mut out = File::create_new(&path)?;
        writeln!(out, "Pack: {}", manifest.id)?;
        writeln!(out, "Chip: {}", manifest.chip)?;
        writeln!(
            out,
            "License: {} (SHA-256 {})",
            manifest.license, manifest.license_sha256
        )?;
        writeln!(out, "Import map: {}", manifest.import_map_version)?;
        for repo in &manifest.repositories {
            writeln!(
                out,
                "Repository: {} {} {}",
                repo.name, repo.commit, repo.url_base
            )?;
        }
        for file in &manifest.files {
            writeln!(
                out,
                "File: {}/{} {} bytes SHA-256 {}",
                file.repository, file.path, file.size, file.sha256
            )?;
        }
        out.flush()
    };
    write().map_err(|e| io_error(&path, e))
}

fn remove_backup(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path).map_err(|e| io_error(path, e)),
        Ok(_) => remove_file_if_present(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(path, e)),
    }
}

/// Return a verified destination, or publish staging by directory rename.
/// Staging and destination must be separate trees on the same filesystem.
/// Reusing an existing installation leaves the caller's staging directory alone.
pub fn install(manifest: &Manifest, staging: &Path, dest: &Path) -> Result<PathBuf, Error> {
    // Two Fab workers may finish separate downloads for the same destination.
    // Serialize the verify / back up / rename sequence.
    static INSTALL: Mutex<()> = Mutex::new(());
    let _guard = INSTALL.lock().unwrap_or_else(|e| e.into_inner());
    manifest.validate()?;
    let exists = match fs::symlink_metadata(dest) {
        Ok(meta) => {
            if meta.is_dir() && verify(manifest, dest).is_ok() {
                return Ok(dest.into());
            }
            true
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(io_error(dest, e)),
    };
    let worker = StagingDir::claim(staging)?;
    verify(manifest, staging)?;
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let invalid_paths = || {
        io_error(
            dest,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "staging, destination and backup must be separate directories",
            ),
        )
    };
    let name = dest.file_name().ok_or_else(invalid_paths)?;
    let staging = &worker.0;
    fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    let target = fs::canonicalize(parent)
        .map_err(|e| io_error(parent, e))?
        .join(name);
    let backup = with_suffix(&target, ".bad");
    if target.starts_with(staging)
        || staging.starts_with(&target)
        || (exists && (backup.starts_with(staging) || staging.starts_with(&backup)))
    {
        return Err(invalid_paths());
    }
    write_provenance(manifest, staging)?;
    if exists {
        remove_backup(&backup)?;
        fs::rename(&target, &backup).map_err(|e| io_error(dest, e))?;
    }
    if let Err(e) = fs::rename(staging, &target) {
        if exists {
            fs::rename(&backup, &target).map_err(|restore| io_error(&backup, restore))?;
        }
        return Err(io_error(dest, e));
    }
    Ok(dest.into())
}
