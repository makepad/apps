//! Read-only plate journal browsing. File offsets follow complete lines; page keys follow
//! creation order. All file access, indexing and JSON decoding belong to the reader worker.
#[path = "sessions.rs"]
pub(crate) mod sessions;
#[path = "script.rs"]
mod script;
pub(crate) use script::DiffVersion;

use super::{FileId, Incarnation, INCARNATION, invalid, live, obj, s, torn_at, Value, LOG, RECORD_DEPTH};
use makepad_widgets::makepad_platform::thread::{SpawnError, ThreadOptions, ThreadSpawner};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    num::NonZeroU64,
    path::{Component, Path, PathBuf},
    sync::{atomic::{AtomicU64, Ordering}, mpsc, Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Call, Done, Effect, History, Reload, Checkpoint, Lost, Other, Diagnostic }

impl Kind {
    pub(super) fn from_name(name: &str) -> Self {
        match name {
            "call" => Kind::Call, "done" => Kind::Done, "effect" => Kind::Effect, "history" => Kind::History,
            "reload" => Kind::Reload, "checkpoint" => Kind::Checkpoint, "lost" => Kind::Lost, _ => Kind::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actor { Person, Ai, Agent, Disk }

#[derive(Clone, Copy, Default)]
pub struct Filter {
    pub actor: Option<Actor>,
    pub kind: Option<Kind>,
}

/// Diagnostics have no creation order. They have their own stable keys and appear
/// first, most recent file offset first, without displacing a valid record's key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Position { Order(u64), Diagnostic(u64) }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key { pub folder: Arc<str>, pub journal: u64, pub position: Position }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target { pub folder: Arc<str>, pub generation: u64 }

pub enum Request {
    /// Start at anchor (inclusive), or at the newest matching record.
    Page { filter: Filter, anchor: Option<Key>, size: usize },
    Count(Filter),
    Detail(Key),
}

pub struct Row {
    pub key: Key,
    pub kind: Kind,
    pub actor: Option<Actor>,
    pub call_time_ms: Option<u64>,
    pub label: String,
    pub changed_files: Option<usize>,
}

pub enum Partner { None, NotFound, Found { key: Key, record: Value } }

pub enum Answer {
    /// The watched folder has been scanned, or its set of complete lines changed.
    Changed,
    Replaced,
    Health(super::Health),
    Page { rows: Vec<Row>, next: Option<Key> },
    Count(usize),
    Detail { record: Value, partner: Partner },
    NotFound,
    Error(io::ErrorKind),
}

pub struct Reply { pub target: Target, pub answer: Answer }

pub(super) fn literal(folder: &str) -> bool {
    folder != fab_ai::plates::REMOVED_SESSIONS && !folder.contains(['/', '\\', ':', '\0']) &&
        matches!(Path::new(folder).components().next(), Some(Component::Normal(_)))
}

fn missing<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// The root comes from the plate store. Only literal directory entries and regular
/// files under that root can supply a journal (including for foreign call lookups).
fn log_file(root: &Path, folder: &str) -> io::Result<Option<(File, FileId)>> {
    if !literal(folder) { return Ok(None) }
    let plate = root.join(folder);
    let journal = plate.join("journal");
    for path in [&plate, &journal] {
        if !missing(fs::symlink_metadata(path))?.is_some_and(|m| m.is_dir()) { return Ok(None) }
    }
    let Some((file, metadata)) = super::open_regular_with_metadata(&journal.join(LOG), fs::OpenOptions::new().read(true))?
        else { return Ok(None) };
    #[cfg(unix)]
    let id = FileId::from_metadata(&metadata);
    #[cfg(not(unix))]
    let id = { let _ = metadata; FileId::of(&file)? };
    Ok(Some((file, id)))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Binding {
    incarnation: Option<Incarnation>,
    file: FileId,
    token: u64,
}

fn read_incarnation(dir: &Path) -> io::Result<Option<Incarnation>> {
    super::open_regular(&dir.join(INCARNATION))?.map(|f| Incarnation::read(&f)?.ok_or_else(invalid)).transpose()
}

/// Called under the Remove freeze. Use the very same binding as a listed session.
pub(super) fn confirmed_binding(root: &Path, folder: &str) -> io::Result<(u64, Option<Incarnation>)> {
    let dir = root.join(folder).join("journal");
    let (_log, file) = log_file(root, folder)?.ok_or(io::ErrorKind::NotFound)?;
    let incarnation = bound_incarnation(&dir, file)?;
    Ok((Binding::new(incarnation, file).token, incarnation))
}

fn bound_incarnation(dir: &Path, file: FileId) -> io::Result<Option<Incarnation>> {
    let incarnation = read_incarnation(dir)?;
    if file.matches_path(&dir.join(LOG))? == Some(false) { return Err(io::ErrorKind::NotFound.into()) }
    Ok(incarnation)
}

impl Binding {
    fn new(incarnation: Option<Incarnation>, file: FileId) -> Self {
        // Fixed-width big-endian fields; an absent incarnation differs from id zero.
        let mut bytes = [0; 33];
        if let Some(id) = incarnation {
            bytes[0] = 1;
            bytes[1..17].copy_from_slice(&id.0.to_be_bytes());
        }
        bytes[17..25].copy_from_slice(&file.0.to_be_bytes());
        bytes[25..].copy_from_slice(&file.1.to_be_bytes());
        let hash = makepad_core_util::sha256::sha256(&bytes);
        Self { incarnation, file, token: u64::from_be_bytes(hash[..8].try_into().unwrap()) }
    }
}

/// The replay pool receives the handle verified by the reader. It never resolves
/// the record key against another log at the same path.
pub(crate) struct VerifiedLog {
    pub(super) file: File,
    binding: Binding,
}

impl VerifiedLog {
    pub(super) fn require_current(&self, dir: &Path) -> io::Result<()> {
        if read_incarnation(dir)? != self.binding.incarnation || self.binding.file.matches_path(&dir.join(LOG))? == Some(false) {
            return Err(io::ErrorKind::NotFound.into())
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Refresh { Unchanged, Changed, Replaced }

pub(super) fn parse(bytes: &[u8]) -> io::Result<Value> {
    // Tool arguments can use the full tool nesting allowance inside the journal envelope.
    makepad_strict_json::parse_depth(bytes, RECORD_DEPTH).map_err(|_| invalid())
}

struct Entry {
    offset: u64,
    kind: Kind,
    own_actor: Option<Actor>,
    call: Option<NonZeroU64>,
}

/// The folder is shared by every entry in one index. Keep the joins sparse, so
/// standalone typing and drag effects pay for neither strings nor a call timestamp.
#[derive(Default)]
pub(super) struct Index {
    binding: Option<Binding>,
    entries: BTreeMap<Position, Entry>,
    calls: HashMap<u64, (Position, Option<u64>)>,
    dones: HashMap<u64, Position>,
    effects: HashMap<u64, Vec<Position>>,
    references: HashMap<Box<str>, Position>,
    foreign: HashMap<Position, (Box<str>, Box<str>)>,
    scanned: u64,
    partial: Vec<u8>,
    // Only the last line of a contended sample can still be rolled back.
    provisional: Vec<u8>,
    last_order: Option<Position>,
    revisions: std::cell::RefCell<HashMap<String, script::CachedRevision>>,
    verified_prefix: Arc<()>,
}

pub(super) fn header(line: &[u8]) -> io::Result<Value> {
    use makepad_strict_json::{parse_picked, Pick, Step};
    parse_picked(line, RECORD_DEPTH, false, &|path| match path {
        [] | [Step::Key("actor")] => Pick::Open,
        [Step::Key("order" | "kind" | "id" | "time_ms" | "ref" | "call" | "call_ref" | "call_journal" | "cause" | "at" | "bytes" | "note")]
            | [Step::Key("actor"), Step::Key("kind")] => Pick::Keep,
        _ => Pick::Skip,
    }).map_err(|_| invalid())
}

impl Index {
    fn insert(&mut self, offset: u64, line: &[u8]) {
        let previous = self.last_order.take();
        if let Ok(row) = header(line) {
            if row.get("order").and_then(Value::as_u64).is_some() {
                if let (Some(pos), Some(at)) = (previous, torn_at(&row, offset)) {
                    self.disown_torn(pos, at);
                }
            }
            if self.index_header(offset, &row) {
                self.last_order = row.get("order").and_then(Value::as_u64).map(Position::Order);
                return
            }
        }
        self.diagnostic(offset);
    }

    fn invalidate_revisions(&mut self) {
        self.revisions.get_mut().clear();
        // Each displayed diff holds a reference to the prefix against which it was verified.
        Arc::make_mut(&mut self.verified_prefix);
    }

    fn diagnostic(&mut self, offset: u64) {
        self.invalidate_revisions();
        self.entries.insert(Position::Diagnostic(offset), Entry { offset, kind: Kind::Diagnostic, own_actor: None, call: None });
    }

    // A write may have lost only its newline. Undo that provisional header and all its joins
    // before indexing the loss marker, which can legitimately reuse the torn row's order.
    fn disown_torn(&mut self, pos: Position, at: u64) {
        if self.entries.get(&pos).is_none_or(|entry| entry.offset != at) { return }
        let Some(entry) = self.entries.remove(&pos) else { return };
        match (entry.kind, entry.call) {
            (Kind::Call, Some(id)) => {
                self.calls.remove(&id.get());
                self.references.retain(|_, p| *p != pos);
            }
            (Kind::Done, Some(id)) => { self.dones.remove(&id.get()); }
            (Kind::Effect, Some(id)) => {
                if let Some(rows) = self.effects.get_mut(&id.get()) { rows.retain(|p| p != &pos); }
            }
            _ => { self.foreign.remove(&pos); }
        }
        self.diagnostic(at);
    }

    pub(super) fn index_header(&mut self, offset: u64, row: &Value) -> bool {
        let uint = |key| row.get(key).and_then(Value::as_u64);
        let string = |key| row.get(key).and_then(Value::as_str);
        let (Some(order), Some(kind)) = (uint("order"), string("kind")) else { return false };
        let pos = Position::Order(order);
        if self.entries.contains_key(&pos) { return false }
        let kind = Kind::from_name(kind);
        let id = match kind {
            Kind::Call | Kind::Done => {
                let Some(id) = uint("id").and_then(NonZeroU64::new) else { return false };
                if kind == Kind::Call {
                    if self.calls.contains_key(&id.get()) || string("ref").is_some_and(|r| self.references.contains_key(r)) {
                        return false
                    }
                    self.calls.insert(id.get(), (pos, uint("time_ms")));
                    if let Some(reference) = string("ref") { self.references.insert(reference.into(), pos); }
                } else {
                    if self.dones.contains_key(&id.get()) { return false }
                    self.dones.insert(id.get(), pos);
                }
                Some(id)
            }
            Kind::Effect => {
                if row.get("call").is_some_and(Value::is_null) {
                    if let (Some(folder), Some(reference)) = (string("call_journal"), string("call_ref")) {
                        self.foreign.insert(pos, (folder.into(), reference.into()));
                    }
                }
                let local = uint("call").and_then(NonZeroU64::new);
                if let Some(call) = local { self.effects.entry(call.get()).or_default().push(pos); }
                local
            }
            _ => None,
        };
        let own_actor = match (kind, row.get("actor").and_then(|a| a.get("kind")).and_then(Value::as_str)) {
            (Kind::Call | Kind::Effect, Some(name)) => match name {
                "person" => Some(Actor::Person), "ai" => Some(Actor::Ai),
                "agent" => Some(Actor::Agent), "disk" => Some(Actor::Disk), _ => None,
            },
            _ => None,
        };
        // A late append can change a revision that the reader has already verified.
        let watching = !self.revisions.get_mut().is_empty() || Arc::strong_count(&self.verified_prefix) > 1;
        if watching && (matches!(kind, Kind::Checkpoint | Kind::Lost) || self.entries.range(pos..Position::Diagnostic(0)).next().is_some()) {
            self.invalidate_revisions();
        }
        self.entries.insert(pos, Entry { offset, kind, own_actor, call: id });
        true
    }

    fn token(&self) -> Option<u64> { self.binding.map(|b| b.token) }

    fn open_log(&self, root: &Path, folder: &str) -> io::Result<File> {
        let binding = self.binding.ok_or(io::ErrorKind::NotFound)?;
        let (file, id) = log_file(root, folder)?.ok_or(io::ErrorKind::NotFound)?;
        if id != binding.file { return Err(io::ErrorKind::NotFound.into()) }
        Ok(file)
    }

    fn refresh(&mut self, root: &Path, folder: &str, current: &impl Fn() -> bool) -> io::Result<Refresh> {
        let dir = root.join(folder).join("journal");
        loop {
            let Some((file, id)) = log_file(root, folder)? else {
                let replaced = self.binding.is_some();
                *self = Self::default();
                return Ok(if replaced { Refresh::Replaced } else { Refresh::Unchanged })
            };
            let Some(incarnation) = missing(bound_incarnation(&dir, id))? else {
                if !current() { return Ok(Refresh::Unchanged) }
                continue
            };
            if self.binding.is_some_and(|b| b.file == id && b.incarnation == incarnation) {
                return self.scan(&dir, &file, current)
            }
            // Publish a new binding only after its scan succeeds. A transient read
            // error must not turn the replacement notification into an ordinary append.
            let mut next = Self { binding: Some(Binding::new(incarnation, id)), ..Self::default() };
            let scanned = next.scan(&dir, &file, current)?;
            let change = if self.binding.is_some() { Refresh::Replaced } else { scanned };
            *self = next;
            return Ok(change)
        }
    }

    fn scan(&mut self, dir: &Path, file: &File, current: &impl Fn() -> bool) -> io::Result<Refresh> {
        let mut changed = false;
        loop {
            let known = self.provisional.is_empty().then_some(self.scanned);
            let boundary = live::read_boundary(dir, file, known)?;
            changed |= self.scan_to(file, boundary, current)?;
            if boundary.still_current(dir, file)? || !current() {
                return Ok(if changed { Refresh::Changed } else { Refresh::Unchanged })
            }
        }
    }

    fn scan_to(&mut self, mut file: &File, boundary: live::ReadBoundary, current: &impl Fn() -> bool) -> io::Result<bool> {
        let end = boundary.end;
        let mut reset = end < self.scanned;
        if !reset && !self.provisional.is_empty() {
            // A failed sync can truncate and then replace a row before the next
            // refresh, even with exactly the same length. Check its bytes once
            // more before trusting the cached order and joins.
            let len = self.provisional.len() as u64;
            file.seek(SeekFrom::Start(self.scanned - len))?;
            let mut actual = Vec::new();
            (&mut file).take(len).read_to_end(&mut actual)?;
            reset = actual != self.provisional;
        }
        if reset { *self = Self { binding: self.binding, ..Self::default() }; }
        if boundary.settled { self.provisional = Vec::new(); }
        let old_count = self.entries.len();
        if end > self.scanned {
            file.seek(SeekFrom::Start(self.scanned))?;
            let mut input = BufReader::new(file.take(end - self.scanned));
            let mut line = std::mem::take(&mut self.partial);
            while self.scanned < end && current() {
                let start = self.scanned - line.len() as u64;
                let before = line.len();
                let read = match input.read_until(b'\n', &mut line) {
                    Ok(n) => n,
                    Err(error) => {
                        line.truncate(before);
                        if boundary.settled { self.partial = line; } else { self.scanned -= before as u64; }
                        return Err(error)
                    }
                };
                self.scanned += read as u64;
                if read == 0 || line.last() != Some(&b'\n') { break }
                self.insert(start, &line);
                if !boundary.settled { std::mem::swap(&mut self.provisional, &mut line); }
                line.clear();
            }
            if !line.is_empty() {
                if boundary.settled { self.partial = line; } else { self.scanned -= line.len() as u64; }
            }
        }
        Ok(reset || self.entries.len() != old_count)
    }

    fn actor(&self, entry: &Entry) -> Option<Actor> {
        if entry.kind == Kind::Done {
            return self.entries.get(&self.calls.get(&entry.call?.get())?.0)?.own_actor
        }
        entry.own_actor
    }

    fn matches(&self, entry: &Entry, filter: Filter) -> bool {
        filter.kind.is_none_or(|k| k == entry.kind) && filter.actor.is_none_or(|a| Some(a) == self.actor(entry))
    }

    fn record(&self, input: &mut BufReader<File>, pos: Position) -> io::Result<Value> {
        if let Position::Diagnostic(offset) = pos {
            return Ok(obj(vec![("kind", s("diagnostic")), ("offset", super::number(offset)), ("reason", s("Damaged journal record"))]))
        }
        let entry = self.entries.get(&pos).ok_or_else(invalid)?;
        if !self.provisional.is_empty() && entry.offset == self.scanned - self.provisional.len() as u64 {
            return parse(&self.provisional)
        }
        input.seek(SeekFrom::Start(entry.offset))?;
        let mut line = Vec::new();
        input.read_until(b'\n', &mut line)?;
        if line.last() != Some(&b'\n') { return Err(invalid()) }
        parse(&line)
    }

    fn page(&self, root: &Path, target: &Target, filter: Filter, anchor: Option<Key>, inclusive: bool, size: usize,
        current: &impl Fn() -> bool) -> io::Result<Answer> {
        use std::ops::Bound::{Excluded, Included, Unbounded};
        if anchor.as_ref().is_some_and(|k| k.folder != target.folder || Some(k.journal) != self.token()) { return Ok(Answer::NotFound) }
        let upper = anchor.map_or(Unbounded, |k| if inclusive { Included(k.position) } else { Excluded(k.position) });
        let mut matching = self.entries.range((Unbounded, upper)).rev()
            .take_while(|_| current()).filter(|(_, entry)| self.matches(entry, filter));
        let mut rows = Vec::new();
        let mut file = None;
        let Some(journal) = self.token() else { return Ok(Answer::Page { rows, next: None }) };
        let key = |position| Key { folder: target.folder.clone(), journal, position };
        for (&pos, entry) in matching.by_ref().take(size) {
            if file.is_none() { file = missing(self.open_log(root, &target.folder))?.map(BufReader::new); }
            let Some(input) = &mut file else { return Ok(Answer::NotFound) };
            let record = self.record(input, pos)?;
            let title = match entry.kind {
                Kind::Call => "action", Kind::Effect => "label", Kind::History => "direction",
                Kind::Done => "outcome", Kind::Diagnostic => "reason", _ => "kind",
            };
            let call_time_ms = entry.call.and_then(|id| self.calls.get(&id.get())?.1);
            let field = |key| record.get(key).and_then(Value::as_str).unwrap_or_default();
            let label = match entry.kind {
                Kind::Done => {
                    let call = entry.call.and_then(|id| self.calls.get(&id.get()));
                    let call = call.map(|(pos, _)| self.record(input, *pos)).transpose()?;
                    match call.as_ref().and_then(|v| v.get("action")).and_then(Value::as_str) {
                        Some(action) => format!("{action} · {}", field("outcome")),
                        None => field("outcome").to_owned(),
                    }
                }
                Kind::History => {
                    let head = |key| record.get(key).and_then(Value::as_u64).map_or_else(|| "start".into(), |n| n.to_string());
                    format!("{} · {} → {}", field("direction"), head("before"), head("after"))
                }
                Kind::Checkpoint => "Plate opened".into(),
                Kind::Reload => "Documents reloaded".into(),
                Kind::Diagnostic => "Damaged journal line".into(),
                Kind::Lost => match record.get("bytes").and_then(Value::as_u64) {
                    Some(n) => format!("Recording gap · {n} torn bytes"), None => "Recording gap".into(),
                },
                _ => field(title).to_owned(),
            };
            rows.push(Row { key: key(pos), kind: entry.kind, actor: self.actor(entry), call_time_ms,
                changed_files: record.get(if entry.kind == Kind::Checkpoint { "files" } else { "changes" })
                    .and_then(Value::as_arr).map(<[_]>::len), label });
        }
        Ok(Answer::Page { rows, next: matching.next().map(|(&pos, _)| key(pos)) })
    }
}

#[derive(Default)]
struct Store {
    folders: HashMap<Arc<str>, Index>,
}

impl Store {
    fn refresh(&mut self, root: &Path, target: &Target, current: &impl Fn() -> bool) -> io::Result<Refresh> {
        self.folders.entry(target.folder.clone()).or_default().refresh(root, &target.folder, current)
    }

    fn detail(&mut self, root: &Path, target: &Target, key: Key, current: &impl Fn() -> bool) -> io::Result<Answer> {
        let Some(index) = self.folders.get(&target.folder) else { return Ok(Answer::NotFound) };
        if key.folder != target.folder || Some(key.journal) != index.token() { return Ok(Answer::NotFound) }
        let Some(entry) = index.entries.get(&key.position) else { return Ok(Answer::NotFound) };
        let Some(file) = missing(index.open_log(root, &key.folder))? else { return Ok(Answer::NotFound) };
        let mut input = BufReader::new(file);
        let record = index.record(&mut input, key.position)?;
        let mut partner = Partner::None;
        if let Some(id) = entry.call {
            let pos = if entry.kind == Kind::Call { index.dones.get(&id.get()).copied() }
                else { index.calls.get(&id.get()).map(|c| c.0) };
            partner = match pos {
                Some(position) => Partner::Found { key: Key { position, ..key }, record: index.record(&mut input, position)? },
                None => Partner::NotFound,
            }
        } else if let Some((folder, reference)) = index.foreign.get(&key.position).cloned() {
            partner = Partner::NotFound;
            // Match the literal directory entry as well as checking its type. This matters on a
            // case-insensitive filesystem: a differently spelled ref must never bind to a plate.
            if literal(&folder) && fs::read_dir(root)?.filter_map(Result::ok).any(|e| e.file_name() == std::ffi::OsStr::new(folder.as_ref())) {
                let folder = Arc::<str>::from(folder);
                let foreign = self.folders.entry(folder.clone()).or_default();
                foreign.refresh(root, &folder, current)?;
                if let Some(&position) = foreign.references.get(reference.as_ref()) {
                    if let (Some(journal), Some(file)) = (foreign.token(), missing(foreign.open_log(root, &folder))?) {
                        partner = Partner::Found { key: Key { folder, journal, position }, record: foreign.record(&mut BufReader::new(file), position)? };
                    }
                }
            }
        }
        Ok(Answer::Detail { record, partner })
    }
}

#[derive(Clone)]
struct Watch {
    unsaved: bool,
    target: Target,
    replies: mpsc::Sender<Reply>,
}

impl Watch {
    fn reply(&self, answer: Answer) {
        if self.replies.send(Reply { target: self.target.clone(), answer }).is_ok() {
            makepad_widgets::SignalToUI::set_ui_signal();
        }
    }
}

/// A request can project its page or detail on the reader thread. The caller never receives
/// access to another folder's index, or to the filesystem.
pub(crate) struct Browse<'a> {
    root: &'a Path,
    target: &'a Target,
    store: &'a mut Store,
    current: &'a dyn Fn() -> bool,
}

impl Browse<'_> {
    pub(crate) fn journal(&self) -> Option<u64> { self.store.folders.get(&self.target.folder)?.token() }

    /// Only the selected row in the watched plate. Rebuilding never needs a linked
    /// call's contents, even when the record has a partner in another journal.
    pub(crate) fn replay_record(&self, key: &Key) -> io::Result<(Value, VerifiedLog)> {
        let missing = || io::Error::from(io::ErrorKind::NotFound);
        if key.folder != self.target.folder || Some(key.journal) != self.journal() { return Err(missing()) }
        let index = self.store.folders.get(&key.folder).ok_or_else(missing)?;
        if !index.entries.contains_key(&key.position) { return Err(missing()) }
        let file = index.open_log(self.root, &key.folder)?;
        let log = VerifiedLog { file: file.try_clone()?, binding: index.binding.ok_or_else(missing)? };
        Ok((index.record(&mut BufReader::new(file), key.position)?, log))
    }

    pub fn page(&self, filter: Filter, before: Option<Key>, count: usize) -> io::Result<(Vec<Row>, Option<Key>)> {
        let index = self.store.folders.get(&self.target.folder).ok_or_else(invalid)?;
        match index.page(self.root, self.target, filter, before, false, count, &self.current)? {
            Answer::Page { rows, next } => Ok((rows, next)),
            _ => Err(io::ErrorKind::NotFound.into()),
        }
    }

    /// The end of the last complete line. A continued read keeps this boundary even if an
    /// effect with an earlier creation order arrives between its parts.
    pub fn complete_end(&self) -> u64 {
        self.store.folders.get(&self.target.folder).map_or(0, |i| i.scanned - i.partial.len() as u64)
    }

    /// Names from this plate's committed effects, even when a call was cancelled before settling.
    pub fn changed_by(&self, call: u64, end: u64) -> io::Result<Vec<String>> {
        let Some(index) = self.store.folders.get(&self.target.folder) else { return Ok(Vec::new()) };
        let Some(positions) = index.effects.get(&call) else { return Ok(Vec::new()) };
        let file = index.open_log(self.root, &self.target.folder)?;
        let mut file = BufReader::new(file);
        let mut names = std::collections::BTreeSet::new();
        for &position in positions.iter().take_while(|_| (self.current)()) {
            if index.entries.get(&position).ok_or_else(invalid)?.offset >= end { break }
            let row = index.record(&mut file, position)?;
            for change in row.get("changes").and_then(Value::as_arr).unwrap_or_default() {
                if let Some(name) = change.get("name").and_then(Value::as_str) { names.insert(name.to_owned()); }
            }
        }
        Ok(names.into_iter().collect())
    }

    pub fn detail(&mut self, key: Key) -> io::Result<Answer> {
        self.store.detail(self.root, self.target, key, &self.current)
    }
}

type Query = Box<dyn FnOnce(io::Result<Browse<'_>>) + Send>;
struct Command { watch: Watch, request: Option<Request>, query: Option<Query> }

struct Worker {
    root: PathBuf,
    commands: mpsc::Receiver<Command>,
    generation: Arc<AtomicU64>,
    selected: Arc<Mutex<Option<Watch>>>,
}

impl Worker {
    fn run(self) {
        self.serve(|generation| self.generation.load(Ordering::Acquire) == generation)
    }

    fn serve(&self, scan_current: impl Fn(u64) -> bool) {
        let mut store = Store::default();
        let mut watching: Option<Watch> = None;
        let mut last_error = None;
        let mut health = super::Health::Ok;
        loop {
            // Requests refresh immediately. An idle watch also discovers peer appends.
            let command = match self.commands.recv_timeout(Duration::from_millis(250)) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => None,
            };
            let mut request = None;
            let mut opened = false;
            let mut query = None;
            if let Some(command) = command {
                if self.generation.load(Ordering::Acquire) == command.watch.target.generation {
                    query = command.query;
                    opened = command.request.is_none() && query.is_none();
                    watching = Some(command.watch);
                    request = command.request;
                } else {
                    // A superseded command can occupy the only slot during a switch. Scan the
                    // selected plate now so its first real delivery also services any queued work.
                    watching = self.selected.lock().unwrap().clone();
                    opened = true;
                }
            }
            let Some(watch) = &watching else { continue };
            let active = || self.generation.load(Ordering::Acquire) == watch.target.generation;
            if !active() { continue }
            let requested = request.is_some();
            let dir = self.root.join(&*watch.target.folder).join("journal");
            let observed = live::writer_progress(&dir).map_or(super::Health::Ok, |p| p.health());
            if observed != health || (opened && observed != super::Health::Ok) {
                watch.reply(Answer::Health(observed));
            }
            health = observed;
            let mut refresh = || store.refresh(&self.root, &watch.target, &|| scan_current(watch.target.generation));
            let result = (if watch.unsaved {
                sessions::eligible(&self.root, &watch.target.folder).and_then(|ok| {
                    if ok { refresh() } else { Err(io::ErrorKind::NotFound.into()) }
                })
            } else { refresh() }).and_then(|changed| {
                if !active() { return Ok(None) }
                if changed == Refresh::Replaced { watch.reply(Answer::Replaced); }
                else if changed == Refresh::Changed || opened || last_error.is_some() { watch.reply(Answer::Changed); }
                if let Some(f) = query.take() {
                    f(Ok(Browse { root: &self.root, target: &watch.target, store: &mut store, current: &active }));
                    return Ok(None)
                }
                let Some(index) = store.folders.get(&watch.target.folder) else { return Ok(Some(Answer::NotFound)) };
                let answer = match request {
                    None => return Ok(None),
                    Some(Request::Count(Filter { actor: None, kind: None })) => Answer::Count(index.entries.len()),
                    Some(Request::Count(filter)) => Answer::Count(index.entries.values().take_while(|_| active())
                        .filter(|entry| index.matches(entry, filter)).count()),
                    Some(Request::Page { filter, anchor, size }) => index.page(&self.root, &watch.target, filter, anchor, true, size, &active)?,
                    Some(Request::Detail(key)) => store.detail(&self.root, &watch.target, key, &active)?,
                };
                Ok(Some(answer))
            });
            if !active() { continue }
            match result {
                Ok(answer) => { last_error = None; if let Some(a) = answer { watch.reply(a); } }
                Err(e) => {
                    if let Some(f) = query { f(Err(e.kind().into())); }
                    if opened || requested || last_error != Some(e.kind()) { watch.reply(Answer::Error(e.kind())); }
                    last_error = Some(e.kind());
                }
            }
        }
    }
}

/// One long-lived worker. Selecting a plate (or changing the filter generation) replaces
/// the reply channel, so even an already delivered old page cannot enter the new view.
pub struct Reader {
    commands: mpsc::SyncSender<Command>,
    waiting: VecDeque<Command>,
    reported: bool,
    generation: Arc<AtomicU64>,
    selected: Arc<Mutex<Option<Watch>>>,
    watch: Option<Watch>,
    replies: mpsc::Receiver<Reply>,
}

impl Reader {
    fn channel(root: PathBuf) -> (Self, Worker) {
        let (commands, inbox) = mpsc::sync_channel(1);
        let generation = Arc::new(AtomicU64::new(0));
        let selected = Arc::new(Mutex::new(None));
        let (_, replies) = mpsc::channel();
        (Self { commands, generation: generation.clone(), selected: selected.clone(), waiting: VecDeque::new(), reported: false, watch: None, replies },
            Worker { root, commands: inbox, generation, selected })
    }

    pub fn start(spawner: &ThreadSpawner, plates_root: PathBuf) -> Result<Self, SpawnError> {
        let (reader, worker) = Self::channel(plates_root);
        spawner.spawn_worker(ThreadOptions { name: Some("Fab journal reader".into()), ..Default::default() }, move || worker.run())?.detach();
        Ok(reader)
    }

    pub fn select(&mut self, folder: &str) -> io::Result<Target> { self.watch_folder(folder, false) }

    pub(crate) fn select_session(&mut self, folder: &str) -> io::Result<Target> { self.watch_folder(folder, true) }

    fn watch_folder(&mut self, folder: &str, unsaved: bool) -> io::Result<Target> {
        if !literal(folder) { return Err(invalid()) }
        let target = Target { folder: Arc::from(folder), generation: self.generation.fetch_add(1, Ordering::AcqRel) + 1 };
        let (tx, rx) = mpsc::channel();
        self.replies = rx;
        self.waiting.clear();
        self.reported = false;
        let watch = Watch { target: target.clone(), replies: tx, unsaved };
        self.watch = Some(watch.clone());
        *self.selected.lock().unwrap() = self.watch.clone();
        // A full slot holds an older generation. Its receiver picks up this watch instead.
        let command = Command { watch, request: None, query: None };
        if let Err(mpsc::TrySendError::Disconnected(_)) = self.commands.try_send(command) {
            return Err(io::ErrorKind::BrokenPipe.into())
        }
        Ok(target)
    }

    pub fn request(&mut self, request: Request) -> io::Result<()> { self.enqueue(Some(request), None) }

    pub(crate) fn list_sessions(&mut self, root: PathBuf, deliver: impl FnOnce(io::Result<Vec<sessions::Session>>) + Send + 'static) -> io::Result<()> {
        let target = self.watch.as_ref().ok_or_else(invalid)?.target.clone();
        let generation = self.generation.clone();
        self.query(move |_| deliver(sessions::list(&root, &target.folder, &|| generation.load(Ordering::Acquire) == target.generation)))
    }

    pub(crate) fn query(&mut self, run: impl FnOnce(io::Result<Browse<'_>>) + Send + 'static) -> io::Result<()> {
        self.enqueue(None, Some(Box::new(run)))
    }

    fn enqueue(&mut self, request: Option<Request>, query: Option<Query>) -> io::Result<()> {
        let Some(watch) = &self.watch else { return Err(invalid()) };
        self.waiting.push_back(Command { watch: watch.clone(), request, query });
        self.flush()
    }

    fn flush(&mut self) -> io::Result<()> {
        while let Some(command) = self.waiting.pop_front() {
            match self.commands.try_send(command) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(c)) => {
                    self.waiting.push_front(c);
                    if !self.reported {
                        self.reported = true;
                        makepad_widgets::log!("journal reader: {} requests waiting", self.waiting.len());
                    }
                    return Ok(())
                }
                Err(mpsc::TrySendError::Disconnected(_)) => return Err(io::ErrorKind::BrokenPipe.into()),
            }
        }
        self.reported = false;
        Ok(())
    }

    pub fn try_recv(&mut self) -> io::Result<Option<Reply>> {
        self.flush()?;
        Ok(self.replies.try_recv().ok())
    }
}

impl Drop for Reader {
    fn drop(&mut self) { self.generation.fetch_add(1, Ordering::Release); }
}

#[cfg(test)]
pub(crate) fn test_token(root: &Path, folder: &str) -> u64 {
    let mut index = Index::default();
    index.refresh(root, folder, &|| true).unwrap();
    index.token().unwrap()
}

#[cfg(test)]
#[path = "reader_tests.rs"]
pub(super) mod tests;
