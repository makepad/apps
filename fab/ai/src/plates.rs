//! The plate store: every plate Fab has had, each with a working copy that is saved all the
//! time, so a switch, a quit or a crash never loses an edit.
//!
//! A plate is a project (`.fab.splash` with its scripts and meshes beside it). One opened from
//! disk is used in place. A new plate, a demo's copy or a duplicate has no file until it is
//! saved; Save then gives it a folder of its own in the plates home (`~/Documents/Fab`). The
//! plate's own file is written only by Save: edits go to its working copy, and a plate whose
//! working copy differs from its file is shown with a `*`.
//!
//! Each plate has a folder under the store's root (`~/.makepad/fab/plates/<id>`):
//!
//! * `plate`: its record (name, file, when it was last edited, its printer, whether the working
//!   copy differs from the file, where it came from), replaced atomically;
//! * the working copy, in the crash-copy format of `autosave` (immutable content files
//!   chosen by one atomically replaced map);
//! * `current.thumb.png`: the plate's last settled frame;
//! * `view`: the view the app left it in (camera, selection, panel), the app's own text.
//!
//! `current` names what is on the bed (a plate, or a demo shown as it ships), which the next
//! start opens. A demo is never a plate: its first edit makes a new plate of it.

use crate::autosave::{decode, encode};
use crate::AUTOSAVE_FILE;
use fab_model::project;
use std::path::{Path, PathBuf};

/// Reserved store directory for journals put away by the person.
pub const REMOVED_SESSIONS: &str = "removed-sessions";

/// Where a plate came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Opened, made new, imported or duplicated.
    Plate,
    /// An image-to-mesh result, kept the moment it arrived.
    Made,
    /// The copy of a demo plate (its key).
    Demo(String),
}

/// One plate's record.
#[derive(Clone, Debug, PartialEq)]
pub struct Plate {
    pub id: String,
    pub name: String,
    /// The plate's own file; none until it is first saved.
    pub file: Option<PathBuf>,
    /// Seconds since the epoch (UTC) its document last changed.
    pub edited: i64,
    /// The printer it was last edited for.
    pub printer: String,
    /// Its working copy differs from its file (a plate never saved differs once it has parts).
    pub modified: bool,
    /// The document's content hash as its file holds it (hex); empty when there is no file.
    pub saved: String,
    pub origin: Origin,
    /// Taken off the list; its files and working copy stay where they are.
    pub removed: bool,
}

impl Plate {
    fn text(&self) -> String {
        let origin = match &self.origin {
            Origin::Plate => "plate".to_string(),
            Origin::Made => "made".to_string(),
            Origin::Demo(key) => format!("demo:{key}"),
        };
        let file = self.file.as_ref().map(|f| f.display().to_string()).unwrap_or_default();
        format!(
            "name\t{}\nfile\t{}\nedited\t{}\nprinter\t{}\nmodified\t{}\nsaved\t{}\norigin\t{}\nremoved\t{}\n",
            encode(&self.name), encode(&file), self.edited, encode(&self.printer), self.modified as u8, self.saved, encode(&origin), self.removed as u8,
        )
    }

    fn parse(id: &str, text: &str) -> Option<Plate> {
        let mut plate = Plate { id: id.to_string(), name: String::new(), file: None, edited: 0, printer: String::new(), modified: false, saved: String::new(), origin: Origin::Plate, removed: false };
        for line in text.lines() {
            let (key, value) = line.split_once('\t')?;
            let value = decode(value)?;
            match key {
                "name" => plate.name = value,
                "file" => plate.file = (!value.is_empty()).then(|| PathBuf::from(value)),
                "edited" => plate.edited = value.parse().ok()?,
                "printer" => plate.printer = value,
                "modified" => plate.modified = value == "1",
                "saved" => plate.saved = value,
                "origin" => {
                    plate.origin = match value.as_str() {
                        "made" => Origin::Made,
                        v => v.strip_prefix("demo:").map_or(Origin::Plate, |k| Origin::Demo(k.to_string())),
                    }
                }
                "removed" => plate.removed = value == "1",
                _ => {}
            }
        }
        Some(plate)
    }

    /// The name the list and the title show: with a `*` while the working copy differs from
    /// the file.
    pub fn title(&self) -> String {
        if self.modified { format!("{} *", self.name) } else { self.name.clone() }
    }
}

/// What the bed showed when Fab last ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Current {
    Plate(String),
    /// A demo as it ships, by key (never edited, or it would be a plate).
    Demo(String),
}

/// The store: plates' records and working copies, the plates home and the demos' pictures.
#[derive(Clone, Debug)]
pub struct PlateStore {
    /// One folder per plate (`~/.makepad/fab/plates`).
    pub root: PathBuf,
    /// Where saved plates get their folders (`~/Documents/Fab`).
    pub home: PathBuf,
    /// The demos' pictures (`~/.makepad/fab/demos`).
    pub demos: PathBuf,
}

/// The content hash of a document head as the records keep it.
/// Beside a picture painted away from the view: the content it shows ([`content_text`]). A
/// picture the view takes has none.
pub fn painted_content(picture: &Path) -> PathBuf {
    let mut name = picture.as_os_str().to_owned();
    name.push(".of");
    PathBuf::from(name)
}

/// A picture file with something in it. Pictures are a cache, written without waiting on the disk
/// ([`project::write_unflushed`]): one a crash cut short is left empty, counts as none and is
/// painted again.
pub fn has_picture(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

pub fn content_text(content: &[u8; 32]) -> String {
    content.iter().map(|b| format!("{b:02x}")).collect()
}

fn modified_secs(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
}

fn fnv(text: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for &byte in text.as_bytes() {
        hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
    }
    hash
}

/// Copy the files of `from` into `to`, folders included, replacing none that are there.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", from.display()))?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(|e| format!("{}: {e}", src.display()))?;
        if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else if kind.is_file() && !dst.exists() {
            std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        }
    }
    Ok(())
}

/// The project files in `dir` (not below it).
fn projects_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(list) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = list.flatten().map(|e| e.path()).filter(|p| p.is_file() && project::is_project(p)).collect();
    out.sort();
    out
}

/// A folder name from a plate's name (no path separators).
fn folder_name(name: &str) -> String {
    let s = name.replace(['/', '\\', ':'], "-");
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "plate".into() } else { s }
}

/// A file stem from a plate's name.
pub fn file_stem(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "plate".into() } else { s }
}

/// What a migration wrote and has not flushed to the disk yet ([`PlateStore::migrate_unflushed`]).
pub struct Unflushed {
    done: PathBuf,
    files: Vec<PathBuf>,
}

impl Unflushed {
    /// Its files flushed, then the migration marked done: a crash before the mark runs it again.
    pub fn finish(self) -> Result<(), String> {
        for file in &self.files {
            project::flush(file)?;
        }
        project::write_atomic(&self.done, b"")
    }
}

/// What `current` holds for the plate on the bed.
fn current_text(current: Option<&Current>) -> String {
    match current {
        Some(Current::Plate(id)) => id.clone(),
        Some(Current::Demo(key)) => format!("demo:{key}"),
        None => String::new(),
    }
}

impl PlateStore {
    /// The store of a Fab folder (`~/.makepad/fab`) with its plates home.
    pub fn under(fab: &Path, home: &Path) -> PlateStore {
        PlateStore { root: fab.join("plates"), home: home.to_path_buf(), demos: fab.join("demos") }
    }

    /// The plate's own folder: its record, working copy, picture and view.
    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// The working copy's picture.
    pub fn picture(&self, id: &str) -> PathBuf {
        project::thumbnail_path(&self.dir(id).join(AUTOSAVE_FILE))
    }

    /// A plate's picture: its working copy's, else the one saved beside its file.
    pub fn picture_of(&self, plate: &Plate) -> Option<PathBuf> {
        Some(self.picture(&plate.id)).filter(|p| has_picture(p))
            .or_else(|| plate.file.as_ref().map(|f| project::thumbnail_path(f)).filter(|p| has_picture(p)))
    }

    /// A plate has a working copy once it was edited (or came from the old crash copy).
    pub fn has_working_copy(&self, id: &str) -> bool {
        self.dir(id).join("scripts").join("map").is_file()
    }

    /// A plate's working copy as a model, none when it has none (or it does not read back whole).
    pub fn working_model(&self, id: &str) -> Option<fab_model::FabModel> {
        crate::autosave::Snapshot::recover(&self.dir(id)).ok().flatten().map(|r| r.model)
    }

    /// What a plate's folder keeps aside, that no autosave prunes or writes over: a working copy
    /// that did not read back, or a file changed outside Fab whose name the autosave needed. Each
    /// folder with why, the oldest first.
    pub fn kept_aside(&self, id: &str) -> Vec<(PathBuf, String)> {
        crate::autosave::kept_aside(&self.dir(id))
    }

    /// A demo's picture.
    pub fn demo_picture(&self, key: &str) -> PathBuf {
        self.demos.join(format!("{key}.thumb.png"))
    }

    /// The view state the app left a plate in.
    pub fn view_file(&self, id: &str) -> PathBuf {
        self.dir(id).join("view")
    }

    pub fn read(&self, id: &str) -> Option<Plate> {
        let text = std::fs::read_to_string(self.dir(id).join("plate")).ok()?;
        Plate::parse(id, &text)
    }

    pub fn write(&self, plate: &Plate) -> Result<(), String> {
        project::write_atomic(&self.dir(&plate.id).join("plate"), plate.text().as_bytes())
    }

    /// Every record, removed ones included, in no order.
    pub fn all(&self) -> Vec<Plate> {
        let Ok(list) = std::fs::read_dir(&self.root) else { return Vec::new() };
        list.flatten().filter_map(|e| {
            let id = e.file_name().into_string().ok()?;
            (id != REMOVED_SESSIONS && e.path().is_dir()).then(|| self.read(&id)).flatten()
        }).collect()
    }

    /// The plates on the list, the most recently edited first.
    pub fn list(&self) -> Vec<Plate> {
        let mut out: Vec<Plate> = self.all().into_iter().filter(|p| !p.removed).collect();
        out.sort_by(|a, b| b.edited.cmp(&a.edited).then_with(|| b.id.cmp(&a.id)));
        out
    }

    /// The plate whose file is `file` (removed or not).
    pub fn find_file(&self, file: &Path) -> Option<Plate> {
        let want = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
        self.all().into_iter().find(|p| p.file.as_ref().is_some_and(|f| f.canonicalize().unwrap_or_else(|_| f.clone()) == want))
    }

    fn fresh_id(&self) -> String {
        let secs = makepad_civil_time::now_secs();
        let mut n = 0;
        loop {
            let id = if n == 0 { format!("{secs}") } else { format!("{secs}-{n}") };
            if !self.dir(&id).exists() {
                return id;
            }
            n += 1;
        }
    }

    /// A new plate's record, on the list now.
    pub fn create(&self, name: &str, file: Option<PathBuf>, saved: String, origin: Origin) -> Result<Plate, String> {
        let id = self.fresh_id();
        std::fs::create_dir_all(self.dir(&id)).map_err(|e| format!("{}: {e}", self.dir(&id).display()))?;
        let modified = file.is_none() && !saved.is_empty();
        let plate = Plate { id, name: name.to_string(), file, edited: makepad_civil_time::now_secs(), printer: String::new(), modified, saved, origin, removed: false };
        self.write(&plate)?;
        Ok(plate)
    }

    /// A plate's name from its file: the folder's for a plate in a folder of its own in the
    /// plates home (the folder Save makes, named after the plate), else the file's.
    pub fn name_of(&self, file: &Path) -> String {
        match file.parent() {
            Some(dir) if dir.parent() == Some(self.home.as_path()) && projects_in(dir).len() == 1 => {
                dir.file_name().and_then(|n| n.to_str()).map(str::to_string).unwrap_or_else(|| crate::project_name(file))
            }
            _ => crate::project_name(file),
        }
    }

    /// What was on the bed when Fab last ran.
    pub fn current(&self) -> Option<Current> {
        let text = std::fs::read_to_string(self.root.join("current")).ok()?;
        let text = text.trim();
        match text.strip_prefix("demo:") {
            Some(key) => Some(Current::Demo(key.to_string())),
            None => (!text.is_empty() && self.read(text).is_some()).then(|| Current::Plate(text.to_string())),
        }
    }

    pub fn set_current(&self, current: Option<&Current>) -> Result<(), String> {
        project::write_atomic(&self.root.join("current"), current_text(current).as_bytes())
    }

    /// A new folder in the plates home for a plate called `name`, and its project file there.
    pub fn new_file(&self, name: &str) -> PathBuf {
        let base = folder_name(name);
        let mut dir = self.home.join(&base);
        let mut n = 1;
        while dir.exists() {
            n += 1;
            dir = self.home.join(format!("{base} {n}"));
        }
        dir.join(format!("{}.{}", file_stem(name), project::EXTENSION))
    }

    /// Fold what earlier versions kept into plates, once: the working document's crash copy
    /// (`autosave`), the recent list, the plates kept when replaced (`previous`), the
    /// image-to-mesh results (`made`) and the plates in the plates home. Nothing is moved or
    /// deleted: a file is used in place, and the crash copy is copied into its plate's working
    /// copy. `load` reads a crash copy: its document's content hash, name and file. A migration
    /// a crash cut short runs again from the start (each source has the same id each time, and a
    /// record that reads back stays as it is) and is done once `migrated` is written. Returns the
    /// plates made.
    pub fn migrate(&self, fab: &Path, load: &dyn Fn(&Path) -> Option<([u8; 32], String, Option<PathBuf>)>, load_file: &dyn Fn(&Path) -> Option<[u8; 32]>) -> Result<usize, String> {
        match self.migrate_unflushed(fab, load, load_file)? {
            Some((made, unflushed)) => unflushed.finish().map(|()| made),
            None => Ok(0),
        }
    }

    /// [`Self::migrate`] with what it writes left on the disk's cache: it reads back at once, so
    /// the plate list shows without waiting on a slow disk's flushes, and [`Unflushed::finish`]
    /// flushes it and then says the migration is done. None: it was done before.
    pub fn migrate_unflushed(&self, fab: &Path, load: &dyn Fn(&Path) -> Option<([u8; 32], String, Option<PathBuf>)>, load_file: &dyn Fn(&Path) -> Option<[u8; 32]>) -> Result<Option<(usize, Unflushed)>, String> {
        let done = self.root.join("migrated");
        if done.exists() {
            return Ok(None);
        }
        let mut written = Vec::new();
        std::fs::create_dir_all(&self.root).map_err(|e| format!("{}: {e}", self.root.display()))?;
        let mut made = 0;
        let mut files: Vec<PathBuf> = Vec::new();
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let mut current = None;
        // The working document: its crash copy becomes its plate's working copy.
        let autosave = fab.join("autosave");
        if let Some((content, name, file)) = load(&autosave) {
            let id = "m-autosave".to_string();
            copy_tree(&autosave, &self.dir(&id))?;
            let file = file.filter(|f| f.is_file());
            let saved = file.as_ref().and_then(|f| load_file(f)).map(|c| content_text(&c)).unwrap_or_default();
            let modified = saved != content_text(&content);
            let edited = modified_secs(&autosave.join("scripts").join("map")).unwrap_or_else(makepad_civil_time::now_secs);
            let name = file.as_ref().map(|f| self.name_of(f)).unwrap_or(name);
            if let Some(f) = &file {
                files.push(canon(f));
            }
            self.migrated(&Plate { id: id.clone(), name, file, edited, printer: String::new(), modified, saved, origin: Origin::Plate, removed: false }, &mut written)?;
            current = Some(Current::Plate(id));
            made += 1;
        }
        // Every plate file the earlier lists and folders knew, used in place.
        let mut found: Vec<(PathBuf, Origin, Option<i64>)> = Vec::new();
        let recent = std::fs::read_to_string(fab.join("recent")).unwrap_or_default();
        found.extend(recent.lines().map(PathBuf::from).filter(|p| p.is_file()).map(|p| (p, Origin::Plate, None)));
        for (folder, origin) in [(fab.join("previous"), Origin::Plate), (fab.join("made"), Origin::Made)] {
            let Ok(list) = std::fs::read_dir(&folder) else { continue };
            let mut dirs: Vec<PathBuf> = list.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            dirs.sort();
            for dir in dirs {
                // `<epoch>-<name>`: kept at that time
                let secs = dir.file_name().and_then(|n| n.to_str()).and_then(|n| n.split('-').next()).and_then(|s| s.parse::<i64>().ok());
                found.extend(projects_in(&dir).into_iter().map(|p| (p, origin.clone(), secs)));
            }
        }
        if let Ok(list) = std::fs::read_dir(&self.home) {
            let mut dirs: Vec<PathBuf> = list.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            dirs.sort();
            found.extend(projects_in(&self.home).into_iter().map(|p| (p, Origin::Plate, None)));
            for dir in dirs {
                found.extend(projects_in(&dir).into_iter().map(|p| (p, Origin::Plate, None)));
            }
        }
        for (file, origin, secs) in found {
            let key = canon(&file);
            if files.contains(&key) {
                continue;
            }
            files.push(key.clone());
            let id = format!("m-{:016x}", fnv(&key.display().to_string()));
            let saved = load_file(&file).map(|c| content_text(&c)).unwrap_or_default();
            let edited = secs.or_else(|| modified_secs(&file)).unwrap_or(0);
            self.migrated(&Plate { id, name: self.name_of(&file), file: Some(file), edited, printer: String::new(), modified: false, saved, origin, removed: false }, &mut written)?;
            made += 1;
        }
        // (one run again keeps what the person put on the bed since)
        if current.is_some() && self.current().is_none() {
            let path = self.root.join("current");
            project::write_unflushed(&path, current_text(current.as_ref()).as_bytes())?;
            written.push(path);
        }
        Ok(Some((made, Unflushed { done, files: written })))
    }

    /// A migrated plate's record, on the disk's cache (`written`), unless one that reads back is
    /// there: a migration a crash cut short, run again, keeps a plate renamed since as it is.
    fn migrated(&self, plate: &Plate, written: &mut Vec<PathBuf>) -> Result<(), String> {
        if self.read(&plate.id).is_some() {
            return Ok(());
        }
        let path = self.dir(&plate.id).join("plate");
        project::write_unflushed(&path, plate.text().as_bytes())?;
        written.push(path);
        Ok(())
    }

    /// Give a plate another name. A saved plate's file is renamed with it (its picture and
    /// meshes beside it), and its folder too when it is one of the plates home's own; the
    /// plate's new record is returned.
    pub fn rename(&self, id: &str, name: &str) -> Result<Plate, String> {
        let mut plate = self.read(id).ok_or_else(|| format!("no plate {id}"))?;
        let name = name.trim();
        if name.is_empty() {
            return Err("a plate needs a name".into());
        }
        if let Some(file) = plate.file.clone() {
            let mut dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
            // A folder of its own in the plates home takes the new name too.
            if dir.parent() == Some(self.home.as_path()) && projects_in(&dir).len() == 1 {
                let mut to = self.home.join(folder_name(name));
                let mut n = 1;
                while to.exists() && to != dir {
                    n += 1;
                    to = self.home.join(format!("{} {n}", folder_name(name)));
                }
                if to != dir {
                    std::fs::rename(&dir, &to).map_err(|e| format!("{}: {e}", dir.display()))?;
                    dir = to;
                }
            }
            let old = dir.join(file.file_name().unwrap_or_default());
            let new = dir.join(format!("{}.{}", file_stem(name), project::EXTENSION));
            if new != old {
                if new.exists() {
                    return Err(format!("{} is there already", new.display()));
                }
                for (from, to) in [(project::thumbnail_path(&old), project::thumbnail_path(&new)), (project::meshes_dir(&old), project::meshes_dir(&new))] {
                    if from.exists() {
                        std::fs::rename(&from, &to).map_err(|e| format!("{}: {e}", from.display()))?;
                    }
                }
                std::fs::rename(&old, &new).map_err(|e| format!("{}: {e}", old.display()))?;
            }
            plate.file = Some(new);
        }
        plate.name = name.to_string();
        self.write(&plate)?;
        Ok(plate)
    }

    /// Take a plate off the list: its file and working copy stay.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut plate = self.read(id).ok_or_else(|| format!("no plate {id}"))?;
        plate.removed = true;
        self.write(&plate)
    }

    /// Move a plate's files to the trash (its file with what is beside it, or the plates home
    /// folder of its own) and its working copy; it is off the list. `trash` moves one path.
    pub fn delete(&self, id: &str, trash: &dyn Fn(&Path) -> Result<(), String>) -> Result<(), String> {
        let plate = self.read(id).ok_or_else(|| format!("no plate {id}"))?;
        // The id folder moves first. If that rename is refused (on Windows, a handle open
        // inside), nothing else has moved: the plate stays taken off the list with every file
        // kept. A later failure on a project file leaves a project the person can still open
        // from its file.
        trash(&self.dir(id))?;
        if let Some(file) = &plate.file {
            let dir = file.parent().unwrap_or(Path::new("."));
            if dir.parent() == Some(self.home.as_path()) && projects_in(dir).len() == 1 {
                trash(dir)?;
            } else {
                for path in [file.clone(), project::thumbnail_path(file), project::meshes_dir(file)] {
                    if path.exists() {
                        trash(&path)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Move `path` to this user's trash: `~/.Trash` on macOS, the freedesktop trash elsewhere
/// (`~/.local/share/Trash`, with its info file), under a free name.
pub fn move_to_trash(home: &Path, path: &Path) -> Result<(), String> {
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("{} has no name", path.display()))?.to_string();
    let (files, info) = if cfg!(target_os = "macos") {
        (home.join(".Trash"), None)
    } else {
        let t = home.join(".local").join("share").join("Trash");
        (t.join("files"), Some(t.join("info")))
    };
    std::fs::create_dir_all(&files).map_err(|e| format!("{}: {e}", files.display()))?;
    let mut to = files.join(&name);
    let mut n = 1;
    while to.exists() {
        n += 1;
        to = files.join(format!("{name} {n}"));
    }
    if let Some(info) = info {
        let t = makepad_civil_time::local_or_utc(makepad_civil_time::now_secs());
        let at = format!("{}T{:02}:{:02}:{:02}", t.date_text(), t.hour, t.minute, t.second);
        let text = format!("[Trash Info]\nPath={}\nDeletionDate={at}\n", path.display());
        project::write_atomic(&info.join(format!("{}.trashinfo", to.file_name().and_then(|n| n.to_str()).unwrap_or(&name))), text.as_bytes())?;
    }
    std::fs::rename(path, &to).map_err(|e| format!("{}: {e}", path.display()))
}

/// The content hash a record keeps, back as bytes; none for an empty or unreadable one.
pub fn content_bytes(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

impl PlateStore {
    /// Forget a plate made a moment ago that never held a document (its build failed): its
    /// record goes with the folder made for it, which holds nothing else.
    pub fn forget_new(&self, id: &str) {
        let dir = self.dir(id);
        let _ = std::fs::remove_file(dir.join("plate"));
        let _ = std::fs::remove_dir(&dir);
    }
}

/// A short date: `07:29` on `today`, `Yesterday`, else `31 Oct`.
pub fn short_when(secs: i64, today: makepad_civil_time::Day) -> String {
    let t = makepad_civil_time::local_or_utc(secs);
    let day = t.day_number();
    if day == today {
        t.hm()
    } else if makepad_civil_time::add_days(day, 1) == today {
        "Yesterday".to_string()
    } else {
        format!("{} {}", t.day, &makepad_civil_time::month_name(t.month)[..3])
    }
}

/// The longest side of a saved picture, in pixels.
pub const THUMB_SIDE: u32 = 320;

/// A plate's picture from the viewport's frame: `rgba` top row first, `width` x `height`
/// pixels, shrunk to [`THUMB_SIDE`] and made opaque.
pub fn thumbnail_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
        return Err(format!("a {width} x {height} frame needs {} bytes, got {}", width as usize * height as usize * 4, rgba.len()));
    }
    let frame = makepad_model::RgbaImage { width, height, pixels: rgba.to_vec() };
    let k = (THUMB_SIDE as f64 / width.max(height) as f64).min(1.0);
    let mut small = crate::img2mesh::shrink(&frame, ((width as f64 * k).round() as u32).max(1), ((height as f64 * k).round() as u32).max(1));
    for p in small.pixels.chunks_exact_mut(4) {
        p[3] = 255;
    }
    small.to_png(&makepad_model::Limits::default()).map_err(|e| e.to_string())
}
