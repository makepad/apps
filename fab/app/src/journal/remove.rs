//! A confirmed Remove runs on a worker. Recovery and the archive rename commit independently.
use super::*;
use super::reader::sessions::{real_directory, removed_revision};
use fab::ui::journal_panel::RemovedSession;
use fab_ai::plates::{PlateStore, REMOVED_SESSIONS};
use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Removed { Moved, DurabilityUnconfirmed }

impl Removed {
    pub(crate) fn warning(&self) -> Option<&'static str> {
        match self {
            Self::Moved => None,
            Self::DurabilityUnconfirmed => Some("The session was moved, but Fab could not confirm the move was saved to disk."),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal { Ineligible, Legacy(PathBuf), Open, CannotLock, Unlocked, Changed, Arrived, CannotMove }

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ineligible => "This session can't be removed.",
            Self::Legacy(path) => return write!(f, "This session was recorded by an earlier Fab, so Fab can't tell whether it is still open. It is kept. To remove it, delete {} yourself.", path.display()),
            Self::Open => "This session is open in Fab right now.",
            Self::CannotLock => "Fab can't lock files in this folder, so it can't tell whether this session is still open. It is kept.",
            Self::Unlocked => "This session was recorded without a file lock, so Fab can't tell whether it is still open. It is kept; you can browse and copy it.",
            Self::Changed => "This session changed. Look again.",
            Self::Arrived => "Records for this session arrived just now. Try again.",
            Self::CannotMove => "Fab couldn't move this session into the removed sessions folder. It is kept.",
        })
    }
}

fn eligible(root: &Path, folder: &str) -> Result<(), Refusal> {
    if reader::sessions::candidate(root, folder).unwrap_or(false) { Ok(()) } else { Err(Refusal::Ineligible) }
}

fn lock(path: &Path, plate: &Path) -> Result<File, Refusal> {
    let file = open_regular_with(path, OpenOptions::new().read(true).write(true)).map_err(|_| Refusal::CannotLock)?.ok_or_else(|| Refusal::Legacy(plate.to_owned()))?;
    exclusive(file, path).map_err(|e| if e.kind() == io::ErrorKind::WouldBlock { Refusal::Open } else { Refusal::CannotLock })
}

fn exclusive(file: File, path: &Path) -> io::Result<File> {
    file.try_lock()?;
    if same_file(&file, path)? != Some(true) { return Err(invalid()) }
    Ok(file)
}

fn destination(archive: &Path, folder: &str) -> io::Result<PathBuf> {
    create_directory(archive)?;
    let mut name = archive.join(folder);
    let mut suffix = 2u64;
    loop {
        match fs::symlink_metadata(&name) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(name),
            Err(e) => return Err(e),
            Ok(_) => {
                name = archive.join(format!("{folder} {suffix}"));
                suffix += 1;
            }
        }
    }
}

pub(crate) fn remove_session(store: &PlateStore, folder: &str, confirmed_token: u64,
    log: &mut impl FnMut(String)) -> Result<Removed, Refusal> {
    eligible(&store.root, folder)?;
    let plate = store.dir(folder);
    let dir = plate.join("journal");
    let settings = store.root.parent();
    if loss::has_note(&plate, settings, None).map_err(|_| Refusal::Ineligible)? {
        // The recovery writer may create a missing sidecar. Never let a linked sidecar
        // redirect that create outside the confirmed folder.
        for path in [plate.join(LIFETIME), dir.join(LOCK)] {
            match fs::symlink_metadata(path) {
                Ok(meta) if meta.is_file() => (),
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                _ => return Err(Refusal::CannotLock),
            }
        }
        // Discovery's recovery writer, with no producers and no permission to recreate the plate.
        let (_, mut writer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
        writer.settings = settings.map(Path::to_owned);
        writer.create_plate = false;
        writer.recover_notes(log).map_err(|e| {
            if e.kind() == io::ErrorKind::WouldBlock { Refusal::Open } else { Refusal::Ineligible }
        })?;
    }
    #[cfg(test)]
    tests::probe("remove recovered").map_err(|_| Refusal::Ineligible)?;

    let _life = lock(&plate.join(LIFETIME), &plate)?;
    let _row = lock(&dir.join(LOCK), &plate)?;
    eligible(&store.root, folder)?;
    match fs::symlink_metadata(plate.join(UNLOCKED)) {
        Ok(_) => return Err(Refusal::Unlocked),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(_) => return Err(Refusal::Ineligible),
    }
    let (token, incarnation) = reader::confirmed_binding(&store.root, folder).map_err(|_| Refusal::Changed)?;
    if confirmed_token != token { return Err(Refusal::Changed) }
    if let Some(current) = incarnation {
        if loss::has_note(&plate, settings, Some(current)).map_err(|_| Refusal::Ineligible)? {
            return Err(Refusal::Arrived)
        }
    }
    #[cfg(test)]
    tests::probe("remove checked").map_err(|_| Refusal::CannotMove)?;

    let archive = store.root.join(REMOVED_SESSIONS);
    let target = destination(&archive, folder).map_err(|_| Refusal::CannotMove)?;
    // Windows refuses the directory rename while a child handle is open. The lifetime lock stays.
    #[cfg(windows)]
    drop(_row);
    fs::rename(&dir, target).map_err(|e| {
        log(format!("journal: {}: {:?}", dir.display(), e.kind()));
        #[cfg(windows)]
        if matches!(e.raw_os_error(), Some(5 | 32 | 33)) { return Refusal::Open }
        Refusal::CannotMove
    })?;

    // The rename committed. A sync failure must never invite the person to retry the move.
    let mut outcome = Removed::Moved;
    for parent in [&plate, &archive] {
        let sync = || {
            #[cfg(test)]
            tests::probe("remove parent sync")?;
            sync_parent(parent)
        };
        if let Err(e) = sync() {
            log(format!("journal: {}: {:?}", parent.display(), e.kind()));
            outcome = Removed::DurabilityUnconfirmed;
        }
    }
    Ok(outcome)
}

pub(super) const EMPTY_LOCK: &str = ".empty.lock";

fn empty_lock(archive: &Path) -> io::Result<File> {
    let path = archive.join(EMPTY_LOCK);
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    let file = match options.create_new(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            open_regular_with(&path, options.create_new(false))?.ok_or(io::ErrorKind::NotFound)?
        }
        Err(e) => return Err(e),
    };
    exclusive(file, &path)
}

#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct Emptied {
    pub deleted: usize,
    pub kept: usize,
}

impl Emptied {
    pub fn message(&self) -> String {
        match self.kept {
            0 => String::new(),
            1 => "1 removed session changed since you looked. It is kept.".into(),
            n => format!("{n} removed sessions changed since you looked. They are kept."),
        }
    }
}

fn delete_confirmed(archive: &Path, entry: &RemovedSession) -> io::Result<()> {
    let confirmed = entry.revision.as_ref().ok_or_else(invalid)?;
    if Path::new(&entry.name).file_name() != Some(entry.name.as_os_str()) || entry.name == EMPTY_LOCK { return Err(invalid()) }
    let dir = archive.join(&entry.name);
    if !real_directory(&dir)? { return Err(invalid()) }
    let path = dir.join(LOCK);
    let file = open_regular_with(&path, OpenOptions::new().read(true).write(true))?.ok_or(io::ErrorKind::NotFound)?;
    let _row = exclusive(file, &path)?;
    if removed_revision(&dir)? != *confirmed { return Err(invalid()) }
    #[cfg(windows)]
    drop(_row);
    #[cfg(test)]
    tests::probe("empty verified")?;
    fs::remove_dir_all(dir)
}

pub(crate) fn empty_removed(store: &PlateStore, confirmed: &[RemovedSession], log: &mut impl FnMut(String)) -> Emptied {
    if confirmed.is_empty() { return Emptied::default() }
    let archive = store.root.join(REMOVED_SESSIONS);
    let acquire = || {
        if !real_directory(&store.root)? || !real_directory(&archive)? { return Err(invalid()) }
        empty_lock(&archive)
    };
    // Serialize Empties until their deletes return. On Windows a peer could
    // otherwise empty A after our row unlock, and Remove could reuse A's name.
    let _empty = match acquire() {
        Ok(lock) => lock,
        Err(e) => {
            log(format!("journal: {}: {:?}", archive.display(), e.kind()));
            return Emptied { deleted: 0, kept: confirmed.len() }
        }
    };
    let mut result = Emptied::default();
    for entry in confirmed {
        match delete_confirmed(&archive, entry) {
            Ok(()) => result.deleted += 1,
            Err(e) => {
                result.kept += 1;
                log(format!("journal: {}: {:?}", archive.display(), e.kind()));
            }
        }
    }
    result
}
