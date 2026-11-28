//! A forced quit records only the outstanding order range. The plate's next
//! writer consumes the note before assigning a prefix to any new reservations.
use super::*;
use std::sync::atomic::AtomicU8;

const FALLBACK: &str = "journal-loss";
const NAME: &str = "loss-";

#[derive(Clone, Copy)]
struct Range {
    count: u64,
    from: u64,
    to: u64,
    local: bool,
}

pub(super) struct Note {
    id: String,
    plate: Option<String>,
    time: u64,
    journal: Option<Incarnation>,
    range: Range,
}

impl Note {
    // A local range has no disk identity. Once reported, keep those rows
    // queued until exit. A writer with a known prefix can still drain.
    pub(super) fn cut(dir: &Path, orders: &mut Orders) -> Option<Vec<Self>> {
        let notes = Self::outstanding(dir, orders, 0)?;
        if notes.iter().any(|n| n.range.local) { orders.cut_without_prefix = true; }
        Some(notes)
    }

    pub(super) fn outstanding(dir: &Path, orders: &Orders, after: u64) -> Option<Vec<Self>> {
        let plate = dir.parent().and_then(plate_id).map(str::to_owned);
        let &last = orders.outstanding.last()?;
        let mut notes = Vec::new();
        let mut add = |first: u64, end: u64, base: Option<u64>, count: u64| {
            if count == 0 || end <= after { return }
            let start = first.max(after + 1);
            let mut pending = orders.outstanding.range(start..=end);
            let count = if start == first { count } else { pending.clone().count() as u64 };
            if count == 0 { return }
            let from = *pending.next().expect("a nonempty segment at the cut");
            let to = pending.next_back().copied().unwrap_or(from);
            let absolute = |n| base.map_or(n, |b| b + (n - first));
            let range = Range { from: absolute(from), to: absolute(to), count, local: base.is_none() };
            static SERIAL: AtomicU64 = AtomicU64::new(0);
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
            let id = format!("{:x}-{:x}-{:x}", now.as_nanos(), std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed));
            let journal = base.and(orders.incarnation);
            notes.push(Self { id, plate: plate.clone(), time: now.as_millis() as u64, journal, range });
        };
        let mut next = 1;
        let mut remaining = orders.outstanding.len() as u64;
        for s in &orders.segments.0 {
            // Beyond a full segment every slot has been reserved. Count without
            // walking the queue on the UI thread during forced quit.
            let count = if last > s.last { s.last + 1 - s.first - s.spent } else { remaining };
            add(s.first, s.last.min(last), Some(s.base), count);
            remaining -= count;
            next = s.last + 1;
            if remaining == 0 { break }
        }
        if remaining != 0 {
            add(next, last, None, remaining);
        }
        Some(notes)
    }

    fn fields(&self, range: Range) -> Vec<(&'static str, Value)> {
        let mut fields = vec![("note", s(&self.id)), ("time_ms", number(self.time)), ("count", number(range.count)),
            ("from", number(range.from)), ("to", number(range.to))];
        if range.local { fields.push(("prefix", s("unknown"))); }
        fields
    }

    fn file(&self) -> String { format!("{NAME}{}.json", self.id) }

    fn foreign_to(&self, current: Incarnation) -> bool {
        !self.range.local && self.journal != Some(current)
    }

    fn write(&self, dir: &Path) -> io::Result<()> {
        self.publish(dir, |_| ())
    }

    fn publish(&self, dir: &Path, published: impl FnOnce(PathBuf)) -> io::Result<()> {
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            if !fs::symlink_metadata(parent)?.is_dir() { return Err(invalid()) }
        }
        if !fs::symlink_metadata(dir)?.is_dir() { return Err(invalid()) }
        let path = dir.join(self.file());
        let tmp = path.with_extension("tmp");
        let mut fields = self.fields(self.range);
        if let Some(id) = self.journal { fields.push(("journal", s(id.to_string()))); }
        fields.push(("plate", s(self.plate.as_deref().ok_or_else(invalid)?)));
        #[cfg(test)]
        tests::probe("publish loss note")?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let result = (|| {
            file.write_all(obj(fields).to_json().as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, &path)?;
            published(path);
            sync_dir(dir)
        })();
        if result.is_err() { let _ = fs::remove_file(tmp); }
        result
    }

    fn read(path: &Path, plate: &str) -> io::Result<Self> {
        let mut bytes = Vec::new();
        open_regular(path)?.ok_or(io::ErrorKind::NotFound)?.read_to_end(&mut bytes)?;
        let row = reader::parse(&bytes)?;
        let n = |k| row.get(k).and_then(Value::as_u64).ok_or_else(invalid);
        let id = row.get("note").and_then(Value::as_str).ok_or_else(invalid)?;
        let local = match row.get("prefix") {
            None => false, Some(v) if v.as_str() == Some("unknown") => true, _ => return Err(invalid()),
        };
        let journal = match row.get("journal") {
            None => None,
            Some(_) if local => return Err(invalid()),
            Some(Value::Null) => None,
            Some(value) => Some(value.as_str().and_then(Incarnation::parse).ok_or_else(invalid)?),
        };
        let range = Range { count: n("count")?, from: n("from")?, to: n("to")?, local };
        let note = Self { range, plate: Some(plate.into()), id: id.into(), time: n("time_ms")?, journal };
        if range.from == 0 || range.count == 0 || range.to < range.from || range.count > range.to - range.from + 1
            || row.get("plate").and_then(Value::as_str) != Some(plate) || path.file_name().and_then(|n| n.to_str()) != Some(&note.file()) {
            return Err(invalid())
        }
        Ok(note)
    }
}

pub(super) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

// Make each missing directory entry durable as well, including a working
// plate's very first store folder. A non-directory at the destination fails.
pub(super) fn ensure_dir(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() { return Ok(()) }
    match fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(invalid()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let parent = dir.parent().ok_or_else(invalid)?;
            ensure_dir(parent)?;
            match fs::create_dir(dir) {
                Ok(()) => (),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && fs::symlink_metadata(dir)?.is_dir() => (),
                Err(e) => return Err(e),
            }
            sync_dir(parent)
        }
        Err(e) => Err(e),
    }
}

pub(super) struct Attempt {
    finished: (Mutex<Option<bool>>, std::sync::Condvar),
    pipe: Mutex<Option<Arc<live::Pipe>>>,
    notes: Vec<(Note, AtomicU8, AtomicBool)>,
    dirs: Vec<PathBuf>,
}

impl Attempt {
    pub(super) fn new(notes: Vec<Note>, journal: &Path, settings: Option<&Path>) -> Self {
        let dirs = match (journal.parent(), notes[0].plate.as_deref()) {
            (Some(plate), Some(id)) => locations(plate, id, settings).collect(),
            _ => Vec::new(),
        };
        Self { pipe: Mutex::default(), finished: Default::default(), notes: notes.into_iter().map(|n| (n, 0.into(), false.into())).collect(), dirs }
    }

    pub(super) fn write(&self) {
        self.publish(false);
    }

    fn publish(&self, retry: bool) {
        let mut complete = true;
        for (note, tried, published) in &self.notes {
            if retry && published.load(Ordering::Acquire) { continue }
            let mut saved = false;
            for (i, dir) in self.dirs.iter().enumerate() {
                tried.store(i as u8 + 1, Ordering::Release);
                // Only the settings fallback belongs to the note writer. Plate Delete
                // may already have moved the primary destination.
                if i != 0 && ensure_dir(dir).is_err() { continue }
                if note.write(dir).is_ok() { saved = true; break }
            }
            if saved { published.store(true, Ordering::Release); }
            complete &= saved;
        }
        self.finish(complete);
    }

    pub(super) fn keep_pipe(&self, pipe: Arc<live::Pipe>) {
        *self.pipe.lock().unwrap() = Some(pipe);
    }

    pub(super) fn finish(&self, complete: bool) {
        self.pipe.lock().unwrap().take();
        *self.finished.0.lock().unwrap() = Some(complete);
        self.finished.1.notify_all();
    }

    pub(super) fn wait(&self) -> io::Result<()> {
        let saved = self.finished.1.wait_while(self.finished.0.lock().unwrap(), |done| done.is_none()).unwrap();
        if *saved == Some(true) { return Ok(()) }
        drop(saved);
        self.publish(true);
        if *self.finished.0.lock().unwrap() == Some(true) { Ok(()) } else { Err(io::Error::other("loss note publication failed")) }
    }

    pub(super) fn count(&self) -> u64 { self.notes.iter().map(|(n, _, _)| n.range.count).sum() }

    pub(super) fn summary(&self) -> String {
        let paths: Vec<_> = self.notes.iter().flat_map(|(n, tried, _)| self.dirs[..tried.load(Ordering::Acquire) as usize].iter()
            .map(move |dir| dir.join(n.file()).display().to_string().escape_debug().to_string())).collect();
        format!("{} for {} (paths tried: {})", self.count(), self.notes[0].0.plate.as_deref().unwrap_or("unknown plate").escape_debug(),
            if paths.is_empty() { "none".into() } else { paths.join(", ") })
    }
}

/// These names belong only to this Delete. If publishing the cut fails, a
/// resumed writer must not discover a loss for reservations it still owns.
/// Quit's notes and the notes already transferred are never part of this set.
#[derive(Default)]
pub(super) struct DeleteNotes(Vec<PathBuf>);

impl DeleteNotes {
    pub(super) fn publish(&mut self, plate: &Path, note: Note) -> io::Result<()> {
        let write = note.publish(plate, |path| self.0.push(path)).and_then(|()| sync_parent(plate));
        if let Err(mut error) = write {
            for path in &self.0 {
                if let Err(e) = fs::remove_file(path) {
                    if e.kind() != io::ErrorKind::NotFound { error = e; }
                }
            }
            sync_parent(plate)?;
            return Err(error)
        }
        Ok(())
    }
}

/// Publish the original bytes beside the plate before removing the fallback
/// name. The filename is also recovery's deduplication key after a crash.
pub(super) fn transfer(plate: &Path, settings: &Path) -> io::Result<()> {
    let root = settings.join(FALLBACK);
    match fs::symlink_metadata(&root) {
        Ok(meta) if meta.is_dir() => (),
        Ok(_) => return Err(invalid()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    let fallback = root.join(plate_id(plate).ok_or_else(invalid)?);
    let Some(files) = entries(&fallback)? else { return Ok(()) };
    if !fs::symlink_metadata(plate)?.is_dir() { return Err(invalid()) }
    for entry in files {
        let entry = entry?;
        if !is_note(&entry) || !entry.file_type()?.is_file() { continue }
        let source = entry.path();
        let mut file = open_regular(&source)?.ok_or(io::ErrorKind::NotFound)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        drop(file);
        let target = plate.join(entry.file_name());
        if let Some(mut primary) = open_regular_with(&target, OpenOptions::new().read(true).write(true))? {
            let mut existing = Vec::new();
            primary.read_to_end(&mut existing)?;
            if bytes != existing { return Err(invalid()) }
            primary.sync_all()?;
        } else {
            let tmp = target.with_extension(format!("{}.transfer.tmp", Incarnation::mint()));
            let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
            let result = (|| {
                file.write_all(&bytes)?;
                file.sync_all()?;
                drop(file);
                fs::rename(&tmp, &target)
            })();
            if let Err(error) = result {
                let _ = fs::remove_file(tmp);
                return Err(error)
            }
        }
        sync_parent(plate)?;
        fs::remove_file(source)?;
        sync_parent(&fallback)?;
        #[cfg(test)]
        tests::probe("transferred loss note")?;
    }
    // Also confirm an unlink whose directory sync failed on an earlier try.
    sync_parent(&fallback)
}

/// The plate folder has moved. Only an empty remainder can be removed;
/// unrelated files and late arrivals in its fallback go to the Trash.
pub(crate) fn finish_delete(plate: &Path, trash: &dyn Fn(&Path) -> Result<(), String>) -> io::Result<()> {
    let settings = plate.parent().and_then(Path::parent).ok_or_else(invalid)?;
    let root = settings.join(FALLBACK);
    let dir = root.join(plate_id(plate).ok_or_else(invalid)?);
    for path in [&root, &dir] {
        match fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() => (),
            Ok(_) => return Err(invalid()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    match fs::remove_dir(&dir) {
        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => trash(&dir).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn entries(dir: &Path) -> io::Result<Option<fs::ReadDir>> {
    match fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => fs::read_dir(dir).map(Some),
        Ok(_) => Err(invalid()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn plate_id(plate: &Path) -> Option<&str> {
    plate.file_name()?.to_str().filter(|id| reader::literal(id))
}

fn locations(plate: &Path, id: &str, settings: Option<&Path>) -> impl Iterator<Item = PathBuf> {
    std::iter::once(plate.to_owned()).chain(settings.map(|root| root.join(FALLBACK).join(id)))
}

fn is_note(entry: &fs::DirEntry) -> bool {
    entry.file_name().to_str().is_some_and(|s| s.starts_with(NAME) && s.ends_with(".json"))
}

/// Search both publication locations using the same note validation as recovery.
pub(super) fn has_note(plate: &Path, settings: Option<&Path>, matching: Option<Incarnation>) -> io::Result<bool> {
    let id = plate_id(plate).ok_or_else(invalid)?;
    for dir in locations(plate, id, settings) {
        match fs::symlink_metadata(dir.parent().ok_or_else(invalid)?) {
            Ok(meta) if meta.is_dir() => (),
            Ok(_) => return Err(invalid()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
        for entry in entries(&dir)?.into_iter().flatten() {
            let entry = entry?;
            if is_note(&entry) && Note::read(&entry.path(), id).is_ok_and(|note| {
                matching.is_none_or(|current| !note.range.local && note.journal == Some(current))
            }) { return Ok(true) }
        }
    }
    Ok(false)
}

// Discovery and a later producer can open separate generations of the same
// writer. A note left for inspection gets one diagnostic per process.
pub(super) fn skipped(path: &Path, log: &mut impl FnMut(String)) {
    static WARNED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
    let first = WARNED.lock().unwrap().insert(path.to_owned());
    if first { log(format!("journal: skipping loss note at {path:?}")); }
}

/// Examine literal store entries, including working ids with no saved-plate
/// metadata and fallback-only ids whose plate folder has never existed.
pub(super) fn discover(store: &fab_ai::plates::PlateStore, settings: Option<&Path>, mut wake: impl FnMut(PathBuf)) {
    let mut plates = BTreeSet::new();
    let fallback = settings.map(|root| root.join(FALLBACK));
    for root in std::iter::once(&store.root).chain(fallback.as_ref()) {
        let Ok(Some(children)) = entries(root) else { continue };
        for entry in children.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) { continue }
            let name = entry.file_name();
            let Some(id) = name.to_str().filter(|id| reader::literal(id)) else { continue };
            if entries(&entry.path()).ok().flatten().is_some_and(|files| files.flatten().any(|f| is_note(&f))) {
                plates.insert(store.dir(id).join("journal"));
            }
        }
    }
    for dir in plates { wake(dir); }
}

// A missing plate has notes only in the fallback. An absolute range means a writer
// prepared in the old folder. An empty, unreadable or malformed set cannot authorize creation.
pub(super) fn may_create_missing(plate: &Path, settings: Option<&Path>) -> bool {
    let Some(id) = plate_id(plate).filter(|id| reader::sessions::minted(id).is_some()) else { return false };
    let Some(fallback) = settings.map(|dir| dir.join(FALLBACK)) else { return false };
    if !fs::symlink_metadata(&fallback).is_ok_and(|m| m.is_dir()) { return false }
    let Ok(Some(files)) = entries(&fallback.join(id)) else { return false };
    let mut found = false;
    for file in files {
        let Ok(file) = file else { return false };
        if !is_note(&file) { continue }
        if !Note::read(&file.path(), id).is_ok_and(|note| note.range.local) { return false }
        found = true;
    }
    found
}

/// Recovery of an existing plate never needs permission to recreate its folder after Delete.
pub(super) fn discover_for_recovery(store: &fab_ai::plates::PlateStore, log: &mut impl FnMut(String),
    mut wake: impl FnMut(PathBuf, bool)) {
    let settings = store.root.parent();
    discover(store, settings, |dir| {
        let Some(plate) = dir.parent() else { return };
        let missing = fs::symlink_metadata(plate).is_err_and(|e| e.kind() == io::ErrorKind::NotFound);
        if missing && !may_create_missing(plate, settings) {
            skipped(plate, log);
            return
        }
        wake(dir, missing);
    });
}

#[derive(Default)]
pub(super) struct Recovery {
    notes: Vec<(PathBuf, Note)>,
    seen: BTreeSet<String>,
    // Kept only on a recovery scan with absolute ranges. A sync already in
    // flight, or a failed writer's retry, may have landed before exit.
    survived: BTreeSet<u64>,
    known: Option<(u64, u64)>,
    highest: u64,
}

impl Recovery {
    pub(super) fn load(journal: &Path, current: Incarnation, settings: Option<&Path>, log: &mut impl FnMut(String)) -> Self {
        let mut recovery = Self::default();
        let Some(plate) = journal.parent() else { return recovery };
        let Some(id) = plate_id(plate) else { return recovery };
        for dir in locations(plate, id, settings) {
            if let Err(e) = recovery.collect(&dir, id, current, log) {
                if e.kind() != io::ErrorKind::NotFound { skipped(&dir, log); }
            }
        }
        if !recovery.is_empty() {
            // No note authorizes an append through a symlink. A regular
            // producer can still prepare its journal independently.
            let safe = |path: &Path, directory: bool| match fs::symlink_metadata(path) {
                Ok(m) => if directory { m.is_dir() } else { m.is_file() },
                Err(e) => e.kind() == io::ErrorKind::NotFound,
            };
            if !plate.parent().is_some_and(|p| p.as_os_str().is_empty() || safe(p, true)) || !safe(plate, true)
                || !safe(journal, true) || !safe(&journal.join(LOG), false) {
                recovery.warn_all(log);
                return Self::default()
            }
            recovery.notes.sort_by(|(_, a), (_, b)| (a.time, &a.id).cmp(&(b.time, &b.id)));
        }
        recovery
    }

    fn collect(&mut self, dir: &Path, id: &str, current: Incarnation, log: &mut impl FnMut(String)) -> io::Result<()> {
        let parent = dir.parent().ok_or_else(invalid)?;
        if !parent.as_os_str().is_empty() && !fs::symlink_metadata(parent)?.is_dir() { return Err(invalid()) }
        for file in entries(dir)?.into_iter().flatten() {
            let Ok(file) = file else { skipped(dir, log); continue };
            if !is_note(&file) { continue }
            let path = file.path();
            let Ok(note) = Note::read(&path, id) else { skipped(&path, log); continue };
            if !note.foreign_to(current) {
                if note.range.to >> LIFE_BITS >= LAST_LIFE {
                    skipped(&path, log);
                    continue
                }
                self.highest = self.highest.max(note.range.to);
                if !note.range.local {
                    let (first, last) = self.known.get_or_insert((note.range.from, note.range.to));
                    *first = (*first).min(note.range.from);
                    *last = (*last).max(note.range.to);
                }
            }
            self.notes.push((path, note));
        }
        Ok(())
    }

    pub(super) fn highest(&self) -> u64 { self.highest }

    pub(super) fn is_empty(&self) -> bool { self.notes.is_empty() }

    pub(super) fn warn_all(&self, log: &mut impl FnMut(String)) {
        for (path, _) in &self.notes { skipped(path, log); }
    }

    pub(super) fn observe(&mut self, order: u64, row: &Value) {
        if self.notes.is_empty() { return }
        if row.get("kind").and_then(Value::as_str) == Some("lost") {
            if row.get("cause").and_then(Value::as_str) == Some("unwritten") {
                if let Some(id) = row.get("note").and_then(Value::as_str) {
                    self.seen.insert(id.to_owned());
                    return
                }
            }
            // Both torn repairs and empty repair markers (which have no note)
            // occupy a producer local in a packed life.
            if order >> LIFE_BITS == 0 { return }
        }
        if self.known.is_some_and(|(first, last)| first <= order && order <= last) {
            self.survived.insert(order);
        }
    }

    pub(super) fn finish(mut self, writer: &Writer, current: Incarnation, mut highest: u64, log: &mut impl FnMut(String)) -> u64 {
        for (path, note) in self.notes {
            let survivors = (!note.foreign_to(current)).then_some(&self.survived);
            if note.apply(writer, &mut highest, &mut self.seen, survivors, &path, log).is_err() {
                skipped(&path, log);
            }
        }
        highest
    }
}

impl Note {
    fn apply(self, writer: &Writer, highest: &mut u64, seen: &mut BTreeSet<String>,
        survived: Option<&BTreeSet<u64>>, path: &Path, log: &mut impl FnMut(String)) -> io::Result<()> {
        if !seen.contains(&self.id) {
            let range = self.range;
            let order = writer.next_startup_order(highest, log)?;
            let lost = match survived {
                Some(orders) if !range.local => range.missing(orders),
                _ => range,
            };
            let mut fields = self.fields(lost);
            if survived.is_none() {
                fields.push(("journal", self.journal.map_or(Value::Null, |id| s(id.to_string()))));
            }
            fields.extend([("kind", s("lost")), ("cause", s("unwritten")), ("order", number(order))]);
            let mut bytes = obj(fields).to_json().into_bytes();
            bytes.push(b'\n');
            writer.append_bytes(&bytes)?;
            *highest = order;
            seen.insert(self.id);
        }
        // After a crash, confirm the marker and its directory before unlink.
        writer.check_log()?;
        writer.log_file()?.sync_all()?;
        sync_dir(&writer.dir)?;
        fs::remove_file(path)?;
        sync_dir(path.parent().ok_or_else(invalid)?)
    }
}

impl Range {
    /// Find holes between durable orders without enumerating an ordinal range.
    fn missing(self, survived: &BTreeSet<u64>) -> Self {
        let mut next = self.from;
        let (mut first, mut last, mut count) = (None, 0, 0);
        for &order in survived.range(self.from..=self.to) {
            if next < order {
                first.get_or_insert(next);
                last = order - 1;
                count += order - next;
            }
            next = order + 1;
        }
        if next <= self.to {
            first.get_or_insert(next);
            last = self.to;
            count += last - next + 1;
        }
        // Further independent disk damage cannot increase the loss recorded at quit.
        if count > self.count { return self }
        // If all retries landed, retain the range inspected at quit and acknowledge
        // its note with a zero-count marker. There is no rebuild gap in that range.
        match first {
            Some(from) => Self { from, to: last, count, local: false },
            None => Self { count: 0, ..self },
        }
    }
}

pub(super) fn file_range(row: &Value) -> Option<(u64, u64)> {
    if row.get("kind")?.as_str()? != "lost" || row.get("cause")?.as_str()? != "unwritten"
        || row.get("prefix").is_some() || row.get("journal").is_some() {
        return None
    }
    let (from, to) = (row.get("from")?.as_u64()?, row.get("to")?.as_u64()?);
    (from > 0 && from <= to).then_some((from, to))
}
