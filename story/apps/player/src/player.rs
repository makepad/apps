//! The player widget: the film's frame letterboxed in the window, a loader
//! until the film can start, and the controls (play/pause, a scrubbable
//! timeline for mouse and touch, the time).
//!
//! Time: while playing, the song's clock (its audio callback's position)
//! is the film's time; before the song's device runs, the wall clock. A
//! seek moves both. The frame is drawn at a resolution scale that steps
//! down while frames run long and back up while they are short.

use crate::bundle::Source;
use crate::film::FilmRuntime;
use crate::kernels::KernelLinker;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*

    // The film's frame: the plate's output texture (premultiplied BGRA).
    set_type_default() do #(DrawFilm::script_shader(vm)){
        ..mod.draw.DrawQuad
        tex: texture_2d(float)
        pixel: fn() {
            let c = self.tex.sample(self.pos)
            return vec4(c.xyz, 1.0)
        }
    }

    // The controls' and loader's shapes: 0 a rounded box, 1 a disc, 2 a
    // play triangle, 3 pause bars, 4 a ring arc drawn to `progress`.
    set_type_default() do #(DrawPlayerUi::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sz = self.rect_size
            let sdf = Sdf2d.viewport(self.pos * sz)
            if self.kind < 0.5 {
                sdf.box(0.0, 0.0, sz.x, sz.y, self.radius)
                sdf.fill(self.color)
            } else if self.kind < 1.5 {
                sdf.circle(sz.x * 0.5, sz.y * 0.5, min(sz.x, sz.y) * 0.5)
                sdf.fill(self.color)
            } else if self.kind < 2.5 {
                sdf.move_to(sz.x * 0.32, sz.y * 0.22)
                sdf.line_to(sz.x * 0.80, sz.y * 0.5)
                sdf.line_to(sz.x * 0.32, sz.y * 0.78)
                sdf.close_path()
                sdf.fill(self.color)
            } else if self.kind < 3.5 {
                sdf.box(sz.x * 0.27, sz.y * 0.24, sz.x * 0.16, sz.y * 0.52, 1.0)
                sdf.box(sz.x * 0.57, sz.y * 0.24, sz.x * 0.16, sz.y * 0.52, 1.0)
                sdf.fill(self.color)
            } else {
                // An engraved ring: hairline circle, the arc drawn to
                // `progress` heavier, hatching inside turning with time.
                let c = sz * 0.5
                let r = min(sz.x, sz.y) * 0.5 - 4.0
                let p = self.pos * sz - c
                let d = length(p)
                let a = atan2(p.x, 0.0 - p.y)
                let turn = fract(a / 6.2831853 + 1.0)
                let ink = self.color
                let ring = 1.0 - smoothstep(0.4, 1.4, abs(d - r))
                let drawn = step(turn, self.progress)
                let arc = (1.0 - smoothstep(1.0, 2.2, abs(d - r))) * drawn
                let hatch_x = p.x * 0.7071 + p.y * 0.7071 + self.radius * 6.0
                let line = 1.0 - smoothstep(0.3, 1.1, abs(fract(hatch_x / 7.0) - 0.5) * 7.0)
                let inside = 1.0 - smoothstep(r * 0.62 - 1.0, r * 0.62, d)
                let shade = line * inside * 0.35
                let alpha = clamp(max(max(ring * 0.45, arc), shade), 0.0, 1.0)
                return vec4(ink.xyz * alpha, alpha)
            }
            return sdf.result
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawFilm {
    #[deref]
    pub draw_super: DrawQuad,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawPlayerUi {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
    #[live]
    pub kind: f32,
    #[live]
    pub radius: f32,
    #[live]
    pub progress: f32,
}

impl DrawPlayerUi {
    fn shape(&mut self, cx: &mut Cx2d, kind: f32, rect: Rect, color: Vec4f, radius: f32) {
        self.kind = kind;
        self.color = color;
        self.radius = radius;
        self.draw_abs(cx, rect);
    }
}

/// The GPU time of the last presented frames (ms, mean), where the
/// platform reports frames (native Metal and Vulkan); None in the browser,
/// where the scale falls back to elimination (frame long, CPU short).
fn gpu_frame_ms() -> Option<f64> {
    use makepad_widgets::makepad_platform::gpu_frame_timer;
    if !gpu_frame_timer::enabled() {
        gpu_frame_timer::set_enabled(true);
    }
    let frames = gpu_frame_timer::take_frames();
    (!frames.is_empty()).then(|| frames.iter().map(|f| f.busy_ms as f64).sum::<f64>() / frames.len() as f64)
}

/// Resolution scales the frame steps through (of the window's pixels).
const SCALES: [f64; 5] = [1.0, 0.85, 0.7, 0.58, 0.5];
const BONE: Vec4f = Vec4f { x: 0.933, y: 0.914, z: 0.875, w: 1.0 };
const INK: Vec4f = Vec4f { x: 0.039, y: 0.039, z: 0.043, w: 1.0 };
const SIGNAL: Vec4f = Vec4f { x: 1.0, y: 0.302, z: 0.071, w: 1.0 };

fn alpha(c: Vec4f, a: f32) -> Vec4f {
    Vec4f { x: c.x * a, y: c.y * a, z: c.z * a, w: a }
}

fn clock_text(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:02}", (t / 60.0).floor() as u64, (t % 60.0).floor() as u64)
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

struct State {
    film: FilmRuntime,
    linker: KernelLinker,
    t: f64,
    playing: bool,
    /// Start playing as soon as the film can (native `--play`).
    autoplay: bool,
    /// Playing when the loader came back over an unprepared plate: play
    /// resumes once it is prepared.
    resume: bool,
    /// The clock's last tick (wall seconds): this frame's start.
    last_tick: f64,
    /// The display's frame interval: the shortest frame interval while
    /// playing over the last stats window (and the current one so far);
    /// the frame's work, preparation included, fits in it.
    interval: f64,
    interval_now: f64,
    /// Smoothed frame interval (seconds) and how long it has been slow/fast.
    ema_dt: f64,
    /// Smoothed CPU seconds of our own work per frame (the draw: eval,
    /// record, submit) and GPU seconds per frame (where reported).
    ema_cpu: f64,
    ema_gpu: f64,
    /// What the last long frames were bound by ("cpu", "gpu"), for stats.
    bound: &'static str,
    slow_for: f64,
    fast_for: f64,
    scale: usize,
    shown: Option<Texture>,
    /// Dragging the timeline.
    scrub: Option<bool>,
    /// The song's output and the film's clock: a deck a finger scratches.
    transport: crate::audio::Transport,
    /// A finger on the picture.
    scratch: Option<crate::scratch::Scratch>,
    /// The controls show until this wall time.
    controls_until: f64,
    /// The film has shown its first frame: the loader is gone.
    loaded: bool,
    started: f64,
    stats_at: f64,
    stats_frames: u32,
    film_rect: Rect,
    bar_rect: Rect,
    play_rect: Rect,
    /// `--analyse <dir>` (native): every frame on locked time with the
    /// recorders on, for the film builder; then quit.
    #[cfg(not(target_arch = "wasm32"))]
    analyser: Option<crate::analyse::Analyser>,
    /// `--gpu-report <dir>` (native): every step on locked time, the GPU
    /// inventory after each, per plate; then quit.
    #[cfg(not(target_arch = "wasm32"))]
    gpu_report: Option<crate::gpu_report::GpuReport>,
}

/// Where the film is: the pack built in (`--features film`), else a
/// bundle directory (`--film <dir>`, default `film`) natively, `film/`
/// beside the page in the browser.
fn film_source(dir: Option<String>) -> Source {
    #[cfg(feature = "film")]
    {
        let _ = dir;
        match makepad_story_film_pack::Pack::parse(crate::FILM_PACK) {
            Ok(pack) => return Source::Pack(pack),
            Err(e) => error!("player: the built-in film: {e}"),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Source::Dir(std::path::PathBuf::from(dir.unwrap_or_else(|| "film".into())))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = dir;
        Source::Url("film/".into())
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FilmPlayer {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_film: DrawFilm,
    #[live]
    draw_ui: DrawPlayerUi,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_title: DrawText,
    #[live]
    draw_note: DrawText,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    state: Option<State>,
}

impl FilmPlayer {
    pub fn start(&mut self, cx: &mut Cx) {
        let args: Vec<String> = std::env::args().collect();
        let value = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
        let source = film_source(value("--film"));
        if cfg!(target_arch = "wasm32") {
            crate::film::install_font_reader();
        }
        let now = Cx::monotonic_now();
        #[cfg(not(target_arch = "wasm32"))]
        let analyser = crate::analyse::Analyser::from_args().map(|mut a| {
            a.start(cx);
            a
        });
        #[cfg(not(target_arch = "wasm32"))]
        let gpu_report = crate::gpu_report::GpuReport::from_args();
        // Each program's first draw (on Direct3D its compile) in a frame of
        // its own: a plate's first draw at load met several new programs at
        // once, a 9 s frame of the browser's GPU process.
        cx.spread_first_draws(true);
        #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
        let mut film = FilmRuntime::new(cx, source);
        #[cfg(not(target_arch = "wasm32"))]
        if gpu_report.is_some() {
            film.made_textures = Some(Vec::new());
        }
        self.state = Some(State {
            #[cfg(not(target_arch = "wasm32"))]
            analyser,
            #[cfg(not(target_arch = "wasm32"))]
            gpu_report,
            film,
            linker: KernelLinker::default(),
            t: value("--at").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            playing: false,
            autoplay: args.iter().any(|a| a == "--play"),
            resume: false,
            last_tick: now,
            interval: f64::INFINITY,
            interval_now: f64::INFINITY,
            ema_dt: 1.0 / 60.0,
            ema_cpu: 0.0,
            ema_gpu: 0.0,
            bound: "",
            slow_for: 0.0,
            fast_for: 0.0,
            scale: 0,
            shown: None,
            scrub: None,
            transport: Default::default(),
            scratch: None,
            controls_until: now + 3.0,
            loaded: false,
            started: now,
            stats_at: now,
            stats_frames: 0,
            film_rect: Rect::default(),
            bar_rect: Rect::default(),
            play_rect: Rect::default(),
        });
        if let Some(s) = &mut self.state {
            let at = s.t;
            s.transport.seek(at);
        }
        self.next_frame = cx.new_next_frame();
    }

    fn set_playing(&mut self, playing: bool) {
        let Some(s) = &mut self.state else { return };
        if playing {
            s.transport.play();
        } else {
            s.transport.pause();
        }
        s.playing = playing;
        s.last_tick = Cx::monotonic_now();
    }

    fn seek(&mut self, t: f64) {
        let Some(s) = &mut self.state else { return };
        s.t = t.clamp(0.0, s.film.duration().max(0.0));
        s.transport.seek(s.t);
    }

    /// The timeline's time under x.
    fn time_at(&self, x: f64) -> f64 {
        let Some(s) = &self.state else { return 0.0 };
        let r = s.bar_rect;
        ((x - r.pos.x) / r.size.x.max(1.0)).clamp(0.0, 1.0) * s.film.duration()
    }

    /// The clock and the resolution scale, once per display frame.
    fn tick(&mut self, cx: &mut Cx) {
        let Some(s) = &mut self.state else { return };
        let now = Cx::monotonic_now();
        let dt = (now - s.last_tick).clamp(0.0, 0.25);
        s.last_tick = now;
        crate::job::new_frame();
        s.linker.pump(cx);
        // The clock: what the song plays now (the deck, see audio.rs).
        if s.film.song_ready() {
            let duration = s.film.duration();
            s.transport.attach(cx, s.film.song.as_mut(), duration);
        }
        s.t = s.transport.tick(dt);
        s.playing = s.transport.playing();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(a) = &s.analyser {
            s.t = a.time();
            s.playing = false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = &s.gpu_report {
            s.t = r.time();
            s.playing = false;
        }
        // The resolution scale follows the GPU only. A frame that runs long
        // (under ~48 fps for a second) steps it down when the GPU is the
        // long pole: its measured frame time where the platform reports one
        // (native), else by elimination, our own CPU work (eval, record,
        // submit) being well under the frame. A CPU-bound frame keeps its
        // resolution (fewer pixels would not help). Short frames (60 fps
        // for three seconds) step it back up.
        if s.playing {
            if dt > 0.0 {
                s.interval_now = s.interval_now.min(dt);
            }
            s.ema_dt += (dt - s.ema_dt) * 0.1;
            let gpu = gpu_frame_ms().map(|ms| ms / 1000.0);
            if let Some(g) = gpu {
                s.ema_gpu += (g - s.ema_gpu) * 0.2;
            }
            let cpu_bound = s.ema_cpu > s.ema_dt * 0.7;
            let gpu_bound = match gpu {
                Some(_) => s.ema_gpu > 1.0 / 60.0,
                None => !cpu_bound,
            };
            s.bound = if s.ema_dt <= 1.0 / 48.0 { "" } else if gpu_bound { "gpu" } else { "cpu" };
            if s.ema_dt > 1.0 / 48.0 && gpu_bound {
                s.slow_for += dt;
                s.fast_for = 0.0;
            } else if s.ema_dt < 1.0 / 57.0 {
                s.fast_for += dt;
                s.slow_for = 0.0;
            } else {
                s.slow_for = 0.0;
                s.fast_for = 0.0;
            }
            if s.slow_for > 1.0 && s.scale + 1 < SCALES.len() {
                s.scale += 1;
                s.slow_for = 0.0;
                s.ema_dt = 1.0 / 60.0;
            }
            // Up only when the measured GPU time, grown by the pixels the
            // step adds, still fits a display frame (where the GPU time is
            // measured): stepping up into a GPU-bound frame stepped straight
            // back down, and every step re-creates the plate's targets.
            let fits_up = s.scale > 0 && match gpu {
                Some(_) => {
                    let grow = (SCALES[s.scale - 1] / SCALES[s.scale]).powi(2);
                    s.ema_gpu * grow < 1.0 / 60.0
                }
                None => true,
            };
            if s.fast_for > 3.0 && fits_up {
                s.scale -= 1;
                s.fast_for = 0.0;
            }
            s.stats_frames += 1;
            if now - s.stats_at >= 2.0 {
                let fps = s.stats_frames as f64 / (now - s.stats_at);
                let (eval, record) = s.film.take_cost();
                let prep = s.film.take_prep();
                log!(
                    "player stats: t={:.2} fps={:.1} scale={} plate={} eval={eval:.1}ms record={record:.1}ms cpu={:.1}ms gpu={} {} prep={:.0}ms/{} steps (longest {:.1}ms, {} alone, {} waited) interval={:.1}ms",
                    s.t, fps, SCALES[s.scale], s.film.plate_name(s.t).unwrap_or("-"), s.ema_cpu * 1000.0,
                    if gpu_frame_ms().is_some() || s.ema_gpu > 0.0 { format!("{:.1}ms", s.ema_gpu * 1000.0) } else { "n/a".into() },
                    if s.bound.is_empty() { "" } else if s.bound == "cpu" { "cpu-bound" } else { "gpu-bound" },
                    prep.seconds * 1000.0, prep.ran, prep.longest * 1000.0, prep.alone, prep.waited, s.interval_now * 1000.0
                );
                s.interval = s.interval_now;
                s.interval_now = f64::INFINITY;
                s.stats_at = now;
                s.stats_frames = 0;
            }
        } else {
            s.stats_at = now;
            s.stats_frames = 0;
        }
        // Play reached (or a seek landed on) a plate not prepared yet: the
        // loader comes back over the film until it is, and play resumes.
        // Not while a hand holds the timeline or the record.
        if s.loaded && s.scrub.is_none() && s.scratch.is_none() && !s.film.showing_prepared(s.t) {
            s.loaded = false;
            s.resume = s.playing;
            log!("player: {} is not prepared yet: the loader shows until it is", s.film.plate_name(s.t).unwrap_or("-"));
            self.set_playing(false);
        }
        let Some(s) = &mut self.state else { return };
        // Start once the song is ready and every plate is prepared (each
        // program's first draw made behind the loader, FilmRuntime::prepared)
        // and no pipeline compiles on the HAL (a pass that has a picture
        // keeps it while a pipeline its draws use compiles: a program the
        // shown plate built since its last whole draw, say its scene's item
        // material, would change its picture in play) (autoplay), or leave
        // the loader for the play button.
        if !s.loaded && s.film.manifest.is_some() && s.film.prepared() && s.film.ready_at(s.t) && s.film.song_ready() && s.shown.is_some() && cx.pipelines_pending() == 0 {
            s.loaded = true;
            s.controls_until = now + 3.0;
            log!("player: ready to play at {:.2} s ({:.1} s after start)", s.t, now - s.started);
            if s.autoplay || std::mem::take(&mut s.resume) {
                self.set_playing(true);
            }
        }
    }

    fn draw_loader(&mut self, cx: &mut Cx2d, rect: Rect) {
        let Some(s) = &self.state else { return };
        let now = Cx::monotonic_now();
        self.draw_ui.shape(cx, 0.0, rect, BONE, 0.0);
        let (progress, got, total, waiting) = s.film.start_progress(s.t);
        let short = rect.size.x.min(rect.size.y);
        let ring = (short * 0.28).clamp(80.0, 180.0);
        let center = dvec2(rect.pos.x + rect.size.x * 0.5, rect.pos.y + rect.size.y * 0.42);
        self.draw_ui.progress = progress as f32;
        self.draw_ui.shape(cx, 4.0, Rect { pos: center - dvec2(ring, ring) * 0.5, size: dvec2(ring, ring) }, INK, ((now - s.started) * 2.0) as f32);
        // A signal dot rides the arc's end.
        let a = progress * std::f64::consts::TAU;
        let dot = center + dvec2(a.sin(), -a.cos()) * (ring * 0.5 - 4.0);
        self.draw_ui.shape(cx, 1.0, Rect { pos: dot - dvec2(4.0, 4.0), size: dvec2(8.0, 8.0) }, SIGNAL, 0.0);
        let title = s.film.manifest.as_ref().map_or("Film".to_string(), |m| if m.film.title.is_empty() { "Film".into() } else { m.film.title.clone() });
        // The title and the note fit the screen's width (a phone held upright
        // is narrower than a long title): they shrink, never overflow.
        let avail = (rect.size.x - 32.0).max(32.0);
        let fit = |w: f64| (avail / w.max(1.0)).min(1.0).max(0.3);
        let tw = self.draw_title.layout(cx, 0.0, 0.0, None, false, Align::default(), &title).size_in_lpxs.width as f64;
        let k = fit(tw);
        self.draw_title.font_scale = k as f32;
        self.draw_title.draw_abs(cx, dvec2(center.x - tw * k * 0.5, center.y + ring * 0.5 + 24.0), &title);
        let what = if let Some(e) = s.film.errors.first() {
            e.lines().next().unwrap_or("").to_string()
        } else if s.film.manifest.is_none() {
            "loading the film".to_string()
        } else if got < total {
            format!("{} of {}  ·  {waiting}", mib(got), mib(total))
        } else {
            waiting
        };
        let nw = self.draw_note.layout(cx, 0.0, 0.0, None, false, Align::default(), &what).size_in_lpxs.width as f64;
        let k = fit(nw);
        self.draw_note.font_scale = k as f32;
        self.draw_note.draw_abs(cx, dvec2(center.x - nw * k * 0.5, center.y + ring * 0.5 + 72.0), &what);
    }

    fn draw_controls(&mut self, cx: &mut Cx2d, rect: Rect) {
        let Some(s) = &mut self.state else { return };
        let now = Cx::monotonic_now();
        let show = !s.playing || s.scrub.is_some() || s.scratch.is_some() || now < s.controls_until;
        let h = 56.0;
        let strip = Rect { pos: dvec2(rect.pos.x, rect.pos.y + rect.size.y - h), size: dvec2(rect.size.x, h) };
        let pad = 16.0;
        s.play_rect = Rect { pos: dvec2(strip.pos.x + pad, strip.pos.y + 10.0), size: dvec2(36.0, 36.0) };
        let time = format!("{} / {}", clock_text(s.t), clock_text(s.film.duration()));
        let text_x = s.play_rect.pos.x + 36.0 + 12.0;
        let text_w = 96.0;
        s.bar_rect = Rect { pos: dvec2(text_x + text_w + 8.0, strip.pos.y + 20.0), size: dvec2((strip.size.x - (text_x + text_w + 8.0 - strip.pos.x) - pad).max(40.0), 16.0) };
        if !show {
            return;
        }
        let (playing, t, duration, bar, play) = (s.playing, s.t, s.film.duration().max(1e-6), s.bar_rect, s.play_rect);
        let starts: Vec<f64> = s.film.manifest.as_ref().map_or(Vec::new(), |m| m.plates.iter().map(|p| p.start).collect());
        self.draw_ui.shape(cx, 0.0, strip, alpha(INK, 0.62), 0.0);
        self.draw_ui.shape(cx, 1.0, play, BONE, 0.0);
        self.draw_ui.shape(cx, if playing { 3.0 } else { 2.0 }, play, INK, 0.0);
        self.draw_text.draw_abs(cx, dvec2(text_x, strip.pos.y + 20.0), &time);
        // The timeline: a hairline, the plates' starts as ticks, the played
        // part in bone, the playhead in signal orange.
        let mid = bar.pos.y + bar.size.y * 0.5;
        self.draw_ui.shape(cx, 0.0, Rect { pos: dvec2(bar.pos.x, mid - 1.0), size: dvec2(bar.size.x, 2.0) }, alpha(BONE, 0.3), 1.0);
        for start in starts {
            let x = bar.pos.x + bar.size.x * start / duration;
            self.draw_ui.shape(cx, 0.0, Rect { pos: dvec2(x - 0.5, mid - 5.0), size: dvec2(1.0, 10.0) }, alpha(BONE, 0.35), 0.0);
        }
        let x = bar.pos.x + bar.size.x * (t / duration).clamp(0.0, 1.0);
        self.draw_ui.shape(cx, 0.0, Rect { pos: dvec2(bar.pos.x, mid - 1.5), size: dvec2(x - bar.pos.x, 3.0) }, BONE, 1.5);
        self.draw_ui.shape(cx, 1.0, Rect { pos: dvec2(x - 7.0, mid - 7.0), size: dvec2(14.0, 14.0) }, SIGNAL, 0.0);
    }
}

impl Widget for FilmPlayer {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Some(s) = &mut self.state {
            s.film.handle_event(cx, event);
            s.transport.handle_event(cx, event);
        }
        if self.next_frame.is_event(event).is_some() {
            self.tick(cx);
            self.draw_bg.redraw(cx);
            self.next_frame = cx.new_next_frame();
        }
        let Some(s) = &self.state else { return };
        let (loaded, play_rect, bar_rect) = (s.loaded, s.play_rect, s.bar_rect);
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerDown(fe) => {
                cx.set_key_focus(self.draw_bg.area());
                if let Some(s) = &mut self.state {
                    s.controls_until = Cx::monotonic_now() + 3.0;
                }
                if !loaded {
                    return;
                }
                let grow = |r: Rect, by: f64| Rect { pos: r.pos - dvec2(by, by), size: r.size + dvec2(by, by) * 2.0 };
                let t = self.time_at(fe.abs.x);
                let Some(s) = &mut self.state else { return };
                if grow(bar_rect, 14.0).contains(fe.abs) {
                    // The timeline: the film stops dead and jumps with the finger.
                    s.transport.press(false);
                    s.scrub = Some(true);
                    self.seek(t);
                } else if grow(play_rect, 8.0).contains(fe.abs) {
                    let playing = s.playing;
                    self.set_playing(!playing);
                } else {
                    // The picture: a record under the finger (scratch.rs).
                    let width = s.film_rect.size.x;
                    s.scratch = Some(crate::scratch::Scratch::press(&mut s.transport, fe.abs.x, fe.time, width));
                }
            }
            Hit::FingerMove(fe) => {
                if self.state.as_ref().is_some_and(|s| s.scrub.is_some()) {
                    let t = self.time_at(fe.abs.x);
                    self.seek(t);
                } else if let Some(s) = &mut self.state {
                    if let Some(g) = &mut s.scratch {
                        g.moved(&mut s.transport, fe.abs.x, fe.time);
                    }
                }
            }
            Hit::FingerUp(fe) => {
                if let Some(s) = &mut self.state {
                    if s.scrub.take().is_some() {
                        s.transport.release(true, None);
                    }
                    if let Some(g) = s.scratch.take() {
                        g.release(&mut s.transport, fe.time);
                    }
                    if s.playing != s.transport.playing() {
                        log!("player: {} at {:.2} s", if s.transport.playing() { "play" } else { "pause" }, s.t);
                    }
                    s.playing = s.transport.playing();
                }
            }
            Hit::FingerHoverOver(_) | Hit::FingerHoverIn(_) => {
                if let Some(s) = &mut self.state {
                    s.controls_until = Cx::monotonic_now() + 2.5;
                }
            }
            Hit::KeyDown(ke) => {
                let t = self.state.as_ref().map_or(0.0, |s| s.t);
                match ke.key_code {
                    KeyCode::Space => {
                        let playing = self.state.as_ref().is_some_and(|s| s.playing);
                        self.set_playing(!playing);
                    }
                    KeyCode::ArrowLeft => self.seek(t - 5.0),
                    KeyCode::ArrowRight => self.seek(t + 5.0),
                    KeyCode::Home => self.seek(0.0),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let started = Cx::monotonic_now();
        let step = self.draw_frame(cx, walk);
        if let Some(s) = &mut self.state {
            s.ema_cpu += ((Cx::monotonic_now() - started) - s.ema_cpu) * 0.1;
        }
        step
    }
}

impl FilmPlayer {
    fn draw_frame(&mut self, cx: &mut Cx2d, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.color = vec4(0.0, 0.0, 0.0, 1.0);
        self.draw_bg.draw_abs(cx, rect);
        let dpi = cx.current_dpi_factor();
        let Some(s) = &mut self.state else { return DrawStep::done() };
        s.film.stream(cx, s.t);
        let (fw, fh) = s.film.manifest.as_ref().map_or((1920.0, 1080.0), |m| (m.film.width, m.film.height));
        // Letterboxed: the whole frame in the window.
        let k = (rect.size.x / fw).min(rect.size.y / fh);
        let size = dvec2(fw * k, fh * k);
        s.film_rect = Rect { pos: rect.pos + (rect.size - size) * 0.5, size };
        let scale = SCALES[s.scale];
        // Never more pixels than the film's own.
        let px_w = (size.x * dpi * scale).min(fw).round().max(2.0) as u32;
        let px_h = ((px_w as f64) * fh / fw).round().max(2.0) as u32;
        #[cfg(not(target_arch = "wasm32"))]
        let (px_w, px_h) = if s.analyser.is_some() {
            (320, 180)
        } else if let Some(r) = &s.gpu_report {
            r.size.unwrap_or((fw as u32, fh as u32))
        } else {
            (px_w, px_h)
        };
        let mut exact = false;
        let mut drawn = false;
        if s.film.manifest.is_some() {
            // While playing, the other plates prepare in what is left of
            // the frame's interval.
            let interval = s.interval.min(s.interval_now);
            let budget = (s.playing && s.loaded && interval.is_finite()).then_some(crate::film::FrameBudget { start: s.last_tick, interval });
            if let Some(shown) = s.film.draw(cx, s.t, (px_w, px_h), budget) {
                drawn = true;
                exact = shown.exact;
                s.shown = Some(shown.texture);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(a) = &mut s.analyser {
            let painted = s.film.painted(cx, s.t);
            a.frame_done(cx, &s.film, crate::analyse::Drawn { drawn, exact, painted });
            if a.finished {
                cx.quit();
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = &mut s.gpu_report {
            for (slot, plate) in s.film.made_textures.as_mut().map(std::mem::take).unwrap_or_default() {
                r.owners.insert(slot, plate);
            }
            let painted = s.film.painted(cx, s.t);
            r.frame_done(cx, &s.film, drawn, exact, painted);
            if r.finished {
                cx.quit();
            }
        }
        let _ = (exact, drawn);
        if let Some(texture) = &s.shown {
            self.draw_film.draw_vars.set_texture(0, texture);
            let film_rect = s.film_rect;
            self.draw_film.draw_abs(cx, film_rect);
        }
        // A plate that failed or stalls says so over the film.
        if let Some(problem) = s.film.problem_at(s.t) {
            let line = problem.lines().next().unwrap_or("").to_string();
            let at = dvec2(s.film_rect.pos.x + 24.0, s.film_rect.pos.y + 24.0);
            self.draw_note.font_scale = 1.0;
            self.draw_note.draw_abs(cx, at, &line);
        }
        let loaded = s.loaded;
        if loaded {
            self.draw_controls(cx, rect);
        } else {
            self.draw_loader(cx, rect);
        }
        DrawStep::done()
    }
}
