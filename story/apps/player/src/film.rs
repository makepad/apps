//! The film's runtime: the manifest, the plates' documents loaded and
//! dropped around the playhead, the files they use, the song and its
//! lyrics, and the frame at a time drawn by Motion's own renderer
//! (`MotionDraw`, as Stage's edit preview draws a `Graphic`).
//!
//! Streaming: before the film starts, behind the loader, every plate in turn
//! loads, has its Scene3D scenes loaded and is drawn once (during play,
//! only the next plate's warm draws). Plate documents, and their scenes,
//! are kept around the playhead ([`doc_window`]) on every platform: a phone
//! browser's tab is killed near 1 GB, and every plate's document and
//! scenes held at once was 800 MB of wasm heap. They load again ahead of
//! the playhead (documents on the task pool where there are threads,
//! scenes a step a frame in the preparation's budget). Plates from a little behind the playhead to [`AHEAD`] seconds
//! after it get their files; only the plate on screen and the one warming
//! hold a draw (GPU targets), and only those and the next keep their
//! scenes' uploaded geometry.

use crate::audio::Song;
use makepad_stage_audio_media::pcm::TrackPcm;
use crate::job::Job;
use crate::bundle::{Bytes, Fetcher, Manifest, Source};
use makepad_motion::{AudioFeatures, BeatGrid, Media, MotionContext, MotionDoc, MotionFrame};
use makepad_motion_draw::{decode_image, MotionDraw, Need};
use makepad_widgets::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The families whose faces Motion reads by file when the manifest names
/// none (the others are fetched when a manifest names them).
const DEFAULT_FONTS: &[&str] = &["Roboto Flex", "JetBrains Mono", "Inter", "Playfair Display", "Cormorant Garamond", "LXGW WenKai"];

/// The files of [`DEFAULT_FONTS`] by the path the browser keys them
/// (`makepad_widgets/resources/Inter.ttf`).
fn default_font_files() -> Vec<String> {
    DEFAULT_FONTS
        .iter()
        .filter_map(|name| makepad_doc_fonts::family(name))
        .flat_map(|family| family.files.iter())
        .filter_map(|file| match file.place {
            makepad_doc_fonts::Place::Resource(resource) => Some(resource.replacen(':', "/", 1)),
            _ => None,
        })
        .collect()
}

/// Seconds before a plate starts during which it is warmed, its warm
/// draws, and the seconds between them (a warm draw is a second plate's
/// frame on the GPU: spread, not back to back).
const WARM_AHEAD: f64 = 2.0;
const WARM_FRAMES: u32 = 3;
const WARM_EVERY: f64 = 0.4;

/// Seconds before a load warm draw made while a scene it drew had its
/// environment preparing is made again. The renderer prepares the
/// environment off the UI thread (about 0.7 s natively), and only a draw
/// adopts it once it has landed. Each such draw is a whole plate, so it is
/// made this often, not every frame.
const ENVIRONMENT_EVERY: f64 = 0.2;
/// Seconds a plate waits for its scenes' environments at most. Past that it
/// is prepared without them (said in the log), so an environment that never
/// lands cannot hold the start forever.
const ENVIRONMENT_LIMIT: f64 = 10.0;

/// Seconds after the playhead whose plates get their files.
const AHEAD: f64 = 12.0;
/// Seconds before the playhead whose plates keep their files (a scrub back).
const BEHIND: f64 = 3.0;

enum Doc {
    Absent,
    Loading(Job<Result<MotionDoc, String>>),
    Ready(MotionDoc),
    Failed,
}

struct Plate {
    doc: Doc,
    draw: Option<MotionDraw>,
    /// The assets its document uses (asset ids).
    uses: Vec<makepad_motion::AssetRef>,
    /// Warm draws made before it shows ([`FilmRuntime::warm`]).
    warmed: u32,
    /// Since when it should show but draws nothing (the last frame stays
    /// up), and whether that stall was reported.
    stalled: Option<(f64, bool)>,
    /// Its next local time whose frame's Scene3D documents load ahead
    /// ([`FilmRuntime::scan`]); past its duration all have.
    scan_at: f64,
    /// The local times it is drawn at once behind the loader
    /// ([`FilmRuntime::load_warm`]): its start and each time a Scene3D first
    /// shows; and how many of them were drawn.
    warm_at: Vec<f64>,
    load_warmed: usize,
    /// Every load warm draw was made once: stays so when the document
    /// leaves the window (prepared is for the film's run, not residency).
    prepared: bool,
    /// Its frames were scanned through once (every scene it shows loaded
    /// once, its load warm times known). A document loaded again scans
    /// again ahead, to load its scenes before they show, but does not wait
    /// for that to be drawn.
    scanned: bool,
    /// Its scenes loading on workers ([`MotionDraw::scene_loads`]): it draws
    /// once they are adopted.
    scene_loads: Vec<Job<makepad_motion_draw::SceneLoaded>>,
    /// Seconds its last preparation draw and its last scan step took on
    /// this thread (0 until measured): what the next one is expected to
    /// cost ([`FilmRuntime::prepare`]).
    draw_cost: f64,
    scan_cost: f64,
    /// Since when its load warm draws wait for a scene's environment
    /// ([`ENVIRONMENT_LIMIT`]); None once a draw of it found them in.
    environment_since: Option<f64>,
}

/// The heap a plate's document and scenes take at most (a full film's largest,
/// measured in the browser): what bounds the plates loaded side by side.
const PLATE_HEAP_BYTES: usize = 96 << 20;

/// The UI thread's time a frame gives scan steps (seconds): each evaluates
/// a plate's frame and hands the scenes it shows to workers.
const SCAN_SLICE: f64 = 0.006;

/// Seconds between the frames of a plate whose Scene3D documents are
/// loaded ahead (a scene shown for less than this may load at its draw).
const SCAN_EVERY: f64 = 1.0;

/// A plate that should show but draws nothing for this long says what it
/// waits for (once): the film plays on, its picture would only freeze.
const STALL_REPORT_S: f64 = 2.0;

enum FileState {
    Fetching,
    Decoding(Job<Result<(Vec<u32>, u32, u32), String>>, Bytes),
    /// The file's bytes, one copy every draw shares (a draw made for a plate
    /// copied them all again: tens of MB of wasm heap per plate drawn).
    Ready { bytes: Arc<Vec<u8>>, image: Option<Arc<(Vec<u32>, u32, u32)>> },
    Failed,
}

/// What a frame drew.
pub struct Shown {
    pub texture: Texture,
    pub exact: bool,
}

pub struct FilmRuntime {
    project_tree: Option<Arc<makepad_asset_data::tree::TreeDraft>>,
    project_loading: Option<Job<Arc<makepad_asset_data::tree::TreeDraft>>>,
    pub fetcher: Fetcher,
    pub manifest: Option<Arc<Manifest>>,
    manifest_req: Option<LiveId>,
    song_req: Option<LiveId>,
    song_decode: Option<Job<Result<TrackPcm, String>>>,
    /// The cut's sound being fetched, decoded and mixed (replaces the song).
    cut_sound: Option<crate::edit_audio::CutSound>,
    pub song: Option<Song>,
    pub song_error: Option<String>,
    lyrics_req: Option<LiveId>,
    features_req: Option<LiveId>,
    /// Font files on their way (resource paths), and those every document
    /// waits for (the rest, a large CJK face, only the documents naming it).
    font_reqs: HashMap<LiveId, String>,
    fonts_eager: HashSet<String>,
    fonts_lazy: HashSet<String>,
    /// Lazy faces not asked for yet: requested once the first plate is
    /// ready, so tens of MB never queue ahead of the start.
    fonts_later: Vec<String>,
    /// The documents' clock and voices; None until the lyrics arrived.
    context: Option<MotionContext>,
    /// Sources by logical document name.
    sources: HashMap<String, Result<Arc<str>, String>>,
    source_reqs: HashMap<LiveId, String>,
    /// Asset files by asset id.
    files: HashMap<makepad_motion::AssetRef, FileState>,
    file_reqs: HashMap<LiveId, makepad_motion::AssetRef>,
    plates: Vec<Plate>,
    /// The shared CPU work of preloaded scenes' first draws
    /// ([`MotionDraw::adopt_scene`]), on a worker: no plate draws until it
    /// is done (a draw would work it out again, on this thread). There is
    /// none now: it was an environment's prefilter, and the renderer
    /// prepares a scene's environment itself, off the UI thread, from the
    /// scene's first draw. The plates wait for that in their load warm
    /// draws instead (`environment`).
    scene_warming: Vec<Job<()>>,
    /// The film pack's kernels linking (the browser): no document loads
    /// until they are registered, so every kernel finds its code.
    #[cfg(target_arch = "wasm32")]
    pack_link: Option<(WasmLinkId, Vec<makepad_story_film_pack::KernelEntry>)>,
    /// The film pack's audio programs linking, the same way (their slots
    /// go to the UI thread's registry, where the audio callback runs).
    #[cfg(target_arch = "wasm32")]
    audio_link: Option<(WasmLinkId, Vec<u64>)>,
    pub errors: Vec<String>,
    /// Loads Scene3D documents ahead ([`Self::scan`]); never draws.
    preloader: Option<MotionDraw>,
    /// The plate drawn behind the loader this frame ([`Self::prepare`]):
    /// the one plate holding a draw while the film starts.
    load_warm: usize,
    /// Load warm draws (plate, warm index) made while programs they use
    /// may still have been compiling, so possibly with parts left out:
    /// each is made again until none is pending, then once more whole.
    settle: std::collections::VecDeque<(usize, usize)>,
    /// The `settle` draw made last frame, and whether a scene it drew had
    /// its environment preparing: made again if it drew a program for the
    /// first time or started a compile, made again later (`environment`)
    /// if its environment was preparing, else it was whole ([`settled`]).
    whole: Option<(usize, usize, bool)>,
    /// `settle` draws made while a scene they drew had its environment
    /// preparing, with when each is made again ([`ENVIRONMENT_EVERY`]).
    /// The renderer prepares an environment off the UI thread, and only a
    /// draw adopts it, binding its lane texture and building its item
    /// material (`__item_ibl`), whose pipeline the HAL then compiles off
    /// the thread. So a plate is prepared only once a draw of it finds its
    /// environments in and, made again, finds no pipeline compiling
    /// ([`programs_compiling`]): that material is built and drawn first
    /// behind the loader, and the lighting does not change in play.
    environment: Vec<(usize, usize, f64)>,
    /// The programs compiling and waiting for their first draw when the
    /// last frame was prepared ([`programs_compiling`]).
    compiling_seen: usize,
    waiting_seen: usize,
    /// The last frame drew `settle`'s first draw while others compiled,
    /// with this many programs waiting.
    tried: bool,
    waiting_tried: usize,
    /// The plates loading ahead (see [`Self::stream`]): those whose draws
    /// may be made.
    warm_queue: Vec<usize>,
    /// `--gpu-report`: the texture slots (index, generation) each plate's
    /// draw created, collected while Some (native).
    #[cfg(not(target_arch = "wasm32"))]
    pub made_textures: Option<Vec<((usize, u64), usize)>>,
    /// Seconds spent evaluating and recording frames, and the frames, since
    /// [`Self::take_cost`].
    cost: (f64, f64, u32),
    /// Preparation while playing ([`Self::prepare`]).
    prep: Prep,
}

/// What the preparation did since [`FilmRuntime::take_prep`], and whether
/// a step ran alone in the last frame.
#[derive(Default)]
pub struct Prep {
    /// Seconds of preparation steps, steps run, steps that waited for a
    /// frame with room, steps that ran alone (longer than a frame).
    pub seconds: f64,
    pub ran: u32,
    pub waited: u32,
    pub alone: u32,
    /// The longest step (seconds).
    pub longest: f64,
    alone_last_frame: bool,
}

/// This frame while playing: when it started and the display's frame
/// interval (seconds); its preparation fits in what is left.
#[derive(Clone, Copy)]
pub struct FrameBudget {
    pub start: f64,
    pub interval: f64,
}

impl FilmRuntime {
    pub fn new(cx: &mut Cx, source: Source) -> Self {
        let mut fetcher = Fetcher::new(source);
        let manifest_req = Some(fetcher.fetch(cx, "manifest.json"));
        #[cfg(target_arch = "wasm32")]
        let pack_link = fetcher.pack_kernels().map(|(module, entries)| (cx.link_wasm_module(module.to_vec()), entries));
        #[cfg(target_arch = "wasm32")]
        let audio_link = fetcher.pack_audio().map(|(module, keys)| (cx.link_wasm_module(module.to_vec()), keys));
        Self {
            fetcher,
            manifest: None,
            manifest_req,
            song_req: None,
            song_decode: None,
            cut_sound: None,
            song: None,
            song_error: None,
            lyrics_req: None,
            features_req: None,
            font_reqs: HashMap::new(),
            fonts_eager: HashSet::new(),
            fonts_lazy: HashSet::new(),
            fonts_later: Vec::new(),
            context: None,
            sources: HashMap::new(),
            source_reqs: HashMap::new(),
            project_tree: None,
            project_loading: None,
            files: HashMap::new(),
            file_reqs: HashMap::new(),
            plates: Vec::new(),
            scene_warming: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            pack_link,
            #[cfg(target_arch = "wasm32")]
            audio_link,
            errors: Vec::new(),
            preloader: None,
            load_warm: 0,
            settle: Default::default(),
            whole: None,
            environment: Vec::new(),
            compiling_seen: 0,
            waiting_seen: 0,
            tried: false,
            waiting_tried: 0,
            warm_queue: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            made_textures: None,
            cost: (0.0, 0.0, 0),
            prep: Prep::default(),
        }
    }

    /// The preparation's work since the last call.
    pub fn take_prep(&mut self) -> Prep {
        let alone_last_frame = self.prep.alone_last_frame;
        let prep = std::mem::take(&mut self.prep);
        self.prep.alone_last_frame = alone_last_frame;
        prep
    }

    /// Mean ms per frame evaluating and recording since the last call.
    pub fn take_cost(&mut self) -> (f64, f64) {
        let (e, r, n) = std::mem::take(&mut self.cost);
        let n = n.max(1) as f64;
        (e * 1000.0 / n, r * 1000.0 / n)
    }

    /// Register the film pack's linked kernels once their link is back;
    /// false while it is on its way.
    #[cfg(target_arch = "wasm32")]
    fn pack_kernels_ready(&mut self, cx: &mut Cx) -> bool {
        use makepad_script_compute::kernel::{register_precompiled, Precompiled};
        if let Some((id, _)) = &self.audio_link {
            let Some(result) = cx.wasm_link_result(*id) else { return false };
            let (_, keys) = self.audio_link.take().unwrap();
            match result {
                Ok(slots) => {
                    for (key, slot) in keys.iter().zip(&slots) {
                        makepad_script_audio_aot::wasm::link(*key, *slot);
                    }
                    log!("player: {} precompiled audio programs linked", keys.len());
                }
                Err(e) => self.error(format!("the film's audio programs did not link: {e}")),
            }
        }
        let Some((id, _)) = &self.pack_link else { return true };
        let Some(result) = cx.wasm_link_result(*id) else { return false };
        let (_, entries) = self.pack_link.take().unwrap();
        match result {
            Ok(slots) => {
                let slot = |i: u32| slots.get(i as usize).copied().unwrap_or(0);
                let table: Vec<Precompiled> = entries.iter().map(|e| Precompiled { key: e.key, scalar: slot(e.scalar), simd: e.simd.map_or(0, slot) }).collect();
                register_precompiled(&table);
                log!("player: {} precompiled kernels linked", table.len());
            }
            Err(e) => self.error(format!("the film's kernels did not link: {e}")),
        }
        true
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn pack_kernels_ready(&mut self, _cx: &mut Cx) -> bool {
        true
    }

    fn error(&mut self, error: String) {
        if !self.errors.contains(&error) {
            error!("player: {error}");
            self.errors.push(error);
        }
    }

    pub fn duration(&self) -> f64 {
        self.manifest.as_ref().map_or(0.0, |m| m.film.duration)
    }

    /// Fetches, parses and decodes that finished; the plates' events.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        for plate in &mut self.plates {
            if let Some(draw) = &mut plate.draw {
                draw.handle_event(cx, event);
            }
        }
        for (id, result) in self.fetcher.handle_event(event) {
            if Some(id) == self.manifest_req {
                self.manifest_req = None;
                match result.and_then(|b| Manifest::parse(&b)) {
                    Ok(m) => self.got_manifest(cx, m),
                    Err(e) => self.error(e),
                }
            } else if self.cut_sound.as_ref().is_some_and(|sound| sound.owns(id)) {
                if let Err(e) = self.cut_sound.as_mut().unwrap().fetched(cx, id, result) {
                    self.cut_sound = None;
                    self.song_error = Some(format!("the cut's sound: {e}"));
                }
            } else if Some(id) == self.song_req {
                self.song_req = None;
                match result {
                    Ok(bytes) => self.song_decode = Some(crate::audio::decode(cx, bytes)),
                    Err(e) => self.song_error = Some(e),
                }
            } else if Some(id) == self.lyrics_req {
                self.lyrics_req = None;
                let m = self.manifest.clone().unwrap();
                let mut features = AudioFeatures::default();
                match result.and_then(|b| {
                    let lyrics = m.lyrics.as_ref().unwrap();
                    let voice = makepad_motion::lyrics::voice_from_lyrics(&String::from_utf8_lossy(&b), -lyrics.offset)?;
                    Ok((lyrics.voice.clone(), voice))
                }) {
                    Ok((name, voice)) => features.voices.push((name, voice)),
                    Err(e) => self.error(format!("lyrics: {e}")),
                }
                self.context = Some(context_for(features, m.film.bpm));
            } else if Some(id) == self.features_req {
                self.features_req = None;
                let m = self.manifest.clone().unwrap();
                let features = result.and_then(|b| AudioFeatures::from_json(&String::from_utf8_lossy(&b)));
                let features = features.unwrap_or_else(|e| {
                    self.error(format!("features: {e}"));
                    AudioFeatures::default()
                });
                self.context = Some(context_for(features, m.film.bpm));
            } else if let Some(path) = self.font_reqs.remove(&id) {
                match result {
                    Ok(bytes) => {
                        font_store().lock().unwrap_or_else(|e| e.into_inner()).insert(path.clone(), Arc::from(&bytes[..]));
                    }
                    Err(e) => self.error(format!("font: {e}")),
                }
                self.fonts_eager.remove(&path);
                self.fonts_lazy.remove(&path);
            } else if let Some(name) = self.source_reqs.remove(&id) {
                self.sources.insert(name, result.map(|b| Arc::<str>::from(String::from_utf8_lossy(&b).as_ref())));
            } else if let Some(asset) = self.file_reqs.remove(&id) {
                match result {
                    Ok(bytes) => {
                        let is_image = bytes.starts_with(&[0x89, b'P', b'N', b'G']) || bytes.starts_with(&[0xff, 0xd8]);
                        if is_image {
                            let b = bytes.clone();
                            self.files.insert(asset, FileState::Decoding(Job::run(cx, move || decode_image(&b)), bytes));
                        } else {
                            self.files.insert(asset, FileState::Ready { bytes: Arc::new(bytes.to_vec()), image: None });
                        }
                    }
                    Err(e) => {
                        self.error(e.clone());
                        self.files.insert(asset, FileState::Failed);
                    }
                }
            }
        }
        if let Some(sound) = &mut self.cut_sound {
            match sound.step() {
                Some(Ok(pcm)) => {
                    self.cut_sound = None;
                    self.song = Some(Song { pcm: Some(pcm) });
                }
                Some(Err(e)) => {
                    self.cut_sound = None;
                    self.song_error = Some(format!("the cut's sound: {e}"));
                }
                None => {}
            }
        }
        if let Some(decode) = &mut self.song_decode {
            if let Some(result) = decode.try_take() {
                self.song_decode = None;
                match result {
                    Ok(pcm) => self.song = Some(Song { pcm: Some(pcm) }),
                    Err(e) => self.song_error = Some(e),
                }
            }
        }
        for state in self.files.values_mut() {
            if let FileState::Decoding(task, bytes) = state {
                if let Some(result) = task.try_take() {
                    let image = result.ok().map(Arc::new);
                    *state = FileState::Ready { bytes: Arc::new(bytes.to_vec()), image };
                }
            }
        }
    }

    fn got_manifest(&mut self, cx: &mut Cx, m: Manifest) {
        if let Some(edit) = &m.edit {
            // The cut's whole sound, mixed here; the song entry stays only
            // for the credits.
            self.cut_sound = Some(crate::edit_audio::CutSound::start(cx, &mut self.fetcher, edit.clone(), m.film.duration));
        } else if let Some(song) = &m.song {
            // Fetched (beside the page, or where the pack names it) and
            // decoded here.
            self.song_req = Some(match self.fetcher.url_of(&song.file) {
                Some(url) => self.fetcher.fetch_app(cx, &url),
                None => self.fetcher.fetch(cx, &song.file),
            });
        }
        match (&m.features, &m.lyrics) {
            (Some(features), _) => self.features_req = Some(self.fetcher.fetch(cx, &features.file)),
            (None, Some(lyrics)) => self.lyrics_req = Some(self.fetcher.fetch(cx, &lyrics.file)),
            (None, None) => self.context = Some(context_for(AudioFeatures::default(), m.film.bpm)),
        }
        self.fetch_fonts(cx, &m);
        self.plates = m.plates.iter().map(|_| Plate { doc: Doc::Absent, draw: None, uses: Vec::new(), warmed: 0, stalled: None, scan_at: 0.0, warm_at: Vec::new(), load_warmed: 0, prepared: false, scanned: false, scene_loads: Vec::new(), draw_cost: 0.0, scan_cost: 0.0, environment_since: None }).collect();
        self.project_tree = None;
        self.project_loading = None;
        self.manifest = Some(Arc::new(m));
    }

    /// The fonts the documents read by file, in the browser (natively Motion
    /// reads the files). The manifest's `fonts`, else the bundled faces;
    /// a CJK face (tens of MB) is fetched last and waited for only by the
    /// documents naming it.
    fn fetch_fonts(&mut self, cx: &mut Cx, m: &Manifest) {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        let mut paths: Vec<String> = m.fonts.iter().flat_map(|(_, files)| files.iter().cloned()).collect();
        if paths.is_empty() {
            paths = default_font_files();
        }
        paths.sort_by_key(|p| p.contains("WenKai"));
        for path in paths {
            if path.contains("WenKai") {
                self.fonts_lazy.insert(path.clone());
                self.fonts_later.push(path);
            } else {
                self.fonts_eager.insert(path.clone());
                self.fetch_font(cx, m, path);
            }
        }
    }

    fn fetch_font(&mut self, cx: &mut Cx, m: &Manifest, path: String) {
        // A bundle file of that logical name, else the app's own resource.
        let id = match m.files.get(&path) {
            Some(entry) => self.fetcher.fetch(cx, &entry.file),
            None => self.fetcher.fetch_app(cx, &path),
        };
        self.font_reqs.insert(id, path);
    }

    /// Keep the plates around `t` loading and drop the far ones. Call every
    /// frame.
    pub fn stream(&mut self, cx: &mut Cx, t: f64) {
        let Some(m) = self.manifest.clone() else { return };
        let Some(context) = self.context.clone() else { return };
        if !self.pack_kernels_ready(cx) || !self.fonts_eager.is_empty() {
            return;
        }
        if !self.fonts_later.is_empty() && self.plates.iter().any(|p| matches!(p.doc, Doc::Ready(_))) {
            for path in std::mem::take(&mut self.fonts_later) {
                self.fetch_font(cx, &m, path);
            }
        }
        // The plate last drawn behind the loader stays while the start may
        // draw it again (its shaders still compiling, see prepare).
        // A plate prepared now stays so when its document leaves.
        for i in 0..self.plates.len() {
            if !self.plates[i].prepared && self.plate_prepared(i) {
                self.plates[i].prepared = true;
            }
        }
        let starting = !self.load_warm_done();
        // While the film starts, the next plates to be drawn behind the
        // loader load ahead of their turn (their documents on workers, side
        // by side), so the start never waits on one load after another: one
        // per worker of the task pool, as many as the app's memory budget
        // holds (a plate's document and scenes are up to ~96 MB of heap; a
        // phone's 320 MB budget takes three).
        let ahead = (cx.memory_budget_bytes() / PLATE_HEAP_BYTES).clamp(1, cx.task_pool().worker_count().max(1));
        let warm_queue: Vec<usize> = if starting { self.load_warm_queue(t, ahead) } else { Vec::new() };
        self.warm_queue = warm_queue.clone();
        // The plates nearest the playhead first (one load per frame where
        // loads run on this thread).
        let mut order: Vec<usize> = (0..m.plates.len()).collect();
        order.sort_by(|&a, &b| plate_distance(&m.plates[a], t).total_cmp(&plate_distance(&m.plates[b], t)));
        for i in order {
            let info = &m.plates[i];
            let end = info.start + info.dur;
            // The plate drawn behind the loader counts as near (its files)
            // and on the GPU (its draw) until it is done.
            let loading = i == self.load_warm || warm_queue.contains(&i);
            let near = loading || end > t - BEHIND && info.start < t + AHEAD;
            // Only the plate on screen and the one warming up hold GPU
            // resources; the others keep their documents (a scrub back
            // draws at once) and drop their draw, the one that just left
            // the screen at once (the picture still shown is the player's
            // own handle to its last frame).
            let on_gpu = i == self.load_warm || end > t && info.start < t + WARM_AHEAD;
            // Its draw goes whole: the platform retires the passes, draw
            // lists and textures of dropped handles (an orphaned pass is
            // never painted), so nothing of it stays on the GPU.
            if !on_gpu && self.plates[i].draw.is_some() {
                self.plates[i].draw = None;
                self.plates[i].warmed = 0;
            }
            // Outside the window its document goes (its scenes stay).
            if !loading && !doc_window(&m, i, t) {
                let plate = &mut self.plates[i];
                if !matches!(plate.doc, Doc::Absent) {
                    // A document's script heap is many MB: it is freed on a
                    // worker, not in the frame (a drop of one took 100-200 ms
                    // of the browser's main thread).
                    if let Doc::Ready(doc) = std::mem::replace(&mut plate.doc, Doc::Absent) {
                        drop(Job::run(cx, move || drop(doc)));
                    }
                    plate.warmed = 0;
                    plate.uses.clear();
                    // Its scenes went with it (keep_scenes below): back in
                    // the window, they load ahead again.
                    plate.scan_at = 0.0;
                }
                continue;
            }
            // Only a plate with no document reads its source and imports
            // (scanning every plate's text each frame was most of this).
            if matches!(self.plates[i].doc, Doc::Absent) {
                if let Some(load) = self.start_load(cx, &m, i, &context) {
                    self.plates[i].doc = Doc::Loading(load);
                }
            }
            let plate = &mut self.plates[i];
            if let Doc::Loading(job) = &mut plate.doc {
                if let Some(result) = job.try_take() {
                    match result {
                        Ok(doc) => {
                            plate.uses = doc.dependencies();
                            plate.doc = Doc::Ready(doc);
                        }
                        Err(e) => {
                            let e = format!("{}: {e}", info.name);
                            plate.doc = Doc::Failed;
                            self.error(e);
                        }
                    }
                }
            }
            if !near {
                continue;
            }
            let uses = self.plates[i].uses.clone();
            for asset in uses {
                if !self.files.contains_key(&asset) {
                    let file = m.asset_file(&asset).file;
                    let id = self.fetcher.fetch(cx, &file);
                    self.file_reqs.insert(id, asset.clone());
                    self.files.insert(asset, FileState::Fetching);
                }
            }
        }
        // Scenes loaded on workers are kept for their first draws here.
        for i in 0..self.plates.len() {
            let mut k = 0;
            while k < self.plates[i].scene_loads.len() {
                match self.plates[i].scene_loads[k].try_take() {
                    Some(loaded) => {
                        self.plates[i].scene_loads.remove(k);
                        if let Some(work) = MotionDraw::adopt_scene(loaded) {
                            self.scene_warming.push(Job::run(cx, work));
                        }
                    }
                    None => k += 1,
                }
            }
        }
        self.scene_warming.retain_mut(|job| !job.is_finished());
        // Scenes keep their uploaded geometry only for the plate on screen,
        // the next one, one warming and one drawn behind the loader.
        let cur = m.plate_at(t);
        let load_warm = self.load_warm;
        MotionDraw::keep_scene_geometry(|offset| {
            m.plate_at(offset + 1e-6).is_some_and(|i| {
                let p = &m.plates[i];
                Some(i) == cur || cur.map(|c| c + 1) == Some(i) || i == load_warm || (p.start > t && p.start - t < WARM_AHEAD)
            })
        });
        // Kept scene documents go with their plate's document.
        // (A scene no plate's span holds, one starting at the film's end,
        // stays: dropped, it would load again every frame.)
        let gone = MotionDraw::keep_scenes(|offset| m.plate_at(offset + 1e-6).is_none_or(|i| i == load_warm || warm_queue.contains(&i) || doc_window(&m, i, t)));
        // Their documents' heaps are freed on a worker, not in the frame.
        if !gone.is_empty() {
            drop(Job::run(cx, move || drop(gone)));
        }
        // Files no plate near the playhead uses go.
        let near = |p: &crate::bundle::PlateInfo| p.start + p.dur > t - BEHIND && p.start < t + AHEAD;
        let used: HashSet<&makepad_motion::AssetRef> = m.plates.iter().zip(&self.plates).enumerate().filter(|(i, (info, _))| *i == self.load_warm || warm_queue.contains(i) || near(info)).flat_map(|(_, (_, p))| p.uses.iter()).collect();
        self.files.retain(|asset, state| used.contains(asset) || matches!(state, FileState::Fetching));
    }

    /// Plate `i`'s document loading (on the task pool where there are
    /// threads), once its source, the modules it imports and its fonts are
    /// here; None while they are not (they are asked for).
    fn start_load(&mut self, cx: &mut Cx, m: &Manifest, i: usize, context: &MotionContext) -> Option<Job<Result<MotionDoc, String>>> {
        let info = &m.plates[i];
        if let Some(tree) = &m.project {
            if self.project_tree.is_none() {
                if let Some(job) = &mut self.project_loading {
                    if let Some(tree) = job.try_take() { self.project_tree = Some(tree); self.project_loading = None; }
                    else { return None; }
                } else {
                    let mut sources = Vec::new();
                    let mut waiting = false;
                    for file in tree.files.values().filter(|file| !matches!(file.kind, makepad_asset_data::AssetKind::Texture | makepad_asset_data::AssetKind::Video | makepad_asset_data::AssetKind::Audio)) {
                        let name = m.file_of(&file.path).file;
                        match self.sources.get(&name) {
                            Some(Ok(text)) => sources.push((file.path.clone(), text.clone())),
                            Some(Err(error)) => { let error = format!("{}: {error}", file.path); return Some(Job::run(cx, move || Err(error))); }
                            None => {
                                waiting = true;
                                if !self.source_reqs.values().any(|pending| pending == &name) { let id = self.fetcher.fetch(cx, &name); self.source_reqs.insert(id, name); }
                            }
                        }
                    }
                    if waiting { return None; }
                    let tree = tree.clone();
                    self.project_loading = Some(Job::run(cx, move || {
                        let mut tree = (*tree).clone();
                        for (path, text) in sources { if let Some(file) = tree.files.get_mut(&path) { file.bytes = text.as_bytes().into(); } }
                        Arc::new(tree)
                    }));
                    return None;
                }
            }
        }
        // The document's source.
        let source = match self.sources.get(&info.doc) {
            Some(Ok(source)) => source.clone(),
            Some(Err(e)) => {
                let e = format!("{}: {e}", info.name);
                self.error(e);
                return None;
            }
            None => {
                if !self.source_reqs.values().any(|d| d == &info.doc) {
                    let file = m.file_of(&info.doc).file;
                    let id = self.fetcher.fetch(cx, &file);
                    self.source_reqs.insert(id, info.doc.clone());
                }
                return None;
            }
        };
        // A document naming a lazily loaded face waits for it.
        if !self.fonts_lazy.is_empty() && source.contains("WenKai") {
            return None;
        }
        let params = makepad_story_cut::graphic::motion_inputs(&info.params);
        let module_host = if let (Some(tree), Some(path)) = (&self.project_tree, &info.path) {
            makepad_motion::modules::Modules::from_tree(tree.context(path))
        } else {
            // The library modules it imports and what they import, all here
            // before it loads (the loader resolves them dependencies first).
            let mut modules: Vec<(makepad_motion::AssetRef, Arc<str>)> = Vec::new();
            let mut waiting = false;
            let mut todo = makepad_motion::modules::asset_imports(&source);
            let mut seen = HashSet::new();
            while let Some(asset) = todo.pop() {
                let key = format!("module {}:{}", asset.id, asset.rev);
                if !seen.insert(key.clone()) {
                    continue;
                }
                match self.sources.get(&key) {
                    Some(Ok(text)) => {
                        todo.extend(makepad_motion::modules::asset_imports(text));
                        modules.push((asset, text.clone()));
                    }
                    // Loading says which import is missing.
                    Some(Err(_)) => {}
                    None => {
                        waiting = true;
                        if !self.source_reqs.values().any(|d| d == &key) {
                            let file = m.asset_file(&asset).file;
                            let id = self.fetcher.fetch(cx, &file);
                            self.source_reqs.insert(id, key);
                        }
                    }
                }
            }
            if waiting {
                return None;
            }
            makepad_motion::modules::Modules::from_sources(modules.iter().map(|(a, t)| (a.id.as_str(), a.rev.as_str(), &**t)))
        };
        let context = MotionContext { offset: info.start, modules: module_host, ..context.clone() };
        let name = info.name.clone();
        let load = move || {
            let t0 = Cx::monotonic_now();
            let doc = MotionDoc::load_with(&source, &params, context).map_err(|d| d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"));
            log!("player: {name} loaded in {:.0} ms ({} KB)", (Cx::monotonic_now() - t0) * 1000.0, source.len() >> 10);
            doc
        };
        Some(Job::run(cx, load))
    }

    /// Load the Scene3D documents of one sampled frame of the plate nearest
    /// `t` that has some left, once per call (a frame's budget where loads
    /// run on this thread): before the film starts every plate's scenes
    /// are loaded, so no first draw evaluates a scene.
    fn scan_next(&self, t: f64) -> Option<usize> {
        let m = self.manifest.as_ref()?;
        (0..m.plates.len())
            .filter(|&i| matches!(self.plates[i].doc, Doc::Ready(_)) && self.plates[i].scan_at <= m.plates[i].dur)
            .min_by(|&a, &b| plate_distance(&m.plates[a], t).total_cmp(&plate_distance(&m.plates[b], t)))
    }

    fn scan(&mut self, cx: &mut Cx, i: usize) {
        let Doc::Ready(doc) = &self.plates[i].doc else { return };
        let local = self.plates[i].scan_at;
        let frame = doc.clone().eval(local);
        let preloader = self.preloader.get_or_insert_with(|| MotionDraw::new(cx));
        // Its scenes not loaded yet load on workers, side by side.
        let loads = preloader.scene_loads(&frame);
        let loaded = !loads.is_empty();
        for load in loads {
            self.plates[i].scene_loads.push(Job::run(cx, move || load.run()));
        }
        self.plates[i].scan_at += SCAN_EVERY;
        if self.manifest.as_ref().is_some_and(|m| self.plates[i].scan_at > m.plates[i].dur) {
            self.plates[i].scanned = true;
        }
        // Its start and every frame showing a new scene get a load warm draw.
        if (loaded || local == 0.0) && !self.plates[i].warm_at.contains(&local) {
            self.plates[i].warm_at.push(local);
        }
    }

    /// Prepare the plates, one step per frame, the plate at the playhead
    /// and the ones after it first (then those before it): each plate is
    /// drawn once (and again where a Scene3D first shows) at the pixels it
    /// will show at, unseen, then its draw is dropped: the GPU builds every
    /// pipeline, shader and first-use state a plate needs before it plays,
    /// so its warm draws and first frames later only allocate its targets.
    /// The film starts once every plate is prepared ([`Self::prepared`]):
    /// a program's first real draw is where Direct3D (ANGLE) compiles it
    /// and the driver builds it, up to seconds of GPU time for a big
    /// raymarcher, so it happens behind the loader and never in play (a
    /// plate drawn first in play was a frame near Windows' 2 s GPU reset).
    /// GPU memory holds one such plate at a time.
    /// The next load warm draw to make: of the plates loading ahead, in
    /// their order, the first whose document, scenes and files are here
    /// and that has a draw not made yet.
    fn next_issue(&self) -> Option<usize> {
        self.warm_queue.iter().copied().find(|&i| {
            let plate = &self.plates[i];
            plate.load_warmed < plate.warm_at.len() && self.doc_prepared(i) && self.plate_ready(i)
        })
    }

    /// The next `count` plates the start will draw behind the loader, in
    /// the film's order from the playhead.
    fn load_warm_queue(&self, t: f64, count: usize) -> Vec<usize> {
        let Some(m) = &self.manifest else { return Vec::new() };
        let n = m.plates.len();
        let first = m.plate_at(t).unwrap_or(0);
        (0..n).map(|k| (first + k) % n).filter(|&i| !self.plate_prepared(i)).take(count).collect()
    }

    fn load_warm(&mut self, cx: &mut Cx2d, i: usize, pixels: (u32, u32)) {
        let k = self.plates[i].load_warmed;
        self.plates[i].load_warmed += 1;
        self.settle.push_back((i, k));
        self.load_warm_at(cx, i, k, pixels);
    }

    /// Plate `i`'s load warm draw `k`; whether a scene it drew has its
    /// environment still preparing ([`MotionDraw::environment_pending`]).
    fn load_warm_at(&mut self, cx: &mut Cx2d, i: usize, k: usize, pixels: (u32, u32)) -> bool {
        let Some(m) = self.manifest.clone() else { return false };
        let Some(&at) = self.plates[i].warm_at.get(k) else { return false };
        self.load_warm = i;
        let local = at.min(m.plates[i].dur);
        let drew = self.draw_plate(cx, i, local, pixels, false).is_some();
        let environment = drew && self.plates[i].draw.as_ref().is_some_and(|d| d.environment_pending());
        let note = if !drew { ": nothing drawn" } else if environment { ": its environment is preparing" } else { "" };
        log!("player: {} prepared at {local:.2} s ({}/{}){note}", m.plates[i].name, k + 1, self.plates[i].warm_at.len());
        environment
    }

    /// Plate `i` is prepared: its document failed, or every one of its load
    /// warm draws was made, and made whole (none waits in `settle` or for
    /// its environment).
    fn plate_prepared(&self, i: usize) -> bool {
        let plate = &self.plates[i];
        plate.prepared
            || matches!(plate.doc, Doc::Failed)
            || (self.doc_prepared(i) && plate.load_warmed >= plate.warm_at.len() && !self.settle.iter().any(|d| d.0 == i) && self.whole.is_none_or(|d| d.0 != i) && !self.environment.iter().any(|d| d.0 == i))
    }

    fn load_warm_done(&self) -> bool {
        (0..self.plates.len()).all(|i| self.plate_prepared(i))
    }

    /// Whether the plate showing at `t` can draw (its document loaded, its
    /// kernels compiled, its files here).
    pub fn ready_at(&self, t: f64) -> bool {
        let Some(m) = &self.manifest else { return false };
        let Some(i) = m.plate_at(t) else { return true };
        self.plate_ready(i)
    }

    /// Play may continue into the plate at `t`: its document, kernels,
    /// scenes and files are there (its draws warm just before it shows,
    /// [`Self::warm`]).
    pub fn showing_prepared(&self, t: f64) -> bool {
        let Some(m) = &self.manifest else { return false };
        m.plate_at(t).is_none_or(|i| self.doc_prepared(i) && self.plate_ready(i))
    }

    /// The film can start: every plate is prepared ([`Self::load_warm`]),
    /// so no program is drawn for the first time in play.
    pub fn prepared(&self) -> bool {
        self.manifest.is_some() && self.load_warm_done()
    }

    fn doc_prepared(&self, i: usize) -> bool {
        let plate = &self.plates[i];
        let dur = self.manifest.as_ref().map_or(0.0, |m| m.plates[i].dur);
        match &plate.doc {
            Doc::Ready(_) => plate.scene_loads.is_empty() && self.scene_warming.is_empty() && (plate.scanned || plate.scan_at > dur),
            Doc::Failed => true,
            _ => false,
        }
    }

    fn plate_ready(&self, i: usize) -> bool {
        let plate = &self.plates[i];
        match &plate.doc {
            Doc::Ready(_) => plate.scene_loads.is_empty() && self.scene_warming.is_empty() && plate.uses.iter().all(|a| matches!(self.files.get(a), Some(FileState::Ready { .. } | FileState::Failed))),
            Doc::Failed => true,
            _ => false,
        }
    }

    /// The song can start (or there is none, or it failed: silent).
    pub fn song_ready(&self) -> bool {
        self.manifest.as_ref().is_some_and(|m| m.song.is_none() && m.edit.is_none()) || self.song.is_some() || self.song_error.is_some()
    }

    /// How far the start is (0..1), the bytes it needs and has, and what it
    /// waits for: the manifest, the features, the fonts every document
    /// reads, the song (when decoded here) and the documents of the plates
    /// at `t`; then those plates' preparation.
    pub fn start_progress(&self, t: f64) -> (f64, u64, u64, String) {
        let Some(m) = &self.manifest else { return (0.0, 0, 0, "the film".into()) };
        let mut need: Vec<(String, u64)> = Vec::new();
        if let Some(f) = &m.features {
            need.push((f.file.clone(), f.bytes));
        }
        for (_, files) in &m.fonts {
            for path in files.iter().filter(|p| !p.contains("WenKai")) {
                if let Some(e) = m.files.get(path) {
                    need.push((e.file.clone(), e.bytes));
                }
            }
        }
        if let Some(edit) = &m.edit {
            need.extend(edit.files().map(|(file, bytes)| (file.to_string(), bytes)));
        } else if let Some(song) = &m.song {
            need.push((self.fetcher.url_of(&song.file).unwrap_or_else(|| song.file.clone()), song.bytes));
        }
        for p in m.plates.iter().filter(|p| p.start + p.dur > t && p.start < t + 1.0) {
            let e = m.file_of(&p.doc);
            need.push((e.file, e.bytes));
        }
        need.sort();
        need.dedup();
        let total: u64 = need.iter().map(|(_, b)| *b).sum();
        let got: u64 = need.iter().map(|(f, b)| self.fetcher.bytes_of(f, *b)).sum();
        let (ready, wanted) = self.readiness(t);
        let fraction = 0.5 * got as f64 / total.max(1) as f64 + 0.5 * ready as f64 / wanted as f64;
        let what = match need.iter().find(|(f, _)| !self.fetcher.done.contains(f)) {
            Some((f, _)) => f.split('.').next().unwrap_or(f).to_string(),
            None if !self.song_ready() => "the song".into(),
            None => match m.plates.get(self.load_warm) {
                Some(p) => format!("preparing {}", p.name),
                None => format!("preparing {}", self.plate_name(t).unwrap_or("the first plate")),
            },
        };
        (fraction.min(1.0), got, total, what)
    }

    /// For the loader: of the plates (all prepared before the film starts)
    /// and of the plates the playhead needs next ready, how many are
    /// (ready, wanted).
    pub fn readiness(&self, t: f64) -> (usize, usize) {
        let Some(m) = &self.manifest else { return (0, 1) };
        let wanted: Vec<usize> = (0..m.plates.len()).filter(|&i| {
            let p = &m.plates[i];
            p.start + p.dur > t && p.start < t + 1.0
        }).collect();
        let prepared = (0..m.plates.len()).filter(|&i| self.plate_prepared(i)).count();
        (prepared + wanted.iter().filter(|&&i| self.plate_ready(i)).count(), (m.plates.len() + wanted.len()).max(1))
    }

    pub fn plate_name(&self, t: f64) -> Option<&str> {
        let m = self.manifest.as_ref()?;
        Some(m.plates[m.plate_at(t)?].name.as_str())
    }

    /// Draw the film at `t` into a target of `pixels`; None when the plate
    /// there is not ready (show the last frame).
    /// While playing, `budget` is this frame's: the preparation of the
    /// other plates only takes what is left of it ([`Self::prepare`]).
    pub fn draw(&mut self, cx: &mut Cx2d, t: f64, pixels: (u32, u32), budget: Option<FrameBudget>) -> Option<Shown> {
        let m = self.manifest.clone()?;
        let i = m.plate_at(t)?;
        let local = (t - m.plates[i].start).clamp(0.0, m.plates[i].dur.max(0.0));
        let shown = self.draw_plate(cx, i, local, pixels, true);
        self.note_stall(i, shown.is_some());
        self.prepare(cx, t, pixels, budget);
        shown
    }

    /// The other plates' preparation, after the frame's own plate, a step
    /// of each kind at most: the plate starting next warmed
    /// ([`Self::warm`]), the plates not prepared yet drawn once
    /// ([`Self::load_warm`]) and their scenes loaded ahead
    /// ([`Self::scan`]). While playing (`budget`), a step runs only where
    /// what the same step took last time fits in what is left of the
    /// frame's interval: preparation takes the frame's idle time, not the
    /// shown plate's. A step longer than a whole interval, or not measured
    /// yet, runs alone: as the frame's only step and never in two frames
    /// running (one late frame per such step, not a late frame every frame).
    fn prepare(&mut self, cx: &mut Cx2d, t: f64, pixels: (u32, u32), budget: Option<FrameBudget>) {
        let alone_last = std::mem::take(&mut self.prep.alone_last_frame);
        let mut ran = false;
        if let Some(j) = self.warm_next(t) {
            // A warm draw a whole step behind its time runs regardless: the
            // plate's first frame must not be its first draw (its pipelines
            // and targets built in the shown frame were a multi-second GPU
            // frame at a boundary when no frame had room for them).
            let late = self.warm_late(t, j);
            if let Some(alone) = if late { Some(false) } else { self.may_run(budget, self.plates[j].draw_cost, ran, alone_last) } {
                let t0 = Cx::monotonic_now();
                self.warm(cx, j, pixels);
                self.plates[j].draw_cost = self.ran(t0, alone);
                ran = true;
            }
        }
        // The load-time draws of the plates not yet drawn run only while
        // the film does not play (behind the loader, paused): during play
        // each was a whole second plate, its targets allocated at full
        // size and dropped again (hundreds of MB a second of GPU
        // allocations), for what the next plate's warm draws do anyway
        // (shaders, kernels, faces, glyphs and scene geometry are the
        // app's, shared by every draw).
        if budget.is_some() {
            self.load_warm = usize::MAX;
        } else if !self.load_warm_done() {
            // A load warm draw exists to be its programs' first real use (on
            // Direct3D the driver builds a big raymarcher then, up to seconds
            // of the browser's GPU process): behind the loader, never in
            // play. A draw made while a program it uses compiles (on the
            // browser's compile threads) or waits for its first draw (one a
            // frame, Cx::spread_first_draws) leaves that part out. So the
            // plates' draws are made as soon as they can be, without waiting
            // (their programs compile side by side); while programs compile
            // only a draw with built programs waiting is made again; once
            // none compiles, each draw is made again, one at a time, until it
            // draws no program for the first time and starts no compile:
            // whole. Natively the programs compiling are the HAL's pipelines
            // (programs_compiling): draw_shaders_compiling_and_waiting counts
            // only the browser's. Nor is a draw whole while a scene it drew
            // has its environment preparing (off the UI thread, no pipeline
            // among it): it is made again every ENVIRONMENT_EVERY, after the
            // others, until a draw finds the environment in. That draw adopts
            // it (its lane texture, its item material), behind the loader,
            // and the item material's pipeline then compiles like any other.
            let (compiling, waiting) = programs_compiling(cx.draw_shaders_compiling_and_waiting(), cx.pipelines_pending());
            let now = Cx::monotonic_now();
            let mut drew_tried = false;
            if let Some((i, k, environment)) = self.whole.take() {
                match settled(compiling, waiting, self.compiling_seen, self.waiting_seen, environment) {
                    Settled::Again => self.settle.push_front((i, k)),
                    Settled::Environment => {
                        let since = *self.plates[i].environment_since.get_or_insert(now);
                        if now - since < ENVIRONMENT_LIMIT {
                            self.environment.push((i, k, now + ENVIRONMENT_EVERY));
                        } else {
                            let name = self.manifest.as_ref().map_or("a plate", |m| m.plates[i].name.as_str());
                            log!("player: {name}: a scene's environment did not land in {ENVIRONMENT_LIMIT} s: prepared without it");
                        }
                    }
                    Settled::Whole => self.plates[i].environment_since = None,
                }
            }
            self.compiling_seen = compiling;
            self.waiting_seen = waiting;
            for d in environment_due(&mut self.environment, now) {
                self.settle.push_back(d);
            }
            if let Some(i) = self.next_issue() {
                self.load_warm(cx, i, pixels);
            } else if compiling == 0 {
                if let Some((i, k)) = self.settle.pop_front() {
                    let environment = self.load_warm_at(cx, i, k, pixels);
                    self.whole = Some((i, k, environment));
                }
            } else if waiting > 0 && !self.settle.is_empty() {
                // Programs built while others still compile get their first
                // draws now: the first waiting draw again while it draws some,
                // else the next one.
                if self.tried && waiting >= self.waiting_tried {
                    self.settle.rotate_left(1);
                }
                let (i, k) = self.settle[0];
                self.load_warm_at(cx, i, k, pixels);
                self.tried = true;
                self.waiting_tried = waiting;
                drew_tried = true;
            }
            if !drew_tried {
                self.tried = false;
            }
        }
        // Scan steps only hand their scenes to workers: as many as fit a
        // few ms of the frame, so every plate's scenes load side by side.
        let scans_from = Cx::monotonic_now();
        while let Some(i) = self.scan_next(t) {
            let Some(alone) = self.may_run(budget, self.plates[i].scan_cost, ran, alone_last) else { break };
            let t0 = Cx::monotonic_now();
            self.scan(cx, i);
            self.plates[i].scan_cost = self.ran(t0, alone);
            ran = true;
            if alone || Cx::monotonic_now() - scans_from > SCAN_SLICE {
                break;
            }
        }
    }

    /// A preparation step expected to take `expect` seconds (0: not
    /// measured) may run now: Some(whether it runs alone), or None (it
    /// waits for a frame with room).
    fn may_run(&mut self, budget: Option<FrameBudget>, expect: f64, ran: bool, alone_last: bool) -> Option<bool> {
        let Some(b) = budget else { return Some(false) };
        if expect > 0.0 && Cx::monotonic_now() + expect <= b.start + b.interval {
            return Some(false);
        }
        if (expect == 0.0 || expect > b.interval) && !ran && !alone_last {
            return Some(true);
        }
        self.prep.waited += 1;
        None
    }

    /// A preparation step started at `t0` is done: its seconds.
    fn ran(&mut self, t0: f64, alone: bool) -> f64 {
        let took = Cx::monotonic_now() - t0;
        self.prep.seconds += took;
        self.prep.ran += 1;
        self.prep.longest = self.prep.longest.max(took);
        if alone {
            self.prep.alone += 1;
            self.prep.alone_last_frame = true;
        }
        took
    }

    /// Why the plate at `t` shows nothing new, once that is known: a
    /// document that failed, or a stall past [`STALL_REPORT_S`] (the player
    /// says so on screen instead of a frozen or black frame).
    pub fn problem_at(&self, t: f64) -> Option<String> {
        let m = self.manifest.as_ref()?;
        let i = m.plate_at(t)?;
        let name = &m.plates[i].name;
        let plate = &self.plates[i];
        if matches!(plate.doc, Doc::Failed) {
            return Some(self.errors.iter().rev().find(|e| e.contains(name.as_str())).cloned().unwrap_or_else(|| format!("{name} failed to load")));
        }
        match plate.stalled {
            Some((_, true)) => Some(self.errors.iter().rev().find(|e| e.starts_with(name.as_str())).cloned().unwrap_or_else(|| format!("{name} draws nothing"))),
            _ => None,
        }
    }

    /// Reports (once per stall) a plate that has drawn nothing for
    /// [`STALL_REPORT_S`] while it should show, with what it waits for.
    fn note_stall(&mut self, i: usize, drew: bool) {
        // A plate whose document is not asked for yet waits for what the
        // whole film needs first (its kernels, the fonts every document
        // reads), which the loader shows: that is not the plate stalling
        // (a slow start reported it as one, an error the export's safety
        // gate rejects).
        if drew || matches!(self.plates[i].doc, Doc::Absent) {
            self.plates[i].stalled = None;
            return;
        }
        let now = Cx::monotonic_now();
        let (since, reported) = *self.plates[i].stalled.get_or_insert((now, false));
        if reported || now - since < STALL_REPORT_S {
            return;
        }
        self.plates[i].stalled = Some((since, true));
        let name = self.manifest.as_ref().map_or(String::new(), |m| m.plates[i].name.clone());
        let plate = &self.plates[i];
        let what = match &plate.doc {
            Doc::Absent | Doc::Loading(_) => "its document to load".to_string(),
            Doc::Failed => "nothing (its document failed)".to_string(),
            Doc::Ready(_) => {
                let files: Vec<&str> = plate.uses.iter().filter(|a| !matches!(self.files.get(*a), Some(FileState::Ready { .. } | FileState::Failed))).map(|a| a.id.as_str()).collect();
                if !files.is_empty() {
                    format!("files {}", files.join(", "))
                } else if plate.draw.as_ref().is_some_and(|d| !d.ready()) {
                    "its effects to build or its models' textures".to_string()
                } else {
                    "its draw (nothing drawn)".to_string()
                }
            }
        };
        self.error(format!("{name} has drawn nothing for {STALL_REPORT_S} s: waiting for {what}"));
    }

    /// Draw the plate starting next a few times before it shows, unseen and
    /// at the pixels it will show at: its targets are allocated, its
    /// shaders compile, its kernels run their first (slow, not yet
    /// optimised) calls and its glyphs rasterise at their real sizes while
    /// the current plate plays, not on its first frame. The draws are
    /// [`WARM_EVERY`] apart.
    fn warm_next(&self, t: f64) -> Option<usize> {
        let m = self.manifest.as_ref()?;
        let j = m.plates.iter().position(|p| p.start > t && p.start - t < WARM_AHEAD)?;
        let k = self.plates[j].warmed;
        let due = m.plates[j].start - WARM_AHEAD + k as f64 * WARM_EVERY;
        (k < WARM_FRAMES && t >= due && self.plate_ready(j)).then_some(j)
    }

    /// Plate `j`'s next warm draw is a whole [`WARM_EVERY`] past its time.
    fn warm_late(&self, t: f64, j: usize) -> bool {
        let Some(m) = &self.manifest else { return false };
        let k = self.plates[j].warmed;
        t >= m.plates[j].start - WARM_AHEAD + (k + 1) as f64 * WARM_EVERY
    }

    fn warm(&mut self, cx: &mut Cx2d, j: usize, pixels: (u32, u32)) {
        let Some(m) = self.manifest.clone() else { return };
        let k = self.plates[j].warmed;
        self.plates[j].warmed += 1;
        let local = (k as f64 * 0.25).min(m.plates[j].dur);
        let _ = self.draw_plate(cx, j, local, pixels, false);
    }

    fn draw_plate(&mut self, cx: &mut Cx2d, i: usize, local: f64, pixels: (u32, u32), shown: bool) -> Option<Shown> {
        #[cfg(not(target_arch = "wasm32"))]
        if self.made_textures.is_some() {
            let before: HashSet<(usize, u64)> = cx.live_texture_slots().into_iter().collect();
            let drawn = self.draw_plate_inner(cx, i, local, pixels, shown);
            let made: Vec<((usize, u64), usize)> = cx.live_texture_slots().into_iter().filter(|s| !before.contains(s)).map(|s| (s, i)).collect();
            self.made_textures.as_mut().unwrap().extend(made);
            return drawn;
        }
        self.draw_plate_inner(cx, i, local, pixels, shown)
    }

    fn draw_plate_inner(&mut self, cx: &mut Cx2d, i: usize, local: f64, pixels: (u32, u32), shown: bool) -> Option<Shown> {
        let m = self.manifest.clone()?;
        if !self.plate_ready(i) {
            return None;
        }
        let info = &m.plates[i];
        let Doc::Ready(doc) = &self.plates[i].doc else { return None };
        let doc = doc.clone();
        let t0 = Cx::monotonic_now();
        let frame: MotionFrame = doc.eval(local);
        let t1 = Cx::monotonic_now();
        if shown {
            self.cost.0 += t1 - t0;
        }
        for e in &frame.errors {
            let e = format!("{}: {e}", info.name);
            if !self.errors.contains(&e) {
                error!("player: {e}");
                self.errors.push(e);
            }
        }
        let files = &self.files;
        let plate = &mut self.plates[i];
        let fresh = plate.draw.is_none();
        let draw = plate.draw.get_or_insert_with(|| MotionDraw::new(cx));
        if fresh {
            // A full Scene3D's files (textures it names), before it loads.
            for asset in &plate.uses {
                if let Some(FileState::Ready { bytes, .. }) = files.get(asset) {
                    draw.set_file(asset.clone(), bytes.clone());
                }
            }
        }
        for need in draw.missing(&frame) {
            if let Need::Image(media @ Media::Asset(_)) = need {
                let Media::Asset(asset) = &media else { continue };
                if let Some(FileState::Ready { image: Some(image), .. }) = files.get(asset) {
                    draw.set_image(cx, media.clone(), image.0.clone(), image.1, image.2);
                }
            }
        }
        if !draw.ready() {
            return None;
        }
        draw.set_locked_time(false);
        // An unseen draw also builds the programs the plate picks by value
        // later (an effect that turns on, an emissive colour): not first
        // drawn in play.
        draw.set_warm(if shown { None } else { Some(&doc) });
        let drawn = draw.draw_fit(cx, &frame, pixels, makepad_motion::Fit::Fill);
        // What the draw could not show (a Scene3D that did not load, a
        // shader that did not build): said once, from a plate's warm and
        // first draws, where scenes load.
        let errors = if fresh || !shown { draw.errors() } else { Vec::new() };
        for e in errors {
            let e = format!("{}: {e}", info.name);
            if !self.errors.contains(&e) {
                error!("player: {e}");
                self.errors.push(e);
            }
        }
        if shown {
            self.cost.1 += Cx::monotonic_now() - t1;
            self.cost.2 += 1;
        }
        Some(Shown { texture: drawn.texture, exact: drawn.exact })
    }

    /// The draw of the plate at `t` painted what it recorded.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fn painted(&self, cx: &Cx, t: f64) -> bool {
        let Some(m) = &self.manifest else { return true };
        let Some(i) = m.plate_at(t) else { return true };
        self.plates[i].draw.as_ref().is_none_or(|d| d.painted(cx) && d.exact(cx))
    }
}

/// The fonts the documents' text reads by file (a browser has none of this
/// machine's files): fetched before the documents load, handed to Motion
/// by path ([`install_font_reader`]).
fn font_store() -> &'static std::sync::Mutex<HashMap<String, Arc<[u8]>>> {
    static FONTS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Arc<[u8]>>>> = std::sync::OnceLock::new();
    FONTS.get_or_init(Default::default)
}

/// A font file's resource path (`makepad_widgets/resources/Inter.ttf`) from
/// the build machine's path Motion names it by.
fn font_resource(path: &std::path::Path) -> Option<String> {
    let p = path.to_string_lossy().replace('\\', "/");
    for (dir, krate) in [("/widgets/resources/", "makepad_widgets"), ("/motion/resources/", "makepad_motion")] {
        if let Some(at) = p.rfind(dir) {
            return Some(format!("{krate}/resources/{}", &p[at + dir.len()..]));
        }
    }
    None
}

/// Motion reads its fonts' files from [`font_store`] (the browser).
pub fn install_font_reader() {
    makepad_doc_fonts::set_font_reader(Box::new(|path| {
        let key = font_resource(path)?;
        font_store().lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned()
    }));
}

/// Whether plate `i`'s document is kept with the playhead at `t`: the plate
/// there, the one before (a scrub back), the two after, and every plate
/// starting within [`AHEAD`] seconds.
fn doc_window(m: &Manifest, i: usize, t: f64) -> bool {
    let cur = m.plate_at(t).unwrap_or(0);
    let p = &m.plates[i];
    (i + 1 >= cur && i <= cur + 2) || (p.start >= t && p.start < t + AHEAD)
}

/// Seconds from the playhead to a plate (0 while it shows).
fn plate_distance(p: &crate::bundle::PlateInfo, t: f64) -> f64 {
    (p.start - t).max(t - (p.start + p.dur)).max(0.0)
}

/// What becomes of the `settle` draw made last frame ([`settled`]).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Settled {
    /// It gave a waiting program its first draw or started a compile: made
    /// again first.
    Again,
    /// A scene it drew has its environment preparing: made again once
    /// [`ENVIRONMENT_EVERY`] has passed, after the others.
    Environment,
    /// It was whole.
    Whole,
}

/// The `settle` draw made last frame, with the draw shaders compiling and
/// waiting for their first draw now and when it was made, and whether a
/// scene it drew had its environment preparing after it: d3de81f4's rule
/// first (a program drawn for the first time or a compile started), then
/// the environment (the draw after it lands adopts it).
fn settled(compiling: usize, waiting: usize, compiling_seen: usize, waiting_seen: usize, environment: bool) -> Settled {
    if waiting < waiting_seen || compiling > compiling_seen {
        Settled::Again
    } else if environment {
        Settled::Environment
    } else {
        Settled::Whole
    }
}

/// The programs compiling, and those built and waiting for their first
/// draw, as the preparation counts them: the browser's draw shaders
/// (`shaders`, [`Cx::draw_shaders_compiling_and_waiting`], (0, 0)
/// natively) and the HAL's pipelines still compiling (`pipelines`,
/// [`Cx::pipelines_pending`], 0 in the browser). The HAL makes a new
/// program's pipeline at the beat after the draw that built it and compiles
/// it off the thread; a pass that has a picture keeps it while a pipeline
/// its draws use compiles. So a draw is whole only once the pipelines it
/// made are built, as in the browser once its programs are.
fn programs_compiling(shaders: (usize, usize), pipelines: usize) -> (usize, usize) {
    (shaders.0 + pipelines, shaders.1)
}

/// The draws of `waits` due again at `now`, in the order they waited.
fn environment_due(waits: &mut Vec<(usize, usize, f64)>, now: f64) -> Vec<(usize, usize)> {
    let due = waits.iter().filter(|w| now >= w.2).map(|w| (w.0, w.1)).collect();
    waits.retain(|w| now < w.2);
    due
}

/// The documents' clock (the film's beat grid) and the voices they hear.
fn context_for(features: AudioFeatures, bpm: f64) -> MotionContext {
    let mut context = MotionContext::analyzed(features, None, 0.0);
    context.timing.grid = Some(BeatGrid::steady(bpm, 0.0));
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A load warm draw made while a scene it drew had its environment
    /// preparing is not whole: the renderer builds the environment off the
    /// UI thread and only a draw adopts it (binding its lane texture and
    /// building its item material), so the draw is made again after
    /// ENVIRONMENT_EVERY, and only one that finds the environment in is
    /// whole. Natively draw_shaders_compiling_and_waiting is (0, 0) and an
    /// environment preparing compiles no pipeline (the HAL's pipelines are
    /// counted through [`programs_compiling`]), so without this rule the
    /// first redraw was whole however the environment stood.
    #[test]
    fn a_draw_made_while_its_environment_prepares_is_not_whole() {
        // d3de81f4's rule stands, environment or not: a draw that started a
        // compile, or gave a waiting program its first draw, is made again first.
        assert_eq!(settled(1, 0, 0, 0, false), Settled::Again);
        assert_eq!(settled(1, 0, 0, 0, true), Settled::Again);
        assert_eq!(settled(0, 0, 0, 1, true), Settled::Again);
        // Nothing compiles or waits: whole only once no scene's environment
        // is preparing.
        assert_eq!(settled(0, 0, 0, 0, true), Settled::Environment, "a draw made while its environment prepares is not whole");
        assert_eq!(settled(0, 0, 0, 0, false), Settled::Whole);
        assert_eq!(settled(0, 2, 0, 2, true), Settled::Environment);
        // The waits come due in their own time, in the order they began.
        let mut waits = vec![(0, 0, 1.0 + ENVIRONMENT_EVERY), (3, 1, 1.5), (1, 2, 1.0 + ENVIRONMENT_EVERY)];
        assert!(environment_due(&mut waits, 1.0 + 0.5 * ENVIRONMENT_EVERY).is_empty());
        assert_eq!(environment_due(&mut waits, 1.0 + 1.5 * ENVIRONMENT_EVERY), vec![(0, 0), (1, 2)]);
        assert_eq!(waits, vec![(3, 1, 1.5)]);
        assert_eq!(environment_due(&mut waits, 1.5), vec![(3, 1)]);
        assert!(waits.is_empty());
    }

    /// Natively a program's pipeline compiles on the HAL's worker, made at
    /// the beat after the draw that built the program. So the draw that
    /// adopts a scene's environment, and builds its item material
    /// (`__item_ibl`), finds the environment in, and its new pipeline is
    /// counted only from the next frame. Counting the browser's programs
    /// alone, which are (0, 0) natively, that draw was whole the frame
    /// after it: the plate was prepared while the pipeline compiled, the
    /// pass at the playhead kept its picture from before the environment,
    /// and the lighting changed in play once the pipeline was built.
    #[test]
    fn a_draw_whose_pipeline_compiles_on_the_hal_is_not_whole() {
        let native = (0, 0);
        // The draw was made with no pipeline compiling; the next frame the
        // pipeline it made compiles.
        let (made, next) = (programs_compiling(native, 0), programs_compiling(native, 1));
        assert_eq!(next, (1, 0), "the HAL's pipeline compiling counts as a program compiling");
        assert_eq!(settled(next.0, next.1, made.0, made.1, false), Settled::Again, "a draw that started a pipeline compile is made again once it is built");
        // Made again once it is built (no settle draw is made while one
        // compiles), that draw is whole.
        let built = programs_compiling(native, 0);
        assert_eq!(settled(built.0, built.1, built.0, built.1, false), Settled::Whole);
        // The browser has no HAL pipelines: its counts stay its own.
        assert_eq!(programs_compiling((2, 1), 0), (2, 1));
    }
}
