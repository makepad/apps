//! Web export: the saved cut compiled into a film for the browser
//! (`makepad-story-web-export`). The settings are the `export_web` card's
//! (chat chip "Web export…": the output folder, device, pack and its knobs,
//! the film.js sections, music, network, credits); this file is the engine
//! and the run's state, with no widgets.
//!
//! The cut lives in the library, not as files: a long-lived worker reads
//! the saved revision and every file its Graphics use through the
//! renderer's store resolver (`StoreMedia`), writes them with a media table
//! into the export's build folder, then runs the export there and serves
//! the folder on this machine (the library's static server). The UI thread
//! only `try_send`s jobs (held and retried when the queue is full) and
//! drains the worker's events with `try_recv` on its pump; cancel is an
//! atomic the export reads between its steps.
//!
//! The studio API's `export_web` starts the export ([`VideoCut::web_export_api_start`])
//! and waits for [`VideoCut::web_export_api_answer`]; the chat shows the
//! phase in the status line and offers "Stop web export", "Open in browser"
//! and "Reveal folder" chips.
//!
//! Main-loop cost: the pump drains the worker's few events (phase steps,
//! one per cargo crate at most).

use makepad_asset_connection as asset_connection;
use super::VideoCut;
use makepad_asset_client::AssetClient;
use makepad_asset_data::{AssetId, AssetRevisionRef, MediaType};
use makepad_story_edit_render::{MediaResolver, StoreMedia};
use makepad_story_cut::{AssetRef, CutDoc, CutNodeKind, EffectRef, MediaRef};
use makepad_story_web_export::{self as web, AssetsKnob, Diagnostic, ExportOptions, ExportReport, FontsKnob, KernelsKnob, PackPreset, Phase, Preset, Progress, ShadersKnob, PackOptions, Severity};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::*;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TrySendError};
use std::sync::Arc;

/// Jobs the worker queues before the page holds them back.
const QUEUE: usize = 4;

// ------------------------------------------------------------------ worker

/// Which edit to export: the page's saved revision, or (the studio API) a
/// library edit's newest revision.
#[derive(Clone, Debug)]
enum Target {
    Open { head: AssetRevisionRef, source: String, tree: Option<(Arc<makepad_asset_data::tree::TreeDraft>, String)> },
    Asset(String),
}

struct ExportJob {
    id: u64,
    target: Target,
    /// `None`: `~/Movies/<label> web`.
    out_dir: Option<PathBuf>,
    preset: Preset,
    pack: PackOptions,
    include_music: bool,
    /// `None`: from the assets' rights.
    credits: Option<String>,
    /// Also serve it to the local network.
    lan: bool,
    cancel: Arc<AtomicBool>,
}

/// A served folder: its local URL and, on the network, its LAN URL.
type Urls = (String, Option<String>);

enum Event {
    Resolved { id: u64, edit: AssetRevisionRef, label: String, out_dir: PathBuf },
    Phase { id: u64, phase: Phase, done: f64 },
    Diagnostic { id: u64, diagnostic: Diagnostic },
    Finished { id: u64, result: Result<ExportReport, Vec<Diagnostic>>, served: Option<Result<Urls, String>> },
}

struct Worker {
    jobs: SyncSender<ExportJob>,
    events: Receiver<Event>,
    held: VecDeque<ExportJob>,
}

impl Worker {
    fn new(cx: &mut Cx) -> Result<Self, String> {
        let (job_tx, job_rx) = sync_channel::<ExportJob>(QUEUE);
        let (event_tx, events) = channel::<Event>();
        let options = ThreadOptions { name: Some(Arc::from("stage-web-export")), stack_size: Some(8 * 1024 * 1024), priority: CxThreadPriority::Utility };
        let handle = cx.thread_spawner().spawn_worker(options, move || run(job_rx, event_tx)).map_err(|error| format!("{error:?}"))?;
        handle.detach();
        Ok(Self { jobs: job_tx, events, held: VecDeque::new() })
    }

    fn send(&mut self, job: ExportJob) {
        self.held.push_back(job);
        self.flush();
    }

    /// Queued jobs a full queue held back go on a later pump.
    fn flush(&mut self) {
        while let Some(job) = self.held.pop_front() {
            match self.jobs.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) => {
                    self.held.push_front(job);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    log!("web export: the worker stopped");
                    break;
                }
            }
        }
    }
}

/// The export's progress, sent to the page.
struct ChannelProgress {
    id: u64,
    events: Sender<Event>,
    cancel: Arc<AtomicBool>,
}

impl Progress for ChannelProgress {
    fn phase(&mut self, phase: Phase, done: f64, _note: &str) {
        let _ = self.events.send(Event::Phase { id: self.id, phase, done });
    }

    fn diagnostic(&mut self, diagnostic: &Diagnostic) {
        let _ = self.events.send(Event::Diagnostic { id: self.id, diagnostic: diagnostic.clone() });
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

/// Folders served so far, with their URLs (the server runs for the life of
/// the process).
type Served = Vec<(PathBuf, bool, Urls)>;

fn serve(served: &mut Served, dir: &Path, lan: bool) -> Result<Urls, String> {
    if let Some((_, _, urls)) = served.iter().find(|(d, l, _)| d == dir && *l == lan) {
        return Ok(urls.clone());
    }
    let server = makepad_web_pack::serve::serve(dir, lan, 0)?;
    let urls = (server.local_url, server.lan_url);
    served.push((dir.to_path_buf(), lan, urls.clone()));
    Ok(urls)
}

fn run(jobs: Receiver<ExportJob>, events: Sender<Event>) {
    let origin = asset_connection::origin();
    let mut client: Option<AssetClient> = None;
    let mut served: Served = Vec::new();
    while let Ok(job) = jobs.recv() {
        let (id, lan) = (job.id, job.lan);
        let mut progress = ChannelProgress { id, events: events.clone(), cancel: job.cancel.clone() };
        let result = asset_connection::connected(&mut client, &origin)
            .map_err(|error| vec![Diagnostic::error(Phase::Resolve, format!("the library is not reachable: {error}"))])
            .and_then(|client| export(client, job, &mut progress, &events));
        let served = match &result {
            Ok((_, out_dir)) => Some(serve(&mut served, out_dir, lan)),
            Err(_) => None,
        };
        if events.send(Event::Finished { id, result: result.map(|(report, _)| report), served }).is_err() {
            break;
        }
    }
}

fn source_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    root.canonicalize().unwrap_or(root)
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// `~/Movies/<label> web`.
fn default_out_dir(label: &str) -> String {
    let name: String = label.trim().chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') { '-' } else { c }).collect();
    let name = if name.trim().is_empty() { "Film".to_string() } else { name.trim().to_string() };
    format!("~/Movies/{name} web")
}

fn expand(path: &str) -> PathBuf {
    match path.trim().strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path.trim()),
    }
}

fn exact(asset: &AssetRef) -> Option<AssetRevisionRef> {
    Some(AssetRevisionRef { asset_id: asset.id.parse().ok()?, revision: asset.rev.parse().ok()? })
}

/// The cut's revision and source.
fn load_target(client: &mut AssetClient, target: &Target) -> Result<(AssetRevisionRef, String, Option<(Arc<makepad_asset_data::tree::TreeDraft>, String)>), String> {
    match target {
        Target::Open { head, source, tree } => Ok((*head, source.clone(), tree.clone())),
        Target::Asset(id) => {
            let asset: AssetId = id.parse().map_err(|_| format!("`{id}` is not an asset id"))?;
            let detail = client.asset_detail(&asset).map_err(|error| makepad_asset_connection::redact(error))?;
            let head = detail.latest_published().map(|head| head.revision).ok_or_else(|| format!("edit {id} has no published revision"))?;
            let head = AssetRevisionRef { asset_id: asset, revision: head };
            let mut store = StoreMedia::new(client.lane_clone());
            let source = store.cut_source(&AssetRef { id: id.clone(), rev: head.revision.to_string() })?;
            Ok((head, source, None))
        }
    }
}

/// What the film draws and plays, by asset: the Graphics' Motion
/// documents, the modules they import, the pictures they name and the
/// songs.
#[derive(Default)]
struct Uses {
    motions: Vec<(AssetRef, String, String)>,
    modules: Vec<(AssetRef, String)>,
    pictures: Vec<AssetRef>,
    songs: Vec<AssetRef>,
}

fn uses(store: &mut StoreMedia, doc: &CutDoc, source: &str, diagnostics: &mut Vec<Diagnostic>) -> Uses {
    let mut uses = Uses::default();
    for graphic in &doc.graphics {
        let makepad_story_cut::graphic::GraphicSrc::Asset(asset) = &graphic.src else { continue };
        if uses.motions.iter().any(|(known, _, _)| known.id == asset.id) {
            continue;
        }
        match store.motion_source(asset) {
            Ok(text) => uses.motions.push((asset.clone(), graphic.id.clone(), text)),
            Err(error) => diagnostics.push(Diagnostic::error(Phase::Resolve, format!("Motion asset {} (Graphic `{}`): {error}", asset.id, graphic.id))),
        }
    }
    // The modules the documents import, transitively.
    let imports = |text: &str| makepad_motion::modules::asset_imports(text).into_iter().map(|module| AssetRef { id: module.id, rev: module.rev });
    let mut todo: Vec<AssetRef> = uses.motions.iter().flat_map(|(_, _, text)| imports(text)).collect();
    while let Some(module) = todo.pop() {
        if uses.modules.iter().any(|(known, _)| known.id == module.id) || uses.motions.iter().any(|(known, _, _)| known.id == module.id) {
            continue;
        }
        match store.motion_source(&module) {
            Ok(text) => {
                todo.extend(imports(&text));
                uses.modules.push((module, text));
            }
            Err(error) => diagnostics.push(Diagnostic::error(Phase::Resolve, format!("module asset {} (imported by a Graphic's document): {error}", module.id))),
        }
    }
    for node in &doc.nodes {
        if let CutNodeKind::AudioClip { src: MediaRef::Asset(song), .. } = &node.kind {
            if !uses.songs.iter().any(|known| known.id == song.id) {
                uses.songs.push(song.clone());
            }
        }
    }
    // Pictures: every other `asset(..)` the documents and the cut name
    // (a Graphic's params among them), not the cut's own clips and effects.
    let mut known: Vec<String> = makepad_story_edit_render::job::doc_media(doc).iter().map(|media| match media {
        MediaRef::Asset(asset) => asset.id.clone(),
        MediaRef::Run(run) => run.render_id.clone(),
        MediaRef::MissingOptional(path) => path.clone(),
    }).collect();
    for node in &doc.nodes {
        match &node.kind {
            CutNodeKind::Clip { src: MediaRef::Asset(asset), .. } => known.push(asset.id.clone()),
            CutNodeKind::Fx { effect: EffectRef::Asset(asset), .. } => known.push(asset.id.clone()),
            _ => {}
        }
    }
    known.extend(uses.motions.iter().map(|(asset, _, _)| asset.id.clone()));
    known.extend(uses.modules.iter().map(|(asset, _)| asset.id.clone()));
    let texts = std::iter::once(source).chain(uses.motions.iter().map(|(_, _, text)| text.as_str())).chain(uses.modules.iter().map(|(_, text)| text.as_str()));
    for text in texts {
        for asset in makepad_story_edit_render::job::asset_refs(text) {
            if !known.contains(&asset.id) && !uses.pictures.iter().any(|known| known.id == asset.id) {
                uses.pictures.push(asset);
            }
        }
    }
    uses
}

/// The credits the assets' rights give (each credits line once, with its
/// license).
fn credits_of(client: &mut AssetClient, uses: &Uses) -> String {
    let refs = uses.songs.iter().chain(uses.motions.iter().map(|(asset, _, _)| asset)).chain(uses.modules.iter().map(|(asset, _)| asset)).chain(uses.pictures.iter());
    let mut lines: Vec<String> = Vec::new();
    for asset in refs {
        let Some(revision) = exact(asset) else { continue };
        let Ok(manifest) = client.fetch_asset_manifest(&revision.revision) else { continue };
        let credits = manifest.rights.credits.trim();
        if credits.is_empty() {
            continue;
        }
        let license = manifest.rights.license.trim();
        let line = if license.is_empty() { credits.to_string() } else { format!("{credits} ({license})") };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines.join("\n")
}

/// Writes the cut and every file it uses into `dir` with the media
/// table the export reads.
fn stage(store: &mut StoreMedia, source: &str, uses: &Uses, dir: &Path, diagnostics: &mut Vec<Diagnostic>) -> Result<(PathBuf, PathBuf), Diagnostic> {
    let io = |path: &Path, error: std::io::Error| Diagnostic::error(Phase::Resolve, format!("{}: {error}", path.display()));
    std::fs::create_dir_all(dir).map_err(|error| io(dir, error))?;
    let write = |name: &str, bytes: &[u8]| -> Result<(), Diagnostic> {
        let path = dir.join(name);
        std::fs::write(&path, bytes).map_err(|error| io(&path, error))
    };
    let mut table = String::from("# id=file, written by Stage's web export\n");
    for (asset, _, text) in &uses.motions {
        let name = format!("motion-{}.splash", asset.id);
        write(&name, text.as_bytes())?;
        table.push_str(&format!("{}={name}\n", asset.id));
    }
    for (asset, text) in &uses.modules {
        let name = format!("module-{}.splash", asset.id);
        write(&name, text.as_bytes())?;
        table.push_str(&format!("{}={name}\n", asset.id));
    }
    for asset in &uses.pictures {
        // Named but perhaps not drawn: what cannot be read is a warning
        // here, and an error from the export when a document uses it.
        match store.asset_bytes(asset) {
            Ok((bytes, kind)) => {
                let name = format!("{}.{kind}", asset.id);
                write(&name, &bytes)?;
                table.push_str(&format!("{}={name}\n", asset.id));
            }
            Err(error) => diagnostics.push(Diagnostic::warning(Phase::Resolve, format!("asset {}: {error}", asset.id))),
        }
    }
    for song in &uses.songs {
        let resolved = match store.resolve(&MediaRef::Asset(song.clone())) {
            Ok(resolved) => resolved,
            Err(error) => {
                diagnostics.push(Diagnostic::error(Phase::Resolve, format!("song asset {}: {error}", song.id)));
                continue;
            }
        };
        let ext = match resolved.media {
            MediaType::Mp3 => "mp3",
            MediaType::Wav => "wav",
            MediaType::Ogg => "ogg",
            MediaType::Mp4 => "m4a",
            other => {
                diagnostics.push(Diagnostic::error(Phase::Resolve, format!("song asset {}: a {other:?} file is not audio a browser plays", song.id)));
                continue;
            }
        };
        let name = format!("{}.{ext}", song.id);
        let link = dir.join(&name);
        if link.symlink_metadata().is_ok() {
            std::fs::remove_file(&link).map_err(|error| io(&link, error))?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&resolved.path, &link).map_err(|error| io(&link, error))?;
        #[cfg(not(unix))]
        std::fs::copy(&resolved.path, &link).map_err(|error| io(&link, error))?;
        table.push_str(&format!("{}={name}\n", song.id));
        match store.lyrics(song) {
            Ok(Some(lyrics)) => {
                let name = format!("lyrics-{}.json", song.id);
                write(&name, lyrics.as_bytes())?;
                table.push_str(&format!("lyrics:{}={name}\n", song.id));
            }
            Ok(None) => {}
            Err(error) => diagnostics.push(Diagnostic::warning(Phase::Resolve, format!("song asset {}: its lyrics: {error}", song.id))),
        }
    }
    let edit = dir.join("film.cut.splash");
    std::fs::write(&edit, source).map_err(|error| io(&edit, error))?;
    let media = dir.join("media.txt");
    std::fs::write(&media, table).map_err(|error| io(&media, error))?;
    Ok((edit, media))
}

/// Resolve the cut from the library into the build folder, then run the
/// export there.
fn export(client: &mut AssetClient, job: ExportJob, progress: &mut ChannelProgress, events: &Sender<Event>) -> Result<(ExportReport, PathBuf), Vec<Diagnostic>> {
    let id = job.id;
    progress.phase(Phase::Resolve, 0.0, "reading the cut from the library");
    let (head, source, tree) = load_target(client, &job.target).map_err(|error| vec![Diagnostic::error(Phase::Resolve, format!("the cut: {error}"))])?;
    let doc = match &tree { Some((tree, path)) => CutDoc::load_tree(&source, tree.context(path)), None => CutDoc::load(&source) }
        .map_err(|diags| diags.iter().map(|diag| Diagnostic::error(Phase::Resolve, diag.message.clone()).at(tree.as_ref().map_or_else(|| "cut.splash".into(), |(_, path)| path.clone()), 0, 0)).collect::<Vec<_>>())?;
    let out_dir = job.out_dir.clone().unwrap_or_else(|| expand(&default_out_dir(&doc.label)));
    let _ = events.send(Event::Resolved { id, edit: head, label: doc.label.clone(), out_dir: out_dir.clone() });
    let source_root = source_root();
    let key = makepad_asset_data::BlobId::hash_of(format!("{head}/{}", tree.as_ref().map_or("", |(_, path)| path.as_str())).as_bytes());
    let build_dir = source_root.join("target").join("web-export").join(format!("build-{key}"));
    let mut store = StoreMedia::new(client.lane_clone());
    let mut diagnostics = Vec::new();
    let mut uses = uses(&mut store, &doc, &source, &mut diagnostics);
    progress.phase(Phase::Resolve, 0.0, &format!("{} documents, {} modules, {} pictures, {} songs from the library", uses.motions.len(), uses.modules.len(), uses.pictures.len(), uses.songs.len()));
    // A tree cut: its render job, every file where it lies (the store's cache).
    let render_job = tree.as_ref().map(|_| build_dir.join("project").join("render.job"));
    let staged = if let (Some((tree, path)), Some(listing)) = (&tree, &render_job) {
        uses.pictures.extend(tree.files.values().filter_map(|file| file.reference.map(|reference| AssetRef { id: reference.asset_id.to_string(), rev: reference.revision.to_string() })));
        makepad_story_edit_render::job::Job::from_tree(Some(&mut store), tree, path, &source, None, &build_dir.join("project"))
            .and_then(|job| {
                job.write(listing)?;
                let open = job.tree.iter().flat_map(|tree| &tree.files).find(|file| file.path == *path).map(|file| file.location.clone());
                Ok((open.unwrap_or_default(), PathBuf::new()))
            })
            .map_err(|error| Diagnostic::error(Phase::Resolve, error))
    } else { stage(&mut store, &source, &uses, &build_dir.join("edit"), &mut diagnostics) };
    for diagnostic in &diagnostics {
        progress.diagnostic(diagnostic);
    }
    let (edit, media) = match staged {
        Ok(paths) => paths,
        Err(error) => {
            progress.diagnostic(&error);
            diagnostics.push(error);
            return Err(diagnostics);
        }
    };
    if diagnostics.iter().any(|diagnostic| diagnostic.severity == Severity::Error) {
        return Err(diagnostics);
    }
    if progress.cancelled() {
        return Err(vec![Diagnostic::error(Phase::Resolve, "cancelled")]);
    }
    let credits = match job.credits {
        Some(credits) => credits,
        None => credits_of(client, &uses),
    };
    let options = ExportOptions { job: render_job, edit, media, bundle: None, out_dir: out_dir.clone(), preset: job.preset, include_music: job.include_music, credits, source_root, build_dir, pack: job.pack, safety: Default::default() };
    match web::export(&options, progress) {
        Ok(report) => Ok((report, out_dir)),
        Err(mut errors) => {
            diagnostics.append(&mut errors);
            Err(diagnostics)
        }
    }
}

// ------------------------------------------------------------------ page

#[derive(Clone, Copy, Debug, PartialEq)]
enum Outcome {
    Done,
    Failed,
    Cancelled,
}

struct Run {
    id: u64,
    label: String,
    out_dir: Option<PathBuf>,
    /// The phase running and how far it is.
    phase: Option<(Phase, f64)>,
    diagnostics: Vec<Diagnostic>,
    outcome: Option<Outcome>,
    cancel: Arc<AtomicBool>,
    report: Option<ExportReport>,
    served: Option<Result<Urls, String>>,
    edit: Option<AssetRevisionRef>,
    project: Option<(String, String)>,
}

impl Run {
    fn new(id: u64, cancel: Arc<AtomicBool>, label: String) -> Self {
        Self { id, label, out_dir: None, phase: None, diagnostics: Vec::new(), outcome: None, cancel, report: None, served: None, edit: None, project: None }
    }

    fn note(&mut self, diagnostic: Diagnostic) {
        if !self.diagnostics.contains(&diagnostic) {
            self.diagnostics.push(diagnostic);
        }
    }

    fn finish(&mut self, result: Result<ExportReport, Vec<Diagnostic>>, served: Option<Result<Urls, String>>) {
        self.phase = None;
        match result {
            Ok(report) => {
                for diagnostic in report.diagnostics.clone() {
                    self.note(diagnostic);
                }
                self.report = Some(report);
                self.served = served;
                self.outcome = Some(Outcome::Done);
            }
            Err(diagnostics) => {
                for diagnostic in diagnostics {
                    self.note(diagnostic);
                }
                let cancelled = self.cancel.load(Ordering::Acquire) && self.diagnostics.iter().any(|d| d.severity == Severity::Error && d.message == "cancelled");
                self.outcome = Some(if cancelled { Outcome::Cancelled } else { Outcome::Failed });
            }
        }
    }

    /// The studio API's answer.
    fn api_answer(&self) -> Result<Json, String> {
        let shown: Vec<Json> = self.diagnostics.iter().map(|d| json::s(d.to_string())).collect();
        match (&self.outcome, &self.report) {
            (Some(Outcome::Done), Some(report)) => {
                let files = report.files.iter().map(|file| json::obj(vec![("name", json::s(file.name.clone())), ("bytes", Json::Int(file.bytes as i64)), ("brotli", Json::Int(file.brotli as i64))])).collect();
                let fonts = report.fonts.iter().map(|font| json::s(format!("{} {} {}/{} glyphs, {} -> {}: {}", font.family, if font.subset { "subset" } else { "full" }, font.glyphs, font.glyphs_total, web::report::mb(font.bytes_before), web::report::mb(font.bytes_after), font.why))).collect();
                let mut fields = vec![
                    ("out_dir", json::s(self.out_dir.as_ref().map(|dir| dir.display().to_string()).unwrap_or_default())),
                    ("files", Json::Arr(files)),
                    ("fonts", Json::Arr(fonts)),
                    ("diagnostics", Json::Arr(shown)),
                ];
                if let Some((project, path)) = &self.project {
                    fields.push(("project", json::s(project)));
                    fields.push(("file", json::s(path)));
                } else if let Some(edit) = &self.edit {
                    fields.push(("edit", json::obj(vec![("asset_id", json::s(edit.asset_id.to_string())), ("revision", json::s(edit.revision.to_string()))])));
                }
                match &self.served {
                    Some(Ok((local, lan))) => {
                        fields.push(("local_url", json::s(local.clone())));
                        if let Some(lan) = lan {
                            fields.push(("lan_url", json::s(lan.clone())));
                        }
                    }
                    Some(Err(error)) => fields.push(("serve_error", json::s(error.clone()))),
                    None => {}
                }
                Ok(json::obj(fields))
            }
            (Some(Outcome::Cancelled), _) => Err("the web export was cancelled".into()),
            _ => Err(format!("the web export failed:\n{}", self.diagnostics.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"))),
        }
    }
}

/// The web export's state (a field of the page): its worker, the run under
/// way or the last one, and the studio API's answers.
#[derive(Default)]
pub struct WebExport {
    worker: Option<Worker>,
    run: Option<Run>,
    next_id: u64,
    /// Finished runs' answers, by run id.
    api_answers: Vec<(u64, Result<Json, String>)>,
}

impl WebExport {
    /// The finished export's folder and the address it is served at: what
    /// `reveal_file` and `open_file` may take.
    pub(super) fn written(&self) -> (Option<PathBuf>, Option<String>) {
        let Some(run) = self.run.as_ref().filter(|run| run.outcome == Some(Outcome::Done)) else { return (None, None) };
        let url = match &run.served {
            Some(Ok((local, _))) => Some(local.clone()),
            _ => None,
        };
        (run.out_dir.clone(), url)
    }

    fn worker(&mut self, cx: &mut Cx) -> Option<&mut Worker> {
        if self.worker.is_none() {
            match Worker::new(cx) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => log!("web export: no worker: {error}"),
            }
        }
        self.worker.as_mut()
    }

    fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.outcome.is_none())
    }
}

/// A pack knob named in the call.
fn knob<T>(args: &Json, key: &str, names: &[(&str, T)]) -> Result<Option<T>, String>
where
    T: Copy,
{
    let Some(name) = args.get(key).and_then(Json::as_str) else { return Ok(None) };
    names.iter().find(|(known, _)| *known == name).map(|(_, value)| Some(*value)).ok_or_else(|| {
        format!("`{key}`: `{name}` ({})", names.iter().map(|(known, _)| *known).collect::<Vec<_>>().join(", "))
    })
}

impl VideoCut {
    /// The studio API's `export_web` (the chat's Web export card confirms
    /// into it): starts an export, answered later by
    /// [`Self::web_export_api_answer`].
    pub(super) fn web_export_api_start(&mut self, cx: &mut Cx, args: &Json) -> Result<u64, String> {
        if self.web_export.running() {
            return Err("a web export is already under way".into());
        }
        if args.get("project").is_some() && args.get("cut").is_some() { return Err("give a project and file, or a standalone cut".into()); }
        if self.session.as_ref().is_some_and(|session| session.tree.is_some() && session.dirty()) || super::motion_edit::has_drafts(self) || self.saving || self.save_requested || self.in_flight.is_some() || !self.queue.is_empty() {
            return Err("save the cut before exporting".into());
        }
        let open = self.session.as_ref().and_then(|session| session.head.map(|head| (head, session.saved.clone(), session.doc.label.clone(), session.saved_tree.clone().map(|tree| (tree, session.saved_path.clone())))));
        let target = match args.get("cut").and_then(Json::as_str).map(str::trim).filter(|id| !id.is_empty()) {
            Some(id) => match &open {
                Some((head, saved, _, tree)) if head.asset_id.to_string() == id => Target::Open { head: *head, source: saved.clone(), tree: tree.clone() },
                _ => Target::Asset(id.to_string()),
            },
            None => match &open {
                Some((head, saved, _, tree)) => Target::Open { head: *head, source: saved.clone(), tree: tree.clone() },
                None => return Err("no saved cut is open in the CUT page: save it, or pass `cut` (a cut's asset id)".into()),
            },
        };
        let preset = match args.get("preset").and_then(Json::as_str) {
            None | Some("desktop") => Preset::Desktop,
            Some("mobile") => Preset::Mobile,
            Some(other) => return Err(format!("`preset`: `{other}` (desktop or mobile)")),
        };
        let mut pack = match args.get("pack").and_then(Json::as_str) {
            None => PackPreset::Film,
            Some(name) => PackPreset::parse(name).ok_or_else(|| format!("`pack`: `{name}` (film, app or full)"))?,
        }.options();
        // The knobs the call names go over the pack's.
        if let Some(fonts) = knob(args, "fonts", &[("analysed", FontsKnob::Analysed), ("full", FontsKnob::Full)])? {
            pack.fonts = fonts;
        }
        if let Some(shaders) = knob(args, "shaders", &[("prebuilt_only", ShadersKnob::PrebuiltOnly), ("keep_compiler", ShadersKnob::KeepCompiler)])? {
            pack.shaders = shaders;
        }
        if let Some(kernels) = knob(args, "kernels", &[("aot_only", KernelsKnob::AotOnly), ("keep_compiler", KernelsKnob::KeepCompiler)])? {
            pack.kernels = kernels;
        }
        if let Some(assets) = knob(args, "assets", &[("by_size", AssetsKnob::BySize), ("embedded", AssetsKnob::Embedded), ("external", AssetsKnob::External)])? {
            pack.assets = assets;
        }
        if let Some(Json::Arr(names)) = args.get("strip_js") {
            let mut stripped: Vec<String> = Vec::new();
            for name in names.iter().filter_map(Json::as_str) {
                if !web::page::STRIPPED.contains(&name) {
                    return Err(format!("`strip_js`: `{name}` is not a film.js section ({})", web::page::STRIPPED.join(", ")));
                }
                stripped.push(name.to_string());
            }
            pack.js_stripped = stripped;
        }
        let include_music = args.get("include_music").and_then(Json::as_bool).unwrap_or(true);
        let lan = args.get("lan").and_then(Json::as_bool).unwrap_or(false);
        let credits = args.get("credits").and_then(Json::as_str).map(str::to_string);
        let out_dir = args.get("out_dir").and_then(Json::as_str).map(str::trim).filter(|dir| !dir.is_empty()).map(expand);
        let label = match &target {
            Target::Open { .. } => open.map(|(_, _, label, _)| label).unwrap_or_default(),
            Target::Asset(id) => id.clone(),
        };
        let project = match &target {
            Target::Open { tree: Some((_, path)), .. } => self.session.as_ref().and_then(|session| session.alias.as_ref()).map(|alias| (alias.to_string(), path.clone())),
            _ => None,
        };
        let state = &mut self.web_export;
        state.next_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let id = state.next_id;
        let job = ExportJob { id, target, out_dir, preset, pack, include_music, credits, lan, cancel: cancel.clone() };
        let Some(worker) = state.worker(cx) else { return Err("the web export's worker did not start".into()) };
        worker.send(job);
        state.run = Some(Run::new(id, cancel, label.clone()));
        if let Some(run) = &mut state.run { run.project = project; }
        super::chat::note(self, format!("Web export of “{label}” started: the first export builds the player for the browser, a few minutes"));
        self.repaint = true;
        Ok(id)
    }

    pub(super) fn web_export_api_answer(&mut self, id: u64) -> Option<Result<Json, String>> {
        let at = self.web_export.api_answers.iter().position(|(answer, _)| *answer == id)?;
        Some(self.web_export.api_answers.remove(at).1)
    }

    /// A web export is under way.
    pub(super) fn web_export_running(&self) -> bool {
        self.web_export.running()
    }

    /// Stop the web export under way: it ends at its next step.
    pub(super) fn stop_web_export(&mut self) {
        if let Some(run) = self.web_export.run.as_ref().filter(|run| run.outcome.is_none()) {
            run.cancel.store(true, Ordering::Release);
            self.status = "Stopping the web export…".into();
            self.repaint = true;
        }
    }

    /// Every pump: held jobs, then the worker's events.
    pub(super) fn pump_web_export(&mut self, _cx: &mut Cx) {
        let state = &mut self.web_export;
        let Some(worker) = &mut state.worker else { return };
        worker.flush();
        let mut changed = false;
        let mut status = None;
        while let Ok(event) = worker.events.try_recv() {
            changed = true;
            match event {
                Event::Resolved { id, edit, label, out_dir } => {
                    if let Some(run) = state.run.as_mut().filter(|run| run.id == id) {
                        run.edit = Some(edit);
                        run.label = label;
                        run.out_dir = Some(out_dir);
                    }
                }
                Event::Phase { id, phase, done } => {
                    if let Some(run) = state.run.as_mut().filter(|run| run.id == id && run.outcome.is_none()) {
                        run.phase = Some((phase, done.clamp(0.0, 1.0)));
                        status = Some(format!("Web export: {} {:.0}%", phase.title(), done.clamp(0.0, 1.0) * 100.0));
                    }
                }
                Event::Diagnostic { id, diagnostic } => {
                    if let Some(run) = state.run.as_mut().filter(|run| run.id == id) {
                        run.note(diagnostic);
                    }
                }
                Event::Finished { id, result, served } => {
                    if let Some(run) = state.run.as_mut().filter(|run| run.id == id) {
                        run.finish(result, served);
                        state.api_answers.push((id, run.api_answer()));
                        status = Some(match run.outcome {
                            Some(Outcome::Done) => format!("Web export of “{}” done", run.label),
                            Some(Outcome::Cancelled) => "Web export cancelled".to_string(),
                            _ => "Web export failed".to_string(),
                        });
                    }
                }
            }
        }
        if let Some(status) = status {
            self.status = status;
        }
        if changed {
            self.repaint = true;
        }
    }
}
