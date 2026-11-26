//! The offline renderer: for `n` in `0..N`, `t = n / fps`;
//! the frame is recorded once every clip frame and still it shows has
//! arrived, read back once all its passes have painted, and handed to the
//! encoder with `pts = n * 10_000_000 / fps`, with that frame's share of
//! the mixdown. Time is locked: a slow frame makes the render slower, it
//! never drops or repeats one. The GPU works on frame `n + 1` while a pool
//! worker converts and encodes frame `n`; at most [`ENCODE_QUEUE`] frames
//! wait for it.
//!
//! A frame larger than the platform's readback budget is read back in
//! horizontal strips. A frame is waited for as long as it takes (a busy
//! machine renders slower, never wrongly); what fails the render is the
//! render saying it cannot draw it: a graphic that does not load or a
//! shader that does not compile while its passes wait to paint. The file is written as
//! `<name>.partial.mp4` and renamed only when it is whole.
//!
//! Readbacks are taken by ticket (`Cx::try_take_texture_readbacks_for`), so
//! the renderer coexists with any other readback user of its process.

use crate::audio::{self, CHANNELS};
use makepad_story_cut::mix::SAMPLE_RATE;
use crate::mixdown::Mixdown;
use crate::compose::Painted;
use crate::engine::{CutEngine, CutJob, EngineStatus};
use crate::engine::EngineOptions;
use makepad_asset_data::BlobId;
use makepad_stage_asset_stream::VideoBudget;
use crate::compose::{PictureForm, PicturePlan};
use makepad_widgets::makepad_platform::video_file::{GpuPicture, GpuPictureMaker, PcmAudioTrackOptions, VideoColor, VideoFileCodec, VideoFileEncoder, VideoFileEncoderOptions};
use makepad_story_cut::{OutputFormat, OutputMatrix, OutputRange};
use makepad_widgets::makepad_platform::video_file::{VideoMatrix, VideoPrimaries, VideoRange, VideoTransfer};
use makepad_widgets::*;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Read-back frames waiting for the encoder before the GPU pauses.
pub const ENCODE_QUEUE: usize = 2;
/// Frames converting (readback → NV12) on the pool at once, ahead of the
/// sequential encoder.
const CONVERT_AHEAD: usize = 3;
/// Seconds of sound pushed ahead of the pictures.
const AUDIO_LEAD_SECONDS: u64 = 1;

#[derive(Clone)]
pub struct RenderOptions {
    pub path: PathBuf,
    /// Video bitrate; `None` scales with the pixel rate.
    pub bitrate: Option<u32>,
    /// Also write every `k`-th frame as `frame_NNNNN.jpg` into the folder,
    /// for inspection.
    pub dump: Option<(PathBuf, u64)>,
    /// Dump the frames as they are, raw RGBA (`frame_NNNNN.rgba`, the
    /// output's size, alpha as rendered): an export's reference pictures
    /// (`frame-check`), instead of JPEGs to look at.
    pub dump_raw: bool,
    /// Only frames `[first, end)` of the cut (a cache segment); its file's
    /// frame 0 is edit frame `first`. `None`: the whole edit, with its
    /// sound and captions.
    pub range: Option<(u64, u64)>,
    /// Mix and write the sound (a whole render only).
    pub audio: bool,
    /// Every frame a key frame, so any frame decodes on its own (the
    /// render cache's files, read back and forth by the preview).
    pub keyframe_only: bool,
    /// Write every frame at half the output's width and height (the
    /// cache's half quality); the cut is still drawn at full size.
    pub half: bool,
    /// Clip loads hold slots of the host's decoder budget (Stage: the VJ
    /// decks' budget).
    pub budget: Option<Arc<VideoBudget>>,
    /// Hash every frame into [`RenderResult::frame_hashes`] (off: zeros).
    pub hashes: bool,
    /// Frame blend for clips at another rate (see
    /// [`CutEngine::set_clip_blend`]); off: the nearest earlier frame.
    pub clip_blend: bool,
    /// Final (sampled motion blur) or draft (one sample a frame).
    pub quality: RenderQuality,
}

/// How a render draws its Motion graphics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderQuality {
    /// As the preview plays: each frame drawn once at its time (no
    /// sub-frames, a pass's preview cuts). Near real time.
    Draft,
    /// Locked time: a graphic's `motion_blur` samples its shutter until the
    /// frame converges, every pass at full quality.
    #[default]
    Final,
}

impl RenderQuality {
    /// `draft` or `final` (the command line's and the control API's word).
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "draft" => Some(Self::Draft),
            "final" => Some(Self::Final),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Final => "final",
        }
    }
}

impl std::fmt::Debug for RenderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderOptions")
            .field("path", &self.path)
            .field("range", &self.range)
            .field("audio", &self.audio)
            .field("keyframe_only", &self.keyframe_only)
            .field("half", &self.half)
            .field("clip_blend", &self.clip_blend)
            .field("quality", &self.quality)
            .field("budget", &self.budget.is_some())
            .finish_non_exhaustive()
    }
}

impl RenderOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), bitrate: None, dump: None, dump_raw: false, range: None, audio: true, keyframe_only: false, half: false, budget: None, hashes: true, clip_blend: false, quality: RenderQuality::Final }
    }
}

/// A finished render.
#[derive(Clone, Debug)]
pub struct RenderResult {
    pub path: PathBuf,
    /// The digest of the cut source it was rendered from: what
    /// `publish_edit_render` checks against the cut revision's own
    /// Source file.
    pub cut_source: BlobId,
    pub frames: u64,
    pub size: (u32, u32),
    pub fps: (u32, u32),
    pub duration_ms: u32,
    pub audio: bool,
    /// FNV-1a of every frame's RGBA as it went to the encoder: two renders
    /// of one cut revision are identical exactly when these are.
    pub frame_hashes: Vec<u64>,
    /// The middle frame, BGRA, for the catalogue thumbnail.
    pub poster: Option<(Vec<u32>, u32, u32)>,
    /// SRT and WebVTT captions written next to the video, one pair per
    /// Subtitles node: `<stem>.<node>.srt` and `.vtt`, published
    /// with it ([`RenderResult::captions`] pairs them).
    pub sidecars: Vec<PathBuf>,
}

impl RenderResult {
    /// The sidecars as caption tracks: every `<stem>.<node>.srt` with its
    /// `.vtt`, in node order.
    pub fn captions(&self) -> Vec<Captions> {
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let mut tracks: Vec<Captions> = self.sidecars.iter().filter(|path| path.extension().is_some_and(|e| e == "srt")).filter_map(|srt| {
            let name = srt.file_stem()?.to_string_lossy().to_string();
            let node = name.strip_prefix(&format!("{stem}."))?.to_string();
            let vtt = srt.with_extension("vtt");
            self.sidecars.contains(&vtt).then(|| Captions { node, srt: srt.clone(), vtt })
        }).collect();
        tracks.sort_by(|a, b| a.node.cmp(&b.node));
        tracks
    }
}

/// One Subtitles node's caption files.
#[derive(Clone, Debug, PartialEq)]
pub struct Captions {
    pub node: String,
    pub srt: PathBuf,
    pub vtt: PathBuf,
}

#[derive(Clone, Debug)]
pub enum RenderState {
    Loading,
    Rendering { frame: u64, frames: u64 },
    Finishing,
    Done(RenderResult),
    Failed(String),
    Cancelled,
}

/// Where a render's time goes, summed over its frames (nanoseconds; the
/// `edit-render: stages` line). The UI thread records and waits for paints
/// and readbacks; pool workers convert and encode, overlapping the GPU.
#[derive(Default)]
pub struct StageTimes {
    /// Recording a frame's passes (UI thread).
    pub record: AtomicU64,
    /// Recorded until every pass painted whole (GPU, paint cadence).
    pub paint: AtomicU64,
    /// Readback asked until its bytes arrived.
    pub readback: AtomicU64,
    /// Read-back bytes to the encoder's picture (pool).
    pub convert: AtomicU64,
    /// Pictures into the encoder (pool).
    pub encode: AtomicU64,
    /// The sound mixed and pushed (pool).
    pub audio: AtomicU64,
    /// The encoder finished and the file closed.
    pub finish: AtomicU64,
}

impl StageTimes {
    fn add(counter: &AtomicU64, since: std::time::Instant) {
        counter.fetch_add(since.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    /// `name=<ms per frame>` for every stage, and the wall time.
    pub fn line(&self, frames: u64, wall: f64) -> String {
        let per = |c: &AtomicU64| c.load(Ordering::Relaxed) as f64 / 1e6 / frames.max(1) as f64;
        format!(
            "edit-render: stages {frames} frames in {wall:.2} s ({:.1} fps); ms/frame record {:.2} paint {:.2} readback {:.2} convert {:.2} encode {:.2} audio {:.2}; finish {:.0} ms",
            frames as f64 / wall.max(1e-6), per(&self.record), per(&self.paint), per(&self.readback), per(&self.convert), per(&self.encode), per(&self.audio),
            self.finish.load(Ordering::Relaxed) as f64 / 1e6
        )
    }
}

/// One strip of a read-back frame, raw.
struct Piece {
    y: usize,
    width: usize,
    height: usize,
    data: HostBuf,
}

/// What a read-back strip is of: the output (RGBA, for the frame's hash,
/// the poster or a dump) or the encoder's picture.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Of {
    Output,
    Picture,
}

/// One frame off the GPU: its read-back strips, top down per kind, and
/// the picture slot it holds; `shared`: the picture's shared texture
/// handle (the encoder takes it from GPU memory).
struct Readback {
    n: u64,
    slot: usize,
    output: Vec<Piece>,
    picture: Vec<Piece>,
    shared: Option<u64>,
}

/// A frame being read back, strip by strip, and (`shared`) its picture
/// waiting for the GPU to complete it.
struct Reading {
    n: u64,
    slot: usize,
    targets: Vec<(Texture, u32, Of)>,
    pieces: Vec<(Piece, Of)>,
    ticket: Option<ReadbackTicket>,
    /// The picture's shared texture (desktop: the browser shares none).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    shared: Option<Texture>,
    /// The submission serial before the frame was recorded, and its last
    /// picture target: its passes are complete when that one is, and whole
    /// unless one of them was completed with draws left out.
    after: u64,
    last: Texture,
}

impl Reading {
    /// The strips of the output read back (more than one: the next record
    /// waits, it draws into them).
    fn output_strips(&self) -> usize {
        self.targets.iter().filter(|(_, _, of)| *of == Of::Output).count()
    }
}

/// The encoder and what it pushes beside each frame. Travels to a pool
/// worker for every frame and back.
struct Encoder {
    encoder: MovableEncoder,
    /// The sound, rendered by the audio engine ([`Mixdown`]).
    mix: Option<Mixdown>,
    fps: (u32, u32),
    /// Frames are numbered from `first`.
    first: u64,
    frames: u64,
    hashes: Vec<u64>,
    poster: Option<(Vec<u32>, u32, u32)>,
    cancel: Arc<AtomicBool>,
    times: Arc<StageTimes>,
}

/// The platform file encoder, handed from one pool worker to the next
/// between frames (never used by two threads at once: it travels inside
/// the one `Encoder`, by value). On Windows it holds Media Foundation COM
/// pointers, which the compiler cannot see are free-threaded (the sink
/// writer is created multithreaded and MF objects are free-threaded), so
/// it is not `Send` there on its own.
struct MovableEncoder(VideoFileEncoder);

// SAFETY: see above; ownership moves between threads, access never overlaps.
unsafe impl Send for MovableEncoder {}

type Finished = (Vec<u64>, Option<(Vec<u32>, u32, u32)>);

enum EncoderSlot {
    None,
    Idle(Box<Encoder>),
    Busy(TaskHandle<Result<Box<Encoder>, String>>),
    Finishing(TaskHandle<Result<Finished, String>>),
}

/// A worker still holding the encoder after a cancel or a failure: the
/// partial file goes once it has let go.
enum Draining {
    Frame(TaskHandle<Result<Box<Encoder>, String>>),
    Finish(TaskHandle<Result<Finished, String>>),
}

pub struct CutRenderer {
    engine: CutEngine,
    options: RenderOptions,
    /// Where the encoder writes until the file is whole.
    partial: PathBuf,
    fps: (u32, u32),
    /// The frames rendered: `[first, total)` of the cut.
    first: u64,
    total: u64,
    /// The next frame to record.
    next: u64,
    /// The frame recorded and not yet read back.
    recorded: Option<u64>,
    reading: Option<Reading>,
    readback_wait_logged: bool,
    /// What the current frame is waiting for, and since when.
    waiting: Option<(String, f64)>,
    queue: VecDeque<Readback>,
    /// Frames converting on the pool (in frame order), and converted ones
    /// waiting for the encoder.
    converting: VecDeque<TaskHandle<Result<Converted, String>>>,
    converted: VecDeque<Converted>,
    convert_spec: Option<ConvertSpec>,
    /// How frames become the encoder's pictures, and (shared textures) what
    /// makes the pictures from them.
    picture: Option<PicturePlan>,
    maker: Option<Arc<std::sync::Mutex<GpuPictureMaker>>>,
    /// The output's readback strips when it is read (`None`: one piece).
    output_rows: Option<u32>,
    encoder: EncoderSlot,
    /// The encoder was opened (it is opened once, when the sound is ready).
    encoder_opened: bool,
    draining: Option<Draining>,
    cancel: Arc<AtomicBool>,
    state: RenderState,
    /// The current frame's slow paint was logged.
    reported: bool,
    /// How often the current frame was recorded again, and what its paint
    /// was waiting on last (for a failure's words).
    rerecords: u32,
    watch_detail: String,
    times: Arc<StageTimes>,
    /// The submission serial before the recorded frame was recorded.
    recorded_after: u64,
    /// When the current frame was recorded, its readback asked, the first
    /// frame recorded.
    recorded_at: Option<std::time::Instant>,
    reading_at: Option<std::time::Instant>,
    started_at: Option<std::time::Instant>,
}

impl CutRenderer {
    pub fn new(cx: &mut Cx, job: CutJob, options: RenderOptions) -> Self {
        let fps = audio::fps_ratio(job.doc.output.fps);
        let (first, total) = match options.range {
            Some((first, end)) => (first, end.min(audio::frame_count(job.doc.duration, fps)).max(first)),
            None => (0, audio::frame_count(job.doc.duration, fps)),
        };
        let sound = options.audio && options.range.is_none();
        let engine_options = EngineOptions { budget: options.budget.clone().map(|budget| Box::new(budget) as Box<dyn crate::frames::DecoderBudget>), ..EngineOptions::default() };
        let mut engine = CutEngine::with_options(cx, CutJob { audio: sound, ..job }, engine_options);
        engine.set_clip_blend(options.clip_blend);
        // Final: every graphic's motion blur samples its shutter; draft
        // draws each frame once, as the preview does.
        engine.set_locked_time(options.quality == RenderQuality::Final);
        let partial = partial_path(&options.path);
        Self {
            engine, options, partial, fps, first, total, next: first, recorded: None, reading: None, readback_wait_logged: false, waiting: None,
            queue: VecDeque::new(), converting: VecDeque::new(), converted: VecDeque::new(), convert_spec: None, picture: None, maker: None, output_rows: None, encoder: EncoderSlot::None, encoder_opened: false, draining: None, cancel: Arc::new(AtomicBool::new(false)),
            state: RenderState::Loading, reported: false, rerecords: 0, watch_detail: String::new(),
            times: Arc::new(StageTimes::default()), recorded_after: 0, recorded_at: None, reading_at: None, started_at: None,
        }
    }

    pub fn state(&self) -> &RenderState {
        &self.state
    }

    /// Whether the host must keep drawing (and so keep the passes painting).
    pub fn busy(&self) -> bool {
        matches!(self.state, RenderState::Loading | RenderState::Rendering { .. } | RenderState::Finishing)
    }

    /// Done, failed or cancelled, and no worker holds the encoder any more
    /// (a partial file is gone): the host may exit.
    pub fn settled(&self) -> bool {
        !self.busy() && self.draining.is_none()
    }

    /// Stop: nothing more is drawn; the worker drops the encoder without
    /// finishing it and the partial file is removed once it has.
    pub fn cancel(&mut self, cx: &mut Cx) {
        if !self.busy() {
            return;
        }
        self.stop(cx);
        self.state = RenderState::Cancelled;
    }

    fn fail(&mut self, cx: &mut Cx, error: String) {
        log!("edit render: {error}");
        self.stop(cx);
        self.state = RenderState::Failed(error);
    }

    fn stop(&mut self, cx: &mut Cx) {
        self.cancel.store(true, Ordering::Release);
        if let Some(reading) = self.reading.take() {
            if let Some(ticket) = reading.ticket {
                cx.cancel_texture_readback(ticket);
            }
        }
        self.engine.stop();
        self.queue.clear();
        self.converting.clear();
        self.converted.clear();
        self.draining = match std::mem::replace(&mut self.encoder, EncoderSlot::None) {
            EncoderSlot::Busy(task) => Some(Draining::Frame(task)),
            EncoderSlot::Finishing(task) => Some(Draining::Finish(task)),
            EncoderSlot::Idle(_) | EncoderSlot::None => None,
        };
        if self.draining.is_none() {
            let _ = std::fs::remove_file(&self.partial);
        }
    }

    /// A drained worker has let the encoder go: remove the partial file.
    fn drain(&mut self) {
        let done = match &mut self.draining {
            Some(Draining::Frame(task)) => task.try_take().is_some(),
            Some(Draining::Finish(task)) => task.try_take().is_some(),
            None => false,
        };
        if done {
            self.draining = None;
            let _ = std::fs::remove_file(&self.partial);
        }
    }

    /// Workers, reader results and readbacks. Call on every event.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.drain();
        if !self.busy() {
            return;
        }
        self.engine.handle_event(cx, event);
        match self.engine.status() {
            EngineStatus::Failed(error) => return self.fail(cx, error),
            EngineStatus::Loading => return,
            EngineStatus::Ready => {}
        }
        // Frames draw once the picture has loaded; the encoder opens once
        // the sound is measured too (its first frame carries sound), and
        // the frames drawn before it wait for it in the queues.
        if matches!(self.state, RenderState::Loading) {
            if let Err(error) = self.begin() {
                return self.fail(cx, error);
            }
            self.state = RenderState::Rendering { frame: 0, frames: self.total - self.first };
        }
        if !self.encoder_opened && self.engine.sound_ready() {
            self.encoder_opened = true;
            match self.open_encoder() {
                Ok(encoder) => self.encoder = EncoderSlot::Idle(Box::new(encoder)),
                Err(error) => return self.fail(cx, error),
            }
        }
        if let Some(Reading { ticket: Some(ticket), .. }) = &self.reading {
            let ticket = *ticket;
            for result in cx.try_take_texture_readbacks_for(&[ticket]) {
                let reading = self.reading.as_mut().expect("reading");
                reading.ticket = None;
                let n = reading.n;
                let data = match result.data {
                    Ok(data) => data,
                    Err(error) => return self.fail(cx, format!("frame {n}: {error}")),
                };
                let (_, y, of) = reading.targets[reading.pieces.len()];
                reading.pieces.push((Piece {
                    y: y as usize,
                    width: result.width,
                    height: result.height,
                    data,
                }, of));
            }
        }
        if let Err(error) = self.read_whole(cx) {
            return self.fail(cx, error);
        }
        self.pump_encoder(cx);
    }

    /// The frame being read, once every strip arrived and (a shared
    /// picture) the GPU completed it: to the conversions.
    fn read_whole(&mut self, cx: &mut Cx) -> Result<(), String> {
        let Some(reading) = &self.reading else { return Ok(()) };
        if reading.pieces.len() < reading.targets.len() || !reading.last.gpu_drawn(cx) {
            return Ok(());
        }
        // A pass completed with draws left out (a pipeline still being
        // built for its target): painted, but not whole. The frame is drawn
        // again (and any recorded after it).
        if cx.frames_partial(reading.after + 1, cx.frame_completed_serial()) {
            let reading = self.reading.take().expect("reading");
            log!("edit render: frame {} drawn again: a pipeline was still being built when it painted", reading.n);
            self.engine.release_picture(reading.slot);
            self.reading_at = None;
            self.recorded = None;
            self.next = reading.n;
            return Ok(());
        }
        // The browser shares no textures (its pictures are plain targets).
        #[cfg(target_arch = "wasm32")]
        let shared: Option<u64> = None;
        #[cfg(not(target_arch = "wasm32"))]
        let shared = match &reading.shared {
            Some(texture) => {
                if !texture.gpu_drawn(cx) {
                    return Ok(());
                }
                match cx.shared_texture_handle(texture) {
                    Some(0) => return Err(format!("frame {}: the picture's texture could not be shared", reading.n)),
                    Some(handle) => Some(handle),
                    None => return Ok(()),
                }
            }
            None => None,
        };
        let reading = self.reading.take().expect("reading");
        if let Some(at) = self.reading_at.take() {
            StageTimes::add(&self.times.readback, at);
        }
        let n = reading.n;
        let (mut output, mut picture) = (Vec::new(), Vec::new());
        for (piece, of) in reading.pieces {
            if of == Of::Output { output.push(piece) } else { picture.push(piece) }
        }
        // Exactly the declared size, or nothing.
        let (width, height) = (self.engine.doc().output.width as usize, self.engine.doc().output.height as usize);
        let rows: usize = output.iter().map(|piece| piece.height).sum();
        if !output.is_empty() && (rows != height || output.iter().any(|piece| piece.width != width)) {
            return Err(format!("frame {n}: read back {}x{rows}, the output is {width}x{height}", output[0].width));
        }
        if let Some(plan) = self.picture.filter(|_| !picture.is_empty()) {
            let (texels, rows) = plan.texels();
            let read: u32 = picture.iter().map(|piece| piece.height as u32).sum();
            if read != rows || picture.iter().any(|piece| piece.width as u32 != texels) {
                return Err(format!("frame {n}: read back a {}x{read} picture, it is {texels}x{rows}", picture[0].width));
            }
        }
        self.queue.push_back(Readback { n, slot: reading.slot, output, picture, shared });
        self.waiting = None;
        self.reported = false;
        self.rerecords = 0;
        self.watch_detail.clear();
        Ok(())
    }

    /// Whether frame `n` is read back as RGBA too: every frame when hashed,
    /// the poster's, and the dumped ones.
    fn wants_output(&self, n: u64) -> bool {
        let Some(spec) = &self.convert_spec else { return false };
        spec.hash || n == spec.poster_frame || spec.dump.as_ref().is_some_and(|(_, every)| *every > 0 && n % every == 0)
    }

    /// Whether the cut sounds at all: any audio source the mix plays
    /// (every sounding source). A cut without one gets no audio
    /// track.
    fn has_audio(&self) -> bool {
        if !self.options.audio || self.options.range.is_some() {
            return false;
        }
        let doc = self.engine.doc();
        !self.engine.eval().audio_plan(0.0, doc.duration).segments.is_empty()
    }

    /// The output's size checked, its readback strips and conversion set:
    /// frames can draw.
    fn begin(&mut self) -> Result<(), String> {
        let doc = self.engine.doc().clone();
        let (width, height) = (doc.output.width, doc.output.height);
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err(format!("output {width}x{height}: H.264 needs even, non-zero sides"));
        }
        // A frame larger than TEXTURE_READBACK_MAX_BYTES is read back in
        // strips that each fit (a single row over it is read alone).
        let strip_rows = |texels: u32, rows: u32| -> Option<u32> {
            let row = (texels as usize * 4).div_ceil(4096) * 4096;
            let fit = (TEXTURE_READBACK_MAX_BYTES / row).max(1);
            (fit < rows as usize).then_some(fit as u32)
        };
        self.output_rows = strip_rows(width, height);
        // The encoder's picture is made on the GPU after the output: NV12
        // (or, keeping the alpha, BGRA) at the encoded size. Where the
        // platform encoder takes pictures from GPU memory it reads them from
        // shared textures; elsewhere they are read back (1.5 bytes a pixel).
        let size = if self.options.half { ((width / 2) & !1, (height / 2) & !1) } else { (width, height) };
        let form = if doc.output.alpha { PictureForm::Bgra } else { PictureForm::Nv12 };
        // `MAKEPAD_EXPORT_READBACK=1`: read the pictures back even where the
        // encoder takes them from GPU memory (the other platforms' path).
        let shared = GpuPictureMaker::TAKES_PLANES && std::env::var_os("MAKEPAD_EXPORT_READBACK").is_none();
        let mut plan = PicturePlan { size, scale: if self.options.half { 2 } else { 1 }, form, color: output_color(&doc.output), shared, strip_rows: None };
        let (texels, rows) = plan.texels();
        plan.strip_rows = strip_rows(texels, rows);
        if shared {
            self.maker = Some(Arc::new(std::sync::Mutex::new(GpuPictureMaker::new().map_err(|e| format!("pictures: {e}"))?)));
        }
        self.picture = Some(plan);
        self.engine.set_picture(Some(plan), doc.output.alpha);
        self.convert_spec = Some(ConvertSpec {
            size: (width as usize, height as usize),
            hash: self.options.hashes,
            poster_frame: self.total / 2,
            dump: self.options.dump.clone(),
            dump_raw: self.options.dump_raw,
            plan,
            alpha: doc.output.alpha,
            maker: self.maker.clone(),
        });
        Ok(())
    }

    fn open_encoder(&mut self) -> Result<Encoder, String> {
        let doc = self.engine.doc().clone();
        let (width, height) = (doc.output.width, doc.output.height);
        let (out_w, out_h) = if self.options.half { ((width / 2) & !1, (height / 2) & !1) } else { (width, height) };
        let pixel_rate = out_w as f64 * out_h as f64 * self.fps.0 as f64 / self.fps.1 as f64;
        let has_audio = self.has_audio();
        // All-intra needs about four times the bits of a GOP for the same look.
        let bits = if self.options.keyframe_only { 0.2 } else { 0.12 };
        let options = VideoFileEncoderOptions {
            // A transparent picture: HEVC with alpha, the only codec that
            // keeps it (an OS encoder without it fails to open, saying so).
            codec: if doc.output.alpha { VideoFileCodec::H265Alpha } else { VideoFileCodec::H264 },
            width: out_w,
            height: out_h,
            fps_num: self.fps.0,
            fps_den: self.fps.1,
            video_bitrate_bps: self.options.bitrate.unwrap_or((pixel_rate * bits).max(2_000_000.0) as u32),
            audio: has_audio.then_some(PcmAudioTrackOptions { sample_rate: SAMPLE_RATE, channels: CHANNELS, aac_bitrate_bps: 192_000 }),
            keyframe_only: self.options.keyframe_only,
            color: output_color(&doc.output),
        };
        if let Some(parent) = self.partial.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let _ = std::fs::remove_file(&self.partial);
        let path = self.partial.to_string_lossy().to_string();
        let encoder = MovableEncoder(VideoFileEncoder::new(&path, options).map_err(|e| format!("{path}: {e}"))?);
        Ok(Encoder {
            encoder,
            mix: has_audio.then(|| Mixdown::new(Arc::new(self.engine.audio_mix()))).transpose()?,
            fps: self.fps,
            first: self.first,
            frames: self.total,
            hashes: Vec::with_capacity((self.total - self.first) as usize),
            poster: None,
            cancel: self.cancel.clone(),
            times: self.times.clone(),
        })
    }

    /// Hand the oldest read-back frame to an idle encoder; finish after the
    /// last one.
    fn pump_encoder(&mut self, cx: &mut Cx) {
        match &mut self.encoder {
            // Conversions keep going while a frame encodes.
            EncoderSlot::Busy(task) => match task.try_take() {
                None => {}
                Some(Ok(Ok(encoder))) => self.encoder = EncoderSlot::Idle(encoder),
                Some(Ok(Err(error))) => return self.fail(cx, error),
                Some(Err(error)) => return self.fail(cx, format!("encoder worker: {error:?}")),
            },
            EncoderSlot::Finishing(task) => match task.try_take() {
                None => return,
                Some(Ok(Ok((frame_hashes, poster)))) => {
                    self.encoder = EncoderSlot::None;
                    return self.finished(cx, frame_hashes, poster);
                }
                Some(Ok(Err(error))) => return self.fail(cx, error),
                Some(Err(error)) => return self.fail(cx, format!("encoder worker: {error:?}")),
            },
            EncoderSlot::Idle(_) | EncoderSlot::None => {}
        }
        // Read-back frames convert on the pool, several at once, in order.
        while !self.queue.is_empty() && self.converting.len() + self.converted.len() < CONVERT_AHEAD {
            let Some(spec) = self.convert_spec.clone() else { break };
            let frame = self.queue.pop_front().expect("queued");
            let times = self.times.clone();
            self.converting.push_back(match cx.task_pool().submit(Class::Background, move || {
                let at = std::time::Instant::now();
                let converted = convert(spec, frame);
                StageTimes::add(&times.convert, at);
                converted
            }) { Ok(task) => task, Err(error) => return self.fail(cx, error.to_string()) });
        }
        while let Some(task) = self.converting.front_mut() {
            match task.try_take() {
                None => break,
                Some(Ok(Ok(converted))) => {
                    self.converting.pop_front();
                    // Its picture targets are free again (copied, or read).
                    self.engine.release_picture(converted.slot);
                    self.converted.push_back(converted);
                }
                Some(Ok(Err(error))) => return self.fail(cx, error),
                Some(Err(error)) => return self.fail(cx, format!("conversion worker: {error:?}")),
            }
        }
        if !matches!(self.encoder, EncoderSlot::Idle(_)) {
            return;
        }
        let done = self.next >= self.total && self.recorded.is_none() && self.reading.is_none()
            && self.queue.is_empty() && self.converting.is_empty();
        if self.converted.is_empty() && !done {
            return;
        }
        let EncoderSlot::Idle(encoder) = std::mem::replace(&mut self.encoder, EncoderSlot::None) else { unreachable!() };
        if let Some(frame) = self.converted.pop_front() {
            self.encoder = EncoderSlot::Busy(match cx.task_pool().submit(Class::Background, move || encode(encoder, frame)) { Ok(task) => task, Err(error) => return self.fail(cx, error.to_string()) });
        } else {
            self.encoder = EncoderSlot::Finishing(match cx.task_pool().submit(Class::Background, move || finish(encoder)) { Ok(task) => task, Err(error) => return self.fail(cx, error.to_string()) });
            self.state = RenderState::Finishing;
        }
    }

    /// The whole file in place, its captions beside it.
    fn finished(&mut self, cx: &mut Cx, frame_hashes: Vec<u64>, poster: Option<(Vec<u32>, u32, u32)>) {
        if frame_hashes.len() as u64 != self.total - self.first {
            return self.fail(cx, format!("{} frames encoded, the render has {}", frame_hashes.len(), self.total - self.first));
        }
        let _ = std::fs::remove_file(&self.options.path);
        if let Err(error) = std::fs::rename(&self.partial, &self.options.path) {
            return self.fail(cx, format!("{}: {error}", self.options.path.display()));
        }
        let mut captions = Vec::new();
        let sidecars = if self.options.range.is_none() { self.engine.sidecars() } else { Vec::new() };
        for (node, srt, vtt) in sidecars {
            let (srt_path, vtt_path) = (sidecar_path(&self.options.path, &node, "srt"), sidecar_path(&self.options.path, &node, "vtt"));
            for (path, text) in [(&srt_path, srt), (&vtt_path, vtt)] {
                if let Err(error) = std::fs::write(path, text) {
                    return self.fail(cx, format!("{}: {error}", path.display()));
                }
            }
            let _ = node;
            captions.push(srt_path);
            captions.push(vtt_path);
        }
        let doc = self.engine.doc().clone();
        let audio = self.has_audio();
        let wall = self.started_at.map_or(0.0, |at| at.elapsed().as_secs_f64());
        // stderr: a JSON tool's stdout is its report alone
        eprintln!("{}", self.times.line(self.total - self.first, wall));
        self.engine.stop();
        self.state = RenderState::Done(RenderResult {
            path: self.options.path.clone(),
            cut_source: BlobId::hash_of(doc.source.as_bytes()),
            frames: self.total - self.first,
            size: if self.options.half { ((doc.output.width / 2) & !1, (doc.output.height / 2) & !1) } else { (doc.output.width, doc.output.height) },
            fps: self.fps,
            duration_ms: (audio::frame_time(self.total - self.first, self.fps) * 1000.0).round() as u32,
            audio,
            frame_hashes,
            poster,
            sidecars: captions,
        });
    }

    /// The current frame waits for `what`: noted (for the report of a long
    /// paint), and failed only when the render says it cannot draw it (a
    /// graphic's errors while its passes wait to paint).
    fn watch(&mut self, cx: &mut Cx, what: String) {
        if !self.waiting.as_ref().is_some_and(|(was, _)| *was == what) {
            self.waiting = Some((what.clone(), Cx::monotonic_now()));
        }
        if what == "its passes to paint" {
            let errors = self.engine.graphic_errors();
            if !errors.is_empty() {
                let n = self.next;
                self.fail(cx, format!("frame {n}: cannot paint: {}", errors.join("; ")));
            }
        }
    }

    /// Record, wait for paint, read back. Call from the host's draw, inside
    /// its window pass, on every paint while [`Self::busy`].
    pub fn draw(&mut self, cx: &mut Cx2d) {
        if !matches!(self.state, RenderState::Rendering { .. }) {
            return;
        }
        // The readback under way: its next strip, once the last came.
        if let Some(reading) = &mut self.reading {
            if reading.ticket.is_none() && reading.pieces.len() < reading.targets.len() {
                let n = reading.n;
                let texture = reading.targets[reading.pieces.len()].0.clone();
                match texture.read_back(cx, ReadbackRequest::default()) {
                    Ok(ticket) => {
                        self.readback_wait_logged = false;
                        reading.ticket = Some(ticket);
                    }
                    // The host keeps drawing while the render is busy.
                    Err(ReadbackError::Backpressure) => {
                        if !self.readback_wait_logged {
                            log!("edit render: context readback request limit reached; frame {n} strip waits");
                            self.readback_wait_logged = true;
                        }
                        return self.watch(cx, "readback room".into());
                    }
                    Err(error) => return self.fail(cx, format!("frame {n}: {error}")),
                }
            }
        }
        if let Some(n) = self.recorded {
            match self.engine.painted(cx) {
                Painted::Painting if self.engine.stale(cx) => {
                    // It will not paint whole: record the frame again. A
                    // frame that never becomes whole fails by the watch,
                    // naming what it kept waiting for, not silently forever.
                    self.rerecords += 1;
                    self.watch_detail = format!("recorded {} times; {}", self.rerecords, self.engine.painting_report(cx));
                    self.recorded = None;
                    self.next = n;
                    return self.watch(cx, "its passes to paint".into());
                }
                Painted::Painting => {
                    self.engine.reattach(cx);
                    // A model the frame places is building (Stage's model
                    // host): progress, not a stall; the frame waits for it.
                    #[cfg(feature = "models")]
                    if makepad_stage_model_host::building() {
                        self.waiting = None;
                    }
                    self.watch_detail = self.engine.painting_report(cx);
                    if self.waiting.as_ref().is_some_and(|(what, since)| what == "its passes to paint" && Cx::monotonic_now() - since > 2.0 && !self.reported) {
                        self.reported = true;
                        log!("edit render: frame {n} is still painting: {}", self.engine.painting_report(cx));
                    }
                    return self.watch(cx, "its passes to paint".into());
                }
                Painted::Whole => {
                    if let Some(at) = self.recorded_at.take() {
                        StageTimes::add(&self.times.paint, at);
                    }
                }
            }
            // Whole: its readback starts now, but only once the frame
            // before has been read (its output target is the one the next
            // record would draw into).
            if self.reading.is_some() {
                return self.watch(cx, "its readback".into());
            }
            let Some((slot, pictures)) = self.engine.take_picture() else {
                return self.fail(cx, format!("frame {n}: it has no picture for the encoder"));
            };
            let mut targets: Vec<(Texture, u32, Of)> = Vec::new();
            if self.wants_output(n) {
                targets.extend(self.engine.readback_targets().into_iter().map(|(texture, y)| (texture, y, Of::Output)));
            }
            let plan = self.picture.expect("a render's picture plan");
            let shared = plan.shared.then(|| pictures[0].0.clone());
            let last = pictures.last().map(|(texture, _)| texture.clone()).expect("a picture has a target");
            if !plan.shared {
                targets.extend(pictures.into_iter().map(|(texture, y)| (texture, y, Of::Picture)));
            }
            self.reading_at = Some(std::time::Instant::now());
            let ticket = match targets.first() {
                Some((texture, _, _)) => match texture.read_back(cx, ReadbackRequest::default()) {
                    Ok(ticket) => {
                        self.readback_wait_logged = false;
                        Some(ticket)
                    }
                    Err(ReadbackError::Backpressure) => {
                        if !self.readback_wait_logged {
                            log!("edit render: context readback request limit reached; frame {n} strip waits");
                            self.readback_wait_logged = true;
                        }
                        None
                    }
                    Err(error) => return self.fail(cx, format!("frame {n}: {error}")),
                },
                None => None,
            };
            let reading = Reading { n, slot, targets, pieces: Vec::new(), ticket, shared, after: self.recorded_after, last };
            let strips = reading.output_strips() > 1;
            self.reading = Some(reading);
            self.recorded = None;
            // A frame whose output is read back in strips reads the strip
            // targets the next record would draw again: that one waits.
            if strips {
                return;
            }
        }
        // The next frame records while the last one is read back (the
        // output target is double-buffered, the pictures are a ring): one
        // frame per paint.
        if self.reading.as_ref().is_some_and(|reading| reading.output_strips() > 1) {
            return;
        }
        if self.next >= self.total || self.queue.len() + self.converting.len() + self.converted.len() >= ENCODE_QUEUE + CONVERT_AHEAD || !self.engine.picture_free() {
            return;
        }
        let n = self.next;
        // Its output is read back in strips only when it is read at all.
        let rows = if self.wants_output(n) { self.output_rows } else { None };
        self.engine.set_readback_strips(rows);
        let at = std::time::Instant::now();
        self.started_at.get_or_insert(at);
        let after = cx.frame_submission_serial();
        let recorded = self.engine.record(cx, audio::frame_time(n, self.fps));
        StageTimes::add(&self.times.record, at);
        match recorded {
            Ok(Some(_)) => {
                self.recorded_after = after;
                self.recorded_at = Some(std::time::Instant::now());
                self.recorded = Some(n);
                self.next = n + 1;
                self.state = RenderState::Rendering { frame: n - self.first, frames: self.total - self.first };
            }
            Ok(None) => {
                let what = self.engine.waiting_for().to_string();
                self.watch(cx, what);
            }
            Err(error) => self.fail(cx, format!("frame {n}: {error}")),
        }
    }
}

/// `<dir>/<stem>.partial.<ext>`: the encoder's file until it is whole (the
/// platform writer picks its container by extension).
pub fn partial_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "render".into());
    let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "mp4".into());
    path.with_file_name(format!("{stem}.partial.{ext}"))
}

/// `<dir>/<stem>.<node>.<ext>`.
fn sidecar_path(path: &Path, node: &str, extension: &str) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "render".into());
    path.with_file_name(format!("{stem}.{node}.{extension}"))
}

/// The colour an edit's output is written and tagged in (`output.matrix`,
/// `output.range`): BT.601 with the 525-line primaries, BT.2020 with its
/// own, both SDR.
pub fn output_color(output: &OutputFormat) -> VideoColor {
    let (matrix, primaries) = match output.matrix {
        OutputMatrix::Bt709 => (VideoMatrix::Bt709, VideoPrimaries::Bt709),
        OutputMatrix::Bt601 => (VideoMatrix::Bt601, VideoPrimaries::Smpte170m),
        OutputMatrix::Bt2020 => (VideoMatrix::Bt2020, VideoPrimaries::Bt2020),
    };
    let range = match output.range {
        OutputRange::Limited => VideoRange::Limited,
        OutputRange::Full => VideoRange::Full,
    };
    VideoColor { matrix, range, primaries, transfer: VideoTransfer::Sdr }
}

/// Convert one read-back frame to tight top-down RGBA, hash it, push it and
/// its share of the mixdown.
/// What a frame's conversion needs to know (a copy per frame, so frames
/// convert in parallel ahead of the one sequential encoder).
#[derive(Clone)]
struct ConvertSpec {
    size: (usize, usize),
    hash: bool,
    poster_frame: u64,
    dump: Option<(PathBuf, u64)>,
    dump_raw: bool,
    plan: PicturePlan,
    alpha: bool,
    maker: Option<Arc<std::sync::Mutex<GpuPictureMaker>>>,
}

/// An encoder's picture: NV12 or (alpha) RGBA bytes read back, or a
/// picture in GPU memory.
enum Picture {
    Nv12(Vec<u8>),
    Rgba(Vec<u8>),
    Gpu(MovablePicture),
}

/// A GPU picture travels from the worker that made it to the encoder's.
struct MovablePicture(GpuPicture);

// SAFETY: a picture is used by one thread at a time (made, then pushed).
unsafe impl Send for MovablePicture {}

/// One frame converted for the encoder: its picture, the slot whose
/// targets it came from, its hash (0 when not asked) and, for the middle
/// frame, the poster.
struct Converted {
    n: u64,
    slot: usize,
    picture: Picture,
    hash: u64,
    poster: Option<(Vec<u32>, u32, u32)>,
}

/// A frame off the GPU to the encoder's picture: its strips joined (or
/// its shared texture copied into the encoder's buffer, on the GPU), and,
/// when its output was read back, its hash, poster and dump. Runs on a
/// pool worker, several frames at once.
fn convert(spec: ConvertSpec, frame: Readback) -> Result<Converted, String> {
    let (ew, eh) = spec.plan.size;
    let picture = match (frame.shared, spec.plan.form) {
        (Some(handle), form) => {
            let maker = spec.maker.as_ref().ok_or("a shared picture without a picture maker")?;
            let mut maker = maker.lock().unwrap_or_else(|e| e.into_inner());
            let picture = match form {
                PictureForm::Nv12 => maker.nv12_picture(handle, ew, eh, spec.plan.color),
                PictureForm::Bgra => maker.bgra_picture(handle, [0, 0, ew, eh]),
            };
            Picture::Gpu(MovablePicture(picture.map_err(|e| format!("frame {}: picture: {e}", frame.n))?))
        }
        (None, form) => {
            let (texels, _) = spec.plan.texels();
            let (row, keep) = match form {
                PictureForm::Nv12 => (texels as usize * 4, ew as usize),
                PictureForm::Bgra => (ew as usize * 4, ew as usize * 4),
            };
            let mut bytes = Vec::with_capacity(frame.picture.iter().map(|piece| piece.height * keep).sum());
            for piece in &frame.picture {
                if row == keep {
                    bytes.extend_from_slice(&piece.data[..piece.height * row]);
                } else {
                    for r in 0..piece.height {
                        bytes.extend_from_slice(&piece.data[r * row..r * row + keep]);
                    }
                }
            }
            match form {
                PictureForm::Nv12 => Picture::Nv12(bytes),
                PictureForm::Bgra => Picture::Rgba(bytes),
            }
        }
    };
    if frame.output.is_empty() {
        return Ok(Converted { n: frame.n, slot: frame.slot, picture, hash: 0, poster: None });
    }
    let (w, h) = spec.size;
    let mut rgba = vec![0u8; w * h * 4];
    for piece in &frame.output {
        let dst = &mut rgba[piece.y * w * 4..(piece.y + piece.height) * w * 4];
        if spec.alpha {
            dst.copy_from_slice(&piece.data[..dst.len()]);
        } else {
            opaque_rgba_into(&piece.data, dst);
        }
    }
    // The frame's hash (renders compare by them) costs a pass over every
    // byte: only when asked.
    let hash = if spec.hash { fnv1a(&rgba) } else { 0 };
    let dump = spec.dump.as_ref().filter(|(_, every)| *every > 0 && frame.n % every == 0).map(|(dir, _)| dir.clone());
    let mut poster = None;
    if frame.n == spec.poster_frame || dump.is_some() {
        let pixels: Vec<u32> = rgba.chunks_exact(4).map(|p| 0xff00_0000 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect();
        if let (Some(dir), true) = (&dump, spec.dump_raw) {
            if let Err(error) = std::fs::write(dir.join(format!("frame_{:05}.rgba", frame.n)), &rgba) {
                log!("edit render: frame {} dump: {error}", frame.n);
            }
        } else {
            #[cfg(feature = "export")]
            if let Some(dir) = dump {
                let bgra: Vec<u8> = pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
                let mut jpeg = Vec::new();
                let written = jpeg_encoder::Encoder::new(&mut jpeg, 90)
                    .encode(&bgra, w as u16, h as u16, jpeg_encoder::ColorType::Bgra)
                    .map_err(|e| e.to_string())
                    .and_then(|()| std::fs::write(dir.join(format!("frame_{:05}.jpg", frame.n)), &jpeg).map_err(|e| e.to_string()));
                if let Err(error) = written {
                    log!("edit render: frame {} dump: {error}", frame.n);
                }
            }
        }
        if frame.n == spec.poster_frame {
            poster = Some((pixels, w as u32, h as u32));
        }
    }
    Ok(Converted { n: frame.n, slot: frame.slot, picture, hash, poster })
}

/// Push one converted frame and its share of the mixdown.
fn encode(mut encoder: Box<Encoder>, frame: Converted) -> Result<Box<Encoder>, String> {
    if encoder.cancel.load(Ordering::Acquire) {
        return Err("cancelled".into());
    }
    encoder.hashes.push(frame.hash);
    if frame.poster.is_some() {
        encoder.poster = frame.poster;
    }
    let (num, den) = encoder.fps;
    let pts = ((frame.n - encoder.first) as i128 * 10_000_000 * den as i128 / num as i128) as i64;
    let encoder_frames = encoder.frames;
    let Encoder { encoder: MovableEncoder(video), mix, fps, times, .. } = &mut *encoder;
    let at = std::time::Instant::now();
    match &frame.picture {
        Picture::Nv12(nv12) => video.push_frame_nv12(nv12, Some(pts)),
        Picture::Rgba(rgba) => video.push_frame_rgba8(rgba, Some(pts)),
        Picture::Gpu(MovablePicture(picture)) => video.push_gpu_picture(picture, Some(pts)),
    }.map_err(|e| format!("frame {}: {e}", frame.n))?;
    StageTimes::add(&times.encode, at);
    let at = std::time::Instant::now();
    if let Some(mix) = mix {
        // The sound runs a second ahead of the pictures: the platform
        // writer interleaves the two and stalls a picture whose sound has
        // not arrived yet. Frame 0 brings the lead's worth.
        let total = encoder_frames;
        let lead = (AUDIO_LEAD_SECONDS * fps.0 as u64 / fps.1 as u64).max(1);
        let (from, to) = if frame.n == 0 { (0, lead.min(total)) } else { (frame.n + lead - 1, frame.n + lead) };
        if from < to && from < total {
            let to = to.min(total);
            let (a, b) = (audio::frame_sample(from, *fps), audio::frame_sample(to, *fps));
            let pcm = mix.mix(a, (b - a) as usize);
            video.push_audio_i16(&pcm).map_err(|e| format!("frame {} audio: {e}", frame.n))?;
        }
    }
    StageTimes::add(&times.audio, at);
    Ok(encoder)
}

fn finish(encoder: Box<Encoder>) -> Result<Finished, String> {
    if encoder.cancel.load(Ordering::Acquire) {
        return Err("cancelled".into());
    }
    let Encoder { encoder: MovableEncoder(encoder), hashes, poster, times, .. } = *encoder;
    let at = std::time::Instant::now();
    encoder.finish().map_err(|e| format!("finish: {e}"))?;
    StageTimes::add(&times.finish, at);
    Ok((hashes, poster))
}

/// The cut's whole mix as an AAC track, for a segmented export whose
/// pictures render in parts ([`crate::mux::concat`] takes the sound from
/// here): mixed once, from sample 0 to the end of frame `frames`. The
/// platform writer needs a picture track, so the file carries a tiny black
/// one at one frame a second, which the join leaves out.
pub fn write_sound(mix: makepad_story_cut::mix::AudioMix, frames: u64, fps: (u32, u32), path: &Path, cancel: &dyn Fn() -> bool) -> Result<(), String> {
    let mut mix = Mixdown::new(Arc::new(mix))?;
    let options = VideoFileEncoderOptions {
        codec: VideoFileCodec::H264,
        width: 16,
        height: 16,
        fps_num: 1,
        fps_den: 1,
        video_bitrate_bps: 100_000,
        audio: Some(PcmAudioTrackOptions { sample_rate: SAMPLE_RATE, channels: CHANNELS, aac_bitrate_bps: 192_000 }),
        keyframe_only: false,
        ..Default::default()
    };
    let _ = std::fs::remove_file(path);
    let name = path.to_string_lossy().to_string();
    let mut encoder = VideoFileEncoder::new(&name, options).map_err(|e| format!("{name}: {e}"))?;
    let total = audio::frame_sample(frames, fps);
    let black = vec![0u8; 16 * 16 * 4];
    let second = SAMPLE_RATE as u64;
    // A picture every second, its sound pushed a second ahead of it (the
    // writer interleaves, see `encode`).
    let mut pushed = 0u64;
    let mut k = 0u64;
    loop {
        if cancel() {
            return Err("cancelled".into());
        }
        let lead = ((k + 2) * second).min(total);
        if lead > pushed {
            let pcm = mix.mix(pushed, (lead - pushed) as usize);
            encoder.push_audio_i16(&pcm).map_err(|e| format!("{name}: sound: {e}"))?;
            pushed = lead;
        }
        if k * second >= total {
            break;
        }
        encoder.push_frame_rgba8(&black, Some((k * 10_000_000) as i64)).map_err(|e| format!("{name}: {e}"))?;
        k += 1;
    }
    encoder.finish().map_err(|e| format!("{name}: finish: {e}"))
}


/// A read-back attachment (RGBA, top-down, packed on every backend) made
/// opaque.
pub fn opaque_rgba(data: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.resize(data.len(), 0);
    opaque_rgba_into(data, out);
}

/// [`opaque_rgba`] into `out` (as many bytes as it holds): a copy, then
/// alpha set in place.
pub fn opaque_rgba_into(data: &[u8], out: &mut [u8]) {
    out.copy_from_slice(&data[..out.len()]);
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_sidecar_names_sit_beside_the_render() {
        assert_eq!(partial_path(Path::new("/r/out.mp4")), Path::new("/r/out.partial.mp4"));
        assert_eq!(sidecar_path(Path::new("/r/out.mp4"), "subs", "srt"), Path::new("/r/out.subs.srt"));
    }
}
