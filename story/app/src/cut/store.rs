//! The CUT page's asset worker: its library lanes and their thumbnails
//! (the library browser's `lanes`, with the cut rows' trees and renders,
//! [`StoryRows`]), opening and publishing cuts, renders and dependents. One long-lived
//! worker owns an asset client; the UI thread only `try_send`s jobs and
//! `try_recv`s replies (bounded both ways, a full queue is retried by the
//! caller on a later pump).

use makepad_asset_connection as asset_connection;
use makepad_asset_connection::redact;
use makepad_asset_client::api::CatalogQuery;
use makepad_asset_client::dto::DependentsDto;
use makepad_library_browser::lanes::{catalogue, catalogue_page, load_thumb, poster_pending, CutRows, LibLane, LibRow, RenderState, Thumb, TreeDocument, TREE_FILE};
use makepad_asset_client::tree::{self, Tree, TreeEntry, TreeRole};
use makepad_asset_client::resolver::{select_file, TierPreference};
use makepad_asset_client::{AssetClient, PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetId, AssetKind, AssetRevisionRef, DeviceTier, FileRole, MediaType, ThumbnailMedia};
use makepad_story_edit_render::RenderResult;
use makepad_story_cut::{AssetRef, CutDoc, Dep};
use std::collections::HashMap;
use makepad_widgets::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::time::Duration;

const QUEUE: usize = 8;
/// A cut's source; far above any timeline, and refused past it.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const NAMESPACE: &str = "stage";
const WORKER_TICK: Duration = Duration::from_millis(250);
const PUBLISH_WAIT: Duration = Duration::from_millis(20);

/// A selected item's extra facts (design §4.8).
#[derive(Clone, Debug, Default)]
pub struct Details {
    pub tags: Vec<String>,
    /// A cut's output (`width`, `height`, `fps`), length and contents
    /// ("3 clips · 1 effect · 1 VO").
    pub edit: Option<((u32, u32, f64), f64, String)>,
}

/// One of the newest edits, for the blank page.
#[derive(Clone, Debug)]
pub struct Recent {
    pub reference: AssetRevisionRef,
    pub tree: Option<TreeDocument>,
    pub title: String,
    /// Epoch ms of its head.
    pub published: u64,
    pub render: RenderState,
    pub poster_pending: bool,
}

/// What a save publishes.
pub struct SaveRequest {
    /// The kind of document published: a cut or a deck.
    pub kind: AssetKind,
    pub source: String,
    pub title: String,
    pub deps: Vec<Dep>,
    /// The cut asset this is a new revision of.
    pub asset: Option<AssetId>,
    /// The preview picture, `0xAARRGGBB`.
    pub thumbnail: Thumb,
    /// The name the revision keeps (a poster of a cut that has one).
    pub alias: Option<makepad_asset_data::AssetAlias>,
}


pub struct OpenedDocument {
    pub source: String,
    pub tree: Option<OpenTree>,
}

/// Both pages publish their complete working snapshot as one tree revision.
pub struct TreeSave {
    pub draft: Arc<makepad_asset_data::tree::TreeDraft>,
    pub head: Option<AssetRevisionRef>,
    pub alias: Option<makepad_asset_data::AssetAlias>,
    pub thumbnail: Thumb,
    pub path: String,
    /// True only for a settled final frame, never a fallback sketch.
    pub settled: bool,
    pub dpi: f64,
}

#[derive(Clone, Debug)]
pub struct OpenTree {
    pub head: AssetRevisionRef,
    pub alias: makepad_asset_data::AssetAlias,
    pub draft: Arc<makepad_asset_data::tree::TreeDraft>,
    pub doc: super::open_doc::TreeDoc,
    pub path: String,
}

pub enum StoreJob {
    /// A lane's items matching `text` (and `tag`), newest first or A–Z.
    Query { serial: u64, lane: LibLane, text: String, tag: Option<String>, az: bool },
    /// Thumbnails for media an open cut names that no lane listed.
    Thumbs { refs: Vec<AssetRevisionRef> },
    /// A document's source and context. A project revision selects its root
    /// of `kind`; a file revision carries its exact tree owner explicitly.
    Open { reference: AssetRevisionRef, title: String, kind: AssetKind, tree: Option<TreeDocument> },
    Save(Box<SaveRequest>),
    OpenTree { reference: AssetRevisionRef, alias: Option<makepad_asset_data::AssetAlias> },
    SaveTree(Box<TreeSave>),
    RestoreTree { head: AssetRevisionRef, tree: makepad_library_browser::drafts::DraftTree },
    /// A visible card's typed source, prepared on the store worker.
    CardSource { reference: AssetRevisionRef, kind: AssetKind, tree: Option<TreeDocument> },
    /// An app run's render, for a clip.
    RenderOf { purpose: RenderPurpose, source: AssetRevisionRef, path: Option<String> },
    Dependents { reference: AssetRevisionRef },
    /// Titles of the media an opened edit names, whatever lane shows.
    Titles,
    /// What the explorer shows of a selected item beyond its row: tags, and
    /// for a cut its header and contents.
    Details { reference: AssetRevisionRef, edit: bool, tree: Option<TreeDocument> },
    /// The page's pane sizes: read them (`None`) or write them.
    Layout(Option<String>),
    /// Publish a finished render of `edit`.
    PublishRender { edit: AssetRevisionRef, path: Option<String>, render: Box<RenderResult>, title: String },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RenderPurpose {
    /// An app run dropped on the timeline at `start` on `track`.
    Clip { start: f64, track: Option<u32> },
    /// A score dropped at `start` on `track`: its latest render.
    Score { start: f64, track: Option<u32> },
    /// The open cut's saved revision: its render bar and Render button.
    Head,
}

pub enum StoreReply {
    /// A lane's rows and its tags with their counts.
    Rows { serial: u64, lane: LibLane, result: Result<(Vec<LibRow>, Vec<(String, u64)>), String> },
    Thumb { reference: AssetRevisionRef, thumb: Thumb },
    Opened { reference: AssetRevisionRef, title: String, result: Result<OpenedDocument, String> },
    Saved { result: Result<AssetRevisionRef, String> },
    /// A deck opens with its first slide's graphic loaded (OpenTree)...
    TreeOpened { requested: AssetRevisionRef, result: Result<OpenTree, String> },
    /// ...and every graphic loaded, when the opened deck had pending slides.
    TreeCompleted { head: AssetRevisionRef, result: Result<super::open_doc::TreeDoc, String> },
    TreeSaved { result: Result<OpenTree, String> },
    TreeRestored { result: Result<(OpenTree, OpenTree), String> },
    CardSource { reference: AssetRevisionRef, tree: Option<TreeDocument>, result: Result<Option<super::super::still_cache::Spec>, String> },
    Render { purpose: RenderPurpose, source: AssetRevisionRef, result: Result<RenderState, String> },
    Dependents { reference: AssetRevisionRef, result: Result<DependentsDto, String> },
    /// Titles by asset id, item counts per lane, and the newest edits.
    Titles { titles: Vec<(String, String)>, counts: Vec<(LibLane, u64)>, recent: Vec<Recent> },
    RenderPublished { edit: AssetRevisionRef, result: Result<AssetRevisionRef, String> },
    Details { reference: AssetRevisionRef, details: Details },
    /// The pane sizes as last written.
    Layout(String),
    Stopped,
}

struct Shared {
    stop: AtomicBool,
    query: AtomicU64,
}

pub struct CutStore {
    jobs: Option<SyncSender<StoreJob>>,
    replies: Receiver<StoreReply>,
    shared: Arc<Shared>,
    pub error: Option<String>,
}

impl CutStore {
    pub fn new(cx: &mut Cx) -> Self {
        let (job_tx, job_rx) = sync_channel(QUEUE);
        let (reply_tx, reply_rx) = sync_channel(QUEUE);
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), query: AtomicU64::new(0) });
        let worker_shared = shared.clone();
        let options = ThreadOptions { name: Some(Arc::from("stage-edit-store")), stack_size: None, priority: CxThreadPriority::Utility };
        let mut store = Self { jobs: None, replies: reply_rx, shared, error: None };
        match cx.thread_spawner().spawn_worker(options, move || worker(job_rx, reply_tx, worker_shared)) {
            Ok(handle) => {
                handle.detach();
                store.jobs = Some(job_tx);
            }
            Err(error) => store.error = Some(format!("edit store worker unavailable: {error:?}")),
        }
        store
    }

    /// The catalogue generation wanted now; older queries end early.
    pub fn set_query_serial(&self, serial: u64) {
        self.shared.query.store(serial, Ordering::Release);
    }

    /// `Err(job)` when the queue is full (retry later) or the worker is gone.
    pub fn try_send(&self, job: StoreJob) -> Result<(), StoreJob> {
        let Some(jobs) = &self.jobs else { return Err(job) };
        match jobs.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => Err(job),
        }
    }

    pub fn try_recv(&self) -> Option<StoreReply> {
        self.replies.try_recv().ok()
    }

    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.jobs.take();
    }
}

impl Drop for CutStore {
    fn drop(&mut self) {
        self.stop();
    }
}

struct StopGuard(SyncSender<StoreReply>);

impl Drop for StopGuard {
    fn drop(&mut self) {
        let _ = self.0.try_send(StoreReply::Stopped);
    }
}

fn worker(jobs: Receiver<StoreJob>, replies: SyncSender<StoreReply>, shared: Arc<Shared>) {
    let _guard = StopGuard(replies.clone());
    // Declared first so the client (a lane clone of its owner) drops before it.
    let origin = asset_connection::origin();
    let mut client: Option<AssetClient> = None;
    let mut thumbs: Vec<(u64, AssetRevisionRef)> = Vec::new();
    let mut migrated = false;
    // An opened deck whose later slides are still to be loaded.
    let mut completing: Option<(AssetRevisionRef, Arc<makepad_asset_data::tree::TreeDraft>, String)> = None;
    loop {
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        let job = if thumbs.is_empty() && completing.is_none() {
            match jobs.recv_timeout(WORKER_TICK) {
                Ok(job) => Some(job),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match jobs.try_recv() {
                Ok(job) => Some(job),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => break,
            }
        };
        if !migrated {
            if let Ok(connected) = asset_connection::connected(&mut client, &origin) {
                migrated = true;
                migrate_names(connected, &shared);
            }
        }
        if job.is_some() {
            // A newer job (another deck to open) goes first; the full load
            // of a deck that was left is not wanted any more.
            if matches!(job, Some(StoreJob::OpenTree { .. })) { completing = None; }
        } else if let Some((head, draft, path)) = completing.take() {
            let result = super::super::document_tree::load(&draft, &path);
            crate::deck::perf::open_late("store: whole deck loaded");
            if !publish(&replies, &shared, StoreReply::TreeCompleted { head, result }) { break; }
            continue;
        }
        let reply = match job {
            None => {
                // Idle: the next queued thumbnail, while its query is current.
                let Some((serial, reference)) = (!thumbs.is_empty()).then(|| thumbs.remove(0)) else { continue };
                if serial != 0 && serial != shared.query.load(Ordering::Acquire) {
                    continue;
                }
                match asset_connection::connected(&mut client, &origin).ok().and_then(|client| load_thumb(client, &reference)) {
                    Some(thumb) => StoreReply::Thumb { reference, thumb },
                    None => continue,
                }
            }
            Some(StoreJob::Query { serial, lane, text, tag, az }) => {
                if serial != shared.query.load(Ordering::Acquire) {
                    continue;
                }
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| catalogue(client, lane, &text, tag.as_deref(), az, &|| serial == shared.query.load(Ordering::Acquire) && !shared.stop.load(Ordering::Acquire), Some(&StoryRows)));
                if result.is_err() {
                    client = None;
                }
                if let Ok((rows, _)) = &result {
                    thumbs.retain(|(serial, _)| *serial == 0);
                    thumbs.extend(rows.iter().filter(|row| !row.poster_pending).map(|row| (serial, row.reference)));
                }
                StoreReply::Rows { serial, lane, result }
            }
            Some(StoreJob::Thumbs { refs }) => {
                // Ahead of the lane's own thumbnails: the open cut shows them.
                for reference in refs.into_iter().rev() {
                    thumbs.insert(0, (0, reference));
                }
                continue;
            }
            Some(StoreJob::Details { reference, edit, tree }) => {
                let Ok(client) = asset_connection::connected(&mut client, &origin) else { continue };
                let mut details = Details { tags: client.get_annotation(&reference.asset_id).map(|note| note.tags).unwrap_or_default(), edit: None };
                if edit {
                    if let Some(doc) = open_document(client, &reference, AssetKind::Cut, tree.as_ref(), false).ok().and_then(|opened| match opened.tree {
                        Some(tree) => tree.doc.into_cut().ok().map(|doc| (*doc).clone()),
                        None => CutDoc::load(&opened.source).ok(),
                    }) {
                        details.edit = Some(((doc.output.width, doc.output.height, doc.output.fps), doc.duration, contents(&doc)));
                    }
                }
                StoreReply::Details { reference, details }
            }
            Some(StoreJob::Open { reference, title, kind, tree }) => {
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| open_document(client, &reference, kind, tree.as_ref(), false));
                StoreReply::Opened { reference, title, result }
            }
            Some(StoreJob::Save(request)) => {
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| save(client, &request));
                StoreReply::Saved { result }
            }
            Some(StoreJob::OpenTree { reference, alias }) => {
                crate::deck::perf::open_mark("store: job taken");
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| open_tree(client, reference, alias, true));
                crate::deck::perf::open_mark("store: tree fetched, deck file and first slide loaded");
                // The rest of the deck loads next, unless another job comes first.
                completing = result.as_ref().ok()
                    .filter(|opened| opened.doc.clone().into_deck().is_ok_and(|doc| doc.pending()))
                    .map(|opened| (opened.head, opened.draft.clone(), opened.path.clone()));
                StoreReply::TreeOpened { requested: reference, result }
            }
            Some(StoreJob::SaveTree(request)) => {
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| save_tree(client, &request));
                StoreReply::TreeSaved { result }
            }
            Some(StoreJob::RestoreTree { head, tree }) => {
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| {
                    let mut saved = tree_revision(client, head, tree.alias.clone(), false)?;
                    saved.path = tree.saved_path.clone();
                    saved.doc = super::super::document_tree::load(&saved.draft, &saved.path)?;
                    let mut draft = (*tree.draft).clone();
                    for file in draft.files.values_mut() {
                        if matches!(file.kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio) {
                            let reference = file.reference.ok_or_else(|| format!("{}: media draft has no revision", file.path))?;
                            file.bytes = match saved.draft.files.values().find(|file| file.reference == Some(reference)) {
                                Some(saved) => saved.bytes.clone(),
                                None => tree::TreeStore::read(client, reference)?.bytes,
                            };
                        }
                    }
                    let draft = Arc::new(draft);
                    let doc = super::super::document_tree::load(&draft, &tree.path)?;
                    Ok((OpenTree { head, alias: tree.alias, draft, doc, path: tree.path }, saved))
                });
                StoreReply::TreeRestored { result }
            }
            Some(StoreJob::CardSource { reference, kind, tree }) => {
                let result = asset_connection::connected(&mut client, &origin)
                    .and_then(|client| open_document(client, &reference, kind, tree.as_ref(), true))
                    .and_then(|opened| card_source(opened, kind));
                StoreReply::CardSource { reference, tree, result }
            }
            Some(StoreJob::RenderOf { purpose, source, path }) => {
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| render_state(client, &source, path.as_deref()));
                StoreReply::Render { purpose, source, result }
            }
            Some(StoreJob::Titles) => {
                let (titles, counts, recent) = asset_connection::connected(&mut client, &origin).map(|client| {
                    let (mut titles, mut counts, mut recent) = (Vec::new(), Vec::new(), Vec::new());
                    for lane in [LibLane::Video, LibLane::Image, LibLane::Audio, LibLane::Scores, LibLane::AppRuns, LibLane::Cuts] {
                        if let Ok(page) = catalogue_page(client, lane, "", None) {
                            counts.push((lane, page.total));
                            if lane == LibLane::Cuts {
                                for hit in page.hits.iter().take(5) {
                                    if let Some((head, published)) = client.asset_detail(&hit.asset_id).ok().and_then(|detail| detail.latest_published().map(|head| (head.revision, head.published_ms.unwrap_or(0)))) {
                                        let reference = AssetRevisionRef { asset_id: hit.asset_id, revision: head };
                                        let (reference, tree) = if hit.kind == Some(AssetKind::Tree) {
                                            match tree_document(client, reference, AssetKind::Cut) {
                                                Ok((file, owner)) => (file, Some(owner)),
                                                Err(error) => { log!("cut library: {}: {error}", hit.asset_id); continue; }
                                            }
                                        } else { (reference, None) };
                                        let render = render_state(client, &reference, None).unwrap_or(RenderState::None);
                                        let poster_pending = tree.is_none() && poster_pending(client, &hit.asset_id);
                                        recent.push(Recent { reference, tree, title: hit.title.clone(), published, render, poster_pending });
                                    }
                                }
                            }
                            titles.extend(page.hits.into_iter().filter(|hit| !hit.title.is_empty()).map(|hit| (hit.asset_id.to_string(), hit.title)));
                        }
                    }
                    (titles, counts, recent)
                }).unwrap_or_default();
                StoreReply::Titles { titles, counts, recent }
            }
            Some(StoreJob::PublishRender { edit, path, render, title }) => {
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| {
                    makepad_story_edit_render::publish::publish_edit_render(client, edit, path.as_deref(), &render, &title)
                        .map(|published| AssetRevisionRef { asset_id: published.asset_id, revision: published.revision })
                });
                StoreReply::RenderPublished { edit, result }
            }
            Some(StoreJob::Layout(write)) => {
                let path = makepad_app_shell::user_state::dir(None, "edit-layout.txt");
                match write {
                    Some(text) => {
                        if let Some(dir) = path.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        if let Err(error) = std::fs::write(&path, text) {
                            log!("edit: pane sizes not kept: {error}");
                        }
                        continue;
                    }
                    None => StoreReply::Layout(std::fs::read_to_string(&path).unwrap_or_default()),
                }
            }
            Some(StoreJob::Dependents { reference }) => {
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| client.dependents(&reference.asset_id, None).map_err(redact));
                StoreReply::Dependents { reference, result }
            }
        };
        if !publish(&replies, &shared, reply) {
            break;
        }
    }
    while jobs.try_recv().is_ok() {}
}

fn publish(replies: &SyncSender<StoreReply>, shared: &Shared, mut reply: StoreReply) -> bool {
    loop {
        match replies.try_send(reply) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(back)) => {
                if shared.stop.load(Ordering::Acquire) {
                    return false;
                }
                reply = back;
                std::thread::sleep(PUBLISH_WAIT);
            }
        }
    }
}


/// The cut lanes' trees and renders, for the library browser's catalogue.
pub struct StoryRows;

impl CutRows for StoryRows {
    fn tree_cut(&self, client: &mut AssetClient, reference: AssetRevisionRef) -> Result<(AssetRevisionRef, TreeDocument), String> {
        tree_document(client, reference, AssetKind::Cut)
    }

    fn render(&self, client: &mut AssetClient, source: &AssetRevisionRef, path: Option<&str>) -> Result<RenderState, String> {
        render_state(client, source, path)
    }
}

/// The newest render of `source`, fresh when made from this revision.
fn render_state(client: &mut AssetClient, source: &AssetRevisionRef, path: Option<&str>) -> Result<RenderState, String> {
    let render = match path { Some(path) => makepad_story_edit_render::publish::project_render(client, *source, path)?, None => client.render_of(&source.asset_id).map_err(redact)? };
    Ok(match render {
        None => RenderState::None,
        Some((render, from)) if from.revision == source.revision => RenderState::Fresh(render),
        Some((render, _)) => RenderState::Stale(render),
    })
}

/// The EDIT -> CUT rename of the Splash names (`mod.edit` -> `mod.cut`,
/// `@edit` -> `@cut`, `EditOutput` -> `CutOutput`, `edit(…)` -> `cut(…)`) in
/// every stored cut's head and in every unsaved draft: the old text is kept
/// in `cut-migration-backup`, the rewritten one is a new revision of the same
/// asset (a draft is rewritten in place). Once per start; nothing to do after
/// the first.
fn migrate_names(client: &mut AssetClient, shared: &Shared) {
    let backup = makepad_library_browser::drafts::dir().with_file_name("cut-migration-backup");
    let keep = |name: &str, old: &str| -> bool {
        std::fs::create_dir_all(&backup).is_ok() && std::fs::write(backup.join(name), old).is_ok()
    };
    let dir = makepad_library_browser::drafts::dir();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "splash") {
            continue;
        }
        let Ok(old) = std::fs::read_to_string(&path) else { continue };
        let Some(new) = makepad_story_cut::migrate::rename_edit_to_cut(&old) else { continue };
        let name = format!("draft-{}", entry.file_name().to_string_lossy());
        if !keep(&name, &old) || std::fs::write(&path, &new).is_err() {
            log!("cut migration: draft {} not rewritten", path.display());
            continue;
        }
        log!("cut migration: draft {} rewritten (old text in {})", path.display(), backup.join(&name).display());
    }
    let serial = shared.query.load(Ordering::Acquire);
    let Ok((rows, _)) = catalogue(client, LibLane::Cuts, "", None, false, &|| serial == shared.query.load(Ordering::Acquire) && !shared.stop.load(Ordering::Acquire), Some(&StoryRows)) else { return };
    // Tree files are saved together through the tree editor. This legacy
    // asset migration must not read or publish one outside its tree.
    for row in rows.into_iter().filter(|row| row.tree.is_none()) {
        let Ok(old) = open(client, &row.reference, AssetKind::Cut) else { continue };
        let Some(new) = makepad_story_cut::migrate::rename_edit_to_cut(&old) else { continue };
        let doc = match CutDoc::load(&new) {
            Ok(doc) => doc,
            Err(diags) => {
                log!("cut migration: “{}” does not load after the rename: {}", row.title, diags.iter().map(|diag| diag.message.clone()).collect::<Vec<_>>().join("; "));
                continue;
            }
        };
        let Some(thumbnail) = load_thumb(client, &row.reference) else {
            log!("cut migration: “{}” has no readable picture", row.title);
            continue;
        };
        if !keep(&format!("{}-{}.splash", row.reference.asset_id, row.reference.revision), &old) {
            log!("cut migration: no backup of “{}”, not rewritten", row.title);
            continue;
        }
        let request = SaveRequest { kind: AssetKind::Cut, source: new, title: row.title.clone(), deps: makepad_story_cut::dependencies(&doc), asset: Some(row.reference.asset_id), thumbnail, alias: row.alias.clone() };
        match save(client, &request) {
            Ok(head) => log!("cut migration: “{}” is revision {} (old text in {})", row.title, head.revision, backup.display()),
            Err(error) => log!("cut migration: “{}” not saved: {error}", row.title),
        }
    }
}


/// Select a top-level document without fetching the tree's media. A folder
/// may have both roots even though the manifest has one default entry point.
fn tree_document(client: &mut AssetClient, reference: AssetRevisionRef, kind: AssetKind) -> Result<(AssetRevisionRef, TreeDocument), String> {
    let tree = Tree::parse(&open(client, &reference, AssetKind::Tree)?)?;
    let role = match kind {
        AssetKind::Cut => TreeRole::Cut,
        AssetKind::Deck => TreeRole::Deck,
        _ => return Err("a tree document must be a cut or deck".into()),
    };
    let path = match tree.root.as_ref().filter(|(root_role, _)| *root_role == role) {
        Some((_, path)) => path.clone(),
        None => {
            let short = format!("{}.splash", role.as_str());
            let suffix = format!(".{}.splash", role.as_str());
            let mut roots = tree.entries.keys().filter(|path| !path.contains('/') && (path.as_str() == short || path.ends_with(&suffix)));
            let path = roots.next().ok_or_else(|| format!("tree has no {} root", role.as_str()))?.clone();
            if roots.next().is_some() { return Err(format!("tree has more than one {} root", role.as_str())); }
            path
        },
    };
    let Some(TreeEntry::File(file)) = tree.entries.get(&path) else { return Err(format!("tree root `{path}` is not a file")); };
    Ok((*file, TreeDocument { reference, path }))
}

/// Fetch on the store worker; the document loader only receives immutable,
/// already-fetched bytes. CUT opens a file revision with its explicit owner.
/// `first`: a deck with its first slide's graphic only (a library card).
fn open_document(client: &mut AssetClient, reference: &AssetRevisionRef, kind: AssetKind, owner: Option<&TreeDocument>, first: bool) -> Result<OpenedDocument, String> {
    let manifest = client.resolve_ref(reference).map_err(redact)?;
    let (file_reference, owner) = if let Some(owner) = owner {
        (*reference, Some(owner.clone()))
    } else if manifest.kind == AssetKind::Tree {
        let (file, owner) = tree_document(client, *reference, kind)?;
        (file, Some(owner))
    } else { (*reference, None) };
    let Some(owner) = owner else {
        return Ok(OpenedDocument { source: open(client, reference, kind)?, tree: None });
    };
    // The root document is loaded once: up to its first slide here, whole
    // below only when asked for.
    let mut opened = open_tree(client, owner.reference, None, true)?;
    let file = opened.draft.files.get(&owner.path).ok_or("the tree no longer contains this file")?;
    if (opened.head == owner.reference && file.reference != Some(file_reference)) || file.kind != kind {
        return Err("the document revision does not match its tree entry".into());
    }
    let source = file.text()?.to_string();
    let loaded = opened.path == owner.path && (first || !matches!(&opened.doc, super::open_doc::TreeDoc::Deck(doc) if doc.pending()));
    opened.path = owner.path;
    if !loaded {
        opened.doc = if first { super::super::document_tree::load_first(&opened.draft, &opened.path)? } else { super::super::document_tree::load(&opened.draft, &opened.path)? };
    }
    Ok(OpenedDocument { source, tree: Some(opened) })
}

/// The source text of a document revision of `kind` (a cut or a deck).
pub(super) fn open(client: &mut AssetClient, reference: &AssetRevisionRef, kind: AssetKind) -> Result<String, String> {
    let noun = document_noun(kind);
    let manifest = client.resolve_ref(reference).map_err(redact)?;
    if manifest.kind != kind {
        return Err(format!("that asset is not a {noun}"));
    }
    let file = select_file(&manifest, FileRole::Source, TierPreference::PreferWithAnyFallback(DeviceTier::High), 0).map_err(redact)?;
    if file.media != MediaType::Text || file.byte_len > MAX_SOURCE_BYTES {
        return Err(format!("the {noun}'s source is not text of a readable size"));
    }
    let (blob, len) = (file.blob, file.byte_len);
    let bytes = client.fetch_blob_bytes(&blob, Some(len)).map_err(redact)?;
    String::from_utf8(bytes).map_err(|_| format!("the {noun}'s source is not UTF-8"))
}

/// Opening always follows the project's head; the caller's card can be stale.
/// `first`: a deck loads its first slide's graphic only ([`super::super::document_tree::load_first`]).
pub(crate) fn open_tree(client: &mut AssetClient, reference: AssetRevisionRef, alias: Option<makepad_asset_data::AssetAlias>, first: bool) -> Result<OpenTree, String> {
    let alias = match alias {
        Some(alias) => alias,
        None => {
            let mut query = CatalogQuery::browse(makepad_asset_client::api::MAX_SEARCH_LIMIT);
            query.kind = Some(AssetKind::Tree);
            let mut cursor = None;
            loop {
                let page = client.catalog_search(&query, cursor.as_ref()).map_err(redact)?;
                if let Some(alias) = page.hits.into_iter().find(|hit| hit.asset_id == reference.asset_id).and_then(|hit| hit.alias) { break alias; }
                cursor = page.next;
                if cursor.is_none() { return Err("the tree has no project alias".into()); }
            }
        }
    };
    let found = client.resolve_alias(&alias).map_err(redact)?;
    if found.asset_id != reference.asset_id { return Err("the project's alias now names another tree; reopen it".into()); }
    let head = AssetRevisionRef { asset_id: found.asset_id, revision: found.head_revision };
    tree_revision(client, head, alias, first)
}

pub(crate) fn tree_revision(client: &mut AssetClient, head: AssetRevisionRef, alias: makepad_asset_data::AssetAlias, first: bool) -> Result<OpenTree, String> {
    let memory = makepad_asset_client::tree::load(client, head)?;
    let draft = Arc::new(makepad_asset_data::tree::TreeDraft::from_memory(&memory)?);
    let path = draft.root.1.clone();
    let doc = if first { super::super::document_tree::load_first(&draft, &path)? } else { super::super::document_tree::load(&draft, &path)? };
    Ok(OpenTree { head, alias, draft, doc, path })
}

pub(crate) fn open_project(client: &mut AssetClient, project: &str, path: Option<&str>) -> Result<OpenTree, String> {
    let alias = project.parse().map_err(|error| format!("project path: {error:?}"))?;
    let resolved = client.resolve_alias(&alias).map_err(redact)?;
    let mut opened = tree_revision(client, AssetRevisionRef { asset_id: resolved.asset_id, revision: resolved.head_revision }, alias, false)?;
    if let Some(path) = path {
        let file = makepad_asset_data::tree::TreeResolver::resolve(opened.draft.as_ref(), &opened.draft.root.1, path)?;
        opened.path = file.path;
        opened.doc = super::super::document_tree::load(&opened.draft, &opened.path)?;
    }
    Ok(opened)
}

fn save_tree(client: &mut AssetClient, request: &TreeSave) -> Result<OpenTree, String> {
    use makepad_asset_client::tree::{self, Tree, TreeEditor};
    let mut draft = (*request.draft).clone();
    // Media that has its bytes but no library revision yet (a folder deck's
    // first save as the person's own copy) goes in first: documents draw
    // media by its revision.
    let mut imported = false;
    for file in draft.files.values_mut().filter(|file| file.reference.is_none() && matches!(file.kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio)) {
        let content = tree::content_file(&file.path, &file.bytes, file.kind)?;
        let card = Thumb { width: 448, height: 256, pixels: Arc::new(vec![0xff16_161b; 448 * 256]) };
        let mut bundle = PublishBundle::new(NAMESPACE, file.kind, &file.path, vec![content], jpeg(&card)?, PublishRights::generated_cc0());
        bundle.tags = vec![TREE_FILE.into()];
        bundle.generator = "stage-document-tree".into();
        let published = client.publish_bundle(&bundle).map_err(redact)?;
        let reference = AssetRevisionRef { asset_id: published.asset_id, revision: published.revision };
        file.reference = Some(reference);
        file.base = Some(reference);
        imported = true;
    }
    let request = &TreeSave { draft: if imported { Arc::new(draft.clone()) } else { request.draft.clone() }, head: request.head, alias: request.alias.clone(), thumbnail: request.thumbnail.clone(), path: request.path.clone(), settled: request.settled, dpi: request.dpi };
    let paths = [draft.root.1.clone(), request.path.clone()].into_iter().collect::<std::collections::BTreeSet<_>>();
    let mut root_still = None;
    for path in paths {
        let context = request.draft.context(&path);
        let doc = super::super::document_tree::load(&request.draft, &path)?;
        let specs = match doc {
            super::open_doc::TreeDoc::Deck(doc) => doc.slides.iter().filter_map(|slide| {
                let makepad_story_deck::SlideCut::Inline(cut) = &slide.cut else { return None };
                Some(super::super::still_cache::Spec { source: super::super::still_cache::declarations(&doc.source, slide.span).into(), context: Some(context.clone()), cut: Arc::new((**cut).clone()), live_name: None })
            }).collect::<Vec<_>>(),
            super::open_doc::TreeDoc::Cut(cut) => vec![super::super::still_cache::Spec { source: cut.source.clone().into(), context: Some(context), cut, live_name: None }],
        };
        for (index, spec) in specs.iter().enumerate() {
            super::super::still_cache::carry_cached(&mut draft, spec)?;
            if request.settled && path == request.path && index == 0 { super::super::still_cache::carry_frame(&mut draft, spec, &request.thumbnail, request.dpi)?; }
            if path == draft.root.1 && index == 0 { root_still = super::super::still_cache::cached_thumb(&draft, spec)?; }
        }
    }
    let draft = Arc::new(draft);
    tree::validate_draft(&draft)?;
    let root = super::super::document_tree::load(&request.draft, &request.draft.root.1)?;
    super::super::document_tree::load(&request.draft, &request.path)?;
    let title = match &root { super::open_doc::TreeDoc::Cut(doc) => &doc.label, super::open_doc::TreeDoc::Deck(doc) => &doc.title };
    let role = request.draft.root.0.as_str();
    let (base, rights, namespace) = match request.head {
        Some(head) => {
            let memory = tree::load(client, head)?;
            let manifest = client.resolve_ref(&head).map_err(redact)?;
            let detail = client.asset_detail(&head.asset_id).map_err(redact)?;
            (memory.tree, PublishRights::from_manifest(&manifest.rights), detail.namespace)
        }
        None => (Tree::default(), PublishRights::generated_cc0(), NAMESPACE.to_string()),
    };
    let alias = match &request.alias {
        Some(alias) => alias.clone(),
        None => {
            let stem: String = title.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
            let stem = stem.trim_matches('_');
            let stem = if stem.is_empty() { "untitled" } else { stem };
            let mut n = 1;
            loop {
                let path = if n == 1 { format!("{namespace}/{role}s/{stem}") } else { format!("{namespace}/{role}s/{stem}_{n}") };
                let alias = path.parse().map_err(|e| format!("project path: {e:?}"))?;
                match client.resolve_alias(&alias) {
                    Err(makepad_asset_client::ClientError::NotFound { .. }) => break alias,
                    Ok(_) => n += 1,
                    Err(error) => return Err(redact(error)),
                }
            }
        }
    };
    let thumbnail = root_still.unwrap_or_else(|| {
        if request.path == request.draft.root.1 { request.thumbnail.clone() }
        else { Thumb { width: 16, height: 9, pixels: Arc::new(vec![0xff16_161b; 16 * 9]) } }
    });
    let mut bundle = PublishBundle::new(&namespace, AssetKind::Tree, title, Vec::new(), jpeg(&thumbnail)?, rights);
    bundle.asset_id = request.head.map(|head| head.asset_id);
    bundle.alias = Some(alias.clone());
    bundle.tags = vec![role.into()];
    bundle.categories = bundle.tags.clone();
    bundle.generator = "stage-document-tree".into();
    let mut editor = if request.head.is_some() { TreeEditor::new(base, bundle, request.head)? } else { TreeEditor::create(client, base, bundle)? };
    let memory = editor.save_draft(client, &draft)?;
    let draft = Arc::new(tree::TreeDraft::from_memory(&memory)?);
    let path = request.path.clone();
    let doc = super::super::document_tree::load(&draft, &path)?;
    Ok(OpenTree { head: editor.head.ok_or("tree publication returned no head")?, alias, draft, doc, path })
}

/// What a cut holds, for people: "3 clips · 1 effect · 1 VO".
pub fn contents(doc: &makepad_story_cut::CutDoc) -> String {
    use makepad_story_cut::CutNodeKind;
    let (mut clips, mut effects, mut transitions, mut voice, mut sounds) = (0, 0, 0, 0, 0);
    for node in &doc.nodes {
        match &node.kind {
            CutNodeKind::Clip { .. } | CutNodeKind::Still { .. } | CutNodeKind::Title { .. } => clips += 1,
            CutNodeKind::Fx { .. } => effects += 1,
            CutNodeKind::Transition { .. } => transitions += 1,
            CutNodeKind::VoiceOver { lines, .. } => voice += lines.len(),
            CutNodeKind::AudioClip { .. } => sounds += 1,
            _ => {}
        }
    }
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts = Vec::new();
    if clips > 0 || (doc.graphics.is_empty() && doc.generates.is_empty()) { parts.push(plural(clips, "clip", "clips")); }
    if !doc.graphics.is_empty() { parts.push(plural(doc.graphics.len(), "graphic", "graphics")); }
    if !doc.generates.is_empty() { parts.push(plural(doc.generates.len(), "generative segment", "generative segments")); }
    if sounds > 0 { parts.push(plural(sounds, "sound", "sounds")); }
    if effects > 0 { parts.push(plural(effects, "effect", "effects")); }
    if transitions > 0 { parts.push(plural(transitions, "transition", "transitions")); }
    if voice > 0 { parts.push(format!("{voice} VO")); }
    parts.join(" · ")
}

fn exact(asset: &AssetRef) -> Result<AssetRevisionRef, String> {
    Ok(AssetRevisionRef {
        asset_id: asset.id.parse().map_err(|_| format!("`{}` is not an asset id", asset.id))?,
        revision: asset.rev.parse().map_err(|_| format!("`{}` is not a revision id", asset.rev))?,
    })
}

/// What a document kind is called in a message.
fn document_noun(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Deck => "deck",
        AssetKind::Tree => "tree",
        _ => "cut",
    }
}

/// Publish a new revision of a cut or a deck whose dependencies are every
/// revision its source names.
fn save(client: &mut AssetClient, request: &SaveRequest) -> Result<AssetRevisionRef, String> {
    let mut dependencies = Vec::new();
    for dep in &request.deps {
        let reference = match dep {
            Dep::Asset(asset) | Dep::AppRun(asset) | Dep::Cut(asset) => exact(asset)?,
            Dep::Render { render, .. } => exact(render)?,
        };
        if !dependencies.contains(&reference) {
            dependencies.push(reference);
        }
    }
    let thumbnail = jpeg(&request.thumbnail)?;
    let files = vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, request.source.clone().into_bytes(), None)];
    let title = if request.title.trim().is_empty() { format!("Untitled {}", document_noun(request.kind)) } else { request.title.trim().to_string() };
    let mut bundle = PublishBundle::new(NAMESPACE, request.kind, title, files, thumbnail, PublishRights::generated_cc0());
    bundle.asset_id = request.asset;
    bundle.dependencies = dependencies;
    bundle.generator = if request.kind == AssetKind::Deck { "stage-deck" } else { "stage-edit" }.into();
    bundle.alias = request.alias.clone();
    let published = client.publish_bundle(&bundle).map_err(redact)?;
    Ok(AssetRevisionRef { asset_id: published.asset_id, revision: published.revision })
}

fn jpeg(thumb: &Thumb) -> Result<PublishThumbnail, String> {
    let bgra: Vec<u8> = thumb.pixels.iter().flat_map(|pixel| pixel.to_le_bytes()).collect();
    let mut bytes = Vec::new();
    jpeg_encoder::Encoder::new(&mut bytes, 85)
        .encode(&bgra, thumb.width as u16, thumb.height as u16, jpeg_encoder::ColorType::Bgra)
        .map_err(|error| format!("thumbnail: {error}"))?;
    Ok(PublishThumbnail::plain(bytes, ThumbnailMedia::Jpeg, thumb.width as u32, thumb.height as u32))
}


/// The viewer's media, from the asset store: connects on the renderer's
/// worker the first time it resolves (never on the UI thread), then keeps
/// a lane of the shared client.
pub struct LazyStoreMedia {
    origin: Arc<asset_connection::AssetOrigin>,
    /// The resolver and the store it was made for: a takeover moves the
    /// store, and the next resolve makes a new one.
    inner: Option<(makepad_story_edit_render::StoreMedia, makepad_asset_client::ApiEndpoints)>,
}

impl LazyStoreMedia {
    pub fn new() -> Self {
        Self { origin: asset_connection::origin(), inner: None }
    }

    /// The store resolver, connected on first use.
    pub fn store(&mut self) -> Result<&mut makepad_story_edit_render::StoreMedia, String> {
        self.inner()
    }

    fn inner(&mut self) -> Result<&mut makepad_story_edit_render::StoreMedia, String> {
        if self.inner.as_ref().is_some_and(|(_, endpoints)| !asset_connection::is_current(*endpoints)) {
            self.inner = None;
        }
        if self.inner.is_none() {
            let client = asset_connection::connect(&self.origin)?;
            let endpoints = client.endpoints();
            self.inner = Some((makepad_story_edit_render::StoreMedia::new(client), endpoints));
        }
        Ok(&mut self.inner.as_mut().unwrap().0)
    }
}

impl makepad_story_edit_render::MediaResolver for LazyStoreMedia {
    fn resolve(&mut self, media: &makepad_story_cut::MediaRef) -> Result<makepad_story_edit_render::ResolvedMedia, String> {
        // A folder document's media is read off disk, with no store.
        if let Some(found) = makepad_story_edit_render::media::folder::resolve(media) { return found; }
        self.inner()?.resolve(media)
    }

    fn effect_source(&mut self, effect: &AssetRef) -> Result<String, String> {
        self.inner()?.effect_source(effect)
    }

    fn cut_source(&mut self, edit: &AssetRef) -> Result<String, String> {
        self.inner()?.cut_source(edit)
    }

    fn transcript(&mut self, transcript: &AssetRef) -> Result<Option<Vec<makepad_story_cut::SpokenLine>>, String> {
        self.inner()?.transcript(transcript)
    }

    /// A voice take's word timings (the viewer's karaoke and captions).
    fn take_info(&mut self, take: &AssetRef) -> Result<Option<makepad_story_cut::TakeInfo>, String> {
        self.inner()?.take_info(take)
    }

    /// An app run's run.json (its marks and focus tracks).
    fn run_data(&mut self, run: &makepad_story_cut::RunRef) -> Result<Option<Vec<u8>>, String> {
        self.inner()?.run_data(run)
    }

    /// A Graphic's Motion document, for the viewer and renders.
    fn motion_source(&mut self, motion: &AssetRef) -> Result<String, String> {
        if let Some(found) = makepad_story_edit_render::media::folder::text(motion) { return found; }
        self.inner()?.motion_source(motion)
    }

    /// An image or SVG a Motion document uses.
    fn asset_bytes(&mut self, asset: &AssetRef) -> Result<(Vec<u8>, &'static str), String> {
        if let Some(found) = makepad_story_edit_render::media::folder::bytes(asset) { return found; }
        self.inner()?.asset_bytes(asset)
    }

    /// A song's aligned lyrics (its music bake), for lyric-locked graphics.
    fn lyrics(&mut self, song: &AssetRef) -> Result<Option<String>, String> {
        self.inner()?.lyrics(song)
    }
}

/// Splash modules from the asset store for every Motion document Stage
/// loads (the in-app AI's write_motion and checks, the Motion editor, the
/// viewer): `import(asset(id, rev))` reads that Motion revision's source,
/// once per revision.
pub struct LibraryModules {
    media: std::sync::Mutex<LazyStoreMedia>,
    cache: std::sync::Mutex<std::collections::HashMap<(String, String), Option<Arc<str>>>>,
}

impl LibraryModules {
    /// Install as the modules every document without its own resolver uses.
    pub fn install() {
        makepad_motion::modules::set_library(Arc::new(Self { media: std::sync::Mutex::new(LazyStoreMedia::new()), cache: Default::default() }));
    }
}

impl makepad_motion::modules::ModuleResolver for LibraryModules {
    fn asset(&self, id: &str, rev: &str) -> Option<makepad_motion::modules::ModuleSource> {
        let key = (id.to_string(), rev.to_string());
        let cached = self.cache.lock().ok()?.get(&key).cloned();
        let text = match cached {
            Some(text) => text,
            None => {
                use makepad_story_edit_render::MediaResolver;
                let asset = AssetRef { id: key.0.clone(), rev: key.1.clone() };
                let text = self.media.lock().ok()?.motion_source(&asset).ok().map(Arc::<str>::from);
                self.cache.lock().ok()?.insert(key, text.clone());
                text
            }
        }?;
        Some(makepad_motion::modules::ModuleSource::new(id, rev, text))
    }
}


fn card_source(opened: OpenedDocument, kind: AssetKind) -> Result<Option<super::super::still_cache::Spec>, String> {
    match opened.tree {
        Some(tree) => {
            let context = tree.draft.context(&tree.path);
            card_spec(tree.doc, Some(context))
        }
        None if kind == AssetKind::Deck => card_spec(super::open_doc::TreeDoc::Deck(Arc::new(makepad_story_deck::DeckDoc::load(&opened.source).map_err(|diags| format!("{diags:?}"))?)), None),
        None => card_spec(super::open_doc::TreeDoc::Cut(Arc::new(CutDoc::load(&opened.source).map_err(|diags| format!("{diags:?}"))?)), None),
    }
}

/// A library card's picture: a deck's first slide, a cut's final frame
/// (`context`: the cut's tree context). None for a deck with no inline
/// first slide; a live input (app, camera, code) names the card instead.
pub(crate) fn card_spec(doc: super::open_doc::TreeDoc, context: Option<makepad_asset_data::tree::TreeContext>) -> Result<Option<super::super::still_cache::Spec>, String> {
    use super::super::still_cache::{Spec, declarations};
    let (source, context, cut, apps) = match doc {
        super::open_doc::TreeDoc::Deck(doc) => {
            let Some(slide) = doc.slides.first() else { return Ok(None) };
            let makepad_story_deck::SlideCut::Inline(cut) = &slide.cut else { return Ok(None) };
            let apps = slide.apps.iter().map(|app| (app.name.clone(), app.app.clone())).collect::<HashMap<_,_>>();
            (declarations(&doc.source, slide.span), doc.tree.clone(), Arc::new((**cut).clone()), apps)
        }
        super::open_doc::TreeDoc::Cut(doc) => (doc.source.clone(), context, doc, HashMap::new()),
    };
    let live: Vec<_> = cut.nodes.iter().filter_map(|node| match &node.kind {
        makepad_story_cut::CutNodeKind::AppView { app, .. } => Some(apps.get(app).unwrap_or(app).clone()),
        makepad_story_cut::CutNodeKind::Camera(_) => Some("Camera".into()),
        makepad_story_cut::CutNodeKind::Code(_) => Some("Source".into()),
        _ => None,
    }).collect();
    let live_name = (!live.is_empty()).then(|| live.join(", "));
    Ok(Some(Spec { source: source.into(), context, cut, live_name }))
}
