//! The cut engine: everything a cut needs before and while its frames
//! are drawn. Loading resolves every reference on a worker (files, still
//! sizes, voice timings, transcripts, the sound the mixdown reads) and
//! builds every effect's geometry on the task pool; then
//! [`CutEngine::record`] turns `t` into a recorded frame once the exact
//! clip frames and stills it shows have arrived. The offline renderer and
//! the CUT page's preview are both drivers of it.
//!
//! Clip loads and stills are held only near `t` (from one
//! second before a span to its end); clip loads are admitted by activity,
//! the clips visible at `t` first, never by the order the cut lists them.

use makepad_story_cut::mix::AudioMix;
use makepad_motion::bind::run::RunData;
use makepad_story_cut::{Blend, DrawOp};
use crate::compose::{Compositor, ExternalFrame, Painted, SourcePicture};
use crate::frames::{ClipFrames, DecoderBudget, FrameState};
use crate::media::{MediaKind, MediaResolver, ResolvedMedia};
use makepad_asset_data::MediaType;
use makepad_stage_asset_stream::limits::MAX_READER_CONSUMERS;
use makepad_stage_asset_stream::VideoFramePayload;
use makepad_stage_audio_media::decode::{decode_audio_clip, open_container_audio};
use makepad_stage_audio_media::pcm::TrackPcm;
use makepad_story_cut::{AssetRef, Caption, DrawKind, CutDoc, CutEval, CutNode, CutNodeKind, CutTree, EffectRef, FramePlan, MediaRef, RunRef, SourceTime, SpokenLine, TakeInfo, Word};
use makepad_content_packs::effects::{self as packs, BuiltEffect, EffectPack, EffectView};
use makepad_widgets::makepad_zune_jpeg::JpegDecoder;
use makepad_widgets::makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_widgets::makepad_zune_png::PngDecoder;
use makepad_widgets::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// How long before its span a clip's reader load opens, and a still
/// decodes.
pub const READER_PREROLL: f64 = 1.0;

/// A cut to draw, with what it needs from outside.
pub struct CutJob {
    pub doc: Arc<CutDoc>,
    /// The marks (`name → seconds`) of every run render a `marker()` uses.
    pub marks: HashMap<RunRef, HashMap<String, f64>>,
    pub resolver: Box<dyn MediaResolver>,
    /// Decode the sound of every audio entry (renders; a preview is silent).
    pub audio: bool,
}

/// How much of the host's decoding an engine may use.
pub struct EngineOptions {
    /// Clip loads open at once (the reader's own bound is 8).
    pub max_loads: usize,
    /// A decoder slot of the host's budget per admitted load (Stage: the VJ
    /// decks' budget, so the preview and the decks share its 8).
    pub budget: Option<Box<dyn DecoderBudget>>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self { max_loads: MAX_READER_CONSUMERS, budget: None }
    }
}

/// What the load worker found.
struct Loaded {
    /// The cut with every cut it nests.
    tree: CutTree,
    media: HashMap<MediaRef, ResolvedMedia>,
    /// Still sizes, from their headers.
    sizes: HashMap<MediaRef, (u32, u32)>,
    /// The sound graphics hear (a render's songs, decoded before the first
    /// frame); the mix's own decode and loudness follow on [`Sound`].
    pcm: HashMap<MediaRef, Arc<TrackPcm>>,
    takes: HashMap<AssetRef, TakeInfo>,
    transcripts: HashMap<AssetRef, Vec<SpokenLine>>,
    /// Library effect sources by node path.
    effects: HashMap<String, String>,
    /// Motion documents and the files they use (lane M1, `crate::graphic`).
    graphics: crate::graphic::GraphicAssets,
    /// Every app run render's recorded data (its run.json), and the marks
    /// in effect: the job's, else the run's own (`RunData::mark_seconds`).
    runs: HashMap<RunRef, Arc<RunData>>,
    /// How long each part of the load took, for the log.
    timings: String,
    /// Songs' aligned lyrics (`vj-lyrics` JSON) by asset, for graphics.
    lyrics: HashMap<AssetRef, String>,
    /// Song clips' analysis (beats, onsets, energy, bands) by node, on
    /// the source's clock, for graphics that read `music(@clip)`.
    music: HashMap<String, makepad_motion::MusicFeatures>,
    /// Song clips' analysed pictures by node, on the cut's clock, for
    /// kinetic type and effects (makepad-audio-reactive `AudioTrack`).
    tracks: HashMap<String, Arc<makepad_audio_reactive::AudioTrack>>,
    marks: HashMap<RunRef, HashMap<String, f64>>,
    /// The resolver, back for the documents loaded ahead and a render's
    /// sound ([`Sound`]).
    resolver: Option<Arc<std::sync::Mutex<Box<dyn MediaResolver>>>>,
    /// The graphics' document loads after the first frames' (timeline
    /// order: by first use).
    later: Vec<crate::graphic::Scan>,
}

/// A render's sound: every source the mix reads, decoded, and the mix's
/// loudness gain (`output.loudness`), measured over the whole mix. It runs
/// on the pool beside the first frames; the encoder takes it before the
/// first frame's sound.
type Sound = FrameJob<Result<(HashMap<MediaRef, Arc<TrackPcm>>, f64), String>>;

/// Every node of `tree`, by path: the root's by id, a nested cut's as
/// `clip/…/id` (the names its plans' ops carry). A cut nested twice is
/// walked twice, once per compound clip.
pub fn tree_nodes(tree: &CutTree) -> Vec<(String, &CutNode)> {
    fn walk<'t>(tree: &'t CutTree, doc: &'t CutDoc, prefix: &str, out: &mut Vec<(String, &'t CutNode)>) {
        for node in &doc.nodes {
            let path = format!("{prefix}{}", node.id);
            if let Some(edit) = node.kind.nested_edit() {
                if let Some(child) = tree.edits.get(edit) {
                    walk(tree, child, &format!("{path}/"), out);
                }
            }
            out.push((path, node));
        }
    }
    let mut out = Vec::new();
    walk(tree, &tree.root, "", &mut out);
    out
}

/// The node at `path` and the cut it is in.
fn node_at<'t>(tree: &'t CutTree, path: &str) -> Option<&'t CutNode> {
    let mut doc = &tree.root;
    let mut parts = path.split('/').peekable();
    while let Some(id) = parts.next() {
        let node = doc.node(id)?;
        if parts.peek().is_none() {
            return Some(node);
        }
        doc = tree.edits.get(node.kind.nested_edit()?)?;
    }
    None
}

/// A nested cut's own captions show only when every compound clip on
/// the way to them says `subtitles: 1`; the cut's own always.
pub fn nested_captions_shown(tree: &CutTree, path: &str) -> bool {
    let Some((clips, _)) = path.rsplit_once('/') else { return true };
    let mut walked = String::new();
    for clip in clips.split('/') {
        walked = if walked.is_empty() { clip.to_string() } else { format!("{walked}/{clip}") };
        if !matches!(node_at(tree, &walked).map(|node| &node.kind), Some(CutNodeKind::Clip { subtitles: true, .. })) {
            return false;
        }
    }
    true
}

/// Map a nested cut's caption out through the compound clip that shows
/// it: child time `c` is parent time `start + (c - in) / speed`, clipped to
/// the clip's span; `None` when none of it is shown.
fn caption_through(caption: &Caption, clip: &CutNode) -> Option<Caption> {
    let CutNodeKind::Clip { source_in: SourceTime::Seconds(source_in), speed, .. } = &clip.kind else { return None };
    let (start, end) = clip.span.map_or((f64::NEG_INFINITY, f64::INFINITY), |span| (span.start, span.end()));
    let origin = clip.span.map_or(0.0, |span| span.start);
    let map = |c: f64| origin + (c - source_in) / speed.max(1e-9);
    let (from, to) = (map(caption.start).max(start), map(caption.end).min(end));
    if from >= to {
        return None;
    }
    let words = caption.words.iter().filter_map(|word| {
        let (a, b) = (map(word.start).max(from), map(word.end).min(to));
        (a < b).then(|| Word { text: word.text.clone(), start: a, end: b })
    }).collect::<Vec<_>>();
    (!words.is_empty()).then(|| Caption { start: from, end: to, words, provisional: caption.provisional })
}

/// One effect unit on its way: its install on this thread, its build
/// (the engine's geometry) on the pool.
struct Building {
    node: String,
    install: Option<Install>,
    task: FrameJob<BuiltEffect>,
}

/// The install half of a prepared pack effect.
pub(crate) type Install = Box<dyn FnOnce(&mut Cx, BuiltEffect) -> Box<dyn EffectView>>;

enum Stage {
    Loading(FrameJob<Result<Loaded, String>>),
    Building(Vec<Building>),
    Ready,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum EngineStatus {
    Loading,
    Ready,
    Failed(String),
}

/// BGRA pixels and their width and height.
type Pixels = (Vec<u32>, usize, usize);

/// A still near `t`: decoding on the pool, or its texture (possibly
/// smaller than the picture: the draw maps by the picture's own size).
enum Still {
    Loading(FrameJob<Result<Pixels, String>>),
    Ready(Texture),
}

/// What one document loaded ahead brings: its nodes and what it uses.
type DocFinds = (Vec<String>, crate::graphic::GraphicAssets);

/// The documents later frames show, loading ahead of the playhead.
enum DocLoads {
    /// One pool task scans them all (fanned out), each one's finds sent as
    /// it is done.
    #[cfg_attr(all(target_arch = "wasm32", not(target_feature = "atomics")), allow(dead_code))]
    Pool { finds: std::sync::mpsc::Receiver<DocFinds>, _task: TaskHandle<()> },
    /// Without workers: a job per document, in timeline order, one per
    /// display frame on the UI thread ([`FrameJob`]).
    #[cfg_attr(not(all(target_arch = "wasm32", not(target_feature = "atomics"))), allow(dead_code))]
    Frames(std::collections::VecDeque<(Vec<String>, FrameJob<crate::graphic::GraphicAssets>)>),
}

impl DocLoads {
    fn arrived(&mut self) -> Vec<DocFinds> {
        match self {
            DocLoads::Pool { finds, .. } => finds.try_iter().collect(),
            DocLoads::Frames(jobs) => {
                let Some((_, job)) = jobs.front_mut() else { return Vec::new() };
                let Some(result) = job.try_take() else { return Vec::new() };
                let (nodes, _) = jobs.pop_front().expect("the front job");
                // A failed scan finds nothing: its graphics load when drawn.
                let used = result.unwrap_or_else(|error| {
                    log!("edit render: a document loaded ahead: {error}");
                    Default::default()
                });
                vec![(nodes, used)]
            }
        }
    }
}

pub struct CutEngine {
    doc: Arc<CutDoc>,
    /// Physical still resolution; all source geometry remains in authored units.
    raster_size: Option<(u32, u32)>,
    /// Output pixels per recorded pixel of the last record (see `placed`).
    placed_scale: [f64; 2],
    /// Documents the host gave (node, document) before the compositor was
    /// made: handed to its graphics when it is.
    given: Vec<(String, makepad_motion::MotionDoc)>,
    /// The cut with everything it nests, once loaded.
    tree: Option<Arc<CutTree>>,
    marks: HashMap<RunRef, HashMap<String, f64>>,
    takes: HashMap<AssetRef, TakeInfo>,
    transcripts: HashMap<AssetRef, Vec<SpokenLine>>,
    stage: Stage,
    options: EngineOptions,
    media: HashMap<MediaRef, ResolvedMedia>,
    sizes: HashMap<MediaRef, (u32, u32)>,
    pcm: HashMap<MediaRef, Arc<TrackPcm>>,
    /// The mix's loudness gain (`output.loudness`), once [`Self::sound`] has measured it.
    mix_gain: f64,
    /// A render decodes its sound (`CutJob::audio`), measured on the pool
    /// while the first frames draw.
    audio: bool,
    sound: Option<Sound>,
    /// Graphics whose documents load ahead of the playhead and have not yet
    /// (by node id): a frame that shows one waits for it ([`Self::unloaded_at`]).
    pending: HashSet<String>,
    /// Those documents' finds as they arrive, and their loader.
    docs: Option<DocLoads>,
    stills: HashMap<MediaRef, Still>,
    /// Each clip node's frame texture and the payload in it.
    uploads: HashMap<String, (Texture, u32, u32, Arc<VideoFramePayload>)>,
    frames: Option<ClipFrames>,
    /// The graphics load as an editor's ([`Self::set_live_literals`]).
    live_literals: bool,
    compositor: Option<Compositor>,
    effects: HashMap<String, Box<dyn EffectView>>,
    /// Each vj_fx effect's source by node, and the second views a Freeze's
    /// picture draws with (`<node>@freeze<k>`) being built.
    effect_sources: HashMap<String, (Arc<dyn EffectPack>, String)>,
    freeze_builds: Vec<Building>,
    /// Graphics whose documents to load ahead, one per display frame, so
    /// the frame that first shows one does not wait for it; and the nodes
    /// queued. Every graphic of the cut is queued at its first preview
    /// frame, the ones entering in the next second first.
    warm_queue: std::collections::VecDeque<DrawOp>,
    warmed: HashSet<String>,
    warm_all: bool,
    /// What the last `record` that returned no frame was waiting for.
    waiting: String,
    /// Decoded bytes each clip load's reader keeps ([`Self::set_keep_frames`]).
    keep_frames: usize,
    /// Pictures a live preview shows for a clip it has no frame of yet (the
    /// host's thumbnails, [`Self::set_stand_ins`]).
    stand_ins: HashMap<MediaRef, Texture>,
    /// What stand-ins in the last preview record were standing in for.
    waiting_note: Vec<String>,
    /// Preview only: loads stay open past their clip while there is room,
    /// and clips just behind the playhead are opened too.
    warm_loads: bool,
    last_used: HashMap<String, f64>,
    /// The last inexact preview record: its time and the frames it drew,
    /// so the same stand-in is not drawn twice.
    last_stand_in: Option<(f64, Vec<usize>)>,
    /// When the engine started loading (seconds, `Cx::monotonic_now`).
    opened: f64,
    /// Renders: a clip between two of its frames mixes both
    /// ([`Self::set_clip_blend`]); else the frame at or before.
    clip_blend: bool,
    /// Locked time (a render or an export, not a preview): Motion
    /// graphics sample their shutters fully ([`Self::set_locked_time`]).
    locked_time: bool,
    /// Scene3D layers draw their coverage id maps ([`Self::set_scene_coverage`]).
    scene_coverage: bool,
    live: Option<f64>,
    pointer: Option<[f64; 2]>,
    /// The pictures the host holds for the edit's `AppView` nodes, by node
    /// ([`Self::set_externals`]).
    externals: HashMap<String, ExternalFrame>,
    /// The language the graphics render in ([`Self::set_lang`]).
    lang: Option<Arc<str>>,
}

impl CutEngine {
    /// Set a fixed raster size before drawing; never retime or edit the source.
    pub fn set_raster_size(&mut self, size: (u32, u32)) { self.raster_size = Some(size); }

    /// The pixel size frames are recorded at from the next record on (None:
    /// the document's output size). A live picture follows the size it is
    /// shown at, so it is drawn at the display's own pixels.
    pub fn set_raster(&mut self, size: Option<(u32, u32)>) { self.raster_size = size; }

    /// Source-aware picking uses the current engine, only on editor demand.
    pub fn inspect_graphics(&mut self, cx: &mut Cx2d, t: f64) -> Vec<crate::graphic::GraphicInspection> {
        let plan = self.eval().frame_plan(t);
        let Some(compositor) = &mut self.compositor else { return Vec::new() };
        plan.ops.iter().enumerate().filter_map(|(slot, op)| {
            if matches!(op.kind, DrawKind::Graphic { opacity, .. } if opacity <= 0.0) { return None; }
            let (placement, clip) = plan.placement(slot)?;
            compositor.graphics.inspect(cx, op, placement, [clip.x, clip.y, clip.w, clip.h])
        }).collect()
    }

    /// Start loading `job`. Nothing blocks: the resolve runs on the pool.
    pub fn new(cx: &mut Cx, job: CutJob) -> Self {
        Self::with_options(cx, job, EngineOptions::default())
    }

    pub fn with_options(cx: &mut Cx, job: CutJob, options: EngineOptions) -> Self {
        let CutJob { doc, marks, resolver, audio } = job;
        let load_doc = doc.clone();
        let load_marks = marks.clone();
        let stage = match cx.task_pool().submit_frame_job(Class::Background, move || {
            trace!("stage.edit_load", "edit load worker: starting cut tree and references");
            let resolver = Arc::new(std::sync::Mutex::new(resolver));
            let result = load(load_doc, &load_marks, &resolver, audio);
            match &result {
                Ok(loaded) => trace!("stage.edit_load", "edit load worker: complete; awaiting Cx intake; {}", loaded.timings),
                Err(error) => log!("edit load worker: failed: {error}"),
            }
            result.map(|loaded| Loaded { resolver: Some(resolver), ..loaded })
        }) {
            Ok(task) => Stage::Loading(task),
            Err(error) => Stage::Failed(format!("edit load: {error:?}")),
        };
        Self {
            doc, raster_size: None, placed_scale: [1.0, 1.0], given: Vec::new(), tree: None, marks, takes: HashMap::new(), transcripts: HashMap::new(), stage, options,
            media: HashMap::new(), sizes: HashMap::new(), pcm: HashMap::new(), mix_gain: 1.0, audio, sound: None, pending: HashSet::new(), docs: None, stills: HashMap::new(),
            uploads: HashMap::new(), frames: None, live_literals: false, live: None, pointer: None, compositor: None, effects: HashMap::new(), effect_sources: HashMap::new(), freeze_builds: Vec::new(), warm_queue: Default::default(), warmed: HashSet::new(), warm_all: false, waiting: String::new(),
            keep_frames: 0, last_stand_in: None, stand_ins: HashMap::new(), waiting_note: Vec::new(),
            warm_loads: false, last_used: HashMap::new(), opened: Cx::monotonic_now(), clip_blend: false, locked_time: false, scene_coverage: false, externals: HashMap::new(), lang: None,
        }
    }

    /// How graphic `node`'s document loads in this cut (its clock, what it
    /// hears of the cut), once the cut has loaded: for a host loading the
    /// same text (the graphic desk), so it has the cut's own document.
    pub fn film_load(&mut self, node: &str) -> Option<crate::graphic::FilmLoad> {
        // (A graphic always active has no span: any time shows it.)
        let at = match self.doc.graphics.iter().find(|g| g.id == node)?.span {
            Some(span) => span.start + span.length * 0.5,
            None => 0.0,
        };
        let plan = self.eval().frame_plan(at);
        let op = plan.ops.iter().find(|op| op.node == node && matches!(op.kind, DrawKind::Graphic { .. }))?;
        let mut film = self.compositor.as_mut()?.graphics.film_load(op)?;
        // What it depends on in the cut: its op (clock, params, beat) and
        // what graphics hear (the songs, the voiceovers, app runs).
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        if let DrawKind::Graphic { params, offset, bpm, .. } = &op.kind {
            format!("{params:?} {offset:?} {bpm:?}").hash(&mut h);
        }
        for n in self.doc.nodes.iter().filter(|n| matches!(n.kind, CutNodeKind::AudioClip { .. } | CutNodeKind::VoiceOver { .. })) {
            format!("{n:?}").hash(&mut h);
        }
        film.fingerprint = h.finish();
        Some(film)
    }

    /// Graphic `node`'s document as the host has it loaded already: drawn
    /// as it is, not loaded again ([`crate::graphic::Graphics::give`]).
    pub fn give_graphic(&mut self, node: &str, doc: makepad_motion::MotionDoc) {
        match &mut self.compositor {
            Some(compositor) => compositor.graphics.give(node, doc),
            None => self.given.push((node.to_string(), doc)),
        }
    }

    /// The ambient clock Motion plates can read independently of cut time.
    pub fn set_live(&mut self, live: f64) {
        if self.live != Some(live) && self.reads_live() {
            self.last_stand_in = None;
        }
        self.live = Some(live);
        if let Some(compositor) = &mut self.compositor {
            compositor.graphics.set_live(live);
        }
    }

    /// The presenter's pointer over the picture (fractions 0..1; `None` off
    /// it) for the Motion plates that declare `pointer_x` / `pointer_y`.
    pub fn set_pointer(&mut self, pointer: Option<[f64; 2]>) {
        if self.pointer != pointer && self.reads_pointer() {
            self.last_stand_in = None;
        }
        self.pointer = pointer;
        if let Some(compositor) = &mut self.compositor {
            compositor.graphics.set_pointer(pointer);
        }
    }

    pub fn reads_pointer(&self) -> bool {
        self.compositor.as_ref().is_some_and(|compositor| compositor.graphics.reads_pointer())
    }

    /// Every frame of the cut is the same picture: nothing in it changes
    /// with time (`CutDoc::picture_is_still`) and each of its Motion
    /// documents, as drawn, is still too. Not known (false) until they have
    /// been drawn once. Live reads (`reads_live`) are asked separately.
    pub fn time_independent(&self) -> bool {
        self.doc.picture_is_still()
            && self.compositor.as_ref().is_some_and(|compositor| compositor.graphics.all_timeless(self.doc.motion_nodes()))
    }

    pub fn reads_live(&self) -> bool {
        self.compositor.as_ref().is_some_and(|compositor| compositor.graphics.reads_live())
    }

    /// Frame blend for clips whose frames do not fall on the output's
    /// (another rate): each output frame mixes the source frame at or
    /// before its time with the next, by how far between them it falls.
    /// Off: the frame at or before (a hold). Recorded frames only, never a
    /// preview's stand-ins.
    pub fn set_clip_blend(&mut self, blend: bool) {
        self.clip_blend = blend;
    }

    /// Frames are pictures of their exact time (a render, an export): an
    /// HDR Motion graphic with `motion_blur` adds sub-frames until each
    /// frame converges, and Scene3D layers sample their shutters fully. A
    /// preview (the default) draws one frame per paint.
    pub fn set_locked_time(&mut self, on: bool) {
        self.locked_time = on;
        if let Some(compositor) = &mut self.compositor {
            compositor.graphics.set_locked_time(on);
        }
    }

    /// Every Scene3D layer also draws its coverage id map: an inspect
    /// grab's ask (an extra pass per scene), never a render's.
    pub fn set_scene_coverage(&mut self, on: bool) {
        self.scene_coverage = on;
        if let Some(compositor) = &mut self.compositor {
            compositor.graphics.set_scene_coverage(on);
        }
    }

    /// The last recorded frame's Scene3D coverage maps, with the graphic
    /// node each is in, while [`Self::set_scene_coverage`] is on.
    pub fn scene_coverage(&self) -> Vec<(String, makepad_motion_draw::coverage::SceneCoverage)> {
        self.compositor.as_ref().map_or(Vec::new(), |compositor| compositor.graphics.scene_coverage())
    }

    /// The graphics load as an editor's (their literals patched live in
    /// place, each document's sites by its own name: `motion:<id>@<rev>`);
    /// a change loads them again.
    /// Render the cut's Motion graphics in a language (`lang()`): this
    /// engine is one output's render (DECK's output window).
    pub fn set_lang(&mut self, lang: Option<Arc<str>>) {
        self.lang = lang.clone();
        if let Some(compositor) = &mut self.compositor {
            compositor.graphics.set_lang(lang);
        }
    }

    pub fn set_live_literals(&mut self, on: bool) {
        self.live_literals = on;
        if let Some(compositor) = &mut self.compositor {
            compositor.graphics.set_editable(on);
        }
    }

    /// The cut's clips moved (a held gesture on their spans): the frames
    /// follow `doc` at once; a graphic whose clock moved draws with the
    /// document it had (on the new clock) until it has loaded again.
    pub fn retime(&mut self, doc: Arc<CutDoc>) {
        if let Some(tree) = &self.tree {
            let mut tree = (**tree).clone();
            tree.root = (*doc).clone();
            self.tree = Some(Arc::new(tree));
        }
        self.doc = doc;
    }

    /// Library documents' texts changed during a held gesture: those that
    /// read them load again behind the ones that ran.
    pub fn replace_texts(&mut self, graphics: Vec<(AssetRef, String)>, modules: Vec<(AssetRef, String)>) {
        if let Some(compositor) = &mut self.compositor {
            compositor.graphics.replace_texts(graphics, modules);
        }
    }

    /// A held recolour's step on the copies the graphics' loads made.
    pub fn recolour_copies(&mut self, map: &[([f32; 4], [f32; 4])]) -> usize {
        self.compositor.as_mut().map_or(0, |c| c.graphics.recolour_copies(map))
    }

    /// A held gesture ended.
    pub fn gesture_end(&mut self) {
        if let Some(compositor) = &mut self.compositor {
            compositor.graphics.gesture_end();
        }
    }

    /// Literals of the graphics were patched (`Cx::patch_literal`): their
    /// models are read again from their running documents and the frame is
    /// drawn again.
    pub fn literals_patched(&mut self) {
        if let Some(compositor) = &mut self.compositor {
            compositor.graphics.literals_patched();
        }
    }

    /// Seconds since the engine started loading.
    pub fn since_open(&self) -> f64 {
        Cx::monotonic_now() - self.opened
    }

    /// Pictures to show for a clip whose frames are not here yet (a live
    /// preview: the library thumbnails), with each source's size in pixels
    /// (so its fit is known before its file has opened).
    pub fn set_stand_ins(&mut self, pictures: HashMap<MediaRef, (Texture, u32, u32)>) {
        for (media, (texture, width, height)) in pictures {
            self.sizes.entry(media.clone()).or_insert((width, height));
            self.stand_ins.insert(media, texture);
        }
    }

    /// What the last preview frame showed stand-ins for, and why.
    pub fn waiting_note(&self) -> &[String] {
        &self.waiting_note
    }

    /// A live preview: loads stay warm (see the field).
    pub fn set_warm_loads(&mut self, warm: bool) {
        self.warm_loads = warm;
    }

    /// Let each clip load's reader keep up to `bytes` of decoded frames,
    /// so reads back and forth over them (a scrub) decode nothing again.
    /// Before the cut has loaded; a render keeps none.
    pub fn set_keep_frames(&mut self, bytes: usize) {
        self.keep_frames = bytes;
    }

    pub fn doc(&self) -> &Arc<CutDoc> {
        &self.doc
    }

    pub fn status(&self) -> EngineStatus {
        match &self.stage {
            Stage::Loading(_) | Stage::Building(_) => EngineStatus::Loading,
            Stage::Ready => EngineStatus::Ready,
            Stage::Failed(error) => EngineStatus::Failed(error.clone()),
        }
    }

    /// What the last `record` without a frame was waiting for.
    pub fn waiting_for(&self) -> &str {
        &self.waiting
    }

    /// Whether a render's sound is decoded and measured ([`Self::audio_mix`]
    /// is whole): at once without one, else once its pool task is done.
    pub fn sound_ready(&self) -> bool {
        !self.audio && self.sound.is_none()
    }

    /// The mixdown's inputs, once loaded (a render's: once [`Self::sound_ready`]).
    pub fn audio_mix(&self) -> AudioMix {
        let mut mix = AudioMix::new(self.doc.clone(), self.pcm.clone());
        mix.tree = self.tree.clone();
        mix.marks = self.marks.clone();
        mix.takes = self.takes.clone();
        mix.gain = self.mix_gain;
        mix
    }

    /// Once loaded, the job that decodes the cut's sound for a live preview
    /// (on a worker: it reads and decodes every sounding source). The cut's
    /// media are the ones the load resolved; a voice take only the audio
    /// plan names plays silent there.
    pub fn sound_job(&self) -> Option<impl FnOnce() -> Result<AudioMix, String> + Send + 'static> {
        let tree = self.tree.clone()?;
        let (doc, marks, takes, mut media) = (self.doc.clone(), self.marks.clone(), self.takes.clone(), self.media.clone());
        Some(move || {
            let mut eval = tree.eval();
            for (run, marks) in &marks {
                eval.set_marks(run.clone(), marks.clone());
            }
            for (take, info) in &takes {
                eval.set_take(take.clone(), info.clone());
            }
            let pcm = decode_sounds(&eval, doc.duration, &mut media, None)?;
            drop(eval);
            Ok(edit_mix(doc, tree, marks, takes, pcm))
        })
    }

    /// The evaluator with everything loaded: marks, takes, transcripts and
    /// every source size known so far.
    pub fn eval(&self) -> CutEval<'_> {
        let mut eval = match &self.tree {
            Some(tree) => tree.eval(),
            None => CutEval::new(&self.doc),
        };
        for (run, marks) in &self.marks {
            eval.set_marks(run.clone(), marks.clone());
        }
        for (take, info) in &self.takes {
            eval.set_take(take.clone(), info.clone());
        }
        for (transcript, lines) in &self.transcripts {
            eval.set_transcript(transcript.clone(), lines.clone());
        }
        for (media, (w, h)) in &self.sizes {
            eval.set_source_size(media.clone(), *w, *h);
        }
        eval
    }

    /// The plan of frame time `t` (for a host that wants to show it).
    pub fn plan(&self, t: f64) -> FramePlan {
        self.eval().frame_plan(t)
    }

    fn fail(&mut self, error: String) {
        log!("edit render: {error}");
        self.stage = Stage::Failed(error);
    }

    /// Advance loading and take reader results. Call on every event.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if let Some(frames) = &mut self.frames {
            frames.poll();
        }
        // Motion graphics' vj_fx builds and model textures (lane M1).
        if let Some(compositor) = &mut self.compositor {
            compositor.graphics.handle_event(cx, event);
        }
        match &mut self.stage {
            Stage::Loading(task) => match task.try_take() {
                None => {}
                Some(Err(error)) => self.fail(format!("edit load: {error:?}")),
                Some(Ok(Err(error))) => self.fail(error),
                Some(Ok(Ok(loaded))) => self.loaded(cx, loaded),
            },
            Stage::Building(building) => {
                let mut failed = None;
                building.retain_mut(|unit| match unit.task.try_take() {
                    None => true,
                    Some(Err(error)) => {
                        failed = Some(format!("effect `{}`: geometry build: {error:?}", unit.node));
                        false
                    }
                    Some(Ok(built)) => {
                        let Some(install) = unit.install.take() else { return false };
                        self.effects.insert(unit.node.clone(), effect_view(cx, install, built, self.doc.bpm));
                        false
                    }
                });
                if let Some(error) = failed {
                    self.fail(error);
                } else if building.is_empty() {
                    log!("edit load: effects built; ready {:.2} s after open", Cx::monotonic_now() - self.opened);
                    self.stage = Stage::Ready;
                }
            }
            Stage::Ready | Stage::Failed(_) => {}
        }
        // Documents loaded ahead of the playhead: what they use joins the
        // graphics', and the frames that show them may draw.
        let arrived: Vec<DocFinds> = self.docs.as_mut().map(DocLoads::arrived).unwrap_or_default();
        for (nodes, used) in arrived {
            for (asset, resolved) in &used.videos {
                self.media.entry(MediaRef::Asset(asset.clone())).or_insert_with(|| resolved.clone());
            }
            if let Some(compositor) = &mut self.compositor {
                compositor.graphics.add(used);
            }
            for node in &nodes {
                self.pending.remove(node);
            }
            if self.warm_all {
                let starts: Vec<f64> = self.doc.graphics.iter().filter(|graphic| nodes.contains(&graphic.id)).filter_map(|graphic| graphic.span.map(|span| span.start + 0.1)).collect();
                self.queue_warm(starts.into_iter(), false);
            }
        }
        if self.pending.is_empty() {
            self.docs = None;
        }
        if let Some(task) = &mut self.sound {
            match task.try_take() {
                None => {}
                Some(Ok(Ok((pcm, gain)))) => {
                    self.sound = None;
                    self.pcm = pcm;
                    self.mix_gain = gain;
                    self.audio = false;
                }
                Some(Ok(Err(error))) => {
                    self.sound = None;
                    self.fail(error);
                }
                Some(Err(error)) => {
                    self.sound = None;
                    self.fail(format!("edit sound: {error:?}"));
                }
            }
        }
        // A Freeze's own effect views.
        let bpm = self.doc.bpm;
        let effects = &mut self.effects;
        self.freeze_builds.retain_mut(|unit| match unit.task.try_take() {
            None => true,
            Some(Ok(built)) => {
                if let Some(install) = unit.install.take() {
                    effects.insert(unit.node.clone(), effect_view(cx, install, built, bpm));
                }
                false
            }
            Some(Err(error)) => {
                log!("edit render: effect `{}`: geometry build: {error:?}", unit.node);
                false
            }
        });
        // Stills that finished decoding.
        let mut failed = None;
        for (media, still) in self.stills.iter_mut() {
            let Still::Loading(task) = still else { continue };
            match task.try_take() {
                None => {}
                Some(Ok(Ok((data, width, height)))) => {
                    let texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
                        width, height, data: Some(data), updated: TextureUpdated::Full,
                    });
                    *still = Still::Ready(texture);
                }
                Some(Ok(Err(error))) => failed = Some(format!("{media:?}: {error}")),
                Some(Err(error)) => failed = Some(format!("{media:?}: {error:?}")),
            }
        }
        if let Some(error) = failed {
            self.fail(error);
        }
    }

    /// The worker's findings in place; effects start building.
    fn loaded(&mut self, cx: &mut Cx, loaded: Loaded) {
        let Loaded { tree, media, sizes, pcm, takes, transcripts, effects: library, graphics, runs, marks, timings, lyrics, music, tracks, resolver, later } = loaded;
        trace!("stage.edit_load", "edit load: {timings}; loaded {:.2} s after open", Cx::monotonic_now() - self.opened);
        self.marks = marks;
        let tree = Arc::new(tree);
        self.tree = Some(tree.clone());
        if self.audio {
            let (doc, tree, marks, takes, mut media, decoded) = (self.doc.clone(), tree.clone(), self.marks.clone(), takes.clone(), media.clone(), (!pcm.is_empty()).then(|| pcm.clone()));
            let resolver = resolver.clone();
            match cx.task_pool().submit_frame_job(Class::Background, move || sound(doc, tree, marks, takes, &mut media, decoded, resolver)) {
                Ok(task) => self.sound = Some(task),
                Err(error) => return self.fail(format!("edit sound: {error:?}")),
            }
        }
        self.media = media;
        self.sizes = sizes;
        self.pcm = pcm;
        self.takes = takes;
        self.transcripts = transcripts;
        let mut frames = match ClipFrames::spawn(cx, "edit-render", self.keep_frames) {
            Ok(frames) => frames,
            Err(error) => return self.fail(error),
        };
        if let Some(budget) = self.options.budget.take() {
            frames.set_budget(budget);
        }
        self.frames = Some(frames);
        let mut compositor = Compositor::new(cx);
        // Motion graphics draw in the compositor (lane M1); the videos
        // their documents show read like the cut's clips.
        for (asset, resolved) in &graphics.videos {
            self.media.entry(MediaRef::Asset(asset.clone())).or_insert_with(|| resolved.clone());
        }
        // The documents the later frames show load ahead of the playhead.
        if let (false, Some(resolver)) = (later.is_empty(), resolver) {
            self.pending = later.iter().flat_map(|scan| scan.nodes.iter().cloned()).collect();
            let root = Arc::new(tree.root.clone());
            let have = crate::graphic::GraphicAssets { sources: graphics.sources.clone(), modules: graphics.modules.clone(), ..Default::default() };
            match self.load_ahead(cx, root, later, have, resolver) {
                Ok(docs) => self.docs = Some(docs),
                Err(error) => return self.fail(format!("edit documents: {error:?}")),
            }
        }
        compositor.graphics.install(graphics);
        compositor.graphics.set_locked_time(self.locked_time);
        compositor.graphics.set_scene_coverage(self.scene_coverage);
        if let Some(live) = self.live {
            compositor.graphics.set_live(live);
        }
        compositor.graphics.set_pointer(self.pointer);
        compositor.graphics.set_editable(self.live_literals);
        compositor.graphics.set_lang(self.lang.clone());
        // Graphics bound to an app run follow its recording (lane M4).
        compositor.graphics.set_runs(self.doc.clone(), runs, self.doc.output.fps);
        // Graphics hear the cut's voiceovers (their takes' word times) and
        // its songs (their sung words and analysis).
        if let Some(context) = crate::graphic::voice_context(&self.doc, &self.eval(), &lyrics, &music) {
            compositor.graphics.set_context(context);
        }
        // Kinetic type and effects read the songs' pictures at each frame's
        // own time (deterministic under locked time).
        compositor.graphics.set_audio_tracks(tracks);
        for (node, doc) in self.given.drain(..) {
            compositor.graphics.give(&node, doc);
        }
        self.compositor = Some(compositor);
        let mut building = Vec::new();
        for (path, node) in tree_nodes(&tree) {
            let (pack, source) = match &node.kind {
                CutNodeKind::Fx { effect: EffectRef::Asset(_), .. } | CutNodeKind::Transition { effect_asset: Some(_), .. } => match library.get(&path) {
                    Some(source) => match packs::library() {
                        Some(pack) => (pack, source.clone()),
                        None => return self.fail(format!("`{}`: {}", path, packs::missing("its library effect"))),
                    },
                    None => return self.fail(format!("`{}`: its library effect was not loaded", path)),
                },
                CutNodeKind::Fx { effect: EffectRef::Preset(name), .. } | CutNodeKind::Transition { effect: name, .. } => {
                    if name.is_empty() {
                        continue;
                    }
                    // Render-kit effects draw in the compositor itself.
                    if crate::compose::is_kit_effect(name) {
                        let dials = match &node.kind {
                            CutNodeKind::Fx { .. } => [-1.0; 4],
                            _ => return self.fail(format!("`{}`: `{name}` is not a transition", path)),
                        };
                        if let Err(error) = crate::compose::kit_effect(name, &dials) {
                            return self.fail(format!("`{}`: {error}", path));
                        }
                        continue;
                    }
                    let bare = name.strip_prefix("vjfx/").unwrap_or(name);
                    // A transition drives its preset's p4 with the mix, as
                    // the live renderer does: only transition presets
                    // mean anything under that lever.
                    let Some((pack, _, source)) = packs::resolve(bare) else {
                        if makepad_content_packs::effect_packs().is_empty() {
                            return self.fail(format!("`{}`: {}", path, packs::missing(&format!("`{name}`"))));
                        }
                        return self.fail(format!("`{}`: `{name}` is not a bundled effect preset", path));
                    };
                    if matches!(node.kind, CutNodeKind::Transition { .. }) && !pack.is_transition(bare) {
                        return self.fail(format!("`{}`: `{name}` is not a transition preset", path));
                    }
                    (pack, source.to_string())
                }
                _ => continue,
            };
            self.effect_sources.insert(path.clone(), (pack.clone(), source.clone()));
            let prepared = match pack.prepare(cx, &format!("edit/{}", path), &source) {
                Ok(prepared) => prepared,
                Err(error) => return self.fail(format!("`{}`: {error}", path)),
            };
            match cx.task_pool().submit_frame_job(Class::Background, prepared.build) {
                Ok(task) => building.push(Building { node: path.clone(), install: Some(prepared.install), task }),
                Err(error) => return self.fail(format!("`{}`: geometry build: {error:?}", path)),
            }
        }
        self.stage = if building.is_empty() { Stage::Ready } else { Stage::Building(building) };
    }

    /// Load the documents of `later` ahead of the playhead.
    #[cfg(not(all(target_arch = "wasm32", not(target_feature = "atomics"))))]
    fn load_ahead(&self, cx: &mut Cx, root: Arc<CutDoc>, later: Vec<crate::graphic::Scan>, have: crate::graphic::GraphicAssets, resolver: Arc<std::sync::Mutex<Box<dyn MediaResolver>>>) -> Result<DocLoads, SubmitError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let task = cx.task_pool().submit(Class::Background, move || {
            let order: Vec<usize> = (0..later.len()).collect();
            let tx = std::sync::Mutex::new(tx);
            crate::graphic::scan_all(&root, &later, &order, &have, &resolver, &|index, used| {
                let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send((later[index].nodes.clone(), used));
                makepad_widgets::makepad_platform::thread::SignalToUI::set_ui_signal();
            });
        })?;
        Ok(DocLoads::Pool { finds: rx, _task: task })
    }

    /// Without workers: one job per document, in timeline order.
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    fn load_ahead(&self, cx: &mut Cx, root: Arc<CutDoc>, later: Vec<crate::graphic::Scan>, have: crate::graphic::GraphicAssets, resolver: Arc<std::sync::Mutex<Box<dyn MediaResolver>>>) -> Result<DocLoads, SubmitError> {
        let have = Arc::new(have);
        let mut jobs = std::collections::VecDeque::new();
        for scan in later {
            let (root, have, resolver, nodes) = (root.clone(), have.clone(), resolver.clone(), scan.nodes.clone());
            let job = cx.task_pool().submit_frame_job(Class::Background, move || {
                let modules = crate::graphic::module_host(&have.modules);
                crate::graphic::scan(&root, &scan, &have, &modules, &mut crate::graphic::Shared(&resolver))
            })?;
            jobs.push_back((nodes, job));
        }
        Ok(DocLoads::Frames(jobs))
    }

    /// The sources the plans of `t` and of the next [`READER_PREROLL`]
    /// show: clip paths visible now, clip paths coming (soonest first),
    /// and stills near.
    fn sources_near(&self, t: f64) -> (Vec<(String, MediaRef)>, Vec<(String, MediaRef)>, HashSet<MediaRef>) {
        let (mut visible, mut soon, mut stills) = (Vec::new(), Vec::new(), HashSet::new());
        let eval = self.eval();
        let steps: &[f64] = if self.warm_loads { &[0.0, 0.5, 1.0, -0.5, -1.0] } else { &[0.0, 0.5, 1.0] };
        for (step, at) in steps.iter().map(|k| k * READER_PREROLL).enumerate() {
            for op in eval.frame_plan(t + at).ops {
                let DrawKind::Source { media, still, .. } = op.kind else { continue };
                if still {
                    stills.insert(media);
                } else if self.media.get(&media).is_some_and(|resolved| resolved.kind == MediaKind::Video) {
                    let known = visible.iter().chain(soon.iter()).any(|(path, _): &(String, MediaRef)| *path == op.node);
                    if !known {
                        if step == 0 { visible.push((op.node, media)) } else { soon.push((op.node, media)) }
                    }
                }
            }
        }
        (visible, soon, stills)
    }

    /// The plan of `t` with what its graphics' Freezes show (Motion's
    /// `Need::CutFrame`): the cut's layers under the graphic at the
    /// freeze's time. At `t` itself those are the plan's own slots (no
    /// second drawing); at another time that time's plan, cut to what is
    /// under the graphic, goes first (its nodes named `<node>@freeze<k>`), so
    /// its clips load and wait like any other. Per graphic node: (place in
    /// the document, slots to stack).
    fn frozen_plan(&mut self, t: f64) -> (FramePlan, HashMap<String, Vec<(String, Vec<(usize, Blend)>)>>) {
        let mut plan = self.eval().frame_plan(t);
        let mut needs = Vec::new();
        if let Some(compositor) = &mut self.compositor {
            for (g, op) in plan.ops.iter().enumerate() {
                if matches!(op.kind, DrawKind::Graphic { .. }) {
                    needs.extend(compositor.graphics.edit_frame_needs(op).into_iter().map(|(path, at)| (g, path, at)));
                }
            }
        }
        if needs.is_empty() {
            return (plan, HashMap::new());
        }
        let fps = crate::audio::fps_ratio(self.doc.output.fps);
        let half_frame = 0.5 * fps.1 as f64 / fps.0 as f64;
        let eval = self.eval();
        let mut first: Vec<DrawOp> = Vec::new();
        // (graphic node, place, slots, whether the slots are the plan's own).
        let mut frames: Vec<(String, String, Vec<(usize, Blend)>, bool)> = Vec::new();
        let mut times = 0usize;
        let mut drawn_then: HashMap<(String, u64), Vec<(usize, Blend)>> = HashMap::new();
        for (g, path, at) in needs {
            let node = plan.ops[g].node.clone();
            if (at - t).abs() <= half_frame {
                frames.push((node, path, below(&plan, g), true));
                continue;
            }
            // Another Freeze of this graphic at the same time (a Polaroid's
            // photo and its full-frame flash): the same picture.
            if let Some(layers) = drawn_then.get(&(node.clone(), at.to_bits())) {
                frames.push((node, path, layers.clone(), false));
                continue;
            }
            let then = eval.frame_plan(at);
            let layers = match then.ops.iter().position(|op| op.node == node && matches!(op.kind, DrawKind::Graphic { .. })) {
                Some(g) => below(&then, g),
                // Not showing then: the whole picture is under it.
                None => then.result.map(|slot| vec![(slot, Blend::Normal)]).unwrap_or_default(),
            };
            let mut wanted = vec![false; then.ops.len()];
            let mut open: Vec<usize> = layers.iter().map(|(slot, _)| *slot).collect();
            while let Some(slot) = open.pop() {
                if !std::mem::replace(&mut wanted[slot], true) {
                    open.extend(slot_inputs(&then.ops[slot].kind));
                }
            }
            // Names stay the same from frame to frame (the k-th picture this
            // frame freezes): its clips' loads and effect views are kept.
            let k = times;
            times += 1;
            let offset = first.len();
            for (op, wanted) in then.ops.into_iter().zip(wanted) {
                let kind = if wanted { shift_slots(op.kind, offset) } else { DrawKind::Stack { layers: Vec::new() } };
                first.push(DrawOp { node: format!("{}@freeze{k}", op.node), size: op.size, kind });
            }
            let layers: Vec<(usize, Blend)> = layers.into_iter().map(|(slot, blend)| (slot + offset, blend)).collect();
            drawn_then.insert((node.clone(), at.to_bits()), layers.clone());
            frames.push((node, path, layers, false));
        }
        let by = first.len();
        if by > 0 {
            first.extend(plan.ops.drain(..).map(|op| DrawOp { kind: shift_slots(op.kind, by), ..op }));
            plan.ops = first;
            plan.result = plan.result.map(|slot| slot + by);
        }
        let mut out: HashMap<String, Vec<(String, Vec<(usize, Blend)>)>> = HashMap::new();
        for (node, path, layers, own) in frames {
            let layers = if own { layers.into_iter().map(|(slot, blend)| (slot + by, blend)).collect() } else { layers };
            out.entry(node).or_default().push((path, layers));
        }
        (plan, out)
    }

    /// Queue the graphics of `times` not loaded ahead yet.
    /// `soon`: ahead of what is queued (the playhead's next second).
    fn queue_warm(&mut self, times: impl Iterator<Item = f64>, soon: bool) {
        let eval = self.eval();
        let ops: Vec<DrawOp> = times.flat_map(|at| eval.frame_plan(at).ops).filter(|op| matches!(op.kind, DrawKind::Graphic { .. })).collect();
        drop(eval);
        for op in ops {
            if self.warmed.insert(op.node.clone()) {
                if soon { self.warm_queue.push_front(op) } else { self.warm_queue.push_back(op) }
            }
        }
    }

    /// Load one queued graphic's document ahead (a preview calls this once
    /// a display frame, playing or not).
    pub fn warm(&mut self, cx: &mut Cx) {
        if !matches!(self.stage, Stage::Ready) {
            return;
        }
        if !self.warm_all {
            // On open: every graphic of the cut (sampled every second).
            self.warm_all = true;
            let duration = self.doc.duration;
            self.queue_warm((0..duration.ceil().max(1.0) as usize).map(|s| s as f64 + 0.1), false);
        }
        let Some(compositor) = &mut self.compositor else { return };
        compositor.graphics.poll_loads();
        let Some(op) = self.warm_queue.pop_front() else { return };
        // A document still loading ahead is warmed once it is in.
        if self.pending.contains(&op.node) {
            return;
        }
        compositor.graphics.prepare(cx, &op);
    }

    /// A graphic the frame at `t` shows (in its frame, or its shutter's
    /// span just around it, or a Freeze's picture) whose document still
    /// loads ahead of the playhead.
    fn unloaded_at(&mut self, t: f64) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let frame = 1.0 / self.doc.output.fps.max(1.0);
        let root = self.tree.as_ref().map_or(&*self.doc, |tree| &tree.root);
        if let Some(node) = waiting_graphic(root, &self.pending, t, frame) {
            return Some(node);
        }
        let plan = self.eval().frame_plan(t);
        let (frozen, _) = self.frozen_plan(t);
        plan.ops.iter().chain(frozen.ops.iter())
            .find(|op| matches!(op.kind, DrawKind::Graphic { .. }) && self.pending.contains(&op.node))
            .map(|op| op.node.clone())
    }

    /// A Freeze's picture draws its effects with views of their own (one
    /// view draws once a frame): built on first use, waited for.
    fn freeze_effects(&mut self, cx: &mut Cx, plan: &FramePlan) -> Vec<String> {
        let mut waiting = Vec::new();
        for op in &plan.ops {
            if !matches!(op.kind, DrawKind::Effect { .. } | DrawKind::Transition { .. }) || self.effects.contains_key(&op.node) {
                continue;
            }
            let Some((base, _)) = op.node.rsplit_once("@freeze") else { continue };
            let Some((pack, source)) = self.effect_sources.get(base) else { continue };
            waiting.push(format!("effect `{}` for a freeze", op.node));
            if self.freeze_builds.iter().any(|unit| unit.node == op.node) {
                continue;
            }
            let prepared = match pack.prepare(cx, &format!("edit/{}", op.node), source) {
                Ok(prepared) => prepared,
                Err(error) => {
                    log!("edit render: `{}`: {error}", op.node);
                    continue;
                }
            };
            if let Ok(task) = cx.task_pool().submit_frame_job(Class::Background, prepared.build) {
                self.freeze_builds.push(Building { node: op.node.clone(), install: Some(prepared.install), task });
            }
        }
        waiting
    }

    /// The video frames the Motion graphics at `t` show: (graphic node,
    /// its place in the document, the load that reads it, the video, the
    /// source time in 100 ns ticks). A timeline clip showing the same video
    /// at the same frame lends its load (no second decode).
    fn graphic_videos(&mut self, plan: &FramePlan) -> Vec<(String, String, String, MediaRef, i64)> {
        let Some(compositor) = &mut self.compositor else { return Vec::new() };
        let half_frame = (5_000_000.0 * crate::audio::fps_ratio(self.doc.output.fps).1 as f64 / crate::audio::fps_ratio(self.doc.output.fps).0 as f64) as i64;
        let mut out = Vec::new();
        for op in plan.ops.iter().filter(|op| matches!(op.kind, DrawKind::Graphic { .. })) {
            for (path, asset, seconds) in compositor.graphics.video_needs(op) {
                let media = MediaRef::Asset(asset);
                let hns = (seconds.max(0.0) * 1e7).round() as i64;
                let shared = plan.ops.iter().find(|clip| match &clip.kind {
                    DrawKind::Source { media: m, still: false, source_ticks: Some(ticks), .. } => *m == media && (ticks - hns).abs() <= half_frame,
                    _ => false,
                });
                let key = shared.map_or_else(|| format!("{}/{path}", op.node), |clip| clip.node.clone());
                out.push((op.node.clone(), path, key, media, hns));
            }
        }
        out
    }

    /// Open the reader loads [`plan_loads`] keeps for `t`, in its order,
    /// and close every other. Clips of nested cuts are loads like any
    /// other (by path), so the whole tree shares one budget.
    fn manage_loads(&mut self, visible: &[(String, MediaRef)], soon: &[(String, MediaRef)]) {
        let Some(frames) = &mut self.frames else { return };
        let ids = |clips: &[(String, MediaRef)]| clips.iter().map(|(path, _)| path.clone()).collect::<Vec<_>>();
        let visible_waiting = visible.iter().any(|(path, _)| frames.queued(path));
        let mut keep = plan_loads(&ids(visible), &ids(soon), self.options.max_loads, visible_waiting);
        if self.warm_loads {
            // Loads already open stay, most recently used first, while there
            // is room: a scrub back and forth finds them warm.
            let now = Cx::time_now();
            for id in keep.iter().chain(visible.iter().map(|(path, _)| path)) {
                self.last_used.insert(id.clone(), now);
            }
            let mut open: Vec<String> = frames.clips().into_iter().filter(|clip| !keep.contains(clip) && !frames.queued(clip)).collect();
            open.sort_by(|a, b| self.last_used.get(b).unwrap_or(&0.0).total_cmp(self.last_used.get(a).unwrap_or(&0.0)));
            for clip in open {
                if keep.len() >= self.options.max_loads.max(1) || visible_waiting {
                    break;
                }
                keep.push(clip);
            }
        }
        for clip in frames.clips() {
            if !keep.contains(&clip) {
                frames.close(&clip);
                self.uploads.remove(&clip);
            }
        }
        for (rank, id) in keep.iter().enumerate() {
            let Some((_, media)) = visible.iter().chain(soon).find(|(path, _)| path == id) else {
                // A warm load kept past its clip: only its rank moves.
                frames.rerank(id, rank);
                continue;
            };
            let Some(resolved) = self.media.get(media) else { continue };
            frames.open(id, resolved.path.clone(), resolved.revision, resolved.content, rank);
        }
    }

    /// Decode the stills near `t`, and let the others go.
    fn manage_stills(&mut self, cx: &mut Cx, near: HashSet<MediaRef>) {
        let (width, height) = (self.doc.output.width, self.doc.output.height);
        self.stills.retain(|media, _| near.contains(media));
        for media in near {
            if self.stills.contains_key(&media) {
                continue;
            }
            let Some(resolved) = self.media.get(&media) else { continue };
            let path = resolved.path.clone();
            match cx.task_pool().submit_frame_job(Class::Background, move || decode_still(&path, width, height)) {
                Ok(task) => {
                    self.stills.insert(media, Still::Loading(task));
                }
                Err(error) => return self.fail(format!("still worker: {error}")),
            }
        }
    }

    /// Record the frame at `t` under the pass being drawn, once every clip
    /// frame and still it shows has arrived: `Ok(Some(output))` when
    /// recorded this call, `Ok(None)` while waiting ([`Self::waiting_for`]).
    pub fn record(&mut self, cx: &mut Cx2d, t: f64) -> Result<Option<Texture>, String> {
        Ok(self.record_frame(cx, t, false)?.map(|(texture, _)| texture))
    }

    /// [`Self::record`] for a live preview: a clip whose exact frame is
    /// still being read shows the nearest frame it has instead, and the
    /// frame is recorded at once; `Ok(Some((output, exact)))`. A clip with
    /// no frame at all yet is waited for. The same stand-in picture at the
    /// same `t` is not recorded twice (`Ok(None)`).
    pub fn record_preview(&mut self, cx: &mut Cx2d, t: f64) -> Result<Option<(Texture, bool)>, String> {
        self.record_frame(cx, t, true)
    }

    fn record_frame(&mut self, cx: &mut Cx2d, t: f64, stand_in: bool) -> Result<Option<(Texture, bool)>, String> {
        match &self.stage {
            Stage::Ready => {}
            Stage::Failed(error) => return Err(error.clone()),
            _ => {
                self.waiting = "the cut to load".into();
                return Ok(None);
            }
        }
        // A graphic whose document still loads ahead of the playhead: the
        // frame waits for it (never drawn without it).
        if let Some(node) = self.unloaded_at(t) {
            self.waiting = format!("the document of `{node}` to load");
            return Ok(None);
        }
        // A graphic whose document is not loaded yet loads on the pool (a
        // preview also asks ahead, [`Self::warm`]; a thumbnail or a render
        // asks here, before anything reads its frame): the frame waits for
        // it, the drawing thread never parses a document.
        let ahead = self.eval().frame_plan(t);
        if let Some(compositor) = &mut self.compositor {
            for op in ahead.ops.iter().filter(|op| matches!(op.kind, DrawKind::Graphic { .. })) {
                compositor.graphics.prepare(cx, op);
            }
        }
        if stand_in {
            // What enters in the next second: loaded ahead ([`Self::warm`]).
            self.queue_warm([1.0, 0.5, 0.25].into_iter().map(|ahead| t + ahead), true);
        }
        let (mut visible, soon, mut stills) = self.sources_near(t);
        // What Freezes show at other times: their clips and stills load too.
        let (frozen, _) = self.frozen_plan(t);
        for op in &frozen.ops {
            let DrawKind::Source { media, still, .. } = &op.kind else { continue };
            if *still {
                stills.insert(media.clone());
            } else if self.media.get(media).is_some_and(|resolved| resolved.kind == MediaKind::Video) && !visible.iter().any(|(path, _)| *path == op.node) {
                visible.push((op.node.clone(), media.clone()));
            }
        }
        // Videos Motion graphics show (a Video layer, a Scene3D's screen):
        // read at the graphic's time like clips, sharing a timeline clip's
        // reader when it shows the same frame.
        let graphic_videos = self.graphic_videos(&frozen);
        for (_, _, key, media, _) in &graphic_videos {
            if !visible.iter().any(|(path, _)| path == key) {
                visible.push((key.clone(), media.clone()));
            }
        }
        self.manage_loads(&visible, &soon);
        self.manage_stills(cx, stills);
        let Some(frames) = &mut self.frames else { return Ok(None) };
        frames.poll();
        // Clip sizes come from their loads as they open.
        for (path, media) in &visible {
            if let Some(size) = frames.size(path) {
                self.sizes.insert(media.clone(), size);
            }
        }
        // A nested cut's captions come in the plan only where every
        // compound clip on the way opts in (`subtitles: 1`): video_cut
        // blanks the others and draws those at the parent's size.
        let (plan, edit_frames) = self.frozen_plan(t);
        let mut waiting: Vec<String> = self.freeze_effects(cx, &plan);
        // A graphic whose document is still loading on the pool: the frame
        // waits for it.
        if let Some(compositor) = &mut self.compositor {
            for op in plan.ops.iter().filter(|op| matches!(op.kind, DrawKind::Graphic { .. })) {
                if compositor.graphics.loading(op) {
                    waiting.push(format!("graphic `{}` loading", op.node));
                }
            }
        }
        let Some(frames) = &mut self.frames else { return Ok(None) };
        let mut shown: Vec<(String, Arc<VideoFramePayload>)> = Vec::new();
        let mut blank: HashSet<String> = HashSet::new();
        let mut blends: HashMap<String, f32> = HashMap::new();
        let mut inexact = false;
        // Clips a preview shows a stand-in for (no frame of them here yet).
        let mut standing: Vec<(String, MediaRef)> = Vec::new();
        self.waiting_note.clear();
        for op in &plan.ops {
            let DrawKind::Source { media, still, source_ticks, src_rect, .. } = &op.kind else { continue };
            if *still {
                if !matches!(self.stills.get(media), Some(Still::Ready(_))) {
                    waiting.push(format!("still `{}`", op.node));
                }
                continue;
            }
            let Some(hns) = source_ticks else {
                // Its marker is unresolved: it shows nothing.
                blank.insert(op.node.clone());
                continue;
            };
            if !frames.is_open(&op.node) {
                // Not a video (an audio-only or unresolved source): nothing to show.
                blank.insert(op.node.clone());
                continue;
            }
            if frames.queued(&op.node) {
                let why = frames.waiting().unwrap_or("the reader").to_string();
                let reason = format!("a decoder for clip `{}` ({why})", op.node);
                // A picture to stand in: the clip's last frame, else its
                // thumbnail. Without either the frame waits (the viewer holds
                // the last whole picture): never a black hole where it goes.
                let has_stand_in = self.uploads.contains_key(&op.node) || self.stand_ins.contains_key(media);
                if stand_in && self.warm_loads && src_rect.is_some() && has_stand_in {
                    // A live preview never stops for it: the clip's last
                    // frame, else its thumbnail, meanwhile.
                    inexact = true;
                    standing.push((op.node.clone(), media.clone()));
                    self.waiting_note.push(reason);
                } else {
                    waiting.push(reason);
                }
                continue;
            }
            if src_rect.is_none() {
                waiting.push(format!("the size of clip `{}`", op.node));
            }
            match frames.want(&op.node, *hns) {
                FrameState::Pending => match frames.nearest(&op.node, *hns).filter(|_| stand_in) {
                    Some(frame) => {
                        inexact = true;
                        shown.push((op.node.clone(), frame));
                    }
                    None if stand_in && self.warm_loads && src_rect.is_some() && (self.uploads.contains_key(&op.node) || self.stand_ins.contains_key(media)) => {
                        inexact = true;
                        standing.push((op.node.clone(), media.clone()));
                        self.waiting_note.push(format!("clip `{}`: {}", op.node, frames.why(&op.node)));
                    }
                    None => waiting.push(format!("the frame of clip `{}` ({})", op.node, frames.why(&op.node))),
                },
                FrameState::Ready(Some(frame)) => {
                    shown.push((op.node.clone(), frame));
                    // A frame blend (renders that ask): the next source
                    // frame too, mixed in by where `t` falls between them.
                    if self.clip_blend && !stand_in {
                        match frames.following(&op.node, *hns) {
                            Some((FrameState::Ready(Some(next)), amount)) => {
                                let key = format!("{}#next", op.node);
                                shown.push((key, next));
                                blends.insert(op.node.clone(), amount);
                            }
                            Some((FrameState::Pending, _)) => waiting.push(format!("the next frame of clip `{}` (frame blend)", op.node)),
                            Some((FrameState::Failed(error), _)) => return Err(format!("clip `{}`: {error}", op.node)),
                            Some((FrameState::Ready(None), _)) | None => {}
                        }
                    }
                }
                FrameState::Ready(None) => {
                    blank.insert(op.node.clone());
                }
                FrameState::Failed(error) => return Err(format!("clip `{}`: {error}", op.node)),
            }
        }
        for (_, _, key, _, hns) in &graphic_videos {
            if plan.ops.iter().any(|op| op.node == *key) {
                // A timeline clip's frame: read above.
                continue;
            }
            if frames.queued(key) {
                let why = frames.waiting().unwrap_or("the reader").to_string();
                waiting.push(format!("a decoder for a graphic's video ({why})"));
                continue;
            }
            match frames.want(key, *hns) {
                FrameState::Pending => match frames.nearest(key, *hns).filter(|_| stand_in) {
                    Some(frame) => {
                        inexact = true;
                        shown.push((key.clone(), frame));
                    }
                    None => waiting.push(format!("a graphic's video frame ({})", frames.why(key))),
                },
                FrameState::Ready(Some(frame)) => shown.push((key.clone(), frame)),
                FrameState::Ready(None) => {}
                FrameState::Failed(error) => return Err(format!("a graphic's video: {error}")),
            }
        }
        if waiting.is_empty() && !plan.unresolved.is_empty() {
            waiting.push(format!("the sizes {:?} need", plan.unresolved));
        }
        if !waiting.is_empty() {
            self.waiting = waiting.join(", ");
            return Ok(None);
        }
        self.waiting.clear();
        if inexact {
            let mut drawn: Vec<usize> = shown.iter().map(|(_, frame)| Arc::as_ptr(frame) as usize).collect();
            drawn.extend(standing.iter().map(|(node, _)| self.uploads.get(node).map_or(1, |(_, _, _, held)| Arc::as_ptr(held) as usize)));
            if self.last_stand_in.as_ref().is_some_and(|(at, frames)| *at == t && *frames == drawn) {
                return Ok(None);
            }
            self.last_stand_in = Some((t, drawn));
        } else {
            self.last_stand_in = None;
        }
        // The frames into their textures, each only when it changed.
        for (node, frame) in shown {
            let (w, h) = (frame.width, frame.height);
            match self.uploads.get_mut(&node) {
                Some((texture, tw, th, held)) if *tw == w && *th == h => {
                    if !Arc::ptr_eq(held, &frame) {
                        let mut buffer = texture.take_vec_u8(cx);
                        buffer.clear();
                        buffer.extend_from_slice(&frame.nv12);
                        texture.put_back_vec_u8(cx, buffer, None);
                        *held = frame;
                    }
                }
                _ => {
                    let texture = Texture::new_nv12(cx, w, h, frame.nv12.clone());
                    self.uploads.insert(node, (texture, w, h, frame));
                }
            }
        }
        let mut pictures = HashMap::new();
        for op in &plan.ops {
            let DrawKind::Source { media, still, .. } = &op.kind else { continue };
            if *still {
                if let (Some(Still::Ready(texture)), Some((width, height))) = (self.stills.get(media), self.sizes.get(media)) {
                    pictures.insert(op.node.clone(), SourcePicture::Image { texture: texture.clone(), width: *width, height: *height });
                }
            } else if let Some((_, media)) = standing.iter().find(|(node, _)| *node == op.node) {
                // The clip's last uploaded frame, else the host's thumbnail.
                if let Some((texture, width, height, frame)) = self.uploads.get(&op.node) {
                    pictures.insert(op.node.clone(), SourcePicture::Nv12 { texture: texture.clone(), width: *width, height: *height, color: frame.color, next: None });
                } else if let (Some(texture), Some((width, height))) = (self.stand_ins.get(media), self.sizes.get(media)) {
                    pictures.insert(op.node.clone(), SourcePicture::Image { texture: texture.clone(), width: *width, height: *height });
                }
            } else if !blank.contains(&op.node) {
                if let Some((texture, width, height, frame)) = self.uploads.get(&op.node) {
                    let next = blends.get(&op.node).and_then(|amount| {
                        let (next, w, h, next_frame) = self.uploads.get(&format!("{}#next", op.node))?;
                        (*w == *width && *h == *height && next_frame.color == frame.color).then(|| (next.clone(), *amount))
                    });
                    pictures.insert(op.node.clone(), SourcePicture::Nv12 { texture: texture.clone(), width: *width, height: *height, color: frame.color, next });
                }
            }
        }
        let mut by_node: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (node, path, key, _, _) in &graphic_videos {
            if let Some((texture, width, height, frame)) = self.uploads.get(key) {
                pictures.entry(key.clone()).or_insert(SourcePicture::Nv12 { texture: texture.clone(), width: *width, height: *height, color: frame.color, next: None });
                by_node.entry(node.clone()).or_default().push((path.clone(), key.clone()));
            }
        }
        // The frame step as the exact ratio `t` is on.
        let dt = crate::audio::frame_time(1, crate::audio::fps_ratio(self.doc.output.fps));
        let compositor = self.compositor.as_mut().expect("ready");
        compositor.graphic_videos = by_node;
        compositor.graphic_edit_frames = edit_frames;
        compositor.externals.clone_from(&self.externals);
        // `placed` is in the recorded frame's pixels; [`Self::placed`] gives
        // it back in output pixels.
        self.placed_scale = self.raster_size.map_or([1.0, 1.0], |(w, h)| [self.doc.output.width.max(1) as f64 / w.max(1) as f64, self.doc.output.height.max(1) as f64 / h.max(1) as f64]);
        Ok(Some((compositor.record(cx, &plan, &pictures, &mut self.effects, dt, self.raster_size), !inexact)))
    }

    /// The pictures of the apps the host hosts, per `AppView` node, for the
    /// records that follow (a node without one draws its placeholder).
    pub fn set_externals(&mut self, externals: HashMap<String, ExternalFrame>) {
        self.externals = externals;
    }

    /// Where the last record placed each `AppView` picture, as `[x, y, w, h]`
    /// in output pixels, by node.
    pub fn placed(&self) -> Vec<(String, [f64; 4])> {
        let [sx, sy] = self.placed_scale;
        self.compositor.as_ref().map_or(Vec::new(), |compositor| compositor.placed.iter().map(|(node, rect)| (node.clone(), [rect.x * sx, rect.y * sy, rect.w * sx, rect.h * sy])).collect())
    }

    /// The output picture the host shows: no frame is recorded into it
    /// (the next goes to the other of the two).
    pub fn hold_output(&mut self, texture: Option<Texture>) {
        if let Some(compositor) = &mut self.compositor {
            compositor.hold = texture;
        }
    }

    /// The target the last recorded frame lands in.
    pub fn output(&self) -> Option<Texture> {
        self.compositor.as_ref().and_then(|compositor| compositor.output().cloned())
    }

    /// Split the output's readback into horizontal strips of at most
    /// `rows` rows (`None`: one piece).
    pub fn set_readback_strips(&mut self, rows: Option<u32>) {
        if let Some(compositor) = &mut self.compositor {
            compositor.set_readback_strips(rows);
        }
    }

    /// Convert every recorded frame into the encoder's picture
    /// ([`crate::compose::PicturePlan`]), the output transparent or not.
    pub fn set_picture(&mut self, plan: Option<crate::compose::PicturePlan>, transparent: bool) {
        if let Some(compositor) = &mut self.compositor {
            compositor.set_picture(plan);
            compositor.set_transparent(transparent);
        }
    }

    /// Whether the next record has a free picture slot.
    pub fn picture_free(&self) -> bool {
        self.compositor.as_ref().is_none_or(|compositor| compositor.picture_free())
    }

    /// The last recorded frame's picture, taken (see
    /// [`crate::compose::Compositor::take_picture`]).
    pub fn take_picture(&mut self) -> Option<(usize, Vec<(Texture, u32)>)> {
        self.compositor.as_mut().and_then(|compositor| compositor.take_picture())
    }

    pub fn release_picture(&mut self, slot: usize) {
        if let Some(compositor) = &mut self.compositor {
            compositor.release_picture(slot);
        }
    }

    /// The textures to read the last recorded frame back from, top down,
    /// with each one's first row.
    pub fn readback_targets(&self) -> Vec<(Texture, u32)> {
        self.compositor.as_ref().map_or(Vec::new(), |compositor| compositor.readback_targets())
    }

    /// How far the last recorded frame has come.
    pub fn painted(&self, cx: &Cx) -> Painted {
        self.compositor.as_ref().map_or(Painted::Painting, |compositor| compositor.painted(cx, &self.effects))
    }

    /// Why the last recorded frame has not painted yet (for a log).
    pub fn painting_report(&self, cx: &Cx) -> String {
        self.compositor.as_ref().map_or("no compositor".into(), |compositor| compositor.painting_report(cx, &self.effects))
    }

    /// The shutter steps the recorded frame's graphics took so far: while
    /// it moves, recording again is progress, not a stall.
    pub fn shutter_progress(&self) -> u64 {
        self.compositor.as_ref().map_or(0, |compositor| compositor.graphics.shutter_progress())
    }

    /// The recorded frame must be recorded again to paint whole.
    pub fn stale(&self, cx: &Cx) -> bool {
        self.compositor.as_ref().is_some_and(|compositor| compositor.stale(cx, &self.effects))
    }

    /// What the cut's Motion graphics could not draw (a layer refused as
    /// not seekable, a document that does not load, a shader that does not
    /// compile, a missing file), each once: a picture tool reports these
    /// instead of showing the graphic blank.
    pub fn graphic_errors(&self) -> Vec<String> {
        self.compositor.as_ref().map_or(Vec::new(), |compositor| compositor.graphics.errors())
    }

    /// Keep the recorded frame's passes attached while it paints.
    pub fn reattach(&self, cx: &mut Cx2d) {
        if let Some(compositor) = &self.compositor {
            compositor.reattach(cx);
        }
    }

    /// The SRT and WebVTT captions of every Subtitles node, named by node
    /// id: this cut's own, and a nested cut's where every compound clip
    /// on the way opts in (named by its path with `.` for `/`), mapped out
    /// to this cut's time and clipped to the clips' spans.
    pub fn sidecars(&self) -> Vec<(String, String, String)> {
        let eval = self.eval();
        let text = |captions: &[Caption]| (makepad_story_edit_voice::sidecar::srt(captions), makepad_story_edit_voice::sidecar::vtt(captions, false));
        let mut out: Vec<(String, String, String)> = self.doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Subtitles { .. })).map(|node| {
            let (srt, vtt) = text(eval.captions(&node.id));
            (node.id.clone(), srt, vtt)
        }).collect();
        let Some(tree) = &self.tree else { return out };
        for (path, node) in tree_nodes(tree) {
            if !path.contains('/') || !matches!(node.kind, CutNodeKind::Subtitles { .. }) || !nested_captions_shown(tree, &path) {
                continue;
            }
            // Down the path: the evaluators and the clips on the way.
            let parts: Vec<&str> = path.split('/').collect();
            let mut child = &eval;
            let mut clips = Vec::new();
            let mut walked = String::new();
            for clip in &parts[..parts.len() - 1] {
                walked = if walked.is_empty() { clip.to_string() } else { format!("{walked}/{clip}") };
                let Some(clip_node) = node_at(tree, &walked) else { break };
                let Some(next) = clip_node.kind.nested_edit().and_then(|edit| child.nested(edit)) else { break };
                clips.push(clip_node);
                child = next;
            }
            if clips.len() + 1 != parts.len() {
                continue;
            }
            let captions: Vec<Caption> = child.captions(parts[parts.len() - 1]).iter().filter_map(|caption| {
                clips.iter().rev().try_fold(caption.clone(), |caption, clip| caption_through(&caption, clip))
            }).collect();
            let (srt, vtt) = text(&captions);
            out.push((path.replace('/', "."), srt, vtt));
        }
        out
    }

    /// Stop the reader's workers.
    /// Let go of every clip load and still, keeping the engine usable: the
    /// next record opens what it needs again ([`crate::CutThumbnail::retarget`]).
    pub fn release(&mut self) {
        if let Some(frames) = &mut self.frames {
            for clip in frames.clips() {
                frames.close(&clip);
            }
        }
        self.uploads.clear();
        self.stills.clear();
    }

    pub fn stop(&mut self) {
        if let Some(frames) = &mut self.frames {
            frames.stop();
        }
        self.stills.clear();
    }
}

/// Which clip loads to hold, in admission order: every clip `visible` now
/// (in plan order), then the ones coming within [`READER_PREROLL`]
/// (`soon`, soonest first), at most `cap`. While a visible clip still waits
/// for a decoder, no preroll load is held: it would keep the slot the
/// visible clip needs.
pub fn plan_loads(visible: &[String], soon: &[String], cap: usize, visible_waiting: bool) -> Vec<String> {
    let mut keep: Vec<String> = visible.to_vec();
    if !visible_waiting {
        keep.extend(soon.iter().filter(|id| !visible.contains(id)).cloned());
    }
    // The model allows at most six visible at once per edit.
    keep.truncate(cap.max(1));
    keep
}

/// A still decoded and brought down to the most the output can show of
/// it: covering the output, never larger than itself.
fn decode_still(path: &Path, width: u32, height: u32) -> Result<Pixels, String> {
    let bytes = makepad_asset_data::source::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let image = makepad_widgets::makepad_draw::image_cache::decode_image_from_data(&bytes).map_err(|e| format!("{}: {e:?}", path.display()))?;
    let scale = (width as f64 / image.width as f64).max(height as f64 / image.height as f64).min(1.0);
    if scale >= 1.0 {
        return Ok((image.data, image.width, image.height));
    }
    let image = image.area_downscaled(((image.width as f64 * scale).ceil() as usize).max(1), ((image.height as f64 * scale).ceil() as usize).max(1));
    Ok((image.data, image.width, image.height))
}

/// A PNG's or JPEG's size from its header.
fn image_dims(bytes: &[u8], media: MediaType) -> Option<(u32, u32)> {
    let (w, h) = match media {
        MediaType::Png => {
            let mut decoder = PngDecoder::new(ZCursor::new(bytes));
            decoder.decode_headers().ok()?;
            decoder.dimensions()?
        }
        _ => {
            let mut decoder = JpegDecoder::new(ZCursor::new(bytes));
            decoder.decode_headers().ok()?;
            decoder.dimensions()?
        }
    };
    Some((w as u32, h as u32))
}

/// The load worker: resolve every reference, read every still's size,
/// the voice takes' timings and the transcripts, and, for a render whose
/// graphics hear its songs, decode every sound the mix reads (the rest of a
/// render's sound follows on [`Sound`], beside its first frames).
fn load(doc: Arc<CutDoc>, marks: &HashMap<RunRef, HashMap<String, f64>>, shared: &std::sync::Mutex<Box<dyn MediaResolver>>, audio: bool) -> Result<Loaded, String> {
    let mut locked = crate::graphic::Shared(shared);
    let resolver: &mut dyn MediaResolver = &mut locked;
    let started = Cx::monotonic_now();
    let mut lap = started;
    let mut timings: Vec<String> = Vec::new();
    let mut note = |what: String, lap: &mut f64| {
        let timing = format!("{what} {:.0} ms", (Cx::monotonic_now() - *lap) * 1000.0);
        // Report on the worker, before Cx consumes Loaded. A blocked Cx must not hide
        // whether the loader is still resolving/decoding or already finished.
        trace!("stage.edit_load", "edit load worker: {timing}");
        timings.push(timing);
        *lap = Cx::monotonic_now();
    };
    let tree = CutTree::from_doc((*doc).clone(), None, &mut |edit| resolver.cut_source(edit))
        .map_err(|errors| errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "))?;
    let nodes: Vec<(String, CutNode)> = tree_nodes(&tree).into_iter().map(|(path, node)| (path, node.clone())).collect();
    let (runs, all_marks) = run_data(&nodes, marks, resolver);
    note(format!("edit tree ({} nested)", tree.edits.len()), &mut lap);
    let marks = &all_marks;
    let mut loaded = Loaded {
        tree,
        media: HashMap::new(), sizes: HashMap::new(), pcm: HashMap::new(), takes: HashMap::new(), transcripts: HashMap::new(),
        effects: HashMap::new(), graphics: Default::default(), runs, marks: marks.clone(), timings: String::new(), lyrics: HashMap::new(),
        music: HashMap::new(),
        tracks: HashMap::new(),
        resolver: None,
        later: Vec::new(),
    };
    // Motion documents: every library source and module the cut
    // names, then the documents the first frame shows, each loaded to find
    // what it uses; the others load ahead of the playhead once the cut is
    // up, in the order they first show (a frame waits for its own). A cut
    // the cut nests loads whole.
    let root = &loaded.tree.root;
    loaded.graphics = crate::graphic::sources(root, resolver)?;
    let scans = crate::graphic::scans(root);
    let first = 1.0 / root.output.fps.max(1.0);
    let (now, mut later): (Vec<usize>, Vec<usize>) = (0..scans.len()).partition(|&i| scans[i].first_use < first);
    later.sort_by(|a, b| scans[*a].first_use.total_cmp(&scans[*b].first_use));
    let found = std::sync::Mutex::new(Vec::new());
    crate::graphic::scan_all(root, &scans, &now, &loaded.graphics, shared, &|_, used| found.lock().unwrap_or_else(|e| e.into_inner()).push(used));
    for used in found.into_inner().unwrap_or_else(|e| e.into_inner()) {
        loaded.graphics.merge(used);
    }
    for doc in loaded.tree.edits.values() {
        loaded.graphics.merge(crate::graphic::resolve_sources(doc, shared)?);
    }
    loaded.later = later.iter().map(|&i| scans[i].clone()).collect();
    note(format!("motion documents ({} sources, {} loaded, {} ahead)", loaded.graphics.sources.len(), now.len(), later.len()), &mut lap);
    for (path, node) in &nodes {
        let src = match &node.kind {
            // A compound clip's source is a cut, not media.
            CutNodeKind::Clip { nested: true, .. } => continue,
            // A song's aligned lyrics: its sung words, for graphics (and
            // then its sound, below).
            CutNodeKind::AudioClip { src: src @ MediaRef::Asset(song), .. } => {
                if !loaded.lyrics.contains_key(song) {
                    match resolver.lyrics(song) {
                        Ok(Some(json)) => {
                            loaded.lyrics.insert(song.clone(), json);
                        }
                        Ok(None) => {}
                        Err(error) => log!("edit render: `{}`: its lyrics: {error}", node.id),
                    }
                }
                src
            }
            CutNodeKind::Clip { src, .. } | CutNodeKind::Still { src, .. } | CutNodeKind::AudioClip { src, .. } => src,
            // A library VjEffect revision (an Fx, or a transition since
            // L8's round 7): its Splash source, compiled like a preset.
            CutNodeKind::Fx { effect: EffectRef::Asset(asset), .. } | CutNodeKind::Transition { effect_asset: Some(asset), .. } => {
                loaded.effects.insert(path.clone(), library_effect_source(resolver, asset).map_err(|e| format!("`{path}`: {e}"))?);
                continue;
            }
            CutNodeKind::VoiceOver { lines, .. } => {
                for take in lines.iter().filter_map(|line| line.take.as_ref()) {
                    if let Some(info) = resolver.take_info(take).map_err(|e| format!("`{}`: {e}", node.id))? {
                        loaded.takes.insert(take.clone(), info);
                    }
                }
                continue;
            }
            CutNodeKind::Subtitles { transcript: Some(transcript), .. } => {
                if let Some(lines) = resolver.transcript(transcript).map_err(|e| format!("`{}`: {e}", node.id))? {
                    loaded.transcripts.insert(transcript.clone(), lines);
                }
                continue;
            }
            _ => continue,
        };
        if loaded.media.contains_key(src) {
            continue;
        }
        if matches!(src, MediaRef::MissingOptional(_)) { continue; }
        let resolved = resolver.resolve(src).map_err(|e| format!("`{}`: {e}", node.id))?;
        if resolved.kind == MediaKind::Image {
            // Only the header now; the pixels decode near their span.
            let bytes = makepad_asset_data::source::read(&resolved.path).map_err(|e| format!("`{}`: {e}", node.id))?;
            let size = image_dims(&bytes, resolved.media).ok_or_else(|| format!("`{}`: not a readable image", node.id))?;
            loaded.sizes.insert(src.clone(), size);
        }
        loaded.media.insert(src.clone(), resolved);
    }
    note(format!("media ({} resolved)", loaded.media.len()), &mut lap);
    // A Generate's chosen take (lane G1) plays as a source of its
    // own, in the cut and in every cut it nests.
    let takes: Vec<MediaRef> = std::iter::once(&loaded.tree.root).chain(loaded.tree.edits.values())
        .flat_map(makepad_story_cut::generate::generate_media).collect();
    for take in takes {
        if !loaded.media.contains_key(&take) {
            let resolved = resolver.resolve(&take).map_err(|e| format!("a generated take: {e}"))?;
            loaded.media.insert(take, resolved);
        }
    }
    note("generated takes".into(), &mut lap);
    // Songs, analysed for the graphics that read them (below): a render
    // decodes its sound first, as they hear it.
    let reads_music = loaded.graphics.sources.values().any(|source| source.contains("music("));
    let reads_sound = loaded.graphics.sources.values().any(|source| source.contains("KineticText") || source.contains("vjfx/"));
    if audio && (reads_music || reads_sound) {
        let mut eval = loaded.tree.eval();
        for (run, marks) in marks {
            eval.set_marks(run.clone(), marks.clone());
        }
        for (take, info) in &loaded.takes {
            eval.set_take(take.clone(), info.clone());
        }
        loaded.pcm = decode_sounds(&eval, doc.duration, &mut loaded.media, Some(resolver))?;
        note(format!("sound ({} sources)", loaded.pcm.len()), &mut lap);
    }
    // Songs, analysed for the graphics that read them (`music(@clip)`): the
    // edit's own song clips, when one of its documents reads music. Only
    // the part the clip plays, with some context for the beat tracker.
    // Kinetic type and vj_fx effects read the song's picture (spectrum,
    // waveform, onsets) at each frame's time: the same window, analysed
    // hop by hop like the live bus (makepad-audio-reactive AudioTrack).
    if reads_music || reads_sound {
        const CONTEXT: f64 = 8.0;
        let eval = loaded.tree.eval();
        for node in &doc.nodes {
            let (CutNodeKind::AudioClip { src, .. }, Some(span)) = (&node.kind, node.span) else { continue };
            let (Some(from), Some(until)) = (eval.source_time(node, span.start), eval.source_time(node, span.end())) else { continue };
            let (from, until) = ((from - CONTEXT).max(0.0), until + CONTEXT);
            let pcm = match loaded.pcm.get(src) {
                Some(pcm) => pcm.clone(),
                None => {
                    let Some(resolved) = loaded.media.get(src) else { continue };
                    match decode_sound(resolved, until) {
                        Ok(pcm) => Arc::new(pcm),
                        Err(error) => {
                            log!("edit render: `{}`: graphics hear no music: {error}", node.id);
                            continue;
                        }
                    }
                }
            };
            let rate = pcm.sample_rate.max(1) as f64;
            let window = pcm.frames.get((from * rate) as usize..((until * rate) as usize).min(pcm.frames.len())).unwrap_or(&[]);
            let mono: Arc<[f32]> = window.iter().map(|[l, r]| (*l as f32 + *r as f32) * (0.5 / 32768.0)).collect();
            if reads_music {
                let features = makepad_story_audio_features::analyze_music(&mono, rate);
                // With the sound itself: `music(@m).spectrum(t, n)` and `.wave(…)`.
                let song = makepad_story_audio_features::to_motion::music_with_sound(&features, mono.clone(), rate);
                loaded.music.insert(node.id.clone(), song.shifted(from));
            }
            if reads_sound {
                // Source second `from` is window sample 0; the clip plays
                // source `source_start` at edit `span.start`.
                if let Some(source_start) = eval.source_time(node, span.start) {
                    let track = makepad_audio_reactive::AudioTrack::analyse(&mono, rate as f32, from + span.start - source_start);
                    loaded.tracks.insert(node.id.clone(), Arc::new(track));
                }
            }
        }
        note(format!("music for graphics ({} analysed, {} pictures)", loaded.music.len(), loaded.tracks.len()), &mut lap);
    }
    drop(note);
    loaded.timings = format!("{} (load {:.0} ms)", timings.join(" · "), (Cx::monotonic_now() - started) * 1000.0);
    Ok(loaded)
}

/// App runs' recorded data: their marks place `marker()` cuts, and
/// graphics bound to a run follow it. With the marks in effect: the job's
/// `marks`, else the run's own (a render without run.json, or a resolver
/// that cannot read it, keeps the job's only).
fn run_data(nodes: &[(String, CutNode)], marks: &HashMap<RunRef, HashMap<String, f64>>, resolver: &mut dyn MediaResolver) -> (HashMap<RunRef, Arc<RunData>>, HashMap<RunRef, HashMap<String, f64>>) {
    let mut runs: HashMap<RunRef, Arc<RunData>> = HashMap::new();
    let mut all_marks = marks.clone();
    for (_, node) in nodes {
        let (CutNodeKind::Clip { src: MediaRef::Run(run), .. } | CutNodeKind::AudioClip { src: MediaRef::Run(run), .. }) = &node.kind else { continue };
        if runs.contains_key(run) {
            continue;
        }
        match resolver.run_data(run) {
            Ok(Some(bytes)) => match RunData::parse(&bytes) {
                Ok(data) => {
                    all_marks.entry(run.clone()).or_insert_with(|| data.mark_seconds().into_iter().collect());
                    runs.insert(run.clone(), Arc::new(data));
                }
                Err(error) => log!("edit render: `{}`: its run.json does not read: {error}", node.id),
            },
            Ok(None) => {}
            Err(error) => log!("edit render: `{}`: its run.json: {error}", node.id),
        }
    }
    (runs, all_marks)
}

/// The first graphic of `doc` in `pending` shown within a frame of `t`
/// (`frame`: one frame's seconds), or always shown.
pub fn waiting_graphic(doc: &CutDoc, pending: &HashSet<String>, t: f64, frame: f64) -> Option<String> {
    doc.graphics.iter().filter(|graphic| pending.contains(&graphic.id)).find(|graphic| match graphic.span {
        Some(span) => span.start < t + 2.0 * frame && span.start + span.length > t - frame,
        None => true,
    }).map(|graphic| graphic.id.clone())
}

/// A render's sound ([`Sound`]): every source its mix reads, decoded up to
/// the furthest second it is read at (`decoded`: already, for the graphics
/// that hear its songs), and the gain that brings the whole mix to the
/// cut's loudness (`output.loudness`; 1 without one).
fn sound(doc: Arc<CutDoc>, tree: Arc<CutTree>, marks: HashMap<RunRef, HashMap<String, f64>>, takes: HashMap<AssetRef, TakeInfo>, media: &mut HashMap<MediaRef, ResolvedMedia>, decoded: Option<HashMap<MediaRef, Arc<TrackPcm>>>, resolver: Option<Arc<std::sync::Mutex<Box<dyn MediaResolver>>>>) -> Result<(HashMap<MediaRef, Arc<TrackPcm>>, f64), String> {
    let started = Cx::monotonic_now();
    let pcm = match decoded {
        Some(pcm) => pcm,
        None => {
            let mut eval = tree.eval();
            for (run, marks) in &marks {
                eval.set_marks(run.clone(), marks.clone());
            }
            for (take, info) in &takes {
                eval.set_take(take.clone(), info.clone());
            }
            let mut shared = resolver.as_deref().map(crate::graphic::Shared);
            decode_sounds(&eval, doc.duration, media, shared.as_mut().map(|shared| shared as &mut dyn MediaResolver))?
        }
    };
    let mut gain = 1.0;
    if doc.output.loudness.is_some() {
        let fps = crate::audio::fps_ratio(doc.output.fps);
        let samples = crate::audio::frame_sample(crate::audio::frame_count(doc.duration, fps), fps);
        let mut mix = AudioMix::new(doc.clone(), pcm.clone());
        mix.tree = Some(tree);
        mix.marks = marks;
        mix.takes = takes;
        if let Some((measured, capped)) = mix.normalize(samples) {
            let db = 20.0 * mix.gain.log10();
            if capped {
                log!("edit render: mix measured {measured:.1} LUFS; gain held to {db:.2} dB so peaks stay under -1 dBFS");
            } else {
                log!("edit render: mix measured {measured:.1} LUFS, gain {db:.2} dB");
            }
        }
        gain = mix.gain;
    }
    log!("edit render: sound ({} sources) {:.0} ms", pcm.len(), (Cx::monotonic_now() - started) * 1000.0);
    Ok((pcm, gain))
}

/// The cut's mix of the decoded `pcm`, at the cut's loudness when it
/// asks for one.
fn edit_mix(doc: Arc<CutDoc>, tree: Arc<CutTree>, marks: HashMap<RunRef, HashMap<String, f64>>, takes: HashMap<AssetRef, TakeInfo>, pcm: HashMap<MediaRef, Arc<TrackPcm>>) -> AudioMix {
    let mut mix = AudioMix::new(doc.clone(), pcm);
    mix.tree = Some(tree);
    mix.marks = marks;
    mix.takes = takes;
    if doc.output.loudness.is_some() {
        let fps = crate::audio::fps_ratio(doc.output.fps);
        mix.normalize(crate::audio::frame_sample(crate::audio::frame_count(doc.duration, fps), fps));
    }
    mix
}

/// The cut's sound alone, for a host that plays it with no picture (the
/// motion desk, under a graphic of the cut): the cut and what it nests,
/// its runs' marks and voice takes, and every source its audio plan reads,
/// decoded and mixed as [`CutEngine::sound_job`] mixes them. It blocks
/// (it reads and decodes): run it on the pool.
pub fn sound_mix(doc: Arc<CutDoc>, resolver: &mut dyn MediaResolver) -> Result<AudioMix, String> {
    let tree = CutTree::from_doc((*doc).clone(), None, &mut |edit| resolver.cut_source(edit))
        .map_err(|errors| errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "))?;
    let nodes: Vec<(String, CutNode)> = tree_nodes(&tree).into_iter().map(|(path, node)| (path, node.clone())).collect();
    let (_, marks) = run_data(&nodes, &HashMap::new(), resolver);
    let mut takes = HashMap::new();
    for (_, node) in &nodes {
        let CutNodeKind::VoiceOver { lines, .. } = &node.kind else { continue };
        for take in lines.iter().filter_map(|line| line.take.as_ref()) {
            if let Some(info) = resolver.take_info(take).map_err(|e| format!("`{}`: {e}", node.id))? {
                takes.insert(take.clone(), info);
            }
        }
    }
    let tree = Arc::new(tree);
    let mut eval = tree.eval();
    for (run, marks) in &marks {
        eval.set_marks(run.clone(), marks.clone());
    }
    for (take, info) in &takes {
        eval.set_take(take.clone(), info.clone());
    }
    let pcm = decode_sounds(&eval, doc.duration, &mut HashMap::new(), Some(resolver))?;
    drop(eval);
    Ok(edit_mix(doc, tree, marks, takes, pcm))
}

/// The decoded sound of every source the cut's audio plan reads, each up
/// to the furthest second it is read at. A source not in `media` is
/// resolved through `resolver`, or (without one) plays silent. A video
/// without a soundtrack plays silent.
pub fn decode_sounds(
    eval: &CutEval<'_>,
    duration: f64,
    media: &mut HashMap<MediaRef, ResolvedMedia>,
    mut resolver: Option<&mut dyn MediaResolver>,
) -> Result<HashMap<MediaRef, Arc<TrackPcm>>, String> {
    let mut reach: HashMap<MediaRef, f64> = HashMap::new();
    for segment in eval.audio_plan(0.0, duration).segments {
        if let Some(start) = segment.source_t0 {
            let end = start + (segment.t1 - segment.t0) * segment.speed;
            let entry = reach.entry(segment.media).or_insert(0.0);
            *entry = entry.max(end);
        }
    }
    let mut pcm = HashMap::new();
    for (source, seconds) in reach {
        // A voiceover's takes are named only by the plan.
        if !media.contains_key(&source) {
            let Some(resolver) = resolver.as_deref_mut() else {
                log!("edit render: {source:?} plays silent in the preview (not resolved)");
                continue;
            };
            let resolved = resolver.resolve(&source).map_err(|e| format!("audio: {e}"))?;
            media.insert(source.clone(), resolved);
        }
        let resolved = &media[&source];
        match decode_sound(resolved, seconds) {
            Ok(decoded) => {
                pcm.insert(source, Arc::new(decoded));
            }
            Err(error) if resolved.kind == MediaKind::Video => log!("edit render: {} plays silent: {error}", resolved.path.display()),
            Err(error) => return Err(format!("{}: {error}", resolved.path.display())),
        }
    }
    Ok(pcm)
}

/// The Splash source of library VjEffect revision `asset`, fetched once per
/// revision for the process (a revision never changes; every new preview
/// renderer of a cut would fetch it again otherwise).
fn library_effect_source(resolver: &mut dyn MediaResolver, asset: &AssetRef) -> Result<String, String> {
    static SOURCES: std::sync::OnceLock<std::sync::Mutex<HashMap<AssetRef, String>>> = std::sync::OnceLock::new();
    let sources = SOURCES.get_or_init(Default::default);
    if let Some(source) = sources.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(asset) {
        return Ok(source.clone());
    }
    let source = resolver.effect_source(asset)?;
    sources.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(asset.clone(), source.clone());
    Ok(source)
}

/// A source's sound up to `seconds` (a little past, for the resampler).
/// Containers stop decoding there; the other formats decode whole.
fn decode_sound(resolved: &ResolvedMedia, seconds: f64) -> Result<TrackPcm, String> {
    if resolved.media != MediaType::Mp4 {
        return decode_audio_clip(&resolved.path, resolved.media, usize::MAX);
    }
    let (_input, mut decoder) = open_container_audio(&resolved.path)?;
    let mut frames: Vec<[i16; 2]> = Vec::new();
    let mut sample_rate = decoder.info().audio_sample_rate.max(1);
    while let Some(chunk) = decoder.next_audio().map_err(|e| e.to_string())? {
        sample_rate = chunk.sample_rate.max(1);
        let channels = chunk.channels.max(1) as usize;
        for frame in chunk.samples.chunks_exact(channels) {
            let pair = if channels == 1 { [frame[0], frame[0]] } else { [frame[0], frame[1]] };
            frames.push(pair);
        }
        if frames.len() as f64 > (seconds + 0.1) * sample_rate as f64 {
            break;
        }
    }
    if frames.is_empty() {
        return Err("no sound".into());
    }
    Ok(TrackPcm { frames, sample_rate })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn visible_clips_are_admitted_before_preroll_ones_whatever_the_order() {
        let visible = ids(&["now0", "now1", "now2", "now3", "now4", "mid/deep/v"]);
        let soon = ids(&["later0", "later1", "later2"]);
        let keep = plan_loads(&visible, &soon, 8, false);
        assert_eq!(&keep[..6], &visible[..]);
        assert_eq!(&keep[6..], &["later0", "later1"]);
        // While a visible clip waits for a decoder, no preroll load is held.
        assert_eq!(plan_loads(&visible, &soon, 8, true), visible);
        // A smaller budget holds the visible ones first.
        assert_eq!(plan_loads(&visible, &soon, 4, false), &visible[..4]);
        // A clip both visible and coming is held once.
        assert_eq!(plan_loads(&ids(&["a"]), &ids(&["a", "b"]), 8, false), ["a", "b"]);
    }

    const A: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
    const A_REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";
    const MID: &str = "ast_fklvc3bvjnuijdg33d2uuitkbi";
    const MID_REV: &str = "arev_e2e0b2ddc168547400e68147bf9e0161866f9cad405c9f94a2a47a5dcfe955ac";
    const DEEP: &str = "ast_qdyytgcolstqfb6rgnbpnwva3m";
    const DEEP_REV: &str = "arev_b07daf865cbe8c601af1644e4b1168662f5faad1224b8806370e61e13b2270c2";

    fn edit(body: &str, nodes: &str) -> String {
        format!("use mod.flow.*\nuse mod.cut.*\n{body}\nFlow{{\n kind: @cut\n output: {{width: 64 height: 36 fps: 30}}\n duration: 2.0\n {nodes}\n}}\n")
    }

    fn tree(outer: &str, inner: &str) -> CutTree {
        let deep = edit(
            &format!("let v = Clip{{src: asset(\"{A}\", \"{A_REV}\")}}\nlet subs = Subtitles{{source: v.out(@audio)}}\nlet s = Stack{{layers: [v.out(@frame), subs.out(@frame)]}}\nlet out = CutOutput{{frame: s.out(@frame)}}"),
            "v, subs, s, out",
        );
        let mid = edit(&format!("let deep = Clip{{src: cut(\"{DEEP}\", \"{DEEP_REV}\") {inner}}}\nlet out = CutOutput{{frame: deep.out(@frame)}}"), "deep, out");
        let root = edit(&format!("let mid = Clip{{src: cut(\"{MID}\", \"{MID_REV}\") {outer}}}\nlet out = CutOutput{{frame: mid.out(@frame)}}"), "mid, out");
        CutTree::load(&root, None, &mut |edit| Ok(if edit.id == MID { mid.clone() } else { deep.clone() }))
            .unwrap_or_else(|errors| panic!("{errors:?}"))
    }

    #[test]
    fn nested_nodes_are_named_by_path() {
        let tree = tree("", "");
        let paths: Vec<String> = tree_nodes(&tree).into_iter().map(|(path, _)| path).collect();
        for path in ["mid/deep/v", "mid/deep/subs", "mid/deep", "mid", "out"] {
            assert!(paths.iter().any(|p| p == path), "{path} missing from {paths:?}");
        }
        assert!(node_at(&tree, "mid/deep/v").is_some());
        assert!(node_at(&tree, "mid/nope").is_none());
    }

    #[test]
    fn nested_captions_need_every_parent_to_opt_in() {
        assert!(nested_captions_shown(&tree("", ""), "subs"));
        assert!(!nested_captions_shown(&tree("", ""), "mid/deep/subs"));
        assert!(!nested_captions_shown(&tree("subtitles: 1", ""), "mid/deep/subs"));
        assert!(nested_captions_shown(&tree("subtitles: 1", "subtitles: 1"), "mid/deep/subs"));
    }

    #[test]
    fn a_nested_caption_maps_out_through_its_clip() {
        let tree = tree("", "");
        let clip = tree.root.node("mid").unwrap().clone();
        // `mid` plays from in 0 over the whole edit, speed 1: unchanged.
        let caption = Caption { start: 0.2, end: 0.9, words: vec![Word { text: "hi".into(), start: 0.2, end: 0.9 }], provisional: false };
        assert_eq!(caption_through(&caption, &clip), Some(caption.clone()));
        // At span [1.0, 0.5] from in 0.1: shifted by 0.9 and clipped at 1.5.
        let mut moved = clip.clone();
        if let CutNodeKind::Clip { source_in, .. } = &mut moved.kind {
            *source_in = SourceTime::Seconds(0.1);
        }
        moved.span = Some(makepad_story_cut::Span { start: 1.0, length: 0.5 });
        let out = caption_through(&caption, &moved).unwrap();
        assert!((out.start - 1.1).abs() < 1e-9 && (out.end - 1.5).abs() < 1e-9, "{out:?}");
        assert!(caption_through(&Caption { start: 1.0, end: 1.5, ..caption.clone() }, &moved).is_none());
    }

}

/// The layers under slot `g` in the Stack that shows it (bottom first);
/// none when no Stack does.
fn below(plan: &FramePlan, g: usize) -> Vec<(usize, Blend)> {
    plan.ops.iter().find_map(|op| match &op.kind {
        DrawKind::Stack { layers } => layers.iter().position(|(slot, _)| *slot == g).map(|at| layers[..at].to_vec()),
        _ => None,
    }).unwrap_or_default()
}

/// The slots an op reads.
pub(crate) fn slot_inputs(kind: &DrawKind) -> Vec<usize> {
    match kind {
        DrawKind::Effect { input, .. } => input.iter().copied().collect(),
        DrawKind::Transition { a, b, .. } => a.iter().chain(b.iter()).copied().collect(),
        DrawKind::Stack { layers } => layers.iter().map(|(slot, _)| *slot).collect(),
        DrawKind::Reframe { input, .. } => vec![*input],
        _ => Vec::new(),
    }
}

/// `kind` with every slot it reads moved by `by`.
fn shift_slots(kind: DrawKind, by: usize) -> DrawKind {
    match kind {
        DrawKind::Effect { effect, input, local_time, beat, params } => DrawKind::Effect { effect, input: input.map(|slot| slot + by), local_time, beat, params },
        DrawKind::Transition { effect, a, b, mix, local_time, beat, params, effect_asset } => {
            DrawKind::Transition { effect, a: a.map(|slot| slot + by), b: b.map(|slot| slot + by), mix, local_time, beat, params, effect_asset }
        }
        DrawKind::Stack { layers } => DrawKind::Stack { layers: layers.into_iter().map(|(slot, blend)| (slot + by, blend)).collect() },
        DrawKind::Reframe { input, crop, dst_rect } => DrawKind::Reframe { input: input + by, crop, dst_rect },
        other => other,
    }
}

/// A built pack effect as the cut draws it (manual clock, no composite).
pub(crate) fn effect_view(cx: &mut Cx, install: Install, built: BuiltEffect, bpm: f64) -> Box<dyn EffectView> {
    let mut view = install(cx, built);
    view.set_bpm(bpm);
    view
}
