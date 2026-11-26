//! `makepad-story-doc-player`: one Stage document played full-window with its sound, by
//! Stage's own engines and nothing of its editor.
//!
//! ```text
//! makepad-story-doc-player <document> [--tree <dir>] [--fill <logical path>=<file>]...
//!     [--deterministic] [--fps <rate>] [--from <s>] [--to <s>] [--slide <n>] [--dwell <s>] [--end exit|hold|loop]
//!     [--capture <dir> [--at <s|@n>,...] [--every <k>]] [--hashes <file>] [--audio-out <file.wav>]
//!     [--seek <s>,<s>,... [--seek-play <s>]] [--shutter-max <n>]
//! ```
//!
//! The document is a cut (`*.cut.splash`, a film), a deck
//! (`deck.splash`, `*.deck.splash`) or a Motion document (`*.splash`),
//! opened in place in its folder's tree as Stage opens it; `--fill` gives an
//! optional entry the folder does not carry (a film's song:
//! `--fill media/song.mp3=<song.mp3>`).
//!
//! Playing (the default) a cut is a viewer's player ([`viewer`]): its clock
//! is the sound's (the cut's whole mix as a clip on a deck, [`transport`]),
//! with the old film player's controls: the play disc, the timeline, a tap
//! or a drag on the picture (a hand on the record: brake, scratch, fling),
//! Space / Left / Right / Home; played to the end it pauses (`--end`). In the
//! browser nothing plays before the first gesture. A deck plays on the wall
//! clock.
//!
//! # Deterministic mode: the tracer's entry point
//!
//! `--deterministic` steps the clock along the timeline, one frame of the
//! document's frame rate at a time (a deck: `--fps`, 60), never the wall
//! clock. Each frame is drawn exactly as Stage's render draws it (locked
//! time: every graphic's shutter sampled, every clip frame, document,
//! image, font and glyph it shows loaded before it is taken), its share of
//! the export's mixdown is mixed, and only then does the clock step. A deck
//! takes each stop `--dwell` seconds after it is reached. At the end
//! (`--end exit`, the default here) it prints one line and exits:
//!
//! ```text
//! makepad-story-doc-player: done <frames> frames <w>x<h> at <num>/<den> fps[, frames hash <h>][, sound hash <h> (<n> samples)]
//! ```
//!
//! Exit status: 0 played to the end, 1 the document failed (it did not
//! load, a frame cannot paint), 2 usage. `--hashes` writes `<frame> <hash>`
//! per frame (the RGBA hash `makepad-edit-render --hashes` writes) and the
//! frames hash over them; `--capture` writes the frames `--at` / `--every`
//! asks for as PNG; `--audio-out` the sound as WAV. In the browser the
//! arguments are the page's query (`?doc=<path>&tree=<dir>&deterministic&end=exit`,
//! paths relative to the page), the files are read from the page
//! (`web_source`), the summary and an `makepad-story-doc-player: exit <status>` line
//! go to the console and the player stops asking for frames.
//!
//! `--seek` (deterministic): once the span has played, the clock jumps to each
//! time in turn and plays `--seek-play` seconds (2) from it, as a viewer's
//! seeks would; a trace's replay runs another seek order than its first run.
//! A time ending in `%` is that share of the timeline (`--seek 75%,10%,50%`).
//!
//! `--shutter-max <n>` caps motion blur's adaptive sub-frames (324 at most
//! by default) for a coverage run: the schedule's levels and measurements
//! all still run, in far fewer paints; its pictures are not the film's.
//!
//! Run hidden and muted for tests: `MAKEPAD_HIDE_WINDOWS=1 MAKEPAD_MUTE=1`
//! (`MAKEPAD_PAINT_INTERVAL=0.001` paints back to back).

mod deck;
mod document;
mod options;
#[cfg(target_arch = "wasm32")]
mod web_source;

#[cfg(target_arch = "wasm32")]
use crate::web_source::Fetching;
/// Natively the files are on disk: nothing is fetched.
#[cfg(not(target_arch = "wasm32"))]
type Fetching = ();
mod picture;
mod scratch;
mod sound;
mod transport;
mod viewer;

use crate::deck::DeckRun;
use crate::document::Document;
use crate::options::{At, End, Options, Seek};
use crate::picture::Picture;
use crate::viewer::{Draws, DrawPlayerUi, Progress, Viewer};
use makepad_story_edit_render::render::{fnv1a, opaque_rgba};
use makepad_story_edit_render::CutJob;
use makepad_widgets::*;
use std::collections::HashMap;

app_main!(App, font_assets: [INTER_FONT_ASSET, ROBOTO_FLEX_FONT_ASSET]);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // The document's frame (the engine's output target).
    set_type_default() do #(DrawFrame::script_shader(vm)){
        ..mod.draw.DrawQuad
        tex: texture_2d(float)
        pixel: fn() {
            let c = self.tex.sample(self.pos)
            return vec4(c.xyz, 1.0)
        }
    }

    mod.widgets.PlayerViewBase = #(PlayerView::register_widget(vm))
    mod.widgets.PlayerView = set_type_default() do mod.widgets.PlayerViewBase{
        width: Fill
        height: Fill
        draw_text +: {
            color: #xeee9df
            text_style: theme.font_regular{font_size: 11.0}
        }
        draw_title +: {
            color: #x0a0a0b
            text_style: theme.font_bold{font_size: 30.0}
        }
        draw_note +: {
            color: #x0a0a0b
            text_style: theme.font_regular{font_size: 10.0}
        }
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Stage player"
                window.inner_size: vec2(1280, 720)
                pass.clear_color: #x000000
                body +: {
                    view := mod.widgets.PlayerView{}
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawFrame {
    #[deref]
    pub draw_super: DrawQuad,
}

/// The program: one cut engine (a cut, a Motion document) or a deck.
enum Program {
    Cut(Picture),
    Deck(DeckRun),
}

/// How time moves.
enum Clock {
    /// Frame `n` of the timeline, next (frames from `--to` on are not played).
    Steps { n: u64 },
    /// The wall clock since `started`, from timeline time `from` (a deck).
    Wall { started: f64, from: f64 },
    /// A cut playing: the viewer's clock ([`Viewer`], the sound's).
    Viewer,
}

struct Run {
    options: Options,
    program: Program,
    clock: Clock,
    /// The first frame counted (hashed, captured, mixed): `--from`. A deck
    /// plays its clock from 0 (its slide changes come from its own past).
    first: u64,
    /// The seeks done (`--seek`), and the end of the one playing.
    seeked: usize,
    seek_end: Option<u64>,
    /// The output size and frame rate.
    size: (u32, u32),
    fps: (u32, u32),
    shown: Option<Texture>,
    /// A frame read back: its index and ticket.
    reading: Option<(u64, ReadbackTicket)>,
    readback_wait_logged: bool,
    hashes: Vec<(u64, u64)>,
    captured: usize,
    drawn: u64,
    /// The sound mixed per frame (deterministic).
    mixed: Option<sound::Mixed>,
    /// A cut playing: the viewer's player (its clock, sound and controls).
    viewer: Option<Viewer>,
    /// The cut sounds (deterministic: its mixdown is made).
    sounds: bool,
    started: f64,
    reported: f64,
    ended: bool,
}

// Natively nothing is fetched (`fetching` is the browser's).
#[derive(Script, ScriptHook, Widget)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub struct PlayerView {
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
    draw_frame: DrawFrame,
    /// The viewer's controls and loader.
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
    run: Option<Run>,
    /// What to play, until its document is open (the browser fetches its
    /// files first).
    #[rust]
    pending: Option<Options>,
    /// The browser's fetches of the files an open wants.
    #[rust]
    fetching: Fetching,
    /// The player has finished (the browser: no more frames are asked for).
    #[rust]
    finished: bool,
    /// When the player started (the loader's clock).
    #[rust]
    started: f64,
}

/// How a step of the player ends early: `Ok` played to the end (the summary
/// line), `Err` the document failed.
type Done = Result<String, String>;

/// A step of the player: on, or done ([`Done`]).
type Step = Result<(), Done>;

/// Print the outcome; natively leave with its status. The browser logs the
/// status and the player stops (no more frames, nothing more drawn).
fn finish(result: Done) {
    #[cfg(not(target_arch = "wasm32"))]
    match result {
        Ok(line) => {
            println!("{line}");
            std::process::exit(0)
        }
        Err(error) => {
            eprintln!("makepad-story-doc-player: FAILED: {error}");
            std::process::exit(1)
        }
    }
    // The page's console has no stdout: both lines are logged.
    #[cfg(target_arch = "wasm32")]
    match result {
        Ok(line) => {
            log!("{line}");
            log!("makepad-story-doc-player: exit 0");
        }
        Err(error) => {
            error!("makepad-story-doc-player: FAILED: {error}");
            log!("makepad-story-doc-player: exit 1");
        }
    }
}

impl PlayerView {
    /// The bytes the page has fetched of those it knows of (natively none).
    fn fetch_progress(&self) -> (u64, u64) {
        #[cfg(target_arch = "wasm32")]
        return self.fetching.progress();
        #[cfg(not(target_arch = "wasm32"))]
        (0, 0)
    }

    fn finish(&mut self, cx: &mut Cx, result: Done) {
        // A native run's Splash record (MAKEPAD_SPLASH_TRACE) before the process exits.
        cx.splash_trace_exit();
        self.finished = true;
        self.run = None;
        self.pending = None;
        finish(result);
    }

    /// Open the pending document and start it; in the browser, once every
    /// file its open reads has been fetched.
    fn try_open(&mut self, cx: &mut Cx) -> Step {
        let Some(options) = self.pending.take() else { return Ok(()) };
        let opened = document::open(cx, &options);
        #[cfg(target_arch = "wasm32")]
        if self.fetching.fetch(cx, makepad_asset_data::source::store::take_wanted()) {
            self.pending = Some(options);
            return Ok(());
        }
        let opened = opened.map_err(Err)?;
        self.start(cx, options, opened)
    }

    fn start(&mut self, cx: &mut Cx, options: Options, opened: document::Opened) -> Step {
        if let Some(n) = options.shutter_max {
            makepad_render_graph::mode::set_max_subframes(n);
        }
        let locked = options.deterministic;
        let now = Cx::monotonic_now();
        let (program, size, fps, sounds) = match opened.document {
            Document::Cut(doc) => {
                let size = (doc.output.width, doc.output.height);
                // A render decodes the sound before the first frame (the
                // graphics hear it); the preview's sound plays from the mix.
                let job = CutJob { doc, marks: HashMap::new(), resolver: Box::new(opened.media), audio: locked };
                let picture = Picture::new(cx, job, locked);
                let fps = picture.fps;
                (Program::Cut(picture), size, fps, true)
            }
            Document::Deck(doc) => {
                let fps = makepad_story_edit_render::audio::fps_ratio(options.fps);
                let size = doc.output;
                if options.slide >= doc.slides.len() {
                    return Err(Err(format!("--slide {}: the deck has {} slides", options.slide, doc.slides.len())));
                }
                let looping = options.end == End::Loop;
                // The deck's clock is timeline time from 0, on either clock.
                (Program::Deck(DeckRun::new(cx, doc, opened.media, locked, options.dwell, looping, options.slide, 0.0)), size, fps, false)
            }
        };
        let frame_at = |t: f64| (t * fps.0 as f64 / fps.1 as f64 + 1e-9).floor() as u64;
        let first = frame_at(options.from);
        let start = if matches!(program, Program::Deck(_)) { 0 } else { first };
        let clock = match (&program, locked) {
            (_, true) => Clock::Steps { n: start },
            (Program::Cut(_), false) => Clock::Viewer,
            (Program::Deck(_), false) => Clock::Wall { started: now, from: options.from },
        };
        if let Some(dir) = &options.capture {
            std::fs::create_dir_all(dir).map_err(|error| Err(format!("{}: {error}", dir.display())))?;
        }
        log!("makepad-story-doc-player: {} ({}x{} at {}/{} fps, {})", opened.path, size.0, size.1, fps.0, fps.1, if locked { "deterministic" } else { "playing" });
        let title = title_of(&options, &opened.path, &program);
        let mut run = Run {
            options, program, clock, first, seeked: 0, seek_end: None, size, fps, shown: None, reading: None, readback_wait_logged: false, hashes: Vec::new(), captured: 0, drawn: 0,
            mixed: None, viewer: None, sounds, started: now, reported: now, ended: false,
        };
        if let (Clock::Viewer, Program::Cut(picture)) = (&run.clock, &run.program) {
            // The timeline: from 0 to the span's end; the plates' starts as ticks.
            let end = run.end_frame().unwrap_or(picture.frames) as f64 * fps.1 as f64 / fps.0 as f64;
            let mut starts: Vec<f64> = picture.engine().doc().graphics.iter().filter_map(|graphic| graphic.span.map(|span| span.start)).filter(|start| *start > 0.0 && *start < end).collect();
            starts.sort_by(f64::total_cmp);
            starts.dedup();
            run.viewer = Some(Viewer::new(run.options.from, end, starts, title));
        }
        self.run = Some(run);
        self.next_frame = cx.new_next_frame();
        Ok(())
    }
}

/// The name the loader shows: `--title`, else the cut's label, else the
/// document's file name.
fn title_of(options: &Options, path: &str, program: &Program) -> String {
    if let Some(title) = &options.title {
        return title.clone();
    }
    if let Program::Cut(picture) = program {
        let label = &picture.engine().doc().label;
        if !label.is_empty() {
            return label.clone();
        }
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split('.').next().unwrap_or(name).to_string()
}

impl Run {
    fn deterministic(&self) -> bool {
        matches!(self.clock, Clock::Steps { .. })
    }

    /// Whether frame `n` is captured as PNG.
    fn captures(&self, n: u64) -> bool {
        if self.options.capture.is_none() || n < self.first {
            return false;
        }
        let fps = self.fps.0 as f64 / self.fps.1 as f64;
        self.options.every.is_some_and(|every| n % every == 0)
            || self.options.at.iter().any(|at| match at {
                At::Frame(f) => *f == n,
                At::Seconds(t) => (t * fps + 1e-9).floor() as u64 == n,
            })
    }

    /// The timeline's last frame (exclusive), when it is known.
    fn end_frame(&self) -> Option<u64> {
        let own = match &self.program {
            Program::Cut(picture) => Some(picture.frames),
            Program::Deck(_) => None,
        };
        let fps = self.fps.0 as f64 / self.fps.1 as f64;
        let to = self.options.to.map(|to| (to * fps + 1e-9).floor() as u64);
        let to = self.seek_end.or(to);
        match (to, own) {
            (Some(to), Some(own)) => Some(to.min(own)),
            (to, own) => to.or(own),
        }
    }

    fn summary(&self) -> String {
        let mut line = format!("makepad-story-doc-player: done {} frames {}x{} at {}/{} fps", self.drawn, self.size.0, self.size.1, self.fps.0, self.fps.1);
        if self.options.hashes.is_some() {
            let bytes: Vec<u8> = self.hashes.iter().flat_map(|(_, hash)| hash.to_le_bytes()).collect();
            line.push_str(&format!(", frames hash {:016x}", fnv1a(&bytes)));
        }
        if let Some(mixed) = &self.mixed {
            line.push_str(&format!(", sound hash {:016x} ({} samples)", mixed.hash, mixed.samples));
        }
        if self.captured > 0 {
            line.push_str(&format!(", {} captured", self.captured));
        }
        line.push_str(&format!(", in {:.1} s", Cx::monotonic_now() - self.started));
        line
    }

    /// Played to the end: exit, hold or start over.
    fn at_end(&mut self) -> Step {
        match self.options.end {
            End::Exit => {
                if let Some(path) = &self.options.hashes {
                    let text: String = self.hashes.iter().map(|(n, hash)| format!("{n} {hash:016x}\n")).collect();
                    std::fs::write(path, text).map_err(|error| Err(format!("{}: {error}", path.display())))?;
                }
                if let (Some(path), Some(mixed)) = (&self.options.audio_out, &self.mixed) {
                    mixed.write_wav(path).map_err(Err)?;
                }
                return Err(Ok(self.summary()));
            }
            // A viewer pauses at the end (play there starts over).
            End::Hold if self.viewer.is_some() => {}
            End::Hold => {
                if !self.ended {
                    log!("{}", self.summary());
                }
                self.ended = true;
            }
            End::Loop => match &mut self.clock {
                Clock::Steps { n } => *n = self.first,
                Clock::Wall { started, from } => {
                    *started = Cx::monotonic_now();
                    *from = 0.0;
                }
                Clock::Viewer => {
                    if let Some(viewer) = &mut self.viewer {
                        viewer.transport.seek(0.0);
                        viewer.transport.play();
                    }
                }
            },
        }
        Ok(())
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) -> Step {
        match &mut self.program {
            Program::Cut(picture) => picture.handle_event(cx, event),
            Program::Deck(deck) => deck.handle_event(cx, event),
        }
        if let Some((n, ticket)) = self.reading {
            if let Some(result) = cx.try_take_texture_readbacks_for(&[ticket]).into_iter().next() {
                self.reading = None;
                let data = result.data.map_err(|error| Err(format!("frame {n}: readback: {error}")))?;
                let mut rgba = Vec::new();
                opaque_rgba(&data, &mut rgba);
                if self.options.hashes.is_some() {
                    self.hashes.push((n, fnv1a(&rgba)));
                }
                if self.captures(n) {
                    if let Some(dir) = &self.options.capture {
                        let file = dir.join(format!("frame_{n:05}.png"));
                        write_png(&file, &rgba, result.width as u32, result.height as u32).map_err(Err)?;
                        self.captured += 1;
                    }
                }
                self.stepped(n)?;
            }
        }
        if let Some(viewer) = &mut self.viewer {
            viewer.handle_event(cx, event);
        }
        let now = Cx::monotonic_now();
        if now - self.reported > 10.0 {
            self.reported = now;
            let waiting = match &self.program {
                Program::Cut(picture) => picture.engine().waiting_for().to_string(),
                Program::Deck(deck) => format!("slide {}", deck.current()),
            };
            // `progress <n>/<total>`: what a tracer (relax export-web) reads.
            let total = self.end_frame().map(|end| end.saturating_sub(self.first).to_string()).unwrap_or_else(|| "?".into());
            log!("makepad-story-doc-player: {} frames so far; waiting for {waiting}; progress {}/{total}", self.drawn, self.drawn);
        }
        Ok(())
    }

    /// Frame `n` is done: the clock steps.
    fn stepped(&mut self, n: u64) -> Step {
        if n >= self.first {
            self.drawn += 1;
            if let Some(mixed) = &mut self.mixed {
                mixed.frame(n);
            }
        }
        if let Clock::Steps { n: next } = &mut self.clock {
            *next = n + 1;
        }
        if self.end_frame().is_some_and(|end| n + 1 >= end) {
            // the next seek, if any: the clock jumps there
            let fps = self.fps.0 as f64 / self.fps.1 as f64;
            let own = match &self.program {
                Program::Cut(picture) => Some(picture.frames),
                Program::Deck(_) => None,
            };
            let to = self.options.to.map(|to| (to * fps).floor() as u64);
            let range_end = match (own, to) { (Some(own), Some(to)) => Some(own.min(to)), (own, to) => to.or(own) };
            // a share of the timeline needs its length (a deck's is not known: skipped)
            let mut seek = None;
            while let Some(&target) = self.options.seeks.get(self.seeked) {
                seek = match target {
                    Seek::At(t) => Some(range_end.map_or(t.max(self.options.from), |end| t.clamp(self.first as f64 / fps, end.saturating_sub(1).max(self.first) as f64 / fps))),
                    Seek::Share(share) => range_end.map(|end| (self.first as f64 + share * end.saturating_sub(self.first + 1) as f64) / fps),
                };
                if seek.is_some() {
                    break;
                }
                self.seeked += 1;
            }
            if let (Some(t), Clock::Steps { n: next }) = (seek, &mut self.clock) {
                let start = (t * fps + 1e-9).floor() as u64;
                *next = start;
                self.seek_end = Some((start + (self.options.seek_play * fps).ceil().max(1.0) as u64).min(range_end.unwrap_or(u64::MAX)));
                self.seeked += 1;
                log!("makepad-story-doc-player: seek {} to {t} s (frame {start})", self.seeked);
                return Ok(());
            }
            self.at_end()?;
        }
        Ok(())
    }

    /// A display frame: the viewer's clock and loader.
    fn tick(&mut self, cx: &mut Cx) {
        let (Some(viewer), Program::Cut(picture)) = (&mut self.viewer, &self.program) else { return };
        let n = picture.frame_of(viewer.t);
        let showing = picture.shown().is_some_and(|(shown, _)| *shown == n);
        let ready = picture.ready().unwrap_or(false);
        viewer.tick(cx, picture.engine().waiting_for(), showing, ready);
    }

    /// How far a viewer's start has come, for its loader.
    fn progress(&self, got: u64, total: u64) -> Progress {
        let (Some(viewer), Program::Cut(picture)) = (&self.viewer, &self.program) else {
            return Progress { done: 1.0, got, total, waiting: String::new(), error: None };
        };
        let waiting = picture.engine().waiting_for();
        let (done, waiting) = match picture.ready() {
            Err(error) => return Progress { done: 0.0, got, total, waiting: String::new(), error: Some(error) },
            Ok(false) => (0.25, "loading the film".to_string()),
            Ok(true) if !viewer.sound_ready => (0.5, "the sound".to_string()),
            Ok(true) if waiting.is_empty() => (0.75, "the first picture".to_string()),
            Ok(true) => (0.75, waiting.to_string()),
        };
        Progress { done, got, total, waiting, error: None }
    }

    /// The sound, once the cut has loaded.
    fn prepare_sound(&mut self, cx: &mut Cx) -> Result<bool, Done> {
        let Program::Cut(picture) = &self.program else { return Ok(true) };
        if !self.sounds { return Ok(true); }
        // A failed loader is not a pending sound. Propagate its diagnostic
        // instead of spinning forever before the first frame can draw.
        if !picture.ready().map_err(Err)? { return Ok(false); }
        let engine = picture.engine();
        let doc = engine.doc().clone();
        let audible = !engine.eval().audio_plan(0.0, doc.duration).segments.is_empty();
        if self.deterministic() {
            if !engine.sound_ready() {
                return Ok(false);
            }
            if audible {
                let keep = self.options.audio_out.is_some();
                self.mixed = Some(sound::Mixed::new(engine.audio_mix(), self.fps, keep).map_err(|error| Err(format!("sound: {error}")))?);
            }
        } else if let Some(viewer) = &mut self.viewer {
            // The viewer's deck plays the cut's whole sound, built a slice
            // at a time (a silent clip for a cut without one).
            let builder = if audible {
                sound::ClipBuilder::new(cx, engine, doc.duration.max(viewer.duration())).unwrap_or_else(|error| {
                    log!("makepad-story-doc-player: no sound: {error}");
                    None
                })
            } else {
                None
            };
            viewer.start_sound(cx, builder);
        }
        self.sounds = false;
        Ok(true)
    }

    fn draw(&mut self, cx: &mut Cx2d) -> Step {
        if self.reading.is_some() || (self.ended && self.options.end == End::Hold) {
            return Ok(());
        }
        if !self.prepare_sound(cx)? {
            return Ok(());
        }
        let result = match &self.clock {
            Clock::Steps { n } => {
                let n = *n;
                let picture = match &mut self.program {
                    Program::Cut(picture) => picture.draw(cx, n, None),
                    Program::Deck(deck) => {
                        let now = n as f64 * self.fps.1 as f64 / self.fps.0 as f64;
                        let picture = deck.draw(cx, now);
                        if deck.finished {
                            return self.at_end();
                        }
                        picture
                    }
                };
                picture.map(|texture| texture.map(|texture| (n, texture)))
            }
            Clock::Viewer => {
                let (Some(viewer), Program::Cut(picture)) = (&mut self.viewer, &mut self.program) else { return Ok(()) };
                let (t, at_end) = (viewer.t, viewer.at_end());
                // `--end exit` / `loop` act once play has reached the end;
                // held, the viewer pauses there.
                if at_end && self.options.end != End::Hold {
                    return self.at_end();
                }
                let n = picture.frame_of(t);
                picture.draw(cx, n, None).map(|texture| texture.map(|texture| (u64::MAX, texture)))
            }
            Clock::Wall { started, from } => {
                let t = from + Cx::monotonic_now() - started;
                let n = (t * self.fps.0 as f64 / self.fps.1 as f64 + 1e-9).floor() as u64;
                if self.end_frame().is_some_and(|end| n >= end) {
                    return self.at_end();
                }
                let picture = match &mut self.program {
                    Program::Cut(picture) => picture.draw(cx, n, None),
                    Program::Deck(deck) => {
                        let picture = deck.draw(cx, t);
                        if deck.finished {
                            return self.at_end();
                        }
                        picture
                    }
                };
                picture.map(|texture| texture.map(|texture| (u64::MAX, texture)))
            }
        };
        let (n, texture) = match result {
            Ok(Some(shown)) => shown,
            Ok(None) => return Ok(()),
            Err(error) => return Err(Err(error)),
        };
        self.shown = Some(texture.clone());
        if n == u64::MAX {
            // Playing: the pictures shown, one per paint.
            self.drawn += 1;
            return Ok(());
        }
        if (self.options.hashes.is_some() && n >= self.first) || self.captures(n) {
            match texture.read_back(cx, ReadbackRequest::default()) {
                Ok(ticket) => {
                    self.readback_wait_logged = false;
                    self.reading = Some((n, ticket));
                }
                // The player's NextFrame loop redraws this same step.
                Err(ReadbackError::Backpressure) => {
                    if !self.readback_wait_logged {
                        log!("doc-player: context readback request limit reached; frame {n} waits");
                        self.readback_wait_logged = true;
                    }
                }
                Err(error) => return Err(Err(format!("frame {n}: readback: {error}"))),
            }
            return Ok(());
        }
        self.stepped(n)
    }
}

fn write_png(path: &std::path::Path, rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
    use makepad_widgets::makepad_zune_png::makepad_zune_core::bit_depth::BitDepth;
    use makepad_widgets::makepad_zune_png::makepad_zune_core::colorspace::ColorSpace;
    use makepad_widgets::makepad_zune_png::makepad_zune_core::options::EncoderOptions;
    use makepad_widgets::makepad_zune_png::PngEncoder;
    let options = EncoderOptions::default().set_width(width as usize).set_height(height as usize).set_depth(BitDepth::Eight).set_colorspace(ColorSpace::RGBA);
    let mut png = Vec::new();
    PngEncoder::new(rgba, options).encode(&mut png).map_err(|e| format!("png: {e:?}"))?;
    std::fs::write(path, png).map_err(|e| format!("{}: {e}", path.display()))
}

impl Widget for PlayerView {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if self.finished {
            return;
        }
        // The page's files: what came, then what any thread wants now (a
        // load worker waits for its file); the open again once all came.
        #[cfg(target_arch = "wasm32")]
        {
            let came = self.fetching.handle_event(cx, event);
            let fetching = self.fetching.fetch(cx, makepad_asset_data::source::store::take_wanted());
            if came && !fetching && self.pending.is_some() {
                if let Err(done) = self.try_open(cx) {
                    return self.finish(cx, done);
                }
            }
        }
        if let Some(run) = &mut self.run {
            if let Err(done) = run.handle_event(cx, event) {
                return self.finish(cx, done);
            }
        }
        // A viewer's fingers, hovers and keys.
        let area = self.draw_bg.area();
        if let Some(viewer) = self.run.as_mut().and_then(|run| run.viewer.as_mut()) {
            let hit = event.hits(cx, area);
            viewer.handle_hit(cx, hit, area);
        }
        if self.next_frame.is_event(event).is_some() {
            if let Some(run) = &mut self.run {
                run.tick(cx);
            }
            self.draw_bg.redraw(cx);
            self.next_frame = cx.new_next_frame();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.color = vec4(0.0, 0.0, 0.0, 1.0);
        self.draw_bg.draw_abs(cx, rect);
        let (got, total) = self.fetch_progress();
        let Some(run) = &mut self.run else {
            // A viewer's page fetching the document's files: the loader.
            if let Some(options) = self.pending.as_ref().filter(|options| !options.deterministic) {
                let name = options.document.file_name().and_then(|name| name.to_str()).unwrap_or("");
                let title = options.title.clone().unwrap_or_else(|| name.split('.').next().unwrap_or(name).to_string());
                let done = if total > 0 { 0.25 * got as f64 / total as f64 } else { 0.0 };
                let progress = Progress { done, got, total, waiting: "the film's files".into(), error: None };
                let draws = Draws { ui: &mut self.draw_ui, text: &mut self.draw_text, title: &mut self.draw_title, note: &mut self.draw_note };
                viewer::draw_loader(cx, draws, rect, &title, self.started, progress);
            }
            return DrawStep::done();
        };
        let started = Cx::monotonic_now();
        // Letterboxed: the whole frame in the window.
        let (fw, fh) = (run.size.0.max(1) as f64, run.size.1.max(1) as f64);
        let k = (rect.size.x / fw).min(rect.size.y / fh);
        let size = dvec2(fw * k, fh * k);
        let film_rect = Rect { pos: rect.pos + (rect.size - size) * 0.5, size };
        if let (Some(viewer), Program::Cut(picture)) = (&mut run.viewer, &mut run.program) {
            // Recorded at the window's pixels times the resolution scale,
            // never more than the cut's own.
            viewer.film_rect = film_rect;
            let px_w = (size.x * cx.current_dpi_factor() * viewer.scale()).min(fw).round().max(2.0) as u32;
            let px_h = (px_w as f64 * fh / fw).round().max(2.0) as u32;
            picture.set_raster(Some((px_w, px_h)));
        }
        if let Err(done) = run.draw(cx) {
            self.finish(cx, done);
            return DrawStep::done();
        }
        if let Some(texture) = &run.shown {
            self.draw_frame.draw_vars.set_texture(0, texture);
            self.draw_frame.draw_abs(cx, film_rect);
        }
        let progress = run.progress(got, total);
        if let Some(viewer) = &mut run.viewer {
            let draws = Draws { ui: &mut self.draw_ui, text: &mut self.draw_text, title: &mut self.draw_title, note: &mut self.draw_note };
            viewer.draw(cx, draws, rect, progress);
            viewer.ema_cpu += ((Cx::monotonic_now() - started) - viewer.ema_cpu) * 0.1;
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
        #[cfg(not(target_arch = "wasm32"))]
        let args: Vec<String> = std::env::args().skip(1).collect();
        #[cfg(target_arch = "wasm32")]
        let args = match cx.os_type() {
            OsType::Web(params) => Options::query_args(&params.search),
            _ => Vec::new(),
        };
        let options = match Options::parse(&args) {
            Ok(options) => options,
            Err(error) => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    eprintln!("{error}");
                    std::process::exit(2);
                }
                #[cfg(target_arch = "wasm32")]
                return finish(Err(error));
            }
        };
        // Off this thread a read of a file the page has not fetched waits;
        // the signal brings the page round to fetch it.
        #[cfg(target_arch = "wasm32")]
        document::install_font_reader();
        #[cfg(target_arch = "wasm32")]
        makepad_asset_data::source::store::set_page(Box::new(makepad_widgets::makepad_platform::thread::SignalToUI::set_ui_signal));
        if let Some(mut view) = self.ui.widget(cx, ids!(view)).borrow_mut::<PlayerView>() {
            // A viewer's loader draws while the page fetches.
            view.started = Cx::monotonic_now();
            if !options.deterministic {
                view.next_frame = cx.new_next_frame();
            }
            view.pending = Some(options);
            if let Err(done) = view.try_open(cx) {
                view.finish(cx, done);
            }
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::script_mod(vm);
        makepad_content_packs::script_mod(vm);
        makepad_story_edit_render::script_mod(vm);
        viewer::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
