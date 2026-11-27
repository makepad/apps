//! The private plate journal. Producers number records; only the writer appends them.
//! A full mailbox retains its overflow until another send or a writer drain.
use makepad_core_util::sha256::sha256_hex;
use makepad_strict_json::{obj, s, Value};
use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{atomic::{AtomicU64, AtomicBool, Ordering}, mpsc::{self, Receiver, SyncSender, TrySendError}, Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod loss;
mod live;
mod presence;
pub(crate) mod remove;
pub(crate) use live::recover_loss_notes;
pub(crate) use loss::finish_delete as finish_delete_notes;
pub(crate) use live::{arguments, Binding, Call, Electronics, Plate, Quit, Work};
pub use live::{pause_plate, writer_progress};
pub(crate) use live::release_presence;
pub(crate) use presence::other_presence_holders;
pub(crate) use live::{watch_settlement, SettleNotice};
#[cfg(test)]
pub(crate) use live::text_args;
mod mechanical;
pub(crate) use mechanical::{ModelJournal, PersonReply};
pub mod replay;
mod rebuild;
pub use rebuild::rebuild;
pub mod reader;

type Files = BTreeMap<String, Arc<[u8]>>;
type Count = Arc<dyn Fn(usize) + Send + Sync>;
const LIFE_BITS: u32 = 40;
const LAST_LIFE: u64 = (i64::MAX as u64) >> LIFE_BITS;
const LIFE_MASK: u64 = (1 << LIFE_BITS) - 1;
const LIFETIME: &str = "journal.lock";
const UNLOCKED: &str = "journal.unlocked";
const LOCK: &str = "records.lock";
#[cfg(test)]
pub(crate) fn at(life: u64, n: u64) -> u64 { (life << LIFE_BITS) + n }
const LOG: &str = "records.jsonl";
const INCARNATION: &str = "incarnation";
const REPAIR: &str = "torn.pending";
const RECORD_DEPTH: u32 = makepad_strict_json::TOOL_JSON_DEPTH + 4;

fn invalid() -> io::Error { io::Error::from(io::ErrorKind::InvalidData) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Incarnation(u128);

impl Incarnation {
    fn mint() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let allocation = Box::new(0u8);
        let seed = format!("{nanos}:{}:{serial}:{:p}", std::process::id(), &*allocation);
        let digest = makepad_core_util::sha256::sha256(seed.as_bytes());
        Self(u128::from_be_bytes(digest[..16].try_into().unwrap()))
    }

    fn parse(text: &str) -> Option<Self> {
        if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return None }
        u128::from_str_radix(text, 16).ok().map(Self)
    }

    fn read(mut file: &File) -> io::Result<Option<Self>> {
        if file.metadata()?.len() != 32 { return Ok(None) }
        let mut bytes = [0; 32];
        file.read_exact(&mut bytes)?;
        Ok(std::str::from_utf8(&bytes).ok().and_then(Self::parse))
    }
}

impl std::fmt::Display for Incarnation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{:032x}", self.0) }
}

// Windows needs write access and directory semantics to flush a newly published name.
fn sync_parent(dir: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use makepad_windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        OpenOptions::new().write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0).open(dir)?.sync_all()?;
    }
    loss::sync_dir(dir)
}

/// Opens a regular file for reading without following a symlink at its final component.
pub(super) fn open_regular(path: &Path) -> io::Result<Option<File>> {
    open_regular_with(path, OpenOptions::new().read(true))
}

fn open_regular_with(path: &Path, options: &mut OpenOptions) -> io::Result<Option<File>> {
    Ok(open_regular_with_metadata(path, options)?.map(|(file, _)| file))
}

fn open_regular_with_metadata(path: &Path, options: &mut OpenOptions) -> io::Result<Option<(File, fs::Metadata)>> {
    let before = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Err(invalid()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let opened = file.metadata()?;
    if !opened.is_file() { return Err(invalid()) }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (before.dev(), before.ino()) != (opened.dev(), opened.ino()) { return Err(invalid()) }
    }
    #[cfg(not(unix))]
    let _ = before;
    Ok(Some((file, opened)))
}

#[cfg(windows)]
fn file_id(file: &File) -> io::Result<(u32, u64)> {
    use std::os::windows::io::AsRawHandle;
    use makepad_windows_sys::Win32::{
        Foundation::HANDLE, Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|_| io::Error::last_os_error())?;
    Ok((info.dwVolumeSerialNumber, (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileId(u64, u64);

impl FileId {
    #[cfg(unix)]
    fn from_metadata(meta: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self(meta.dev() as u64, meta.ino())
    }

    fn of(file: &File) -> io::Result<Self> {
        #[cfg(unix)]
        { Ok(Self::from_metadata(&file.metadata()?)) }
        #[cfg(windows)]
        { let (volume, index) = file_id(file)?; Ok(Self(volume.into(), index)) }
        // Fab journals run on Unix and Windows.
        #[cfg(not(any(unix, windows)))]
        { let _ = file; Ok(Self(0, 0)) }
    }

    fn matches_path(self, path: &Path) -> io::Result<Option<bool>> {
        #[cfg(unix)]
        {
            let named = fs::symlink_metadata(path)?;
            Ok(Some(named.is_file() && self == Self::from_metadata(&named)))
        }
        #[cfg(windows)]
        {
            let named = open_regular(path)?.ok_or(io::ErrorKind::NotFound)?;
            Ok(Some(self == Self::of(&named)?))
        }
        #[cfg(not(any(unix, windows)))]
        { let _ = path; Ok(None) }
    }
}

fn same_file(file: &File, path: &Path) -> io::Result<Option<bool>> {
    FileId::of(file)?.matches_path(path)
}

fn require_same(file: &File, path: &Path) -> io::Result<()> {
    if same_file(file, path)? == Some(false) { return Err(invalid()) }
    Ok(())
}

/// Saved plates may acquire their first lifetime lock at Delete. Never
/// follow an existing sidecar link or create a missing plate directory.
pub(crate) fn delete_lock(plate: &Path) -> io::Result<File> {
    let path = plate.join(LIFETIME);
    let file = match OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            open_regular_with(&path, OpenOptions::new().read(true).write(true))?.ok_or(io::ErrorKind::NotFound)?
        }
        Err(e) => return Err(e),
    };
    file.try_lock()?;
    require_same(&file, &path)?;
    Ok(file)
}

fn create_directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => loss::sync_dir(path.parent().ok_or_else(invalid)?),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && fs::symlink_metadata(path)?.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

fn number(n: u64) -> Value { Value::Int(i64::try_from(n).expect("journal order fits in JSON")) }
fn time_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }

pub(crate) enum Record {
    Checkpoint { manifest: Option<String>, files: Arc<Files> },
    Call { reference: String, actor: Value, action: String, args: Value, replay: bool },
    Done { call: u64, outcome: &'static str, reply: String, seqs: Vec<u64>, elsewhere: BTreeMap<String, Vec<u64>> },
    Effect { call: Option<u64>, foreign: Option<(String, String)>, seq: u64, actor: Value, label: String, action: Option<Value>, changes: Vec<Change> },
    History { undo: bool, before: Option<u64>, after: Option<u64>, changes: Vec<Change> },
    Reload(Vec<Change>),
}

/// UTF-8 edits replace byte ranges in the document *before* this record. A whole new or
/// reloaded file uses its content blob. A missing after-hash removes a file.
pub(crate) struct Change {
    name: String,
    before: Option<String>,
    after: Option<String>,
    edits: Vec<(usize, usize, String)>,
    blob: Option<Arc<[u8]>>,
}

impl Change {
    fn text(name: String, old: &str, new: &str) -> Self {
        let patch = fab_elec_design::TextEdit::trimmed(fab_elec_core::DocId(0), old, new);
        Self { name, before: Some(sha256_hex(old.as_bytes())), after: Some(sha256_hex(new.as_bytes())),
            edits: vec![(patch.span.start as usize, patch.span.end as usize, patch.text)], blob: None }
    }

    fn whole(name: String, before: Option<&[u8]>, after: Option<Arc<[u8]>>) -> Self {
        Self { name, before: before.map(sha256_hex), after: after.as_deref().map(sha256_hex), edits: vec![], blob: after }
    }
    fn json(&self, dir: &Path) -> io::Result<Value> {
        if let (Some(hash), Some(bytes)) = (&self.after, &self.blob) { put_blob(dir, hash, bytes)?; }
        Ok(obj(vec![("name", s(&self.name)), ("before", self.before.as_ref().map_or(Value::Null, s)),
            ("after", self.after.as_ref().map_or(Value::Null, s)), ("blob", Value::Bool(self.blob.is_some())),
            ("edits", Value::Arr(self.edits.iter().map(|(start, end, text)| {
                obj(vec![("start", number(*start as u64)), ("end", number(*end as u64)), ("text", s(text))])
            }).collect()))]))
    }
}

#[derive(Clone, Copy)]
struct Reservation { order: u64, time_ms: u64 }

struct Ordered { reservation: Reservation, checkpoint: Option<u64>, record: Record }

impl Ordered {
    #[cfg(test)]
    fn json(&self, dir: &Path, prefix: u64) -> io::Result<Value> {
        self.json_mapped(dir, |local| prefix.checked_add(local))
    }

    fn json_mapped(&self, dir: &Path, map: impl Fn(u64) -> Option<u64>) -> io::Result<Value> {
        let absolute = |n| map(n).and_then(|v| i64::try_from(v).ok()).map(Value::Int).ok_or_else(invalid);
        let changes = |items: &[Change]| -> io::Result<Value> {
            Ok(Value::Arr(items.iter().map(|item| item.json(dir)).collect::<io::Result<_>>()?))
        };
        let (kind, mut fields) = match &self.record {
            Record::Checkpoint { manifest, files } => {
                let mut names = Vec::with_capacity(files.len());
                for (name, bytes) in files.iter() {
                    let hash = sha256_hex(bytes);
                    put_blob(dir, &hash, bytes)?;
                    names.push(obj(vec![("name", s(name)), ("hash", s(hash))]));
                }
                let mut fields = vec![("files", Value::Arr(names))];
                if let Some(name) = manifest { fields.push(("manifest", s(name))); }
                ("checkpoint", fields)
            }
            Record::Call { reference, actor, action, args, .. } => ("call", vec![("id", absolute(self.reservation.order)?), ("ref", s(reference)),
                ("actor", actor.clone()), ("action", s(action)), ("args", args.clone())]),
            Record::Done { call, outcome, reply, seqs, .. } => ("done", vec![("id", absolute(*call)?), ("outcome", s(*outcome)),
                ("reply", s(reply)), ("seqs", Value::Arr(seqs.iter().copied().map(number).collect()))]),
            Record::Effect { call, seq, actor, label, action, changes: docs, .. } => ("effect", vec![("call", call.map(absolute).transpose()?.unwrap_or(Value::Null)), ("seq", number(*seq)),
                ("actor", actor.clone()), ("label", s(label)), ("action", action.clone().unwrap_or(Value::Null)), ("changes", changes(docs)?)]),
            Record::History { undo, before, after, changes: docs } => ("history", vec![("direction", s(if *undo { "undo" } else { "redo" })),
                ("before", before.map_or(Value::Null, number)), ("after", after.map_or(Value::Null, number)), ("changes", changes(docs)?)]),
            Record::Reload(docs) => ("reload", vec![("changes", changes(docs)?)]),
        };
        match &self.record {
            Record::Call { replay: false, .. } => fields.push(("replay", Value::Bool(false))),
            Record::Effect { foreign: Some((reference, journal)), .. } => {
                fields.push(("call_ref", s(reference)));
                fields.push(("call_journal", s(journal)));
            }
            Record::Done { elsewhere, .. } if !elsewhere.is_empty() => {
                fields.push(("elsewhere", Value::Arr(elsewhere.iter().map(|(journal, seqs)| obj(vec![
                    ("journal", s(journal)), ("seqs", Value::Arr(seqs.iter().copied().map(number).collect())),
                ])).collect())));
            }
            _ => {}
        }
        if let Some(checkpoint) = self.checkpoint { fields.push(("checkpoint", absolute(checkpoint)?)); }
        fields.push(("time_ms", number(self.reservation.time_ms)));
        fields.push(("kind", s(kind)));
        fields.push(("order", absolute(self.reservation.order)?));
        Ok(obj(fields))
    }
}

fn put_blob(dir: &Path, hash: &str, bytes: &[u8]) -> io::Result<()> {
    let path = dir.join("blobs").join(hash);
    if path.try_exists()? { return Ok(()); }
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_data()?;
    #[cfg(test)]
    tests::probe("blob synced")?;
    require_same(&file, &tmp)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
pub(crate) fn read_rows(dir: &Path) -> io::Result<Vec<Value>> {
    let mut rows = Vec::new();
    for line in BufReader::new(File::open(dir.join(LOG))?).lines() {
        rows.push(makepad_strict_json::parse(line?.as_bytes()).map_err(|_| invalid())?);
    }
    Ok(rows)
}

struct Backlog {
    records: VecDeque<Ordered>,
    reported: bool,
}

struct Sender {
    tx: SyncSender<Ordered>,
    backlog: Arc<Mutex<Backlog>>,
    count: Count,
}

impl Sender {
    fn push(&self, record: Ordered) {
        let mut backlog = self.backlog.lock().unwrap();
        backlog.records.push_back(record);
        let mut report = None;
        while let Some(row) = backlog.records.pop_front() {
            if let Err(TrySendError::Full(row) | TrySendError::Disconnected(row)) = self.tx.try_send(row) {
                backlog.records.push_front(row);
                if !backlog.reported {
                    backlog.reported = true;
                    report = Some(backlog.records.len());
                }
                break;
            }
        }
        drop(backlog);
        if let Some(n) = report { (self.count)(n); }
    }
}

#[derive(Clone)]
pub(crate) struct Journal {
    progress: Arc<Progress>,
    counter: Arc<AtomicU64>,
    sender: Arc<Sender>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health { Ok, Failing(Failure) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure { Io(io::ErrorKind), NoFreeLives, IncarnationChanged }

impl From<io::ErrorKind> for Failure {
    fn from(kind: io::ErrorKind) -> Self { Self::Io(kind) }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "{kind:?}"),
            Self::NoFreeLives => f.write_str("this journal has no free writer lives"),
            Self::IncarnationChanged => f.write_str("this writer's journal incarnation changed"),
        }
    }
}
impl std::error::Error for Failure {}

#[derive(Clone, Copy)]
struct Segment {
    first: u64, // first producer local
    last: u64,  // inclusive capacity
    base: u64,  // absolute order of first
    spent: u64, // acknowledged producer locals and locals reserved for repair
}

#[derive(Default, Clone)]
struct Segments(Vec<Segment>);

impl Segments {
    fn index(&self, local: u64) -> Option<usize> {
        let i = self.0.partition_point(|s| s.first <= local).checked_sub(1)?;
        (local <= self.0[i].last).then_some(i)
    }

    fn absolute(&self, local: u64) -> Option<u64> {
        let s = self.0[self.index(local)?];
        Some(s.base + (local - s.first))
    }

    fn through(&self, order: u64) -> Option<u64> {
        let i = self.0.partition_point(|s| s.base <= order).checked_sub(1)?;
        let s = self.0[i];
        // Startup markers alone can fill the first life before any producer local fits.
        Some(s.first + (order - s.base).min(s.last.checked_sub(s.first)?))
    }
}

#[derive(Default)]
struct Orders {
    outstanding: BTreeSet<u64>,
    waiters: Vec<live::SettleWaiter>,
    segments: Arc<Segments>,
    incarnation: Option<Incarnation>,
    cut_without_prefix: bool,
    quit_cut: Option<(u64, Arc<loss::Attempt>)>,
}

pub struct Progress {
    // No I/O under this lock. Reservations, durable acknowledgements and the
    // forced-quit cut share one ordering, even when records arrive out of order.
    orders: Mutex<Orders>,
    pending: AtomicU64,
    write_error: AtomicBool,
    health: Mutex<Health>,
    // Before startup's life claim, every byte already on disk belongs to a previous writer.
    durable_end: AtomicU64,
    lock_waits: AtomicU64,
}

impl Progress {
    pub fn pending(&self) -> u64 { self.pending.load(Ordering::Acquire) }
    pub fn write_error(&self) -> bool { self.write_error.load(Ordering::Acquire) }
    pub fn health(&self) -> Health {
        *self.health.lock().unwrap()
    }
}

struct Lifetime {
    file: File,
    name: &'static str,
}

struct Handles {
    row: Arc<File>,
    log: File,
}

struct RowLock {
    file: Arc<File>,
    locked: bool,
}

impl Drop for RowLock {
    fn drop(&mut self) {
        if self.locked { let _ = self.file.unlock(); }
    }
}

struct Writer {
    lifetime: RefCell<Option<Lifetime>>,
    handles: OnceCell<Handles>,
    opened: Cell<Option<FileId>>,
    gate: Option<Arc<live::Gate>>,
    // Drop after the file handles, so an acknowledged pause can rename on Windows.
    recovery_writer: OnceCell<live::RecoveryWriter>,
    create_plate: bool,
    marker_pending: Cell<bool>,
    counter: Arc<AtomicU64>,
    block_bits: u32,
    life: Cell<Option<u64>>,
    incarnation_changed: Cell<bool>,
    warned_incarnation: Cell<bool>,
    warned_life: Cell<bool>,
    warned_lock: Cell<bool>,
    // A failed rollback's kind, reported with the append's own failure.
    rollback_failed: Cell<Option<io::ErrorKind>>,
    warned_rollback: Cell<bool>,
    pending_bases: RefCell<VecDeque<Rollover>>,
    torn_local: Cell<Option<u64>>,
    progress: Arc<Progress>,
    // The configured settings root for fallback loss notes on a store-backed writer.
    settings: Option<PathBuf>,
    dir: PathBuf,
    rx: Receiver<Ordered>,
    backlog: Arc<Mutex<Backlog>>,
}

#[derive(Default)]
struct LogTail {
    highest: u64,
    mine: u64,
    after: Option<u64>,
    fragment: Option<(u64, u64)>,
    end: u64,
}

// A claimed rollover survives I/O retries, including a failed blob or base append.
struct Rollover {
    through: u64,
    order: u64,
    snapshot: Option<replay::Snapshot>,
}

/// Undo a failed append. On Windows an append-only handle cannot change the
/// file's length, so the rollback opens its own write handle by path.
fn roll_back(held: &File, path: &Path, at: u64) -> io::Result<()> {
    #[cfg(windows)]
    {
        let write = OpenOptions::new().write(true).open(path)?;
        if file_id(&write)? != file_id(held)? { return Err(invalid()) }
        write.set_len(at)
    }
    #[cfg(not(windows))]
    { let _ = path; held.set_len(at) }
}

/// Original byte range of a torn line, including one that lost only its newline.
fn torn_range(row: &Value) -> Option<std::ops::Range<u64>> {
    if row.get("kind")?.as_str()? != "lost" || row.get("cause")?.as_str()? != "torn" { return None }
    let at = row.get("at")?.as_u64()?;
    let bytes = row.get("bytes")?.as_u64()?;
    Some(at..at.checked_add(bytes)?)
}

fn torn_at(row: &Value, offset: u64) -> Option<u64> {
    let range = torn_range(row)?;
    (range.end.checked_add(1)? == offset).then_some(range.start)
}

/// Stage the marker before terminating the original tail. If the process exits after only
/// the separator (or part of the marker), a restart can finish precisely that append.
struct Repair { end: u64, marker: Vec<u8> }

impl Repair {
    fn pending(dir: &Path) -> io::Result<Option<Self>> {
        let path = dir.join(REPAIR);
        let Some(mut file) = open_regular(&path)? else { return Ok(None) };
        let mut marker = Vec::new();
        file.read_to_end(&mut marker)?;
        let row = reader::parse(&marker)?;
        let range = torn_range(&row).ok_or_else(invalid)?;
        if row.get("order").and_then(Value::as_u64).is_none() || marker.contains(&b'\n') { return Err(invalid()) }
        Ok(Some(Self { end: range.end, marker }))
    }

    fn stage(&self, dir: &Path) -> io::Result<()> {
        let tmp = dir.join("torn.tmp");
        match fs::remove_file(&tmp) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
        file.write_all(&self.marker)?;
        file.sync_all()?;
        drop(file);
        fs::rename(tmp, dir.join(REPAIR))
    }

    fn finish(&self, writer: &Writer) -> io::Result<()> {
        #[cfg(unix)]
        File::open(&writer.dir)?.sync_all()?;
        let mut file = writer.log_file()?;
        let suffix = [b"\n", self.marker.as_slice(), b"\n"].concat();
        let present = file.metadata()?.len().checked_sub(self.end).ok_or_else(invalid)?.min(suffix.len() as u64) as usize;
        writer.progress.durable_end.store(self.end, Ordering::Release);
        file.seek(SeekFrom::Start(self.end))?;
        let mut existing = vec![0; present];
        file.read_exact(&mut existing)?;
        if existing != suffix[..present] { return Err(invalid()) }
        writer.check_log()?;
        file.write_all(&suffix[present..])?;
        file.sync_data()?;
        #[cfg(test)]
        tests::probe("repair synced")?;
        writer.check_log()?;
        fs::remove_file(writer.dir.join(REPAIR))?;
        writer.progress.durable_end.store(file.metadata()?.len(), Ordering::Release);
        Ok(())
    }
}

/// Visit complete records in file order, retaining one line so a torn marker can disown it.
/// Unreadable records have an offset and no value. An unterminated tail is returned separately.
fn scan(input: impl Read, decode: fn(&[u8]) -> io::Result<Value>, mut visit: impl FnMut(u64, Option<Value>)) -> io::Result<Option<(u64, u64)>> {
    let mut input = BufReader::new(input);
    let mut index = reader::Index::default();
    let mut finish = |at, row: Option<Value>| visit(at, row.filter(|r| index.index_header(at, r)));
    let mut line = Vec::new();
    let mut at = 0;
    let mut previous = None;
    loop {
        line.clear();
        let n = input.read_until(b'\n', &mut line)? as u64;
        if line.last() != Some(&b'\n') {
            if let Some((offset, row)) = previous { finish(offset, row); }
            return Ok((n != 0).then_some((at, n)))
        }
        let row = decode(&line).ok().filter(|v|
            v.get("order").and_then(Value::as_u64).is_some() && v.get("kind").and_then(Value::as_str).is_some());
        if let Some((offset, value)) = previous {
            let torn = row.as_ref().and_then(|r| torn_at(r, at)) == Some(offset);
            finish(offset, if torn { None } else { value });
        }
        previous = Some((at, row));
        at += n;
    }
}

impl Journal {
    fn channel(dir: PathBuf, counter: Arc<AtomicU64>, capacity: usize, count: Count) -> (Self, Writer) {
        let (tx, rx) = mpsc::sync_channel(capacity);
        let backlog = Arc::new(Mutex::new(Backlog { records: VecDeque::new(), reported: false }));
        let progress = Arc::new(Progress { orders: Mutex::default(), pending: 0.into(), write_error: false.into(), health: Mutex::new(Health::Ok), lock_waits: 0.into(), durable_end: u64::MAX.into() });
        (Self { counter: counter.clone(), progress: progress.clone(), sender: Arc::new(Sender { tx, backlog: backlog.clone(), count }) }, Writer { dir, rx, backlog, progress, settings: None, counter,
            lifetime: RefCell::default(), handles: OnceCell::new(), opened: Cell::new(None), gate: None, recovery_writer: OnceCell::new(), create_plate: true,
            marker_pending: Cell::new(false),
            block_bits: LIFE_BITS, life: Cell::new(None), warned_life: Cell::new(false), warned_lock: Cell::new(false),
            incarnation_changed: Cell::new(false), warned_incarnation: Cell::new(false),
            rollback_failed: Cell::new(None), warned_rollback: Cell::new(false), pending_bases: RefCell::default(), torn_local: Cell::new(None) })
    }

    fn reserve(&self) -> Reservation {
        let mut orders = self.progress.orders.lock().unwrap();
        let order = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        orders.outstanding.insert(order);
        self.progress.pending.fetch_add(1, Ordering::Release);
        Reservation { order, time_ms: time_ms() }
    }

    fn send(&self, record: Record) -> u64 {
        let reservation = self.reserve();
        self.sender.push(Ordered { reservation, checkpoint: None, record });
        reservation.order
    }
}

impl Writer {
    fn close_handles(&mut self) {
        self.handles.take();
        self.lifetime.take();
    }

    fn report_failure(&self, error: &io::Error, log: &mut impl FnMut(String)) {
        if error.kind() == io::ErrorKind::Interrupted && self.gate.as_ref().is_some_and(|g| g.paused()) { return }
        if let Some(kind) = self.rollback_failed.take() {
            if !self.warned_rollback.replace(true) {
                log(format!("journal: {}: rollback failed: {kind:?}; the next append repairs the tail", self.dir.join(LOG).display()));
            }
        }
        let failure = error.get_ref().and_then(|cause| cause.downcast_ref::<Failure>()).copied()
            .unwrap_or(Failure::Io(error.kind()));
        let health = Health::Failing(failure);
        let previous = std::mem::replace(&mut *self.progress.health.lock().unwrap(), health);
        if !self.progress.write_error.swap(true, Ordering::AcqRel) || (!matches!(failure, Failure::Io(_)) && previous != health) {
            log(format!("journal: {}: {failure}", self.dir.join(LOG).display()));
        }
    }

    fn try_write(&self, row: &Ordered, mut log: impl FnMut(String)) -> bool {
        if let Err(e) = self.append(row, &mut log) {
            self.report_failure(&e, &mut log);
            return false
        }
        let mut orders = self.progress.orders.lock().unwrap();
        if self.progress.write_error() {
            *self.progress.health.lock().unwrap() = Health::Ok;
            self.progress.write_error.store(false, Ordering::Release);
        }
        let local = row.reservation.order;
        let i = orders.segments.index(local).expect("the durable row belongs to a claimed life");
        Arc::make_mut(&mut orders.segments).0[i].spent += 1;
        orders.outstanding.remove(&local);
        self.progress.pending.fetch_sub(1, Ordering::Release);
        let notices = live::settled(&mut orders, row.reservation.order);
        drop(orders);
        for (notify, remaining) in notices { notify(remaining); }
        true
    }

    fn log_file(&self) -> io::Result<&File> {
        self.handles.get().map(|h| &h.log).ok_or_else(invalid)
    }

    fn check_log(&self) -> io::Result<()> {
        require_same(self.log_file()?, &self.dir.join(LOG))
    }

    fn mark_unlocked(&self, plate: &Path) -> io::Result<File> {
        let path = plate.join(UNLOCKED);
        let marker = OpenOptions::new().write(true).create(true).truncate(false).open(&path)?;
        require_same(&marker, &path)?;
        #[cfg(test)]
        tests::probe("marker sync")?;
        marker.sync_all()?;
        sync_parent(plate)?;
        Ok(marker)
    }

    fn lifetime(&self, plate: &Path, recovery: bool, log: &mut impl FnMut(String)) -> io::Result<bool> {
        if let Some(life) = self.lifetime.borrow().as_ref() {
            require_same(&life.file, &plate.join(life.name))?;
            return Ok(true)
        }
        match fs::symlink_metadata(plate) {
            Ok(meta) if meta.is_dir() => (),
            Ok(_) => return Err(invalid()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Another note may have arrived after discovery queued this writer.
                if recovery && (!self.create_plate || !loss::may_create_missing(plate, self.settings.as_deref())) {
                    loss::skipped(plate, log);
                    return Ok(false)
                }
                if !self.create_plate { return Err(e) }
                loss::ensure_dir(plate)?;
            }
            Err(e) => return Err(e),
        }
        let path = plate.join(LIFETIME);
        let saved = match fs::symlink_metadata(plate.join("plate")) {
            Ok(meta) => meta.is_file(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => false,
            Err(e) => return Err(e),
        };
        let legacy = !saved && self.dir.try_exists()?;
        let shared = || -> io::Result<File> {
            let file = OpenOptions::new().read(true).write(true).create(!legacy).truncate(false).open(&path)?;
            #[cfg(test)]
            tests::probe("shared try")?;
            file.try_lock_shared().map_err(io::Error::from)?;
            Ok(file)
        };
        let previous_marker = if self.marker_pending.get() {
            let file = self.mark_unlocked(plate)?;
            self.marker_pending.set(false);
            Some(file)
        } else { None };
        let life = match shared() {
            Ok(file) => Lifetime { file, name: LIFETIME },
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Err(e),
            // The loss-note writer never writes unlocked into a plate-less folder.
            Err(_) if recovery && !saved => return Ok(false),
            Err(_) => {
                // A plate-less unlocked write requires a durable marker before creating journal/.
                let marker = match previous_marker {
                    Some(file) => file,
                    None => {
                        self.marker_pending.set(true);
                        let file = self.mark_unlocked(plate)?;
                        self.marker_pending.set(false);
                        file
                    }
                };
                match shared() {
                    Ok(file) => Lifetime { file, name: LIFETIME },
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Err(e),
                    Err(_) => {
                        log(format!("journal: recording without a lifetime lock at {}", path.display()));
                        Lifetime { file: marker, name: UNLOCKED }
                    }
                }
            }
        };
        require_same(&life.file, &plate.join(life.name))?;
        self.lifetime.replace(Some(life));
        Ok(true)
    }

    fn row_lock(&self, file: Arc<File>, log: &mut impl FnMut(String)) -> io::Result<RowLock> {
        let try_lock = || {
            #[cfg(test)]
            tests::probe("row try").map_err(fs::TryLockError::Error)?;
            file.try_lock()
        };
        let result = match try_lock() {
            Ok(()) => Ok(()),
            Err(fs::TryLockError::WouldBlock) => {
                self.progress.lock_waits.fetch_add(1, Ordering::Release);
                if let Some(gate) = self.gate.as_ref().or_else(|| self.recovery_writer.get().map(|w| &w.0)) {
                    loop {
                        gate.wait_for_row()?;
                        match try_lock() {
                            Ok(()) => break Ok(()),
                            Err(fs::TryLockError::WouldBlock) => (),
                            Err(fs::TryLockError::Error(e)) => break Err(e),
                        }
                    }
                } else { file.lock() }
            }
            Err(fs::TryLockError::Error(e)) => Err(e),
        };
        let locked = result.is_ok();
        if let Err(e) = result {
            self.mark_unlocked(self.dir.parent().ok_or_else(invalid)?)?;
            if !self.warned_lock.replace(true) {
                log(format!("journal: {}: {:?}; recording without the append lock", self.dir.join(LOCK).display(), e.kind()));
            }
        }
        Ok(RowLock { file, locked })
    }

    fn enter(&self, recovery: bool, log: &mut impl FnMut(String)) -> io::Result<Option<RowLock>> {
        if self.incarnation_changed.get() { return Err(io::Error::other(Failure::IncarnationChanged)) }
        let plate = self.dir.parent().ok_or_else(invalid)?;
        if let Some(handles) = self.handles.get() {
            let lock = self.row_lock(handles.row.clone(), log)?;
            let decision = self.lifetime.borrow();
            let decision = decision.as_ref().ok_or_else(invalid)?;
            require_same(&decision.file, &plate.join(decision.name))?;
            require_same(&lock.file, &self.dir.join(LOCK))?;
            self.check_log()?;
            return Ok(Some(lock))
        }

        if !self.lifetime(plate, recovery, log)? { return Ok(None) }
        #[cfg(test)]
        tests::probe("before row lock")?;
        // Only prepare creates directories below <id>, one level at a time.
        create_directory(&self.dir)?;
        let row_path = self.dir.join(LOCK);
        let row = Arc::new(OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&row_path)?);
        let lock = self.row_lock(row.clone(), log)?;
        #[cfg(test)]
        tests::probe("acquired row lock")?;
        let verify = || {
            let life = self.lifetime.borrow();
            let life = life.as_ref().ok_or_else(invalid)?;
            require_same(&life.file, &plate.join(life.name))?;
            require_same(&row, &row_path)
        };
        let prove = |result: io::Result<()>| {
            if result.is_err() { self.lifetime.take(); }
            result
        };
        let prepare = || {
            prove(verify())?;
            create_directory(&self.dir.join("blobs"))?;
            let path = self.dir.join(LOG);
            if let Ok(meta) = fs::symlink_metadata(&path) {
                if !meta.is_file() { return Err(invalid()) }
            }
            let file = OpenOptions::new().read(true).append(true).create(self.opened.get().is_none()).open(&path)?;
            let id = FileId::of(&file)?;
            if self.opened.get().is_some_and(|original| original != id) {
                return Err(io::Error::other(Failure::IncarnationChanged))
            }
            prove(require_same(&file, &path))?;
            #[cfg(test)]
            tests::probe("validated log")?;
            prove(verify().and_then(|_| require_same(&file, &path)))?;
            Ok((file, id))
        };
        let (file, id) = prepare()?;
        self.opened.set(Some(id));
        self.handles.set(Handles { row, log: file }).map_err(|_| invalid())?;
        Ok(Some(lock))
    }

    fn append_lock(&self, log: &mut impl FnMut(String)) -> io::Result<RowLock> {
        self.enter(false, log)?.ok_or_else(invalid)
    }

    fn incarnation(&self, _lock: &RowLock, log: &mut impl FnMut(String)) -> io::Result<Incarnation> {
        let path = self.dir.join(INCARNATION);
        let bound = self.progress.orders.lock().unwrap().incarnation;
        // Windows requires write access for the initial flush. Subsequent identity checks only read.
        let file = open_regular_with(&path, OpenOptions::new().read(true).write(cfg!(windows) && bound.is_none()))?;
        let found = file.as_ref().map(Incarnation::read).transpose()?.flatten();
        if let Some(id) = bound {
            if found == bound { return Ok(id) }
            // Even restoring the old name or bytes cannot revive a displaced writer.
            self.incarnation_changed.set(true);
            return Err(io::Error::other(Failure::IncarnationChanged))
        }

        let id = match (file, found) {
            (Some(file), Some(id)) => {
                // A previous creator may have died after writing the complete id but before sync.
                file.sync_all()?;
                id
            }
            (file, _) => {
                // Without working locks, concurrent creators can replace a fresh id.
                // The displaced writer fails permanently with IncarnationChanged. Its loss
                // note accounts for unwritten rows, and replacing the id leaves the log intact.
                if let Some(invalid_file) = file {
                    drop(invalid_file);
                    if !self.warned_incarnation.replace(true) {
                        log(format!("journal: replacing invalid incarnation at {path:?}"));
                    }
                    fs::remove_file(&path)?;
                }
                let id = Incarnation::mint();
                let mut file = File::create_new(&path)?;
                file.write_all(id.to_string().as_bytes())?;
                file.sync_all()?;
                id
            }
        };
        #[cfg(test)]
        tests::probe("incarnation directory sync")?;
        sync_parent(&self.dir)?;
        self.progress.orders.lock().unwrap().incarnation = Some(id);
        Ok(id)
    }

    fn claim(&self, highest: u64, log: &mut impl FnMut(String)) -> io::Result<u64> {
        let path = self.dir.join("life");
        let previous = open_regular(&path).and_then(|file| {
            let Some(mut file) = file else { return Ok(0) };
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            text.trim().parse::<u64>().ok().filter(|n| *n <= LAST_LIFE).ok_or_else(invalid)
        }).unwrap_or_else(|e| {
            if e.kind() != io::ErrorKind::NotFound && !self.warned_life.replace(true) {
                log(format!("journal: {}: {:?}", path.display(), e.kind()));
            }
            0
        });
        let used = previous.max(highest >> LIFE_BITS);
        if used >= LAST_LIFE { return Err(io::Error::other(Failure::NoFreeLives)) }
        let life = used + 1;
        let tmp = path.with_extension("tmp");
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        File::create_new(&tmp)?.write_all(life.to_string().as_bytes())?;
        fs::rename(tmp, path)?;
        self.life.set(Some(life));
        Ok(life << LIFE_BITS)
    }

    fn capacity(&self) -> u64 { (1 << self.block_bits) - 1 }

    // Before publication all rows in this life are recovery markers. If they
    // fill it, there is no state checkpoint to carry into the next block.
    fn next_startup_order(&self, highest: &mut u64, log: &mut impl FnMut(String)) -> io::Result<u64> {
        if *highest & LIFE_MASK == self.capacity() { *highest = self.claim(*highest, log)?; }
        Ok(*highest + 1)
    }

    fn scan_log(&self, recovery: &mut loss::Recovery) -> io::Result<LogTail> {
        let mut state = LogTail::default();
        let mut file = self.log_file()?;
        file.rewind()?;
        state.end = file.metadata()?.len();
        state.fragment = scan(file, reader::header, |_, row| {
            if let Some((order, row)) = row.and_then(|r| Some((r.get("order")?.as_u64()?, r))) {
                recovery.observe(order, &row);
                state.highest = state.highest.max(order);
                if Some(order >> LIFE_BITS) == self.life.get() { state.mine = state.mine.max(order); }
                state.after = Some(order);
            }
        })?;
        Ok(state)
    }

    fn repair(&self, tail: (u64, u64), after: Option<u64>, order: u64, foreign: bool) -> io::Result<()> {
        let (at, bytes) = tail;
        let mut fields = vec![("kind", s("lost")), ("cause", s("torn")), ("time_ms", number(time_ms())),
            ("at", number(at)), ("bytes", number(bytes)), ("after", after.map_or(Value::Null, number)), ("order", number(order))];
        if foreign { fields.push(("foreign", Value::Bool(true))); }
        let repair = Repair { end: at + bytes, marker: obj(fields).to_json().into_bytes() };
        repair.stage(&self.dir)?;
        repair.finish(self)
    }

    fn prepare(&self, log: &mut impl FnMut(String)) -> io::Result<u64> {
        let lock = self.append_lock(log)?;
        self.prepare_locked(&lock, false, log).inspect_err(|e| self.report_failure(e, log))
    }

    fn recover_notes(&self, log: &mut impl FnMut(String)) -> io::Result<()> {
        if self.recovery_writer.get().is_none() {
            let permit = live::RecoveryWriter::enter(&self.dir)?;
            self.recovery_writer.set(permit).map_err(|_| invalid())?;
        }
        self.recover_unpaused(log)
    }

    fn recover_unpaused(&self, log: &mut impl FnMut(String)) -> io::Result<()> {
        // Discovery cannot create a lock through a linked store or plate.
        for dir in self.dir.ancestors().take(3) {
            match fs::symlink_metadata(dir) {
                Ok(m) if !m.is_dir() => return Err(invalid()),
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        let Some(lock) = self.enter(true, log)? else { return Ok(()) };
        self.prepare_locked(&lock, true, log).inspect_err(|e| self.report_failure(e, log)).map(|_| ())
    }

    fn prepare_locked(&self, lock: &RowLock, notes_only: bool, log: &mut impl FnMut(String)) -> io::Result<u64> {
        let claimed = {
            let orders = self.progress.orders.lock().unwrap();
            let base = orders.segments.0.first().map(|s| s.base - 1);
            if base.is_none() && orders.cut_without_prefix { return Err(io::ErrorKind::Interrupted.into()) }
            base
        };
        let incarnation = self.incarnation(lock, log)?;
        if let Some(base) = claimed { return Ok(base) }
        let mut recovery = loss::Recovery::load(&self.dir, incarnation, self.settings.as_deref(), log);
        if notes_only && recovery.is_empty() { return Ok(0) }
        // The forced-quit cut may have published this very writer's local note
        // during discovery. Leave it for the next writer.
        if self.progress.orders.lock().unwrap().cut_without_prefix { return Err(io::ErrorKind::Interrupted.into()) }
        if let Some(repair) = Repair::pending(&self.dir)? { repair.finish(self)?; }
        let scan = self.scan_log(&mut recovery).inspect_err(|_| recovery.warn_all(log))?;
        let mut highest = match self.life.get() {
            Some(life) => (life << LIFE_BITS).max(scan.mine),
            None => self.claim(scan.highest.max(recovery.highest()), log)?,
        };
        self.progress.durable_end.store(scan.end, Ordering::Release);
        if let Some(tail) = scan.fragment {
            highest = self.next_startup_order(&mut highest, log)?;
            self.repair(tail, scan.after, highest, false)?;
        }
        highest = recovery.finish(self, incarnation, highest, log);
        let mut orders = self.progress.orders.lock().unwrap();
        if orders.cut_without_prefix { return Err(io::ErrorKind::Interrupted.into()) }
        let capacity = self.capacity() - (highest & LIFE_MASK);
        Arc::make_mut(&mut orders.segments).0.push(Segment { first: 1, last: capacity, base: highest + 1, spent: 0 });
        Ok(highest)
    }

    fn tail_changed(&self) -> io::Result<bool> {
        let mut file = self.log_file()?;
        let len = file.metadata()?.len();
        if len == self.progress.durable_end.load(Ordering::Acquire) { return Ok(false) }
        if let Some(repair) = Repair::pending(&self.dir)? { repair.finish(self)?; return Ok(false) }
        if len == 0 { return Ok(false) }
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        file.read_exact(&mut last)?;
        Ok(last[0] != b'\n')
    }

    /// Keep a claimed base across retries. Reacquire the append lock before
    /// publishing anything; another process can append or tear a row during replay.
    fn rollover_base(&self, lock: &mut Option<RowLock>, log: &mut impl FnMut(String)) -> io::Result<()> {
        let mut pending = self.pending_bases.borrow_mut();
        let Some(base) = pending.back_mut().filter(|b| b.snapshot.is_none()) else { return Ok(()) };
        let mut prefix = self.log_file()?.try_clone()?;
        let end = prefix.metadata()?.len();
        prefix.rewind()?;
        drop(lock.take());
        let start = std::time::Instant::now();
        log(format!("journal: {}: rollover replay started through order {}", self.dir.display(), base.through));
        // A clone shares its cursor with the held file. This worker is its only user
        // during replay; every write uses append access, and every read seeks explicitly.
        #[cfg(test)]
        let mut prefix = tests::ReadingPrefix(prefix);
        let result = replay::snapshot(&self.dir, &mut prefix, base.through, end, base.order);
        let rows = result.as_ref().map_or(0, |s| s.rows);
        let outcome = match &result {
            Ok(s) if s.checkpoint.is_none() => "gap, no base",
            Err(_) => "read failed",
            _ => "complete",
        };
        log(format!("journal: {}: rollover replay ended ({rows} rows, {} ms, {outcome})", self.dir.display(), start.elapsed().as_millis()));
        let reacquired = self.append_lock(log)?;
        self.incarnation(&reacquired, log)?;
        *lock = Some(reacquired);
        base.snapshot = Some(result?);
        Ok(())
    }

    fn claim_segment(&self, previous: Segment, log: &mut impl FnMut(String)) -> io::Result<()> {
        let incarnation = {
            let orders = self.progress.orders.lock().unwrap();
            if orders.cut_without_prefix { return Err(io::ErrorKind::Interrupted.into()) }
            orders.incarnation.ok_or_else(invalid)?
        };
        let mut notes = loss::Recovery::load(&self.dir, incarnation, self.settings.as_deref(), log);
        let highest = self.scan_log(&mut notes)?.highest.max(notes.highest());
        let base = self.claim(highest, log)? + 1;
        let mut orders = self.progress.orders.lock().unwrap();
        if orders.cut_without_prefix { return Err(io::ErrorKind::Interrupted.into()) }
        let first = previous.last + 1;
        let last = first + self.capacity() - 2;
        let spent = u64::from(self.torn_local.get().is_some_and(|n| (first..=last).contains(&n)));
        Arc::make_mut(&mut orders.segments).0.push(Segment { first, last, base: base + 1, spent });
        let through = previous.base - 1 + (first - previous.first);
        self.pending_bases.borrow_mut().push_back(Rollover { through, order: base, snapshot: None });
        Ok(())
    }

    fn write_record(&self, row: &Ordered, absolute: impl Fn(u64) -> Option<u64>) -> io::Result<()> {
        let mut bytes = row.json_mapped(&self.dir, absolute)?.to_json().into_bytes();
        bytes.push(b'\n');
        self.append_bytes(&bytes)
    }

    fn append(&self, row: &Ordered, log: &mut impl FnMut(String)) -> io::Result<()> {
        let lock = self.append_lock(log)?;
        self.prepare_locked(&lock, false, log)?;
        let mut lock = Some(lock);
        // Another process may have completed our staged repair since the failed
        // append. Its order has already been spent, even if our row still needs a retry.
        if let Some(local) = self.torn_local.get() {
            if let Some(repair) = Repair::pending(&self.dir)? { repair.finish(self)?; }
            let absolute = self.progress.orders.lock().unwrap().segments.absolute(local);
            if absolute == Some(self.scan_log(&mut loss::Recovery::default())?.mine) { self.torn_local.set(None); }
        }
        loop {
            self.rollover_base(&mut lock, log)?;
            let torn = self.tail_changed()?;
            if let Some(local) = self.torn_local.get().filter(|_| !torn) {
                let mut orders = self.progress.orders.lock().unwrap();
                if self.counter.compare_exchange(local, local - 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                    if let Some(i) = orders.segments.index(local) { Arc::make_mut(&mut orders.segments).0[i].spent -= 1; }
                    self.torn_local.set(None);
                }
            }
            let segments = self.progress.orders.lock().unwrap().segments.clone();
            let last = *segments.0.last().ok_or_else(invalid)?;
            let marker = self.torn_local.get().or_else(|| torn.then(|| {
                let mut orders = self.progress.orders.lock().unwrap();
                let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
                if let Some(i) = orders.segments.index(n) { Arc::make_mut(&mut orders.segments).0[i].spent += 1; }
                self.torn_local.set(Some(n));
                n
            }));
            if row.reservation.order.max(marker.unwrap_or(0)) > last.last {
                self.claim_segment(last, log)?;
                continue
            }
            if let Some(local) = marker {
                let order = segments.absolute(local).ok_or_else(invalid)?;
                if torn {
                    let scan = self.scan_log(&mut loss::Recovery::default())?;
                    self.repair(scan.fragment.ok_or_else(invalid)?, scan.after, order, true)?;
                } else {
                    // A peer repaired the tail and later reservations pin this slot.
                    let mut bytes = obj(vec![("kind", s("lost")), ("cause", s("unwritten")), ("count", number(0)),
                        ("from", number(order)), ("to", number(order)), ("order", number(order)),
                        ("time_ms", number(time_ms()))]).to_json().into_bytes();
                    bytes.push(b'\n');
                    self.append_bytes(&bytes)?;
                }
                self.torn_local.set(None);
            }
            let mut pending = self.pending_bases.borrow_mut();
            while let Some(base) = pending.front() {
                if let Some(row) = base.snapshot.as_ref().and_then(|s| s.checkpoint.as_ref()) {
                    self.write_record(row, Some)?;
                }
                pending.pop_front();
            }
            return self.write_record(row, |n| segments.absolute(n))
        }
    }

    fn append_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        let path = self.dir.join(LOG);
        let mut file = self.log_file()?;
        let at = file.metadata()?.len();
        if self.progress.durable_end.load(Ordering::Relaxed) == u64::MAX {
            self.progress.durable_end.store(at, Ordering::Release);
        }
        // A writer appends only to the log it opened. This final identity check is
        // inside the row lock, immediately before the write, so Remove can freeze it.
        require_same(file, &path)?;
        if let Err(e) = file.write_all(bytes).and_then(|()| file.sync_data()) {
            // The caller still holds the append lock. The append's own error wins.
            if let Err(rollback) = roll_back(file, &path, at) { self.rollback_failed.set(Some(rollback.kind())); }
            return Err(e);
        }
        self.progress.durable_end.store(at + bytes.len() as u64, Ordering::Release);
        Ok(())
    }
    #[cfg(test)]
    fn run(self) {
        while let Ok(first) = self.rx.recv() { self.batch(first); }
    }
    #[cfg(test)]
    fn batch(&self, first: Ordered) {
        // Take ownership in constant time. A producer never waits for a backlog
        // to be copied, freed, serialized or written under its queue lock.
        let backlog = std::mem::take(&mut self.backlog.lock().unwrap().records);
        for row in std::iter::once(first).chain(backlog) {
            while !self.try_write(&row, |line| makepad_widgets::log!("{}", line)) {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        let mut backlog = self.backlog.lock().unwrap();
        if backlog.records.is_empty() { backlog.reported = false; }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use tests::{calls_fixture, temp as test_dir};
