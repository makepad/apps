//! `makepad-edit-render`: the standalone edit renderer. It draws on the
//! real GPU in its own (normally hidden) window, so Stage, CI and tests can
//! render a cut without a Stage UI in the way:
//!
//! ```text
//! MAKEPAD_HIDE_WINDOWS=1 makepad-edit-render render --edit e.splash \
//!     --media <asset id>=<file> ... --out out.mp4 [--quality draft|final] [--hashes out.txt] [--dump dir --every k [--raw]] [--blend]
//! MAKEPAD_HIDE_WINDOWS=1 makepad-edit-render render --job render.job --out out.mp4 \
//!     [--fps <rate>] [--loudness <LUFS>|off] [--no-burn]   a job (Stage's): every file where it lies; the render's settings
//! MAKEPAD_HIDE_WINDOWS=1 makepad-edit-render preview --edit e.splash --media ... --at 3.0,1.0,2.0 \
//!     [--hashes render.txt]                  frames through the CUT preview seam, checked against a render
//! MAKEPAD_HIDE_WINDOWS=1 makepad-edit-render scrub --edit e.splash --media ... --pattern fwd:2@3,play@1 \
//!     [--seconds 4] [--log frames.csv]      the preview seam driven every display frame; latency and fps
//! MAKEPAD_HIDE_WINDOWS=1 makepad-edit-render export (--job j | --edit e.splash --media ...) --out out.mp4 [--jobs N] [--parent-pipe]
//!                                          the cut in segments, N renders at once (child processes; default by
//!                                          the core count), joined losslessly; Stage's export
//! makepad-edit-render fixture <dir>        two generated test clips and a cut of them
//! makepad-edit-render probe <file.mp4>     frames, size, rate and sound of a file
//! makepad-edit-render compare <a> <b>      decoded pictures and sound, frame by frame
//! makepad-edit-render frame-check <mp4> <dir>   PSNR (and alpha error) against `render --dump <dir> --every k --raw`
//! makepad-edit-render av-check <mp4> [--flashes n,…]   every flash frame has its click within 1 ms
//! ```
//!
//! `render` and `export` draw offscreen; with `MAKEPAD_PAINT_INTERVAL=0.001` beside
//! `MAKEPAD_HIDE_WINDOWS=1` the hidden window paints frames back to back instead
//! of one per display refresh (Stage's renderer and export parts set it).
//!
//! A `--media` table stands in for the asset store (`LocalMedia`); a run's
//! render is keyed by its render revision. A `--job` (`job::Job`) names the
//! cut and every file it reads where they lie: an in-place folder's own
//! files, the asset store's cached blobs.
//!
//! `--fps`, `--loudness` and `--no-burn` draw the cut as a render of it
//! with those settings ([`CutDoc::render_as`]); without `--loudness`, the
//! cut's own.

use makepad_asset_data::{AssetId, BlobId};
use makepad_story_edit_render::render::{fnv1a, opaque_rgba};
use makepad_story_edit_render::{CutEngine, EngineStatus};
use makepad_story_edit_render::{CutJob, CutPreview, CutThumbnail, CutRenderer, LocalMedia, PreviewSource, PreviewStatus, RenderOptions, RenderQuality, RenderState};
use makepad_story_edit_voice::{Transcript, TranscriptLine};
use makepad_story_cut::{CutDoc, Word};
use makepad_widgets::makepad_platform::video_file::{PcmAudioTrackOptions, VideoFileCodec, VideoFileDecoder, VideoFileEncoder, VideoFileEncoderOptions};
use makepad_widgets::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.CutRenderHostBase = #(CutRenderHost::register_widget(vm))
    mod.widgets.CutRenderHost = set_type_default() do mod.widgets.CutRenderHostBase{
        width: Fill
        height: Fill
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Edit render"
                window.inner_size: vec2(480, 300)
                body +: {
                    host := mod.widgets.CutRenderHost{}
                }
            }
        }
    }
}

/// What the command line asked for.
enum Command {
    Render { edit: PathBuf, media: LocalMedia, options: RenderOptions, hashes: Option<PathBuf>, settings: Option<Settings> },
    Preview { edit: PathBuf, media: LocalMedia, times: Vec<String>, hashes: Option<PathBuf> },
    /// Moves of the playhead, one per display frame, as the CUT page makes
    /// them while scrubbing or playing (see [`ScrubRun`]).
    /// A segmented export (see [`ExportRun`]); `forward` are the cut and
    /// media arguments the part renders get.
    Export { edit: PathBuf, media: LocalMedia, forward: Vec<String>, out: PathBuf, jobs: usize, settings: Option<Settings> },
    Scrub { edit: PathBuf, media: LocalMedia, patterns: Vec<String>, seconds: f64, log: Option<PathBuf>, cold: bool, black: bool, hashes: Option<PathBuf>, prewarm: bool },
    /// The cut's frame at `at` (seconds, or `@n`), `width` wide, as JPEG.
    Thumbnail { edit: PathBuf, media: LocalMedia, at: String, width: u32, out: PathBuf },
    Fixture(PathBuf),
    Probe(PathBuf),
    AvCheck(PathBuf, Option<Vec<u64>>),
    BandCheck(PathBuf, String),
    /// `tolerance`: the largest mean absolute difference (0..255) a frame
    /// may have; `None` is byte for byte.
    Compare(PathBuf, PathBuf, Option<f64>),
    FrameCheck(PathBuf, PathBuf),
}

/// A render's settings (`--fps`, `--loudness`, `--no-burn`).
#[derive(Clone, Copy)]
struct Settings {
    fps: Option<f64>,
    /// `Some(None)`: `--loudness off`; `None`: the cut's own.
    loudness: Option<Option<f64>>,
    burn: bool,
}

impl Settings {
    fn parse(value: &dyn Fn(&str) -> Option<String>, burn: bool) -> Result<Option<Settings>, String> {
        let fps = value("--fps").map(|fps| fps.parse::<f64>().ok().filter(|fps| *fps > 0.0).ok_or_else(|| format!("--fps: `{fps}`"))).transpose()?;
        let loudness = value("--loudness").map(|lufs| match lufs.as_str() {
            "off" => Ok(None),
            _ => lufs.parse::<f64>().map(Some).map_err(|_| format!("--loudness: `{lufs}` (LUFS, or off)")),
        }).transpose()?;
        Ok((fps.is_some() || loudness.is_some() || !burn).then_some(Settings { fps, loudness, burn }))
    }

    /// The flags that give these settings again (an export's parts).
    fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(fps) = self.fps {
            args.extend(["--fps".to_string(), fps.to_string()]);
        }
        if let Some(loudness) = self.loudness {
            args.extend(["--loudness".to_string(), loudness.map_or_else(|| "off".to_string(), |lufs| lufs.to_string())]);
        }
        if !self.burn {
            args.push("--no-burn".into());
        }
        args
    }
}

/// Parts an export renders at once without `--jobs`: each is a process
/// with its own GPU context and encoder session, so the GPU and the media
/// engine bound them, not the cores; a part per three cores, at most four.
fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(1, |n| (n.get() / 3).clamp(1, 4))
}

fn usage() -> String {
    "usage: makepad-edit-render render (--job <render.job> | --edit <cut.splash> [--media <id>=<file>]... [--media-file <list>]) --out <out.mp4> \
     [--fps <rate>] [--loudness <LUFS>|off] [--no-burn] [--quality draft|final] [--hashes <file>] [--dump <dir> --every <k>] [--parent-pipe] | fixture <dir> | probe <file.mp4> | compare <a.mp4> <b.mp4> | frame-check <file.mp4> <dump dir> \
     | thumbnail --edit <cut.splash> --media-file <list> [--at <s>|@n] [--width <px>] --out <thumb.jpg> \
     | av-check <file.mp4> [--flashes n,n,...] | band-check <file.mp4> --safe reels|tiktok|shorts"
        .into()
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut args = args.iter().skip(1).filter(|arg| !arg.starts_with("--remote") && *arg != "--stdin-loop");
    let mode = args.next().ok_or_else(usage)?;
    let rest: Vec<&String> = args.collect();
    let value = |name: &str| rest.iter().position(|arg| *arg == name).and_then(|at| rest.get(at + 1)).map(|v| v.to_string());
    match mode.as_str() {
        "render" | "preview" | "thumbnail" | "scrub" | "export" => {
            if value("--job").is_some() && ["--edit", "--media", "--media-file"].iter().any(|name| value(name).is_some()) {
                return Err("give a render job, or standalone edit and media files".into());
            }
            let settings = Settings::parse(&value, !rest.iter().any(|arg| *arg == "--no-burn"))?;
            let (edit, mut media) = if let Some(job) = value("--job") {
                makepad_story_edit_render::job::Job::read(Path::new(&job))?.load()?
            } else {
                (PathBuf::from(value("--edit").ok_or_else(usage)?), LocalMedia::new())
            };
            let mut pairs: Vec<String> = rest.windows(2).filter(|pair| pair[0] == "--media").map(|pair| pair[1].to_string()).collect();
            if let Some(list) = value("--media-file") {
                let text = std::fs::read_to_string(&list).map_err(|e| format!("{list}: {e}"))?;
                let base = Path::new(&list).parent().unwrap_or(Path::new("."));
                for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
                    let (id, file) = line.split_once('=').ok_or_else(|| format!("{list}: `{line}` is not id=file"))?;
                    pairs.push(format!("{id}={}", base.join(file).display()));
                }
            }
            for pair in pairs {
                let (id, file) = pair.split_once('=').ok_or_else(|| format!("--media {pair}: expected <id>=<file>"))?;
                let id = id.trim();
                if let Some(id) = id.strip_prefix("lyrics:") {
                    // A voice take's or a transcript's word times.
                    media.insert_lyrics(id, file.trim());
                } else if let Some(id) = id.strip_prefix("run:") {
                    // An app run render's run.json, by the render's id.
                    media.insert_run(id, file.trim());
                } else if let Some(id) = id.strip_prefix("effect:") {
                    // A library VjEffect's source (a Splash file).
                    let source = std::fs::read_to_string(file.trim()).map_err(|e| format!("{}: {e}", file.trim()))?;
                    media.insert_effect(id, source);
                } else if let Some(id) = id.strip_prefix("edit:") {
                    // A nested cut's source.
                    media.insert_edit(id, file.trim());
                } else {
                    media.insert(id, file.trim());
                }
            }
            let hashes = value("--hashes").map(PathBuf::from);
            if mode == "export" {
                let out = PathBuf::from(value("--out").ok_or_else(usage)?);
                let jobs = value("--jobs").map(|j| j.parse::<usize>().map_err(|_| format!("--jobs: `{j}`"))).transpose()?.unwrap_or_else(default_jobs).max(1);
                let mut forward = match value("--job") {
                    Some(job) => vec!["--job".into(), job],
                    None => vec!["--edit".to_string(), edit.to_string_lossy().to_string()],
                };
                forward.extend(settings.iter().flat_map(Settings::args));
                for pair in rest.windows(2).filter(|pair| pair[0] == "--media" || pair[0] == "--media-file") {
                    forward.push(pair[0].to_string());
                    forward.push(pair[1].to_string());
                }
                if rest.iter().any(|arg| *arg == "--blend") {
                    forward.push("--blend".into());
                }
                for name in ["--quality", "--bitrate"] {
                    if let Some(v) = value(name) {
                        forward.push(name.into());
                        forward.push(v);
                    }
                }
                return Ok(Command::Export { edit, media, forward, out, jobs, settings });
            }
            if mode == "thumbnail" {
                let at = value("--at").unwrap_or_else(|| "0".into());
                let width = value("--width").map(|w| w.parse::<u32>().map_err(|_| format!("--width: `{w}`"))).transpose()?.unwrap_or(512);
                let out = PathBuf::from(value("--out").ok_or_else(usage)?);
                return Ok(Command::Thumbnail { edit, media, at, width, out });
            }
            if mode == "scrub" {
                let patterns = value("--pattern").unwrap_or_else(|| "play@0".into()).split(',').map(|p| p.trim().to_string()).collect();
                let seconds = value("--seconds").and_then(|s| s.parse().ok()).unwrap_or(4.0);
                // `--cold`: the first pattern starts at once, as a person
                // opening the cut and pressing play (no warm-up); `--black`:
                // every picture shown is read back, and any all-black one is
                // checked again once warm (a first-time miss if it then has
                // content).
                let cold = rest.iter().any(|arg| *arg == "--cold");
                let black = rest.iter().any(|arg| *arg == "--black");
                // `--prewarm`: the patterns start once the preview has drawn
                // its frames ahead (every graphic ready), as after an idle
                // moment in Stage.
                let prewarm = rest.iter().any(|arg| *arg == "--prewarm");
                return Ok(Command::Scrub { edit, media, patterns, seconds, log: value("--log").map(PathBuf::from), cold, black, hashes, prewarm });
            }
            if mode == "preview" {
                // Seconds, or `@n`: frame n of the cut, at exactly the
                // time a render gives it.
                let times = value("--at").ok_or_else(usage)?.split(',').map(|t| t.trim().to_string()).collect();
                return Ok(Command::Preview { edit, media, times, hashes });
            }
            let path = PathBuf::from(value("--out").ok_or_else(usage)?);
            let dump = match (value("--dump"), value("--every")) {
                (Some(dir), every) => Some((PathBuf::from(dir), every.and_then(|k| k.parse().ok()).unwrap_or(1))),
                _ => None,
            };
            let mut options = RenderOptions::new(path);
            options.dump = dump;
            options.dump_raw = rest.iter().any(|arg| *arg == "--raw");
            // `--bitrate bps`: the render card's `bitrate` high.
            if let Some(bitrate) = value("--bitrate") {
                options.bitrate = Some(bitrate.parse().map_err(|_| format!("--bitrate: `{bitrate}`"))?);
            }
            // `--range a,b`: only frames [a, b) (a segment of an export),
            // without sound or captions.
            if let Some(range) = value("--range") {
                let parse = |n: &str| n.trim().parse::<u64>().map_err(|_| format!("--range: `{range}`"));
                let (a, b) = range.split_once(',').ok_or_else(|| format!("--range: `{range}` is not a,b"))?;
                options.range = Some((parse(a)?, parse(b)?));
            }
            // `--quality draft|final`: draft draws each graphic once a frame
            // (as the preview), final samples motion blur (the default).
            if let Some(quality) = value("--quality") {
                options.quality = RenderQuality::parse(&quality).ok_or_else(|| format!("--quality: `{quality}` (draft or final)"))?;
            }
            // `--blend`: clips at another rate mix their two nearest frames.
            options.clip_blend = rest.iter().any(|arg| *arg == "--blend");
            // Frame hashes cost a pass over every pixel: only when asked.
            options.hashes = hashes.is_some();
            Ok(Command::Render { edit, media, options, hashes, settings })
        }
        "fixture" => Ok(Command::Fixture(PathBuf::from(rest.first().ok_or_else(usage)?.as_str()))),
        "probe" => Ok(Command::Probe(PathBuf::from(rest.first().ok_or_else(usage)?.as_str()))),
        "band-check" => {
            let path = PathBuf::from(rest.first().ok_or_else(usage)?.as_str());
            Ok(Command::BandCheck(path, value("--safe").ok_or_else(usage)?))
        }
        "av-check" => {
            let path = PathBuf::from(rest.first().ok_or_else(usage)?.as_str());
            let expected = match value("--flashes") {
                Some(list) => Some(list.split(',').map(|n| n.trim().parse::<u64>().map_err(|_| format!("--flashes: `{n}`"))).collect::<Result<Vec<_>, _>>()?),
                None => None,
            };
            Ok(Command::AvCheck(path, expected))
        }
        "frame-check" => match rest.as_slice() {
            [file, dir, ..] => Ok(Command::FrameCheck(PathBuf::from(file.as_str()), PathBuf::from(dir.as_str()))),
            _ => Err(usage()),
        },
        "compare" => match rest.as_slice() {
            [a, b, ..] => {
                let tolerance = match value("--tolerance") {
                    Some(t) => Some(t.parse::<f64>().map_err(|_| format!("--tolerance: `{t}`"))?),
                    None => None,
                };
                Ok(Command::Compare(PathBuf::from(a.as_str()), PathBuf::from(b.as_str()), tolerance))
            }
            _ => Err(usage()),
        },
        _ => Err(usage()),
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct CutRenderHost {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawQuad,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    renderer: Option<CutRenderer>,
    #[rust]
    preview: Option<PreviewRun>,
    #[rust]
    scrub: Option<ScrubRun>,
    #[rust]
    export: Option<ExportRun>,
    #[rust]
    thumbnail: Option<(CutThumbnail, PathBuf)>,
    /// `--at` times still to take, and the lines of those taken.
    #[rust]
    thumbnail_times: Option<(std::collections::VecDeque<f64>, Vec<String>)>,
    #[rust]
    hashes: Option<PathBuf>,
    #[rust]
    started: f64,
    #[rust]
    last_report: u64,
    /// A command that runs on the task pool: its report, or why it failed.
    #[rust]
    task: Option<TaskHandle<Result<String, String>>>,
    /// `cancel` lines from the parent (`--parent-pipe`).
    #[rust]
    parent: Option<std::sync::mpsc::Receiver<()>>,
}

impl CutRenderHost {
    fn start(&mut self, cx: &mut Cx, command: Command) {
        self.started = Cx::monotonic_now();
        match command {
            Command::Render { edit, media, options, hashes, settings } => {
                let doc = load_edit(&edit, &media, settings);
                println!(
                    "edit-render: {} {}x{} at {} fps, {} s -> {}",
                    edit.display(), doc.output.width, doc.output.height, doc.output.fps, doc.duration, options.path.display()
                );
                if let Some((dir, _)) = &options.dump {
                    let _ = std::fs::create_dir_all(dir);
                }
                let job = CutJob { doc: Arc::new(doc), marks: HashMap::new(), resolver: Box::new(media), audio: true };
                self.renderer = Some(CutRenderer::new(cx, job, options));
                self.hashes = hashes;
            }
            Command::Thumbnail { edit, media, at, width, out } => {
                let doc = Arc::new(load_edit(&edit, &media, None));
                let fps = makepad_story_edit_render::audio::fps_ratio(doc.output.fps);
                // Several times (`--at 1.0,@45`): one engine, retargeted.
                let mut times: std::collections::VecDeque<f64> = at.split(',').map(|at| at.trim()).map(|at| match at.strip_prefix('@') {
                    Some(n) => n.parse::<u64>().map(|n| makepad_story_edit_render::audio::frame_time(n, fps)).map_err(|_| at.to_string()),
                    None => at.parse::<f64>().map_err(|_| at.to_string()),
                }.unwrap_or_else(|at| finish(Err(format!("--at: `{at}`"))))).collect();
                let t = times.pop_front().unwrap_or(0.0);
                let job = CutJob { doc, marks: HashMap::new(), resolver: Box::new(media), audio: false };
                self.thumbnail = Some((CutThumbnail::new(cx, job, t, width), out));
                self.thumbnail_times = Some((times, Vec::new()));
            }
            Command::Preview { edit, media, times, hashes } => {
                let doc = Arc::new(load_edit(&edit, &media, None));
                let reference = match &hashes {
                    Some(path) => match std::fs::read_to_string(path) {
                        Ok(text) => text.lines().filter_map(|line| line.split_once(' ')).filter_map(|(_, hash)| u64::from_str_radix(hash, 16).ok()).collect(),
                        Err(error) => finish(Err(format!("{}: {error}", path.display()))),
                    },
                    None => Vec::new(),
                };
                let fps = makepad_story_edit_render::audio::fps_ratio(doc.output.fps);
                let times = times
                    .iter()
                    .map(|t| match t.strip_prefix('@') {
                        Some(n) => n.parse::<u64>().map(|n| makepad_story_edit_render::audio::frame_time(n, fps)).map_err(|_| t.clone()),
                        None => t.parse::<f64>().map_err(|_| t.clone()),
                    })
                    .collect::<Result<Vec<f64>, _>>()
                    .unwrap_or_else(|t| finish(Err(format!("--at: `{t}`"))));
                let job = CutJob { doc, marks: HashMap::new(), resolver: Box::new(media), audio: false };
                let frames = makepad_story_edit_render::audio::frame_count(job.doc.duration, fps);
                self.preview = Some(PreviewRun { preview: CutPreview::new(cx, job), times: times.into(), reading: None, readback_wait_logged: false, reference, fps, frames, report: Vec::new(), since: None });
            }
            Command::Scrub { edit, media, patterns, seconds, log, cold, black, hashes, prewarm } => {
                let doc = Arc::new(load_edit(&edit, &media, None));
                let fps = makepad_story_edit_render::audio::fps_ratio(doc.output.fps);
                let duration = doc.duration;
                let job = CutJob { doc: doc.clone(), marks: HashMap::new(), resolver: Box::new(media), audio: false };
                let preview = CutPreview::new(cx, job);
                let mut run = ScrubRun::new(preview, patterns, seconds, fps, duration, log);
                run.cold = cold;
                run.prewarm = prewarm;
                run.black = black.then(|| BlackCheck {
                    // `--hashes`: a render's; every exact picture read must be its frame.
                    reference: hashes.and_then(|path| std::fs::read_to_string(path).ok()).map_or(Vec::new(), |text| {
                        text.lines().filter_map(|line| line.split_once(' ')).filter_map(|(_, hash)| u64::from_str_radix(hash, 16).ok()).collect()
                    }),
                    ..BlackCheck::default()
                });
                self.scrub = Some(run);
            }
            Command::Export { edit, media, forward, out, jobs, settings } => {
                let doc = Arc::new(load_edit(&edit, &media, settings));
                let job = CutJob { doc: doc.clone(), marks: HashMap::new(), resolver: Box::new(media), audio: true };
                self.export = Some(ExportRun::new(cx, doc, job, forward, out, jobs));
            }
            Command::Fixture(dir) => self.task = cx.task_pool().submit(Class::Background, move || make_fixture(&dir)).ok(),
            Command::Probe(path) => self.task = cx.task_pool().submit(Class::Background, move || probe(&path).map(|p| p.report())).ok(),
            Command::BandCheck(path, preset) => self.task = cx.task_pool().submit(Class::Background, move || band_check(&path, &preset)).ok(),
            Command::AvCheck(path, expected) => self.task = cx.task_pool().submit(Class::Background, move || av_check(&path, expected)).ok(),
            Command::FrameCheck(file, dir) => self.task = cx.task_pool().submit(Class::Background, move || frame_check(&file, &dir)).ok(),
            Command::Compare(a, b, tolerance) => self.task = cx.task_pool().submit(Class::Background, move || compare(&a, &b, tolerance)).ok(),
        }
        self.next_frame = cx.new_next_frame();
    }

    fn check(&mut self, cx: &mut Cx) {
        if let Some((thumbnail, out)) = &mut self.thumbnail {
            match thumbnail.result() {
                Some(Ok(thumb)) => {
                    let (times, lines) = self.thumbnail_times.get_or_insert_with(Default::default);
                    // The first keeps `--out`; later ones `<stem>_<i>.jpg`.
                    let path = match lines.len() {
                        0 => out.clone(),
                        i => out.with_file_name(format!("{}_{i}.jpg", out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())),
                    };
                    let mut jpeg = Vec::new();
                    let written = jpeg_encoder::Encoder::new(&mut jpeg, 90)
                        .encode(&thumb.rgba, thumb.width as u16, thumb.height as u16, jpeg_encoder::ColorType::Rgba)
                        .map_err(|e| e.to_string())
                        .and_then(|()| std::fs::write(&path, &jpeg).map_err(|e| format!("{}: {e}", path.display())));
                    if let Err(error) = written {
                        finish(Err(error));
                    }
                    let rgba = makepad_story_edit_render::render::fnv1a(&thumb.rgba);
                    lines.push(format!(
                        "edit-render: thumbnail {}x{} of the frame at {} s (RGBA hash {rgba:016x}) -> {}",
                        thumb.width, thumb.height, thumb.t, path.display()
                    ));
                    match times.pop_front() {
                        Some(t) => {
                            if !thumbnail.retarget(t) {
                                finish(Err("retarget refused".into()));
                            }
                        }
                        None => finish(Ok(lines.join("\n"))),
                    }
                }
                Some(Err(error)) => finish(Err(error.clone())),
                None => {}
            }
        }
        if let Some(run) = &mut self.preview {
            run.check(cx);
        }
        if let Some(run) = &mut self.export {
            let cancel = self.parent.as_ref().is_some_and(|parent| parent.try_recv().is_ok());
            run.check(cx, cancel);
        }
        if let Some(task) = &mut self.task {
            if let Some(result) = task.try_take() {
                finish(result.unwrap_or_else(|error| Err(format!("worker: {error:?}"))));
            }
        }
        let Some(renderer) = &mut self.renderer else { return };
        if self.parent.as_ref().is_some_and(|parent| parent.try_recv().is_ok()) {
            renderer.cancel(cx);
        }
        // A failed or cancelled render leaves only once its encoder has let
        // go, so no partial file outlives it.
        if matches!(renderer.state(), RenderState::Failed(_) | RenderState::Cancelled) && !renderer.settled() {
            return;
        }
        match renderer.state().clone() {
            // The first frame drawn (recording frame 1 means frame 0 is whole).
            RenderState::Rendering { frame: 1, frames } if self.last_report == 1 => {
                self.last_report = 2;
                println!("edit-render: frame 1 / {frames} ({:.1} s)", Cx::monotonic_now() - self.started);
            }
            RenderState::Rendering { frame, frames } if frame >= self.last_report + 30 || (frame == 0 && self.last_report == 0) => {
                self.last_report = frame.max(1);
                println!("edit-render: frame {frame} / {frames} ({:.1} s)", Cx::monotonic_now() - self.started);
            }
            RenderState::Done(result) => {
                let elapsed = Cx::monotonic_now() - self.started;
                if let Some(path) = &self.hashes {
                    let text: String = result.frame_hashes.iter().enumerate().map(|(n, hash)| format!("{n} {hash:016x}\n")).collect();
                    if let Err(error) = std::fs::write(path, text) {
                        finish(Err(format!("{}: {error}", path.display())));
                    }
                }
                // Frames are hashed only with `--hashes`.
                let combined = match self.hashes.is_some() {
                    true => format!("; frames hash {:016x}", makepad_story_edit_render::render::fnv1a(
                        &result.frame_hashes.iter().flat_map(|hash| hash.to_le_bytes()).collect::<Vec<u8>>(),
                    )),
                    false => String::new(),
                };
                for captions in result.captions() {
                    println!("edit-render: captions of `{}`: {} {}", captions.node, captions.srt.display(), captions.vtt.display());
                }
                finish(Ok(format!(
                    "edit-render: done {} frames {}x{} at {}/{} fps, audio {}, in {elapsed:.1} s ({:.1} fps){combined} -> {}",
                    result.frames, result.size.0, result.size.1, result.fps.0, result.fps.1, result.audio,
                    result.frames as f64 / elapsed.max(1e-3), result.path.display()
                )))
            }
            RenderState::Failed(error) => finish(Err(error)),
            RenderState::Cancelled => finish(Err("cancelled".into())),
            _ => {}
        }
    }
}

fn load_edit(edit: &Path, media: &LocalMedia, settings: Option<Settings>) -> CutDoc {
    let source = match std::fs::read_to_string(edit) {
        Ok(source) => source,
        Err(error) => finish(Err(format!("{}: {error}", edit.display()))),
    };
    match match &media.tree { Some((tree, path)) => CutDoc::load_tree(&source, tree.context(path)), None => CutDoc::load_named(&source, &edit.to_string_lossy()) } {
        Ok(mut doc) => {
            if let Some(settings) = settings {
                let loudness = settings.loudness.unwrap_or(doc.output.loudness);
                doc.render_as(settings.fps, loudness, settings.burn);
            }
            doc
        }
        Err(errors) => {
            let text: Vec<String> = errors.iter().map(|e| format!("{}:{e}", edit.display())).collect();
            finish(Err(text.join("\n")))
        }
    }
}

/// How long the preview harness waits for one seek's frame (a busy
/// machine compiling shaders for the first frames can take long).
const PREVIEW_STALL_SECONDS: f64 = 120.0;

/// The preview seam driven the way the CUT page drives it: seek, draw
/// until idle, then read the shown frame back and hash it like a render.
struct PreviewRun {
    preview: CutPreview,
    times: std::collections::VecDeque<f64>,
    reading: Option<(f64, ReadbackTicket)>,
    readback_wait_logged: bool,
    /// A render's frame hashes to check against.
    reference: Vec<u64>,
    fps: (u32, u32),
    frames: u64,
    report: Vec<String>,
    /// When the current seek was first asked for.
    since: Option<f64>,
}

impl PreviewRun {
    /// The render frame a seek to `t` shows: the one `t` falls in.
    fn frame_of(&self, t: f64) -> u64 {
        ((t.max(0.0) * self.fps.0 as f64 / self.fps.1 as f64 + 1e-9).floor() as u64).min(self.frames.saturating_sub(1))
    }

    fn draw(&mut self, cx: &mut Cx2d) {
        if let Some(&t) = self.times.front() {
            self.preview.seek(t);
            let since = *self.since.get_or_insert(Cx::monotonic_now());
            if !self.readback_wait_logged && Cx::monotonic_now() - since > PREVIEW_STALL_SECONDS {
                finish(Err(format!("preview at {t} s: no frame after {PREVIEW_STALL_SECONDS} s ({:?})", self.preview.status())));
            }
        }
        self.preview.draw(cx);
        if self.reading.is_some() {
            return;
        }
        let (Some(&t), PreviewStatus::Idle) = (self.times.front(), self.preview.status()) else { return };
        let Some((shown, texture)) = self.preview.frame() else { return };
        if shown != makepad_story_edit_render::audio::frame_time(self.frame_of(t), self.fps) {
            return;
        }
        match texture.read_back(cx, ReadbackRequest::default()) {
            Ok(ticket) => {
                if self.readback_wait_logged { self.since = Some(Cx::monotonic_now()); }
                self.readback_wait_logged = false;
                self.reading = Some((t, ticket));
            }
            Err(ReadbackError::Backpressure) => {
                if !self.readback_wait_logged {
                    log!("edit preview: context readback request limit reached; preview frame waits");
                    self.readback_wait_logged = true;
                }
            }
            Err(error) => finish(Err(format!("preview readback: {error}"))),
        }
    }

    fn readback(&mut self, cx: &mut Cx) {
        let Some((t, ticket)) = self.reading else { return };
        for result in cx.try_take_texture_readbacks_for(&[ticket]) {
            self.reading = None;
            let data = result.data.unwrap_or_else(|error| finish(Err(format!("preview readback: {error}"))));
            let mut rgba = Vec::new();
            opaque_rgba(&data, &mut rgba);
            let hash = fnv1a(&rgba);
            let n = self.frame_of(t) as usize;
            let verdict = match self.reference.get(n) {
                Some(&expected) if expected == hash => "= render frame".to_string(),
                Some(&expected) => format!("!= render frame ({expected:016x})"),
                None => String::new(),
            };
            if verdict.starts_with("!=") {
                // The preview's picture beside the reference, for a look.
                let bgra: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
                let mut jpeg = Vec::new();
                let path = std::env::temp_dir().join(format!("edit_preview_frame_{n:05}.jpg"));
                if jpeg_encoder::Encoder::new(&mut jpeg, 90).encode(&bgra, result.width as u16, result.height as u16, jpeg_encoder::ColorType::Bgra).is_ok() {
                    let _ = std::fs::write(&path, &jpeg);
                }
                finish(Err(format!("preview at {t} s (frame {n}) {hash:016x} {verdict}; picture {}", path.display())));
            }
            println!("  t {t} s (frame {n}): {hash:016x} {verdict}");
            self.report.push(format!("  t {t} s (frame {n}): {hash:016x} {verdict}"));
            self.times.pop_front();
            self.since = None;
        }
    }

    fn check(&mut self, _cx: &mut Cx) {
        if let PreviewStatus::Failed(error) = self.preview.status() {
            finish(Err(error));
        }
        if self.times.is_empty() && self.reading.is_none() {
            finish(Ok(format!("edit-render: preview frames\n{}", self.report.join("\n"))));
        }
    }
}

/// A segmented export: the cut's frames in segments
/// ([`makepad_story_edit_render::segments::segment_frames`]), each rendered on
/// locked time by a child `render --range` process (at most `jobs` at
/// once, each in its own hidden window), the sound mixed once here, and
/// the parts joined without re-encoding ([`makepad_story_edit_render::mux`]).
/// Every frame is drawn on locked time whatever part it falls in, so the
/// pictures do not depend on how many render at once; only where a part
/// (and its key frame) begins does: the cut's share per part at once. A part that fails fails the export with
/// its own words (a refused effect names its node); `cancel` on stdin (with
/// `--parent-pipe`) stops every part and removes what was written.
struct ExportRun {
    engine: CutEngine,
    doc: Arc<CutDoc>,
    forward: Vec<String>,
    out: PathBuf,
    parts_dir: PathBuf,
    jobs: usize,
    total: u64,
    segments: Vec<(u64, u64)>,
    next: usize,
    running: Vec<ExportPart>,
    /// Frames of finished parts.
    finished: u64,
    parts_done: usize,
    sound: ExportStep,
    join: Option<TaskHandle<Result<usize, String>>>,
    started: f64,
    reported: f64,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

struct ExportPart {
    index: usize,
    frames: u64,
    done: u64,
    child: std::process::Child,
    lines: std::sync::mpsc::Receiver<String>,
    _readers: Vec<TaskHandle<()>>,
    last_error: Option<String>,
}

enum ExportStep {
    Waiting,
    Running(TaskHandle<Result<(), String>>),
    Done,
    /// The cut has no sound: its file has none either.
    Silent,
}

impl ExportRun {
    fn new(cx: &mut Cx, doc: Arc<CutDoc>, job: CutJob, forward: Vec<String>, out: PathBuf, jobs: usize) -> Self {
        let fps = makepad_story_edit_render::audio::fps_ratio(doc.output.fps);
        let total = makepad_story_edit_render::audio::frame_count(doc.duration, fps);
        let segments = makepad_story_edit_render::segments::segment_frames(&doc, jobs);
        let stem = out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "export".into());
        let parts_dir = out.with_file_name(format!("{stem}.parts"));
        let _ = std::fs::remove_dir_all(&parts_dir);
        if let Err(error) = std::fs::create_dir_all(&parts_dir) {
            finish(Err(format!("{}: {error}", parts_dir.display())));
        }
        println!("edit-render: export {} {}x{} at {} fps, {total} frames in {} segments, {jobs} at once -> {}", doc.label, doc.output.width, doc.output.height, doc.output.fps, segments.len(), out.display());
        Self {
            engine: CutEngine::new(cx, job), doc, forward, out, parts_dir, jobs, total, segments, next: 0, running: Vec::new(), finished: 0,
            parts_done: 0, sound: ExportStep::Waiting, join: None, started: Cx::monotonic_now(), reported: 0.0,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn part_path(&self, index: usize) -> PathBuf {
        // The parts are of the output's container (`.mov` for an alpha cut).
        let ext = self.out.extension().map_or("mp4".into(), |ext| ext.to_string_lossy().to_string());
        self.parts_dir.join(format!("part{index:04}.{ext}"))
    }

    fn sound_path(&self) -> PathBuf {
        self.parts_dir.join("sound.mp4")
    }

    /// Stop every part and leave nothing behind.
    fn abort(&mut self, error: String) -> ! {
        self.cancel.store(true, std::sync::atomic::Ordering::Release);
        for part in &mut self.running {
            let _ = part.child.kill();
            let _ = part.child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.parts_dir);
        finish(Err(error))
    }

    fn spawn(&mut self, cx: &mut Cx, index: usize) {
        use std::io::BufRead;
        let (a, b) = self.segments[index];
        let exe = std::env::current_exe().unwrap_or_else(|error| finish(Err(format!("export: {error}"))));
        let mut command = std::process::Command::new(exe);
        command
            .arg("render")
            .args(&self.forward)
            .args(["--out", &self.part_path(index).to_string_lossy(), "--range", &format!("{a},{b}"), "--parent-pipe"])
            .env("MAKEPAD_HIDE_WINDOWS", "1")
            // Offscreen frames back to back, not one per display refresh.
            .env("MAKEPAD_PAINT_INTERVAL", "0.001")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => self.abort(format!("export: part {index}: {error}")),
        };
        let (send, lines) = std::sync::mpsc::channel();
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let err_send = send.clone();
        // Both streams, line by line, until the part exits (errors marked).
        let readers = [
            cx.spawn_worker(move || {
                if let Some(stdout) = stdout {
                    for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                        let _ = send.send(line);
                    }
                }
            }),
            cx.spawn_worker(move || {
                if let Some(stderr) = stderr {
                    for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                        let _ = err_send.send(format!("! {line}"));
                    }
                }
            }),
        ];
        let mut handles = Vec::new();
        for reader in readers {
            match reader {
                Ok(reader) => handles.push(reader),
                Err(error) => self.abort(format!("export: part {index}: {error:?}")),
            }
        }
        self.running.push(ExportPart { index, frames: b - a, done: 0, child, lines, _readers: handles, last_error: None });
    }

    fn check(&mut self, cx: &mut Cx, cancel: bool) {
        if cancel {
            self.abort("cancelled".into());
        }
        if let EngineStatus::Failed(error) = self.engine.status() {
            self.abort(error);
        }
        // The sound, once the cut (and every sound in it) has loaded.
        if matches!(self.sound, ExportStep::Waiting) && self.engine.status() == EngineStatus::Ready && self.engine.sound_ready() {
            let doc = self.doc.clone();
            if self.engine.eval().audio_plan(0.0, doc.duration).segments.is_empty() {
                self.sound = ExportStep::Silent;
            } else {
                let mix = self.engine.audio_mix();
                let (total, fps, path, cancel) = (self.total, makepad_story_edit_render::audio::fps_ratio(doc.output.fps), self.sound_path(), self.cancel.clone());
                match cx.task_pool().submit(Class::Background, move || {
                    makepad_story_edit_render::render::write_sound(mix, total, fps, &path, &|| cancel.load(std::sync::atomic::Ordering::Acquire))
                }) {
                    Ok(task) => self.sound = ExportStep::Running(task),
                    Err(error) => self.abort(format!("export: sound: {error:?}")),
                }
            }
        }
        if let ExportStep::Running(task) = &mut self.sound {
            match task.try_take() {
                None => {}
                Some(Ok(Ok(()))) => self.sound = ExportStep::Done,
                Some(Ok(Err(error))) => self.abort(format!("export: sound: {error}")),
                Some(Err(error)) => self.abort(format!("export: sound: {error:?}")),
            }
        }
        // Parts: progress, exits, and the next ones.
        let mut failed = None;
        for part in &mut self.running {
            while let Ok(line) = part.lines.try_recv() {
                if let Some(rest) = line.strip_prefix("edit-render: frame ") {
                    if let Some(n) = rest.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()) {
                        part.done = n.min(part.frames);
                    }
                } else if let Some(error) = line.strip_prefix("! ") {
                    if error.contains("FAILED") || error.contains("error") {
                        part.last_error = Some(error.trim_start_matches("edit-render: FAILED: ").to_string());
                    }
                }
            }
        }
        let mut finished = Vec::new();
        for (i, part) in self.running.iter_mut().enumerate() {
            match part.child.try_wait() {
                Ok(Some(status)) if status.success() => finished.push(i),
                Ok(Some(status)) => {
                    // Its last words may still be in the channel.
                    while let Ok(line) = part.lines.recv_timeout(std::time::Duration::from_millis(50)) {
                        if let Some(error) = line.strip_prefix("! ") {
                            part.last_error = Some(error.trim_start_matches("edit-render: FAILED: ").to_string());
                        }
                    }
                    let (a, b) = self.segments[part.index];
                    failed = Some(format!("frames {a}..{b}: {}", part.last_error.clone().unwrap_or_else(|| format!("the part render exited with {status}"))));
                }
                Ok(None) => {}
                Err(error) => failed = Some(format!("part {}: {error}", part.index)),
            }
        }
        if let Some(error) = failed {
            self.abort(error);
        }
        for i in finished.into_iter().rev() {
            let part = self.running.remove(i);
            self.finished += part.frames;
            self.parts_done += 1;
        }
        while self.running.len() < self.jobs && self.next < self.segments.len() {
            let index = self.next;
            self.next += 1;
            self.spawn(cx, index);
        }
        let done = self.finished + self.running.iter().map(|part| part.done).sum::<u64>();
        let now = Cx::monotonic_now();
        if now - self.reported > 0.5 {
            self.reported = now;
            let elapsed = now - self.started;
            let eta = if done > 0 { elapsed / done as f64 * (self.total - done.min(self.total)) as f64 } else { f64::NAN };
            println!("edit-render: frame {done} / {} ({:.0}%), {:.1} s, eta {eta:.0} s", self.total, done as f64 * 100.0 / self.total.max(1) as f64, elapsed);
        }
        // Every part and the sound: join.
        let sound_ready = matches!(self.sound, ExportStep::Done | ExportStep::Silent);
        if self.join.is_none() && self.parts_done == self.segments.len() && sound_ready {
            let parts: Vec<PathBuf> = (0..self.segments.len()).map(|i| self.part_path(i)).collect();
            let sound = matches!(self.sound, ExportStep::Done).then(|| self.sound_path());
            let (out, cancel) = (self.out.clone(), self.cancel.clone());
            self.join = cx.task_pool().submit(Class::Background, move || {
                let refs: Vec<&Path> = parts.iter().map(|p| p.as_path()).collect();
                let at = std::time::Instant::now();
                makepad_story_edit_render::mux::concat(&refs, sound.as_deref(), &out, &|| cancel.load(std::sync::atomic::Ordering::Acquire))?;
                println!("edit-render: joined {} parts in {:.0} ms", refs.len(), at.elapsed().as_secs_f64() * 1000.0);
                let tracks = makepad_story_edit_render::mux::read_tracks(&out)?;
                Ok(tracks.iter().find(|t| &t.handler == b"vide").map_or(0, |t| t.samples.len()))
            }).ok();
        }
        if let Some(task) = &mut self.join {
            match task.try_take() {
                None => {}
                Some(Ok(Ok(frames))) => {
                    if frames as u64 != self.total {
                        self.abort(format!("export: {frames} frames joined, the cut has {}", self.total));
                    }
                    for (node, srt, vtt) in self.engine.sidecars() {
                        let stem = self.out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        for (ext, text) in [("srt", srt), ("vtt", vtt)] {
                            let path = self.out.with_file_name(format!("{stem}.{node}.{ext}"));
                            if let Err(error) = std::fs::write(&path, text) {
                                self.abort(format!("{}: {error}", path.display()));
                            }
                            println!("edit-render: captions {}", path.display());
                        }
                    }
                    let _ = std::fs::remove_dir_all(&self.parts_dir);
                    let elapsed = Cx::monotonic_now() - self.started;
                    finish(Ok(format!(
                        "edit-render: exported {frames} frames in {} parts, {} at once, in {elapsed:.1} s ({:.1} fps) -> {}",
                        self.segments.len(), self.jobs, frames as f64 / elapsed.max(1e-3), self.out.display()
                    )));
                }
                Some(Ok(Err(error))) => self.abort(format!("export: join: {error}")),
                Some(Err(error)) => self.abort(format!("export: join: {error:?}")),
            }
        }
    }
}

/// The scrub bench: each pattern moves the playhead once per display frame
/// for `seconds`, the way the CUT page does while its playhead is dragged
/// or plays, and records what the preview shows at every display frame.
///
/// Patterns: `play@T` (real time from T), `fwd:S@T` / `back:S@T` (S× real
/// time from T), `saw:S@T` (S× forward for a second, then back, around T),
/// `jump@T` (a random frame within 5 s of T every display frame).
struct ScrubRun {
    preview: CutPreview,
    patterns: std::collections::VecDeque<String>,
    seconds: f64,
    fps: (u32, u32),
    duration: f64,
    log: Option<PathBuf>,
    rows: Vec<String>,
    summary: Vec<String>,
    /// This pattern's start (after its first frame showed), and its log.
    started: Option<f64>,
    warm: Option<f64>,
    frames: Vec<ScrubFrame>,
    /// When each wanted frame index was first asked for.
    asked: HashMap<u64, f64>,
    seed: u64,
    /// `--cold`: the first pattern starts without a warm-up.
    cold: bool,
    /// `--prewarm`: wait for the preview's frames ahead first.
    prewarm: bool,
    /// When the bench opened the cut, and when its first exact picture
    /// showed.
    opened: f64,
    first_picture: Option<f64>,
    black: Option<BlackCheck>,
}

/// `--black`: the picture shown, read back every display frame one can be
/// (one readback at a time); the frames seen all black, again once warm.
#[derive(Default)]
struct BlackCheck {
    reading: Option<(u64, bool, ReadbackTicket)>,
    asking: Option<(u64, bool, Texture)>,
    readback_wait_logged: bool,
    pictures: usize,
    /// A render's frame hashes, and the exact pictures read that were not
    /// the render's frame (shown frame, hash).
    reference: Vec<u64>,
    wrong: Vec<u64>,
    black: std::collections::BTreeSet<u64>,
    /// After the patterns: the black frames to show again, and what they
    /// showed warm (frame, black).
    again: Option<std::collections::VecDeque<u64>>,
    warm: Vec<(u64, bool)>,
}

/// All but a few pixels black (sampled).
fn is_black(data: &[u8], width: usize, height: usize, stride: usize) -> bool {
    let (mut dark, mut all) = (0usize, 0usize);
    for y in (0..height).step_by(7) {
        let row = &data[y * stride..y * stride + width * 4];
        for px in row.chunks_exact(4).step_by(7) {
            all += 1;
            if px[0] < 10 && px[1] < 10 && px[2] < 10 {
                dark += 1;
            }
        }
    }
    all > 0 && dark * 100 >= all * 99
}

struct ScrubFrame {
    at: f64,
    want: u64,
    shown: Option<u64>,
    /// The shown frame is exactly the wanted one (no stand-in clip frame).
    exact: bool,
}

impl ScrubRun {
    fn new(preview: CutPreview, patterns: Vec<String>, seconds: f64, fps: (u32, u32), duration: f64, log: Option<PathBuf>) -> Self {
        Self {
            preview, patterns: patterns.into(), seconds, fps, duration, log, rows: vec!["pattern,at_ms,want,shown,age_ms,exact".into()], summary: Vec::new(),
            started: None, warm: None, frames: Vec::new(), asked: HashMap::new(), seed: 0x9e3779b97f4a7c15,
            cold: false, prewarm: false, opened: Cx::monotonic_now(), first_picture: None, black: None,
        }
    }

    fn frame_of(&self, t: f64) -> u64 {
        (t.max(0.0) * self.fps.0 as f64 / self.fps.1 as f64 + 1e-9).floor() as u64
    }

    /// The playhead of `pattern` at `tau` seconds into it.
    fn position(&mut self, pattern: &str, tau: f64) -> f64 {
        let pattern = pattern.split_once('~').map_or(pattern, |(pattern, _)| pattern);
        let (kind, from) = pattern.split_once('@').unwrap_or((pattern, "0"));
        let from: f64 = from.parse().unwrap_or(0.0);
        let (kind, speed) = kind.split_once(':').map(|(k, s)| (k, s.parse().unwrap_or(1.0))).unwrap_or((kind, 1.0));
        let t = match kind {
            "play" | "fwd" => from + speed * tau,
            "back" => from - speed * tau,
            "saw" => {
                let phase = tau % 2.0;
                from + speed * if phase < 1.0 { phase } else { 2.0 - phase }
            }
            // A random frame anywhere in the cut (`rand:W@T`: within W
            // seconds from T), a new one every half second: seeks.
            "rand" => {
                let k = (tau / 0.5).floor() as u64 + 1;
                let mut h = k.wrapping_mul(0x9e3779b97f4a7c15) ^ 0xd1b54a32d192ed03;
                h ^= h >> 29;
                h = h.wrapping_mul(0xbf58476d1ce4e5b9);
                h ^= h >> 32;
                let (start, width) = if pattern.contains(':') { (from, speed) } else { (0.0, self.duration) };
                start + (h % 1_000_000) as f64 / 1_000_000.0 * width
            }
            "jump" => {
                self.seed ^= self.seed << 13;
                self.seed ^= self.seed >> 7;
                self.seed ^= self.seed << 17;
                from + (self.seed % 5000) as f64 / 1000.0
            }
            _ => finish(Err(format!("--pattern: `{pattern}`"))),
        };
        t.clamp(0.0, self.duration - 1e-3)
    }

    fn draw(&mut self, cx: &mut Cx2d) {
        if self.black.as_ref().is_some_and(|check| check.asking.is_some()) {
            // Keep the requested picture unchanged until its ticket is admitted.
            // The host's NextFrame loop calls this draw again.
            self.read_shown(cx);
            return;
        }
        let Some(pattern) = self.patterns.front().cloned() else {
            if !self.black_again(cx) {
                return;
            }
            if let Some(log) = &self.log {
                let _ = std::fs::write(log, self.rows.join("\n"));
            }
            finish(Ok(format!("edit-render: scrub\n{}", self.summary.join("\n"))));
        };
        let now = Cx::monotonic_now();
        if self.prewarm {
            self.preview.draw(cx);
            if let Some((_, all)) = self.preview.prewarm_progress().filter(|(done, all)| done == all) {
                let line = format!("  prewarm: {all} frames ahead drawn {:.1} s after open", now - self.opened);
                println!("{line}");
                self.summary.push(line);
                self.prewarm = false;
            }
            return;
        }
        if self.cold {
            self.cold = false;
            self.started = Some(now);
            return;
        }
        let Some(started) = self.started else {
            // Warm up: the pattern's first position, until it shows.
            let t = self.position(&pattern, 0.0);
            self.preview.seek(t);
            self.preview.draw(cx);
            if let PreviewStatus::Failed(error) = self.preview.status() {
                finish(Err(error));
            }
            let want = self.frame_of(t);
            let shown = self.preview.frame().map(|(t, _)| self.frame_of(t));
            if shown == Some(want) {
                // Settle a little so the start is not the first paint's.
                let warm = *self.warm.get_or_insert(now);
                if now - warm > 0.5 {
                    self.started = Some(now);
                }
            }
            return;
        };
        let tau = now - started;
        // `pattern~S`: this pattern runs S seconds instead of `--seconds`.
        let seconds = pattern.split_once('~').and_then(|(_, s)| s.parse::<f64>().ok()).unwrap_or(self.seconds);
        if tau > seconds {
            self.report(&pattern);
            self.patterns.pop_front();
            self.started = None;
            self.warm = None;
            self.frames.clear();
            self.asked.clear();
            return;
        }
        let t = self.position(&pattern, tau);
        let want = self.frame_of(t);
        self.asked.entry(want).or_insert(now);
        self.preview.seek(t);
        self.preview.draw(cx);
        let shown = self.preview.frame().map(|(t, _)| self.frame_of(t));
        let exact = self.preview.exact();
        let age = shown.and_then(|n| self.asked.get(&n)).map_or(-1.0, |asked| (now - asked) * 1000.0);
        self.rows.push(format!("{pattern},{:.1},{want},{},{age:.1},{}", tau * 1000.0, shown.map_or(-1, |n| n as i64), exact as u8));
        self.frames.push(ScrubFrame { at: tau, want, shown, exact });
        if exact && self.first_picture.is_none() {
            self.first_picture = Some(now - self.opened);
        }
        self.read_shown(cx);
    }

    /// `--black`: read back the picture shown when it changed.
    fn read_shown(&mut self, cx: &mut Cx2d) {
        let Some(check) = &mut self.black else { return };
        if check.reading.is_some() {
            return;
        }
        if check.asking.is_none() {
            let Some((t, texture)) = self.preview.frame() else { return };
            let n = (t * self.fps.0 as f64 / self.fps.1 as f64 + 1e-6).floor() as u64;
            check.asking = Some((n, self.preview.exact(), texture.clone()));
        }
        let (n, exact, texture) = check.asking.as_ref().unwrap();
        match texture.read_back(cx, ReadbackRequest::default()) {
            Ok(ticket) => {
                check.readback_wait_logged = false;
                check.reading = Some((*n, *exact, ticket));
                check.asking = None;
            }
            Err(ReadbackError::Backpressure) => {
                if !check.readback_wait_logged {
                    log!("edit scrub: context readback request limit reached; shown frame {n} waits");
                    check.readback_wait_logged = true;
                }
            }
            Err(_) => {
                check.readback_wait_logged = false;
                check.asking = None;
            }
        }
    }

    fn readback(&mut self, cx: &mut Cx) {
        let Some(check) = &mut self.black else { return };
        let Some((n, exact, ticket)) = check.reading else { return };
        for result in cx.try_take_texture_readbacks_for(&[ticket]) {
            check.reading = None;
            let Ok(data) = result.data else { continue };
            let black = is_black(&data, result.width as usize, result.height as usize, result.width as usize * 4);
            if let (true, true, Some(&expected)) = (exact, check.again.is_none(), check.reference.get(n as usize)) {
                let mut rgba = Vec::new();
                opaque_rgba(&data, &mut rgba);
                if fnv1a(&rgba) != expected && !check.wrong.contains(&n) {
                    check.wrong.push(n);
                }
            }
            match &check.again {
                Some(_) => check.warm.push((n, black)),
                None => {
                    check.pictures += 1;
                    if black {
                        check.black.insert(n);
                    }
                }
            }
        }
    }

    /// After the patterns: show each frame that was black again, warm, and
    /// read it. True when done (and the summary has its line).
    fn black_again(&mut self, cx: &mut Cx2d) -> bool {
        let Some(check) = &mut self.black else { return true };
        if check.reading.is_some() {
            self.preview.draw(cx);
            return false;
        }
        let again = check.again.get_or_insert_with(|| check.black.iter().copied().take(24).collect());
        if let Some(&n) = again.front() {
            self.preview.seek(makepad_story_edit_render::audio::frame_time(n, self.fps));
            self.preview.draw(cx);
            let shown = self.preview.frame().map(|(t, _)| (t * self.fps.0 as f64 / self.fps.1 as f64 + 1e-6).floor() as u64);
            if shown == Some(n) && self.preview.exact() && matches!(self.preview.status(), PreviewStatus::Idle) {
                if let Some((_, texture)) = self.preview.frame() {
                    match texture.read_back(cx, ReadbackRequest::default()) {
                        Ok(ticket) => {
                            check.readback_wait_logged = false;
                            check.reading = Some((n, true, ticket));
                            again.pop_front();
                        }
                        Err(ReadbackError::Backpressure) => {
                            if !check.readback_wait_logged {
                                log!("edit scrub: context readback request limit reached; warm frame {n} waits");
                                check.readback_wait_logged = true;
                            }
                        }
                        Err(_) => check.readback_wait_logged = false,
                    }
                }
            }
            return false;
        }
        let misses: Vec<u64> = check.warm.iter().filter(|(_, black)| !black).map(|(n, _)| *n).collect();
        let line = format!("  black: {} frames of {} readbacks were black at frames {:?}; with content once warm (first-time misses): {:?}; exact pictures not the render's frame: {:?}; first exact picture {:.2} s after open",
            check.black.len(), check.pictures, check.black.iter().take(24).collect::<Vec<_>>(), misses, check.wrong, self.first_picture.unwrap_or(f64::NAN));
        println!("{line}");
        self.summary.push(line);
        true
    }

    /// Frames shown per second, request→present latency of what was shown,
    /// the longest time the picture stood still while the playhead moved,
    /// display frames not showing the wanted frame, and the mean lag.
    fn report(&mut self, pattern: &str) {
        let frames = &self.frames;
        let n = frames.len().max(1);
        let span = frames.last().map_or(1.0, |f| f.at) - frames.first().map_or(0.0, |f| f.at);
        let mut changes = 0;
        let mut stall: f64 = 0.0;
        let mut still_since = frames.first().map_or(0.0, |f| f.at);
        let mut ages: Vec<f64> = Vec::new();
        let mut first_shown: HashMap<u64, f64> = HashMap::new();
        for pair in frames.windows(2) {
            if pair[1].shown != pair[0].shown || pair[1].exact != pair[0].exact {
                changes += 1;
                still_since = pair[1].at;
            } else if pair[1].want != pair[0].want {
                stall = stall.max(pair[1].at - still_since);
            }
        }
        for frame in frames.iter().filter(|frame| frame.exact) {
            if let Some(shown) = frame.shown {
                first_shown.entry(shown).or_insert(frame.at);
            }
        }
        let started = self.started.unwrap_or(0.0);
        for (shown, at) in &first_shown {
            if let Some(asked) = self.asked.get(shown) {
                ages.push((at - (asked - started)) * 1000.0);
            }
        }
        ages.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| ages.get(((ages.len() as f64 - 1.0) * p).round().max(0.0) as usize).copied().unwrap_or(f64::NAN);
        // A picture is recorded in one display frame and shows in the next:
        // a frame is behind when it shows neither the want of the frame
        // before (the pipeline's one-frame latency) nor the current one.
        let missed = frames.windows(2).filter(|f| !f[1].exact || (f[1].shown != Some(f[0].want) && f[1].shown != Some(f[1].want))).count();
        let lag: f64 = frames.iter().map(|f| f.want as f64 - f.shown.map_or(f.want as f64, |s| s as f64)).sum::<f64>() / n as f64;
        let lag_ms = lag * self.fps.1 as f64 / self.fps.0 as f64 * 1000.0;
        let wanted = frames.iter().map(|f| f.want).collect::<std::collections::HashSet<_>>().len();
        let line = format!(
            "  {pattern:<14} display {:5.1} Hz | pictures {:5.1}/s ({} of {} wanted frames exact) | latency p50 {:6.1} ms p95 {:6.1} ms | worst stall {:6.1} ms | behind {:4.1}% | mean lag {:6.1} ms",
            n as f64 / span.max(1e-3), changes as f64 / span.max(1e-3), first_shown.len(), wanted, pct(0.5), pct(0.95), stall * 1000.0,
            missed as f64 * 100.0 / n as f64, lag_ms
        );
        println!("{line}");
        self.summary.push(line);
    }
}

/// `--parent-pipe`: the parent holds our stdin. When it closes (the parent
/// quit or died) the renderer exits at once, whatever it was doing, and
/// takes its partial file with it; a `cancel` line cancels the render
/// cleanly.
fn watch_parent(cx: &mut Cx, partial: Option<PathBuf>) -> std::sync::mpsc::Receiver<()> {
    let (cancel, cancelled) = std::sync::mpsc::channel();
    let watcher = cx.spawn_worker(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) if line.trim() == "cancel" => {
                    let _ = cancel.send(());
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        eprintln!("edit-render: the parent is gone; exiting");
        if let Some(partial) = &partial {
            let _ = std::fs::remove_file(partial);
        }
        std::process::exit(2);
    });
    match watcher {
        Ok(task) => task.detach(),
        Err(error) => finish(Err(format!("--parent-pipe: {error:?}"))),
    }
    cancelled
}

/// Print and leave: 0 on success, 1 on failure.
fn finish(result: Result<String, String>) -> ! {
    match result {
        Ok(report) => {
            println!("{report}");
            std::process::exit(0)
        }
        Err(error) => {
            eprintln!("edit-render: FAILED: {error}");
            std::process::exit(1)
        }
    }
}

impl Widget for CutRenderHost {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Some(renderer) = &mut self.renderer {
            renderer.handle_event(cx, event);
        }
        if let Some((thumbnail, _)) = &mut self.thumbnail {
            thumbnail.handle_event(cx, event);
        }
        if let Some(run) = &mut self.preview {
            run.preview.handle_event(cx, event);
            run.readback(cx);
        }
        if let Some(run) = &mut self.export {
            run.engine.handle_event(cx, event);
        }
        if let Some(run) = &mut self.scrub {
            run.preview.handle_event(cx, event);
            run.readback(cx);
        }
        if self.next_frame.is_event(event).is_some() {
            self.check(cx);
            self.draw_bg.redraw(cx);
            self.next_frame = cx.new_next_frame();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.draw_bg.draw_walk(cx, walk);
        if let Some(renderer) = &mut self.renderer {
            renderer.draw(cx);
        }
        if let Some(run) = &mut self.preview {
            run.draw(cx);
        }
        if let Some(run) = &mut self.scrub {
            run.draw(cx);
        }
        if let Some((thumbnail, _)) = &mut self.thumbnail {
            thumbnail.draw(cx);
        }
        DrawStep::done()
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        let args: Vec<String> = std::env::args().collect();
        let command = match parse_args(&args) {
            Ok(command) => command,
            Err(error) => finish(Err(error)),
        };
        let partial = match &command {
            Command::Render { options, .. } => Some(makepad_story_edit_render::render::partial_path(&options.path)),
            _ => None,
        };
        let parent = args.iter().any(|arg| arg == "--parent-pipe").then(|| watch_parent(cx, partial));
        if let Some(mut host) = self.ui.widget(cx, ids!(host)).borrow_mut::<CutRenderHost>() {
            host.parent = parent;
            host.start(cx, command);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::script_mod(vm);
        makepad_content_packs::script_mod(vm);
        makepad_story_edit_render::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}

// ---------------------------------------------------------------------------
// fixture, probe, compare (pool workers; no GPU)
// ---------------------------------------------------------------------------

const FIXTURE_FPS: u32 = 30;
const FIXTURE_SECONDS: u32 = 4;

/// A fixed-bytes asset id, so the fixture's edit names the same assets
/// every time it is made.
fn fixture_id(seed: u8) -> String {
    AssetId::from_bytes([seed; 16]).to_string()
}

fn revision_of(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let digest = BlobId::hash_of(&bytes);
    Ok(format!("arev_{}", digest.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

/// One generated clip: every frame different (a moving bar, a scrolling
/// pattern, its frame number in binary) and a steady tone.
fn write_clip(path: &Path, width: u32, height: u32, tone: f64, pattern: u32) -> Result<(), String> {
    let options = VideoFileEncoderOptions {
        codec: VideoFileCodec::H264,
        width,
        height,
        fps_num: FIXTURE_FPS,
        fps_den: 1,
        video_bitrate_bps: 4_000_000,
        audio: Some(PcmAudioTrackOptions { sample_rate: 48_000, channels: 2, aac_bitrate_bps: 128_000 }),
        keyframe_only: false,
        ..Default::default()
    };
    let _ = std::fs::remove_file(path);
    let mut encoder = VideoFileEncoder::new(&path.to_string_lossy(), options).map_err(|e| format!("{}: {e}", path.display()))?;
    let frames = FIXTURE_FPS * FIXTURE_SECONDS;
    let per_frame = 48_000 / FIXTURE_FPS as usize;
    let mut rgb = vec![0u8; (width * height * 3) as usize];
    for n in 0..frames {
        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 3) as usize;
                let (r, g, b) = if pattern == 0 {
                    // Diagonal colour sweep with a moving white bar.
                    let bar = (x + width - (n * 6) % width) % width < 10;
                    if bar { (255, 255, 255) } else { (((x + n * 3) % 256) as u8, ((y * 2) % 256) as u8, (((x + y) / 3) % 256) as u8) }
                } else {
                    // A scrolling checkerboard with a red disc.
                    let check = ((x + n * 4) / 40 + y / 40) % 2 == 0;
                    let (cx, cy) = ((width / 2) as f64 + (n as f64 * 0.1).sin() * width as f64 * 0.3, (height / 2) as f64);
                    let disc = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt() < height as f64 * 0.15;
                    if disc { (230, 30, 40) } else if check { (40, 60, 200) } else { (230, 230, 210) }
                };
                rgb[i] = r;
                rgb[i + 1] = g;
                rgb[i + 2] = b;
            }
        }
        // The frame number, 8 bits, as 16 px squares along the top.
        for bit in 0..8u32 {
            let on = (n >> bit) & 1 == 1;
            for y in 4..20 {
                for x in (4 + bit * 20)..(20 + bit * 20) {
                    let i = ((y * width + x) * 3) as usize;
                    let v = if on { 255 } else { 0 };
                    rgb[i..i + 3].copy_from_slice(&[v, v, v]);
                }
            }
        }
        let pts = n as i64 * 10_000_000 / FIXTURE_FPS as i64;
        encoder.push_frame_rgb8(&rgb, Some(pts)).map_err(|e| e.to_string())?;
        let pcm: Vec<i16> = (0..per_frame)
            .flat_map(|i| {
                let t = (n as usize * per_frame + i) as f64 / 48_000.0;
                let v = ((t * tone * std::f64::consts::TAU).sin() * 0.3 * 32767.0) as i16;
                [v, v]
            })
            .collect();
        encoder.push_audio_i16(&pcm).map_err(|e| e.to_string())?;
    }
    encoder.finish().map_err(|e| e.to_string())
}

fn make_fixture(dir: &Path) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (a_path, b_path) = (dir.join("clip_a.mp4"), dir.join("clip_b.mp4"));
    write_clip(&a_path, 640, 360, 440.0, 0)?;
    write_clip(&b_path, 480, 480, 660.0, 1)?;
    let (a, b) = (fixture_id(0xa1), fixture_id(0xb2));
    let (a_rev, b_rev) = (revision_of(&a_path)?, revision_of(&b_path)?);
    // A transcript of clip A (source seconds), for the captions.
    let spoken = |text: &str, start: f64, end: f64| {
        let words: Vec<&str> = text.split_whitespace().collect();
        let step = (end - start) / words.len() as f64;
        TranscriptLine {
            start,
            end,
            text: text.into(),
            words: words.iter().enumerate().map(|(i, w)| Word { text: w.to_string(), start: start + i as f64 * step, end: start + (i as f64 + 0.8) * step }).collect(),
            confident: true,
        }
    };
    let transcript = Transcript {
        backend: "fixture".into(),
        model: String::new(),
        language: "en".into(),
        duration_secs: FIXTURE_SECONDS as f64,
        timing: String::new(),
        lines: vec![spoken("Every frame lands exactly", 0.3, 1.6), spoken("and every click with it", 1.9, 3.4)],
        phones: Vec::new(),
    };
    let subs_path = dir.join("subs.json");
    std::fs::write(&subs_path, transcript.to_json("")).map_err(|e| e.to_string())?;
    let (t, t_rev) = (fixture_id(0xd4), revision_of(&subs_path)?);
    let edit = format!(
        r#"// The L5 renderer fixture: two generated clips, one transition, one effect,
// a title and two sounds.
use mod.flow.*
use mod.cut.*

let a     = Clip{{src: asset("{a}", "{a_rev}") in: 0.0 span: [0.0, 2.5] fit: @fill}}
let b     = Clip{{src: asset("{b}", "{b_rev}") in: 0.5 span: [1.5, 2.5] fit: @fit}}
let wipe  = Transition{{a: a.out(@frame) b: b.out(@frame) effect: "" span: [1.5, 1.0]}}
let tint  = Fx{{input: wipe.out(@frame) effect: "kit/hue_saturation" span: [0.0, 4.0] p2: keys([0.0, 0.2], [4.0, 0.8, @ease_out])}}
let title = Title{{text: "L5 edit render" span: [0.5, 3.0] anchor: vec2(0.5, 0.85) font_size: 28}}
let late  = Title{{text: "Zwölf Qüxj 42%" span: [2.2, 1.8] anchor: vec2(0.5, 0.15) font_size: 20}}
let subs  = Subtitles{{source: a.out(@audio) transcript: asset("{t}", "{t_rev}") anchor: vec2(0.5, 0.62) max_words: 4 font_size: 22}}
let top   = Stack{{layers: [tint.out(@frame), title.out(@frame), late.out(@frame), subs.out(@frame)]}}
let music = AudioClip{{src: asset("{a}", "{a_rev}") span: [0.0, 4.0] gain: keys([0.0, 0.0], [0.5, 1.0], [3.5, 1.0], [4.0, 0.0])}}
let voice = AudioClip{{src: asset("{b}", "{b_rev}") in: 0.5 span: [1.5, 2.5] gain: 0.5 pan: 0.5}}
let out   = CutOutput{{frame: top.out(@frame) audio: [conn(1, music.out(@audio)), conn(2, voice.out(@audio))]}}

Flow{{
    kind: @cut
    label: "L5 fixture"
    output: {{width: 640 height: 360 fps: 30}}
    duration: 4.0
    bpm: 120
    a, b, wipe, tint, title, late, subs, top, music, voice, out
}}
"#
    );
    let edit_path = dir.join("fixture.splash");
    std::fs::write(&edit_path, &edit).map_err(|e| e.to_string())?;
    CutDoc::load(&edit).map_err(|errors| format!("fixture edit does not load: {errors:?}"))?;
    // A still for the 9:16 edit: a 512 x 512 gradient with a ring.
    let still_path = dir.join("still.jpg");
    let mut bgra = Vec::with_capacity(512 * 512 * 4);
    for y in 0..512i32 {
        for x in 0..512i32 {
            let d = (((x - 256).pow(2) + (y - 256).pow(2)) as f64).sqrt();
            let ring = (d - 180.0).abs() < 24.0;
            let (r, g, b) = if ring { (250, 250, 250) } else { ((x / 2) as u8, (y / 2) as u8, 160) };
            bgra.extend_from_slice(&[b, g, r, 255]);
        }
    }
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 92).encode(&bgra, 512, 512, jpeg_encoder::ColorType::Bgra).map_err(|e| e.to_string())?;
    std::fs::write(&still_path, &jpeg).map_err(|e| e.to_string())?;
    let (s, s_rev) = (fixture_id(0xc3), revision_of(&still_path)?);
    // The same clips at 1080 x 1920, 60 fps: a still with keyed opacity, a
    // dissolve, a keyed Reframe and a Stack in three blend modes.
    let tall = format!(
        r#"// The L5 renderer fixture at 9:16: stills, a dissolve, a reframe and blends.
use mod.flow.*
use mod.cut.*

let a     = Clip{{src: asset("{a}", "{a_rev}") span: [0.0, 4.0] fit: @fill}}
let b     = Clip{{src: asset("{b}", "{b_rev}") span: [0.0, 4.0] fit: @fit}}
let card  = Still{{src: asset("{s}", "{s_rev}") span: [0.0, 4.0] fit: @fit opacity: keys([0.0, 0.0], [1.0, 1.0])}}
let fade  = Transition{{a: a.out(@frame) b: b.out(@frame) effect: "" span: [1.0, 2.0]}}
let crop  = Reframe{{input: a.out(@frame) mode: @keys crop: keys([0.0, [0.0, 0.0, 202.5, 360.0]], [4.0, [437.5, 0.0, 202.5, 360.0]])}}
let mixed = Stack{{layers: [fade.out(@frame), crop.out(@frame), card.out(@frame)] blend: [@normal, @screen, @multiply]}}
let title = Title{{text: "9:16" span: [0.0, 4.0] anchor: vec2(0.5, 0.1) font_size: 96}}
let subs  = Subtitles{{source: a.out(@audio) transcript: asset("{t}", "{t_rev}") style: "karaoke/pop" anchor: vec2(0.5, 0.72) max_words: 4 font_size: 72}}
let top   = Stack{{layers: [mixed.out(@frame), title.out(@frame), subs.out(@frame)]}}
let out   = CutOutput{{frame: top.out(@frame)}}

Flow{{
    kind: @cut
    label: "L5 fixture 9:16"
    output: {{width: 1080 height: 1920 fps: 60}}
    duration: 4.0
    bpm: 120
    a, b, card, fade, crop, mixed, title, subs, top, out
}}
"#
    );
    std::fs::write(dir.join("fixture_916.splash"), &tall).map_err(|e| e.to_string())?;
    CutDoc::load(&tall).map_err(|errors| format!("9:16 fixture edit does not load: {errors:?}"))?;
    make_nested_fixture(dir, &a, &a_rev, &t, &t_rev)?;
    make_safe_fixtures(dir, &a, &a_rev)?;
    let (av, av_rev) = make_av_fixture(dir)?;
    let media = format!(
        "{a}=clip_a.mp4\n{b}=clip_b.mp4\n{s}=still.jpg\nlyrics:{t}=subs.json\nlyrics:{}=subs_long.json\n{av}=av_clip.mp4\nedit:{}=nested_mid.splash\nedit:{}=nested_deep.splash\nedit:{}=nested_mid_subs.splash\n",
        fixture_id(0xd9), fixture_id(0xf6), fixture_id(0xf7), fixture_id(0xf8)
    );
    let _ = av_rev;
    std::fs::write(dir.join("media.txt"), media).map_err(|e| e.to_string())?;
    Ok(format!(
        "edit-render: fixture in {}: fixture.splash, fixture_916.splash, av.splash, nested*.splash, media.txt, clip_a.mp4, clip_b.mp4, av_clip.mp4, still.jpg, subs.json",
        dir.display()
    ))
}

/// The nesting fixture (§3.1): `nested.splash` plays `nested_mid.splash`
/// (at 0.5 s), which plays `nested_deep.splash` (from 0.5 s in), which
/// plays clip A with a title and captions of its own; `nested_flat.splash`
/// is the same picture and sound written flat. The deep captions are off in
/// the nested render (no parent opts in), so the flat edit has none.
fn make_nested_fixture(dir: &Path, a: &str, a_rev: &str, t: &str, t_rev: &str) -> Result<(), String> {
    let header = |label: &str, duration: f64, nodes: &str| {
        format!("Flow{{\n    kind: @cut\n    label: \"{label}\"\n    output: {{width: 640 height: 360 fps: 30}}\n    duration: {duration}\n    bpm: 120\n    {nodes}\n}}\n")
    };
    let deep = format!(
        "// L5 nesting fixture, level 2.\nuse mod.flow.*\nuse mod.cut.*\n\n\
         let v    = Clip{{src: asset(\"{a}\", \"{a_rev}\") span: [0.0, 4.0] fit: @fill}}\n\
         let deep = Title{{text: \"deep\" span: [0.5, 1.5] anchor: vec2(0.5, 0.3) font_size: 28}}\n\
         let subs = Subtitles{{source: v.out(@audio) transcript: asset(\"{t}\", \"{t_rev}\") anchor: vec2(0.5, 0.62) max_words: 4 font_size: 22}}\n\
         let s    = Stack{{layers: [v.out(@frame), deep.out(@frame), subs.out(@frame)]}}\n\
         let out  = CutOutput{{frame: s.out(@frame)}}\n\n{}",
        header("nested deep", 4.0, "v, deep, subs, s, out")
    );
    let deep_path = dir.join("nested_deep.splash");
    std::fs::write(&deep_path, &deep).map_err(|e| e.to_string())?;
    let (deep_id, deep_rev) = (fixture_id(0xf7), revision_of(&deep_path)?);
    let mid = format!(
        "// L5 nesting fixture, level 1.\nuse mod.flow.*\nuse mod.cut.*\n\n\
         let inner = Clip{{src: cut(\"{deep_id}\", \"{deep_rev}\") in: 0.5 span: [0.0, 3.5]}}\n\
         let mid   = Title{{text: \"mid\" span: [1.0, 1.0] anchor: vec2(0.5, 0.15) font_size: 24}}\n\
         let s     = Stack{{layers: [inner.out(@frame), mid.out(@frame)]}}\n\
         let out   = CutOutput{{frame: s.out(@frame)}}\n\n{}",
        header("nested mid", 3.5, "inner, mid, s, out")
    );
    let mid_path = dir.join("nested_mid.splash");
    std::fs::write(&mid_path, &mid).map_err(|e| e.to_string())?;
    let (mid_id, mid_rev) = (fixture_id(0xf6), revision_of(&mid_path)?);
    let root = format!(
        "// L5 nesting fixture: two levels of compound clips.\nuse mod.flow.*\nuse mod.cut.*\n\n\
         let outer = Clip{{src: cut(\"{mid_id}\", \"{mid_rev}\") span: [0.5, 3.5]}}\n\
         let out   = CutOutput{{frame: outer.out(@frame)}}\n\n{}",
        header("nested", 4.0, "outer, out")
    );
    let flat = format!(
        "// The nesting fixture written flat: the same picture and sound.\nuse mod.flow.*\nuse mod.cut.*\n\n\
         let v    = Clip{{src: asset(\"{a}\", \"{a_rev}\") in: 0.5 span: [0.5, 3.5] fit: @fill}}\n\
         let deep = Title{{text: \"deep\" span: [0.5, 1.5] anchor: vec2(0.5, 0.3) font_size: 28}}\n\
         let mid  = Title{{text: \"mid\" span: [1.5, 1.0] anchor: vec2(0.5, 0.15) font_size: 24}}\n\
         let s    = Stack{{layers: [v.out(@frame), deep.out(@frame), mid.out(@frame)]}}\n\
         let out  = CutOutput{{frame: s.out(@frame)}}\n\n{}",
        header("nested, flat", 4.0, "v, deep, mid, s, out")
    );
    // The same nest with the captions opted in at both clips: the deep
    // edit's captions burn in and come out as sidecars of the root.
    let mid_subs = mid.replace("in: 0.5 span: [0.0, 3.5]}", "in: 0.5 span: [0.0, 3.5] subtitles: 1}");
    let mid_subs_path = dir.join("nested_mid_subs.splash");
    std::fs::write(&mid_subs_path, &mid_subs).map_err(|e| e.to_string())?;
    let (mid_subs_id, mid_subs_rev) = (fixture_id(0xf8), revision_of(&mid_subs_path)?);
    let root_subs = root
        .replace(&format!("cut(\"{mid_id}\", \"{mid_rev}\") span: [0.5, 3.5]}}"), &format!("cut(\"{mid_subs_id}\", \"{mid_subs_rev}\") span: [0.5, 3.5] subtitles: 1}}"));
    std::fs::write(dir.join("nested_subs.splash"), &root_subs).map_err(|e| e.to_string())?;
    for text in [&mid_subs, &root_subs] {
        CutDoc::load(text).map_err(|errors| format!("nesting fixture edit does not load: {errors:?}"))?;
    }
    for (name, text) in [("nested.splash", &root), ("nested_flat.splash", &flat)] {
        std::fs::write(dir.join(name), text).map_err(|e| e.to_string())?;
    }
    for text in [&deep, &mid, &root, &flat] {
        CutDoc::load(text).map_err(|errors| format!("nesting fixture edit does not load: {errors:?}"))?;
    }
    Ok(())
}

/// The safe-zone fixtures (§8.3): for each platform preset, 1080x1920
/// captions of a long line (wrapping, shrinking) anchored inside the bottom
/// band and inside the top-left corner band, over black (the words are
/// clip A's sound, an AudioClip). `band-check` then looks for caption pixels
/// in the bands.
const SAFE_PRESETS: [&str; 3] = ["reels", "tiktok", "shorts"];

fn make_safe_fixtures(dir: &Path, a: &str, a_rev: &str) -> Result<(), String> {
    let text = "Captions stay clear of every platform band, wrapping to two lines and shrinking when a line is long like this one";
    let words: Vec<&str> = text.split_whitespace().collect();
    let step = 0.9 / words.len() as f64;
    let transcript = Transcript {
        backend: "fixture".into(),
        model: String::new(),
        language: "en".into(),
        duration_secs: FIXTURE_SECONDS as f64,
        timing: String::new(),
        lines: vec![TranscriptLine {
            start: 0.05,
            end: 0.95,
            text: text.into(),
            words: words.iter().enumerate().map(|(i, w)| Word { text: w.to_string(), start: 0.05 + i as f64 * step, end: 0.05 + (i as f64 + 0.9) * step }).collect(),
            confident: true,
        }],
        phones: Vec::new(),
    };
    let path = dir.join("subs_long.json");
    std::fs::write(&path, transcript.to_json("")).map_err(|e| e.to_string())?;
    let (t, t_rev) = (fixture_id(0xd9), revision_of(&path)?);
    for preset in SAFE_PRESETS {
        let edit = format!(
            "// L5 safe-zone fixture: @{preset}.\nuse mod.flow.*\nuse mod.cut.*\n\n\
             let v      = AudioClip{{src: asset(\"{a}\", \"{a_rev}\") span: [0.0, 1.0]}}\n\
             let low    = Subtitles{{source: v.out(@audio) transcript: asset(\"{t}\", \"{t_rev}\") anchor: vec2(0.5, 0.86) safe: @{preset} max_words: 40 font_size: 84}}\n\
             let corner = Subtitles{{source: v.out(@audio) transcript: asset(\"{t}\", \"{t_rev}\") anchor: vec2(0.02, 0.03) safe: @{preset} max_words: 40 font_size: 60}}\n\
             let s      = Stack{{layers: [low.out(@frame), corner.out(@frame)]}}\n\
             let out    = CutOutput{{frame: s.out(@frame)}}\n\n\
             Flow{{\n    kind: @cut\n    label: \"safe {preset}\"\n    output: {{width: 1080 height: 1920 fps: 30}}\n    duration: 1.0\n    bpm: 120\n    v, low, corner, s, out\n}}\n"
        );
        CutDoc::load(&edit).map_err(|errors| format!("safe fixture edit does not load: {errors:?}"))?;
        std::fs::write(dir.join(format!("safe_{preset}.splash")), edit).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// No caption pixel in the preset's bands: every frame's bright luma
/// (Y > 48) inside a band is counted, and the safe region must hold some
/// (a caption was drawn at all).
fn band_check(path: &Path, preset: &str) -> Result<String, String> {
    let bands = makepad_story_cut::SafeZone::from_id(preset).ok_or_else(|| format!("unknown preset `{preset}`"))?.bands();
    let probe = probe(path)?;
    let (w, h) = (probe.width as usize, probe.height as usize);
    let (top, bottom) = ((bands.top * h as f64).floor() as usize, h - (bands.bottom * h as f64).floor() as usize);
    let (left, right) = ((bands.left * w as f64).floor() as usize, w - (bands.right * w as f64).floor() as usize);
    let (mut inside_band, mut inside_safe, mut frames_with_text) = (0usize, 0usize, 0usize);
    let mut worst = Vec::new();
    for (n, frame) in probe.frames.iter().enumerate() {
        let (mut band, mut safe) = (0usize, 0usize);
        for y in 0..h {
            for x in 0..w {
                if frame[y * w + x] <= 48 {
                    continue;
                }
                if y < top || y >= bottom || x < left || x >= right {
                    band += 1;
                } else {
                    safe += 1;
                }
            }
        }
        if band > 0 {
            worst.push(format!("frame {n}: {band} px"));
        }
        if safe > 0 {
            frames_with_text += 1;
        }
        inside_band += band;
        inside_safe += safe;
    }
    let report = format!(
        "@{preset}: {w}x{h}, bands top {top} bottom {bottom} left {left} right {right}: {inside_band} caption px in the bands, {inside_safe} inside, text on {frames_with_text}/{} frames",
        probe.frames.len()
    );
    if inside_band > 0 {
        return Err(format!("captions in the band: {report}; {}", worst.join(", ")));
    }
    if frames_with_text == 0 {
        return Err(format!("no caption drawn: {report}"));
    }
    Ok(format!("edit-render: band-check clear, {report}"))
}

/// The A/V alignment fixture: black frames with a white flash at
/// [`AV_FLASHES`] and, at exactly each flash's time, a click (a 5 ms
/// 1 kHz burst from silence).
const AV_FLASHES: [u32; 4] = [20, 47, 95, 131];
const AV_SECONDS: u32 = 6;
/// The fixture edit plays the clip from 0.5 s at 0.2 s: a source frame k
/// is edit frame k - 15 + 6.
const AV_IN_FRAMES: u32 = 15;
const AV_AT_FRAMES: u32 = 6;

fn make_av_fixture(dir: &Path) -> Result<(String, String), String> {
    let path = dir.join("av_clip.mp4");
    let (width, height) = (320u32, 180u32);
    let options = VideoFileEncoderOptions {
        codec: VideoFileCodec::H264,
        width,
        height,
        fps_num: FIXTURE_FPS,
        fps_den: 1,
        video_bitrate_bps: 2_000_000,
        audio: Some(PcmAudioTrackOptions { sample_rate: 48_000, channels: 2, aac_bitrate_bps: 128_000 }),
        keyframe_only: false,
        ..Default::default()
    };
    let _ = std::fs::remove_file(&path);
    let mut encoder = VideoFileEncoder::new(&path.to_string_lossy(), options).map_err(|e| format!("{}: {e}", path.display()))?;
    let per_frame = 48_000 / FIXTURE_FPS as usize;
    for n in 0..FIXTURE_FPS * AV_SECONDS {
        let flash = AV_FLASHES.contains(&n);
        let rgb = vec![if flash { 255u8 } else { 0 }; (width * height * 3) as usize];
        encoder.push_frame_rgb8(&rgb, Some(n as i64 * 10_000_000 / FIXTURE_FPS as i64)).map_err(|e| e.to_string())?;
        let pcm: Vec<i16> = (0..per_frame)
            .flat_map(|i| {
                let v = if flash && i < 240 { ((i as f64 / 48_000.0 * 1000.0 * std::f64::consts::TAU).sin() * 0.8 * 32767.0) as i16 } else { 0 };
                [v, v]
            })
            .collect();
        encoder.push_audio_i16(&pcm).map_err(|e| e.to_string())?;
    }
    encoder.finish().map_err(|e| e.to_string())?;
    let (id, rev) = (fixture_id(0xe5), revision_of(&path)?);
    let edit = format!(
        r#"// The L5 A/V alignment fixture: flashes and clicks at the same instants.
use mod.flow.*
use mod.cut.*

let v   = Clip{{src: asset("{id}", "{rev}") in: {in_s} span: [{at_s}, 5.0] audio: 1}}
let out = CutOutput{{frame: v.out(@frame) audio: [conn(1, v.out(@audio))]}}

Flow{{
    kind: @cut
    label: "L5 A/V alignment"
    output: {{width: 320 height: 180 fps: 30}}
    duration: 5.2
    v, out
}}
"#,
        in_s = AV_IN_FRAMES as f64 / FIXTURE_FPS as f64,
        at_s = AV_AT_FRAMES as f64 / FIXTURE_FPS as f64,
    );
    std::fs::write(dir.join("av.splash"), &edit).map_err(|e| e.to_string())?;
    CutDoc::load(&edit).map_err(|errors| format!("A/V fixture edit does not load: {errors:?}"))?;
    Ok((id, rev))
}

/// Flash frames and click onsets of a file, paired: every flash must have
/// its click within 1 ms, and `expected` (when given) names the flash
/// frames there must be.
fn av_check(path: &Path, expected: Option<Vec<u64>>) -> Result<String, String> {
    let probe = probe(path)?;
    let pixels = (probe.width * probe.height) as usize;
    let flashes: Vec<u64> = probe.frames.iter().enumerate()
        .filter(|(_, nv12)| nv12[..pixels].iter().map(|&y| y as u64).sum::<u64>() / pixels as u64 > 128)
        .map(|(n, _)| n as u64)
        .collect();
    if let Some(expected) = &expected {
        if &flashes != expected {
            return Err(format!("flash frames {flashes:?}, expected {expected:?}"));
        }
    }
    // Click onsets: the first sample past a fifth of full scale after at
    // least 10 ms without one (the burst reaches that 40 µs after it
    // starts; AAC pre-echo stays far below it).
    let channels = probe.audio_channels.max(1) as usize;
    let left: Vec<i16> = probe.samples.iter().step_by(channels).copied().collect();
    let rate = probe.audio_rate.max(1) as f64;
    let quiet = (rate * 0.010) as usize;
    let mut onsets = Vec::new();
    let mut last_loud: Option<usize> = None;
    for (i, &v) in left.iter().enumerate() {
        if (v as i32).abs() > 32767 / 5 {
            if last_loud.map_or(true, |at| i - at > quiet) {
                onsets.push(i as f64 / rate);
            }
            last_loud = Some(i);
        }
    }
    if onsets.len() != flashes.len() {
        return Err(format!("{} flashes but {} clicks (at {onsets:?} s)", flashes.len(), onsets.len()));
    }
    let fps = probe.fps.0 as f64 / probe.fps.1.max(1) as f64;
    let mut report = Vec::new();
    for (n, onset) in flashes.iter().zip(&onsets) {
        let flash = probe.pts[*n as usize] as f64 / 1e7;
        let delta = onset - flash;
        report.push(format!("frame {n} at {flash:.4} s, click at {onset:.4} s ({:+.3} ms)", delta * 1000.0));
        if delta.abs() > 0.001 || (onset * fps).floor() as u64 != *n {
            return Err(format!("out of step: {}", report.join("; ")));
        }
    }
    Ok(format!("edit-render: av aligned, {} flashes each with its click within 1 ms:\n  {}", flashes.len(), report.join("\n  ")))
}

struct Probe {
    width: u32,
    height: u32,
    fps: (u32, u32),
    frames: Vec<Vec<u8>>,
    pts: Vec<i64>,
    audio_rate: u32,
    audio_channels: u16,
    samples: Vec<i16>,
}

impl Probe {
    fn report(&self) -> String {
        format!(
            "edit-render: probe {}x{} at {}/{} fps, {} frames (pts {}..{}), sound {} Hz x{} {} samples",
            self.width, self.height, self.fps.0, self.fps.1, self.frames.len(),
            self.pts.first().copied().unwrap_or(0), self.pts.last().copied().unwrap_or(0),
            self.audio_rate, self.audio_channels, self.samples.len()
        )
    }
}

fn probe(path: &Path) -> Result<Probe, String> {
    let name = path.to_string_lossy();
    let mut decoder = VideoFileDecoder::open(&name).map_err(|e| format!("{name}: {e}"))?;
    let info = decoder.info().clone();
    let mut probe = Probe {
        width: info.width, height: info.height, fps: (info.fps_num, info.fps_den), frames: Vec::new(), pts: Vec::new(),
        audio_rate: info.audio_sample_rate, audio_channels: info.audio_channels, samples: Vec::new(),
    };
    while let Some(frame) = decoder.next_frame().map_err(|e| format!("{name}: {e}"))? {
        probe.pts.push(frame.pts_100ns);
        probe.frames.push(frame.nv12);
    }
    if info.has_audio {
        let mut audio = VideoFileDecoder::open(&name).map_err(|e| format!("{name}: {e}"))?;
        while let Some(chunk) = audio.next_audio().map_err(|e| format!("{name}: {e}"))? {
            probe.samples.extend_from_slice(&chunk.samples);
        }
    }
    Ok(probe)
}

/// An export decoded against the exact pictures it was made from (a
/// `render --dump <dir> --every k --raw`, `frame_NNNNN.rgba`): the PSNR of
/// every dumped frame's colour (Y'CbCr decoded as BT.709 limited), and, for
/// a transparent reference (an alpha export's; the file decoded as BGRA by
/// the OS), the largest error of its alpha.
fn frame_check(file: &Path, dir: &Path) -> Result<String, String> {
    let mut reference: Vec<(u64, PathBuf)> = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter_map(|path| {
            let n = path.file_name()?.to_str()?.strip_prefix("frame_")?.strip_suffix(".rgba")?.parse().ok()?;
            Some((n, path))
        })
        .collect();
    reference.sort();
    if reference.is_empty() {
        return Err(format!("{}: no frame_NNNNN.rgba", dir.display()));
    }
    let name = file.to_string_lossy().to_string();
    let psnr = |mse: f64| if mse > 0.0 { 10.0 * (255.0f64 * 255.0 / mse).log10() } else { f64::INFINITY };
    let (mut worst, mut worst_frame, mut sum, mut checked, mut alpha_max) = (f64::INFINITY, 0u64, 0.0, 0usize, None::<u8>);
    let mut check = |n: u64, width: usize, height: usize, decoded: &dyn Fn(usize) -> [u8; 4], alpha: bool| -> Result<(), String> {
        let Some((_, path)) = reference.iter().find(|(m, _)| *m == n) else { return Ok(()) };
        let want = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if want.len() != width * height * 4 {
            return Err(format!("frame {n}: the reference is {} B, the file's picture is {width}x{height}", want.len()));
        }
        let mut error = 0.0f64;
        for i in 0..width * height {
            let got = decoded(i);
            for c in 0..3 {
                let d = got[c] as f64 - want[i * 4 + c] as f64;
                error += d * d;
            }
            if alpha {
                let d = got[3].abs_diff(want[i * 4 + 3]);
                alpha_max = Some(alpha_max.unwrap_or(0).max(d));
            }
        }
        let db = psnr(error / (width * height * 3) as f64);
        if db < worst {
            (worst, worst_frame) = (db, n);
        }
        sum += db.min(99.0);
        checked += 1;
        Ok(())
    };
    let info = VideoFileDecoder::open(&name).map_err(|e| format!("{name}: {e}"))?.info().clone();
    let fps = (info.fps_num.max(1), info.fps_den.max(1));
    let frame_of = |pts: i64| ((pts as i128 * fps.0 as i128 + 5_000_000 * fps.1 as i128) / (10_000_000 * fps.1 as i128)) as u64;
    // A transparent reference (an alpha export's): the file decodes as BGRA,
    // its alpha layer included.
    let transparent = std::fs::read(&reference[0].1).map_err(|e| format!("{}: {e}", reference[0].1.display()))?.chunks_exact(4).any(|p| p[3] != 255);
    if transparent {
        #[cfg(target_vendor = "apple")]
        {
            let mut reader = makepad_widgets::makepad_platform::video_file::BgraFrameReader::open(&name).map_err(|e| format!("{name}: {e}"))?;
            while let Some((pts, w, h, bgra)) = reader.next_frame().map_err(|e| format!("{name}: {e}"))? {
                check(frame_of(pts), w as usize, h as usize, &|i| [bgra[i * 4 + 2], bgra[i * 4 + 1], bgra[i * 4], bgra[i * 4 + 3]], true)?;
            }
        }
        #[cfg(not(target_vendor = "apple"))]
        return Err(format!("{name}: HEVC with alpha cannot be decoded with its alpha here"));
    } else {
        let mut decoder = VideoFileDecoder::open(&name).map_err(|e| format!("{name}: {e}"))?;
        while let Some(frame) = decoder.next_frame().map_err(|e| format!("{name}: {e}"))? {
            let rgb = frame.to_rgb8();
            check(frame_of(frame.pts_100ns), frame.width as usize, frame.height as usize, &|i| [rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2], 255], false)?;
        }
    }
    if checked != reference.len() {
        return Err(format!("{name}: {checked} of the {} reference frames were in the file", reference.len()));
    }
    let alpha = alpha_max.map_or(String::new(), |max| format!(", alpha max |Δ| {max}"));
    Ok(format!("edit-render: frame-check {checked} frames: PSNR worst {worst:.2} dB (frame {worst_frame}), mean {:.2} dB{alpha}", sum / checked as f64))
}

fn compare(a: &Path, b: &Path, tolerance: Option<f64>) -> Result<String, String> {
    let (pa, pb) = (probe(a)?, probe(b)?);
    if (pa.width, pa.height, pa.frames.len()) != (pb.width, pb.height, pb.frames.len()) {
        return Err(format!("differ: {} vs {}", pa.report(), pb.report()));
    }
    if let Some(tolerance) = tolerance {
        // Decoded NV12, byte by byte: the mean and the largest difference
        // of every frame, and of the sound's samples.
        let mut worst = (0usize, 0f64, 0u8);
        for (n, (x, y)) in pa.frames.iter().zip(&pb.frames).enumerate() {
            let sum: u64 = x.iter().zip(y).map(|(p, q)| p.abs_diff(*q) as u64).sum();
            let mean = sum as f64 / x.len().max(1) as f64;
            let max = x.iter().zip(y).map(|(p, q)| p.abs_diff(*q)).max().unwrap_or(0);
            if mean > worst.1 {
                worst = (n, mean, max);
            }
        }
        if pa.samples.len() != pb.samples.len() {
            return Err(format!("sound lengths differ: {} vs {}", pa.samples.len(), pb.samples.len()));
        }
        let rms = (pa.samples.iter().zip(&pb.samples).map(|(p, q)| ((*p as f64 - *q as f64) / 32768.0).powi(2)).sum::<f64>()
            / pa.samples.len().max(1) as f64).sqrt();
        let rms_db = if rms > 0.0 { 20.0 * rms.log10() } else { f64::NEG_INFINITY };
        let level = (pa.samples.iter().map(|p| (*p as f64 / 32768.0).powi(2)).sum::<f64>() / pa.samples.len().max(1) as f64).sqrt();
        let report = format!(
            "{} frames {}x{}: worst frame {} mean |Δ| {:.3} (max {}), sound at {:.1} dBFS RMS, difference {rms_db:.1} dBFS RMS",
            pa.frames.len(), pa.width, pa.height, worst.0, worst.1, worst.2, 20.0 * level.max(1e-12).log10()
        );
        if worst.1 > tolerance {
            return Err(format!("over the tolerance {tolerance}: {report}"));
        }
        return Ok(format!("edit-render: compare within {tolerance}: {report}"));
    }
    if let Some(n) = (0..pa.frames.len()).find(|&n| pa.frames[n] != pb.frames[n]) {
        return Err(format!("decoded frame {n} differs ({} frames)", pa.frames.len()));
    }
    if pa.pts != pb.pts {
        return Err("frame timestamps differ".into());
    }
    if pa.samples != pb.samples {
        return Err(format!("the sound differs ({} vs {} samples)", pa.samples.len(), pb.samples.len()));
    }
    let sound = "identical";
    Ok(format!(
        "edit-render: compare identical: {} decoded frames {}x{}, byte for byte; sound {sound}",
        pa.frames.len(), pa.width, pa.height
    ))
}
