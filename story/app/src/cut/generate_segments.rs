//! Generative segments on the CUT page: a clip or a part
//! of the timeline piped through a FLOW preset on the LAN's AI Hub nodes
//! (image-to-video, and video-to-video as keyframes of the input chunk by
//! chunk), each result a pinned `Video` take of a `Generate` node.
//!
//! The model is `makepad_story_cut::generate` (the node, its key and
//! staleness, chunking, the helper edits). This module is the host:
//!
//! 1. **Input.** The input alone is written as its own edit at the model's
//!    size ([`input_edit_source`]) and its keyframes are grabbed on locked
//!    time by `makepad-edit-render thumbnail`, turned into PNGs and
//!    published as `Texture` assets (a job's media inputs are library
//!    revisions, recorded as provenance parents).
//! 2. **Jobs.** Every take (one seed each) runs one FLOW job per chunk
//!    through `FlowStudio::run_preset`, never with `preview` unless asked:
//!    only presets whose generators are all `video` models installed on a
//!    LAN node now are offered or run (a job's model kind is checked before
//!    anything is sent). Chunks of an image-to-video span run one after the
//!    other, each from the last frame of the one before.
//! 3. **Takes.** A take's chunks are laid on a small edit, dissolved across
//!    their overlaps and stretched to the span ([`blend_edit_source`]),
//!    rendered by `makepad-edit-render`, and published as one `Video` with
//!    its provenance (preset, model, prompt, seed, chunk and keyframe
//!    parents, the key).
//! 4. **The cut.** The takes are appended to the node's `takes` and the
//!    first new one chosen, as one undoable change of the page's source.
//!
//! The UI is one inspector section (GENERATE) for the selected clip or the
//! generate under the playhead: its state, per-take progress and the take
//! switcher with thumbnails. Making and changing a generate is the chat's
//! (`generate_add`, `generate_run`, `generate_cancel` cards; ⌃R / ⇧⌃R open
//! the card), through the catalogue (studio_api).
//!
//! Main-loop cost: the pump drains a bounded channel (`try_recv`), reads
//! each running FLOW job's new events, and recomputes node states only
//! when the document revision changes (a Generate's key samples its input's
//! frame plan per output frame of its span: about a microsecond a frame).
//! Rendering, grabbing, publishing and every Splash evaluation run on one
//! long-lived worker thread; the renderer runs as a hidden child process.

use super::ops::{media_value, CutOp};
use super::patch::{NewNode, Patcher, Value};
use super::store::StoreJob;
use makepad_asset_connection as asset_connection;
use makepad_flow_studio::{JobEvent, JobPhase, PresetInfo, RunRequest};
use makepad_asset_connection::redact;
use super::super::StoryApp;
use super::VideoCut;
use makepad_asset_client::{AssetClient, PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetKind, AssetRevisionRef, DerivativePolicy, FileRole, MediaType, Redistribution, ThumbnailMedia};
use makepad_flow::{Graph, Literal, NodeInputValue, PortType};
use makepad_flow::graph::edges_of;
use makepad_story_edit_render::MediaResolver;
use makepad_story_cut::generate::{
    blend_edit_source, chunks, chunks_in_sequence, generate_state, input_digest, input_edit_source, next_seeds, settings_digest,
    BlendPart, Chunk, GenState, GenTake, GenerateNode, Staleness, KEY_VERSION,
};
use makepad_story_cut::{AssetRef, CutDoc, CutEval, CutNodeKind, CutTree, Fit, MediaRef};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::makepad_platform::thread::SignalToUI;
use makepad_widgets::*;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

/// The video models' own frame rate (MiniMax H3: 24 fps; its frame counts
/// snap to 17n+5).
const MODEL_FPS: f64 = 24.0;
/// Frames one job makes when neither the node nor the preset says (H3's
/// default, about 5.2 s): a chunk's maximum length.
const DEFAULT_FRAMES: f64 = 124.0;
/// Seconds consecutive chunks share, blended at the seam.
const OVERLAP: f64 = 0.5;
/// How long one renderer run may take before it is stopped and the take
/// fails (a source that never arrives, a hung GPU). Generous: a take's
/// render is seconds; the failure is reported, never silent.
const RENDER_TIMEOUT: f64 = 600.0;
const QUEUE: usize = 8;
/// Seconds before a chunk the fleet turned away for lack of room asks again.
const RETRY: f64 = 20.0;
/// The take switcher's tiles.
const TILES: usize = 6;
/// Library namespace and tags of what this publishes.
const NAMESPACE: &str = "stage";
const ORIGIN: &str = "Generate segment";

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let GsCaps = Label{width: Fit text: "" draw_text +: {color: stage.secondary text_style: theme.font_bold{font_size: 7.5}}}
    let GsNote = Label{width: Fill height: Fit text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
    // One take: its poster, and a button naming it (the chosen one marked).
    let GsTile = View{visible: false width: 84 height: Fit flow: Down spacing: 2
        poster := Image{width: 84 height: 48 fit: ImageFit.Smallest}
        pick := StageButton{width: 84 height: 20 margin: 0 padding: Inset{left: 4 right: 4} text: ""
            draw_text +: {text_style: theme.font_regular{font_size: 8}}}}

    mod.widgets.CutGenerateSection = View{visible: false width: Fill height: Fit flow: Down
        View{width: Fill height: 28 flow: Right padding: Inset{left: 12 right: 12} align: Align{y: 0.5}
            GsCaps{text: "GENERATE"}
            View{width: Fill height: 1}
            gs_badge := RoundedView{width: Fit height: 16 padding: Inset{left: 6 right: 6} align: Align{y: 0.5} show_bg: true
                draw_bg +: {color: stage.raised border_radius: 3.0}
                gs_badge_text := Label{width: Fit text: "" draw_text +: {color: stage.warm text_style: theme.font_bold{font_size: 7.5}}}}
        }
        View{width: Fill height: Fit flow: Down spacing: 4 padding: Inset{left: 12 right: 12 bottom: 8}
            gs_state := Label{width: Fill height: Fit text: "" draw_text +: {color: stage.text text_style: theme.font_regular{font_size: 9}}}
            gs_note := GsNote{}
            gs_takes := View{width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 4 margin: Inset{top: 4}
                gs_t0 := GsTile{} gs_t1 := GsTile{} gs_t2 := GsTile{} gs_t3 := GsTile{} gs_t4 := GsTile{} gs_t5 := GsTile{}}
        }
        SolidView{width: Fill height: 1 draw_bg +: {color: stage.line_faint}}
    }
}

pub(super) fn register(vm: &mut ScriptVm) {
    script_mod(vm);
}

const TILE_IDS: [LiveId; TILES] = [live_id!(gs_t0), live_id!(gs_t1), live_id!(gs_t2), live_id!(gs_t3), live_id!(gs_t4), live_id!(gs_t5)];

// ------------------------------------------------------------------ state

/// A video preset a Generate can use, as the LAN offers it now.
#[derive(Clone, Debug)]
pub(super) struct VideoPreset {
    pub key: String,
    pub label: String,
    /// Every generator's `video` model is installed on some LAN node.
    pub runnable: bool,
    /// Those nodes.
    pub nodes: Vec<String>,
    /// The media input the keyframe goes to, and a `last_frame` one.
    image: String,
    last_frame: Option<String>,
    /// The text input the prompt goes to.
    prompt: Option<String>,
    /// The pinned model ("" when the hub chooses among installed ones).
    pub model: String,
    /// `(node, key, value)` parameters a request may set.
    params: Vec<(String, String, String)>,
}

impl VideoPreset {
    fn has_param(&self, key: &str) -> bool {
        self.params.iter().any(|(_, name, _)| name == key)
    }

    fn param_number(&self, key: &str) -> Option<f64> {
        self.params.iter().find(|(_, name, _)| name == key).and_then(|(_, _, value)| value.parse().ok())
    }
}

/// The presets a Generate may run: VIDEO presets that take a picture and
/// whose generators are all `video` models (a text-to-image or edit model
/// never gets a video job, and the other way round).
pub(super) fn video_presets(infos: &[PresetInfo]) -> Vec<VideoPreset> {
    infos.iter().filter_map(|info| {
        if info.needs.is_empty() || !info.needs.iter().all(|(_, domain, _, _)| domain == "video") {
            return None;
        }
        let image = info.inputs.iter().find(|(node, ty, _, _)| *ty == PortType::Image && node != "last_frame")?.0.clone();
        let last_frame = info.inputs.iter().find(|(node, ty, _, _)| *ty == PortType::Image && node == "last_frame").map(|input| input.0.clone());
        let prompt = info.inputs.iter().find(|(node, ty, _, _)| *ty == PortType::Text && node == "prompt").map(|input| input.0.clone());
        Some(VideoPreset {
            key: info.key.clone(),
            label: info.label.clone(),
            runnable: info.runnable,
            nodes: info.nodes.clone(),
            image,
            last_frame,
            prompt,
            model: info.needs.first().map(|need| need.2.clone()).unwrap_or_default(),
            params: info.params.clone(),
        })
    }).collect()
}

/// A segment's state for the render bar and the take switcher.
#[derive(Clone, Debug, PartialEq)]
pub enum SegmentState {
    NeedsGeneration(GenState),
    /// Generating: fraction done over all its takes.
    Generating(f64),
    Generated,
}

/// One Generate node as a render-bar segment (the P1 render cache's
/// provider for generative segments).
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub node: String,
    pub start: f64,
    pub end: f64,
    pub state: SegmentState,
    pub takes: usize,
    pub chosen: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ChunkPhase {
    /// Waiting for its keyframe (image-to-video: the previous chunk).
    Waiting,
    Running(u64),
    Done,
    Failed,
}

struct ChunkJob {
    chunk: Chunk,
    first: Option<AssetRevisionRef>,
    last: Option<AssetRevisionRef>,
    phase: ChunkPhase,
    permille: u16,
    video: Option<AssetRevisionRef>,
    /// A chunk the fleet turned away (busy, no VRAM free now) waits until
    /// then and runs again.
    retry_at: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TakePhase {
    Chunks,
    /// Its chunks are being blended and published.
    Finishing,
    Done,
    Failed,
}

struct TakeJob {
    seed: u64,
    chunks: Vec<ChunkJob>,
    phase: TakePhase,
    model: String,
    hubs: Vec<String>,
    result: Option<GenTake>,
    error: Option<String>,
}

impl TakeJob {
    fn progress(&self) -> f64 {
        match self.phase {
            TakePhase::Done | TakePhase::Failed => 1.0,
            TakePhase::Finishing => 0.95,
            TakePhase::Chunks => {
                let done: f64 = self.chunks.iter().map(|chunk| match chunk.phase {
                    ChunkPhase::Done | ChunkPhase::Failed => 1.0,
                    ChunkPhase::Running(_) => chunk.permille as f64 / 1000.0,
                    ChunkPhase::Waiting => 0.0,
                }).sum();
                0.1 + 0.85 * done / self.chunks.len().max(1) as f64
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum JobPhase_ {
    /// Grabbing and publishing the keyframes.
    Keyframes,
    Generating,
    /// Waiting to write the takes into the cut.
    Patching,
    Done,
    Failed(String),
    Cancelled,
}

struct GenJob {
    id: u64,
    node: String,
    /// The node as it was asked for.
    gen: GenerateNode,
    preset: VideoPreset,
    preview: bool,
    size: (u32, u32),
    /// Frames each job makes at most.
    frames: f64,
    cut_fps: f64,
    dir: PathBuf,
    phase: JobPhase_,
    input_digest: Option<u64>,
    keyframes: Vec<AssetRevisionRef>,
    takes: Vec<TakeJob>,
    status: String,
    /// The source a patch was computed on, and the patched source.
    patch: Option<(String, String)>,
    patch_asked: bool,
    /// The status and take lines last written to the log.
    logged: String,
}

impl GenJob {
    fn active(&self) -> bool {
        !matches!(self.phase, JobPhase_::Done | JobPhase_::Failed(_) | JobPhase_::Cancelled)
    }

    fn progress(&self) -> f64 {
        match self.phase {
            JobPhase_::Keyframes => 0.05,
            JobPhase_::Generating => self.takes.iter().map(TakeJob::progress).sum::<f64>() / self.takes.len().max(1) as f64,
            _ => 1.0,
        }
    }
}

/// What the page or the AI asked for, done on the next pump (it needs the
/// FLOW studio).
enum Intent {
    Generate { node: String, count: usize, preview: bool, reply: Option<u64> },
    /// Stop a node's generation: its FLOW jobs are cancelled.
    Cancel { node: String },
}

struct State {
    worker: Option<Worker>,
    jobs: Vec<GenJob>,
    next: u64,
    intents: VecDeque<Intent>,
    presets: Vec<VideoPreset>,
    presets_at: f64,
    /// Node states by id, for the document revision they were computed at.
    states: HashMap<String, GenState>,
    states_rev: Option<u64>,
    /// Wrap requests on the worker: the source they were computed on.
    wrapping: Option<String>,
    /// A wrap or patch result waiting for the page to be idle: `(from, to)`.
    rewrite: Option<(String, String, String)>,
    /// What the section last showed, so it refills only on change.
    shown: String,
    /// Finished API calls: id → result.
    replies: HashMap<u64, Result<Json, String>>,
    next_reply: u64,
    /// The section is showing: keep the LAN's video presets fresh.
    showing: bool,
    error: Option<String>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(cx: &mut Cx, f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let state = cell.get_or_insert_with(|| {
            let worker = Worker::new(cx);
            let error = worker.as_ref().err().cloned();
            State {
                worker: worker.ok(), jobs: Vec::new(), next: 1, intents: VecDeque::new(), presets: Vec::new(), presets_at: -1.0,
                states: HashMap::new(), states_rev: None, wrapping: None, rewrite: None, shown: String::new(),
                replies: HashMap::new(), next_reply: 1, showing: false, error,
            }
        });
        f(state)
    })
}

/// Only when the state exists (never creates the worker).
fn peek<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|cell| cell.borrow_mut().as_mut().map(f))
}

// ------------------------------------------------------------------ worker

enum WJob {
    /// The input's keyframes at `times` as published PNG textures, and its
    /// digest, from the preview's loaded cut and immutable tree snapshot.
    Keyframes { job: u64, doc: Arc<CutDoc>, tree: Option<(Arc<makepad_asset_data::tree::TreeDraft>, String)>, node: String, size: (u32, u32), times: Vec<f64>, dir: PathBuf },
    /// The last frame of a generated chunk, as a published PNG.
    LastFrame { job: u64, take: usize, chunk: usize, video: AssetRevisionRef, size: (u32, u32), dir: PathBuf },
    /// A take's chunks blended, rendered and published.
    Finish { job: u64, take: usize, finish: Finish },
    /// The node's takes written into `source`.
    Patch { job: u64, source: String, tree: Option<(Arc<makepad_asset_data::tree::TreeDraft>, String)>, node: String, takes: Vec<GenTake>, take: usize },
    /// A Generate node put over a clip or a range (`reply`: an API call
    /// waiting for its id).
    Wrap { source: String, tree: Option<(Arc<makepad_asset_data::tree::TreeDraft>, String)>, wrap: Wrap, reply: Option<u64> },
}

struct Finish {
    parts: Vec<(AssetRevisionRef, Chunk)>,
    keyframes: Vec<AssetRevisionRef>,
    length: f64,
    size: (u32, u32),
    fps: f64,
    fit: Fit,
    dir: PathBuf,
    title: String,
    prompt: String,
    model: String,
    key: String,
    preset: String,
    seed: u64,
}

enum WDone {
    Keyframes { job: u64, result: Result<(u64, Vec<AssetRevisionRef>), String> },
    LastFrame { job: u64, take: usize, chunk: usize, result: Result<AssetRevisionRef, String> },
    Finished { job: u64, take: usize, result: Result<AssetRevisionRef, String> },
    Patched { job: u64, source: String, result: Result<String, String> },
    Wrapped { source: String, reply: Option<u64>, result: Result<(String, String), String> },
}

struct Worker {
    jobs: SyncSender<WJob>,
    done: Receiver<WDone>,
    held: VecDeque<WJob>,
}

impl Worker {
    fn new(cx: &mut Cx) -> Result<Self, String> {
        let (job_tx, job_rx) = sync_channel::<WJob>(QUEUE);
        let (done_tx, done) = sync_channel::<WDone>(QUEUE);
        let options = ThreadOptions { name: Some(Arc::from("stage-generate-segments")), stack_size: Some(8 * 1024 * 1024), priority: CxThreadPriority::Utility };
        let handle = cx.thread_spawner().spawn_worker(options, move || {
            let origin = asset_connection::origin();
            let mut client: Option<AssetClient> = None;
            let mut patcher: Option<Patcher> = None;
            while let Ok(job) = job_rx.recv() {
                let done = run(job, &origin, &mut client, &mut patcher);
                // The UI takes replies every pump; waiting here is this
                // thread's own time.
                if done_tx.send(done).is_err() {
                    break;
                }
                SignalToUI::set_ui_signal();
            }
        }).map_err(|error| format!("generate worker: {error:?}"))?;
        handle.detach();
        Ok(Self { jobs: job_tx, done, held: VecDeque::new() })
    }

    fn send(&mut self, job: WJob) {
        self.held.push_back(job);
        self.flush();
    }

    fn flush(&mut self) {
        while let Some(job) = self.held.pop_front() {
            match self.jobs.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => {
                    self.held.push_front(job);
                    break;
                }
            }
        }
    }
}


fn run(job: WJob, origin: &asset_connection::AssetOrigin, client: &mut Option<AssetClient>, patcher: &mut Option<Patcher>) -> WDone {
    match job {
        WJob::Keyframes { job, doc, tree, node, size, times, dir } => {
            let result = asset_connection::connected(client, origin).and_then(|client| keyframes(client, &doc, tree.as_ref(), &node, size, &times, &dir));
            if result.is_err() {
                *client = None;
            }
            WDone::Keyframes { job, result }
        }
        WJob::LastFrame { job, take, chunk, video, size, dir } => {
            let result = asset_connection::connected(client, origin).and_then(|client| last_frame(client, &video, size, &dir, take, chunk));
            WDone::LastFrame { job, take, chunk, result }
        }
        WJob::Finish { job, take, finish } => {
            let result = asset_connection::connected(client, origin).and_then(|client| finish_take(client, &finish, take));
            WDone::Finished { job, take, result }
        }
        WJob::Patch { job, source, tree, node, takes, take } => {
            let result = the_patcher(patcher, tree).and_then(|patcher| write_takes(patcher, &source, &node, &takes, take));
            WDone::Patched { job, source, result }
        }
        WJob::Wrap { source, tree, wrap, reply } => {
            let result = the_patcher(patcher, tree).and_then(|patcher| wrap_in_generate(patcher, &source, &wrap));
            WDone::Wrapped { source, reply, result }
        }
    }
}

fn the_patcher(patcher: &mut Option<Patcher>, tree: Option<(Arc<makepad_asset_data::tree::TreeDraft>, String)>) -> Result<&Patcher, String> {
    if patcher.is_none() {
        *patcher = Some(Patcher::new()?);
    }
    let patcher = patcher.as_mut().unwrap();
    patcher.tree = tree;
    Ok(patcher)
}

/// The renderer binary (as `render.rs` finds it): `STAGE_EDIT_RENDER`,
/// next to Stage, or the workspace's target folder for this profile.
fn binary() -> Result<PathBuf, String> {
    let name = if cfg!(windows) { "makepad-edit-render.exe" } else { "makepad-edit-render" };
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("STAGE_EDIT_RENDER") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe.with_file_name(name));
    }
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../target").join(profile).join(name));
    candidates.into_iter().find(|path| path.is_file())
        .ok_or_else(|| "the renderer is not built (cargo build --release -p makepad-story-edit-render)".to_string())
}

/// Run the renderer hidden with `args` until it exits; its log beside.
fn renderer(args: &[String], log: &Path) -> Result<(), String> {
    let stderr = std::fs::File::create(log).map_err(|error| format!("{}: {error}", log.display()))?;
    let mut child = Command::new(binary()?)
        .args(args)
        .arg("--parent-pipe")
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| format!("could not start the renderer: {error}"))?;
    // Held open while it runs: a closed pipe tells it Stage has gone.
    let _stdin = child.stdin.take();
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed().as_secs_f64() > RENDER_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                log!("generate: the renderer ran past {RENDER_TIMEOUT} s and was stopped ({})", log.display());
                return Err(format!("the renderer did not finish in {RENDER_TIMEOUT} s"));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(error) => return Err(error.to_string()),
        }
    };
    if status.success() {
        return Ok(());
    }
    let error = std::fs::read_to_string(log).ok().and_then(|text| {
        text.lines().rev().find_map(|line| line.split_once("edit-render: FAILED: ").map(|(_, error)| error.to_string()))
    });
    Err(error.unwrap_or_else(|| format!("the renderer exited with {status}")))
}

fn exact(asset: &AssetRef) -> Result<AssetRevisionRef, String> {
    Ok(AssetRevisionRef {
        asset_id: asset.id.parse().map_err(|_| format!("`{}` is not an asset id", asset.id))?,
        revision: asset.rev.parse().map_err(|_| format!("`{}` is not a revision id", asset.rev))?,
    })
}

fn asset_ref(reference: &AssetRevisionRef) -> AssetRef {
    AssetRef { id: reference.asset_id.to_string(), rev: reference.revision.to_string() }
}

/// The render job of cut `source` (made here; its tree's file when
/// `tree`) as `<name>.job` in `dir`: every file it reads where it lies, the
/// cut itself under `<name>.files` (it is nowhere else).
fn prepare(client: &mut AssetClient, source: &str, tree: Option<&(Arc<makepad_asset_data::tree::TreeDraft>, String)>, dir: &Path, name: &str) -> Result<PathBuf, String> {
    use makepad_story_edit_render::job::Job;
    let mut store = makepad_story_edit_render::StoreMedia::new(client.lane_clone());
    let scratch = dir.join(format!("{name}.files"));
    let job = match tree {
        Some((tree, path)) => Job::from_tree(Some(&mut store), tree, path, source, None, &scratch)?,
        None => Job::from_cut(&mut store, None, source, &scratch)?,
    };
    let listing = dir.join(format!("{name}.job"));
    job.write(&listing)?;
    Ok(listing)
}

/// A JPEG the renderer wrote, as RGB at exactly `size` (covering it).
fn read_rgb(path: &Path, size: (u32, u32)) -> Result<(Vec<u8>, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let image = ImageBuffer::from_jpg(&bytes).map_err(|error| format!("{}: {error:?}", path.display()))?;
    let (w, h) = (size.0 as usize, size.1 as usize);
    let (sw, sh) = (image.width.max(1), image.height.max(1));
    // Cover: scale to fill, centre crop.
    let scale = (w as f64 / sw as f64).max(h as f64 / sh as f64);
    let (ox, oy) = ((sw as f64 * scale - w as f64) * 0.5, (sh as f64 * scale - h as f64) * 0.5);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let sy = (((y as f64 + 0.5 + oy) / scale) as usize).min(sh - 1);
        for x in 0..w {
            let sx = (((x as f64 + 0.5 + ox) / scale) as usize).min(sw - 1);
            let px = image.data[sy * sw + sx];
            rgb.extend_from_slice(&[(px >> 16) as u8, (px >> 8) as u8, px as u8]);
        }
    }
    Ok((rgb, bytes))
}

/// A catalogue thumbnail (256..=4096 px a side) of an RGB picture.
fn thumbnail(rgb: &[u8], size: (u32, u32)) -> Result<PublishThumbnail, String> {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let scale = (256.0 / w.min(h).max(1) as f64).max(1.0).min(4096.0 / w.max(h).max(1) as f64);
    let (tw, th) = (((w as f64 * scale).round() as usize).max(256), ((h as f64 * scale).round() as usize).max(256));
    let mut out = Vec::with_capacity(tw * th * 3);
    for y in 0..th {
        let sy = ((y * h) / th).min(h - 1);
        for x in 0..tw {
            let sx = ((x * w) / tw).min(w - 1);
            out.extend_from_slice(&rgb[(sy * w + sx) * 3..(sy * w + sx) * 3 + 3]);
        }
    }
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 85).encode(&out, tw as u16, th as u16, jpeg_encoder::ColorType::Rgb).map_err(|error| format!("thumbnail: {error}"))?;
    Ok(PublishThumbnail::plain(jpeg, ThumbnailMedia::Jpeg, tw as u32, th as u32))
}

/// What generated segments publish: made here from the cut's own
/// pictures, kept on the LAN and for local previews (their sources' terms
/// are not re-checked here).
fn rights() -> PublishRights {
    PublishRights::declared("LicenseRef-Stage-Generated-Derivative", "Generated in Stage from a cut's pictures", "", Redistribution::LanLocal, DerivativePolicy::LocalPreview)
}

/// Grab the frame of `edit` at `t` (edit seconds) at `size`, publish it as
/// a PNG texture.
fn grab_and_publish(client: &mut AssetClient, dir: &Path, job: &Path, t: f64, size: (u32, u32), title: &str, name: &str) -> Result<AssetRevisionRef, String> {
    let jpeg = dir.join(format!("{name}.jpg"));
    let mut args: Vec<String> = vec!["thumbnail".into(), "--job".into(), job.to_string_lossy().to_string()];
    args.extend([
        "--at".into(), format!("{t}"), "--width".into(), size.0.to_string(),
        "--out".into(), jpeg.to_string_lossy().to_string(),
    ]);
    renderer(&args, &dir.join(format!("{name}.log")))?;
    let (rgb, _) = read_rgb(&jpeg, size)?;
    let png = makepad_ai_hub::testpattern::encode_png_rgb8(&rgb, size.0 as usize, size.1 as usize).map_err(|error| error.to_string())?;
    std::fs::write(dir.join(format!("{name}.png")), &png).map_err(|error| error.to_string())?;
    let mut bundle = PublishBundle::new(NAMESPACE, AssetKind::Texture, title, vec![PublishBundleFile::bytes(FileRole::Texture, MediaType::Png, png, Some(size))], thumbnail(&rgb, size)?, rights());
    bundle.generator = "stage-generate".into();
    bundle.tags = vec!["generate".into(), "keyframe".into()];
    let published = client.publish_bundle(&bundle).map_err(redact)?;
    Ok(AssetRevisionRef { asset_id: published.asset_id, revision: published.revision })
}

fn keyframes(client: &mut AssetClient, doc: &CutDoc, context: Option<&(Arc<makepad_asset_data::tree::TreeDraft>, String)>, node: &str, size: (u32, u32), times: &[f64], dir: &Path) -> Result<(u64, Vec<AssetRevisionRef>), String> {
    let gen = doc.generate(node).ok_or_else(|| format!("the cut has no generate `{node}`"))?.clone();
    // The key reads the input's plans through nested cuts too.
    let mut store = makepad_story_edit_render::StoreMedia::new(client.lane_clone());
    let tree = CutTree::from_doc(doc.clone(), None, &mut |edit| store.cut_source(edit))
        .map_err(|diags| diags.iter().map(|diag| diag.message.clone()).collect::<Vec<_>>().join("; "))?;
    let digest = input_digest(&tree.eval(), &gen);
    let input = input_edit_source(doc, &gen, size.0, size.1, doc.output.fps)?;
    let job = prepare(client, &input, context, dir, "input.splash")?;
    let mut refs = Vec::new();
    for (index, t) in times.iter().enumerate() {
        let title = format!("{} · input of {} at {t:.2} s", doc.label, gen.id);
        refs.push(grab_and_publish(client, dir, &job, *t, size, &title, &format!("key{index}"))?);
    }
    Ok((digest, refs))
}

/// A published video's length in seconds (its manifest's media length).
fn video_seconds(client: &mut AssetClient, video: &AssetRevisionRef) -> Result<f64, String> {
    let manifest = client.resolve_ref(video).map_err(redact)?;
    let millis = manifest.metrics.media_millis;
    if millis == 0 {
        return Err(format!("{} has no media length", video.asset_id));
    }
    Ok(millis as f64 / 1000.0)
}

fn last_frame(client: &mut AssetClient, video: &AssetRevisionRef, size: (u32, u32), dir: &Path, take: usize, chunk: usize) -> Result<AssetRevisionRef, String> {
    let seconds = video_seconds(client, video)?;
    let part = BlendPart { video: asset_ref(video), chunk: Chunk { start: 0.0, length: seconds }, seconds };
    let edit = blend_edit_source(&[part], seconds, size.0, size.1, MODEL_FPS, Fit::Fill);
    let name = format!("last-{take}-{chunk}.splash");
    let job = prepare(client, &edit, None, dir, &name)?;
    let t = (seconds - 1.0 / MODEL_FPS).max(0.0);
    grab_and_publish(client, dir, &job, t, size, &format!("Generated chunk {chunk} · last frame"), &format!("last-{take}-{chunk}"))
}

fn finish_take(client: &mut AssetClient, finish: &Finish, take: usize) -> Result<AssetRevisionRef, String> {
    let mut parts = Vec::new();
    for (video, chunk) in &finish.parts {
        parts.push(BlendPart { video: asset_ref(video), chunk: *chunk, seconds: video_seconds(client, video)? });
    }
    let edit = blend_edit_source(&parts, finish.length, finish.size.0, finish.size.1, finish.fps, finish.fit);
    let name = format!("take{take}.splash");
    let job = prepare(client, &edit, None, &finish.dir, &name)?.to_string_lossy().to_string();
    let out = finish.dir.join(format!("take{take}.mp4"));
    renderer(&[
        "render".into(), "--job".into(), job.clone(),
        "--out".into(), out.to_string_lossy().to_string(),
    ], &finish.dir.join(format!("take{take}.log")))?;
    let poster = finish.dir.join(format!("take{take}-poster.jpg"));
    renderer(&[
        "thumbnail".into(), "--job".into(), job,
        "--at".into(), format!("{}", finish.length * 0.5), "--width".into(), finish.size.0.to_string(),
        "--out".into(), poster.to_string_lossy().to_string(),
    ], &finish.dir.join(format!("take{take}-poster.log")))?;
    let (rgb, _) = read_rgb(&poster, finish.size)?;
    let files = vec![PublishBundleFile::file(FileRole::Video, MediaType::Mp4, out.clone(), None)];
    let mut bundle = PublishBundle::new(NAMESPACE, AssetKind::Video, finish.title.clone(), files, thumbnail(&rgb, finish.size)?, rights());
    bundle.media_millis = (finish.length * 1000.0).round() as u32;
    bundle.generator = "stage-generate".into();
    bundle.model = finish.model.clone();
    bundle.prompt = finish.prompt.clone();
    bundle.provenance = format!("{KEY_VERSION} key {} · preset {} · seed {} · {} chunk(s)", finish.key, finish.preset, finish.seed, finish.parts.len());
    bundle.dependencies = finish.parts.iter().map(|(video, _)| *video).chain(finish.keyframes.iter().copied()).collect();
    bundle.dependencies.dedup();
    bundle.tags = vec!["generate".into(), "take".into(), "flow".into()];
    let published = client.publish_bundle(&bundle).map_err(redact)?;
    Ok(AssetRevisionRef { asset_id: published.asset_id, revision: published.revision })
}

/// `{take: asset(…) seed: … key: "…" seconds: … model: "…"}`: the object
/// the prelude's `take()` makes, written plain so the page's patcher can
/// rewrite the list.
fn take_value(take: &GenTake) -> Value {
    let mut fields = vec![("take", media_value(&MediaRef::Asset(take.video.clone()))), ("seed", Value::num(take.seed as f64))];
    fields.push(("key", take.key.as_deref().map_or_else(Value::nil, Value::string)));
    fields.push(("seconds", take.seconds.map_or_else(Value::nil, Value::num)));
    fields.push(("model", Value::string(&take.model)));
    Value::object(fields)
}

fn write_takes(patcher: &Patcher, source: &str, node: &str, takes: &[GenTake], take: usize) -> Result<String, String> {
    let list = Value::list(takes.iter().map(take_value).collect());
    patcher.set_params(source, Some(node), &[("takes".into(), list), ("take".into(), Value::num(take as f64))])
}

/// What a new Generate goes over.
#[derive(Clone, Debug)]
pub(super) enum WrapTarget {
    /// A clip and its effects: the Generate takes its chain's picture, over
    /// the clip's span, on its track.
    Clip(String),
    /// The whole picture over `[start, start + length)`.
    Range(f64, f64),
}

#[derive(Clone, Debug)]
pub(super) struct Wrap {
    pub target: WrapTarget,
    pub preset: String,
    pub prompt: String,
    pub seed: u64,
    pub frame_at: Option<f64>,
}

/// The node standing for `id`'s picture: `id`, or the effects that wrap
/// it, followed up while one effect reads it.
fn chain_top(doc: &CutDoc, id: &str) -> String {
    let mut current = id.to_string();
    for _ in 0..doc.nodes.len() {
        let readers: Vec<&str> = doc.wiring().filter(|(from, port, _, _)| *from == current && *port == "frame").map(|(_, _, to, _)| to).collect();
        match readers.as_slice() {
            [single] if matches!(doc.node(single).map(|node| &node.kind), Some(CutNodeKind::Fx { .. })) => current = single.to_string(),
            _ => break,
        }
    }
    current
}

/// Every reader of `from`'s picture but `except` reads `to`'s instead.
fn redirect(graph: &Graph, from: &str, to: &str, except: &str) -> Graph {
    let mut next = graph.clone();
    for consumer in next.nodes.iter_mut().filter(|node| node.id != except) {
        for input in &mut consumer.inputs {
            match &mut input.value {
                NodeInputValue::Edge(edge) if edge.from_node == from && edge.from_port == "frame" => edge.from_node = to.to_string(),
                NodeInputValue::Edges(entries) => {
                    for entry in entries.iter_mut().filter(|entry| entry.from.from_node == from && entry.from.from_port == "frame") {
                        entry.from.from_node = to.to_string();
                    }
                }
                _ => {}
            }
        }
    }
    next.edges = edges_of(&next.nodes);
    next
}

/// Put a new Generate over `wrap.target`: returns the new source and the
/// node's id.
fn wrap_in_generate(patcher: &Patcher, source: &str, wrap: &Wrap) -> Result<(String, String), String> {
    let doc = patcher.load(source)?;
    let (input, start, length, track) = match &wrap.target {
        WrapTarget::Clip(id) => {
            let node = doc.node(id).ok_or_else(|| format!("the cut has no `{id}`"))?;
            if !matches!(node.kind, CutNodeKind::Clip { .. } | CutNodeKind::Still { .. } | CutNodeKind::Title { .. }) {
                return Err(format!("`{id}` is not a clip"));
            }
            let span = node.span.ok_or_else(|| format!("`{id}` has no span"))?;
            (chain_top(&doc, id), span.start, span.length, node.track)
        }
        WrapTarget::Range(start, length) => {
            let CutNodeKind::Output { frame: Some(frame), .. } = &doc.node(&doc.output_node).ok_or("the cut has no output")?.kind else {
                return Err("the cut's output shows nothing".into());
            };
            (frame.node.clone(), *start, *length, 0)
        }
    };
    if doc.generates.iter().any(|gen| gen.input.as_ref().is_some_and(|wire| wire.node == input) && gen.span.start < start + length && start < gen.span.end()) {
        return Err("a generate already takes this picture there".into());
    }
    let id = patcher.fresh_id(source, "gen");
    let mut params = vec![
        ("span".to_string(), Value::span(start.max(0.0), length.max(0.1))),
        ("flow".to_string(), Value::string(&wrap.preset)),
        ("prompt".to_string(), Value::string(&wrap.prompt)),
        ("seed".to_string(), Value::num(wrap.seed as f64)),
        ("track".to_string(), Value::num(track as f64)),
    ];
    if let Some(at) = wrap.frame_at {
        params.push(("frame_at".to_string(), Value::num(at)));
    }
    let new = NewNode { id: id.clone(), type_name: "Generate".into(), at: (start * 40.0 + 380.0, track as f64 * 90.0 + 80.0), params, wires: vec![("input".into(), input.clone(), "frame".into())] };
    let added = patcher.add_nodes(source, &[new])?;
    let written = patcher.rewire(&added, |graph| Ok(redirect(graph, &input, &id, &id)))?;
    patcher.load(&written)?;
    Ok((written, id))
}

// ------------------------------------------------------------------ jobs

/// The model's size for a cut's aspect: the node's `width`/`height`, the
/// preset's, else H3's own canvases (640×352, 352×640, 512×512).
fn model_size(gen: &GenerateNode, preset: &VideoPreset, output: (u32, u32)) -> (u32, u32) {
    let pick = |key: &str| gen.number(key).or_else(|| preset.param_number(key)).filter(|value| *value >= 16.0).map(|value| (value as u32) & !15);
    if let (Some(width), Some(height)) = (pick("width"), pick("height")) {
        return (width, height);
    }
    let aspect = output.0 as f64 / output.1.max(1) as f64;
    if aspect > 1.2 {
        (640, 352)
    } else if aspect < 0.83 {
        (352, 640)
    } else {
        (512, 512)
    }
}

impl State {
    fn job(&mut self, id: u64) -> Option<&mut GenJob> {
        self.jobs.iter_mut().find(|job| job.id == id)
    }

    fn running(&self, node: &str) -> Option<&GenJob> {
        self.jobs.iter().find(|job| job.node == node && job.active())
    }

    fn send(&mut self, job: WJob) {
        match &mut self.worker {
            Some(worker) => worker.send(job),
            None => self.error = Some("the generate worker is not running".into()),
        }
    }
}

/// The directory a job keeps its files in.
fn job_dir(edit: &str, job: u64) -> PathBuf {
    makepad_widgets::makepad_platform::home::app_state_home().join("stage-product").join("generate").join(edit).join(format!("{}-{job}", std::process::id()))
}

impl StoryApp {
    /// Pumps generative segments (after the page's own pump): starts what
    /// was asked for, follows its FLOW jobs, writes finished takes.
    pub(super) fn pump_generate_segments(&mut self, cx: &mut Cx) {
        let wanted = peek(|state| state.showing || !state.intents.is_empty() || state.jobs.iter().any(GenJob::active) || state.rewrite.is_some() || state.wrapping.is_some() || state.worker.as_ref().is_some_and(|worker| !worker.held.is_empty()));
        if wanted != Some(true) {
            return;
        }
        // The FLOW studio's catalogue and LAN probe, refreshed every 2 s.
        let now = Cx::time_now();
        let stale = peek(|state| now - state.presets_at > 2.0).unwrap_or(true);
        if stale {
            self.flow_studio.api_start(cx);
            if self.flow_studio.api_ready() {
                let presets = video_presets(&self.flow_studio.presets_info());
                with(cx, |state| {
                    state.presets = presets;
                    state.presets_at = now;
                });
            }
        }
        let ready = self.flow_studio.api_ready();
        let mut done = Vec::new();
        with(cx, |state| {
            if let Some(worker) = &mut state.worker {
                worker.flush();
                while let Ok(reply) = worker.done.try_recv() {
                    done.push(reply);
                }
            }
        });
        for reply in done {
            self.generate_reply(cx, reply);
        }
        if ready {
            while let Some(intent) = with(cx, |state| state.intents.pop_front()) {
                match intent {
                    Intent::Generate { node, count, preview, reply } => {
                        let result = self.start_generate(cx, &node, count, preview);
                        if let Some(id) = reply {
                            with(cx, |state| state.replies.insert(id, result.clone().map(|job| json::obj(vec![("job", Json::Int(job as i64)), ("node", json::s(node.clone()))]))));
                        }
                        if let Err(error) = result {
                            self.video_cut.status = error;
                            self.video_cut.repaint = true;
                        }
                    }
                    Intent::Cancel { node } => self.cancel_generate(cx, &node),
                }
            }
        }
        self.follow_flow_jobs(cx);
        let generating: Vec<u64> = peek(|state| state.jobs.iter().filter(|job| job.phase == JobPhase_::Generating).map(|job| job.id).collect()).unwrap_or_default();
        for id in generating {
            self.launch_chunks(cx, id);
        }
        self.write_ready(cx);
        // Every change of a job's state goes to the log (hidden runs and
        // the AI read it there).
        with(cx, |state| {
            for job in &mut state.jobs {
                let takes = job.takes.iter().map(|take| format!("seed {} {:?}{}", take.seed, take.phase, take.error.as_ref().map_or(String::new(), |error| format!(" ({error})")))).collect::<Vec<_>>().join(", ");
                let line = format!("{} [{takes}]", job.status);
                if line != job.logged {
                    log!("generate {}: {line}", job.node);
                    job.logged = line;
                }
            }
        });
    }

    fn cancel_generate(&mut self, cx: &mut Cx, node: &str) {
        let flows = with(cx, |state| {
            let Some(job) = state.jobs.iter_mut().find(|job| job.node == node && job.active()) else { return Vec::new() };
            job.phase = JobPhase_::Cancelled;
            job.status = "Cancelled; takes already in the library stay there".into();
            job.takes.iter().flat_map(|take| take.chunks.iter()).filter_map(|chunk| match chunk.phase {
                ChunkPhase::Running(flow) => Some(flow),
                _ => None,
            }).collect()
        });
        for flow in flows {
            self.flow_studio.cancel_job(flow);
        }
        self.video_cut.status = format!("Stopped generating {node}");
        self.video_cut.repaint = true;
    }

    fn start_generate(&mut self, cx: &mut Cx, node: &str, count: usize, preview: bool) -> Result<u64, String> {
        let page = &self.video_cut;
        let session = page.session.as_ref().ok_or("open a cut first")?;
        let gen = session.doc.generate(node).ok_or_else(|| format!("the cut has no generate `{node}`"))?.clone();
        if peek(|state| state.running(node).is_some()) == Some(true) {
            return Err(format!("`{node}` is generating already"));
        }
        let preset = with(cx, |state| state.presets.iter().find(|preset| preset.key == gen.preset).cloned());
        let preset = match preset {
            Some(preset) => preset,
            None => {
                let known = self.flow_studio.presets_info().iter().any(|info| info.key == gen.preset);
                return Err(if known {
                    format!("`{}` is not an image/video-to-video preset: its generators are not all video models", gen.preset)
                } else {
                    format!("there is no FLOW preset `{}`", gen.preset)
                });
            }
        };
        if !preset.runnable && !preview {
            return Err(format!("no LAN AI Hub node has the video model `{}` needs installed; nothing was sent", preset.label));
        }
        let output = (session.doc.output.width, session.doc.output.height);
        let size = model_size(&gen, &preset, output);
        let frames = gen.number("frames").or_else(|| preset.param_number("frames")).filter(|frames| *frames >= 8.0).unwrap_or(DEFAULT_FRAMES);
        // Image-to-video continues each chunk from the last frame of the one
        // before: they follow each other. Video-to-video chunks overlap and
        // are blended.
        let parts = if gen.frame_at.is_some() { chunks_in_sequence(gen.span.length, frames / MODEL_FPS) } else { chunks(gen.span.length, frames / MODEL_FPS, OVERLAP) };
        let cut_fps = session.doc.output.fps;
        let times: Vec<f64> = match gen.frame_at {
            Some(at) => vec![at],
            None => parts.iter().flat_map(|chunk| {
                let first = gen.span.start + chunk.start;
                let last = (gen.span.start + chunk.end() - 1.0 / cut_fps).max(first);
                if preset.last_frame.is_some() { vec![first, last] } else { vec![first] }
            }).collect(),
        };
        let edit = session.head.map_or_else(|| "draft".to_string(), |head| head.asset_id.to_string());
        let doc = session.doc.clone();
        let tree = session.tree.clone().map(|tree| (tree, session.path.clone()));
        let seeds = next_seeds(&gen, count.clamp(1, TILES));
        let id = with(cx, |state| {
            let id = state.next;
            state.next += 1;
            let dir = job_dir(&edit, id);
            let takes = seeds.iter().map(|seed| TakeJob {
                seed: *seed,
                chunks: parts.iter().map(|chunk| ChunkJob { chunk: *chunk, first: None, last: None, phase: ChunkPhase::Waiting, permille: 0, video: None, retry_at: 0.0 }).collect(),
                phase: TakePhase::Chunks, model: String::new(), hubs: Vec::new(), result: None, error: None,
            }).collect();
            state.jobs.push(GenJob {
                id, node: node.to_string(), gen: gen.clone(), preset: preset.clone(), preview, size, frames, cut_fps,
                dir: dir.clone(), phase: JobPhase_::Keyframes, input_digest: None, keyframes: Vec::new(), takes,
                status: format!("Grabbing the input of {node} at {}×{}…", size.0, size.1), patch: None, patch_asked: false, logged: String::new(),
            });
            state.send(WJob::Keyframes { job: id, doc, tree, node: node.to_string(), size, times, dir });
            id
        });
        let page = &mut self.video_cut;
        page.status = format!("Generating {} take{} of {node} on {}", seeds.len(), if seeds.len() == 1 { "" } else { "s" }, if preset.nodes.is_empty() { "the preview generator".to_string() } else { preset.nodes.join(", ") });
        page.repaint = true;
        Ok(id)
    }

    fn generate_reply(&mut self, cx: &mut Cx, reply: WDone) {
        match reply {
            WDone::Keyframes { job, result } => {
                let started = with(cx, |state| {
                    let job = state.job(job)?;
                    if job.phase != JobPhase_::Keyframes {
                        return None;
                    }
                    match result {
                        Ok((digest, keyframes)) => {
                            job.input_digest = Some(digest);
                            job.keyframes = keyframes;
                            job.phase = JobPhase_::Generating;
                            job.status = "Generating on the LAN…".into();
                            Some(job.id)
                        }
                        Err(error) => {
                            job.status = format!("Could not grab the input: {error}");
                            job.phase = JobPhase_::Failed(error);
                            None
                        }
                    }
                });
                if let Some(id) = started {
                    self.launch_chunks(cx, id);
                }
            }
            WDone::LastFrame { job, take, chunk, result } => {
                let launch = with(cx, |state| {
                    let job = state.job(job)?;
                    let next = job.takes.get_mut(take)?.chunks.get_mut(chunk + 1)?;
                    match result {
                        Ok(frame) => {
                            next.first = Some(frame);
                            Some(job.id)
                        }
                        Err(error) => {
                            let take = &mut job.takes[take];
                            take.phase = TakePhase::Failed;
                            take.error = Some(format!("the last frame of chunk {chunk}: {error}"));
                            None
                        }
                    }
                });
                if let Some(id) = launch {
                    self.launch_chunks(cx, id);
                }
            }
            WDone::Finished { job, take, result } => {
                with(cx, |state| {
                    let Some(job) = state.job(job) else { return };
                    let key_input = job.input_digest.unwrap_or_default();
                    let gen = job.gen.clone();
                    let Some(entry) = job.takes.get_mut(take) else { return };
                    match result {
                        Ok(video) => {
                            let key = format!("{KEY_VERSION}-{key_input:016x}{:016x}", settings_digest(&gen, entry.seed, &entry.model));
                            entry.result = Some(GenTake { video: asset_ref(&video), seed: entry.seed, key: Some(key), seconds: Some(gen.span.length), model: entry.model.clone() });
                            entry.phase = TakePhase::Done;
                        }
                        Err(error) => {
                            entry.error = Some(error);
                            entry.phase = TakePhase::Failed;
                        }
                    }
                });
            }
            WDone::Patched { job, source, result } => {
                with(cx, |state| {
                    let Some(entry) = state.job(job) else { return };
                    entry.patch_asked = false;
                    match result {
                        Ok(written) => entry.patch = Some((source, written)),
                        Err(error) => {
                            entry.status = format!("The takes are in the library but could not be written into the cut: {error}");
                            entry.phase = JobPhase_::Failed(error);
                        }
                    }
                });
            }
            WDone::Wrapped { source, reply, result } => {
                let page = &mut self.video_cut;
                with(cx, |state| {
                    state.wrapping = None;
                    if let Some(reply) = reply {
                        state.replies.insert(reply, result.clone().map(|(_, id)| json::obj(vec![("node", json::s(id))])));
                    }
                });
                match result {
                    Ok((written, id)) => with(cx, |state| state.rewrite = Some((source, written, id))),
                    Err(error) => {
                        page.status = format!("Cannot generate here: {error}");
                        page.repaint = true;
                    }
                }
            }
        }
    }

    /// Start every chunk whose keyframe is known and whose turn it is.
    fn launch_chunks(&mut self, cx: &mut Cx, id: u64) {
        // One job is loaded at a time: FLOW answers a load of a preset that
        // a newer load of the same preset overtakes with "superseded", so
        // the next chunk starts once the last one has left preparation (the
        // pump calls this again).
        let running: Vec<u64> = peek(|state| state.jobs.iter().flat_map(|job| job.takes.iter().flat_map(|take| take.chunks.iter())).filter_map(|chunk| match chunk.phase {
            ChunkPhase::Running(flow) => Some(flow),
            _ => None,
        }).collect()).unwrap_or_default();
        if running.iter().any(|flow| self.flow_studio.job(*flow).is_some_and(|job| job.phase == JobPhase::Preparing)) {
            return;
        }
        // No more at once than the LAN has nodes for the model: more would
        // only queue on the nodes and be refused for VRAM.
        let nodes = peek(|state| state.job(id).map_or(1, |job| job.preset.nodes.len().max(1))).unwrap_or(1);
        if running.len() >= nodes {
            return;
        }
        let now = Cx::time_now();
        let mut starts = Vec::new();
        with(cx, |state| {
            let Some(job) = state.job(id) else { return };
            if job.phase != JobPhase_::Generating {
                return;
            }
            let last_frames = job.preset.last_frame.is_some();
            let i2v = job.gen.frame_at.is_some();
            for (t, take) in job.takes.iter_mut().enumerate() {
                if take.phase != TakePhase::Chunks {
                    continue;
                }
                for c in 0..take.chunks.len() {
                    if take.chunks[c].phase != ChunkPhase::Waiting || take.chunks[c].retry_at > now {
                        continue;
                    }
                    // Keyframes: one (image-to-video's first chunk), or a
                    // first (and last) per chunk of a video-to-video.
                    if take.chunks[c].first.is_none() {
                        take.chunks[c].first = match (i2v, c) {
                            (true, 0) => job.keyframes.first().copied(),
                            (true, _) => None,
                            (false, _) => job.keyframes.get(if last_frames { c * 2 } else { c }).copied(),
                        };
                        if !i2v && last_frames {
                            take.chunks[c].last = job.keyframes.get(c * 2 + 1).copied();
                        }
                    }
                    if take.chunks[c].first.is_some() {
                        starts.push((t, c));
                    }
                }
            }
        });
        for (t, c) in starts.into_iter().take(1) {
            let request = with(cx, |state| {
                let job = state.job(id)?;
                let take = &job.takes[t];
                let chunk = &take.chunks[c];
                let preset = &job.preset;
                let mut request = RunRequest::new(&format!("{ORIGIN} · {}", job.node));
                request.preview = job.preview;
                request.tags = vec!["generate".into(), "chunk".into()];
                request = request.media(&preset.image, result_ref(chunk.first?));
                if let (Some(input), Some(last)) = (&preset.last_frame, chunk.last) {
                    request = request.media(input, result_ref(last));
                }
                if let Some(input) = &preset.prompt {
                    request = request.text(input, &job.gen.prompt);
                }
                if preset.has_param("width") && preset.has_param("height") {
                    request = request.size(job.size.0, job.size.1);
                }
                if preset.has_param("seed") {
                    request = request.seed(take.seed);
                }
                let frames = ((chunk.chunk.length * MODEL_FPS).ceil() + 1.0).min(job.frames).max(8.0);
                if preset.has_param("frames") {
                    request = request.param(None, "frames", Literal::Num(frames));
                }
                if !job.gen.negative.is_empty() && preset.has_param("negative") {
                    request = request.param(None, "negative", Literal::Str(job.gen.negative.clone()));
                }
                for (key, value) in &job.gen.params {
                    if !matches!(key.as_str(), "width" | "height" | "frames") {
                        request = request.param(None, key, value.clone());
                    }
                }
                Some((preset.key.clone(), request))
            });
            let Some((key, request)) = request else { continue };
            let started = self.flow_studio.run_preset(cx, &key, request);
            with(cx, |state| {
                let Some(job) = state.job(id) else { return };
                let take = &mut job.takes[t];
                match started {
                    Ok(flow) => {
                        log!("generate {}: take seed {} chunk {c} is FLOW job {flow}", job.node, take.seed);
                        take.chunks[c].phase = ChunkPhase::Running(flow);
                    }
                    Err(error) => {
                        take.chunks[c].phase = ChunkPhase::Failed;
                        take.phase = TakePhase::Failed;
                        take.error = Some(error);
                    }
                }
            });
        }
    }

    /// Read every running FLOW job's news; finish takes whose chunks are in.
    fn follow_flow_jobs(&mut self, cx: &mut Cx) {
        let running: Vec<(u64, usize, usize, u64)> = peek(|state| {
            state.jobs.iter().filter(|job| job.phase == JobPhase_::Generating).flat_map(|job| {
                job.takes.iter().enumerate().flat_map(move |(t, take)| take.chunks.iter().enumerate().filter_map(move |(c, chunk)| match chunk.phase {
                    ChunkPhase::Running(flow) => Some((job.id, t, c, flow)),
                    _ => None,
                }))
            }).collect()
        }).unwrap_or_default();
        let mut relaunch = Vec::new();
        for (id, t, c, flow) in running {
            let events = self.flow_studio.take_job_events(flow);
            if events.is_empty() {
                continue;
            }
            let chained = with(cx, |state| {
                let job = state.job(id)?;
                let (i2v, size, dir) = (job.gen.frame_at.is_some(), job.size, job.dir.clone());
                let take = &mut job.takes[t];
                let mut chained = None;
                for event in events {
                    match event {
                        JobEvent::Node { permille: Some(permille), .. } => take.chunks[c].permille = take.chunks[c].permille.max(permille),
                        JobEvent::Ran { model, hub, .. } => {
                            if take.model.is_empty() {
                                take.model = model;
                            }
                            let hub = if hub.is_empty() { "preview".to_string() } else { hub };
                            if !take.hubs.contains(&hub) {
                                take.hubs.push(hub);
                            }
                        }
                        JobEvent::Published { asset, kind, .. } if kind.eq_ignore_ascii_case("video") => take.chunks[c].video = asset_revision(&asset),
                        JobEvent::Finished { phase, error, .. } => {
                            let video = take.chunks[c].video;
                            match (phase, video) {
                                (JobPhase::Done, Some(video)) => {
                                    take.chunks[c].phase = ChunkPhase::Done;
                                    take.chunks[c].permille = 1000;
                                    // Image-to-video continues from this chunk's end.
                                    if i2v && c + 1 < take.chunks.len() {
                                        chained = Some(WJob::LastFrame { job: id, take: t, chunk: c, video, size, dir: dir.clone() });
                                    }
                                }
                                // Overtaken while loading (see launch_chunks): again.
                                (JobPhase::Failed, _) if error.as_deref().is_some_and(|error| error.contains("superseded")) => {
                                    log!("generate: chunk {c} of seed {} was superseded while loading; starting it again", take.seed);
                                    take.chunks[c].phase = ChunkPhase::Waiting;
                                    take.chunks[c].permille = 0;
                                }
                                // The fleet is busy now (no VRAM free, a node in local use):
                                // wait and ask again, as long as it takes.
                                (JobPhase::Failed, _) if error.as_deref().is_some_and(|error| error.contains("admission refused") || error.contains("retry after")) => {
                                    log!("generate: chunk {c} of seed {} was turned away by the fleet ({}); again in {RETRY} s", take.seed, error.as_deref().unwrap_or(""));
                                    take.chunks[c].phase = ChunkPhase::Waiting;
                                    take.chunks[c].permille = 0;
                                    take.chunks[c].retry_at = Cx::time_now() + RETRY;
                                }
                                (phase, _) => {
                                    take.chunks[c].phase = ChunkPhase::Failed;
                                    take.phase = TakePhase::Failed;
                                    take.error = Some(error.unwrap_or_else(|| match phase {
                                        JobPhase::Cancelled => "cancelled".into(),
                                        _ => "the job published no video".into(),
                                    }));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                chained
            });
            if let Some(job) = chained {
                with(cx, |state| state.send(job));
                relaunch.push(id);
            }
        }
        // Takes whose chunks are all in: blend and publish.
        with(cx, |state| {
            let mut sends = Vec::new();
            for job in state.jobs.iter_mut().filter(|job| job.phase == JobPhase_::Generating) {
                for (t, take) in job.takes.iter_mut().enumerate() {
                    if take.phase != TakePhase::Chunks || !take.chunks.iter().all(|chunk| chunk.phase == ChunkPhase::Done) {
                        continue;
                    }
                    take.phase = TakePhase::Finishing;
                    let key = format!("{KEY_VERSION}-{:016x}{:016x}", job.input_digest.unwrap_or_default(), settings_digest(&job.gen, take.seed, &take.model));
                    sends.push(WJob::Finish { job: job.id, take: t, finish: Finish {
                        parts: take.chunks.iter().filter_map(|chunk| Some((chunk.video?, chunk.chunk))).collect(),
                        keyframes: job.keyframes.clone(),
                        length: job.gen.span.length,
                        size: job.size,
                        fps: job.cut_fps,
                        fit: Fit::Fill,
                        dir: job.dir.clone(),
                        title: format!("{} · take seed {}", job.node, take.seed),
                        prompt: job.gen.prompt.clone(),
                        model: take.model.clone(),
                        key,
                        preset: job.preset.key.clone(),
                        seed: take.seed,
                    } });
                }
                // Every take ended: write the ones that made it.
                if job.takes.iter().all(|take| matches!(take.phase, TakePhase::Done | TakePhase::Failed)) {
                    let made = job.takes.iter().filter(|take| take.result.is_some()).count();
                    if made == 0 {
                        let error = job.takes.iter().find_map(|take| take.error.clone()).unwrap_or_else(|| "no take was made".into());
                        job.status = format!("Generation failed: {error}");
                        job.phase = JobPhase_::Failed(error);
                    } else {
                        job.phase = JobPhase_::Patching;
                        job.status = format!("{made} take{} made; writing them into the cut…", if made == 1 { "" } else { "s" });
                    }
                }
            }
            for job in sends {
                state.send(job);
            }
        });
        for id in relaunch {
            self.launch_chunks(cx, id);
        }
    }

    /// Patches and wraps land only while the page has nothing of its own in
    /// flight, and only on the source they were computed on; otherwise they
    /// are computed again on the new source.
    fn write_ready(&mut self, cx: &mut Cx) {
        let page = &mut self.video_cut;
        let idle = page.queue.is_empty() && page.in_flight.is_none();
        let Some(session) = page.session.as_ref() else { return };
        let source = session.source.clone();
        let doc = session.doc.clone();
        let tree = session.tree.clone().map(|tree| (tree, session.path.clone()));
        // Takes to write.
        let mut asks = Vec::new();
        let mut land = None;
        with(cx, |state| {
            for job in state.jobs.iter_mut().filter(|job| job.phase == JobPhase_::Patching) {
                match &job.patch {
                    Some((from, to)) if *from == source && idle && land.is_none() => {
                        land = Some((job.id, to.clone()));
                    }
                    Some((from, _)) if *from != source => job.patch = None,
                    _ => {}
                }
                if job.patch.is_none() && !job.patch_asked {
                    let Some(current) = doc.generate(&job.node) else {
                        job.status = format!("`{}` is gone from the cut; its takes stay in the library", job.node);
                        job.phase = JobPhase_::Failed("the node is gone".into());
                        continue;
                    };
                    let mut takes = current.takes.clone();
                    let first = takes.len();
                    takes.extend(job.takes.iter().filter_map(|take| take.result.clone()));
                    job.patch_asked = true;
                    asks.push(WJob::Patch { job: job.id, source: source.clone(), tree: tree.clone(), node: job.node.clone(), takes, take: first });
                }
            }
            for ask in asks {
                state.send(ask);
            }
        });
        if let Some((id, written)) = land {
            page.op(CutOp::Source { text: written });
            with(cx, |state| {
                if let Some(job) = state.job(id) {
                    let made = job.takes.iter().filter(|take| take.result.is_some()).count();
                    let failed = job.takes.iter().filter_map(|take| take.error.as_ref()).next();
                    job.status = match failed {
                        Some(error) => format!("{made} take{} made; others failed: {error}", if made == 1 { "" } else { "s" }),
                        None => format!("{made} take{} made", if made == 1 { "" } else { "s" }),
                    };
                    job.phase = JobPhase_::Done;
                }
            });
            page.status = "Generated takes are in the cut (cmd-Z takes them out)".into();
            page.repaint = true;
            return;
        }
        // A new Generate over a clip or range.
        let rewrite = with(cx, |state| state.rewrite.clone());
        if let Some((from, to, id)) = rewrite {
            if from != source {
                with(cx, |state| state.rewrite = None);
                page.status = "The cut changed while the generate was being added; try again".into();
            } else if idle {
                with(cx, |state| state.rewrite = None);
                page.op(CutOp::Source { text: to });
                page.status = format!("Added {id}: set its prompt, then Generate");
                page.repaint = true;
            }
        }
    }
}

// ------------------------------------------------------------------ page

/// The Generate under the selection: one wrapping the selected block's
/// chain, else one over the whole picture at the playhead.
fn generate_for<'d>(doc: &'d CutDoc, selected: Option<&str>, playhead: f64) -> Option<&'d GenerateNode> {
    if let Some(id) = selected {
        let id = super::split_selection(id).0;
        if let Some(gen) = doc.generate(id) {
            return Some(gen);
        }
        let top = chain_top(doc, id);
        if let Some(gen) = doc.generates.iter().find(|gen| gen.input.as_ref().is_some_and(|wire| wire.node == top)) {
            return Some(gen);
        }
    }
    let picture = match doc.node(&doc.output_node).map(|node| &node.kind) {
        Some(CutNodeKind::Output { frame: Some(frame), .. }) => frame.node.clone(),
        _ => return None,
    };
    // The output reads a generate that wraps the stack (a range).
    let mut current = doc.generate(&picture);
    while let Some(gen) = current {
        if gen.active(playhead) {
            return Some(gen);
        }
        current = gen.input.as_ref().and_then(|wire| doc.generate(&wire.node));
    }
    None
}

impl VideoCut {
    /// The render bar's generative segments (rendered by the same system as
    /// the rest of the cut): every Generate's span and state.
    pub(super) fn generate_segments(&self) -> Vec<Segment> {
        let Some(session) = &self.session else { return Vec::new() };
        let doc = &session.doc;
        peek(|state| {
            doc.generates.iter().map(|gen| {
                let running = state.running(&gen.id).map(GenJob::progress);
                let own = state.states.get(&gen.id).cloned().filter(|_| state.states_rev == Some(session.revision));
                let state = match (running, own) {
                    (Some(progress), _) => SegmentState::Generating(progress),
                    (None, Some(GenState::Generated)) => SegmentState::Generated,
                    (None, Some(other)) => SegmentState::NeedsGeneration(other),
                    (None, None) => SegmentState::NeedsGeneration(GenState::Missing),
                };
                Segment { node: gen.id.clone(), start: gen.span.start, end: gen.span.end(), state, takes: gen.takes.len(), chosen: gen.chosen().map(|_| gen.take) }
            }).collect()
        }).unwrap_or_default()
    }

    /// The viewer's badge at edit time `t` (a segment
    /// without a current take shows its input, badged): `(node, text)`.
    fn generate_badge(&self, t: f64) -> Option<(String, String)> {
        self.generate_segments().into_iter().find(|segment| segment.start <= t && t < segment.end).and_then(|segment| match segment.state {
            SegmentState::Generated => None,
            SegmentState::Generating(progress) => Some((segment.node, format!("GENERATING {:.0}%", progress * 100.0))),
            SegmentState::NeedsGeneration(GenState::Missing) => Some((segment.node, "INPUT · NEEDS GENERATION".into())),
            SegmentState::NeedsGeneration(_) => Some((segment.node, "OLD TAKE · NEEDS GENERATION".into())),
        })
    }

    /// The Generate node whose job is running, for the chat's Stop chip.
    pub(super) fn generating_node(&self) -> Option<String> {
        peek(|state| state.jobs.iter().find(|job| job.active()).map(|job| job.node.clone())).flatten()
    }

    /// The card ⌃R / ⇧⌃R and the chat's "Generate over selection" open:
    /// `generate_run` over the Generate in range that needs takes (the
    /// selected one first), else `generate_add` over the selected clip or
    /// the in–out range. `all`: the whole cut.
    pub(super) fn generate_card(&self, all: bool) -> Option<(&'static str, Json)> {
        let session = self.session.as_ref()?;
        let doc = &session.doc;
        let run = |node: &str| Some(("generate_run", json::obj(vec![("node", json::s(node)), ("takes", Json::Int(1))])));
        if !all {
            if let Some(gen) = generate_for(doc, self.selected.as_deref(), self.playhead) {
                return run(&gen.id);
            }
        }
        let range = if all {
            (0.0, doc.duration)
        } else if let (Some(a), Some(b)) = self.marks {
            (a.min(b), a.max(b))
        } else {
            (self.playhead, self.playhead + 1.0 / self.fps())
        };
        if let Some(segment) = self.generate_segments().into_iter().find(|segment| matches!(segment.state, SegmentState::NeedsGeneration(_)) && segment.start < range.1 && range.0 < segment.end) {
            return run(&segment.node);
        }
        if all {
            return None;
        }
        let clip = self.selected.as_deref().map(|id| super::split_selection(id).0.to_string())
            .filter(|id| matches!(doc.node(id).map(|node| &node.kind), Some(CutNodeKind::Clip { .. } | CutNodeKind::Still { .. } | CutNodeKind::Title { .. })));
        match (clip, self.marks) {
            (Some(clip), _) => Some(("generate_add", json::obj(vec![("clip", json::s(clip))]))),
            (None, (Some(a), Some(b))) if b > a => Some(("generate_add", json::obj(vec![("start", Json::F64(a)), ("length", Json::F64(b - a))]))),
            _ => None,
        }
    }

    /// Ask for `count` takes of Generate `node` (the section's button, the
    /// render bar's Render Selection / Render All, the AI's tool).
    pub(super) fn request_generate(&mut self, cx: &mut Cx, node: &str, count: usize, preview: bool) {
        with(cx, |state| state.intents.push_back(Intent::Generate { node: node.to_string(), count, preview, reply: None }));
        self.status = format!("Starting {node} on the LAN…");
        self.repaint = true;
    }

    /// Generate every segment that needs it and overlaps `[t0, t1)` (Render
    /// Selection / Render All).
    pub(super) fn generate_range(&mut self, cx: &mut Cx, t0: f64, t1: f64, count: usize) -> usize {
        let wanted: Vec<String> = self.generate_segments().into_iter()
            .filter(|segment| matches!(segment.state, SegmentState::NeedsGeneration(_)) && segment.start < t1 && t0 < segment.end)
            .map(|segment| segment.node).collect();
        for node in &wanted {
            self.request_generate(cx, node, count, false);
        }
        wanted.len()
    }

    fn pick_take(&mut self, node: &str, take: usize) {
        self.op(CutOp::Param { node: node.to_string(), key: "take".into(), value: Value::num(take as f64) });
    }
}

/// A message cut to one or two lines of the section.
fn short(text: &str) -> String {
    const MAX: usize = 120;
    match text.char_indices().nth(MAX) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// The GENERATE section: filled from the page on every pump that changes
/// what it shows.
pub(super) fn update(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let section = ui.widget(cx, ids!(cut_s_generate));
    if section.is_empty() {
        return;
    }
    let Some(session) = &page.session else {
        section.set_visible(cx, false);
        return;
    };
    let doc = session.doc.clone();
    let revision = session.revision;
    // States once per document revision.
    let stale_states = peek(|state| state.states_rev != Some(revision)).unwrap_or(!doc.generates.is_empty());
    if stale_states && !doc.generates.is_empty() {
        let eval = CutEval::new(&doc);
        let states: HashMap<String, GenState> = doc.generates.iter().map(|gen| (gen.id.clone(), generate_state(&eval, gen))).collect();
        with(cx, |state| {
            state.states = states;
            state.states_rev = Some(revision);
        });
    }
    let selected = page.selected.clone();
    let gen = generate_for(&doc, selected.as_deref(), page.playhead).cloned();
    let clip = selected.as_deref().map(|id| super::split_selection(id).0.to_string())
        .filter(|id| matches!(doc.node(id).map(|node| &node.kind), Some(CutNodeKind::Clip { .. } | CutNodeKind::Still { .. } | CutNodeKind::Title { .. })));
    let range = matches!(page.marks, (Some(a), Some(b)) if b > a);
    let visible = gen.is_some() || clip.is_some() || range;
    section.set_visible(cx, visible);
    with(cx, |state| state.showing = visible);
    if !visible {
        return;
    }
    let segment = gen.as_ref().and_then(|gen| page.generate_segments().into_iter().find(|segment| segment.node == gen.id));
    let (presets, job) = with(cx, |state| {
        let job = gen.as_ref().and_then(|gen| state.jobs.iter().rev().find(|job| job.node == gen.id)).map(|job| {
            let takes = job.takes.iter().map(|take| {
                let hubs = if take.hubs.is_empty() { String::new() } else { format!(" on {}", take.hubs.join(", ")) };
                match take.phase {
                    TakePhase::Chunks => format!("seed {} {:.0}%{hubs}", take.seed, take.progress() * 100.0),
                    TakePhase::Finishing => format!("seed {} blending{hubs}", take.seed),
                    TakePhase::Done => format!("seed {} done{hubs}", take.seed),
                    TakePhase::Failed => format!("seed {} failed: {}", take.seed, short(take.error.as_deref().unwrap_or("?"))),
                }
            }).collect::<Vec<_>>();
            (job.active(), short(&job.status), takes, job.progress())
        });
        (state.presets.clone(), job)
    });
    let thumbs = gen.as_ref().map_or(0, |gen| gen.takes.iter().filter(|take| page.lib.thumbs.contains_key(&take.video.id)).count());
    let shown = format!("{revision}|{selected:?}|{:?}|{:?}|{}|{}|{thumbs}|{range}", segment, job, presets.len(), gen.as_ref().map_or(0, |gen| gen.takes.len()));
    let changed = with(cx, |state| std::mem::replace(&mut state.shown, shown.clone()) != shown);
    if !changed {
        return;
    }
    let (badge, state_text) = match (&segment, &gen) {
        (Some(segment), Some(gen)) => match &segment.state {
            SegmentState::Generating(progress) => (format!("{:.0}%", progress * 100.0), format!("Generating {} · {:.0}%", gen.id, progress * 100.0)),
            SegmentState::Generated => ("GENERATED".into(), format!("{} · take {} of {}", gen.id, gen.take + 1, gen.takes.len())),
            SegmentState::NeedsGeneration(state) => {
                let why = match state {
                    GenState::Missing => "no take yet: plays its input".to_string(),
                    GenState::Stale(why) => format!("{} changed: the take plays until you update", why.iter().map(|why| match why {
                        Staleness::Input => "the input",
                        Staleness::Settings => "the settings",
                        Staleness::Seed => "the seed",
                        Staleness::Unkeyed => "an unkeyed take",
                    }).collect::<Vec<_>>().join(", ")),
                    GenState::Generated => String::new(),
                };
                ("NEEDS GENERATION".into(), format!("{} · {why}", gen.id))
            }
        },
        _ if clip.is_some() => (String::new(), "Put this clip through a generation flow".into()),
        _ => (String::new(), "Put the in–out range through a generation flow".into()),
    };
    ui.label(cx, ids!(gs_badge_text)).set_text(cx, &badge);
    ui.widget(cx, ids!(gs_badge)).set_visible(cx, !badge.is_empty());
    ui.label(cx, ids!(gs_state)).set_text(cx, &state_text);
    let runnable: Vec<&VideoPreset> = presets.iter().filter(|preset| preset.runnable).collect();
    let note = match &job {
        Some((_, status, takes, _)) if !takes.is_empty() => format!("{status}\n{}", takes.join("\n")),
        Some((_, status, _, _)) => status.clone(),
        None if presets.is_empty() => "Looking for AI Hub nodes on the LAN…".into(),
        None if runnable.is_empty() => "No LAN node has a video model installed; nothing will be sent".into(),
        None => format!("Video models on {}", runnable.iter().flat_map(|preset| preset.nodes.iter()).fold(Vec::<&String>::new(), |mut all, node| {
            if !all.contains(&node) { all.push(node) }
            all
        }).into_iter().map(String::as_str).collect::<Vec<_>>().join(", ")),
    };
    ui.label(cx, ids!(gs_note)).set_text(cx, &note);
    // The take switcher.
    let takes = gen.as_ref().map(|gen| gen.takes.clone()).unwrap_or_default();
    let mut want_thumbs = Vec::new();
    for (index, tile) in TILE_IDS.iter().enumerate() {
        let tile_ref = ui.widget(cx, &[*tile]);
        let Some(take) = takes.get(index) else {
            tile_ref.set_visible(cx, false);
            continue;
        };
        tile_ref.set_visible(cx, true);
        let chosen = gen.as_ref().is_some_and(|gen| gen.take == index);
        ui.button(cx, &[*tile, live_id!(pick)]).set_text(cx, &format!("{}{} · seed {}", if chosen { "● " } else { "" }, index + 1, take.seed));
        match page.lib.thumbs.get(&take.video.id) {
            Some((_, texture)) => ui.image(cx, &[*tile, live_id!(poster)]).set_texture(cx, Some(texture.clone())),
            None => {
                if let Ok(reference) = exact(&take.video) {
                    if page.requested.insert(take.video.id.clone()) {
                        want_thumbs.push(reference);
                    }
                }
            }
        }
    }
    if !want_thumbs.is_empty() {
        page.send(StoreJob::Thumbs { refs: want_thumbs });
    }
}

/// The section's controls.
pub(super) fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    let Some(session) = &page.session else { return };
    let doc = session.doc.clone();
    let gen = generate_for(&doc, page.selected.as_deref(), page.playhead).cloned();
    let Some(gen) = gen else { return };
    for (index, tile) in TILE_IDS.iter().enumerate() {
        if ui.button(cx, &[*tile, live_id!(pick)]).clicked(actions) && index < gen.takes.len() && index != gen.take {
            page.pick_take(&gen.id, index);
        }
    }
}

// ------------------------------------------------------------------ AI tools

/// The control API's generative-segment tools (its "Assets / generation"
/// family), in A1's registry format. Stage registers them
/// with `Registry::add_local`, handled by [`call_tool`].
/// Run tool `name` with `args` (the host's handler for the catalogue's generate tools).
/// `generate_run` answers at once with the job id: the job runs on the
/// pump like the section's button.
fn call_tool(app: &mut StoryApp, cx: &mut Cx, name: &str, args: &Json) -> Result<Json, String> {
    let text = |key: &str| args.get(key).and_then(Json::as_str).map(str::to_string);
    let number = |key: &str| args.get(key).and_then(|value| match value {
        Json::Int(value) => Some(*value as f64),
        Json::F64(value) => Some(*value),
        _ => None,
    });
    match name {
        "generate_status" => {
            let page = &app.video_cut;
            let session = page.session.as_ref().ok_or("open a cut first")?;
            let doc = session.doc.clone();
            let eval = CutEval::new(&doc);
            let segments = doc.generates.iter().map(|gen| {
                let state = generate_state(&eval, gen);
                let job = peek(|state| state.running(&gen.id).map(|job| (job.progress(), job.status.clone(), job.takes.iter().map(|take| take.progress()).collect::<Vec<_>>()))).flatten();
                json::obj(vec![
                    ("node", json::s(gen.id.clone())),
                    ("start", Json::F64(gen.span.start)),
                    ("length", Json::F64(gen.span.length)),
                    ("preset", json::s(gen.preset.clone())),
                    ("prompt", json::s(gen.prompt.clone())),
                    ("seed", Json::Int(gen.seed as i64)),
                    ("frame_at", gen.frame_at.map_or(Json::Null, Json::F64)),
                    ("state", json::s(match (&job, &state) {
                        (Some(_), _) => "generating",
                        (None, GenState::Generated) => "generated",
                        (None, _) => "needs generation",
                    })),
                    ("why", Json::Arr(match &state {
                        GenState::Stale(why) => why.iter().map(|why| json::s(format!("{why:?}").to_lowercase())).collect(),
                        GenState::Missing => vec![json::s("no take")],
                        GenState::Generated => Vec::new(),
                    })),
                    ("progress", job.as_ref().map_or(Json::Null, |job| Json::F64(job.0))),
                    ("status", job.as_ref().map_or(Json::Null, |job| json::s(job.1.clone()))),
                    ("take", gen.chosen().map_or(Json::Null, |_| Json::Int(gen.take as i64))),
                    ("takes", Json::Arr(gen.takes.iter().enumerate().map(|(index, take)| json::obj(vec![
                        ("index", Json::Int(index as i64)),
                        ("asset_id", json::s(take.video.id.clone())),
                        ("revision", json::s(take.video.rev.clone())),
                        ("seed", Json::Int(take.seed as i64)),
                        ("model", json::s(take.model.clone())),
                        ("key", take.key.clone().map_or(Json::Null, json::s)),
                    ])).collect())),
                ])
            }).collect();
            let presets = with(cx, |state| state.presets.clone()).into_iter().map(|preset| json::obj(vec![
                ("key", json::s(preset.key)),
                ("label", json::s(preset.label)),
                ("runnable", Json::Bool(preset.runnable)),
                ("nodes", Json::Arr(preset.nodes.into_iter().map(json::s).collect())),
                ("model", json::s(preset.model)),
                ("last_frame", Json::Bool(preset.last_frame.is_some())),
            ])).collect();
            // Keep the LAN probe going so the next look has presets.
            with(cx, |state| state.presets_at = state.presets_at.min(Cx::time_now() - 2.0));
            app.flow_studio.api_start(cx);
            if app.flow_studio.api_ready() {
                let presets = video_presets(&app.flow_studio.presets_info());
                with(cx, |state| state.presets = presets);
            }
            Ok(json::obj(vec![("segments", Json::Arr(segments)), ("presets", Json::Arr(presets))]))
        }
        "generate_run" => {
            let node = text("node").ok_or("`node` names the Generate")?;
            let count = number("takes").unwrap_or(1.0).clamp(1.0, TILES as f64) as usize;
            let preview = args.get("preview").and_then(Json::as_bool).unwrap_or(false);
            let page = &app.video_cut;
            let doc = page.session.as_ref().ok_or("open a cut first")?.doc.clone();
            doc.generate(&node).ok_or_else(|| format!("the cut has no Generate `{node}`; generate_status lists them"))?;
            app.flow_studio.api_start(cx);
            let reply = with(cx, |state| {
                let id = state.next_reply;
                state.next_reply += 1;
                state.intents.push_back(Intent::Generate { node: node.clone(), count, preview, reply: Some(id) });
                id
            });
            Ok(json::obj(vec![("node", json::s(node)), ("request", Json::Int(reply as i64)), ("takes", Json::Int(count as i64))]))
        }
        "generate_cancel" => {
            let node = text("node").ok_or("`node` names the Generate")?;
            if peek(|state| state.running(&node).is_some()) != Some(true) {
                return Err(format!("`{node}` is not generating"));
            }
            with(cx, |state| state.intents.push_back(Intent::Cancel { node }));
            Ok(json::obj(Vec::new()))
        }
        "generate_pick" => {
            let node = text("node").ok_or("`node` names the Generate")?;
            let take = number("take").ok_or("`take` is an index")? as usize;
            let page = &mut app.video_cut;
            let doc = page.session.as_ref().ok_or("open a cut first")?.doc.clone();
            let gen = doc.generate(&node).ok_or_else(|| format!("the cut has no Generate `{node}`"))?;
            if take >= gen.takes.len() {
                return Err(format!("`{node}` has {} takes", gen.takes.len()));
            }
            page.pick_take(&node, take);
            Ok(json::obj(Vec::new()))
        }
        "generate_add" => {
            let page = &mut app.video_cut;
            let doc = page.session.as_ref().ok_or("open a cut first")?.doc.clone();
            let target = match (text("clip"), number("start"), number("length")) {
                (Some(clip), _, _) => {
                    doc.node(&clip).ok_or_else(|| format!("the cut has no `{clip}`"))?;
                    WrapTarget::Clip(clip)
                }
                (None, Some(start), Some(length)) => WrapTarget::Range(start, length),
                _ => return Err("give `clip`, or `start` and `length`".into()),
            };
            let source = page.session.as_ref().map(|session| session.source.clone()).unwrap_or_default();
            let tree = page.session.as_ref().and_then(|session| session.tree.clone().map(|tree| (tree, session.path.clone())));
            let presets = with(cx, |state| state.presets.clone());
            let preset = text("preset").or_else(|| presets.iter().find(|preset| preset.runnable).map(|preset| preset.key.clone())).unwrap_or_else(|| "bundled:image-to-video".into());
            let wrap = Wrap { target, preset, prompt: text("prompt").unwrap_or_default(), seed: number("seed").unwrap_or(1.0) as u64, frame_at: number("frame_at") };
            // Written on the worker (it evaluates Splash); the answer names
            // the node once it is in the cut (`tool_reply`).
            let reply = with(cx, |state| {
                let id = state.next_reply;
                state.next_reply += 1;
                state.wrapping = Some(source.clone());
                state.send(WJob::Wrap { source, tree, wrap, reply: Some(id) });
                id
            });
            Ok(json::obj(vec![("request", Json::Int(reply as i64))]))
        }
        other => Err(format!("`{other}` is not a generate tool")),
    }
}

/// A finished `generate_run` or `generate_add` request's answer (the job it started, or why
/// not), once the pump has handled it.
fn tool_reply(request: u64) -> Option<Result<Json, String>> {
    peek(|state| state.replies.remove(&request)).flatten()
}

/// The generative segments' entry points for the other parts of Stage:
/// the render bar and the viewer (P1, L7) and the control API (A1).
impl StoryApp {
    /// Every Generate of the open cut as a render-bar segment.
    pub fn generate_segments(&self) -> Vec<Segment> {
        self.video_cut.generate_segments()
    }

    /// Render Selection / Render All: generate `count` takes of
    /// every segment in `[t0, t1)` that needs generation; how many.
    pub fn generate_range(&mut self, cx: &mut Cx, t0: f64, t1: f64, count: usize) -> usize {
        self.video_cut.generate_range(cx, t0, t1, count)
    }

    /// The viewer's badge at `t`: `(node, text)` while the segment there
    /// shows its input or an out-of-date take, or is generating.
    pub fn generate_badge(&self, t: f64) -> Option<(String, String)> {
        self.video_cut.generate_badge(t)
    }

    /// Handle one of the catalogue's generate tools. `generate_run` and `generate_add` answer
    /// with a `request` id; [`Self::generate_tool_reply`] has the result.
    pub fn generate_tool_call(&mut self, cx: &mut Cx, name: &str, args: &Json) -> Result<Json, String> {
        call_tool(self, cx, name, args)
    }

    pub fn generate_tool_reply(request: u64) -> Option<Result<Json, String>> {
        tool_reply(request)
    }
}

/// A library revision as the generation studio names stored results.
fn result_ref(asset: makepad_asset_data::AssetRevisionRef) -> makepad_flow_studio::ResultRef {
    makepad_flow_studio::ResultRef { id: asset.asset_id.to_string(), revision: Some(asset.revision.to_string()), alias: None }
}

/// The library revision a studio result names (`None` for one that names
/// no revision of this library).
fn asset_revision(result: &makepad_flow_studio::ResultRef) -> Option<makepad_asset_data::AssetRevisionRef> {
    Some(makepad_asset_data::AssetRevisionRef { asset_id: result.id.parse().ok()?, revision: result.revision.as_deref()?.parse().ok()? })
}
