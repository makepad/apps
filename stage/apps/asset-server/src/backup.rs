//! A copy of the store before its catalog is upgraded.
//!
//! Opening a store migrates its catalog to the current schema in place, and
//! there is no way back down. So before a host opens a catalog whose schema
//! is older than this build's, the whole store directory is copied beside it
//! as `<store>.backup-v<old>-<unix seconds>`, once per old version: a later
//! start that finds such a backup (the upgrade already happened, or it was
//! interrupted and is being finished) does not copy again.
//!
//! The copy is made under the store's own `server.lock`, so no server opens
//! the store halfway through; when another process holds the lock, that
//! process owns the store and the upgrade, and nothing is copied here. The
//! copy is written as `<name>.partial` and renamed when complete, so an
//! interrupted copy never counts as the backup. The CAS objects are
//! immutable and named by their digest, so the backup HARDLINKS them (no
//! bytes copied, on any filesystem; a later GC unlink of the live name
//! leaves the backup's); only the databases and other files are copied.

use makepad_asset_store::SERVER_SCHEMA_VERSION;
use std::path::{Path, PathBuf};

const CATALOG: &str = "catalog.sqlite3";
const LOCK: &str = "server.lock";

/// The catalog's schema version as its file header records it
/// (`PRAGMA user_version`, bytes 60..64), or `None` when there is no
/// catalog. A version the header holds may lag one still in the WAL; that
/// can only cause one spare backup, never a missing one.
pub(crate) fn catalog_version(root: &Path) -> Option<i64> {
    use std::io::Read;
    let mut header = [0u8; 64];
    let mut file = std::fs::File::open(root.join(CATALOG)).ok()?;
    file.read_exact(&mut header).ok()?;
    if &header[..16] != b"SQLite format 3\0" {
        return None;
    }
    Some(i32::from_be_bytes([header[60], header[61], header[62], header[63]]) as i64)
}

/// Copy `root` aside when its catalog is older than this build's, before
/// anything opens it. `Ok(Some(path))` names a backup made now; `Ok(None)`
/// means none was needed (no catalog, current schema, a backup of that
/// version exists, or another process holds the store). An `Err` means the
/// copy failed and the store must not be opened: upgrading it without a
/// copy is what this exists to prevent.
pub fn backup_before_upgrade(root: &Path, log: bool) -> Result<Option<PathBuf>, String> {
    let Some(old) = catalog_version(root) else {
        return Ok(None);
    };
    // 0 is an empty catalog; newer versions are refused by the store itself.
    if old <= 0 || old >= SERVER_SCHEMA_VERSION {
        return Ok(None);
    }
    let (Some(parent), Some(name)) = (root.parent(), root.file_name().and_then(|n| n.to_str())) else {
        return Err(format!("store root {} has no parent to back up into", root.display()));
    };
    let prefix = format!("{name}.backup-v{old}-");

    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(LOCK))
        .map_err(|e| format!("store lock: {e}"))?;
    if lock.try_lock().is_err() {
        // The holder serves this store and has already opened it.
        return Ok(None);
    }

    let mut stale_partials = Vec::new();
    for entry in std::fs::read_dir(parent).map_err(|e| format!("read {}: {e}", parent.display()))? {
        let Ok(entry) = entry else { continue };
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else { continue };
        if !file_name.starts_with(&prefix) {
            continue;
        }
        if file_name.ends_with(".partial") {
            stale_partials.push(entry.path());
        } else {
            return Ok(None);
        }
    }
    for partial in stale_partials {
        // Always `<parent>/<name>.backup-v<old>-<secs>.partial`, a name only
        // this function writes.
        let _ = std::fs::remove_dir_all(&partial);
    }

    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = parent.join(format!("{prefix}{secs}"));
    let partial = parent.join(format!("{prefix}{secs}.partial"));
    if log {
        eprintln!(
            "[asset-host] catalog schema v{old} is older than v{SERVER_SCHEMA_VERSION}: copying {} to {} before upgrading",
            root.display(),
            backup.display()
        );
    }
    copy_dir(root, &partial, false).map_err(|e| format!("backing up the store before its upgrade: {e}"))?;
    std::fs::rename(&partial, &backup).map_err(|e| format!("finishing the store backup: {e}"))?;
    drop(lock);
    if log {
        eprintln!("[asset-host] store backup written: {}", backup.display());
    }
    Ok(Some(backup))
}

/// Copy a directory tree; the lock file stays behind (it guards the store,
/// not the copy). Below `<root>/cas` files are hardlinked (`link`): CAS
/// objects never change in place. A filesystem without hardlinks copies.
fn copy_dir(from: &Path, to: &Path, link: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&source, &target, link || entry.file_name() == "cas")?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&source)?, &target)?;
        } else if entry.file_name() != LOCK {
            if !(link && std::fs::hard_link(&source, &target).is_ok()) {
                std::fs::copy(&source, &target)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_base(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "mp_asset_backup_{}_{}_{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    /// A catalog file with just enough header to carry `version`.
    fn fake_store(root: &Path, version: i32) {
        std::fs::create_dir_all(root.join("cas/ab")).unwrap();
        let mut header = vec![0u8; 100];
        header[..16].copy_from_slice(b"SQLite format 3\0");
        header[60..64].copy_from_slice(&version.to_be_bytes());
        std::fs::write(root.join(CATALOG), header).unwrap();
        std::fs::write(root.join("cas/ab/blob"), b"bytes").unwrap();
        std::fs::write(root.join("server-id"), "00112233445566778899aabbccddeeff\n").unwrap();
    }

    fn backups(base: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(base)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.contains(".backup-"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn an_older_catalog_is_copied_once_before_its_upgrade() {
        let base = temp_base("older");
        let root = base.join("store");
        fake_store(&root, 15);
        assert_eq!(catalog_version(&root), Some(15));

        let made = backup_before_upgrade(&root, false).unwrap().expect("a v15 store is backed up");
        assert!(made.file_name().unwrap().to_str().unwrap().starts_with("store.backup-v15-"));
        assert_eq!(std::fs::read(made.join("cas/ab/blob")).unwrap(), b"bytes");
        // CAS objects are linked, not copied; the catalog is a real copy.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(made.join("cas/ab/blob")).unwrap().ino(), std::fs::metadata(root.join("cas/ab/blob")).unwrap().ino());
            assert_ne!(std::fs::metadata(made.join(CATALOG)).unwrap().ino(), std::fs::metadata(root.join(CATALOG)).unwrap().ino());
        }
        assert!(made.join(CATALOG).is_file());
        assert!(!made.join(LOCK).exists(), "the lock stays with the store");

        // Once per old version: the next start (upgrade interrupted, or the
        // header still lagging the WAL) does not copy again.
        assert_eq!(backup_before_upgrade(&root, false).unwrap(), None);
        assert_eq!(backups(&base).len(), 1);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn current_new_and_missing_catalogs_are_left_alone() {
        let base = temp_base("current");
        let root = base.join("store");
        assert_eq!(backup_before_upgrade(&root, false).unwrap(), None, "no store yet");
        fake_store(&root, SERVER_SCHEMA_VERSION as i32);
        assert_eq!(backup_before_upgrade(&root, false).unwrap(), None);
        fake_store(&root, 0);
        assert_eq!(backup_before_upgrade(&root, false).unwrap(), None, "an empty catalog");
        assert!(backups(&base).is_empty());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_held_store_is_not_copied_and_a_stale_partial_does_not_count() {
        let base = temp_base("held");
        let root = base.join("store");
        fake_store(&root, 14);
        let partial = base.join("store.backup-v14-1.partial");
        std::fs::create_dir_all(&partial).unwrap();

        let holder = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(root.join(LOCK)).unwrap();
        holder.try_lock().unwrap();
        assert_eq!(backup_before_upgrade(&root, false).unwrap(), None, "the holder owns the upgrade");
        drop(holder);

        let made = backup_before_upgrade(&root, false).unwrap().expect("an interrupted copy is not the backup");
        assert!(!partial.exists(), "the stale partial is cleared");
        assert_eq!(backups(&base), vec![made.file_name().unwrap().to_str().unwrap().to_string()]);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
