//! The hidden window the picture commands draw in: `frame`, `strip` and
//! `onion` drive a `FrameGrabber`, `render` the renderer's `CutRenderer`,
//! from this widget's draw. The window is always hidden, and the process
//! leaves as soon as its parent does (taking a partial render with it).

use makepad_story_edit_render::render::{fnv1a, partial_path};
use makepad_story_edit_render::{CutJob, CutRenderer, LocalMedia, MediaResolver, RenderOptions, RenderState};
use makepad_story_edit_tools::effect_strip::{default_card_dir, EffectSpec, EffectStrip, StripOptions};
use makepad_story_edit_tools::exposure::ExposureSummary;
use makepad_story_edit_tools::grab::{onion_picture, strip_picture, FrameGrabber};
use makepad_story_edit_tools::image::{encode_png, Picture};
use makepad_story_edit_tools::inspect::num;
use makepad_story_cut::{CutDoc, RunRef};
use makepad_strict_json::{obj, s, Value};
use makepad_widgets::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.CutToolsHostBase = #(CutToolsHost::register_widget(vm))
    mod.widgets.CutToolsHost = set_type_default() do mod.widgets.CutToolsHostBase{
        width: Fill
        height: Fill
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Edit tools"
                window.inner_size: vec2(320, 200)
                body +: {
                    host := mod.widgets.CutToolsHost{}
                }
            }
        }
    }
}

/// What a picture command draws: the cut (none for `effect-strip`, which
/// makes its own).
pub struct Job {
    pub doc: Option<Arc<CutDoc>>,
    pub resolver: Box<dyn MediaResolver>,
    pub marks: HashMap<RunRef, HashMap<String, f64>>,
    pub json: bool,
}

/// A picture command.
pub enum Gpu {
    /// `crop` `[x, y, w, h]` in the output's pixels (`output`); `names`:
    /// the layers to outline with their names (a motion document's).
    Frame { times: Vec<f64>, width: u32, out: PathBuf, crop: Option<[f64; 4]>, output: (u32, u32), names: Option<Vec<makepad_story_edit_tools::motion::FrameLayer>> },
    Strip { times: Vec<f64>, width: u32, columns: Option<usize>, out: PathBuf },
    Onion { times: Vec<f64>, width: u32, out: PathBuf },
    Render { draft: bool, out: PathBuf },
    EffectStrip { spec: EffectSpec, options: StripOptions, columns: Option<usize>, out: PathBuf },
}

thread_local! {
    static PENDING: RefCell<Option<(Job, Gpu)>> = const { RefCell::new(None) };
}

/// `effect-strip` in the hidden window; never returns.
pub fn run_effect_strip(spec: EffectSpec, options: StripOptions, columns: Option<usize>, out: PathBuf, json: bool) -> ! {
    // No cut document: the strip builds its own.
    let job = Job { doc: None, resolver: Box::new(LocalMedia::new()), marks: HashMap::new(), json };
    run(job, Gpu::EffectStrip { spec, options, columns, out })
}

/// Run `gpu` in the hidden window; never returns.
pub fn run(job: Job, gpu: Gpu) -> ! {
    // Always hidden: this is a tool, never a window for the user.
    std::env::set_var("MAKEPAD_HIDE_WINDOWS", "1");
    PENDING.with(|pending| *pending.borrow_mut() = Some((job, gpu)));
    app_main();
    std::process::exit(0)
}

/// Print and leave: the report on stdout (0), or the failure (1).
fn finish(json: bool, result: Result<(String, Value), String>) -> ! {
    match result {
        Ok((text, value)) => {
            if json {
                println!("{}", value.to_json());
            } else {
                println!("{text}");
            }
            std::process::exit(0)
        }
        Err(error) => {
            if json {
                println!("{}", obj(vec![("ok", Value::Bool(false)), ("error", s(error.clone()))]).to_json());
            } else {
                eprintln!("makepad-edit-tools: FAILED: {error}");
            }
            std::process::exit(1)
        }
    }
}

/// Leave when the parent process goes away (the agent or shell that ran
/// this was killed), removing `partial` first.
fn watch_parent(cx: &mut Cx, partial: Option<PathBuf>) {
    #[cfg(unix)]
    {
        let parent = std::os::unix::process::parent_id();
        let watcher = cx.spawn_worker(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if std::os::unix::process::parent_id() != parent {
                eprintln!("makepad-edit-tools: the parent is gone; exiting");
                if let Some(partial) = &partial {
                    let _ = std::fs::remove_file(partial);
                }
                std::process::exit(3);
            }
        });
        if let Ok(task) = watcher {
            task.detach();
        }
    }
    #[cfg(not(unix))]
    let _ = (cx, partial);
}

fn write_png(picture: &Picture, out: &Path) -> Result<(), String> {
    let png = encode_png(picture)?;
    if let Some(dir) = out.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(out, png).map_err(|e| format!("{}: {e}", out.display()))
}

#[derive(Script, ScriptHook, Widget)]
pub struct CutToolsHost {
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
    grab: Option<(FrameGrabber, Gpu)>,
    #[rust]
    renderer: Option<CutRenderer>,
    #[rust]
    effect: Option<(EffectStrip, Option<usize>, PathBuf)>,
    #[rust]
    json: bool,
    #[rust]
    started: f64,
    #[rust]
    last_report: u64,
}

impl CutToolsHost {
    fn start(&mut self, cx: &mut Cx, job: Job, gpu: Gpu) {
        self.json = job.json;
        self.started = Cx::monotonic_now();
        let edit = |doc: Option<Arc<CutDoc>>| CutJob { doc: doc.expect("a cut to draw"), marks: job.marks, resolver: job.resolver, audio: false };
        match gpu {
            Gpu::EffectStrip { spec, options, columns, out } => {
                watch_parent(cx, None);
                match EffectStrip::new(cx, &spec, &options, &default_card_dir()) {
                    Ok(strip) => {
                        for warning in &strip.warnings {
                            eprintln!("makepad-edit-tools: {warning}");
                        }
                        self.effect = Some((strip, columns, out));
                    }
                    Err(error) => finish(self.json, Err(error)),
                }
            }
            Gpu::Render { draft, out } => {
                let mut options = RenderOptions::new(out);
                options.half = draft;
                watch_parent(cx, Some(partial_path(&options.path)));
                self.renderer = Some(CutRenderer::new(cx, CutJob { audio: true, ..edit(job.doc) }, options));
            }
            Gpu::Frame { ref times, width, .. } | Gpu::Strip { ref times, width, .. } | Gpu::Onion { ref times, width, .. } => {
                watch_parent(cx, None);
                let grabber = FrameGrabber::new(cx, edit(job.doc), times, width);
                self.grab = Some((grabber, gpu));
            }
        }
        self.next_frame = cx.new_next_frame();
    }

    fn check(&mut self) {
        if let Some((strip, columns, out)) = &self.effect {
            match strip.result() {
                Some(Ok(frames)) => finish(self.json, pictures(frames, &Gpu::Strip { times: Vec::new(), width: 0, columns: *columns, out: out.clone() })),
                Some(Err(error)) => finish(self.json, Err(error.to_string())),
                None => {}
            }
        }
        if let Some((grabber, gpu)) = &self.grab {
            match grabber.result() {
                Some(Ok(frames)) => finish(self.json, pictures(frames, gpu)),
                Some(Err(error)) => finish(self.json, Err(error.to_string())),
                None => {}
            }
        }
        let Some(renderer) = &self.renderer else { return };
        // A failed render leaves only once its encoder has let go, so no
        // partial file outlives it.
        if matches!(renderer.state(), RenderState::Failed(_) | RenderState::Cancelled) && !renderer.settled() {
            return;
        }
        match renderer.state().clone() {
            RenderState::Rendering { frame, frames } if frame >= self.last_report + 60 => {
                self.last_report = frame;
                eprintln!("makepad-edit-tools: frame {frame} / {frames} ({:.1} s)", Cx::monotonic_now() - self.started);
            }
            RenderState::Done(result) => {
                let seconds = Cx::monotonic_now() - self.started;
                let hash = fnv1a(&result.frame_hashes.iter().flat_map(|hash| hash.to_le_bytes()).collect::<Vec<u8>>());
                let text = format!(
                    "rendered {} frames {}x{} at {}/{} fps, sound {}, in {seconds:.1} s -> {}{}",
                    result.frames,
                    result.size.0,
                    result.size.1,
                    result.fps.0,
                    result.fps.1,
                    result.audio,
                    result.path.display(),
                    result.sidecars.iter().map(|p| format!("\n  captions {}", p.display())).collect::<String>()
                );
                let value = obj(vec![
                    ("ok", Value::Bool(true)),
                    ("command", s("render")),
                    ("out", s(result.path.to_string_lossy())),
                    ("frames", Value::Int(result.frames as i64)),
                    ("width", Value::Int(result.size.0 as i64)),
                    ("height", Value::Int(result.size.1 as i64)),
                    ("fps", s(format!("{}/{}", result.fps.0, result.fps.1))),
                    ("audio", Value::Bool(result.audio)),
                    ("seconds", num(seconds)),
                    ("frames_hash", s(format!("{hash:016x}"))),
                    ("sidecars", Value::Arr(result.sidecars.iter().map(|p| s(p.to_string_lossy())).collect())),
                ]);
                finish(self.json, Ok((text, value)))
            }
            RenderState::Failed(error) => finish(self.json, Err(error)),
            RenderState::Cancelled => finish(self.json, Err("cancelled".into())),
            _ => {}
        }
    }
}

/// Write what a picture command made; its report. Each frame carries its
/// exposure (of the whole frame, before scaling or a crop); the report's
/// `exposure` is the one frame's, or the run's worst frames and mean luma.
fn pictures(frames: &[makepad_story_edit_tools::GrabbedFrame], gpu: &Gpu) -> Result<(String, Value), String> {
    let list = Value::Arr(frames.iter().map(|f| obj(vec![("frame", Value::Int(f.frame as i64)), ("t", num(f.t)), ("exposure", f.exposure.to_json())])).collect());
    let (exposure, exposure_text) = match frames {
        [one] => (one.exposure.to_json(), one.exposure.to_text()),
        _ => match ExposureSummary::of(&frames.iter().map(|f| (f.frame, f.t, f.exposure)).collect::<Vec<_>>()) {
            Some(summary) => (summary.to_json(), summary.to_text()),
            None => (Value::Null, String::new()),
        },
    };
    let (command, picture, out) = match gpu {
        Gpu::Frame { out, crop, output, names, .. } => {
            let mut picture = frames[0].picture.flattened();
            // Output pixels to picture pixels.
            let k = picture.width as f64 / output.0.max(1) as f64;
            if let Some(layers) = names {
                let boxes: Vec<(String, [f64; 4], bool)> = layers.iter().map(|l| (l.label.clone(), l.bounds.map(|v| v * k), l.group)).collect();
                let scale = if picture.width >= 1280 { 2 } else { 1 };
                makepad_story_edit_tools::image::draw_names(&mut picture, &boxes, scale);
            }
            if let Some([x, y, w, h]) = crop {
                picture = picture.cropped((x * k).round() as i64, (y * k).round() as i64, (w * k).round() as i64, (h * k).round() as i64);
            }
            ("frame", picture, out)
        }
        Gpu::Strip { columns, out, .. } => ("strip", strip_picture(frames, *columns), out),
        Gpu::Onion { out, .. } => ("onion", onion_picture(frames)?, out),
        Gpu::Render { .. } | Gpu::EffectStrip { .. } => return Err("not a picture command".into()),
    };
    write_png(&picture, out)?;
    let times = match frames {
        [one] => format!("frame {} ({:.3} s)", one.frame, one.t),
        [first, .., last] => format!("{} frames {}..{} ({:.3}–{:.3} s)", frames.len(), first.frame, last.frame, first.t, last.t),
        [] => String::new(),
    };
    let mut text = format!("{command}: {times}, {}x{} -> {}", picture.width, picture.height, out.display());
    let mut fields = vec![
        ("ok", Value::Bool(true)),
        ("command", s(command)),
        ("out", s(out.to_string_lossy())),
        ("width", Value::Int(picture.width as i64)),
        ("height", Value::Int(picture.height as i64)),
        ("frames", list),
        ("exposure", exposure),
    ];
    if let Gpu::Frame { crop, names, .. } = gpu {
        if let Some(c) = crop {
            fields.push(("crop", Value::Arr(c.iter().map(|v| num(*v)).collect())));
            text.push_str(&format!(" (crop {:.0},{:.0} {:.0}x{:.0})", c[0], c[1], c[2], c[3]));
        }
        if let Some(layers) = names {
            fields.push(("layers", Value::Arr(layers.iter().map(|l| obj(vec![
                ("path", s(l.path.clone())),
                ("name", l.name.clone().map_or(Value::Null, s)),
                ("kind", s(l.kind)),
                ("box", Value::Arr([l.bounds[0], l.bounds[1], l.bounds[2] - l.bounds[0], l.bounds[3] - l.bounds[1]].iter().map(|v| num(v.round())).collect())),
            ])).collect())));
            text.push_str(&format!(", {} layers named", layers.len()));
        }
    }
    if !exposure_text.is_empty() {
        text.push_str(&format!("\nexposure: {exposure_text}"));
    }
    // What each 3D scene's nodes cover, per grabbed time.
    let scenes = makepad_story_edit_tools::coverage::summary(&frames.iter().map(|f| (f.t, f.coverage.as_slice())).collect::<Vec<_>>());
    if !scenes.is_empty() {
        for (_, report) in &scenes {
            text.push_str(&format!("\n{report}"));
        }
        fields.push(("coverage", Value::Arr(scenes.into_iter().map(|(json, _)| json).collect())));
    }
    let value = obj(fields);
    Ok((text, value))
}

impl Widget for CutToolsHost {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Some(renderer) = &mut self.renderer {
            renderer.handle_event(cx, event);
        }
        if let Some((grabber, _)) = &mut self.grab {
            grabber.handle_event(cx, event);
        }
        if let Some((strip, ..)) = &mut self.effect {
            strip.handle_event(cx, event);
        }
        if self.next_frame.is_event(event).is_some() {
            self.check();
            self.draw_bg.redraw(cx);
            self.next_frame = cx.new_next_frame();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.draw_bg.draw_walk(cx, walk);
        if let Some(renderer) = &mut self.renderer {
            renderer.draw(cx);
        }
        if let Some((grabber, _)) = &mut self.grab {
            grabber.draw(cx);
        }
        if let Some((strip, ..)) = &mut self.effect {
            strip.draw(cx);
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
        let Some((job, gpu)) = PENDING.with(|pending| pending.borrow_mut().take()) else {
            eprintln!("makepad-edit-tools: nothing to draw");
            std::process::exit(2)
        };
        if let Some(mut host) = self.ui.widget(cx, ids!(host)).borrow_mut::<CutToolsHost>() {
            host.start(cx, job, gpu);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::script_mod(vm);
        makepad_content_packs::script_mod(vm);
        makepad_story_edit_render::script_mod(vm);
        makepad_stage_render_kit::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
