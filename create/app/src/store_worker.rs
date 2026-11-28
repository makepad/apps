//! The home's single blob store owner. The UI submits file paths or captured PNGs and
//! receives blob descriptors. The link has its own bounded port so disk pressure cannot block Call or Sync.

use makepad_widgets::makepad_platform::thread::{Class, TaskPool};
use makepad_town_core::blob::{self, lane::StoreRequest, Hash, TownStore, Served, WriteError, CHUNK};
use makepad_town_core::event::{Id, TownId, SessionId};
use makepad_town_core::home::Home;
use makepad_town_core::key::PublicKey;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{mpsc::{self, Receiver, SyncSender, TrySendError}, Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(100);
/// How much of a dropped file is read to sniff its kind and a picture's size.
const HEAD: u64 = 1 << 20;
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredFile {
    pub size: u64,
    pub root: Hash,
    pub id: Id,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardMediaTarget {
    pub board: Id,
    /// Explicit top-left in 1/64 canvas units, or the next free spot at publication.
    pub pos: Option<(i64, i64)>,
}

#[derive(Clone, Debug)]
pub struct FileResult {
    pub town: TownId,
    /// The project a file belongs to. A picture or video dropped on the chat belongs to the town.
    pub project: Option<Id>,
    pub title: String,
    pub result: Result<StoredFile, &'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CaptureFormat { Mp4, Mov, Jpeg, Png, Gif }

impl CaptureFormat {
    pub(crate) fn extension(self) -> &'static str {
        match self { Self::Mp4 => "mp4", Self::Mov => "mov", Self::Jpeg => "jpg", Self::Png => "png", Self::Gif => "gif" }
    }

    pub(crate) fn media_kind(self) -> &'static str {
        match self {
            Self::Mp4 => "video/mp4", Self::Mov => "video/quicktime",
            Self::Jpeg => "image/jpeg", Self::Png => "image/png", Self::Gif => "image/gif",
        }
    }

    pub(crate) fn is_video(self) -> bool { matches!(self, Self::Mp4 | Self::Mov) }

    /// The kind of a picture or movie from its first bytes. The file's name never decides.
    pub(crate) fn sniff(head: &[u8]) -> Option<Self> {
        if head.starts_with(b"\x89PNG\r\n\x1a\n") { Some(Self::Png) }
        else if head.starts_with(&[0xFF, 0xD8, 0xFF]) { Some(Self::Jpeg) }
        else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") { Some(Self::Gif) }
        else if head.len() >= 12 && &head[4..8] == b"ftyp" {
            // Only named video brands count: a HEIC or AVIF photo or an M4A song shares the box.
            match &head[8..12] {
                b"qt  " => Some(Self::Mov),
                b"isom" | b"iso2" | b"iso4" | b"iso5" | b"iso6" | b"mp41" | b"mp42" | b"avc1" | b"dash" | b"M4V " => Some(Self::Mp4),
                _ => None,
            }
        } else { None }
    }
}

/// Where a clip's file came from. A recording sits in the recordings folder and is removed once
/// posted. A person's own file is read where it lies and never removed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ClipSource { Recording, Own }

pub(crate) const NOT_MEDIA: &str = "Only pictures and videos post to the chat.";

#[derive(Clone, Debug)]
pub(crate) struct ClipImport {
    pub source: ClipSource,
    pub format: CaptureFormat,
    pub session: Option<SessionId>,
    pub duration_ms: u64,
    pub poster: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct ClipResult {
    pub file: FileResult,
    pub board: Option<BoardMediaTarget>,
    pub source_peer: Option<PublicKey>,
    pub path: PathBuf,
    pub info: ClipImport,
    pub stored_poster: Option<StoredFile>,
}

#[derive(Debug, Clone, Copy)]
pub enum IssueMedia {
    Picture([StoredFile; 2]),
    Video { file: StoredFile, duration_ms: i64 },
}

pub type IssueReceipt = (u64, Result<IssueMedia, String>);

#[derive(Default, Debug)]
pub struct Snapshot {
    pub holdings: BTreeMap<(TownId, Id), bool>,
    pub files: Vec<FileResult>,
    pub(crate) issues: Vec<IssueReceipt>,
    pub(crate) clips: Vec<ClipResult>,
    pub(crate) recording_problems: Vec<String>,
    /// Some work on this device is waiting for storage. None means no change.
    pub waiting: Option<bool>,
    pub checked: Vec<TownId>,
}

impl Snapshot {
    fn changed(&self) -> bool {
        !self.holdings.is_empty() || !self.files.is_empty() || !self.clips.is_empty() || !self.issues.is_empty()
            || !self.recording_problems.is_empty() || self.waiting.is_some() || !self.checked.is_empty()
    }
}

#[derive(Debug)]
pub enum LinkWork {
    Begin { town: TownId, size: u64, root: Hash },
    Transfer(StoreRequest),
}

#[derive(Debug)]
pub enum LinkResult {
    Holdings(BTreeMap<(TownId, Id), bool>),
    Begun { town: TownId, file: StoredFile, absent: Vec<Range<u64>> },
    Read { carrier: PublicKey, town: TownId, id: Id, index: u64, result: Served, complete: bool },
    Written { town: TownId, id: Id, index: u64, complete: bool },
    Bad { carrier: PublicKey, town: TownId, id: Id, index: u64 },
}

/// One link worker’s port. Each admitted operation receives one FIFO reply, with
/// unsolicited holdings snapshots interleaved. The pump retains its originating attempt.
pub struct StorePort {
    to: SyncSender<LinkWork>,
    from: Receiver<LinkResult>,
    connected: Arc<AtomicBool>,
    worker: Thread,
}

impl StorePort {
    pub fn try_send(&self, work: LinkWork) -> Result<(), TrySendError<LinkWork>> {
        self.to.try_send(work)?;
        self.worker.unpark();
        Ok(())
    }

    pub fn try_recv(&self) -> Option<LinkResult> {
        let result = self.from.try_recv().ok()?;
        self.worker.unpark();
        Some(result)
    }
}

impl Drop for StorePort {
    fn drop(&mut self) {
        self.connected.store(false, Ordering::Release);
        self.worker.unpark();
    }
}

struct Link {
    connected: Arc<AtomicBool>,
    announce: BTreeMap<(TownId, Id), bool>,
    from: Receiver<LinkWork>,
    to: SyncSender<LinkResult>,
    wake: Box<dyn Fn() + Send>,
    retry: Option<LinkWork>,
    reply: Option<LinkResult>,
}

impl Link {
    fn pair(worker: Thread, wake: impl Fn() + Send + 'static) -> (StorePort, Self) {
        let (to, requests) = mpsc::sync_channel(2);
        let (results, from) = mpsc::sync_channel(2);
        let connected = Arc::new(AtomicBool::new(true));
        (StorePort { to, from, worker, connected: connected.clone() }, Self {
            connected, announce: BTreeMap::new(),
            from: requests, to: results, wake: Box::new(wake), retry: None, reply: None,
        })
    }
}

enum Command {
    Open(TownId),
    Add { town: TownId, project: Id, path: PathBuf, title: String, clip: Option<ClipImport> },
    Issue { draft: u64, town: TownId, png: Arc<Vec<u8>>, poster: Arc<Vec<u8>> },
    BoardSnap { town: TownId, board: Id, title: String, size: [u32; 2], session: SessionId,
        source_peer: Option<PublicKey>, png: Arc<Vec<u8>>, poster: Arc<Vec<u8>> },
    IssueVideo { draft: u64, town: TownId, recording: crate::app_link::report::Recording },
    AddOwn { sequence: usize, town: TownId, path: PathBuf, title: String, board: Option<BoardMediaTarget> },
    StorePoster(Box<Poster>),
    RemoveRecording { path: PathBuf, title: String, extension: &'static str },
    Attach(Link),
}

#[derive(Default)]
struct Batch {
    commands: Vec<Command>,
    ack: bool,
    stop: bool,
}

/// Owned by the model. On Full the returned batch stays here in its original order.
pub struct StoreWorker {
    to: SyncSender<Batch>,
    from: Receiver<Arc<Snapshot>>,
    thread: JoinHandle<()>,
    flushed: Arc<AtomicBool>,
    recordings: Arc<std::sync::OnceLock<Result<PathBuf, String>>>,
    wake_ui: Arc<dyn Fn() + Send + Sync>,
    opened: HashSet<TownId>,
    next_own: usize,
    pending: Batch,
    full: bool,
}

impl StoreWorker {
    pub fn start(home: Arc<Home>, pool: TaskPool, wake_ui: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        Self::spawn(home, pool, wake_ui, || {})
    }

    pub(crate) fn spawn(home: Arc<Home>, pool: TaskPool, wake_ui: impl Fn() + Send + Sync + 'static,
        before_start: impl FnOnce() + Send + 'static) -> io::Result<Self> {
        Self::spawn_with(home, pool, wake_ui, default_poster_decoder(), before_start)
    }

    #[doc(hidden)]
    pub fn start_with_poster_decoder(home: Arc<Home>, pool: TaskPool, wake_ui: impl Fn() + Send + Sync + 'static,
        poster_decoder: PosterDecoder) -> io::Result<Self> {
        Self::spawn_with(home, pool, wake_ui, poster_decoder, || {})
    }

    fn spawn_with(home: Arc<Home>, pool: TaskPool, wake_ui: impl Fn() + Send + Sync + 'static,
        poster_decoder: PosterDecoder, before_start: impl FnOnce() + Send + 'static) -> io::Result<Self> {
        let (to, commands) = mpsc::sync_channel(1);
        let (snapshots, from) = mpsc::sync_channel(1);
        let flushed = Arc::new(AtomicBool::new(false));
        let done = flushed.clone();
        let wake_ui = Arc::new(wake_ui);
        let signal = wake_ui.clone();
        let recordings = Arc::new(std::sync::OnceLock::new());
        let folder = recordings.clone();
        let thread = thread::Builder::new().name("create blob store".into()).spawn(move || {
            before_start();
            let dir = home.dir.join("local/recordings");
            folder.get_or_init(|| std::fs::create_dir_all(&dir).map(|()| dir)
                .map_err(|e| format!("Could not open the recordings folder: {e}")));
            signal();
            Stores::with_poster_decoder(home, pool, poster_decoder).run(commands, snapshots, &*signal);
            done.store(true, Ordering::Release);
            signal();
        })?;
        Ok(Self { to, from, thread, flushed, recordings, wake_ui, opened: HashSet::new(), next_own: 0, pending: Batch::default(), full: false })
    }

    pub(crate) fn recordings_dir(&self) -> Option<Result<&std::path::Path, &str>> {
        self.recordings.get().map(|dir| dir.as_deref().map_err(String::as_str))
    }

    pub fn open(&mut self, town: TownId) {
        if self.opened.insert(town) { self.pending.commands.push(Command::Open(town)); }
    }

    pub fn add(&mut self, town: TownId, project: Id, path: PathBuf, title: String) {
        self.pending.commands.push(Command::Add { town, project, path, title, clip: None });
    }

    pub(crate) fn add_clip(&mut self, town: TownId, project: Id, path: PathBuf, title: String, info: ClipImport) {
        self.pending.commands.push(Command::Add { town, project, path, title, clip: Some(info) });
    }

    pub(crate) fn add_issue(&mut self, draft: u64, town: TownId, png: Arc<Vec<u8>>, poster: Arc<Vec<u8>>) {
        self.pending.commands.push(Command::Issue { draft, town, png, poster });
    }

    pub(crate) fn add_board_snap(&mut self, town: TownId, board: Id, title: String, session: SessionId,
        source_peer: Option<PublicKey>, still: crate::app_link::IssueStill) {
        self.pending.commands.push(Command::BoardSnap { town, board, title, size: still.size, session, source_peer,
            png: still.png, poster: still.poster });
    }

    pub(crate) fn add_issue_video(&mut self, draft: u64, town: TownId, recording: crate::app_link::report::Recording) {
        self.pending.commands.push(Command::IssueVideo { draft, town, recording });
    }

    /// A person's own picture or video, posted to the town. The worker sniffs it.
    pub(crate) fn add_own(&mut self, town: TownId, path: PathBuf, title: String) {
        self.queue_own(town, path, title, None);
    }

    pub(crate) fn add_board_file(&mut self, town: TownId, target: BoardMediaTarget, path: PathBuf, title: String) {
        self.queue_own(town, path, title, Some(target));
    }

    fn queue_own(&mut self, town: TownId, path: PathBuf, title: String, board: Option<BoardMediaTarget>) {
        self.pending.commands.push(Command::AddOwn { sequence: self.next_own, town, path, title, board });
        self.next_own += 1;
    }

    pub(crate) fn remove_recording(&mut self, path: PathBuf, title: String, extension: &'static str) {
        self.pending.commands.push(Command::RemoveRecording { path, title, extension });
    }

    pub fn attach(&mut self, wake_link: impl Fn() + Send + 'static) -> StorePort {
        let (port, link) = Link::pair(self.thread.thread().clone(), wake_link);
        self.pending.commands.push(Command::Attach(link));
        port
    }

    pub fn try_recv(&self) -> Option<Arc<Snapshot>> { self.from.try_recv().ok() }
    pub fn acknowledge(&mut self) { self.pending.ack = true; }
    pub fn stop(&mut self) { self.pending.stop = true; }
    pub fn stopped(&self) -> bool { self.flushed.load(Ordering::Acquire) || self.thread.is_finished() }

    /// True when the UI should request another frame to deliver its retained batch.
    pub fn dispatch(&mut self) -> bool {
        if self.pending.commands.is_empty() && !self.pending.ack && !self.pending.stop { return false; }
        match self.to.try_send(std::mem::take(&mut self.pending)) {
            Ok(()) => {
                self.full = false;
                self.thread.thread().unpark();
                false
            }
            Err(TrySendError::Full(batch)) => {
                self.pending = batch;
                if !self.full {
                    eprintln!("town: blob store queue full; retrying on the next frame");
                    (self.wake_ui)();
                }
                self.full = true;
                true
            }
            Err(TrySendError::Disconnected(batch)) => {
                self.pending = batch;
                eprintln!("town: blob store stopped");
                false
            }
        }
    }
}

struct Poster {
    sequence: usize,
    clip: ClipResult,
    jpeg: Result<Arc<Vec<u8>>, String>,
}

/// Geometry and duration read beside a video poster JPEG.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PosterMeta {
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
}

/// Decodes a poster JPEG and fills video metadata for a dropped video file.
pub type PosterDecoder = Arc<dyn Fn(&Path, &mut PosterMeta) -> Result<Vec<u8>, String> + Send + Sync>;

fn default_poster_decoder() -> PosterDecoder {
    Arc::new(decode_poster)
}

/// Dropping the closure also returns the clip if the pool refuses or cancels it.
struct PosterJob {
    decode: PosterDecoder,
    poster: Option<Poster>,
    results: mpsc::Sender<Poster>,
    worker: Thread,
}

impl PosterJob {
    fn run(mut self) {
        let poster = self.poster.as_mut().unwrap();
        let info = &mut poster.clip.info;
        let mut meta = PosterMeta { width: info.width, height: info.height, duration_ms: info.duration_ms };
        poster.jpeg = (self.decode)(&poster.clip.path, &mut meta).map(Arc::new);
        // The container can report its geometry and duration even if reading its first frame fails.
        (info.width, info.height, info.duration_ms) = (meta.width, meta.height, meta.duration_ms);
    }
}

impl Drop for PosterJob {
    fn drop(&mut self) {
        let _ = self.results.send(self.poster.take().unwrap());
        self.worker.unpark();
    }
}

fn decode_poster(path: &Path, meta: &mut PosterMeta) -> Result<Vec<u8>, String> {
    let name = path.to_str().ok_or("The file name is not text.")?;
    let mut decoder = makepad_video::VideoFileDecoder::open(name).map_err(|e| e.to_string())?;
    let found = decoder.info();
    (meta.width, meta.height) = (found.width, found.height);
    meta.duration_ms = u64::try_from(found.duration_100ns / 10_000).unwrap_or(0);

    let frame = decoder.next_frame().map_err(|e| e.to_string())?.ok_or("the video has no frame")?;
    let w = u16::try_from(frame.width).map_err(|_| "frame too wide")?;
    let h = u16::try_from(frame.height).map_err(|_| "frame too tall")?;
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 80).encode(&frame.to_rgb8(), w, h, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| e.to_string())?;
    if meta.width == 0 { (meta.width, meta.height) = (frame.width, frame.height); }
    Ok(jpeg)
}

struct Stores {
    pool: TaskPool,
    poster_decoder: PosterDecoder,
    posters: mpsc::Sender<Poster>,
    poster_results: Receiver<Poster>,
    // The front is the first unposted drop. A storage retry keeps its original slot.
    first_own: usize,
    own_clips: VecDeque<Option<ClipResult>>,
    home: Arc<Home>,
    towns: HashMap<TownId, TownStore>,
    changes: Snapshot,
    announce: BTreeMap<(TownId, Id), bool>,
    // A read can discover damage just as its connection goes away. Keep that repair intent
    // with the open store so the replacement port can request its verified gaps.
    repairs: HashSet<(TownId, Id)>,
    chunk: usize,
    #[cfg(test)]
    checked_slots: u64,
    retry: Vec<Command>,
    recovery: Vec<(TownId, Id)>,
}

impl Stores {
    #[cfg(test)]
    fn new(home: Arc<Home>, pool: TaskPool) -> Self {
        Self::with_poster_decoder(home, pool, default_poster_decoder())
    }

    fn with_poster_decoder(home: Arc<Home>, pool: TaskPool, poster_decoder: PosterDecoder) -> Self {
        let (posters, poster_results) = mpsc::channel();
        Self { pool, poster_decoder, posters, poster_results, first_own: 0, own_clips: VecDeque::new(),
            home, chunk: CHUNK, #[cfg(test)] checked_slots: 0, announce: BTreeMap::new(), repairs: HashSet::new(), towns: HashMap::new(), changes: Snapshot::default(), retry: Vec::new(), recovery: Vec::new() }
    }

    fn town(&mut self, town: TownId) -> io::Result<&mut TownStore> {
        use std::collections::hash_map::Entry;
        Ok(match self.towns.entry(town) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let (store, report) = TownStore::open(self.home.blobs_dir().join(makepad_town_core::hex(&town)), self.chunk)?;
                #[cfg(test)]
                { self.checked_slots += report.slots_checked; }
                for (id, err) in report.failed {
                    eprintln!("town: blob recovery will retry: {err}");
                    self.recovery.push((town, id));
                }
                for id in store.complete_ids() {
                    self.announce.insert((town, id), true);
                    self.changes.holdings.insert((town, id), true);
                }
                self.changes.checked.push(town);
                entry.insert(store)
            }
        })
    }

    // G11 supplies an absolute path into this private directory. Never import or unlink a
    // caller-supplied path outside it, even if it traverses a symlink or names a directory.
    fn owned_recording(&self, path: &Path, extension: &str) -> io::Result<bool> {
        if path.extension().is_none_or(|ext| ext != extension) { return Ok(false); }
        let dir = self.home.dir.join("local/recordings").canonicalize()?;
        Ok(path.symlink_metadata()?.is_file() && path.canonicalize()?.parent() == Some(dir.as_path()))
    }

    fn import(&mut self, town: TownId, path: &Path) -> io::Result<Result<StoredFile, &'static str>> {
        let result = self.town(town).and_then(|store| {
            // Source errors are reported to the caller without the local path. Store
            // errors keep the request so a full disk never consumes the user's add.
            let file = match path.metadata().and_then(|meta| {
                if meta.is_file() { File::open(path) } else { Err(io::Error::other("expected a regular file")) }
            }) {
                Ok(file) => file,
                Err(err) => {
                    eprintln!("town: could not open {}: {err}", path.display());
                    return Ok(Err("This file could not be read."));
                }
            };
            let mut input = Source { file, error: None };
            match store.add(&mut input) {
                Ok((size, root)) => Ok(Ok(StoredFile { id: blob::id(size, &root), size, root })),
                Err(err) if input.error.is_some() => {
                    eprintln!("town: could not read {}: {err}", path.display());
                    Ok(Err("This file could not be read."))
                }
                Err(err) => Err(err),
            }
        })?;
        if let Ok(file) = result {
            self.changes.holdings.insert((town, file.id), true);
            self.announce.insert((town, file.id), true);
            self.repairs.remove(&(town, file.id));
        }
        Ok(result)
    }

    fn import_reader(&mut self, town: TownId, reader: impl io::Read) -> io::Result<StoredFile> {
        let (size, root) = self.town(town)?.add(reader)?;
        let file = StoredFile { id: blob::id(size, &root), size, root };
        self.changes.holdings.insert((town, file.id), true);
        self.announce.insert((town, file.id), true);
        self.repairs.remove(&(town, file.id));
        Ok(file)
    }

    /// A person's file: sniff its kind from its first bytes, store it as it lies (no folder
    /// check, never removed), read a picture's size from its header, and for a video take its
    /// size and length and a first-frame poster. A refusal comes back as a result, not a retry.
    fn add_own(&mut self, sequence: usize, town: TownId, path: &Path, title: &str, board: Option<BoardMediaTarget>) -> io::Result<()> {
        let refuse = |me: &mut Self, why: &'static str| {
            let info = ClipImport { source: ClipSource::Own, format: CaptureFormat::Png, session: None,
                duration_ms: 0, poster: None, width: 0, height: 0 };
            let file = FileResult { town, project: None, title: title.to_owned(), result: Err(why) };
            me.finish_own(sequence, ClipResult { file, board, source_peer: None, path: path.to_owned(), info, stored_poster: None });
            Ok(())
        };
        let mut head = Vec::new();
        let read = path.metadata().and_then(|meta| {
            if meta.is_file() { File::open(path) } else { Err(io::Error::other("expected a regular file")) }
        }).and_then(|file| io::Read::read_to_end(&mut io::Read::take(file, HEAD), &mut head));
        if let Err(err) = read {
            eprintln!("town: could not read {}: {err}", path.display());
            return refuse(self, "This file could not be read.");
        }
        let Some(format) = CaptureFormat::sniff(&head) else {
            return refuse(self, if board.is_some() { "Only pictures and videos can be dropped on a board." } else { NOT_MEDIA })
        };
        let file = match self.import(town, path)? {
            Ok(file) => file,
            Err(why) => return refuse(self, why),
        };
        let info = ClipImport { source: ClipSource::Own, format, session: None, duration_ms: 0, poster: None, width: 0, height: 0 };
        let file_result = FileResult { town, project: None, title: title.to_owned(), result: Ok(file) };
        let mut clip = ClipResult { file: file_result, board, source_peer: None, path: path.to_owned(), info, stored_poster: None };
        if format.is_video() {
            let job = PosterJob {
                decode: self.poster_decoder.clone(),
                poster: Some(Poster { sequence, clip, jpeg: Err("The poster job did not finish.".into()) }),
                results: self.posters.clone(),
                worker: thread::current(),
            };
            if let Ok(task) = self.pool.submit_named(Class::Background, "Town video poster", move || job.run()) {
                task.detach();
            }
        } else {
            // A picture is its own poster. Probe the sniffed kind, never the person's file name.
            clip.stored_poster = Some(file);
            let probe = Path::new(match format { CaptureFormat::Jpeg => "picture.jpg", CaptureFormat::Gif => "picture.gif", _ => "picture.png" });
            if let Ok((w, h)) = makepad_widgets::makepad_draw::image_cache::image_size_by_data(&head, probe) {
                (clip.info.width, clip.info.height) = (u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0));
            }
            self.finish_own(sequence, clip);
        }
        Ok(())
    }

    fn store_poster(&mut self, mut poster: Poster, retrying: bool) {
        match &poster.jpeg {
            Ok(jpeg) => match self.import_reader(poster.clip.file.town, jpeg.as_slice()) {
                Ok(file) => poster.clip.stored_poster = Some(file),
                Err(err) => {
                    if !retrying { eprintln!("town: blob store will retry: {err}"); }
                    self.retry.push(Command::StorePoster(Box::new(poster)));
                    return;
                }
            },
            Err(error) => {
                // The decoder's own words (its source paths, the file's location) stay in this computer's log.
                let title = &poster.clip.file.title;
                eprintln!("town: {title}: no poster could be decoded: {error}");
                self.changes.recording_problems.push(format!("{title}: posted without a preview, since no picture could be read from it."));
            }
        }
        self.finish_own(poster.sequence, poster.clip);
    }

    fn finish_own(&mut self, sequence: usize, clip: ClipResult) {
        self.own_clips[sequence - self.first_own] = Some(clip);
        while let Some(clip) = self.own_clips.front_mut().and_then(Option::take) {
            self.own_clips.pop_front();
            self.first_own += 1;
            self.changes.clips.push(clip);
        }
    }

    fn command(&mut self, cmd: Command, links: &mut Vec<Link>, retrying: bool) {
        let result = match cmd {
            Command::Attach(mut link) => {
                link.announce.extend(self.repairs.iter().map(|&key| (key, false)));
                for (&town, store) in &self.towns {
                    link.announce.extend(store.complete_ids().map(|id| ((town,id), true)));
                }
                links.push(link);
                return;
            }
            Command::Open(town) => self.town(town).map(|_| ()),
            Command::IssueVideo { draft, town, recording } => {
                let result = self.import_reader(town, recording.file)
                    .map(|file| IssueMedia::Video { file, duration_ms: recording.duration_ms })
                    .map_err(|e| format!("Could not store the issue recording: {e}"));
                self.changes.issues.push((draft, result));
                return;
            }
            Command::Issue { draft, town, png, poster } => {
                let result = self.import_reader(town, png.as_slice()).and_then(|full|
                    self.import_reader(town, poster.as_slice()).map(|small| IssueMedia::Picture([full, small])));
                self.changes.issues.push((draft, result.map_err(|e| format!("Could not store the issue picture: {e}"))));
                return;
            }
            Command::BoardSnap { town, board, ref title, size: [width, height], session, source_peer, ref png, ref poster } => (|| {
                let full = self.import_reader(town, png.as_slice())?;
                let small = self.import_reader(town, poster.as_slice())?;
                let file = FileResult { town, project: None, title: title.clone(), result: Ok(full) };
                let info = ClipImport { source: ClipSource::Own, format: CaptureFormat::Png, session: Some(session),
                    duration_ms: 0, poster: None, width, height };
                self.changes.clips.push(ClipResult { file, board: Some(BoardMediaTarget { board, pos: None }),
                    source_peer, path: PathBuf::new(), info, stored_poster: Some(small) });
                Ok(())
            })(),
            Command::RemoveRecording { ref path, ref title, extension } => {
                let result = self.owned_recording(path, extension).and_then(|owned| {
                    if owned { std::fs::remove_file(path) }
                    else { Err(io::Error::other("the file is outside the recordings folder")) }
                });
                if let Err(e) = result {
                    if e.kind() != io::ErrorKind::NotFound {
                        if !retrying {
                            self.changes.recording_problems.push(format!("{title}: the clip was posted, but its recording file could not be removed: {e}"));
                        }
                        self.retry.push(cmd);
                    }
                }
                return;
            }
            Command::AddOwn { sequence, town, ref path, ref title, board } => {
                if !retrying { self.own_clips.push_back(None); }
                self.add_own(sequence, town, path, title, board)
            }
            Command::StorePoster(poster) => {
                self.store_poster(*poster, retrying);
                return;
            }
            Command::Add { town, project, ref path, ref title, ref clip } => (|| {
                let media = if clip.as_ref().is_some_and(|info| !self.owned_recording(path, info.format.extension()).unwrap_or(false)) {
                    Err("Cannot read this file from the recordings folder.")
                } else { self.import(town, path)? };
                let file = FileResult { town, project: Some(project), title: title.clone(), result: media };
                if let Some(info) = clip {
                    let mut stored_poster = if info.format == CaptureFormat::Mp4 { None } else { media.ok() };
                    if let Some(poster) = info.poster.as_deref().filter(|_| media.is_ok()) {
                        let result = self.owned_recording(poster, "jpg").and_then(|owned| {
                            if owned { self.import(town, poster) }
                            else { Ok(Err("The poster is outside the recordings folder.")) }
                        });
                        let problem = match result {
                            Ok(Ok(stored)) => { stored_poster = Some(stored); None }
                            Ok(Err(why)) => Some(why),
                            Err(err) => {
                                eprintln!("town: {title}: storing the poster failed: {err}");
                                Some("Its preview couldn't be stored.")
                            }
                        };
                        if let Some(why) = problem {
                            let why = format!("{title}: posted without a preview. {why}");
                            eprintln!("town: {why}");
                            self.changes.recording_problems.push(why);
                        }
                    }
                    self.changes.clips.push(ClipResult { file, board: None, source_peer: None, path: path.clone(), info: info.clone(), stored_poster });
                } else {
                    self.changes.files.push(file);
                }
                Ok(())
            })(),
        };
        if let Err(err) = result {
            if let Command::Add { clip: Some(_), ref title, .. } | Command::BoardSnap { ref title, .. } = cmd {
                if !retrying { self.changes.recording_problems.push(format!("{title}: storage will retry: {err}")); }
            }
            if !retrying { eprintln!("town: blob store will retry: {err}"); }
            self.retry.push(cmd);
        }
    }

    fn transfer(&mut self, work: &LinkWork) -> io::Result<LinkResult> {
        match *work {
            LinkWork::Begin { town, size, root } => {
                let store = self.town(town)?;
                let id = store.begin(size, root)?;
                let absent = store.absent(&id)?;
                if absent.is_empty() { self.changes.holdings.insert((town, id), true); }
                Ok(LinkResult::Begun { town, file: StoredFile { size, root, id }, absent })
            }
            LinkWork::Transfer(StoreRequest::Read { carrier, town, id, index }) => {
                let store = self.town(town)?;
                let before = store.is_complete(&id);
                let result = store.serve(&id, index);
                let complete = store.is_complete(&id);
                if before != complete {
                    self.changes.holdings.insert((town, id), complete);
                    self.announce.insert((town, id), complete);
                    if !complete { self.repairs.insert((town, id)); }
                }
                Ok(LinkResult::Read { carrier, town, id, index, result: result?, complete })
            }
            LinkWork::Transfer(StoreRequest::Chunk { carrier, town, id, index, ref proof, ref bytes }) => {
                let store = self.town(town)?;
                match store.write_chunk(&id, index, proof, bytes) {
                    Ok(()) => {
                        let complete = store.is_complete(&id);
                        if complete {
                            self.changes.holdings.insert((town, id), true);
                            self.announce.insert((town, id), true);
                            self.repairs.remove(&(town, id));
                        }
                        Ok(LinkResult::Written { town, id, index, complete })
                    }
                    Err(WriteError::Verify(_)) => Ok(LinkResult::Bad { carrier, town, id, index }),
                    Err(WriteError::Store(err)) => Err(err),
                }
            }
        }
    }

    fn pump_links(&mut self, links: &mut Vec<Link>, timed: bool) -> bool {
        links.retain(|link| link.connected.load(Ordering::Acquire));
        let changes = std::mem::take(&mut self.announce);
        let mut progress = false;
        for link in links {
            link.announce.extend(changes.iter().map(|(&key, &complete)| (key, complete)));
            progress |= self.pump_link(link, timed);
        }
        progress
    }

    fn pump_link(&mut self, link: &mut Link, timed: bool) -> bool {
        if let Some(reply) = link.reply.take() {
            match link.to.try_send(reply) {
                Ok(()) => { (link.wake)(); return true; }
                Err(TrySendError::Full(reply)) => { link.reply = Some(reply); return false; }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        if !link.announce.is_empty() {
            link.reply = Some(LinkResult::Holdings(std::mem::take(&mut link.announce)));
            return true;
        }
        let retrying = link.retry.is_some();
        if retrying && !timed { return false; }
        let Some(work) = link.retry.take().or_else(|| link.from.try_recv().ok()) else { return false };
        (link.wake)();
        match self.transfer(&work) {
            Ok(reply) => { link.reply = Some(reply); true }
            Err(err) => {
                if !retrying { eprintln!("town: blob transfer waiting for storage: {err}"); }
                link.retry = Some(work);
                false
            }
        }
    }

    fn flush(&mut self, already_waiting: bool) -> bool {
        let mut clean = true;
        for store in self.towns.values_mut() {
            if let Err(err) = store.flush() {
                if !already_waiting { eprintln!("town: blob flush will retry: {err}"); }
                clean = false;
            }
        }
        clean
    }

    fn run(mut self, commands: Receiver<Batch>, snapshots: SyncSender<Arc<Snapshot>>, wake_ui: &dyn Fn()) {
        let mut next_flush = Instant::now() + RETRY_INTERVAL;
        let mut next_snapshot = Instant::now();
        let mut outstanding = None;
        let mut unsent = None;
        let mut links = Vec::new();
        let mut stopping = false;
        let mut waiting = false;
        let mut flush_failed = false;
        loop {
            match commands.try_recv() {
                Ok(batch) => {
                    // Taking the batch frees the UI's one slot, even when an acknowledgement
                    // produces no snapshot. A retained add or stop can now retry.
                    wake_ui();
                    if batch.ack { outstanding = None; }
                    for cmd in batch.commands { self.command(cmd, &mut links, false); }
                    stopping |= batch.stop;
                }
                Err(mpsc::TryRecvError::Disconnected) => { self.flush(false); return; }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            while let Ok(poster) = self.poster_results.try_recv() { self.store_poster(poster, false); }
            let timed = Instant::now() >= next_flush;
            if timed {
                for cmd in std::mem::take(&mut self.retry) { self.command(cmd, &mut links, true); }
                for (town, id) in std::mem::take(&mut self.recovery) {
                    if let Some(store) = self.towns.get_mut(&town) {
                        match store.retry_recovery(id) {
                            Ok(()) => {
                                let complete = store.is_complete(&id);
                                self.changes.holdings.insert((town, id), complete);
                                if complete { self.repairs.remove(&(town, id)); }
                                if complete || self.repairs.contains(&(town, id)) {
                                    self.announce.insert((town, id), complete);
                                }
                            }
                            Err(_) => self.recovery.push((town, id)),
                        }
                    }
                }
                flush_failed = !self.flush(flush_failed);
                next_flush = Instant::now() + RETRY_INTERVAL;
            }
            let more = self.pump_links(&mut links, timed);
            let blocked = flush_failed || !self.retry.is_empty() || !self.recovery.is_empty()
                || links.iter().any(|l| l.retry.is_some());
            if blocked != waiting { self.changes.waiting = Some(blocked); waiting = blocked; }
            let mut deadline = next_flush;
            if outstanding.is_none() && (unsent.is_some() || self.changes.changed()) {
                if Instant::now() >= next_snapshot {
                    let was_full = unsent.is_some();
                    let snapshot = unsent.take().unwrap_or_else(|| Arc::new(std::mem::take(&mut self.changes)));
                    match snapshots.try_send(snapshot.clone()) {
                        Ok(()) => { outstanding = Some(snapshot); wake_ui(); }
                        Err(TrySendError::Full(saved)) => {
                            if !was_full { eprintln!("town: blob snapshot queue full; retaining results"); }
                            unsent = Some(saved);
                        }
                        Err(TrySendError::Disconnected(_)) => { self.flush(false); return; }
                    }
                    next_snapshot = Instant::now() + SNAPSHOT_INTERVAL;
                }
                if outstanding.is_none() { deadline = deadline.min(next_snapshot); }
            }
            if stopping && self.own_clips.is_empty() && unsent.is_none() && !self.changes.changed() && !flush_failed {
                if self.flush(false) { return; }
                flush_failed = true;
            }
            if !more { thread::park_timeout(deadline.saturating_duration_since(Instant::now())); }
        }
    }
}

struct Source {
    file: File,
    error: Option<io::ErrorKind>,
}

impl io::Read for Source {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.file.read(bytes).inspect_err(|err| {
            if err.kind() != io::ErrorKind::Interrupted { self.error = Some(err.kind()); }
        })
    }
}

#[cfg(test)]
pub(crate) fn test_pool() -> TaskPool {
    makepad_widgets::Cx::new(Box::new(|_, _| {})).task_pool()
}

/// Drive the same bounded port and disk operations one turn at a time in the network simulator.
#[cfg(test)]
pub(crate) struct SimStore {
    stores: Stores,
    links: Vec<Link>,
}

#[cfg(test)]
impl SimStore {
    pub(crate) fn open(home: Arc<Home>, town: TownId, chunk: usize) -> Self {
        let mut stores = Stores::new(home, test_pool());
        stores.chunk = chunk;
        stores.town(town).unwrap();
        Self { stores, links: Vec::new() }
    }

    pub(crate) fn attach(&mut self) -> StorePort {
        let (port, link) = Link::pair(thread::current(), || {});
        self.stores.command(Command::Attach(link), &mut self.links, false);
        port
    }

    pub(crate) fn turn(&mut self) {
        self.stores.pump_links(&mut self.links, true);
    }

    pub(crate) fn add(&mut self, town: TownId, bytes: &[u8]) -> StoredFile {
        let (size, root) = self.stores.town(town).unwrap().add(bytes).unwrap();
        StoredFile { size, root, id: blob::id(size, &root) }
    }

    pub(crate) fn checked(&self) -> u64 { self.stores.checked_slots }
    pub(crate) fn flush(&mut self) { assert!(self.stores.flush(false)); }
    pub(crate) fn absent(&mut self, town: TownId, id: Id) -> Vec<Range<u64>> {
        self.stores.town(town).unwrap().absent(&id).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::ShareHomes;

    #[test]
    fn store_replies_follow_the_attachment_across_a_reconnect() {
        let dir = ShareHomes::new("store-attachments");
        let home = Arc::new(Home::open(&dir.0).unwrap());
        let towns = [[1; 32], [2; 32]];
        let mut store = SimStore::open(home, towns[0], CHUNK);
        let ben = store.attach();
        let kit = store.attach();
        let bytes: Arc<[u8]> = Arc::from(&b"file"[..]);
        let root = blob::Tree::new(&bytes, CHUNK).unwrap().root();
        for (port, town) in [(&ben, towns[0]), (&kit, towns[1])] {
            port.try_send(LinkWork::Begin { town, size: bytes.len() as u64, root }).unwrap();
        }
        store.turn(); // Each attachment now has a reply ready at the store.
        drop(ben);
        let ben = store.attach();
        store.turn();
        assert!(ben.try_recv().is_none(), "the replacement cannot receive the old Begin reply");
        let Some(LinkResult::Begun { town, file, .. }) = kit.try_recv() else { panic!("Kit lost its reply") };
        assert_eq!(town, towns[1]);
        kit.try_send(LinkWork::Transfer(StoreRequest::Chunk {
            carrier: [3; 65], town, id: file.id, index: 0, proof: Arc::from([]), bytes,
        })).unwrap();
        store.turn();
        store.turn();
        assert!(matches!(kit.try_recv(), Some(LinkResult::Written { town, id, complete: true, .. })
            if town == towns[1] && id == file.id), "Kit finishes on its original port");
        store.turn();
        assert!(matches!(ben.try_recv(), Some(LinkResult::Holdings(held))
            if held.get(&(town, file.id)) == Some(&true)), "holdings reach both live ports");
    }

    #[test]
    fn stopping_after_a_video_and_a_picture_returns_both_in_drop_order() {
        let dir = ShareHomes::new("poster-order");
        let home = Arc::new(Home::open(&dir.0).unwrap());
        let video = dir.0.join("broken.mp4");
        let picture = dir.0.join("picture.gif");
        std::fs::write(&video, b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2").unwrap();
        std::fs::write(&picture, b"GIF89a\x01\0\x01\0").unwrap();

        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = std::sync::Mutex::new(release_rx);
        let mut worker = StoreWorker::start_with_poster_decoder(home, test_pool(), || {}, Arc::new(move |_, _| {
            release_rx.lock().unwrap().recv().unwrap();
            Err("synthetic broken video".into())
        })).unwrap();
        worker.add_own([1; 32], video.clone(), "broken.mp4".into());
        worker.add_own([1; 32], picture.clone(), "picture.gif".into());
        worker.stop();
        worker.dispatch();
        let snapshot = worker.from.recv().unwrap();
        assert!(snapshot.clips.is_empty(), "the ready picture waits for the earlier video");
        worker.acknowledge();
        worker.dispatch();
        release_tx.send(()).unwrap();
        let mut clips = Vec::new();
        let mut problems = snapshot.recording_problems.clone();
        while let Ok(snapshot) = worker.from.recv() {
            clips.extend_from_slice(&snapshot.clips);
            problems.extend_from_slice(&snapshot.recording_problems);
            worker.acknowledge();
            worker.dispatch();
        }
        worker.thread.join().unwrap();
        assert_eq!(clips.len(), 2, "both results are delivered before shutdown");
        assert_eq!((&clips[0].path, &clips[1].path), (&video, &picture), "the video is posted first");
        assert!(clips[0].file.result.is_ok() && clips[0].stored_poster.is_none(), "the broken video posts without a poster");
        assert_eq!(clips[1].stored_poster, Some(clips[1].file.result.unwrap()), "the picture is its own poster");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems, ["broken.mp4: posted without a preview, since no picture could be read from it."],
            "the shown line is plain; the decoder's detail goes only to the log");
    }

    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    #[test]
    fn decode_poster_reads_a_real_clip_and_refuses_a_header_only_mp4() {
        use makepad_video::{VideoFileCodec, VideoFileEncoder, VideoFileEncoderOptions};

        let dir = ShareHomes::new("poster-decode");
        std::fs::create_dir_all(&dir.0).unwrap();
        let real = dir.0.join("real.mp4");
        let mut encoder = VideoFileEncoder::new(real.to_str().unwrap(), VideoFileEncoderOptions {
            codec: VideoFileCodec::H264, width: 64, height: 48, ..Default::default()
        }).expect("create video encoder");
        for grey in [96, 160] {
            encoder.push_frame_rgb8(&[grey; 64 * 48 * 3], None).expect("encode frame");
        }
        encoder.finish().expect("finish the clip");

        let mut meta = PosterMeta::default();
        let jpeg = decode_poster(&real, &mut meta).expect("decode the poster");
        assert!(jpeg.starts_with(&[0xff, 0xd8, 0xff]), "the poster has a JPEG header");
        assert_eq!((meta.width, meta.height), (64, 48), "poster dimensions match the clip");

        let mp4 = b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2".as_slice();
        let mov = b"\0\0\0\x14ftypqt  \0\0\0\0qt  ".as_slice();
        for (name, bytes) in [
            ("broken.mp4", mp4.to_vec()),
            ("lying.png", [mp4, &[0; 8]].concat()),
            ("screen.mov", [mov, &[0; 8]].concat()),
        ] {
            let path = dir.0.join(name);
            std::fs::write(&path, bytes).unwrap();
            assert!(decode_poster(&path, &mut PosterMeta::default()).is_err(), "{name} has no poster frame");
        }
    }

    #[test]
    fn a_local_write_error_keeps_credit_and_retries_the_same_verified_chunk() {
        let dir = ShareHomes::new("store-write-retry");
        let home = Arc::new(Home::open(&dir.0).unwrap());
        let (town, carrier) = ([1; 32], [2; 65]);
        let bytes: Arc<[u8]> = Arc::from(&b"blob"[..]);
        let root = blob::Tree::new(&bytes, CHUNK).unwrap().root();
        let mut stores = Stores::new(home.clone(), test_pool());
        let LinkResult::Begun { file, absent, .. } = stores.transfer(&LinkWork::Begin { town, size: 4, root }).unwrap()
            else { panic!("begin supplies a signed descriptor and the absent ranges") };
        assert_eq!(absent, [0..1]);
        let write = |bytes| LinkWork::Transfer(StoreRequest::Chunk {
            carrier, town, id: file.id, index: 0, proof: Arc::from([]), bytes,
        });
        assert!(matches!(stores.transfer(&write(Arc::from(&b"bad!"[..]))).unwrap(), LinkResult::Bad { .. }));

        let part = home.blobs_dir().join(makepad_town_core::hex(&town)).join(format!("{}.part", makepad_town_core::hex(&file.id)));
        std::fs::rename(&part, part.with_extension("held")).unwrap();
        std::fs::create_dir(&part).unwrap();
        let (port, mut link) = Link::pair(thread::current(), || {});
        port.try_send(write(bytes)).unwrap();
        assert!(!stores.pump_link(&mut link, false));
        assert!(link.retry.is_some() && port.try_recv().is_none(), "local I/O returns neither write credit nor bad");
        std::fs::remove_dir(&part).unwrap();
        std::fs::rename(part.with_extension("held"), &part).unwrap();
        assert!(stores.pump_link(&mut link, true), "retry the retained write at the next deadline");
        assert!(stores.pump_link(&mut link, false));
        assert!(matches!(port.try_recv(), Some(LinkResult::Written { id, complete: true, .. }) if id == file.id));
        assert_eq!(stores.changes.holdings.get(&(town, file.id)), Some(&true));
    }
}
