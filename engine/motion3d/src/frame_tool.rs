//! Render Scene3D frames on the GPU for the command-line tool or a batch of jobs.

use crate::anim::{AudioSource, NoAudio, SyntheticMusic};
use crate::doc::{FileResolver, LoadOptions};
use crate::layer::{FrameTime, Scene3dLayer};
use crate::script::InputValue;
use makepad_widgets::*;
use makepad_zune_png::makepad_zune_core::bit_depth::BitDepth;
use makepad_zune_png::makepad_zune_core::colorspace::ColorSpace;
use makepad_zune_png::makepad_zune_core::options::EncoderOptions;
use makepad_zune_png::PngEncoder;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.Motion3dHostBase = #(Motion3dHost::register_widget(vm))
    mod.widgets.Motion3dHost = set_type_default() do mod.widgets.Motion3dHostBase{
        width: Fill
        height: Fill
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Motion3D"
                window.inner_size: vec2(640, 360)
                body +: {
                    host := mod.widgets.Motion3dHost{}
                }
            }
        }
    }
}

/// An RGBA8 picture, top row first.
#[derive(Clone)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Picture {
    pub fn px(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y.min(self.height - 1) * self.width + x.min(self.width - 1)) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }
    /// The pixel at a fraction of the width and height.
    pub fn at(&self, fx: f32, fy: f32) -> [u8; 4] {
        self.px((fx * self.width as f32) as usize, (fy * self.height as f32) as usize)
    }
    pub fn hash(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &b in &self.rgba {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    }
}

pub fn encode_png(p: &Picture) -> Result<Vec<u8>, String> {
    let options = EncoderOptions::default().set_width(p.width).set_height(p.height).set_depth(BitDepth::Eight).set_colorspace(ColorSpace::RGBA);
    let mut encoder = PngEncoder::new(&p.rgba, options);
    let mut out = Vec::new();
    encoder.encode(&mut out).map_err(|e| format!("png: {e:?}"))?;
    Ok(out)
}

/// Pictures side by side, `cols` per row, over dark grey.
pub fn contact_sheet(pics: &[Picture], cols: usize) -> Picture {
    let (w, h) = (pics[0].width, pics[0].height);
    let cols = cols.max(1).min(pics.len());
    let rows = pics.len().div_ceil(cols);
    let gap = 4;
    let (sw, sh) = (cols * w + (cols + 1) * gap, rows * h + (rows + 1) * gap);
    let mut rgba = vec![0u8; sw * sh * 4];
    for px in rgba.chunks_exact_mut(4) {
        px.copy_from_slice(&[24, 24, 28, 255]);
    }
    for (i, p) in pics.iter().enumerate() {
        let (cx, cy) = (gap + (i % cols) * (w + gap), gap + (i / cols) * (h + gap));
        for y in 0..h {
            for x in 0..w {
                let s = (y * w + x) * 4;
                let t = ((cy + y) * sw + cx + x) * 4;
                // Composite the premultiplied frame over the sheet.
                let a = p.rgba[s + 3] as u32;
                for k in 0..3 {
                    rgba[t + k] = (p.rgba[s + k] as u32 + rgba[t + k] as u32 * (255 - a) / 255).min(255) as u8;
                }
                rgba[t + 3] = 255;
            }
        }
    }
    Picture { width: sw, height: sh, rgba }
}

/// Everything a render needs besides the times.
pub struct DocSpec {
    pub source: String,
    pub first_line: u32,
    pub base: PathBuf,
    pub duration: f32,
}

/// The scene of a file: the file itself (a scene-only source).
pub fn scene_source(text: &str) -> Result<(String, u32, Option<f32>), String> {
    let duration = find_number(text, "duration");
    if text.contains("Motion{") {
        return Err("this is a Motion document: its scenes are drawn with it by `makepad-motion`".into());
    }
    Ok((text.to_string(), 1, duration))
}

fn find_number(text: &str, key: &str) -> Option<f32> {
    let at = text.find(&format!("{key}:"))?;
    let rest = text[at + key.len() + 1..].trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[derive(Clone)]
pub struct Options {
    pub size: (usize, usize),
    pub fps: f32,
    pub edit_offset: f32,
    pub music: Option<f32>,
    pub library: Option<PathBuf>,
    pub media: HashMap<String, PathBuf>,
    pub inputs: HashMap<String, InputValue>,
    /// `--texture name=image.png`: host textures for `texture: "name"`.
    pub textures: HashMap<String, PathBuf>,
    /// Song timing for `word(..)` / `beat_near(..)` (hosts supply it; the
    /// CLI renders without).
    pub song: Option<std::sync::Arc<crate::script::SongTiming>>,
    /// Draw as a preview does (realtime: motion blur by the camera's screen
    /// motion, one frame per paint), not locked time.
    pub realtime: bool,
    /// Point of the key light's disc toward which shadow cascades draw,
    /// for a host averaging recordings; `None` is the centre.
    pub sun_disc: Option<[f32; 2]>,
}

impl Default for Options {
    fn default() -> Self {
        Self { size: (960, 540), fps: 30.0, edit_offset: 0.0, music: None, library: None, media: HashMap::new(), inputs: HashMap::new(), textures: HashMap::new(), song: None, realtime: false, sun_disc: None }
    }
}

/// One frame to render, and what to do with it.
pub struct Job {
    pub label: String,
    pub source: String,
    pub first_line: u32,
    pub base: PathBuf,
    pub t: f32,
    pub opts: Options,
}

enum Command {
    Check(PathBuf, Options),
    Render { doc: PathBuf, at: Vec<f32>, out: PathBuf, opts: Options },
    Strip { doc: PathBuf, frames: usize, cols: usize, duration: Option<f32>, edit_times: Option<Vec<f32>>, out: PathBuf, opts: Options },
    Bench { doc: PathBuf, frames: usize, opts: Options },
    Batch { jobs: Vec<Job>, check: BatchCheck },
}

type BatchCheck = Box<dyn FnOnce(&HashMap<String, Picture>) -> Result<String, String> + Send>;

static BATCH: std::sync::Mutex<Option<Command>> = std::sync::Mutex::new(None);

/// Queue frames before starting the app. After the last readback, `check`
/// receives the pictures by job label and its result sets the exit status.
pub fn set_batch(jobs: Vec<Job>, check: impl FnOnce(&HashMap<String, Picture>) -> Result<String, String> + Send + 'static) {
    *BATCH.lock().unwrap_or_else(|e| e.into_inner()) = Some(Command::Batch { jobs, check: Box::new(check) });
}

fn usage() -> String {
    "usage: makepad-motion3d check <doc> | render <doc> --at t,t,.. --out <dir> | strip <doc> --out <png> [--frames n --cols n --duration s] \
     | bench <doc> [--frames n]; options: --size WxH --fps n --music synthetic[:bpm] --edit-offset s \
     --library dir --media id=file --input name=value"
        .into()
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    let args: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--remote") && *a != "--stdin-loop").collect();
    let mode = args.first().ok_or_else(usage)?.as_str();
    let value = |name: &str| args.iter().position(|a| *a == name).and_then(|i| args.get(i + 1)).map(|v| v.to_string());
    let all = |name: &str| args.windows(2).filter(|w| w[0] == name).map(|w| w[1].to_string()).collect::<Vec<_>>();
    let mut opts = Options::default();
    if let Some(s) = value("--size") {
        let (w, h) = s.split_once('x').ok_or("--size is WxH")?;
        opts.size = (w.parse().map_err(|_| "--size width")?, h.parse().map_err(|_| "--size height")?);
    }
    if let Some(f) = value("--fps") {
        opts.fps = f.parse().map_err(|_| "--fps")?;
    }
    if let Some(o) = value("--edit-offset") {
        opts.edit_offset = o.parse().map_err(|_| "--edit-offset")?;
    }
    if let Some(m) = value("--music") {
        opts.music = Some(m.strip_prefix("synthetic").map(|r| r.trim_start_matches(':').parse().unwrap_or(120.0)).ok_or("--music synthetic[:bpm]")?);
    }
    opts.library = value("--library").map(PathBuf::from);
    for m in all("--media") {
        let (id, file) = m.split_once('=').ok_or("--media id=file")?;
        opts.media.insert(id.to_string(), PathBuf::from(file));
    }
    for t in all("--texture") {
        let (k, v) = t.split_once('=').ok_or("--texture name=image.png")?;
        opts.textures.insert(k.to_string(), PathBuf::from(v));
    }
    for i in all("--input") {
        let (k, v) = i.split_once('=').ok_or("--input name=value")?;
        let value = v.parse::<f64>().map(InputValue::Number).unwrap_or_else(|_| InputValue::Text(v.to_string()));
        opts.inputs.insert(k.to_string(), value);
    }
    let doc = || args.get(1).map(|d| PathBuf::from(d.as_str())).ok_or_else(usage);
    Ok(match mode {
        "check" => Command::Check(doc()?, opts),
        "render" => {
            let at = value("--at").ok_or("--at t,t,...")?.split(',').map(|t| t.trim().parse::<f32>().map_err(|_| format!("--at: `{t}`"))).collect::<Result<Vec<_>, _>>()?;
            Command::Render { doc: doc()?, at, out: PathBuf::from(value("--out").ok_or("--out <dir>")?), opts }
        }
        "strip" => Command::Strip {
            doc: doc()?,
            frames: value("--frames").and_then(|f| f.parse().ok()).unwrap_or(8),
            cols: value("--cols").and_then(|f| f.parse().ok()).unwrap_or(4),
            duration: value("--duration").and_then(|f| f.parse().ok()),
            // Frames at these EDIT times (the layer's t = edit time − --edit-offset).
            edit_times: value("--edit-times").map(|v| v.split(',').filter_map(|t| t.trim().parse().ok()).collect()),
            out: PathBuf::from(value("--out").ok_or("--out <strip.png>")?),
            opts,
        },
        "bench" => Command::Bench { doc: doc()?, frames: value("--frames").and_then(|f| f.parse().ok()).unwrap_or(30), opts },
        _ => return Err(usage()),
    })
}

fn read_doc(path: &Path) -> Result<DocSpec, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (source, first_line, duration) = scene_source(&text)?;
    Ok(DocSpec { source, first_line, base: path.parent().unwrap_or(Path::new(".")).to_path_buf(), duration: duration.unwrap_or(6.0) })
}

fn finish(result: Result<String, String>) -> ! {
    match result {
        Ok(report) => {
            println!("{report}");
            std::process::exit(0)
        }
        Err(error) => {
            eprintln!("motion3d: FAILED: {error}");
            std::process::exit(1)
        }
    }
}

/// What happens to finished frames.
enum Sink {
    Files(PathBuf),
    Strip { out: PathBuf, cols: usize, pics: Vec<Picture> },
    Bench { frames: usize, started: Option<f64>, times: Vec<f64>, record_ms: Vec<f64>, instances: usize, passes: usize },
    Batch { pictures: HashMap<String, Picture>, check: BatchCheck },
}

enum State {
    Idle,
    Recorded,
    Reading(ReadbackTicket),
}

#[derive(Script, ScriptHook, Widget)]
pub struct Motion3dHost {
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
    readback_wait_logged: bool,
    #[rust]
    jobs: VecDeque<Job>,
    #[rust]
    sink: Option<Sink>,
    #[rust]
    layers: HashMap<(String, u32), Scene3dLayer>,
    #[rust]
    state: Option<State>,
    #[rust]
    since: f64,
    #[rust]
    report: Vec<String>,
}

impl Motion3dHost {
    fn start(&mut self, cx: &mut Cx, command: Command) {
        match command {
            Command::Check(path, opts) => {
                let spec = read_doc(&path).unwrap_or_else(|e| finish(Err(e)));
                let resolver = FileResolver { base: spec.base.clone(), assets: opts.media.clone(), library: opts.library.clone() };
                match Scene3dLayer::load(&spec.source, LoadOptions { name: path.display().to_string(), inputs: opts.inputs.clone(), resolver: &resolver, first_line: spec.first_line, origin_file: None, song: opts.song.clone() }) {
                    Ok(l) => {
                        let w: Vec<String> = l.doc.warnings.iter().map(|d| format!("{}:{d}", path.display())).collect();
                        finish(Ok(format!("motion3d: {} loads ({} warnings)\n{}", path.display(), w.len(), w.join("\n"))))
                    }
                    Err(e) => finish(Err(e.iter().map(|d| format!("{}:{d}", path.display())).collect::<Vec<_>>().join("\n"))),
                }
            }
            Command::Render { doc, at, out, opts } => {
                let spec = read_doc(&doc).unwrap_or_else(|e| finish(Err(e)));
                let _ = std::fs::create_dir_all(&out);
                for t in at {
                    self.jobs.push_back(Job { label: format!("t{t:.3}"), source: spec.source.clone(), first_line: spec.first_line, base: spec.base.clone(), t, opts: opts.clone() });
                }
                self.sink = Some(Sink::Files(out));
            }
            Command::Strip { doc, frames, cols, duration, edit_times, out, opts } => {
                let spec = read_doc(&doc).unwrap_or_else(|e| finish(Err(e)));
                let d = duration.unwrap_or(spec.duration);
                let times: Vec<f32> = match edit_times {
                    Some(e) => e.iter().map(|e| e - opts.edit_offset).collect(),
                    None => (0..frames).map(|k| d * k as f32 / (frames.max(2) - 1) as f32 * 0.999).collect(),
                };
                for t in times {
                    self.jobs.push_back(Job { label: format!("t{t:.2}"), source: spec.source.clone(), first_line: spec.first_line, base: spec.base.clone(), t, opts: opts.clone() });
                }
                self.sink = Some(Sink::Strip { out, cols, pics: Vec::new() });
            }
            Command::Bench { doc, frames, opts } => {
                let spec = read_doc(&doc).unwrap_or_else(|e| finish(Err(e)));
                // Two warm-up frames (shader compiles, uploads), then timed.
                for k in 0..frames + 2 {
                    let t = (k as f32 / opts.fps) % spec.duration.max(0.1);
                    self.jobs.push_back(Job { label: format!("f{k}"), source: spec.source.clone(), first_line: spec.first_line, base: spec.base.clone(), t, opts: opts.clone() });
                }
                self.sink = Some(Sink::Bench { frames, started: None, times: Vec::new(), record_ms: Vec::new(), instances: 0, passes: 0 });
            }
            Command::Batch { jobs, check } => {
                self.jobs = jobs.into();
                self.sink = Some(Sink::Batch { pictures: HashMap::new(), check });
                if self.jobs.is_empty() {
                    self.done();
                }
            }
        }
        self.state = Some(State::Idle);
        self.since = Cx::monotonic_now();
        self.next_frame = cx.new_next_frame();
    }

    fn layer_for(&mut self, job: &Job) -> Result<&mut Scene3dLayer, String> {
        let key = (job.source.clone(), job.first_line);
        if !self.layers.contains_key(&key) {
            let resolver = FileResolver { base: job.base.clone(), assets: job.opts.media.clone(), library: job.opts.library.clone() };
            let opts = LoadOptions { name: job.label.clone(), inputs: job.opts.inputs.clone(), resolver: &resolver, first_line: job.first_line, origin_file: None, song: job.opts.song.clone() };
            let diags = |e: Vec<crate::script::Diagnostic>| e.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n");
            let layer = Scene3dLayer::load(&job.source, opts).map_err(diags)?;
            for w in &layer.doc.warnings {
                eprintln!("motion3d: warning: {w}");
            }
            self.layers.insert(key.clone(), layer);
        }
        Ok(self.layers.get_mut(&key).unwrap())
    }

    fn draw_job(&mut self, cx: &mut Cx2d) {
        match self.state {
            Some(State::Idle) => {
                let Some(job) = self.jobs.pop_front() else { return };
                // A built model it places landed since it loaded: load it again.
                let key = (job.source.clone(), job.first_line);
                if self.layers.get(&key).is_some_and(|l| l.doc.models_changed()) {
                    self.layers.remove(&key);
                }
                let opts = job.opts.clone();
                let t = job.t;
                let music = opts.music.map(|bpm| SyntheticMusic { bpm, drop: 4.0 });
                let audio: &dyn AudioSource = match &music {
                    Some(m) => m,
                    None => &NoAudio,
                };
                let layer = match self.layer_for(&job) {
                    Ok(l) => l,
                    Err(e) => finish(Err(format!("{}: {e}", job.label))),
                };
                for (name, path) in &opts.textures {
                    let img = std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| crate::scene::ImageData::decode(&b, true));
                    match img {
                        Ok(img) => {
                            let data: Vec<u32> = img.rgba.chunks_exact(4).map(|p| ((p[3] as u32) << 24) | ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32).collect();
                            let t = Texture::new_with_format(cx.cx, TextureFormat::VecBGRAu8_32 { width: img.width, height: img.height, data: Some(data), updated: TextureUpdated::Full });
                            layer.set_texture(name, t);
                        }
                        Err(e) => finish(Err(format!("--texture {name}: {e}"))),
                    }
                }
                layer.set_realtime(opts.realtime);
                layer.set_sun_disc_sample(opts.sun_disc);
                let tex = layer.draw(cx, &FrameTime { t, fps: opts.fps, edit_offset: opts.edit_offset }, opts.size, audio);
                if tex.is_none() {
                    // The VM was busy: try this job again next frame.
                    self.jobs.push_front(job);
                    return;
                }
                for e in layer.renderer.errors.drain(..) {
                    eprintln!("motion3d: {}: {e}", job.label);
                }
                let s = layer.renderer.stats.clone();
                if let Some(Sink::Bench { record_ms, instances, passes, .. }) = &mut self.sink {
                    record_ms.push(s.record_ms);
                    *instances = s.instances;
                    *passes = s.passes;
                }
                self.jobs.push_front(job);
                self.state = Some(State::Recorded);
                self.since = Cx::monotonic_now();
            }
            Some(State::Recorded) => {
                let Some(job) = self.jobs.front() else { return };
                let key = (job.source.clone(), job.first_line);
                let layer = self.layers.get_mut(&key).unwrap();
                // Sub-frames are evaluated as they are recorded: the same
                // audio as the frame's.
                let music = job.opts.music.map(|bpm| SyntheticMusic { bpm, drop: 4.0 });
                let audio: &dyn AudioSource = match &music {
                    Some(m) => m,
                    None => &NoAudio,
                };
                // Its models landed: record the frame again with them.
                if layer.doc.models_changed() {
                    self.layers.remove(&key);
                    self.state = Some(State::Idle);
                    return;
                }
                if !layer.painted(cx.cx) {
                    layer.reattach(cx, audio);
                    // A model building is progress, not a stall.
                    if layer.doc.waiting_for_models() {
                        self.since = Cx::monotonic_now();
                    }
                    if Cx::monotonic_now() - self.since > 60.0 {
                        finish(Err(format!("{}: the frame did not paint in 60 s: {}", job.label, layer.renderer.painting_report(cx.cx))));
                    }
                    return;
                }
                if matches!(self.sink, Some(Sink::Bench { .. })) {
                    // Benchmarks time record + GPU paint; no readback (a 4K
                    // frame is over the platform's 32 MB readback limit).
                    let job = self.jobs.pop_front().unwrap();
                    self.state = Some(State::Idle);
                    self.frame_done(job, Picture { width: 0, height: 0, rgba: Vec::new() });
                    return;
                }
                let tex = layer.renderer.output().expect("a recorded frame has an output");
                match tex.read_back(cx, ReadbackRequest::default()) {
                    Ok(ticket) => {
                        if self.readback_wait_logged { self.since = Cx::monotonic_now(); }
                        self.readback_wait_logged = false;
                        self.state = Some(State::Reading(ticket));
                    }
                    Err(ReadbackError::Backpressure) => {
                        if !self.readback_wait_logged {
                            log!("motion3d: context readback request limit reached; {} frame waits", job.label);
                            self.readback_wait_logged = true;
                        }
                        layer.reattach(cx, audio);
                    }
                    Err(e) => finish(Err(format!("readback: {e}"))),
                }
            }
            _ => {}
        }
    }

    fn take_readback(&mut self, cx: &mut Cx) {
        let Some(State::Reading(ticket)) = self.state else { return };
        if Cx::monotonic_now() - self.since > 90.0 {
            finish(Err("the readback did not arrive in 90 s".into()));
        }
        for result in cx.try_take_texture_readbacks_for(&[ticket]) {
            let data = result.data.unwrap_or_else(|e| finish(Err(format!("readback: {e}"))));
            let pic = Picture { width: result.width, height: result.height, rgba: data.to_vec() };
            let job = self.jobs.pop_front().unwrap();
            self.state = Some(State::Idle);
            self.frame_done(job, pic);
        }
    }

    fn frame_done(&mut self, job: Job, pic: Picture) {
        let now = Cx::monotonic_now();
        match self.sink.as_mut().unwrap() {
            Sink::Files(dir) => {
                let path = dir.join(format!("{}.png", job.label));
                std::fs::write(&path, encode_png(&pic).unwrap_or_else(|e| finish(Err(e)))).unwrap_or_else(|e| finish(Err(format!("{}: {e}", path.display()))));
                self.report.push(format!("  {} {:016x} -> {}", job.label, pic.hash(), path.display()));
            }
            Sink::Strip { pics, .. } => {
                self.report.push(format!("  {} {:016x}", job.label, pic.hash()));
                pics.push(pic);
            }
            Sink::Bench { started, times, .. } => {
                // Timing starts when the warm-up frames (f0, f1) are done.
                if job.label == "f1" {
                    *started = Some(now);
                } else if started.is_some() {
                    times.push(now);
                }
            }
            Sink::Batch { pictures, .. } => {
                pictures.insert(job.label, pic);
            }
        }
        if self.jobs.is_empty() {
            self.done();
        }
    }

    fn done(&mut self) -> ! {
        match self.sink.take().unwrap() {
            Sink::Files(_) => finish(Ok(format!("motion3d: rendered\n{}", self.report.join("\n")))),
            Sink::Strip { out, cols, pics } => {
                let sheet = contact_sheet(&pics, cols);
                std::fs::write(&out, encode_png(&sheet).unwrap_or_else(|e| finish(Err(e)))).unwrap_or_else(|e| finish(Err(format!("{}: {e}", out.display()))));
                finish(Ok(format!("motion3d: strip {} ({} frames)\n{}", out.display(), pics.len(), self.report.join("\n"))))
            }
            Sink::Bench { frames, started, times, record_ms, instances, passes } => {
                let start = started.unwrap_or(0.0);
                let total = times.last().copied().unwrap_or(start) - start;
                let per = total / frames.max(1) as f64 * 1000.0;
                let rec: f64 = record_ms.iter().skip(2).sum::<f64>() / record_ms.len().saturating_sub(2).max(1) as f64;
                finish(Ok(format!(
                    "motion3d: bench {frames} frames: {per:.2} ms/frame end to end (evaluate + record + GPU paint), CPU record {rec:.2} ms; {passes} passes, {instances} instances"
                )))
            }
            Sink::Batch { pictures, check } => finish(check(&pictures)),
        }
    }
}

impl Widget for Motion3dHost {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        self.take_readback(cx);
        if self.next_frame.is_event(event).is_some() {
            self.draw_bg.redraw(cx);
            self.next_frame = cx.new_next_frame();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.draw_bg.draw_walk(cx, walk);
        if self.state.is_some() {
            self.draw_job(cx);
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
        // Makepad's bundled fonts (Text3D, extruded text) are the widget
        // library's: the host says where (no build-machine path).
        makepad_text_mesh::letters::set_fonts_dir(std::path::Path::new(makepad_widgets::widgets_dir()).join("resources"));
        let batch = BATCH.lock().unwrap_or_else(|e| e.into_inner()).take();
        let command = batch.unwrap_or_else(|| {
            let args: Vec<String> = std::env::args().collect();
            parse_args(&args).unwrap_or_else(|e| finish(Err(e)))
        });
        if let Some(mut host) = self.ui.widget(cx, ids!(host)).borrow_mut::<Motion3dHost>() {
            host.start(cx, command);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::script_mod(vm);
        crate::shared::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
