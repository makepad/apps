//! A read-only inventory of working journals. Run only on the reader worker.
use super::{invalid, live, missing, Index, Kind};
use fab::ui::journal_panel::{RemovedRevision, RemovedSession};
use std::{fs, io, path::Path, sync::Arc};

#[derive(Clone)]
pub(crate) struct Session {
    pub folder: Arc<str>,
    /// An empty journal directory has no log identity to confirm yet.
    pub token: Option<u64>,
    pub start_ms: Option<u64>,
    pub records: usize,
    pub lost: bool,
}

// WorkingId prints the clock, process id and serial with lower-case hex formatting.
pub(in crate::journal) fn minted(folder: &str) -> Option<u128> {
    working_id(folder).map(|(nanos, _)| nanos)
}

pub(in crate::journal) fn working_id(folder: &str) -> Option<(u128, u32)> {
    let mut parts = folder.strip_prefix("working-")?.split('-');
    let mut hex = || {
        let s = parts.next()?;
        (!s.is_empty() && !(s.len() > 1 && s.starts_with('0'))
            && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then_some(s)
    };
    let nanos = u128::from_str_radix(hex()?, 16).ok()?;
    let pid = u32::from_str_radix(hex()?, 16).ok()?;
    u64::from_str_radix(hex()?, 16).ok()?;
    parts.next().is_none().then_some((nanos, pid))
}

pub(in crate::journal) fn candidate(root: &Path, folder: &str) -> io::Result<bool> {
    if minted(folder).is_none() { return Ok(false) }
    let plate = root.join(folder);
    let journal = plate.join("journal");
    for dir in [root, plate.as_path(), journal.as_path()] {
        if !missing(fs::symlink_metadata(dir))?.is_some_and(|m| m.is_dir()) { return Ok(false) }
    }
    Ok(missing(fs::symlink_metadata(plate.join("plate")))?.is_none())
}

pub(super) fn eligible(root: &Path, folder: &str) -> io::Result<bool> {
    Ok(candidate(root, folder)? && live::writer_progress(&root.join(folder).join("journal")).is_none())
}

pub(in crate::journal) fn list(root: &Path, own: &str, current: &impl Fn() -> bool) -> io::Result<Vec<Session>> {
    let Some(meta) = missing(fs::symlink_metadata(root))? else { return Ok(Vec::new()) };
    if !meta.is_dir() { return Err(invalid()) }
    let mut sessions = Vec::new();
    for child in fs::read_dir(root)? {
        if !current() { return Err(io::ErrorKind::Interrupted.into()) }
        let child = child?;
        let name = child.file_name();
        let Some(folder) = name.to_str() else { continue };
        if folder == fab_ai::plates::REMOVED_SESSIONS || folder == own || !eligible(root, folder).unwrap_or(false) { continue }
        let mut index = Index::default();
        if index.refresh(root, folder, current).is_err() { continue }
        let start_ms = index.calls.values().filter_map(|(_, time)| *time).min()
            .or_else(|| u64::try_from(minted(folder)? / 1_000_000).ok());
        sessions.push(Session { folder: Arc::from(folder), token: index.token(), start_ms, records: index.entries.len(),
            lost: index.entries.values().any(|e| e.kind == Kind::Lost) });
    }
    sessions.sort_unstable_by(|a, b| b.start_ms.cmp(&a.start_ms).then_with(|| a.folder.cmp(&b.folder)));
    Ok(sessions)
}

pub(in crate::journal) fn real_directory(path: &Path) -> io::Result<bool> {
    Ok(missing(fs::symlink_metadata(path))?.is_some_and(|m| m.is_dir()))
}

pub(in crate::journal) fn removed_revision(dir: &Path) -> io::Result<RemovedRevision> {
    if !real_directory(dir)? { return Err(invalid()) }
    let incarnation = super::read_incarnation(dir)?.map(|id| id.0);
    let length = missing(fs::symlink_metadata(dir.join(super::LOG)))?.map(|m| {
        if m.is_file() { Ok(m.len()) } else { Err(invalid()) }
    }).transpose()?;
    Ok(RemovedRevision { incarnation, length })
}

pub(crate) fn list_removed(root: &Path) -> io::Result<Arc<[RemovedSession]>> {
    let archive = root.join(fab_ai::plates::REMOVED_SESSIONS);
    if !real_directory(root)? || !real_directory(&archive)? { return Ok(Arc::default()) }
    let mut removed = Vec::new();
    for child in fs::read_dir(archive)? {
        let child = child?;
        let name = child.file_name();
        if name == crate::journal::remove::EMPTY_LOCK || !child.file_type()?.is_dir() { continue }
        removed.push(RemovedSession { name, revision: removed_revision(&child.path()).ok() });
    }
    removed.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(removed.into())
}
