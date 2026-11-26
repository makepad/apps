//! A viewer's player, over a cut that plays (not deterministic): the old
//! film player's controls (`story/apps/player/src/player.rs`) on the cut
//! engine's picture.
//!
//! - The control strip (56 px): a play/pause disc, `m:ss / m:ss`, the
//!   timeline with the plates' starts as ticks, the played part and the
//!   playhead. It shows for 3 s after a press, 2.5 s after a hover, and
//!   always while paused or a hand is on the film.
//! - The play disc plays or pauses; a press on the timeline stops the film
//!   dead under the finger and drags it ([`Transport::press`] without brake);
//!   a tap on the picture (under 6 px) plays or pauses; a press and drag on
//!   the picture is a hand on the record ([`crate::scratch`]): it brakes,
//!   scratches, and flings back into play either way on release.
//! - Space plays or pauses, Left/Right step 5 s, Home goes to the start.
//! - The clock is the sound's ([`Transport`]): played to the end it pauses
//!   there, play at the end starts over, flung backwards it pauses at 0.
//! - A loader until the cut can start (its load, its sound, its first
//!   picture). It comes back over the film while play has reached a time
//!   the engine cannot draw yet (a document still loading), and play
//!   resumes once it can. In the browser nothing plays before the first
//!   gesture (it unlocks the audio); natively play starts at once.
//! - The picture is letterboxed and recorded at the window's pixels times a
//!   resolution scale that steps down while frames run long (GPU-bound) and
//!   back up while they are short.

use crate::scratch::Scratch;
use crate::transport::Transport;
use crate::sound::ClipBuilder;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*

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

/// What the viewer draws with (the player view's own draw fields).
pub struct Draws<'a> {
    pub ui: &'a mut DrawPlayerUi,
    pub text: &'a mut DrawText,
    pub title: &'a mut DrawText,
    pub note: &'a mut DrawText,
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
/// How long the engine may wait for what a frame needs while playing
/// before the loader comes back over the film.
const STALL_SECONDS: f64 = 0.25;
/// Seconds of sound past the film's time a clip must hold to be played.
const PLAYABLE_SECONDS: f64 = 4.0;

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

/// What the loader says: how far the start has come (0..1), the bytes
/// fetched of those known, and what is waited for.
pub struct Progress {
    pub done: f64,
    pub got: u64,
    pub total: u64,
    pub waiting: String,
    pub error: Option<String>,
}

pub struct Viewer {
    pub transport: Transport,
    /// The cut's sound becoming a clip (once the cut has loaded), and when
    /// it started.
    pub builder: Option<ClipBuilder>,
    built_from: f64,
    /// The cut's sound is on the deck (or it has none).
    pub sound_ready: bool,
    /// The film's time, and whether it plays (or means to).
    pub t: f64,
    pub playing: bool,
    /// The timeline's end (seconds).
    duration: f64,
    /// The plates' starts: the timeline's ticks.
    starts: Vec<f64>,
    title: String,
    /// Start playing as soon as the film can (natively; the browser waits
    /// for the first gesture).
    autoplay: bool,
    /// Playing when the loader came back: play resumes once it goes.
    resume: bool,
    /// The film has shown its first picture: the loader is gone.
    pub loaded: bool,
    /// Since when the engine has been waiting for what the frame needs.
    stalled_since: Option<f64>,
    /// Dragging the timeline.
    scrub: bool,
    /// A finger on the picture.
    scratch: Option<Scratch>,
    /// The controls show until this wall time.
    controls_until: f64,
    /// The clock's last tick (wall seconds): this frame's start.
    last_tick: f64,
    /// Smoothed frame interval and CPU seconds of the draw, GPU seconds
    /// per frame (where reported), and how long frames were slow/fast.
    ema_dt: f64,
    pub ema_cpu: f64,
    ema_gpu: f64,
    slow_for: f64,
    fast_for: f64,
    scale: usize,
    started: f64,
    stats_at: f64,
    stats_frames: u32,
    pub film_rect: Rect,
    bar_rect: Rect,
    play_rect: Rect,
}

impl Viewer {
    pub fn new(from: f64, duration: f64, starts: Vec<f64>, title: String) -> Self {
        let now = Cx::monotonic_now();
        let mut transport = Transport::default();
        transport.seek(from);
        Self {
            transport,
            builder: None,
            built_from: 0.0,
            sound_ready: false,
            t: from,
            playing: false,
            duration,
            starts,
            title,
            autoplay: !cfg!(target_arch = "wasm32"),
            resume: false,
            loaded: false,
            stalled_since: None,
            scrub: false,
            scratch: None,
            controls_until: now + 3.0,
            last_tick: now,
            ema_dt: 1.0 / 60.0,
            ema_cpu: 0.0,
            ema_gpu: 0.0,
            slow_for: 0.0,
            fast_for: 0.0,
            scale: 0,
            started: now,
            stats_at: now,
            stats_frames: 0,
            film_rect: Rect::default(),
            bar_rect: Rect::default(),
            play_rect: Rect::default(),
        }
    }

    /// The cut has loaded: its sound starts becoming a clip (none: the
    /// deck plays a silent one).
    pub fn start_sound(&mut self, cx: &mut Cx, builder: Option<ClipBuilder>) {
        match builder {
            Some(builder) => {
                self.builder = Some(builder);
                self.built_from = Cx::monotonic_now();
            }
            None => {
                self.transport.attach(cx, None, 0.0, self.duration);
                self.sound_ready = true;
            }
        }
    }

    /// The clip as it is built: playable once its first seconds are (or the
    /// timeline's), longer while play nears where it ends, whole at the end.
    fn build_sound(&mut self, cx: &mut Cx) {
        let Some(builder) = &mut self.builder else { return };
        if let Err(error) = builder.pump(cx) {
            log!("makepad-story-doc-player: no sound: {error}");
            self.builder = None;
            if !self.transport.attached() {
                self.transport.attach(cx, None, 0.0, self.duration);
            }
            self.sound_ready = true;
            return;
        }
        let (built, complete) = (builder.built(), builder.complete());
        let since = Cx::monotonic_now() - self.built_from;
        if !self.transport.attached() {
            if complete || built >= self.t + PLAYABLE_SECONDS {
                let pcm = if complete { self.builder.take().map(ClipBuilder::finish) } else { Some(builder.snapshot()) };
                self.transport.attach(cx, pcm, built, self.duration);
                self.sound_ready = true;
                log!("makepad-story-doc-player: sound playable {since:.2} s after the load ({built:.1} s of it mixed)");
                if complete {
                    log!("makepad-story-doc-player: sound whole {since:.2} s after the load");
                }
            }
            return;
        }
        if !self.transport.can_extend() {
            return;
        }
        if complete {
            if let Some(builder) = self.builder.take() {
                self.transport.extend(builder.finish(), built);
                log!("makepad-story-doc-player: sound whole {since:.2} s after the load");
            }
        } else if self.t > self.transport.built() - 1.0 && built >= self.t + PLAYABLE_SECONDS {
            // Play (or a seek, a scratch) nears the end of what the deck
            // holds: the mixed part so far takes over.
            self.transport.extend(builder.snapshot(), built);
        }
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.transport.handle_event(cx, event);
    }

    fn set_playing(&mut self, playing: bool) {
        if playing {
            self.transport.play();
        } else {
            self.transport.pause();
        }
        self.playing = playing;
        self.last_tick = Cx::monotonic_now();
    }

    fn seek(&mut self, t: f64) {
        self.t = t.clamp(0.0, self.duration.max(0.0));
        self.transport.seek(self.t);
    }

    /// The timeline's time under x.
    fn time_at(&self, x: f64) -> f64 {
        let r = self.bar_rect;
        ((x - r.pos.x) / r.size.x.max(1.0)).clamp(0.0, 1.0) * self.duration
    }

    /// The timeline's end (seconds; the last frame's end, at or past the
    /// cut's duration).
    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// Play has reached the timeline's end (and paused there).
    pub fn at_end(&self) -> bool {
        self.duration > 0.0 && self.t >= self.duration - 1e-3
    }

    /// The resolution scale of the picture (of the window's pixels).
    pub fn scale(&self) -> f64 {
        SCALES[self.scale]
    }

    /// Once per display frame: the clock, the resolution scale, and the
    /// loader's coming and going. `waiting`: what the engine's last record
    /// waited for (empty: it drew); `showing`: the picture shown is the
    /// frame of the film's time; `ready`: the cut can start (loaded, its
    /// first picture there).
    pub fn tick(&mut self, cx: &mut Cx, waiting: &str, showing: bool, ready: bool) {
        let now = Cx::monotonic_now();
        let dt = (now - self.last_tick).clamp(0.0, 0.25);
        self.last_tick = now;
        self.build_sound(cx);
        // The clock: what the deck plays now.
        self.t = self.transport.tick(dt);
        self.playing = self.transport.playing();
        // A page opened with ?test=1 hands its harness the picture's time
        // and the deck's.
        #[cfg(target_arch = "wasm32")]
        {
            let (rendered, speed) = self.transport.test_clock();
            cx.web_test_clock(self.t, rendered, speed);
        }
        self.scale_step(dt, now);
        // Play reached (or a seek landed on) a time the engine cannot draw
        // yet: the loader comes back over the film until it can, and play
        // resumes. Not while a hand holds the timeline or the record.
        // ... or a time past what the deck's clip holds yet.
        let sound_short = self.transport.attached() && self.transport.built() < self.duration - 1e-3 && self.t > self.transport.built() - 0.05;
        let stalled = (!waiting.is_empty() && !showing) || sound_short;
        if stalled {
            self.stalled_since.get_or_insert(now);
        } else {
            self.stalled_since = None;
        }
        if self.loaded && !self.scrub && self.scratch.is_none() && self.stalled_since.is_some_and(|since| now - since >= STALL_SECONDS) {
            self.loaded = false;
            self.resume = self.playing;
            log!("player: the film at {:.2} s waits for {waiting}: the loader shows until it can draw", self.t);
            self.set_playing(false);
        }
        // Start once the sound is on the deck and the picture of the
        // film's time is there (autoplay), or leave the loader for a
        // gesture.
        if !self.loaded && ready && self.sound_ready && showing && waiting.is_empty() && !sound_short {
            self.loaded = true;
            self.controls_until = now + 3.0;
            log!("player: ready to play at {:.2} s ({:.1} s after start)", self.t, now - self.started);
            if self.autoplay || std::mem::take(&mut self.resume) {
                self.autoplay = false;
                self.set_playing(true);
            }
        }
    }

    /// The resolution scale follows the GPU only. A frame that runs long
    /// (under ~48 fps for a second) steps it down when the GPU is the long
    /// pole: its measured frame time where the platform reports one, else by
    /// elimination (our own CPU work well under the frame). Short frames
    /// (60 fps for three seconds) step it back up when the measured GPU
    /// time, grown by the pixels the step adds, still fits.
    fn scale_step(&mut self, dt: f64, now: f64) {
        if !self.playing {
            self.stats_at = now;
            self.stats_frames = 0;
            return;
        }
        self.ema_dt += (dt - self.ema_dt) * 0.1;
        let gpu = gpu_frame_ms().map(|ms| ms / 1000.0);
        if let Some(g) = gpu {
            self.ema_gpu += (g - self.ema_gpu) * 0.2;
        }
        let cpu_bound = self.ema_cpu > self.ema_dt * 0.7;
        let gpu_bound = match gpu {
            Some(_) => self.ema_gpu > 1.0 / 60.0,
            None => !cpu_bound,
        };
        if self.ema_dt > 1.0 / 48.0 && gpu_bound {
            self.slow_for += dt;
            self.fast_for = 0.0;
        } else if self.ema_dt < 1.0 / 57.0 {
            self.fast_for += dt;
            self.slow_for = 0.0;
        } else {
            self.slow_for = 0.0;
            self.fast_for = 0.0;
        }
        if self.slow_for > 1.0 && self.scale + 1 < SCALES.len() {
            self.scale += 1;
            self.slow_for = 0.0;
            self.ema_dt = 1.0 / 60.0;
        }
        let fits_up = self.scale > 0
            && match gpu {
                Some(_) => self.ema_gpu * (SCALES[self.scale - 1] / SCALES[self.scale]).powi(2) < 1.0 / 60.0,
                None => true,
            };
        if self.fast_for > 3.0 && fits_up {
            self.scale -= 1;
            self.fast_for = 0.0;
        }
        self.stats_frames += 1;
        if now - self.stats_at >= 2.0 {
            let fps = self.stats_frames as f64 / (now - self.stats_at);
            log!("player stats: t={:.2} fps={:.1} scale={} cpu={:.1}ms", self.t, fps, SCALES[self.scale], self.ema_cpu * 1000.0);
            self.stats_at = now;
            self.stats_frames = 0;
        }
    }

    /// A finger, a hover or a key on the player.
    pub fn handle_hit(&mut self, cx: &mut Cx, hit: Hit, area: Area) {
        match hit {
            Hit::FingerDown(fe) => {
                cx.set_key_focus(area);
                self.controls_until = Cx::monotonic_now() + 3.0;
                if !self.loaded {
                    return;
                }
                let grow = |r: Rect, by: f64| Rect { pos: r.pos - dvec2(by, by), size: r.size + dvec2(by, by) * 2.0 };
                if grow(self.bar_rect, 14.0).contains(fe.abs) {
                    // The timeline: the film stops dead and jumps with the finger.
                    self.transport.press(false);
                    self.scrub = true;
                    let t = self.time_at(fe.abs.x);
                    self.seek(t);
                } else if grow(self.play_rect, 8.0).contains(fe.abs) {
                    let playing = self.playing;
                    self.set_playing(!playing);
                } else {
                    // The picture: a record under the finger (scratch.rs).
                    let width = self.film_rect.size.x;
                    self.scratch = Some(Scratch::press(&mut self.transport, fe.abs.x, fe.time, width));
                }
            }
            Hit::FingerMove(fe) => {
                if self.scrub {
                    let t = self.time_at(fe.abs.x);
                    self.seek(t);
                } else if let Some(scratch) = &mut self.scratch {
                    scratch.moved(&mut self.transport, fe.abs.x, fe.time);
                }
            }
            Hit::FingerUp(fe) => {
                if std::mem::take(&mut self.scrub) {
                    self.transport.release(true, None);
                }
                if let Some(scratch) = self.scratch.take() {
                    scratch.release(&mut self.transport, fe.time);
                }
                if self.playing != self.transport.playing() {
                    log!("player: {} at {:.2} s", if self.transport.playing() { "play" } else { "pause" }, self.t);
                }
                self.playing = self.transport.playing();
            }
            Hit::FingerHoverOver(_) | Hit::FingerHoverIn(_) => {
                self.controls_until = Cx::monotonic_now() + 2.5;
            }
            Hit::KeyDown(ke) => {
                let t = self.t;
                match ke.key_code {
                    KeyCode::Space => {
                        let playing = self.playing;
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

    /// Over the picture: the loader until the film can play, else the controls.
    pub fn draw(&mut self, cx: &mut Cx2d, d: Draws, rect: Rect, progress: Progress) {
        if self.loaded {
            self.draw_controls(cx, d, rect);
        } else {
            draw_loader(cx, d, rect, &self.title, self.started, progress);
        }
    }

    fn draw_controls(&mut self, cx: &mut Cx2d, d: Draws, rect: Rect) {
        let now = Cx::monotonic_now();
        let show = !self.playing || self.scrub || self.scratch.is_some() || now < self.controls_until;
        let h = 56.0;
        let strip = Rect { pos: dvec2(rect.pos.x, rect.pos.y + rect.size.y - h), size: dvec2(rect.size.x, h) };
        let pad = 16.0;
        self.play_rect = Rect { pos: dvec2(strip.pos.x + pad, strip.pos.y + 10.0), size: dvec2(36.0, 36.0) };
        let time = format!("{} / {}", clock_text(self.t), clock_text(self.duration));
        let text_x = self.play_rect.pos.x + 36.0 + 12.0;
        let text_w = 96.0;
        self.bar_rect = Rect {
            pos: dvec2(text_x + text_w + 8.0, strip.pos.y + 20.0),
            size: dvec2((strip.size.x - (text_x + text_w + 8.0 - strip.pos.x) - pad).max(40.0), 16.0),
        };
        if !show {
            return;
        }
        let (bar, play, duration) = (self.bar_rect, self.play_rect, self.duration.max(1e-6));
        d.ui.shape(cx, 0.0, strip, alpha(INK, 0.62), 0.0);
        d.ui.shape(cx, 1.0, play, BONE, 0.0);
        d.ui.shape(cx, if self.playing { 3.0 } else { 2.0 }, play, INK, 0.0);
        d.text.draw_abs(cx, dvec2(text_x, strip.pos.y + 20.0), &time);
        // The timeline: a hairline, the plates' starts as ticks, the played
        // part in bone, the playhead in signal orange.
        let mid = bar.pos.y + bar.size.y * 0.5;
        d.ui.shape(cx, 0.0, Rect { pos: dvec2(bar.pos.x, mid - 1.0), size: dvec2(bar.size.x, 2.0) }, alpha(BONE, 0.3), 1.0);
        for start in &self.starts {
            let x = bar.pos.x + bar.size.x * start / duration;
            d.ui.shape(cx, 0.0, Rect { pos: dvec2(x - 0.5, mid - 5.0), size: dvec2(1.0, 10.0) }, alpha(BONE, 0.35), 0.0);
        }
        let x = bar.pos.x + bar.size.x * (self.t / duration).clamp(0.0, 1.0);
        d.ui.shape(cx, 0.0, Rect { pos: dvec2(bar.pos.x, mid - 1.5), size: dvec2(x - bar.pos.x, 3.0) }, BONE, 1.5);
        d.ui.shape(cx, 1.0, Rect { pos: dvec2(x - 7.0, mid - 7.0), size: dvec2(14.0, 14.0) }, SIGNAL, 0.0);
    }
}

/// The loader: the title, an engraved ring drawn to how far the start has
/// come, and what it waits for (the bytes fetched while the page fetches).
pub fn draw_loader(cx: &mut Cx2d, d: Draws, rect: Rect, title: &str, since: f64, p: Progress) {
    let now = Cx::monotonic_now();
    d.ui.shape(cx, 0.0, rect, BONE, 0.0);
    let short = rect.size.x.min(rect.size.y);
    let ring = (short * 0.28).clamp(80.0, 180.0);
    let center = dvec2(rect.pos.x + rect.size.x * 0.5, rect.pos.y + rect.size.y * 0.42);
    d.ui.progress = p.done.clamp(0.0, 1.0) as f32;
    d.ui.shape(cx, 4.0, Rect { pos: center - dvec2(ring, ring) * 0.5, size: dvec2(ring, ring) }, INK, ((now - since) * 2.0) as f32);
    // A signal dot rides the arc's end.
    let a = p.done.clamp(0.0, 1.0) * std::f64::consts::TAU;
    let dot = center + dvec2(a.sin(), -a.cos()) * (ring * 0.5 - 4.0);
    d.ui.shape(cx, 1.0, Rect { pos: dot - dvec2(4.0, 4.0), size: dvec2(8.0, 8.0) }, SIGNAL, 0.0);
    // The title and the note fit the screen's width (a phone held
    // upright is narrower than a long title): they shrink, never overflow.
    let avail = (rect.size.x - 32.0).max(32.0);
    let fit = |w: f64| (avail / w.max(1.0)).min(1.0).max(0.3);
    let tw = d.title.layout(cx, 0.0, 0.0, None, false, Align::default(), title).size_in_lpxs.width as f64;
    let k = fit(tw);
    d.title.font_scale = k as f32;
    d.title.draw_abs(cx, dvec2(center.x - tw * k * 0.5, center.y + ring * 0.5 + 24.0), title);
    let what = if let Some(error) = &p.error {
        error.lines().next().unwrap_or("").to_string()
    } else if p.got < p.total {
        format!("{} of {}  ·  {}", mib(p.got), mib(p.total), p.waiting)
    } else {
        p.waiting
    };
    let nw = d.note.layout(cx, 0.0, 0.0, None, false, Align::default(), &what).size_in_lpxs.width as f64;
    let k = fit(nw);
    d.note.font_scale = k as f32;
    d.note.draw_abs(cx, dvec2(center.x - nw * k * 0.5, center.y + ring * 0.5 + 72.0), &what);
}
