//! Motion graphics in the cut renderer: the compositor's
//! `DrawKind::Graphic` ops (a `Graphic` node's Motion document, or a
//! `Title` lowered to the built-in "title" document) drawn by
//! `makepad_motion_draw::MotionDraw` into the op's texture, on the
//! document's own clock at the op's `local_time`.
//!
//! Documents load once per (source, params, clock offset) and stay; each
//! graphic node draws with its own `MotionDraw` (its passes are its own).
//! A Motion asset's source is resolved on the loader's worker
//! ([`resolve_sources`], through [`crate::media::MediaResolver::motion_source`])
//! and handed in with [`Graphics::set_source`]; images and SVGs the
//! documents use arrive the same way ([`GraphicAssets`]).
//!
//! Musical time and signals follow the cut: without a host context
//! ([`Graphics::set_context`]), beats count at the cut's own `bpm` on its
//! clock.

use crate::media::MediaResolver;
use makepad_motion_draw::{decode_image, MotionDraw};
use makepad_motion::{builtin, BeatGrid, InputValue, Media, MotionContext, MotionDoc, Timing};
use makepad_motion::bind::run::RunData;
use makepad_story_cut::{AssetRef, DrawKind, DrawOp, CutDoc, Fit, GraphicSrc, RunRef};
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;

/// The library files the cut's graphics use, fetched on a worker.
#[derive(Default)]
pub struct GraphicAssets {
    /// Motion documents' sources by revision.
    pub sources: HashMap<AssetRef, String>,
    /// The Splash modules they import (`import(asset(id, rev))`, and what
    /// those import), sources by revision.
    pub modules: HashMap<AssetRef, String>,
    /// Decoded images (BGRA) by revision.
    pub images: HashMap<AssetRef, (Vec<u32>, u32, u32)>,
    /// SVG markup by revision.
    pub svgs: HashMap<AssetRef, String>,
    /// Other files (a full Scene3D's models, HDRs, fonts) by revision.
    pub files: HashMap<AssetRef, Vec<u8>>,
    /// Videos and app-run renders the documents show (`Video` layers, a
    /// Scene3D's `videos:`), resolved like the cut's clips: their frames
    /// come from the cut's own readers.
    pub videos: HashMap<AssetRef, crate::media::ResolvedMedia>,
}

/// What each Motion revision (with its node's params) uses from the
/// library, as its load found it (see [`scan`]).
static USED: std::sync::LazyLock<std::sync::Mutex<HashMap<(AssetRef, String), Vec<makepad_motion::AssetRef>>>> = std::sync::LazyLock::new(Default::default);

impl GraphicAssets {
    pub fn merge(&mut self, other: GraphicAssets) {
        self.sources.extend(other.sources);
        self.modules.extend(other.modules);
        self.images.extend(other.images);
        self.svgs.extend(other.svgs);
        self.files.extend(other.files);
        self.videos.extend(other.videos);
    }
}

/// A resolver several workers share: each call holds it alone.
pub struct Shared<'a>(pub &'a std::sync::Mutex<Box<dyn MediaResolver>>);

impl Shared<'_> {
    fn with<T>(&mut self, f: impl FnOnce(&mut dyn MediaResolver) -> T) -> T {
        f(&mut **self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl MediaResolver for Shared<'_> {
    fn resolve(&mut self, media: &makepad_story_cut::MediaRef) -> Result<crate::media::ResolvedMedia, String> { self.with(|r| r.resolve(media)) }
    fn effect_source(&mut self, effect: &AssetRef) -> Result<String, String> { self.with(|r| r.effect_source(effect)) }
    fn take_info(&mut self, take: &AssetRef) -> Result<Option<makepad_story_cut::TakeInfo>, String> { self.with(|r| r.take_info(take)) }
    fn cut_source(&mut self, edit: &AssetRef) -> Result<String, String> { self.with(|r| r.cut_source(edit)) }
    fn transcript(&mut self, transcript: &AssetRef) -> Result<Option<Vec<makepad_story_cut::SpokenLine>>, String> { self.with(|r| r.transcript(transcript)) }
    fn motion_source(&mut self, motion: &AssetRef) -> Result<String, String> { self.with(|r| r.motion_source(motion)) }
    fn asset_bytes(&mut self, asset: &AssetRef) -> Result<(Vec<u8>, &'static str), String> { self.with(|r| r.asset_bytes(asset)) }
    fn run_data(&mut self, run: &RunRef) -> Result<Option<Vec<u8>>, String> { self.with(|r| r.run_data(run)) }
    fn lyrics(&mut self, song: &AssetRef) -> Result<Option<String>, String> { self.with(|r| r.lyrics(song)) }
}

/// One document load that finds what graphics use from the library: a
/// library Motion document (loaded with its first graphic's params; every
/// graphic of that revision shares what it found) or a tree's plate. `nodes`
/// are the graphics it serves, `first_use` when the first of them shows
/// (seconds on the cut's clock; always shown: 0).
#[derive(Clone, Debug, PartialEq)]
pub struct Scan {
    pub graphic: usize,
    pub nodes: Vec<String>,
    pub first_use: f64,
}

/// A cut's scans, in the order its graphics are written.
pub fn scans(doc: &CutDoc) -> Vec<Scan> {
    let mut scans: Vec<Scan> = Vec::new();
    for (index, graphic) in doc.graphics.iter().enumerate() {
        let first_use = graphic.span.map_or(0.0, |span| span.start.max(0.0));
        let same = match &graphic.src {
            GraphicSrc::Asset(asset) => scans.iter_mut().find(|scan| matches!(&doc.graphics[scan.graphic].src, GraphicSrc::Asset(other) if other == asset)),
            GraphicSrc::Tree(_) => None,
            _ => continue,
        };
        match same {
            Some(scan) => {
                scan.nodes.push(graphic.id.clone());
                scan.first_use = scan.first_use.min(first_use);
            }
            None => scans.push(Scan { graphic: index, nodes: vec![graphic.id.clone()], first_use }),
        }
    }
    scans
}

/// Every library Motion source a cut's graphics name, and the modules
/// they import (file reads; no document loads). A source that cannot be
/// read fails the cut.
pub fn sources(doc: &CutDoc, resolver: &mut dyn MediaResolver) -> Result<GraphicAssets, String> {
    let mut assets = GraphicAssets::default();
    for graphic in &doc.graphics {
        let GraphicSrc::Asset(asset) = &graphic.src else { continue };
        if assets.sources.contains_key(asset) {
            continue;
        }
        let source = resolver.motion_source(asset).map_err(|e| format!("`{}`: {e}", graphic.id))?;
        assets.sources.insert(asset.clone(), source);
    }
    let mut todo: Vec<makepad_motion::AssetRef> = assets.sources.values().flat_map(|s| makepad_motion::modules::asset_imports(s)).collect();
    while let Some(m) = todo.pop() {
        let module = AssetRef { id: m.id, rev: m.rev };
        if assets.modules.contains_key(&module) {
            continue;
        }
        // A missing module is the document's load error, which says how to fix it.
        let Ok(source) = resolver.motion_source(&module) else { continue };
        todo.extend(makepad_motion::modules::asset_imports(&source));
        assets.modules.insert(module, source);
    }
    Ok(assets)
}

/// Load one scan's document and fetch what it uses: pictures (decoded),
/// SVGs, other files, and the videos it shows. `assets` holds the cut's
/// sources and modules ([`sources`]). Blocking: call on a worker.
pub fn scan(doc: &CutDoc, scan: &Scan, assets: &GraphicAssets, modules: &makepad_motion::modules::Modules, resolver: &mut dyn MediaResolver) -> GraphicAssets {
    let graphic = &doc.graphics[scan.graphic];
    let inputs = makepad_story_cut::graphic::motion_inputs(&graphic.params);
    let used = match &graphic.src {
        GraphicSrc::Asset(asset) => {
            // What a revision uses is known once it has loaded: a cut loaded
            // again loads only the documents it has not seen.
            let key = (asset.clone(), format!("{:?}", &graphic.params[..]));
            let known = USED.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
            match known {
                Some(used) => used,
                None => match assets.sources.get(asset) {
                    Some(source) => {
                        let context = MotionContext { modules: modules.clone(), ..Default::default() };
                        match MotionDoc::load_with(source, &inputs, context) {
                            Ok(motion) => {
                                let used = motion.dependencies();
                                USED.lock().unwrap_or_else(|e| e.into_inner()).insert(key, used.clone());
                                used
                            }
                            Err(_) => Vec::new(),
                        }
                    }
                    None => Vec::new(),
                },
            }
        }
        // A plate that does not load (a dangling path, a name that is no
        // font) uses nothing here and says why where it draws, as its own
        // graphic's error.
        GraphicSrc::Tree(tree) => tree.resolve(&tree.document).ok().and_then(|file| {
            let text = file.text().ok()?;
            let context = MotionContext { modules: makepad_motion::modules::Modules::from_tree(tree.clone()), ..Default::default() };
            MotionDoc::load_with(text, &inputs, context).ok().map(|motion| motion.dependencies())
        }).unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut found = GraphicAssets::default();
    for used in used {
        let used = AssetRef { id: used.id, rev: used.rev };
        if found.images.contains_key(&used) || found.svgs.contains_key(&used) || found.files.contains_key(&used) || found.videos.contains_key(&used) {
            continue;
        }
        // A video (or an app run's render) plays through a reader,
        // never as bytes in memory.
        if let Ok(resolved) = resolver.resolve(&makepad_story_cut::MediaRef::Asset(used.clone())) {
            if resolved.kind == crate::media::MediaKind::Video {
                found.videos.insert(used, resolved);
                continue;
            }
        }
        match resolver.asset_bytes(&used) {
            Ok((bytes, kind)) if kind == "svg" => {
                found.svgs.insert(used, String::from_utf8_lossy(&bytes).into_owned());
            }
            Ok((bytes, kind)) => {
                // A full Scene3D may texture with a picture too.
                if matches!(kind, "png" | "jpg") {
                    if let Ok(image) = decode_image(&bytes) {
                        found.images.insert(used.clone(), image);
                    }
                }
                found.files.insert(used, bytes);
            }
            // A document asset the resolver cannot give draws nothing; the draw says so.
            Err(_) => {}
        }
    }
    found
}

/// Run `order` (indices into `scans`) on the compute pool, taken in that
/// order, each result handed to `done` as it is found.
pub fn scan_all(doc: &CutDoc, scans: &[Scan], order: &[usize], assets: &GraphicAssets, resolver: &std::sync::Mutex<Box<dyn MediaResolver>>, done: &(dyn Fn(usize, GraphicAssets) + Sync)) {
    let modules = module_host(&assets.modules);
    let next = std::sync::atomic::AtomicUsize::new(0);
    use makepad_script_compute_sched::sched::Executor;
    let executor = makepad_script_compute_sched::sched::ThreadExecutor::shared();
    executor.fan_out(executor.workers() + 1, &|_| loop {
        let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Some(&index) = order.get(at) else { break };
        let found = scan(doc, &scans[index], assets, &modules, &mut Shared(resolver));
        done(index, found);
    });
}

/// Everything a cut's graphics use from the library at once: their
/// sources and modules and what every document uses.
pub fn resolve_sources(doc: &CutDoc, resolver: &std::sync::Mutex<Box<dyn MediaResolver>>) -> Result<GraphicAssets, String> {
    let mut assets = sources(doc, &mut Shared(resolver))?;
    let scans = scans(doc);
    let order: Vec<usize> = (0..scans.len()).collect();
    let found = std::sync::Mutex::new(Vec::new());
    scan_all(doc, &scans, &order, &assets, resolver, &|_, used| found.lock().unwrap_or_else(|e| e.into_inner()).push(used));
    for used in found.into_inner().unwrap_or_else(|e| e.into_inner()) {
        assets.merge(used);
    }
    Ok(assets)
}

/// The imported modules as a document's host resolves them.
pub fn module_host(modules: &HashMap<AssetRef, String>) -> makepad_motion::modules::Modules {
    makepad_motion::modules::Modules::from_sources(modules.iter().map(|(a, s)| (a.id.as_str(), a.rev.as_str(), s.as_str())))
}

/// How a graphic's document loads in its cut: its params, its clock in
/// the cut (`offset`), what it hears of the cut (its songs' and voices'
/// words and analysis, the cut's beat), the cut's picture under its
/// freezes, its app run, its characters' art. The cut's renderer loads with
/// it, and so can a host loading the same text (the graphic desk), so both
/// have the one document.
#[derive(Clone)]
pub struct FilmLoad {
    pub node: String,
    pub params: Vec<(String, InputValue)>,
    pub context: MotionContext,
    pub offset: f64,
    bound: Option<(Arc<CutDoc>, HashMap<RunRef, Arc<RunData>>, f64)>,
    svgs: HashMap<AssetRef, String>,
    /// What the load depends on, as one number (equal: the same document
    /// from the same text): set by the engine from the cut.
    pub fingerprint: u64,
}

/// A bound graphic's document size by its text, node and params (see [`FilmLoad::load`]).
fn bound_sizes() -> &'static std::sync::Mutex<HashMap<u64, [f64; 2]>> {
    static SIZES: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, [f64; 2]>>> = std::sync::OnceLock::new();
    SIZES.get_or_init(Default::default)
}

impl FilmLoad {
    /// `source` loaded as the cut loads it, its imports from `modules`.
    pub fn load(&self, source: &str, modules: makepad_motion::modules::Modules) -> Result<MotionDoc, Vec<makepad_motion::Diagnostic>> {
        // Freezes show the cut's picture (`Need::CutFrame`).
        let context = MotionContext { offset: self.offset, edit_frames: true, modules, ..self.context.clone() };
        let params = makepad_story_cut::graphic::motion_inputs(&self.params);
        // A bound graphic loads with its run placed by the cut, which needs the document's size:
        // known from an earlier load of the same document, it loads
        // once; the first time it loads twice (to learn the size), and a size that changed
        // loads again.
        let doc = match &self.bound {
            Some((edit, runs, fps)) => {
                let bound_load = |size: [f64; 2]| {
                    makepad_story_cut::bind::graphic_runs(edit, &self.node, size, runs, *fps)
                        .map(|runs| MotionDoc::load_with(source, &params, runs.apply(context.clone())))
                };
                // the size follows the text and the params (checked below: a size that differs
                // loads again)
                let key = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    source.hash(&mut h);
                    self.node.hash(&mut h);
                    format!("{:?}", self.params).hash(&mut h);
                    h.finish()
                };
                let known = bound_sizes().lock().ok().and_then(|sizes| sizes.get(&key).copied());
                let first = match known {
                    Some(size) => match bound_load(size) {
                        Some(Ok(doc)) if doc.size == size => Some(Ok(doc)),
                        _ => None,
                    },
                    None => None,
                };
                match first {
                    Some(doc) => doc,
                    None => {
                        let plain = MotionDoc::load_with(source, &params, context.clone());
                        match &plain {
                            Ok(first) => {
                                if let Ok(mut sizes) = bound_sizes().lock() {
                                    sizes.insert(key, first.size);
                                }
                                bound_load(first.size).unwrap_or(plain)
                            }
                            Err(_) => plain,
                        }
                    }
                }
            }
            None => MotionDoc::load_with(source, &params, context.clone()),
        };
        // Characters drawn from library SVGs (`Character{src: asset(…)}`)
        // are built from the markup the cut fetched.
        doc.map(|mut doc| {
            for problem in doc.resolve_characters(&|asset| self.svgs.get(&AssetRef { id: asset.id.clone(), rev: asset.rev.clone() }).cloned()) {
                log!("edit render: {problem}");
            }
            doc
        })
    }
}

/// The cut's voiceovers and songs as graphics hear them: each
/// VoiceOver node's placed lines and word times, on the cut's clock, so
/// `word(@vo, …)`, `line(@vo, …)` and `voice(@vo)` follow the takes; each
/// song clip's sung words (`voice(@clip)`) and its analysis (`music(@clip)`:
/// beats, onsets, energy, bands, sections) where the clip places the track.
/// `None` when the cut has neither (graphics keep the cut's beat). The
/// edit's `bpm` stays the grid of `beat(n)`.
pub fn voice_context(doc: &CutDoc, eval: &makepad_story_cut::CutEval<'_>, lyrics: &HashMap<AssetRef, String>, music: &HashMap<String, makepad_motion::MusicFeatures>) -> Option<MotionContext> {
    use makepad_motion::{AudioFeatures, FeatureWord, VoiceFeatures};
    let mut features = AudioFeatures::default();
    // A song with aligned lyrics is a voice too (`voice(@<clip id>)`): its
    // sung words on the cut's clock, where the clip places the track.
    for node in &doc.nodes {
        let (makepad_story_cut::CutNodeKind::AudioClip { src: makepad_story_cut::MediaRef::Asset(song), .. }, Some(span)) = (&node.kind, node.span) else { continue };
        let Some(json) = lyrics.get(song) else { continue };
        let Some(source) = eval.source_time(node, span.start) else { continue };
        match makepad_motion::lyrics::voice_from_lyrics(json, span.start - source) {
            Ok(voice) => features.voices.push((node.id.clone(), voice)),
            Err(error) => log!("edit render: `{}`: its lyrics: {error}", node.id),
        }
    }
    for node in &doc.nodes {
        let (makepad_story_cut::CutNodeKind::AudioClip { .. }, Some(span)) = (&node.kind, node.span) else { continue };
        let (Some(song), Some(source)) = (music.get(&node.id), eval.source_time(node, span.start)) else { continue };
        features.music.push((node.id.clone(), song.shifted(span.start - source)));
    }
    for node in doc.nodes.iter().filter(|node| matches!(node.kind, makepad_story_cut::CutNodeKind::VoiceOver { .. })) {
        let mut voice = VoiceFeatures::default();
        for (index, line) in eval.voice_lines(&node.id).iter().enumerate() {
            voice.lines.push((line.start, line.end));
            voice.words.extend(line.words.iter().map(|word| FeatureWord { text: word.text.clone(), start: word.start, end: word.end, line: index as u32, ..Default::default() }));
            // The take's phonemes (a Kokoro take times them): lip sync.
            voice.phones.extend(line.phones.iter().map(|p| makepad_motion::FeaturePhone { text: p.text.clone(), start: p.start, end: p.end }));
        }
        features.voices.push((node.id.clone(), voice));
    }
    if features.voices.is_empty() && features.music.is_empty() {
        return None;
    }
    let mut context = MotionContext::analyzed(features, None, 0.0);
    context.timing.grid = Some(BeatGrid::steady(doc.bpm, 0.0));
    Some(context)
}

/// What a document key is (see [`Graphics::key_of`]).
struct KeyMeta {
    key2: u64,
    offset: f64,
    src: Option<AssetRef>,
}

/// Frames a graphic may go undrawn before it gives its GPU targets back.
const GRAPHIC_IDLE_FRAMES: u32 = 3;

/// The node whose document an op draws: a Freeze's picture draws a node's
/// graphic again at another time (`<node>@freeze<k>`, see the engine's
/// frozen plan), with its own drawing but the node's one document (already
/// loaded with the cut, not parsed a second time).
fn doc_node(node: &str) -> &str {
    node.rsplit_once("@freeze").map_or(node, |(base, _)| base)
}

pub struct Graphics {
    draws: HashMap<String, MotionDraw>,
    docs: HashMap<u64, Result<MotionDoc, String>>,
    /// Documents loading on the pool ahead of the frame that draws them
    /// ([`Self::prepare`]), by the same key as `docs`.
    loading: HashMap<u64, FrameJob<Result<MotionDoc, String>>>,
    sources: HashMap<AssetRef, String>,
    /// The modules the documents import, and their resolver.
    modules: HashMap<AssetRef, String>,
    module_host: makepad_motion::modules::Modules,
    images: HashMap<AssetRef, (Vec<u32>, u32, u32)>,
    svgs: HashMap<AssetRef, String>,
    files: HashMap<AssetRef, Vec<u8>>,
    videos: HashMap<AssetRef, crate::media::ResolvedMedia>,
    context: Option<MotionContext>,
    /// The song clips' analysed pictures by clip id, on the cut's clock:
    /// what kinetic type and vj_fx effects read as the sound.
    tracks: HashMap<String, Arc<makepad_audio_reactive::AudioTrack>>,
    /// The cut and the recorded app runs (each render's run.json), for
    /// graphics bound to a run (`bind:`, video_cut::bind).
    bound: Option<(Arc<CutDoc>, HashMap<RunRef, Arc<RunData>>, f64)>,
    /// Each node's frame at its last evaluated time (asked for its videos
    /// first, then drawn: evaluated once).
    frames: HashMap<String, (u64, Option<u64>, Option<[u64; 2]>, makepad_motion::MotionFrame)>,
    /// The presenter's pointer over the picture (fractions 0..1), for the
    /// documents that declare `pointer_x` / `pointer_y`.
    pointer: Option<[f64; 2]>,
    /// The host's ambient clock. Without one, Motion uses its story time.
    live: Option<f64>,
    /// The graphic nodes drawn in the last recorded frame.
    drawn: Vec<String>,
    /// Those of them drawn not exact (a shutter still gathering, a glyph or
    /// shader not ready): a picture made from them is not final yet.
    drawn_inexact: std::collections::HashSet<String>,
    /// Nodes whose Freezes show pictures not final yet, for their next draw
    /// ([`Self::set_pictures_pending`]).
    pictures_pending: HashMap<String, String>,
    /// Each node's document as last drawn: whether every frame of it is the
    /// same picture (`MotionDoc::is_time_independent`). A standin or a
    /// document that did not load is not.
    timeless: HashMap<String, bool>,
    /// Frames in a row each graphic was not drawn (see `begin_frame`).
    idle: HashMap<String, u32>,
    exact: bool,
    /// Locked time: see [`Graphics::set_locked_time`].
    locked_time: bool,
    /// Documents load as an editor's: see [`Graphics::set_editable`].
    editable: bool,
    /// Documents patched since they were last read ([`Self::literals_patched`]).
    stale: std::collections::HashSet<u64>,
    /// What each document key is: its node's key without the clock offset,
    /// the offset, the Motion asset it loads.
    meta: HashMap<u64, KeyMeta>,
    /// While editable: a node's document as it ran before its text changed
    /// ([`Self::replace_texts`]), by the key without offset, with its offset;
    /// it stands in (on the node's clock) until the new one has loaded.
    standins: HashMap<u64, (f64, MotionDoc)>,
    /// Documents the host has loaded already, by node ([`Self::give`]):
    /// drawn as they are, never loaded again here.
    given: HashMap<String, MotionDoc>,
    /// A held recolour's copies in each document: (key, colour as loaded)
    /// -> the places holding it, and the colour they hold now.
    copies: HashMap<(u64, [u32; 4]), (makepad_motion::schema::ColourData, [f32; 4])>,
    errors: Vec<String>,
    /// The language the documents render in ([`Self::set_lang`]).
    lang: Option<Arc<str>>,
    /// Scene coverage maps are drawn ([`Self::set_scene_coverage`]).
    scene_coverage: bool,
}

/// The pointer a cached frame was evaluated with: kept only by documents
/// that declare `pointer_x` / `pointer_y`; all others stay cached whatever
/// the pointer does.
fn pointer_key(declares: bool, pointer: Option<[f64; 2]>) -> Option<[u64; 2]> {
    let [x, y] = pointer.unwrap_or([-1.0, -1.0]);
    declares.then_some([x.to_bits(), y.to_bits()])
}

/// Whether a cached frame (evaluated with `at`; `None` for a document that
/// does not read the pointer) still stands for `pointer`.
fn pointer_current(at: Option<[u64; 2]>, pointer: Option<[f64; 2]>) -> bool {
    at.is_none_or(|at| Some(at) == pointer_key(true, pointer))
}

impl Default for Graphics {
    fn default() -> Self {
        Self::new()
    }
}

impl Graphics {
    /// Inspect an already loaded graphic on demand. Uses its existing draw for
    /// glyph bounds; it creates no renderer, pass or animation request.
    pub fn inspect(&mut self, cx: &mut Cx2d, op: &DrawOp, placement: [f64; 4], clip: [f64; 4]) -> Option<GraphicInspection> {
        let DrawKind::Graphic { src, params, offset, bpm, local_time, fit, .. } = &op.kind else { return None };
        let key = self.key_of(&op.node, src, params, *offset, *bpm);
        let doc = self.docs.get(&key)?.as_ref().ok()?.clone();
        let frame = doc.eval_traced_pointer(*local_time, self.live.unwrap_or(*local_time), self.pointer);
        let draw = self.draws.get_mut(&op.node)?;
        let mut boxes = Vec::new();
        for (_, item) in frame.walk() {
            if let Some(b) = draw.item_bounds(cx, item) {
                boxes.push((item.path.clone(), [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]]));
            }
        }
        Some(GraphicInspection { node: op.node.clone(), source: src.clone(), fit: *fit, size: op.size, placement, clip, doc, frame, boxes })
    }

    pub fn new() -> Self {
        Self {
            draws: HashMap::new(), docs: HashMap::new(), loading: HashMap::new(), sources: HashMap::new(), modules: HashMap::new(), module_host: Default::default(), images: HashMap::new(), svgs: HashMap::new(), files: HashMap::new(), videos: HashMap::new(),
            context: None, tracks: HashMap::new(), bound: None, frames: HashMap::new(), pointer: None, live: None, drawn: Vec::new(), drawn_inexact: Default::default(), pictures_pending: HashMap::new(), timeless: HashMap::new(), idle: HashMap::new(), exact: true, locked_time: false, editable: false, stale: Default::default(), meta: HashMap::new(), standins: HashMap::new(), given: HashMap::new(), copies: HashMap::new(), errors: Vec::new(), lang: None, scene_coverage: false,
        }
    }

    /// Render the documents in a language (`lang()`; an output's own
    /// render of the cut); the documents load again with it.
    pub fn set_lang(&mut self, lang: Option<Arc<str>>) {
        if self.lang == lang {
            return;
        }
        self.lang = lang;
        self.docs.clear();
        self.timeless.clear();
        self.loading.clear();
        self.frames.clear();
    }

    pub fn set_live(&mut self, live: f64) {
        self.live = Some(live);
    }

    /// Draw every Scene3D's coverage id map with the graphics' frames (an
    /// inspect grab; see `MotionDraw::set_scene_coverage`).
    pub fn set_scene_coverage(&mut self, on: bool) {
        self.scene_coverage = on;
    }

    /// The coverage maps of the Scene3D layers the last recorded frame
    /// drew, with the graphic node each is in.
    pub fn scene_coverage(&self) -> Vec<(String, makepad_motion_draw::coverage::SceneCoverage)> {
        self.drawn.iter().filter_map(|node| Some((node, self.draws.get(node)?))).flat_map(|(node, draw)| draw.scene_coverage().into_iter().map(|c| (node.clone(), c))).collect()
    }

    /// Only the graphics visible in the last frame keep a held cut awake.
    pub fn reads_live(&self) -> bool {
        self.drawn.iter().any(|node| self.frames.get(node).is_some_and(|(_, live, _, _)| live.is_some()))
    }

    /// Whether each of `nodes` has been drawn with a loaded document whose
    /// every frame is the same picture. A node not drawn yet is not known to
    /// be.
    pub fn all_timeless<'a>(&self, mut nodes: impl Iterator<Item = &'a str>) -> bool {
        nodes.all(|node| self.timeless.get(node).copied().unwrap_or(false))
    }

    /// The presenter's pointer over the picture, `None` when it is not over it.
    pub fn set_pointer(&mut self, pointer: Option<[f64; 2]>) {
        self.pointer = pointer;
    }

    /// Whether a graphic visible in the last frame declares `pointer_x` /
    /// `pointer_y` (only these re-evaluate when the pointer moves).
    pub fn reads_pointer(&self) -> bool {
        self.drawn.iter().any(|node| self.frames.get(node).is_some_and(|(_, _, pointer, _)| pointer.is_some()))
    }

    /// Locked time (a render, an export): an HDR graphic with
    /// `motion_blur` samples its shutter, recording one sub-frame per paint
    /// (not exact until the last); a preview draws one frame.
    pub fn set_locked_time(&mut self, on: bool) {
        self.locked_time = on;
    }

    /// Library documents load as an editor's: their literals stay settable
    /// in place (`Cx::patch_literal`), each document's sites named
    /// `motion:<id>@<rev>` (a module's are `module:<id>@<rev>`). A change
    /// loads them again.
    pub fn set_editable(&mut self, on: bool) {
        if self.editable != on {
            self.editable = on;
            self.docs.clear();
        self.timeless.clear();
            self.loading.clear();
            self.frames.clear();
        }
    }

    /// Literals were patched: each document's model read again from its
    /// running VM (what a layer's static fields copied at load now say) and
    /// its frames evaluated again.
    pub fn literals_patched(&mut self) {
        // Read again when next drawn (most are not at this time).
        self.stale.extend(self.docs.keys().copied());
        self.frames.clear();
    }

    /// The texts of library documents changed while editable (a held
    /// gesture the running documents cannot take in place): `graphics`'
    /// and `modules`' sources, under the revisions they load as. Each
    /// document that reads one loads again on the pool; until it has, the
    /// one that ran stands in.
    pub fn replace_texts(&mut self, graphics: Vec<(AssetRef, String)>, modules: Vec<(AssetRef, String)>) {
        let mut affected: Vec<u64> = Vec::new();
        for (asset, text) in graphics {
            affected.extend(self.meta.iter().filter(|(_, m)| m.src.as_ref() == Some(&asset)).map(|(k, _)| *k));
            self.sources.insert(asset, text);
        }
        if !modules.is_empty() {
            for (asset, text) in &modules {
                affected.extend(self.meta.iter().filter(|(_, m)| m.src.as_ref().and_then(|s| self.sources.get(s)).is_some_and(|src| src.contains(&asset.id))).map(|(k, _)| *k));
                self.modules.insert(asset.clone(), text.clone());
            }
            self.module_host = module_host(&self.modules);
        }
        for key in affected {
            self.loading.remove(&key);
            if let (Some(Ok(doc)), Some(meta)) = (self.docs.remove(&key), self.meta.get(&key)) {
                self.standins.insert(meta.key2, (meta.offset, doc));
            }
        }
        self.frames.clear();
    }

    /// A held recolour's step in every running document: the copies its
    /// load made of each colour (`from`, as loaded) moved to `to`
    /// (`MotionDoc::colour_data`, found once per gesture per document).
    pub fn recolour_copies(&mut self, map: &[([f32; 4], [f32; 4])]) -> usize {
        let mut changed = 0;
        let bits = |c: [f32; 4]| c.map(f32::to_bits);
        for (key, doc) in self.docs.iter() {
            let Ok(doc) = doc else { continue };
            for (from, to) in map {
                let entry = self.copies.entry((*key, bits(*from))).or_insert_with(|| (doc.colour_data(*from, None), *from));
                if entry.0.is_empty() || entry.1 == *to {
                    continue;
                }
                changed += doc.recolour_data(&entry.0, entry.1, *to);
                entry.1 = *to;
            }
        }
        if changed > 0 {
            self.stale.extend(self.docs.keys().copied());
            self.frames.clear();
        }
        changed
    }

    /// A held gesture ended: its recolour's places are forgotten.
    pub fn gesture_end(&mut self) {
        self.copies.clear();
    }

    /// A node's document as the host has it loaded (the graphic desk's,
    /// for its scrubber's strip): drawn for that node instead of loading
    /// its text again.
    pub fn give(&mut self, node: &str, doc: MotionDoc) {
        self.given.insert(node.to_string(), doc);
        self.frames.remove(node);
    }

    /// The host's document for `key`, when it gave one for `node`.
    fn adopt_given(&mut self, node: &str, key: u64) {
        if self.docs.contains_key(&key) {
            return;
        }
        if let Some(doc) = self.given.get(node) {
            self.docs.insert(key, Ok(doc.clone()));
            self.loading.remove(&key);
        }
    }

    /// The document key of a node's document, noted with what it is.
    fn key_of(&mut self, node: &str, src: &GraphicSrc, params: &[(String, InputValue)], offset: f64, bpm: f64) -> u64 {
        let node = doc_node(node);
        let key = Self::doc_key(node, src, params, offset, bpm);
        self.meta.entry(key).or_insert_with(|| KeyMeta {
            key2: LiveId::from_str(&format!("{node}|{src:?}|{params:?}|{bpm:?}")).0,
            offset,
            src: match src {
                GraphicSrc::Asset(a) => Some(a.clone()),
                GraphicSrc::Builtin(_) | GraphicSrc::Tree(_) => None,
            },
        });
        key
    }

    /// While editable, a document that can stand for `key` until it has
    /// loaded: the node's document at another clock offset (a clip moved),
    /// or as it ran before its text changed; and how far its clock is from
    /// the node's now (added to the node's time).
    fn standin(&self, key: u64) -> Option<(MotionDoc, f64)> {
        if !self.editable || self.docs.contains_key(&key) {
            return None;
        }
        let meta = self.meta.get(&key)?;
        let loaded = self.meta.iter().filter(|(k, m)| m.key2 == meta.key2 && **k != key).find_map(|(k, m)| match self.docs.get(k) {
            Some(Ok(doc)) => Some((doc.clone(), meta.offset - m.offset)),
            _ => None,
        });
        loaded.or_else(|| self.standins.get(&meta.key2).map(|(o, doc)| (doc.clone(), meta.offset - o)))
    }

    /// The files a loader resolved ([`resolve_sources`]).
    pub fn install(&mut self, assets: GraphicAssets) {
        self.sources.extend(assets.sources);
        self.modules.extend(assets.modules);
        self.module_host = module_host(&self.modules);
        self.images.extend(assets.images);
        self.svgs.extend(assets.svgs);
        // Draws made before (a reload) get the new files too.
        for draw in self.draws.values_mut() {
            for (asset, bytes) in &assets.files {
                draw.set_file(makepad_motion::AssetRef { id: asset.id.clone(), rev: asset.rev.clone() }, bytes.clone());
            }
        }
        self.files.extend(assets.files);
        self.videos.extend(assets.videos);
        self.docs.clear();
        self.timeless.clear();
        self.loading.clear();
        self.frames.clear();
    }

    /// What documents found after the first frames use (a document loaded
    /// ahead of the playhead): the files join those installed; nothing
    /// loaded is loaded again.
    pub fn add(&mut self, assets: GraphicAssets) {
        self.images.extend(assets.images);
        self.svgs.extend(assets.svgs);
        for draw in self.draws.values_mut() {
            for (asset, bytes) in &assets.files {
                draw.set_file(makepad_motion::AssetRef { id: asset.id.clone(), rev: asset.rev.clone() }, bytes.clone());
            }
        }
        self.files.extend(assets.files);
        self.videos.extend(assets.videos);
    }

    /// One Motion asset's source.
    pub fn set_source(&mut self, motion: AssetRef, source: String) {
        self.sources.insert(motion, source);
        self.docs.clear();
        self.timeless.clear();
        self.loading.clear();
        self.frames.clear();
    }

    /// The cut's timing and signals, on the cut's clock.
    /// The song clips' analysed pictures on the cut's clock, for every
    /// graphic.
    pub fn set_audio_tracks(&mut self, tracks: HashMap<String, Arc<makepad_audio_reactive::AudioTrack>>) {
        for draw in self.draws.values_mut() {
            draw.set_audio_tracks(tracks.clone());
        }
        self.tracks = tracks;
    }

    pub fn set_context(&mut self, context: MotionContext) {
        self.context = Some(context);
        self.docs.clear();
        self.timeless.clear();
        self.loading.clear();
        self.frames.clear();
    }

    /// The cut and its app runs' recorded data, so bound graphics follow
    /// their run (`fps`: the cut's, the placement's sampling rate).
    pub fn set_runs(&mut self, edit: Arc<CutDoc>, runs: HashMap<RunRef, Arc<RunData>>, fps: f64) {
        self.bound = Some((edit, runs, fps));
        self.docs.clear();
        self.timeless.clear();
        self.loading.clear();
        self.frames.clear();
    }

    /// Problems drawing the last frames (a missing source, a document that
    /// does not load, a shader that does not compile), each once.
    pub fn errors(&self) -> Vec<String> {
        let mut out = self.errors.clone();
        for (node, draw) in &self.draws {
            for error in draw.errors() {
                out.push(format!("`{node}`: {error}"));
            }
        }
        out
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        for draw in self.draws.values_mut() {
            draw.handle_event(cx, event);
        }
    }

    /// Start of a recorded frame.
    pub fn begin_frame(&mut self, cx: &mut Cx) {
        // A graphic that was not drawn for a few frames gives its GPU targets back: a scrub
        // visits a scene per click and each graphic keeps its own pools and scenes, so without
        // this every scene visited stays resident (0.5 GB each at 2880x1620).
        let drawn = std::mem::take(&mut self.drawn);
        for (node, draw) in &mut self.draws {
            let idle = self.idle.entry(node.clone()).or_insert(0);
            if drawn.contains(node) {
                *idle = 0;
            } else {
                *idle += 1;
                if *idle == GRAPHIC_IDLE_FRAMES {
                    draw.release_gpu(cx);
                }
            }
        }
        self.exact = true;
        self.drawn_inexact.clear();
        self.pictures_pending.clear();
    }

    /// Whether node `node` drew exact in the frame being recorded (or did
    /// not draw): what it drew is its final picture of this frame.
    pub fn drew_final(&self, node: &str) -> bool {
        !self.drawn_inexact.contains(node)
    }

    /// The pictures node `node`'s Freezes show this record are not final
    /// (`why`): a graphic under it at the freeze's time still gathers its
    /// shutter. Its draw is then not exact and sums no sub-frame with them.
    pub fn set_pictures_pending(&mut self, node: &str, why: String) {
        self.pictures_pending.insert(node.to_string(), why);
    }

    fn push_error(&mut self, error: String) {
        if !self.errors.contains(&error) {
            log!("edit render: {error}");
            self.errors.push(error);
        }
    }

    /// What a document an op draws is kept by: its node, source, params
    /// and clock.
    fn doc_key(node: &str, src: &GraphicSrc, params: &[(String, InputValue)], offset: f64, bpm: f64) -> u64 {
        LiveId::from_str(&format!("{node}|{src:?}|{params:?}|{offset:?}|{bpm:?}")).0
    }

    /// How a node's document loads in this cut: its params, its clock, the
    /// edit's songs and voices, its app run ([`FilmLoad`]).
    fn film_load_of(&self, node: &str, params: &[(String, InputValue)], offset: f64, bpm: f64) -> FilmLoad {
        let node = doc_node(node);
        FilmLoad {
            node: node.to_string(),
            params: params.to_vec(),
            context: MotionContext {
                lang: self.lang.clone(),
                ..self.context.clone().unwrap_or_else(|| MotionContext {
                    timing: Timing { grid: Some(BeatGrid::steady(bpm, 0.0)), ..Default::default() },
                    ..Default::default()
                })
            },
            offset,
            bound: self.bound.clone(),
            svgs: self.svgs.clone(),
            fingerprint: 0,
        }
    }

    /// How graphic `op`'s document loads in this cut, for a host that
    /// loads the same document itself (the graphic desk: its document is
    /// then the one the cut draws).
    pub fn film_load(&mut self, op: &DrawOp) -> Option<FilmLoad> {
        let DrawKind::Graphic { params, offset, bpm, .. } = &op.kind else { return None };
        Some(self.film_load_of(&op.node, params, *offset, *bpm))
    }

    /// Loading the document an op draws, with its params on its clock: here,
    /// or on the pool ([`Self::prepare`]).
    fn doc_load(&self, node: &str, src: &GraphicSrc, params: &[(String, InputValue)], offset: f64, bpm: f64) -> impl FnOnce() -> Result<MotionDoc, String> + Send + 'static {
        let source = match src {
            GraphicSrc::Tree(tree) => tree.resolve(&tree.document).and_then(|file| file.text().map(str::to_string)),
            GraphicSrc::Builtin(name) => builtin::source(name).map(str::to_string).ok_or_else(|| format!("no built-in graphic \"{name}\"")),
            GraphicSrc::Asset(asset) => self.sources.get(asset).cloned().ok_or_else(|| format!("the source of Motion asset {} was not resolved", asset.id)),
        };
        let film = self.film_load_of(node, params, offset, bpm);
        let modules = match src {
            GraphicSrc::Tree(tree) => makepad_motion::modules::Modules::from_tree(tree.clone()),
            GraphicSrc::Asset(_) | GraphicSrc::Builtin(_) => self.module_host.clone(),
        };
        let editable = match (self.editable, src) {
            (true, GraphicSrc::Asset(asset)) => Some(format!("motion:{}@{}", asset.id, asset.rev)),
            _ => None,
        };
        let load = move || film.load(&source?, modules).map_err(|diags| diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"));
        move || match &editable {
            Some(file) => makepad_motion::editable_as(file, load),
            None => load(),
        }
    }

    /// The document an op draws, loaded here unless it is loading on the
    /// pool ([`Self::loading`] says so; the frame waits for it then).
    fn doc(&mut self, node: &str, src: &GraphicSrc, params: &[(String, InputValue)], offset: f64, bpm: f64) -> Result<&MotionDoc, String> {
        let key = self.key_of(node, src, params, offset, bpm);
        self.adopt_given(node, key);
        self.poll_loads();
        if self.loading.contains_key(&key) {
            return Err(format!("`{node}` is still loading"));
        }
        if !self.docs.contains_key(&key) {
            let loaded = self.doc_load(node, src, params, offset, bpm)();
            self.docs.insert(key, loaded);
        }
        if self.stale.remove(&key) {
            if let Some(Ok(d)) = self.docs.get_mut(&key) {
                if let Ok(again) = d.reread() {
                    *d = again;
                }
            }
        }
        self.docs.get(&key).unwrap().as_ref().map_err(|e| e.clone())
    }

    /// Keep the documents whose loads on the pool finished.
    pub fn poll_loads(&mut self) {
        let (docs, standins, meta) = (&mut self.docs, &mut self.standins, &self.meta);
        self.loading.retain(|key, task| match task.try_take() {
            None => true,
            Some(result) => {
                let doc = result.unwrap_or_else(|error| Err(format!("the document's load: {error:?}")));
                if let Ok(doc) = &doc {
                    // Its stand-in is done (a later change stands in with this one).
                    if let Some(m) = meta.get(key) {
                        if standins.remove(&m.key2).is_some() {
                            standins.insert(m.key2, (m.offset, doc.clone()));
                        }
                    }
                }
                docs.insert(*key, doc);
                false
            }
        });
    }

    /// Whether the document `op` draws is still loading on the pool (the
    /// frame waits for it rather than the UI thread).
    pub fn loading(&mut self, op: &DrawOp) -> bool {
        let DrawKind::Graphic { src, params, offset, bpm, .. } = &op.kind else { return false };
        self.poll_loads();
        let key = self.key_of(&op.node, src, params, *offset, *bpm);
        self.adopt_given(&op.node, key);
        // (A stand-in draws meanwhile: the frame does not wait.)
        self.loading.contains_key(&key) && self.standin(key).is_none()
    }

    /// The document frame an op draws (cached for the op's time).
    fn frame_of(&mut self, op: &DrawOp) -> Option<makepad_motion::MotionFrame> {
        let DrawKind::Graphic { src, params, local_time, offset, bpm, .. } = &op.kind else { return None };
        let live = self.live.unwrap_or(*local_time);
        if let Some((at, at_live, at_pointer, frame)) = self.frames.get(&op.node) {
            if *at == local_time.to_bits() && at_live.is_none_or(|at| at == live.to_bits()) && pointer_current(*at_pointer, self.pointer) {
                return Some(frame.clone());
            }
        }
        let key = self.key_of(&op.node, src, params, *offset, *bpm);
        if let Some((doc, shift)) = self.standin(key) {
            let frame = doc.eval_pointer(*local_time + shift, self.live.unwrap_or(*local_time + shift), self.pointer);
            self.frames.insert(op.node.clone(), (local_time.to_bits(), doc.reads_live().then_some(live.to_bits()), pointer_key(doc.declares_pointer(), self.pointer), frame.clone()));
            return Some(frame);
        }
        if self.loading(op) {
            return None;
        }
        let doc = match self.doc(&op.node, src, params, *offset, *bpm) {
            Ok(doc) => doc.clone(),
            Err(error) => {
                self.push_error(format!("`{}`: {error}", op.node));
                return None;
            }
        };
        let frame = doc.eval_pointer(*local_time, live, self.pointer);
        self.frames.insert(op.node.clone(), (local_time.to_bits(), doc.reads_live().then_some(live.to_bits()), pointer_key(doc.declares_pointer(), self.pointer), frame.clone()));
        Some(frame)
    }

    /// The video frames a `Graphic` op shows at its time: for each, the
    /// place `MotionDraw` takes it (a Video layer's path, or a Scene3D's
    /// `<path>/videos/<name>`), the video, and the source second wanted
    /// (a Video layer's `source_time`; a Scene3D video runs on the
    /// document's clock).
    pub fn video_needs(&mut self, op: &DrawOp) -> Vec<(String, AssetRef, f64)> {
        let Some(frame) = self.frame_of(op) else { return Vec::new() };
        let mut out = Vec::new();
        for (_, item) in frame.walk() {
            match &item.kind {
                makepad_motion::ItemKind::Video { src: Media::Asset(asset), source_time, .. } => {
                    out.push((item.path.clone(), AssetRef { id: asset.id.clone(), rev: asset.rev.clone() }, *source_time));
                }
                makepad_motion::ItemKind::Scene3dDoc { videos, local_time, .. } => {
                    for (name, media) in videos {
                        if let Media::Asset(asset) = media {
                            out.push((format!("{}/videos/{name}", item.path), AssetRef { id: asset.id.clone(), rev: asset.rev.clone() }, *local_time));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The cut pictures a `Graphic` op's Freezes show: for each, the
    /// place `MotionDraw` takes it and the cut time it shows.
    /// A Freeze that starts inside the frame's shutter (motion blur) is in
    /// its later sub-frames, not at the frame's time: its picture is asked
    /// for too, as the one of the shutter's end.
    pub fn edit_frame_needs(&mut self, op: &DrawOp) -> Vec<(String, f64)> {
        let Some(frame) = self.frame_of(op) else { return Vec::new() };
        let backdrops = |frame: &makepad_motion::MotionFrame| -> Vec<(String, f64)> {
            frame.walk().into_iter().filter_map(|(_, item)| match item.kind {
                makepad_motion::ItemKind::Video { src: Media::Backdrop, source_time, .. } => Some((item.path.clone(), source_time)),
                _ => None,
            }).collect()
        };
        let mut out = backdrops(&frame);
        let shutter = frame.motion_blur.map_or(0.0, |(shutter, _, _)| shutter);
        if let (true, DrawKind::Graphic { src, params, local_time, offset, bpm, .. }) = (shutter > 0.0 && frame.fps > 0.0, &op.kind) {
            let end = *local_time + shutter / 360.0 / frame.fps;
            let doc = self.doc(&op.node, src, params, *offset, *bpm).ok().filter(|doc| doc.all_layers().iter().any(|l| matches!(l.kind, makepad_motion::LayerKind::Freeze { .. }))).cloned();
            if let Some(doc) = doc {
                let later = doc.eval_pointer(end, self.live.unwrap_or(end), self.pointer);
                for (path, at) in backdrops(&later) {
                    if !out.iter().any(|(p, _)| *p == path) {
                        out.push((path, at));
                    }
                }
            }
        }
        out
    }

    /// The videos the documents show, resolved at load.
    pub fn videos(&self) -> &HashMap<AssetRef, crate::media::ResolvedMedia> {
        &self.videos
    }

    /// Give node `node`'s document the frame of its video at `path`
    /// (a BGRA texture, `width` x `height`), for the next draw.
    pub fn set_video_frame(&mut self, cx: &mut Cx, node: &str, path: &str, texture: Texture, width: u32, height: u32) {
        let tracks = &self.tracks;
        let draw = self.draws.entry(node.to_string()).or_insert_with(|| {
            let mut draw = MotionDraw::new(cx);
            draw.set_audio_tracks(tracks.clone());
            draw
        });
        draw.set_video_frame(path, texture, width, height);
    }

    /// Load and evaluate `op`'s document ahead of the frame that draws it
    /// (parse, characters, voices): that frame does not wait for it.
    pub fn prepare(&mut self, cx: &mut Cx, op: &DrawOp) {
        self.poll_loads();
        let DrawKind::Graphic { src, params, offset, bpm, .. } = &op.kind else { return };
        let key = self.key_of(&op.node, src, params, *offset, *bpm);
        self.adopt_given(&op.node, key);
        if self.docs.contains_key(&key) || self.loading.contains_key(&key) {
            return;
        }
        let load = self.doc_load(&op.node, src, params, *offset, *bpm);
        // Parsing a document is the heavy part (a plate's tables run to
        // megabytes): on the pool, so the playing preview does not stop.
        match cx.task_pool().submit_frame_job(Class::Background, load) {
            Ok(task) => {
                self.loading.insert(key, task);
            }
            Err(_) => {
                let _ = self.frame_of(op);
            }
        }
    }

    /// Draw a `DrawKind::Graphic` op into a target of the op's size. Its
    /// passes join the compositor's order through `spans`.
    pub fn draw(&mut self, cx: &mut Cx2d, op: &DrawOp, logical_size: (u32, u32), spans: &mut Vec<(DrawPassId, DrawPassId)>) -> Option<Texture> {
        let DrawKind::Graphic { fit, opacity, local_time, .. } = &op.kind else { return None };
        if *opacity <= 0.0 {
            return None;
        }
        let mut frame = self.frame_of(op)?;
        for error in &frame.errors {
            let error = format!("`{}`: {error}", op.node);
            if !self.errors.contains(&error) {
                self.errors.push(error);
            }
        }
        // The node's opacity over the whole document.
        let opacity = *opacity;
        with_opacity(&mut frame, opacity);
        // Sub-frames (an HDR graphic's motion blur in locked time) are the
        // document at other times, with the same opacity.
        let DrawKind::Graphic { src, params, offset, bpm, .. } = &op.kind else { return None };
        let key = self.key_of(&op.node, src, params, *offset, *bpm);
        let (doc, shift) = match self.standin(key) {
            Some((doc, shift)) => {
                // Its own document loads on the pool meanwhile.
                if !self.loading.contains_key(&key) {
                    let load = self.doc_load(&op.node, src, params, *offset, *bpm);
                    if let Ok(task) = cx.task_pool().submit_frame_job(Class::Background, load) {
                        self.loading.insert(key, task);
                    }
                }
                (Some(doc), shift)
            }
            None => (self.doc(&op.node, src, params, *offset, *bpm).ok().cloned(), 0.0),
        };
        self.timeless.insert(op.node.clone(), shift == 0.0 && doc.as_ref().is_some_and(|doc| doc.is_time_independent()));
        // Share with the shutter callback without cloning the layer model.
        let doc = doc.map(Arc::new);
        let images = &self.images;
        let svgs = &self.svgs;
        let files = &self.files;
        let fresh = !self.draws.contains_key(&op.node);
        let tracks = &self.tracks;
        let draw = self.draws.entry(op.node.clone()).or_insert_with(|| {
            let mut draw = MotionDraw::new(cx);
            draw.set_audio_tracks(tracks.clone());
            draw
        });
        if fresh {
            // A full Scene3D's files, before it first loads.
            for (asset, bytes) in files {
                draw.set_file(makepad_motion::AssetRef { id: asset.id.clone(), rev: asset.rev.clone() }, bytes.clone());
            }
        }
        // Host resources the frame needs, handed over once.
        for need in draw.missing(&frame) {
            match need {
                makepad_motion_draw::Need::Image(media @ Media::Asset(_)) => {
                    let Media::Asset(asset) = &media else { continue };
                    if let Some((pixels, w, h)) = images.get(&AssetRef { id: asset.id.clone(), rev: asset.rev.clone() }) {
                        draw.set_image(cx, media.clone(), pixels.clone(), *w, *h);
                    }
                }
                makepad_motion_draw::Need::Svg(media @ Media::Asset(_)) => {
                    let Media::Asset(asset) = &media else { continue };
                    if let Some(markup) = svgs.get(&AssetRef { id: asset.id.clone(), rev: asset.rev.clone() }) {
                        draw.set_svg(cx, media.clone(), markup);
                    }
                }
                makepad_motion_draw::Need::Model(media @ Media::Asset(_)) => {
                    let Media::Asset(asset) = &media else { continue };
                    if let Some(glb) = files.get(&AssetRef { id: asset.id.clone(), rev: asset.rev.clone() }) {
                        if let Err(error) = draw.set_model(cx, media.clone(), glb) {
                            log!("edit render: `{}`: {error}", op.node);
                        }
                    }
                }
                _ => {}
            }
        }
        let fit = match fit {
            Fit::Fill => makepad_motion::Fit::Fill,
            Fit::Fit => makepad_motion::Fit::Fit,
            Fit::None => makepad_motion::Fit::None,
        };
        draw.set_locked_time(self.locked_time);
        draw.set_scene_coverage(self.scene_coverage);
        draw.set_inputs_pending(self.pictures_pending.remove(&op.node));
        let live = self.live;
        let pointer = self.pointer;
        draw.set_frames(doc.clone().map(|doc| std::sync::Arc::new(move |t: f64| {
            let mut frame = doc.eval_pointer(t + shift, live.unwrap_or(t + shift), pointer);
            with_opacity(&mut frame, opacity);
            frame
        }) as std::sync::Arc<dyn Fn(f64) -> makepad_motion::MotionFrame + Send + Sync>));
        let fit = makepad_motion_draw::fit_transform([logical_size.0 as f64, logical_size.1 as f64], frame.size, fit);
        let raster = makepad_motion::Affine::scale(op.size.0 as f64 / logical_size.0.max(1) as f64, op.size.1 as f64 / logical_size.1.max(1) as f64);
        let drawn = draw.draw_placed(cx, &frame, op.size, raster.then(&fit));
        // Scene functions can read the document's clock during drawing.
        if doc.as_ref().is_some_and(|doc| doc.reads_live()) {
            if let Some((_, at_live, _, _)) = self.frames.get_mut(&op.node) {
                *at_live = Some(live.unwrap_or(*local_time).to_bits());
            }
        }
        self.exact &= drawn.exact;
        if !drawn.exact {
            self.drawn_inexact.insert(op.node.clone());
        }
        spans.push((drawn.first, drawn.last));
        self.drawn.push(op.node.clone());
        Some(drawn.texture)
    }

    /// Whether every graphic of the last recorded frame painted whole.
    pub fn painted(&self, cx: &Cx) -> bool {
        self.drawn.iter().all(|node| self.draws.get(node).is_none_or(|draw| draw.painted(cx)))
    }

    /// Whether the last recorded frame drew every graphic with everything
    /// ready (glyphs, shaders, effects, host files): when not, record it
    /// again once it painted.
    pub fn exact(&self, cx: &Cx) -> bool {
        self.exact && self.drawn.iter().all(|node| self.draws.get(node).is_none_or(|draw| draw.exact(cx)))
    }

    /// The shutter steps of the last recorded frame's graphics so far
    /// ([`MotionDraw::shutter_progress`]): moving, recording again is progress.
    pub fn shutter_progress(&self) -> u64 {
        self.drawn.iter().filter_map(|node| self.draws.get(node).and_then(MotionDraw::shutter_progress)).map(u64::from).sum()
    }

    /// The graphics' paint state, for a log.
    pub fn debug_state(&self, cx: &Cx) -> String {
        self.drawn.iter().map(|node| match self.draws.get(node) {
            Some(draw) => format!("`{node}`: painted {} exact {} ({})", draw.painted(cx), draw.exact(cx), draw.debug_state(cx)),
            None => format!("`{node}`: no draw"),
        }).collect::<Vec<_>>().join("; ") + &format!("; own exact {}", self.exact)
    }

    pub fn reattach(&self, cx: &mut Cx2d) {
        for node in &self.drawn {
            if let Some(draw) = self.draws.get(node) {
                draw.reattach(cx);
            }
        }
    }
}

/// CPU selection data from the current cut renderer, in graphic document units.
pub struct GraphicInspection {
    pub node: String,
    pub size: (u32, u32),
    pub placement: [f64; 4],
    pub clip: [f64; 4],
    pub source: GraphicSrc,
    pub fit: Fit,
    pub doc: MotionDoc,
    pub frame: makepad_motion::MotionFrame,
    pub boxes: Vec<(String, [[f64; 2]; 4])>,
}

/// A graphic node's opacity over its whole document.
fn with_opacity(frame: &mut makepad_motion::MotionFrame, opacity: f64) {
    if opacity < 1.0 {
        frame.items = vec![makepad_motion::Item {
            path: "graphic".into(),
            world: makepad_motion::Affine::IDENTITY,
            local: makepad_motion::Local { anchor: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0 },
            opacity,
            blur: 0.0,
            feather: 0.0,
            feather_side: Default::default(),
            effects: Vec::new(),
            blend: makepad_motion::Blend::Normal,
            is_static: false,
            kind: makepad_motion::ItemKind::Group { children: std::mem::take(&mut frame.items), mask: None, mask_mode: Default::default(), size: None },
        }];
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pointer_keys_only_for_declaring_documents() {
        use super::{pointer_current, pointer_key};
        assert_eq!(pointer_key(false, Some([0.5, 0.5])), None);
        assert!(pointer_current(None, Some([0.2, 0.3])));
        let at = pointer_key(true, Some([0.25, 0.75]));
        assert!(pointer_current(at, Some([0.25, 0.75])));
        assert!(!pointer_current(at, Some([0.26, 0.75])));
        assert!(!pointer_current(at, None));
        assert!(pointer_current(pointer_key(true, None), Some([-1.0, -1.0])));
    }

    use super::*;
    use makepad_story_cut::CutEval;

    const A: &str = "ast_yqzlg4xa4mbgpzs6e2qsuqwhsu";
    const A_REV: &str = "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f";

    /// A click on every half second (120 bpm), 8 s at 48 kHz.
    fn clicks() -> Vec<f32> {
        let rate = 48_000usize;
        (0..rate * 8).map(|i| {
            let into = i % (rate / 2);
            if into < 480 { (1.0 - into as f32 / 480.0) * ((i as f32 * 0.37).sin()) } else { 0.0 }
        }).collect()
    }

    /// A song clip whose song has aligned lyrics is a voice in the cut's
    /// graphics: its words, and each word's sung syllables when the lyrics
    /// carry them (`s`), where the clip plays the track.
    #[test]
    fn a_song_clips_voice_has_its_words_and_syllables() {
        let source = format!(
            "use mod.flow.*\nuse mod.cut.*\nlet song = AudioClip{{src: asset(\"{A}\", \"{A_REV}\") in: 0.5 span: [1.0, 6.0]}}\n\
             let title = Title{{text: \"t\"}}\n\
             let out = CutOutput{{frame: title.out(@frame) audio: [conn(1, song.out(@audio))]}}\n\
             Flow{{\n kind: @cut\n output: {{width: 64 height: 36 fps: 30}}\n duration: 8.0\n song, title, out\n}}\n"
        );
        let doc = CutDoc::load(&source).unwrap_or_else(|errors| panic!("{errors:?}"));
        let eval = CutEval::new(&doc);
        let lyrics = r#"{"format":"vj-lyrics","version":4,"digest":"d","backend":"whisper","model":"m","language":"en","lines":[
            {"t0":1.0,"t1":3.0,"text":"I'm upping my doom","w":[1.0,1.4,2.0,2.4],"s":[[1.0],[1.4,1.7],[2.0],[2.4]],"c":true}]}"#;
        let lyrics = HashMap::from([(AssetRef { id: A.into(), rev: A_REV.into() }, lyrics.to_string())]);
        let context = voice_context(&doc, &eval, &lyrics, &HashMap::new()).expect("the song is a voice");
        let voice = context.signals.voice("song").expect("voice(@song)");
        assert_eq!(voice.words.len(), 4);
        // Source 0.5 plays at edit 1.0.
        assert!((voice.words[1].start - 1.9).abs() < 1e-9);
        assert_eq!(voice.words[1].syllables, vec![(1.9, 2.2), (2.2, 2.5)]);
        // A document reads them; `??` falls back for a one-syllable word.
        let motion = "use mod.motion.*\nlet w = voice(@song).words\nlet a = w[1].syllables[1].start ?? 0\nlet b = w[3].syllables[1].start ?? 7\n\
                      Motion{size: vec2(100, 100) duration: 8.0 layers: [Text{text: \"x\" x: a y: b}]}";
        let loaded = MotionDoc::load_with(motion, &[], context).unwrap_or_else(|d| panic!("{d:?}"));
        let frame = loaded.eval(0.0);
        assert!(frame.errors.is_empty(), "{:?}", frame.errors);
        let at = frame.items[0].world.apply([0.0, 0.0]);
        assert!((at[0] - 2.2).abs() < 1e-9 && (at[1] - 7.0).abs() < 1e-9, "{at:?}");
    }

    /// `music(@clip)` in a cut's graphics is the song clip's analysis,
    /// where the clip places the track (it read as silence before: only
    /// voices reached the graphics).
    #[test]
    fn graphics_hear_the_song_clips_analysis() {
        let source = format!(
            "use mod.flow.*\nuse mod.cut.*\nlet song = AudioClip{{src: asset(\"{A}\", \"{A_REV}\") in: 0.5 span: [1.0, 6.0]}}\n\
             let title = Title{{text: \"t\"}}\n\
             let out = CutOutput{{frame: title.out(@frame) audio: [conn(1, song.out(@audio))]}}\n\
             Flow{{\n kind: @cut\n output: {{width: 64 height: 36 fps: 30}}\n duration: 8.0\n bpm: 100\n song, title, out\n}}\n"
        );
        let doc = CutDoc::load(&source).unwrap_or_else(|errors| panic!("{errors:?}"));
        let eval = CutEval::new(&doc);
        assert!(voice_context(&doc, &eval, &HashMap::new(), &HashMap::new()).is_none(), "nothing to hear");
        let analysed = makepad_story_audio_features::analyze_music(&clicks(), 48_000.0);
        let song = makepad_story_audio_features::to_motion::music(&analysed);
        assert!(song.beats.len() >= 8, "{:?}", song.beats);
        let music = HashMap::from([("song".to_string(), song.clone())]);
        let context = voice_context(&doc, &eval, &HashMap::new(), &music).expect("the song is heard");
        let heard = context.signals.music("song").expect("music(@song)");
        // Source 0.5 plays at edit 1.0: everything half a second later.
        for (a, b) in song.beats.iter().zip(&heard.beats) {
            assert!((b - a - 0.5).abs() < 1e-9);
        }
        assert!((heard.energy.start - song.energy.start - 0.5).abs() < 1e-9);
        // `beat(n)` keeps the cut's bpm.
        let grid = context.timing.grid.clone().unwrap();
        assert!((grid.bpm - 100.0).abs() < 1e-9);
        // A document reads it.
        let motion = "use mod.motion.*; Motion{size: vec2(100, 100) duration: 8.0 layers: [Text{text: \"x\" opacity: fn(t) { music(@song).beat_phase(t) }}]}";
        let loaded = MotionDoc::load_with(motion, &[], context).unwrap_or_else(|d| panic!("{d:?}"));
        let frame = loaded.eval(3.2);
        assert!(frame.errors.is_empty(), "{:?}", frame.errors);
    }

    /// A graphic's imported modules (and what they import) are fetched with
    /// its source, and the document loads with them.
    #[test]
    fn graphics_fetch_the_modules_they_import() {
        struct Library(HashMap<String, String>);
        impl MediaResolver for Library {
            fn resolve(&mut self, media: &makepad_story_cut::MediaRef) -> Result<crate::media::ResolvedMedia, String> {
                Err(format!("{media:?}"))
            }
            fn effect_source(&mut self, effect: &AssetRef) -> Result<String, String> {
                Err(effect.id.clone())
            }
            fn motion_source(&mut self, motion: &AssetRef) -> Result<String, String> {
                self.0.get(&motion.id).cloned().ok_or_else(|| motion.id.clone())
            }
        }
        let rev = |id: &str| format!("\"{id}\", \"{A_REV}\"");
        let plate = format!("use mod.motion.*; let look = import(asset({}))\nMotion{{size: vec2(64, 36) duration: 1.0 layers: [Rect{{size: vec2(look.w, 4) fill: #ffffff}}]}}", rev("ast_lookaaaaaaaaaaaaaaaaaaaaaa"));
        let library = Library(HashMap::from([
            ("ast_plateaaaaaaaaaaaaaaaaaaaaa".to_string(), plate),
            ("ast_lookaaaaaaaaaaaaaaaaaaaaaa".to_string(), format!("use mod.motion.*\nlet base = import(asset({}))\n{{w: base.w * 2.0}}", rev("ast_baseaaaaaaaaaaaaaaaaaaaaaa"))),
            ("ast_baseaaaaaaaaaaaaaaaaaaaaaa".to_string(), "{w: 5.0}".to_string()),
        ]));
        let source = format!("use mod.flow.*\nuse mod.cut.*\nlet g = Graphic{{src: asset({}) span: [0.0, 1.0]}}\n\
                      let out = CutOutput{{frame: g.out(@frame)}}\nFlow{{\n kind: @cut\n output: {{width: 64 height: 36 fps: 30}}\n duration: 1.0\n g, out\n}}\n", rev("ast_plateaaaaaaaaaaaaaaaaaaaaa"));
        let doc = CutDoc::load(&source).unwrap_or_else(|errors| panic!("{errors:?}"));
        let library = std::sync::Mutex::new(Box::new(library) as Box<dyn MediaResolver>);
        let assets = resolve_sources(&doc, &library).unwrap();
        let mut ids: Vec<&str> = assets.modules.keys().map(|a| a.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["ast_baseaaaaaaaaaaaaaaaaaaaaaa", "ast_lookaaaaaaaaaaaaaaaaaaaaaa"]);
        let context = MotionContext { modules: module_host(&assets.modules), ..Default::default() };
        let plate = &assets.sources[&AssetRef { id: "ast_plateaaaaaaaaaaaaaaaaaaaaa".into(), rev: A_REV.into() }];
        let loaded = MotionDoc::load_with(plate, &[], context).unwrap_or_else(|d| panic!("{d:?}"));
        let makepad_motion::ItemKind::Rect { size, .. } = &loaded.eval(0.0).items[0].kind else { panic!("not a rect") };
        assert_eq!(size[0], 10.0);
    }
}
