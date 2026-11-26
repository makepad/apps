//! Decks and cuts that live in a folder (the bundled `stage/content/<name>`)
//! open in place: their tree is read straight from the files, an edit made in
//! Stage is written back into the folder, and a watcher reloads the open
//! document when the files change on disk (a person's editor, an agent, git).
//! git is their history; nothing is copied into the asset store. A build
//! without the source folder opens its bundled copy read-only, and the first
//! save of an edit keeps the person's own copy in the store.

use super::super::document_tree;
use super::open_doc::TreeDoc;
use makepad_asset_data::tree::TreeDraft;
use makepad_asset_data::AssetKind;
use makepad_story_edit_render::media::folder::Folder;
use makepad_filesystem_watcher::{FileSystemEventKind, FileSystemWatcher, WatchRoot};
use makepad_model_install::{self as models, InstallHandle, InstallMsg, Installable};
use makepad_widgets::makepad_platform::file_dialogs::{FileDialog, FileDialogAction};
use makepad_widgets::makepad_platform::thread::SignalToUI;
use makepad_widgets::*;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// How long a folder stays quiet before it is read again (an editor's save
/// is several events).
const QUIET: Duration = Duration::from_millis(300);

/// Where the folder documents are, each root once, in this order (written
/// back to, except a shipped build's bundled copy):
/// - the source checkout's own `stage/content` (Story's example decks);
/// - a sibling `commercial` checkout's `stage/content` when one sits beside
///   this repository in the Makepad checkout (`apps/commercial`), so a person's
///   decks and films there stay in the Library with no setup;
/// - the folders the person adds in Story's local settings
///   ([`settings_roots`]: one absolute folder per line);
/// - without a source checkout, the copy a shipped build bundles (read-only).
pub(crate) fn content_roots() -> &'static [(PathBuf, bool)] {
    static ROOTS: OnceLock<Vec<(PathBuf, bool)>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let settings = std::fs::read_to_string(settings_roots()).unwrap_or_default();
        let roots = roots_of(&manifest, &settings);
        if !roots.is_empty() { return roots; }
        let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) else { return Vec::new() };
        [dir.join("content"), dir.join("../Resources/content")].into_iter().find(|dir| dir.is_dir()).map(|dir| (dir, false)).into_iter().collect()
    })
}

/// Story's local settings file of extra content roots: one absolute folder
/// per line, `#` starts a comment.
pub(crate) fn settings_roots() -> PathBuf {
    makepad_ai_hub::home::makepad_home().join("story").join("content-roots.txt")
}

/// The checkout roots of a build whose package is at `manifest` (`<repo>/story/app`),
/// then the settings' roots: each existing folder once (by its canonical path).
fn roots_of(manifest: &Path, settings: &str) -> Vec<(PathBuf, bool)> {
    let own = manifest.join("../../stage/content");
    let sibling = manifest.join("../../../commercial/stage/content");
    let listed = settings.lines().map(|line| line.split('#').next().unwrap_or("").trim()).filter(|line| !line.is_empty()).map(PathBuf::from);
    let mut roots: Vec<(PathBuf, bool)> = Vec::new();
    for dir in [own, sibling].into_iter().chain(listed) {
        let Ok(dir) = dir.canonicalize() else { continue };
        if dir.is_dir() && !roots.iter().any(|(root, _)| *root == dir) { roots.push((dir, true)); }
    }
    roots
}

/// A file a folder names but does not carry, and where to fetch it.
pub struct Pin {
    /// The optional logical path this download fills.
    pub path: String,
    /// The installer's record: the pinned URL, size and SHA-256.
    pub model: &'static Installable,
}

/// One folder document: a deck tree (one or more deck files) or a cut tree.
/// Its documents, root and optional entries are the shared folder reader's
/// ([`Folder`], which the player reads folders with too).
pub struct FolderDoc {
    /// The folder's name.
    pub name: String,
    folder: Folder,
    /// Edits are written back into the folder (a source checkout).
    pub writable: bool,
    /// The folder's credits, shown before a download.
    pub credits: String,
    pub pin: Option<Pin>,
}

/// A Library card: one document file of a folder.
#[derive(Clone)]
pub struct Card {
    pub doc: &'static FolderDoc,
    pub path: String,
    pub title: String,
}

impl Card {
    /// The card's line under its title: empty, unless another card has the
    /// same title (then where each lives tells them apart).
    pub fn detail(&self) -> String {
        let lane = if self.is_deck() { deck_cards() } else { cut_cards() };
        let twins = lane.iter().filter(|other| other.title == self.title).count();
        if twins > 1 { format!("{} · {}", self.doc.name, self.path) } else { String::new() }
    }

    /// A deck file (DECK opens it), else a cut.
    pub fn is_deck(&self) -> bool {
        self.doc.decks.contains(&self.path)
    }

    pub fn matches(&self, search: &str) -> bool {
        let search = search.trim().to_lowercase();
        search.is_empty() || self.title.to_lowercase().contains(&search) || self.doc.name.to_lowercase().contains(&search)
    }
}

/// The folder documents, read once (their files are read when opened).
pub fn docs() -> &'static [FolderDoc] {
    static DOCS: OnceLock<Vec<FolderDoc>> = OnceLock::new();
    DOCS.get_or_init(|| docs_in(content_roots()))
}

/// The folder documents under `roots`, in their order; a folder name an
/// earlier root has already is that root's.
fn docs_in(roots: &[(PathBuf, bool)]) -> Vec<FolderDoc> {
    let mut out: Vec<FolderDoc> = Vec::new();
    for (root, writable) in roots {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(root).map(|entries| entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect()).unwrap_or_default();
        dirs.sort();
        for dir in dirs {
            match FolderDoc::at(&dir, *writable) {
                Ok(Some(doc)) if out.iter().any(|other| other.name == doc.name) => log!("folder docs: {}: {} is already in the Library", dir.display(), doc.name),
                Ok(doc) => out.extend(doc),
                Err(error) => log!("folder docs: {}: {error}", dir.display()),
            }
        }
    }
    out
}

/// A card per deck file: a tree of several talks gets one per talk, each
/// opening that deck in the one tree (links between them keep working).
pub fn deck_cards() -> &'static [Card] {
    static CARDS: OnceLock<Vec<Card>> = OnceLock::new();
    CARDS.get_or_init(|| {
        let listed = listed_decks(content_roots(), docs());
        let cards: Vec<Card> = listed.into_iter().filter_map(|(folder, path)| {
            let doc = docs().iter().find(|doc| doc.name == folder && doc.decks.iter().any(|deck| *deck == path))?;
            Some(Card { doc, title: doc.title_of(&path), path })
        }).collect();
        trace!("stage.library", "folder docs: the Library lists {}", cards.iter().map(|card| format!("“{}”", card.title)).collect::<Vec<_>>().join(", "));
        cards
    })
}

/// A content root's list of the presentations the Library offers, in order:
/// `{"decks": ["<folder>/<deck file>", ...]}`. A root's other folders stay on
/// disk but are not offered (user: "reduce the set of presentations to just
/// the final ones"); a talk left out is still reachable by a link from
/// another talk of its tree. A root without the file offers each of its
/// folders' root deck, after the listed ones.
pub(crate) const LIBRARY: &str = "library.json";

/// The (folder, deck file) pairs the Library lists, each once: first every
/// root's [`LIBRARY`] entries, in root order (an entry may name a folder of
/// any root), then the root decks of the folders of roots without one.
fn listed_decks(roots: &[(PathBuf, bool)], docs: &[FolderDoc]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut unlisted = Vec::new();
    for (root, _) in roots {
        let Ok(bytes) = std::fs::read(root.join(LIBRARY)) else { unlisted.push(root); continue };
        let value = makepad_strict_json::parse_depth(&bytes, 4).ok();
        let Some(decks) = value.as_ref().and_then(|value| value.get("decks")).and_then(|decks| decks.as_arr()) else {
            log!("folder docs: {}: not a {{\"decks\": [\"<folder>/<deck file>\", ...]}} object", root.join(LIBRARY).display());
            continue;
        };
        out.extend(decks.iter().filter_map(|value| value.as_str()?.split_once('/')).map(|(folder, path)| (folder.to_string(), path.to_string())));
    }
    for root in unlisted {
        out.extend(docs.iter().filter(|doc| doc.dir.starts_with(root)).filter_map(|doc| Some((doc.name.clone(), doc.decks.first()?.clone()))));
    }
    let mut seen = BTreeSet::new();
    out.retain(|pair| seen.insert(pair.clone()));
    out
}

/// A card per folder cut.
pub fn cut_cards() -> &'static [Card] {
    static CARDS: OnceLock<Vec<Card>> = OnceLock::new();
    CARDS.get_or_init(|| docs().iter().filter_map(|doc| doc.cut.as_ref().map(|path| Card { doc, path: path.clone(), title: doc.title_of(path) })).collect())
}

/// A document's title (a deck's `title:`, a cut's `label:`) from its source,
/// without loading it.
fn source_title(text: &str) -> Option<String> {
    let syntax = makepad_widgets::makepad_script::syntax::parse(text).ok()?;
    let node = syntax.statements().last()?;
    let field = node.field("title").or_else(|| node.field("label"))?;
    field.value.string().map(str::to_string)
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

impl std::ops::Deref for FolderDoc {
    type Target = Folder;
    fn deref(&self) -> &Folder {
        &self.folder
    }
}

impl FolderDoc {
    /// `None` when `dir` holds no deck or cut.
    pub fn at(dir: &Path, writable: bool) -> Result<Option<FolderDoc>, String> {
        let name = dir.file_name().and_then(|name| name.to_str()).ok_or("folder name is not UTF-8")?.to_string();
        let mut credits_file = None;
        for entry in std::fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))? {
            let path = entry.map_err(|error| format!("{}: {error}", dir.display()))?.path();
            if !path.is_file() { continue; }
            let Some(file) = path.file_name().and_then(|name| name.to_str()).map(str::to_string) else { continue };
            if file.ends_with("_credits.txt") && credits_file.replace(path.clone()).is_some() {
                return Err("more than one credits file".into());
            }
        }
        let Some(folder) = Folder::at(dir)? else { return Ok(None) };
        let credits = credits_file.and_then(|path| std::fs::read_to_string(path).ok()).unwrap_or_default().trim().to_string();
        let pin = match std::fs::read_to_string(dir.join("song.pin")) {
            Ok(text) => Some(Pin::parse(&name, &text, &credits)?),
            Err(_) => None,
        };
        if let Some(pin) = &pin {
            if !folder.optional.contains(&pin.path) { return Err(format!("song.pin: `{}` is not declared optional", pin.path)); }
        }
        Ok(Some(FolderDoc { name, folder, writable, credits, pin }))
    }

    fn title_of(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.join(path)).ok().as_deref().and_then(source_title).unwrap_or_else(|| self.name.clone())
    }

    /// The song an optional entry names (the pinned one, else the one audio file).
    pub fn song_path(&self) -> Option<&str> {
        if let Some(pin) = &self.pin { return Some(&pin.path); }
        let mut audio = self.optional.iter().filter(|path| makepad_motion::tree::file_kind(path) == AssetKind::Audio);
        let path = audio.next()?;
        audio.next().is_none().then_some(path.as_str())
    }

    /// The tree as the folder holds it now. Media files are named by a
    /// handle of their path and bytes that every resolver reads off disk;
    /// an optional entry is filled from the folder, else from its pinned
    /// download.
    pub fn read(&self) -> Result<Arc<TreeDraft>, String> {
        let pinned = |path: &str| self.pin.as_ref().filter(|pin| pin.path == path && pin.installed()).map(|pin| models::weights_path(pin.model));
        let draft = self.folder.read(self.title_of(&self.root), &pinned)?;
        makepad_asset_client::tree::validate_draft(&draft)?;
        Ok(Arc::new(draft))
    }

    /// Write the files that differ between `saved` (as read or last
    /// written) and `tree` into the folder; a file removed from the tree is
    /// removed. Refuses the files that changed on disk since `saved` unless
    /// `overwrite` names them.
    pub fn write(&self, saved: &TreeDraft, tree: &TreeDraft, overwrite: &[String]) -> Result<usize, WriteRefused> {
        if !self.writable { return Err(WriteRefused::Failed(format!("{} is read-only here", self.name))); }
        let changed = document_tree::changed_files(saved, tree);
        let conflicts: Vec<String> = changed.iter().filter(|path| {
            let on_disk = std::fs::read(self.dir.join(path)).ok();
            let expected = saved.files.get(*path).map(|file| file.bytes.as_ref());
            on_disk.as_deref() != expected && !overwrite.contains(*path)
        }).cloned().collect();
        if !conflicts.is_empty() { return Err(WriteRefused::Conflict(conflicts)); }
        let failed = |path: &Path, error: std::io::Error| WriteRefused::Failed(format!("{}: {error}", path.display()));
        for path in &changed {
            let target = self.dir.join(path);
            match tree.files.get(path) {
                Some(file) => {
                    if let Some(parent) = target.parent() { std::fs::create_dir_all(parent).map_err(|error| failed(parent, error))?; }
                    // A whole file or none: a reader (the watcher, an editor) never sees half of it.
                    let temporary = target.with_file_name(format!(".{}.stage-save", target.file_name().and_then(|name| name.to_str()).unwrap_or("file")));
                    std::fs::write(&temporary, &file.bytes).map_err(|error| failed(&temporary, error))?;
                    std::fs::rename(&temporary, &target).map_err(|error| failed(&target, error))?;
                }
                None => match std::fs::remove_file(&target) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(failed(&target, error)),
                },
            }
        }
        Ok(changed.len())
    }
}

/// Why a folder save did not happen.
#[derive(Debug, PartialEq)]
pub enum WriteRefused {
    /// These files changed on disk too since they were read.
    Conflict(Vec<String>),
    Failed(String),
}

/// The tree as a store document: the folder handles are not store
/// revisions, so every file goes in as its bytes.
pub fn for_store(tree: &TreeDraft) -> Arc<TreeDraft> {
    let mut copy = tree.clone();
    for file in copy.files.values_mut() {
        file.reference = None;
        file.base = None;
    }
    Arc::new(copy)
}


impl Pin {
    /// `song.pin`: `path`, `url`, `bytes`, `sha256` lines (`#` comments).
    fn parse(name: &str, text: &str, credits: &str) -> Result<Pin, String> {
        let mut fields: HashMap<&str, &str> = HashMap::new();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let (key, value) = line.split_once(' ').ok_or_else(|| format!("song.pin: `{line}` is not `key value`"))?;
            fields.insert(key, value.trim());
        }
        let field = |key: &str| fields.get(key).copied().ok_or_else(|| format!("song.pin has no `{key}`"));
        let (path, url, sha256) = (field("path")?, field("url")?, field("sha256")?);
        makepad_asset_data::tree::validate_path(path)?;
        let bytes: u64 = field("bytes")?.parse().map_err(|_| "song.pin: `bytes` is not a number".to_string())?;
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("song.pin: `sha256` is not 64 hex digits".into());
        }
        let file = url.rsplit('/').next().unwrap_or("song.mp3");
        let ext = file.rsplit_once('.').map_or("mp3", |(_, ext)| ext);
        let model = Installable::dataset(
            leak(format!("{name} song")),
            "the song's authors' (not part of the repository's licence; fetched for this machine only)",
            leak(credits.to_string()),
            leak(format!("the copy of the film's song beside its web build, {url}")),
            bytes,
            leak(sha256.to_ascii_lowercase()),
            leak(url.to_string()),
            leak(format!("examples/{name}/song.{ext}")),
        );
        Ok(Pin { path: path.to_string(), model: Box::leak(Box::new(model)) })
    }

    pub fn installed(&self) -> bool {
        models::installed(self.model)
    }
}

/// A folder document being read and loaded on its own thread: a deck shows
/// its first slide while the others load.
pub struct OpenJob {
    pub card: Card,
    rx: Receiver<Loaded>,
}

pub enum Loaded {
    /// The tree and the document (a deck: its first slide's graphic only).
    First(Arc<TreeDraft>, TreeDoc),
    /// The whole deck, after `First`.
    Whole(TreeDoc),
    Failed(String),
}

impl OpenJob {
    pub fn start(card: Card) -> OpenJob {
        let (out, rx) = channel();
        let (doc, path) = (card.doc, card.path.clone());
        let spawned = std::thread::Builder::new().name("stage-folder-open".into()).spawn(move || {
            let first = doc.read().and_then(|tree| document_tree::load_first(&tree, &path).map(|loaded| (tree, loaded)));
            match first {
                Ok((tree, loaded)) => {
                    let pending = matches!(&loaded, TreeDoc::Deck(deck) if deck.pending());
                    let _ = out.send(Loaded::First(tree.clone(), loaded));
                    SignalToUI::set_ui_signal();
                    if pending {
                        let _ = out.send(match document_tree::load(&tree, &path) {
                            Ok(whole) => Loaded::Whole(whole),
                            Err(error) => Loaded::Failed(error),
                        });
                    }
                }
                Err(error) => { let _ = out.send(Loaded::Failed(error)); }
            }
            SignalToUI::set_ui_signal();
        });
        if let Err(error) = spawned {
            let (out, rx) = channel();
            let _ = out.send(Loaded::Failed(format!("folder open: {error}")));
            return OpenJob { card, rx };
        }
        OpenJob { card, rx }
    }

    pub fn poll(&self) -> Option<Loaded> {
        self.rx.try_recv().ok()
    }
}

/// One OS watcher over an open folder document, feeding a worker that
/// blocks on it (no timer runs while nothing changes), waits for QUIET and
/// reads the tree again.
pub struct Watch {
    rx: Receiver<Result<Arc<TreeDraft>, String>>,
    _watcher: FileSystemWatcher,
}

impl Watch {
    pub fn start(doc: &'static FolderDoc) -> Result<Watch, String> {
        let (events, changes) = channel::<()>();
        let events = std::sync::Mutex::new(events);
        let watcher = FileSystemWatcher::start(vec![WatchRoot { mount: doc.name.clone(), path: doc.dir.clone() }], move |event| {
            if matches!(event.kind, FileSystemEventKind::RescanRequired { reason: makepad_filesystem_watcher::RescanReason::Startup | makepad_filesystem_watcher::RescanReason::Restart, .. }) { return; }
            // An editor's swap and backup files, and a save's own temporary.
            let quiet = event.path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with('.') || name.ends_with('~'));
            if quiet { return; }
            if let Ok(events) = events.lock() { let _ = events.send(()); }
        })?;
        let (out, rx) = channel();
        std::thread::Builder::new().name("stage-folder-watch".into()).spawn(move || {
            // Ends when the watcher (and its sender) is dropped.
            while changes.recv().is_ok() {
                loop {
                    match changes.recv_timeout(QUIET) {
                        Ok(()) => {}
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                if out.send(doc.read()).is_err() { return; }
                SignalToUI::set_ui_signal();
            }
        }).map_err(|error| error.to_string())?;
        Ok(Watch { rx, _watcher: watcher })
    }

    /// The newest reading since the last poll.
    pub fn poll(&self) -> Option<Result<Arc<TreeDraft>, String>> {
        self.rx.try_iter().last()
    }
}

/// The folder document a page has open: its watcher, and a save refused
/// for files that changed on disk too (the next save overwrites those).
pub struct OpenFolder {
    pub doc: &'static FolderDoc,
    watch: Option<Watch>,
    conflict: Vec<String>,
}

impl OpenFolder {
    pub fn open(doc: &'static FolderDoc) -> OpenFolder {
        // A read-only copy does not change under Stage: nothing to watch.
        let watch = doc.writable.then(|| Watch::start(doc).map_err(|error| log!("folder docs: not watching {}: {error}", doc.dir.display())).ok()).flatten();
        OpenFolder { doc, watch, conflict: Vec::new() }
    }

    pub fn poll(&self) -> Option<Result<Arc<TreeDraft>, String>> {
        self.watch.as_ref()?.poll()
    }

    /// Stage is closing: the watcher and its thread end.
    pub fn stop_watching(&mut self) {
        self.watch = None;
    }

    /// Save `tree` over `saved` in the folder. A file changed on disk too
    /// is refused, saying so; an `explicit` save (the person's, not the
    /// autosave) after that refusal writes Stage's version over it.
    pub fn save(&mut self, saved: &TreeDraft, tree: &TreeDraft, explicit: bool) -> Result<usize, String> {
        let overwrite = if explicit { self.conflict.clone() } else { Vec::new() };
        match self.doc.write(saved, tree, &overwrite) {
            Ok(written) => {
                self.conflict.clear();
                Ok(written)
            }
            Err(WriteRefused::Conflict(paths)) => {
                let list = paths.join(", ");
                self.conflict = paths;
                Err(format!("Cannot save {list}: it changed on disk while you edited it here, the disk's version stays. Save writes Stage's version over it (git keeps the other)"))
            }
            Err(WriteRefused::Failed(error)) => Err(format!("Not saved: {error}")),
        }
    }

    /// Leaving this document with changes (another one opens): what the
    /// folder can take is written, a file that changed on disk too (or every
    /// change, in a read-only copy) is kept aside in Stage's data folder, so
    /// switching never waits and never writes over an outside change.
    /// Returns the status line.
    pub fn save_or_keep(&mut self, saved: &TreeDraft, tree: &TreeDraft) -> String {
        let kept: Vec<String> = match self.doc.writable.then(|| self.doc.write(saved, tree, &[])) {
            Some(Ok(written)) => {
                self.conflict.clear();
                return format!("Saved {written} file{} to {}", if written == 1 { "" } else { "s" }, self.doc.dir.display());
            }
            Some(Err(WriteRefused::Conflict(paths))) => {
                // The other files still go in: the conflicting ones stay as the disk has them.
                let mut rest = tree.clone();
                for path in &paths {
                    match saved.files.get(path) {
                        Some(file) => { rest.files.insert(path.clone(), file.clone()); }
                        None => { rest.files.remove(path); }
                    }
                }
                if let Err(WriteRefused::Failed(error)) = self.doc.write(saved, &rest, &[]) { log!("folder docs: {error}"); }
                paths
            }
            Some(Err(WriteRefused::Failed(error))) => {
                log!("folder docs: {error}");
                document_tree::changed_files(saved, tree)
            }
            None => document_tree::changed_files(saved, tree),
        };
        self.conflict.clear();
        let aside = makepad_library_browser::drafts::dir().join("folder-kept").join(&self.doc.name);
        for path in &kept {
            let Some(file) = tree.files.get(path) else { continue };
            let target = aside.join(path);
            let written = target.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&target, &file.bytes));
            if let Err(error) = written { log!("folder docs: {}: {error}", target.display()); }
        }
        let line = format!("Stage's version of {} kept in {} ({})", kept.join(", "), aside.display(), if self.doc.writable { "it changed on disk too" } else { "the folder is read-only" });
        log!("folder docs: {line}");
        line
    }
}

/// Every folder card in one numbering: the deck cards, then the cut cards.
pub fn card_number(card: &Card) -> usize {
    let same = |other: &Card| std::ptr::eq(other.doc, card.doc) && other.path == card.path;
    let decks = deck_cards();
    decks.iter().position(same).or_else(|| cut_cards().iter().position(same).map(|at| decks.len() + at)).unwrap_or(usize::MAX)
}

fn card_at(number: usize) -> Option<&'static Card> {
    let decks = deck_cards();
    decks.get(number).or_else(|| number.checked_sub(decks.len()).and_then(|at| cut_cards().get(at)))
}

/// The still ids of folder cards (a page's Stills host): apart from the
/// slide list's and the library rows'.
const CARD_STILL_IDS: usize = usize::MAX / 2;

/// The picture a card shows: its last thumbnail (kept per card file), else a
/// plain card in the slides' dark ground, until its first slide's still is in.
const PLACEHOLDER: (usize, usize) = (224, 126);
const PLACEHOLDER_GROUND: u32 = 0xff16_161b;

fn last_still(card: &Card) -> PathBuf {
    makepad_asset_client::paths::library_root().join("tree-stills").join("folder-cards").join(format!("{}__{}.png", card.doc.name, card.path.replace('/', "__")))
}

/// What the background service knows of the folder cards: each card's
/// still source (its first slide, read from the folder; renewed when the
/// folder changes) and the picture to show until the still is in.
#[derive(Default)]
struct CardState {
    revision: u64,
    specs: HashMap<usize, Option<Arc<crate::still_cache::Spec>>>,
    pictures: HashMap<usize, Arc<(usize, usize, Vec<u32>)>>,
    /// When each card's document last changed: the newest file it reads (ms).
    modified: HashMap<usize, u64>,
}

/// The newest modification time (ms since the epoch) of a document file and
/// every file it reads, transitively (its slides, plates, modules, media).
fn last_modified(doc: &FolderDoc, tree: &Arc<TreeDraft>, path: &str) -> u64 {
    let mut seen = BTreeSet::new();
    let mut pending = vec![path.to_string()];
    while let Some(path) = pending.pop() {
        if !seen.insert(path.clone()) { continue; }
        let Some(file) = tree.files.get(&path) else { continue };
        if !path.ends_with(".splash") { continue; }
        let Ok(syntax) = file.text().and_then(|text| makepad_widgets::makepad_script::syntax::parse(text).map_err(|error| format!("{error:?}"))) else { continue };
        let context = tree.context(&path);
        for (_, reference) in makepad_asset_client::tree::references(&syntax).unwrap_or_default() {
            if let Ok(makepad_asset_data::tree::TreeLookup::File(found)) = context.lookup(&reference) { pending.push(found.path); }
        }
    }
    seen.iter().filter_map(|path| std::fs::metadata(doc.dir.join(path)).and_then(|meta| meta.modified()).ok())
        .filter_map(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|age| age.as_millis() as u64).max().unwrap_or(0)
}

fn card_state() -> &'static std::sync::Mutex<CardState> {
    static STATE: OnceLock<std::sync::Mutex<CardState>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}

/// The card sources of one folder's cards, read from its files now.
fn read_card_specs(doc: &'static FolderDoc) -> Vec<(usize, Option<Arc<crate::still_cache::Spec>>, u64)> {
    let tree = match doc.read() {
        Ok(tree) => tree,
        Err(error) => { log!("folder docs: card pictures of {}: {error}", doc.name); return Vec::new() }
    };
    let cards = deck_cards().iter().chain(cut_cards()).filter(|card| std::ptr::eq(card.doc, doc));
    cards.map(|card| {
        let spec = if card.is_deck() {
            document_tree::load_first(&tree, &card.path).and_then(|loaded| super::store::card_spec(loaded, None))
        } else {
            document_tree::load(&tree, &card.path).and_then(|loaded| super::store::card_spec(loaded, Some(tree.context(&card.path))))
        };
        let spec = spec.map_err(|error| log!("folder docs: card picture of {}/{}: {error}", doc.name, card.path)).ok().flatten();
        (card_number(card), spec.map(Arc::new), last_modified(doc, &tree, &card.path))
    }).collect()
}

/// The service's thread, started once: the kept pictures first (a warm start
/// shows them at once), then every card's source, then it blocks on one
/// watcher over the folders and reads a changed folder's cards again.
fn start_cards() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new().name("stage-folder-cards".into()).spawn(|| {
            let publish = |update: &mut dyn FnMut(&mut CardState)| {
                if let Ok(mut state) = card_state().lock() { update(&mut state); state.revision += 1; }
                SignalToUI::set_ui_signal();
            };
            for number in 0..deck_cards().len() + cut_cards().len() {
                let Some(card) = card_at(number) else { continue };
                let picture = std::fs::read(last_still(card)).ok().and_then(|png| ImageBuffer::from_png(&png).ok()).map(|image| (image.width, image.height, image.data))
                    .unwrap_or_else(|| (PLACEHOLDER.0, PLACEHOLDER.1, vec![PLACEHOLDER_GROUND; PLACEHOLDER.0 * PLACEHOLDER.1]));
                publish(&mut |state| { state.pictures.insert(number, Arc::new(picture.clone())); });
            }
            for doc in docs() {
                let specs = read_card_specs(doc);
                publish(&mut |state| for (number, spec, modified) in &specs {
                    state.specs.insert(*number, spec.clone());
                    state.modified.insert(*number, *modified);
                });
            }
            let roots: Vec<WatchRoot> = content_roots().iter().filter(|(_, writable)| *writable).enumerate()
                .map(|(i, (root, _))| WatchRoot { mount: format!("content{i}"), path: root.clone() }).collect();
            if roots.is_empty() { return; }
            let (events, changes) = channel::<PathBuf>();
            let events = std::sync::Mutex::new(events);
            let watcher = FileSystemWatcher::start(roots, move |event| {
                let quiet = event.path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with('.') || name.ends_with('~'));
                if quiet || matches!(event.kind, FileSystemEventKind::RescanRequired { .. }) { return; }
                if let Ok(events) = events.lock() { let _ = events.send(event.path.clone()); }
            });
            let Ok(watcher) = watcher.map_err(|error| log!("folder docs: card pictures do not follow the folders: {error}")) else { return };
            // Stage's quit drops it (stop_cards), which ends this thread.
            {
                let Ok(mut held) = card_watcher().lock() else { return };
                if CARDS_STOPPED.load(std::sync::atomic::Ordering::Acquire) { return; }
                *held = Some(watcher);
            }
            while let Ok(first) = changes.recv() {
                let mut paths = vec![first];
                loop {
                    match changes.recv_timeout(QUIET) {
                        Ok(path) => paths.push(path),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                for doc in docs().iter().filter(|doc| paths.iter().any(|path| path.starts_with(&doc.dir))) {
                    let specs = read_card_specs(doc);
                    publish(&mut |state| for (number, spec, modified) in &specs {
                    state.specs.insert(*number, spec.clone());
                    state.modified.insert(*number, *modified);
                });
                }
            }
        });
        if let Err(error) = spawned { log!("folder docs: card pictures: {error}"); }
    });
}

static CARDS_STOPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn card_watcher() -> &'static std::sync::Mutex<Option<FileSystemWatcher>> {
    static WATCHER: OnceLock<std::sync::Mutex<Option<FileSystemWatcher>>> = OnceLock::new();
    WATCHER.get_or_init(Default::default)
}

/// Stage is closing: the card service's watcher and thread end.
pub fn stop_cards() {
    CARDS_STOPPED.store(true, std::sync::atomic::Ordering::Release);
    let watcher = card_watcher().lock().ok().and_then(|mut held| held.take());
    drop(watcher);
}

/// A page's side of the folder cards' pictures: textures of the kept
/// pictures and of the stills its Stills host draws.
#[derive(Default)]
pub struct CardPictures {
    seen: Option<u64>,
    specs: HashMap<usize, Option<Arc<crate::still_cache::Spec>>>,
    modified: HashMap<usize, u64>,
    textures: HashMap<usize, Texture>,
    /// The still installed per card, and the source whose still was kept
    /// for the next start (kept once per source, not on every reinstall).
    installed: HashMap<usize, Texture>,
    kept: HashMap<usize, Arc<crate::still_cache::Spec>>,
    stills_seen: Option<u64>,
}

impl CardPictures {
    /// Takes what the service has learned; true when a card changed.
    pub fn pump(&mut self, cx: &mut Cx) -> bool {
        // Prepared after the first present: never a hitch in the first frames.
        if self.seen.is_none() && !cx.passes.id_iter().any(|id| matches!(cx.passes[id].parent, CxDrawPassParent::Window(_)) && cx.passes[id].painted_serial > 0) { return false; }
        start_cards();
        let Ok(state) = card_state().lock() else { return false };
        if self.seen == Some(state.revision) { return false; }
        self.seen = Some(state.revision);
        for (number, picture) in &state.pictures {
            if self.textures.contains_key(number) { continue; }
            let (width, height, pixels) = &**picture;
            self.textures.insert(*number, Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 { width: *width, height: *height, data: Some(pixels.clone()), updated: TextureUpdated::Full }));
        }
        self.specs = state.specs.clone();
        self.modified = state.modified.clone();
        true
    }

    /// The still demands of the visible folder cards: `cards` maps a list
    /// index to its card.
    pub fn demands(&self, visible: &[(usize, f64)], dpi: f64, cards: impl Fn(usize) -> Option<&'static Card>) -> Vec<crate::still_cache::Demand> {
        visible.iter().filter_map(|(index, width)| {
            let number = card_number(cards(*index)?);
            let spec = self.specs.get(&number)?.clone()?;
            let aspect = spec.cut.output.width as f64 / spec.cut.output.height.max(1) as f64;
            Some(crate::still_cache::Demand { id: CARD_STILL_IDS + number, size: crate::still_cache::Size::new(*width, dpi, aspect), spec })
        }).collect()
    }

    /// The stills the host drew: each replaces its card's picture, and is
    /// kept per card file for the next start.
    pub fn accept(&mut self, stills: &crate::still_cache::Stills) -> bool {
        if self.stills_seen == Some(stills.revision) { return false; }
        self.stills_seen = Some(stills.revision);
        let mut changed = false;
        for number in self.specs.keys().copied().collect::<Vec<_>>() {
            let Some(texture) = stills.texture(CARD_STILL_IDS + number) else { continue };
            if self.installed.get(&number) == Some(&texture) { continue; }
            self.installed.insert(number, texture.clone());
            self.textures.insert(number, texture);
            changed = true;
            let Some(Some(spec)) = self.specs.get(&number) else { continue };
            if self.kept.get(&number).is_some_and(|kept| Arc::ptr_eq(kept, spec)) { continue; }
            self.kept.insert(number, spec.clone());
            if let (Some(thumb), Some(card)) = (stills.thumb(CARD_STILL_IDS + number), card_at(number)) {
                let target = last_still(card);
                let _ = std::thread::Builder::new().name("stage-card-keep".into()).spawn(move || {
                    let rgba: Vec<u8> = thumb.pixels.iter().flat_map(|pixel| [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, (pixel >> 24) as u8]).collect();
                    let Ok(png) = Cx::encode_rgba_as_png(thumb.width as u32, thumb.height as u32, &rgba) else { return };
                    let written = target.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
                        let temporary = target.with_extension("png.tmp");
                        std::fs::write(&temporary, &png).and_then(|()| std::fs::rename(&temporary, &target))
                    });
                    if let Err(error) = written { log!("folder docs: {}: {error}", target.display()); }
                });
            }
        }
        changed
    }

    /// A card's picture: its still, else its kept picture or placeholder.
    pub fn texture(&self, card: &Card) -> Option<Texture> {
        self.textures.get(&card_number(card)).cloned()
    }

    /// The card's picture aspect, as library cards take it from their source.
    pub fn aspect(&self, card: &Card) -> Option<f64> {
        self.specs.get(&card_number(card))?.as_ref().map(|spec| spec.cut.output.width as f64 / spec.cut.output.height.max(1) as f64)
    }

    /// The card's line under its title: a live input's name (its first
    /// slide hosts one), else when it last changed, as library cards say it
    /// (plus where it lives when two cards share a title).
    pub fn detail(&self, card: &Card) -> String {
        let number = card_number(card);
        if let Some(name) = self.specs.get(&number).and_then(|spec| spec.as_ref()?.live_name.clone()) { return format!("App · {name}"); }
        let modified = self.modified.get(&number).map(|ms| super::modified(*ms)).unwrap_or_default();
        match (card.detail(), modified.is_empty()) {
            (place, _) if place.is_empty() => modified,
            (place, true) => place,
            (place, false) => format!("{modified} · {place}"),
        }
    }
}

/// The CUT page's song prompt for a folder cut playing without its song: a
/// download through the installer, or a picked file put into the folder.
#[derive(Default)]
pub struct SongDesk {
    install: Option<InstallHandle>,
    progress: Option<(u64, u64)>,
    note: String,
    /// "Not now": the prompt stays away for this run.
    dismissed: bool,
}

const SONG_DIALOG: LiveId = live_id!(cut_example_song);

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

impl super::VideoCut {
    /// The open folder cut, while it plays without its song.
    fn song_missing(&self) -> Option<&'static FolderDoc> {
        let doc = self.folder.as_ref()?.doc;
        let session = self.session.as_ref()?;
        doc.song_path().filter(|path| session.doc.missing_media.iter().any(|missing| missing == path)).map(|_| doc)
    }

    pub(super) fn pump_song(&mut self) {
        let Some(install) = &self.song.install else { return };
        let mut finished = false;
        loop {
            match install.rx.try_recv() {
                Ok(InstallMsg::Progress { done, total, .. }) => self.song.progress = Some((done, total)),
                Ok(InstallMsg::Done { .. }) => self.song.note = "downloaded and verified".into(),
                Ok(InstallMsg::Failed { error, .. }) => self.song.note = format!("not downloaded: {error}"),
                Ok(InstallMsg::Cancelled) => self.song.note = "stopped: Download resumes where it left off".into(),
                Ok(InstallMsg::Finished) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
            }
        }
        if finished {
            self.song.install = None;
            self.song.progress = None;
            // The song is outside the folder: read the folder again for it.
            if let Some(doc) = self.folder.as_ref().map(|folder| folder.doc) {
                let read = doc.read();
                self.folder_changed(read);
            }
        }
        self.repaint = true;
    }

    pub(super) fn paint_song_prompt(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let doc = self.song_missing().filter(|_| !self.song.dismissed);
        // Only an actionable song prompt (a song to download or pick) is a
        // card over the picture; a missing optional file is already said on
        // its clip and in the inspector.
        ui.view(cx, ids!(cut_song)).set_visible(cx, doc.is_some());
        let Some(doc) = doc else { return };
        let missing = self.session.as_ref().map(|session| session.doc.missing_media.as_slice()).unwrap_or_default();
        ui.label(cx, ids!(cut_song_title)).set_text(cx, &missing.iter().map(|path| format!("media not present: {path} (plays silent)")).collect::<Vec<_>>().join("\n"));
        ui.button(cx, ids!(cut_song_dismiss)).set_visible(cx, true);
        let busy = self.song.install.is_some();
        let pin = doc.pin.as_ref();
        let pick = if doc.writable { "; or pick your own copy" } else { "" };
        let source = pin.map(|pin| {
            let host = models::source_url(pin.model).split('/').nth(2).unwrap_or("").to_string();
            format!("\n\nDownload fetches it from {host} ({}), checked against its pinned size and SHA-256 before it is kept{pick}.", megabytes(pin.model.bytes))
        }).unwrap_or_else(|| "\n\nPick your copy of the song (MP3 or WAV): it is put in the folder.".into());
        ui.label(cx, ids!(cut_song_credits)).set_text(cx, &format!("{}{source}", doc.credits));
        let state = match (self.song.progress, &self.song.install) {
            (Some((done, total)), Some(_)) => format!("downloading {}% ({} of {})", done * 100 / total.max(1), megabytes(done), megabytes(total)),
            (None, Some(_)) => "starting the download…".into(),
            _ => self.song.note.clone(),
        };
        ui.label(cx, ids!(cut_song_state)).set_text(cx, &state);
        ui.button(cx, ids!(cut_song_download)).set_visible(cx, pin.is_some() && !busy);
        ui.button(cx, ids!(cut_song_stop)).set_visible(cx, busy);
        ui.button(cx, ids!(cut_song_pick)).set_visible(cx, !busy && doc.writable);
    }

    pub(super) fn song_prompt_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        for action in actions {
            if let Some(FileDialogAction::FileSelected { id, paths }) = action.downcast_ref::<FileDialogAction>() {
                if *id == SONG_DIALOG {
                    if let (Some(doc), Some(picked)) = (self.song_missing(), paths.first()) {
                        // Into the folder at its optional path: the watcher reads it from there.
                        let target = doc.song_path().map(|path| doc.dir.join(path));
                        let copied = target.ok_or_else(|| "no song path".to_string()).and_then(|target| {
                            if let Some(parent) = target.parent() { std::fs::create_dir_all(parent).map_err(|error| error.to_string())?; }
                            std::fs::copy(picked, &target).map(|_| ()).map_err(|error| format!("{}: {error}", target.display()))
                        });
                        self.song.note = match copied { Ok(()) => "the song is in the folder".into(), Err(error) => format!("not added: {error}") };
                        self.repaint = true;
                    }
                }
            }
        }
        let Some(doc) = self.song_missing() else { return };
        let busy = self.song.install.is_some();
        if ui.button(cx, ids!(cut_song_download)).clicked(actions) && !busy {
            if let Some(pin) = &doc.pin {
                if pin.installed() {
                    let read = doc.read();
                    self.folder_changed(read);
                } else {
                    // Pressing Download is the acknowledgement of the notice above it.
                    self.song.install = Some(models::start_install(vec![pin.model], Some(models::unix_now())));
                    self.song.note = String::new();
                }
                self.repaint = true;
            }
        }
        if ui.button(cx, ids!(cut_song_stop)).clicked(actions) {
            if let Some(install) = &self.song.install {
                install.cancel();
            }
        }
        if ui.button(cx, ids!(cut_song_pick)).clicked(actions) && !busy && doc.writable {
            let mut dialog = FileDialog::new().set_title("The song (MP3 or WAV)".to_string()).add_filter("Audio (MP3, WAV)".to_string(), vec!["mp3".to_string(), "wav".to_string()]);
            dialog.id = SONG_DIALOG;
            cx.open_select_file_dialog(dialog);
        }
        if ui.button(cx, ids!(cut_song_dismiss)).clicked(actions) {
            self.song.dismissed = true;
            self.repaint = true;
        }
    }
}

impl super::VideoCut {
    /// The folder cuts the Cuts lane lists (after the library's cuts and drafts).
    /// (The Cuts lane: the folder cuts; the Decks lane: the folder decks.)
    pub(super) fn folder_cards(&self) -> impl Iterator<Item = &'static Card> + '_ {
        let cards = match self.lib.lane {
            Some(super::PageLane::Store(super::LibLane::Cuts)) => cut_cards(),
            Some(super::PageLane::Store(super::LibLane::Decks)) => deck_cards(),
            _ => &[],
        };
        cards.iter().filter(move |card| card.matches(&self.lib.search))
    }

    pub(super) fn folder_rows(&self) -> Vec<super::ListRow> {
        self.folder_cards().map(|card| super::ListRow {
            title: card.title.clone(),
            detail: self.card_pictures.detail(card),
            kind: if self.lib.lane == Some(super::PageLane::Store(super::LibLane::Decks)) { super::CardKind::Deck } else { super::CardKind::Cut },
            texture: self.card_pictures.texture(card),
            aspect: self.card_pictures.aspect(card),
            ..super::ListRow::default()
        }).collect()
    }

    /// The folder card at a list index.
    pub(super) fn folder_at(&self, index: usize) -> Option<&'static Card> {
        let first = self.lib.rows.len() + self.draft_rows().len();
        index.checked_sub(first).and_then(|at| self.folder_cards().nth(at))
    }

    /// Stage is closing: the watcher (and its thread) stops.
    pub(crate) fn close_folder(&mut self) {
        if let Some(folder) = &mut self.folder { folder.stop_watching(); }
    }

    /// Open a folder cut card in place.
    pub(super) fn open_folder_cut(&mut self, card: &Card) {
        let opened = card.doc.read().and_then(|draft| Ok((draft.text(&card.path)?.to_string(), draft)));
        match opened {
            Ok((source, draft)) => {
                self.crumbs.clear();
                self.status = format!("Opening “{}”…", card.title);
                self.submit(super::Pending::Open { source, head: None, saved: true, tree: None, folder: Some(super::FolderOpen { doc: card.doc, draft, path: card.path.clone(), reload: false }) });
            }
            Err(error) => {
                self.status = format!("Could not open “{}”: {error}", card.title);
                self.open_error = self.status.clone();
            }
        }
        self.repaint = true;
    }

    /// The open folder cut's files changed on disk; an edit that stood
    /// still for a moment is saved into the folder (never over an outside
    /// change: that is said, with the file, and Save writes Stage's version).
    pub(super) fn pump_folder(&mut self) {
        if let Some(read) = self.folder.as_ref().and_then(OpenFolder::poll) {
            self.folder_changed(read);
        }
        let Some(session) = self.session.as_ref().filter(|_| self.folder.as_ref().is_some_and(|folder| folder.doc.writable)) else { self.folder_seen = None; return };
        if !session.dirty() { self.folder_seen = None; return; }
        let revision = session.revision;
        let now = std::time::Instant::now();
        let since = match self.folder_seen {
            Some((seen, since)) if seen == revision => since,
            _ => { self.folder_seen = Some((revision, now)); now }
        };
        let busy = self.saving || self.save_requested || self.in_flight.is_some() || !self.queue.is_empty() || super::motion_edit::has_drafts(self);
        if busy || self.folder_refused == Some(revision) || now.duration_since(since) < AUTOSAVE_AFTER { return; }
        self.save_in_place(false);
    }

    /// Another document replaces the open folder cut: its changes go into the
    /// folder now, a file that changed on disk too is kept aside (switching
    /// never waits).
    pub(super) fn leave_folder(&mut self) {
        let (Some(folder), Some(session)) = (self.folder.as_mut(), self.session.as_ref()) else { return };
        if !session.dirty() { return; }
        let (Some(saved), Some(tree)) = (session.saved_tree.clone(), session.tree.clone()) else { return };
        self.status = folder.save_or_keep(&saved, &tree);
    }

    /// The folder as read again: the open cut reloads at its playhead, unless
    /// it has changes of its own (those stay; Save writes them).
    pub(super) fn folder_changed(&mut self, read: Result<Arc<TreeDraft>, String>) {
        let Some(doc) = self.folder.as_ref().map(|folder| folder.doc) else { return };
        self.repaint = true;
        let tree = match read {
            Ok(tree) => tree,
            Err(error) => {
                self.status = format!("“{}” changed on disk but does not read: {error}", doc.name);
                return;
            }
        };
        let Some(session) = &self.session else { return };
        // What is open is what the disk holds (Stage's own save).
        if session.tree.as_ref() == Some(&tree) { return; }
        if session.dirty() || self.saving || self.in_flight.is_some() || !self.queue.is_empty() || super::motion_edit::has_drafts(self) {
            log!("edit: {} changed on disk while the cut has unsaved changes: they stay", doc.name);
            self.status = format!("“{}” changed on disk: your unsaved changes stay; Save writes the files you changed", doc.name);
            return;
        }
        let path = if tree.files.contains_key(&session.path) { session.path.clone() } else { tree.root.1.clone() };
        let Ok(source) = tree.text(&path).map(str::to_string) else { return };
        self.submit(super::Pending::Open { source, head: None, saved: true, tree: None, folder: Some(super::FolderOpen { doc, draft: tree, path, reload: true }) });
    }

    /// Save (`explicit`) or the autosave writes the changed files back into the folder.
    pub(super) fn save_in_place(&mut self, explicit: bool) {
        let (Some(folder), Some(session)) = (self.folder.as_mut(), self.session.as_mut()) else { return };
        let (Some(saved), Some(tree)) = (session.saved_tree.clone(), session.tree.clone()) else { return };
        match folder.save(&saved, &tree, explicit) {
            Ok(0) => self.status = "Already saved".into(),
            Ok(written) => {
                session.saved = session.source.clone();
                session.saved_doc = Some(session.doc.clone());
                session.saved_path = session.path.clone();
                session.saved_tree = Some(tree);
                self.saved_at = Some(makepad_civil_time::local_now().hm());
                self.save_error = None;
                self.status = format!("Saved {written} file{} to {}", if written == 1 { "" } else { "s" }, folder.doc.dir.display());
            }
            Err(error) => {
                log!("edit: {error}");
                self.folder_refused = Some(session.revision);
                self.save_error = Some(error.clone());
                self.status = error;
                self.status_sticky = true;
            }
        }
        self.repaint = true;
    }
}

/// How long an edit of a folder cut stands still before it is saved.
const AUTOSAVE_AFTER: Duration = Duration::from_millis(1500);

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_story_cut::{CutNodeKind, MediaRef};

    fn content() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../stage/content")
    }

    /// A scratch folder for one test, removed when it ends.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("stage-folder-docs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("slides")).unwrap();
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn copy_motion_deck(dir: &Path) {
        for (path, bytes) in makepad_motion::tree::folder_files(&content().join("motion-deck")).unwrap() {
            let target = dir.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, bytes).unwrap();
        }
    }

    /// The Motion deck, copied into a scratch folder.
    fn scratch_deck(name: &str) -> (Scratch, &'static FolderDoc) {
        let scratch = Scratch::new(name);
        copy_motion_deck(&scratch.0);
        std::fs::write(scratch.0.join("notes.md"), "notes\n").unwrap();
        let doc = Box::leak(Box::new(FolderDoc::at(&scratch.0, true).unwrap().unwrap()));
        (scratch, doc)
    }

    fn deck_titled(title: &str) -> String {
        std::fs::read_to_string(content().join("motion-deck/deck.splash")).unwrap().replace("title: \"Motion\"", &format!("title: \"{title}\""))
    }

    #[test]
    fn the_same_decks_are_listed_before_and_after_story_moves() {
        let scratch = Scratch::new("library");
        let base = scratch.0.canonicalize().unwrap();
        let commercial = base.join("apps/commercial");
        let open = base.join("apps/apps");
        let private_root = commercial.join("stage/content");
        let open_root = open.join("stage/content");
        let extra = base.join("coast");
        let write_deck = |dir: &Path, title: &str| {
            copy_motion_deck(dir);
            std::fs::write(dir.join("deck.splash"), deck_titled(title)).unwrap();
        };
        write_deck(&private_root.join("orchard"), "Orchard");
        std::fs::write(private_root.join("orchard/bloom.deck.splash"), deck_titled("Blossom")).unwrap();
        write_deck(&private_root.join("seedlings"), "Seedlings");
        write_deck(&extra.join("harbour"), "Harbour");
        std::fs::write(private_root.join(LIBRARY), r#"{"decks":[
            "orchard/deck.splash", "orchard/bloom.deck.splash", "weather/deck.splash"
        ]}"#).unwrap();
        let settings = format!("# More decks\n{}\n{} # same root\n", extra.display(), extra.join(".").display());

        // The example travels with Story. The person's Library and settings stay in place.
        for (repo, own, expected_roots) in [
            (&commercial, &private_root, vec![&private_root, &extra]),
            (&open, &open_root, vec![&open_root, &private_root, &extra]),
        ] {
            let manifest = repo.join("story/app");
            std::fs::create_dir_all(&manifest).unwrap();
            let example = own.join("weather");
            write_deck(&example, "Weather");
            let roots = roots_of(&manifest, &settings);
            assert_eq!(roots, expected_roots.into_iter().map(|root| (root.clone(), true)).collect::<Vec<_>>());

            let folders = docs_in(&roots);
            let listed = listed_decks(&roots, &folders);
            let cards: Vec<_> = listed.iter().map(|(name, path)| {
                let doc = folders.iter().find(|doc| doc.name == *name).unwrap();
                assert!(doc.decks.contains(path), "{name}/{path}");
                (name.as_str(), path.as_str(), doc.title_of(path))
            }).collect();
            assert_eq!(cards, [
                ("orchard", "deck.splash", "Orchard".to_string()),
                ("orchard", "bloom.deck.splash", "Blossom".to_string()),
                ("weather", "deck.splash", "Weather".to_string()),
                ("harbour", "deck.splash", "Harbour".to_string()),
            ]);
            if repo == &open {
                // The unlisted example root precedes the unlisted settings root.
                std::fs::write(private_root.join(LIBRARY), r#"{"decks":["orchard/deck.splash","orchard/bloom.deck.splash"]}"#).unwrap();
                assert_eq!(listed_decks(&roots, &folders), listed);
            }
            std::fs::remove_dir_all(example).unwrap();
        }
    }

    #[test]
    fn every_content_folder_is_a_folder_doc_and_reads_as_its_tree() {
        assert!(!docs().is_empty());
        for doc in docs() {
            let name = &doc.name;
            let tree = doc.read().unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(tree.files.contains_key(&doc.root), "{name}");
            for path in tree.files.keys() { makepad_asset_client::tree::validate_path(path).unwrap(); }
            // Media is named by a folder handle, documents by their bytes only.
            for file in tree.files.values() {
                let media = matches!(file.kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio);
                assert_eq!(file.reference.is_some(), media, "{name}: {}", file.path);
            }
            assert!(!tree.files.contains_key("media.txt"));
        }
    }

    fn checked_decks(doc: &FolderDoc) -> HashMap<String, Arc<makepad_story_deck::DeckDoc>> {
        makepad_doc_fonts::register_root("makepad_widgets", makepad_widgets::widgets_dir());
        let tree = doc.read().unwrap();
        assert_eq!(document_tree::deck_files(&tree), doc.decks);
        let decks: HashMap<_, _> = doc.decks.iter().map(|path| {
            let deck = document_tree::load_deck_at(&tree, path).unwrap_or_else(|e| panic!("{}/{path}: {e}", doc.name));
            (path.clone(), deck)
        }).collect();
        for (path, deck) in &decks {
            let problems = makepad_story_deck::check_links(deck, |target| decks.get(target).map(Arc::as_ref));
            assert!(problems.is_empty(), "{}/{path}: {problems:?}", doc.name);
        }
        let problems = document_tree::check_deck_links(&tree);
        assert!(problems.is_empty(), "{}: {problems:?}", doc.name);
        assert!(tree.files.values().filter(|file| file.path.ends_with(".mp4")).all(|file| file.kind == AssetKind::Video));
        decks
    }

    #[test]
    fn every_folders_decks_load_and_their_links_resolve() {
        for doc in docs() { checked_decks(doc); }
    }

    #[test]
    fn two_cards_in_one_folder_link_to_a_named_slide() {
        let scratch = Scratch::new("links");
        let dir = scratch.0.join("garden");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("deck.splash"), r#"use mod.deck.*
Deck{title: "Garden" slides: [
    Slide{name: @gate sketch: "Garden gate"}
    Slide{name: @pond sketch: "The pond"}
]}"#).unwrap();
        std::fs::write(dir.join("path.deck.splash"), r#"use mod.deck.*
Deck{title: "Path" slides: [
    Slide{name: @turn sketch: "Follow the path" link: "deck.splash" link_slide: @pond}
]}"#).unwrap();
        // Only the container type matters when the tree classifies a video.
        std::fs::write(dir.join("pond.mp4"), b"\x00\x00\x00\x14ftypisom\x00\x00\x00\x00isom").unwrap();
        std::fs::write(scratch.0.join(LIBRARY), r#"{"decks":["garden/deck.splash","garden/path.deck.splash"]}"#).unwrap();

        let roots = [(scratch.0.clone(), true)];
        let folders = docs_in(&roots);
        assert_eq!(folders.len(), 1);
        let doc = &folders[0];
        assert_eq!(doc.root, "deck.splash");
        assert_eq!(doc.decks, ["deck.splash", "path.deck.splash"]);
        assert_eq!(listed_decks(&roots, &folders), [
            ("garden".to_string(), "deck.splash".to_string()),
            ("garden".to_string(), "path.deck.splash".to_string()),
        ]);
        assert_eq!(doc.title_of("deck.splash"), "Garden");
        assert_eq!(doc.title_of("path.deck.splash"), "Path");
        let decks = checked_decks(doc);
        let link = decks["path.deck.splash"].slides[0].link.as_ref().unwrap();
        assert_eq!(link.path, "deck.splash");
        assert_eq!(link.entry(&decks[&link.path]), Ok(1));
    }

    #[test]
    fn folder_media_plays_from_disk_through_the_store_resolver() {
        // One green RGB pixel.
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x28, 0x98, 0xed, 0x05, 0x00, 0x02, 0xd4, 0x01, 0x56, 0xcd, 0x4b, 0x84,
            0x99, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let (scratch, doc) = scratch_deck("media");
        let path = "leaf.png";
        std::fs::write(scratch.0.join(path), PNG).unwrap();
        let tree = doc.read().unwrap();
        let file = &tree.files[path];
        assert_eq!(file.bytes.as_ref(), PNG);
        let handle = file.reference.unwrap();
        let asset = makepad_story_cut::AssetRef { id: handle.asset_id.to_string(), rev: handle.revision.to_string() };
        let resolved = makepad_story_edit_render::media::folder::resolve(&MediaRef::Asset(asset.clone())).unwrap().unwrap();
        assert_eq!(resolved.path, doc.dir.join(path));
        let (bytes, kind) = makepad_story_edit_render::media::folder::bytes(&asset).unwrap().unwrap();
        assert_eq!((bytes.as_slice(), kind), (file.bytes.as_ref(), "png"));
        // A store copy of the tree carries bytes, never folder handles.
        assert!(for_store(&tree).files.values().all(|file| file.reference.is_none() && file.base.is_none()));
    }

    #[test]
    fn a_cut_folder_reads_in_place_and_plays_without_its_optional_song() {
        // The Library caches its roots on first use. Give the test its own
        // settings and download cache before opening the Library.
        const SCRATCH_ROOT: &str = "STORY_TEST_CUT_ROOT";
        let Some(base) = std::env::var_os(SCRATCH_ROOT).map(PathBuf::from) else {
            let scratch = Scratch::new("optional-song");
            let base = scratch.0.canonicalize().unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("a_cut_folder_reads_in_place_and_plays_without_its_optional_song")
                .env(SCRATCH_ROOT, &base)
                .env("MAKEPAD_HOME", &base)
                .env_remove("MAKEPAD_ASSET_AI_CACHE")
                .status().unwrap();
            assert!(status.success());
            assert!(base.join("story/content-roots.txt").is_file(), "the child did not exercise the fixture");
            return;
        };

        let root = base.join("content");
        let dir = root.join(format!("estuary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tide.splash"), "use mod.motion.*\nMotion{duration: 1.0 layers: [Rect{size: vec2(8, 8) fill: #26888c}]}").unwrap();
        std::fs::write(dir.join("tide.cut.splash"), r#"use mod.flow.*
use mod.cut.*
let water = Graphic{src: "tide.splash"}
let chimes = AudioClip{src: "chimes.wav" span: [0.0, 1.0]}
let out = CutOutput{frame: water.out(@frame) audio: [conn(1, chimes.out(@audio))]}
Flow{kind: @cut label: "High tide" output: {width: 64 height: 64 fps: 24}
    duration: 1.0 water, chimes, out}
"#).unwrap();
        std::fs::write(dir.join("media.tree"), "optional chimes.wav\n").unwrap();
        // The pin describes ten silent 16-bit mono samples in an 8 kHz WAV.
        std::fs::write(dir.join("song.pin"), "path chimes.wav
url https://example.invalid/chimes.wav
bytes 64
sha256 a7e800b0ad361814c3d2ada82bdb63f8b59aa553207658907043128e4052a0ec
").unwrap();
        std::fs::write(dir.join("chimes_credits.txt"), "Estuary chimes\n").unwrap();

        let settings = settings_roots();
        assert!(settings.starts_with(&base));
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(settings, root.to_str().unwrap()).unwrap();
        let cards: Vec<_> = cut_cards().iter().filter(|card| card.doc.dir == dir).collect();
        assert_eq!(cards.len(), 1);
        let card = cards[0];
        assert_eq!(card.path, "tide.cut.splash");
        assert_eq!(card.title, "High tide");
        let doc = card.doc;
        let pin = doc.pin.as_ref().unwrap();
        assert_eq!(pin.model.bytes, 64);
        assert_eq!(pin.path, "chimes.wav");
        assert_eq!(doc.song_path(), Some(pin.path.as_str()));
        assert_eq!(doc.credits, "Estuary chimes");
        assert_eq!(doc.root, card.path);
        let tree = doc.read().unwrap();
        assert!(!tree.files.contains_key(&pin.path));
        let cut = document_tree::load(&tree, &card.path).unwrap().into_cut().unwrap();
        assert_eq!(cut.label, "High tide");
        assert_eq!(cut.graphics.len(), 1);
        assert!(matches!(&cut.node("chimes").unwrap().kind, CutNodeKind::AudioClip { src: MediaRef::MissingOptional(path), .. } if path == &pin.path));
        assert_eq!(cut.duration, 1.0);
        assert!(makepad_story_cut::CutEval::new(&cut).audio_plan(0.0, cut.duration).segments.is_empty());
    }

    /// Every listed deck and every folder's slide plates load through their own tree.
    #[test]
    fn every_listed_deck_loads_and_its_slides_load_clean() {
        makepad_doc_fonts::register_root("makepad_widgets", makepad_widgets::widgets_dir());
        assert!(!deck_cards().is_empty());
        for card in deck_cards() {
            let tree = card.doc.read().unwrap();
            let deck = document_tree::load_deck_at(&tree, &card.path).unwrap_or_else(|error| panic!("{}/{}: {error}", card.doc.name, card.path));
            assert!(!deck.slides.is_empty(), "{}", card.title);
        }
        for doc in docs() {
            let name = &doc.name;
            let tree = doc.read().unwrap();
            for (path, file) in &tree.files {
                // The slide plates (`*_slide.splash`); slides/ also holds data modules.
                if file.kind != AssetKind::Motion || !path.starts_with("slides/") || !path.ends_with("_slide.splash") { continue; }
                let motion = makepad_motion::MotionContext { modules: makepad_motion::modules::Modules::from_tree(tree.context(path)), ..Default::default() };
                makepad_motion::MotionDoc::load_with(file.text().unwrap(), &[], motion)
                    .unwrap_or_else(|d| panic!("{name}/{path}:\n{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
            }
        }
    }

    /// An edit is written back into the folder; a file changed on disk since
    /// it was read is refused once, then overwritten when asked.
    #[test]
    fn a_save_writes_the_changed_files_and_refuses_a_file_changed_on_disk_once() {
        let (scratch, doc) = scratch_deck("save");
        let saved = doc.read().unwrap();
        let mut edited = (*saved).clone();
        let deck = deck_titled("Edited");
        edited.write("deck.splash", AssetKind::Deck, deck.as_bytes().into()).unwrap();
        edited.write("slides/two.splash", AssetKind::Motion, b"// new\n".as_slice().into()).unwrap();
        edited.files.remove("notes.md");
        let mut open = OpenFolder { doc, watch: None, conflict: Vec::new() };
        assert_eq!(open.save(&saved, &edited, true), Ok(3));
        assert_eq!(std::fs::read_to_string(scratch.0.join("deck.splash")).unwrap(), deck);
        assert!(scratch.0.join("slides/two.splash").is_file());
        assert!(!scratch.0.join("notes.md").exists());
        assert!(!std::fs::read_dir(&scratch.0).unwrap().flatten().any(|entry| entry.file_name().to_string_lossy().ends_with(".stage-save")));
        // Someone else edits the deck file; Stage's next edit of it is refused once.
        let saved = doc.read().unwrap();
        std::fs::write(scratch.0.join("deck.splash"), deck_titled("Theirs")).unwrap();
        let mut ours = (*saved).clone();
        ours.write("deck.splash", AssetKind::Deck, deck_titled("Ours").as_bytes().into()).unwrap();
        let refused = open.save(&saved, &ours, false).unwrap_err();
        assert!(refused.contains("Cannot save deck.splash: it changed on disk"), "{refused}");
        // The autosave never writes over it, however often it tries.
        assert!(open.save(&saved, &ours, false).is_err());
        assert!(std::fs::read_to_string(scratch.0.join("deck.splash")).unwrap().contains("Theirs"));
        assert_eq!(open.save(&saved, &ours, true), Ok(1));
        assert!(std::fs::read_to_string(scratch.0.join("deck.splash")).unwrap().contains("Ours"));
        // A read-only copy is never written.
        let read_only = FolderDoc::at(&scratch.0, false).unwrap().unwrap();
        assert!(matches!(read_only.write(&saved, &ours, &[]), Err(WriteRefused::Failed(_))));
    }

    /// An edit on disk reaches the watcher's reading once the folder is
    /// quiet, without any polling of the files.
    #[test]
    fn the_watcher_reads_the_folder_again_after_an_edit_on_disk() {
        let (scratch, doc) = scratch_deck("watch");
        let watch = Watch::start(doc).unwrap();
        assert!(watch.poll().is_none());
        std::fs::write(scratch.0.join("deck.splash"), deck_titled("Changed")).unwrap();
        let mut read = None;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(50));
            if let Some(tree) = watch.poll() { read = Some(tree.unwrap()); break; }
        }
        let tree = read.expect("the edit was seen");
        assert!(tree.text("deck.splash").unwrap().contains("Changed"));
        assert_eq!(source_title(tree.text("deck.splash").unwrap()).as_deref(), Some("Changed"));
    }
}
