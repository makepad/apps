//! The CUT page's preview seam: real frames at `t`, drawn by the same
//! engine and compositor as a render, so what the page shows is what the
//! MP4 will hold (without the encoder, the readback or the sound).
//!
//! The page owns a `Box<dyn PreviewSource>`; it calls `seek` when the
//! playhead moves, `handle_event` on every event, `draw` from its own draw
//! (inside its window pass) and shows `frame()`. While `busy()` it keeps
//! drawing, so the preview's passes paint.

use crate::compose::Painted;
use makepad_story_cut::{AssetRef, MediaRef};
use crate::engine::{CutEngine, CutJob, EngineOptions, EngineStatus};
use crate::sound::CutSound;
use makepad_stage_asset_stream::VideoBudget;
use std::collections::HashMap;
use std::sync::Arc;
use makepad_widgets::*;

#[derive(Clone, Debug, PartialEq)]
pub enum PreviewStatus {
    /// Resolving media and building effects.
    Loading,
    /// Drawing the frame at this time.
    Drawing(f64),
    /// The shown frame is the one asked for.
    Idle,
    Failed(String),
}

pub trait PreviewSource {
    /// Show the frame at `t` (edit seconds): the render's frame that `t`
    /// falls in. The newest ask wins; a frame already on its way is
    /// finished first, then the newest is drawn.
    fn seek(&mut self, t: f64);
    /// Workers and reader results. Call on every event.
    fn handle_event(&mut self, cx: &mut Cx, event: &Event);
    /// Record or finish a frame. Call from the page's draw, inside its
    /// window pass.
    fn draw(&mut self, cx: &mut Cx2d);
    /// The newest fully painted frame: its time and texture (output size).
    /// The texture stays whole until the frame after next is recorded;
    /// read it on every draw.
    fn frame(&self) -> Option<(f64, Texture)>;
    fn status(&self) -> PreviewStatus;
    /// Whether the shown frame ([`Self::frame`]) is exact for its time.
    /// While a clip's exact frame is still being read (a scrub, a seek) the
    /// preview shows the frame at `t` with the nearest frame that clip has
    /// standing in, and then the exact one.
    fn exact(&self) -> bool {
        true
    }
    /// The page plays (`Some((rate, t))`: from cut time `t` at `rate`) or
    /// not. A preview with sound plays the cut's mix at rate 1; call it on
    /// every frame of the playback, it restarts the sound only when the
    /// playhead left it (a loop, a seek).
    fn set_playing(&mut self, _playing: Option<(f64, f64)>) {}
    /// How graphic `node` loads in this cut ([`crate::graphic::FilmLoad`]),
    /// once the cut has loaded.
    fn film_load(&mut self, _node: &str) -> Option<crate::graphic::FilmLoad> {
        None
    }
    /// The pictures of the apps a host hosts, per `AppView` node, for the
    /// frames recorded from now on; `changed` says one is new since the
    /// last call (the shown frame is then recorded again).
    fn set_externals(&mut self, _externals: HashMap<String, crate::compose::ExternalFrame>, _changed: bool) {}
    /// Where the last frame placed each `AppView` picture, as `[x, y, w, h]`
    /// in output pixels, by node.
    fn placed(&self) -> Vec<(String, [f64; 4])> {
        Vec::new()
    }
    /// Where the sound has reached, while it plays (edit seconds).
    fn sound_clock(&self) -> Option<f64> {
        None
    }
    /// Pictures to show for a clip not read yet (library thumbnails), with
    /// each source's size.
    fn set_stand_ins(&mut self, _pictures: HashMap<MediaRef, (Texture, u32, u32)>) {}
    /// What the frame asked for is waiting for, while it waits (clip
    /// frames, a decoder slot, the cut to load, ...).
    fn waiting_for(&self) -> Option<String> {
        None
    }
    /// What the cut's Motion graphics could not draw (a document that does
    /// not load, a shader that does not compile, ...), each once: the page
    /// shows these instead of a blank graphic.
    fn graphic_errors(&self) -> Vec<String> {
        Vec::new()
    }
    /// Whether the page must keep drawing for the preview to progress.
    /// Frames drawn live while playing since the last call: each frame
    /// and the seconds from its record to its being whole (the render
    /// cache's measure of what plays live).
    fn take_frame_costs(&mut self) -> Vec<(u64, f64)> {
        Vec::new()
    }

    /// While the preview loads the edit and draws its first picture: how
    /// far along (0 to 1) and what it loads now. `None` once it shows one.
    fn progress(&self) -> Option<(f64, String)> {
        None
    }

    /// Load the graphics as an editor's, their literals patched in place
    /// (an editor panel over the whole edit is open).
    fn set_live_literals(&mut self, _on: bool) {}

    /// Literals of the graphics were patched: the frame is drawn again.
    fn literals_patched(&mut self) {}

    /// A held gesture moved the cut's clips: the frames follow `doc` now.
    fn retime(&mut self, _doc: std::sync::Arc<makepad_story_cut::CutDoc>) {}

    /// Library documents' texts changed during a held gesture.
    fn replace_texts(&mut self, _graphics: Vec<(AssetRef, String)>, _modules: Vec<(AssetRef, String)>) {}

    /// A held recolour's step on the copies the graphics' loads made.
    fn recolour_copies(&mut self, _map: &[([f32; 4], [f32; 4])]) -> usize {
        0
    }

    /// A held gesture ended.
    fn gesture_end(&mut self) {}

    fn busy(&self) -> bool {
        matches!(self.status(), PreviewStatus::Loading | PreviewStatus::Drawing(_))
    }
}

/// Draws a recorded frame may take to report painted (shader compiles
/// aside) before the preview gives up on it and records the newest want: a
/// frame paints in the draw after its recording.
const PAINT_PATIENCE: u32 = 60;

/// Frames a preview draws ahead at most, however long the cut: every graphic
/// starts ready first, and what is left spreads over the cut.
const PREWARM_FRAMES: usize = 64;

/// Draws a frame drawn ahead may take. Its shader compiles run on without
/// more draws; nobody is waiting for its picture.
const PREWARM_PATIENCE: u32 = 8;

/// Decoded frames each clip load of a preview keeps for scrubbing: about a
/// second of 1080p60, twenty 4K frames. Past it the frames
/// farthest from the playhead go, and a scrub back decodes again.
pub const PREVIEW_KEEP_BYTES: usize = 256 << 20;

/// The renderer's own preview: an [`CutEngine`] without sound. Seeks snap
/// to the render's frame grid, so every frame it shows is one the MP4 will
/// hold, and the texture it hands out stays whole while the next frame
/// paints (the output target is double-buffered).
pub struct CutPreview {
    engine: CutEngine,
    fps: (u32, u32),
    frames: u64,
    /// The frame asked for, as its index.
    want: Option<u64>,
    /// The frame recorded and painting, and whether it is exact; the draws
    /// it has been painting for.
    recorded: Option<(u64, bool)>,
    painting_draws: u32,
    shown: Option<(u64, Texture, bool)>,
    /// The last failure, until the next seek elsewhere.
    error: Option<String>,
    /// The sound, once asked for ([`Self::with_sound`]) and the cut has
    /// loaded; and how the page plays.
    sound: Option<CutSound>,
    sound_wanted: bool,
    playing: Option<(f64, f64)>,
    /// A frame is being recorded again because it drew before something it
    /// shows was ready.
    again: bool,
    first_logged: bool,
    /// Until the first whole picture: what it waited for, and from when.
    first_waits: Vec<String>,
    /// When the frame painting was recorded, and the costs measured.
    recorded_at: f64,
    costs: Vec<(u64, f64)>,
    /// Frames drawn ahead while the preview is idle, so every graphic's
    /// scenes, shaders, kernels and textures are ready before the playhead
    /// reaches them (planned once the cut has loaded), and how many there
    /// were; whether the frame painting is one of them (not shown).
    prewarm: Option<std::collections::VecDeque<u64>>,
    prewarm_total: usize,
    prewarming: bool,
    /// Frames ahead that could not be recorded yet (asked again once the
    /// others are drawn, up to a bound).
    prewarm_misses: usize,
    prewarm_deferred: Vec<u64>,
    /// An `AppView` picture is new since the shown frame was recorded.
    externals_new: bool,
}

impl CutPreview {
    /// `job.audio` is ignored: a preview decodes no sound.
    pub fn new(cx: &mut Cx, job: CutJob) -> Self {
        Self::with_options(cx, job, EngineOptions::default())
    }

    /// Inside Stage: every clip load holds a slot of the VJ decks' decoder
    /// `budget`, and at most `max_loads` are open at once.
    pub fn with_budget(cx: &mut Cx, job: CutJob, budget: Arc<VideoBudget>, max_loads: usize) -> Self {
        Self::with_options(cx, job, EngineOptions { max_loads, budget: Some(Box::new(budget)) })
    }

    pub fn with_options(cx: &mut Cx, job: CutJob, options: EngineOptions) -> Self {
        let fps = crate::audio::fps_ratio(job.doc.output.fps);
        let frames = crate::audio::frame_count(job.doc.duration, fps);
        let mut engine = CutEngine::with_options(cx, CutJob { audio: false, ..job }, options);
        engine.set_keep_frames(PREVIEW_KEEP_BYTES);
        engine.set_warm_loads(true);
        Self {
            engine, fps, frames, want: Some(0), recorded: None, painting_draws: 0, shown: None, error: None, sound: None, sound_wanted: false,
            playing: None, again: false, first_logged: false, first_waits: Vec::new(), recorded_at: 0.0, costs: Vec::new(),
            prewarm: None, prewarm_total: 0, prewarming: false, prewarm_misses: 0, prewarm_deferred: Vec::new(), externals_new: false,
        }
    }

    /// Also play the cut's sound while the page plays: it is decoded on
    /// the pool once the cut has loaded, and played through [`crate::sound`].
    pub fn with_sound(mut self) -> Self {
        self.sound_wanted = true;
        self
    }

    /// The sound's decode and start, one step per call.
    fn step_sound(&mut self, cx: &mut Cx) {
        if !self.sound_wanted {
            return;
        }
        if self.sound.is_none() && self.engine.status() == EngineStatus::Ready {
            let Some(job) = self.engine.sound_job() else { return };
            match CutSound::new(cx, job) {
                Ok(sound) => self.sound = Some(sound),
                Err(error) => {
                    log!("edit preview: no sound: {error}");
                    self.sound_wanted = false;
                    return;
                }
            }
        }
        let Some(sound) = &mut self.sound else { return };
        sound.pump();
        sound.follow(self.playing.filter(|(rate, _)| (rate - 1.0).abs() < 1e-6).map(|(_, t)| t));
    }

    /// The frames to draw ahead, once each: see [`prewarm_plan`].
    fn plan_prewarm(&mut self) {
        let doc = self.engine.doc().clone();
        let seconds: Vec<u64> = (0..doc.duration.ceil().max(1.0) as usize).map(|s| self.frame_of(s as f64 + 0.1)).collect();
        let starts: Vec<u64> = doc.nodes.iter().filter(|node| doc.graphics.iter().any(|g| g.id == node.id)).filter_map(|node| node.span).map(|span| self.frame_of(span.start + 0.02)).collect();
        let frames = prewarm_plan(starts, &seconds);
        self.prewarm_total = frames.len();
        self.prewarm = Some(frames.into());
    }

    /// Whether drawing ahead gives way now: the page plays, or a frame is
    /// asked for that is not shown (a seek). Drawing ahead is for idle
    /// moments only; it never holds up a frame asked for.
    fn ahead_yields(&self) -> bool {
        ahead_yields(self.playing.is_some(), self.want, self.shown.as_ref().map(|(n, _, exact)| (*n, *exact)))
    }

    /// Frames still to draw ahead (none before the plan).
    fn prewarm_left(&self) -> usize {
        self.prewarm.as_ref().map_or(0, |frames| frames.len()) + self.prewarm_deferred.len() + self.prewarming as usize
    }

    /// Frames drawn ahead so far and in all, once planned.
    pub fn prewarm_progress(&self) -> Option<(usize, usize)> {
        self.prewarm.as_ref()?;
        Some((self.prewarm_total - self.prewarm_left().min(self.prewarm_total), self.prewarm_total))
    }

    /// The frame index `t` falls in, on the render's grid.
    fn frame_of(&self, t: f64) -> u64 {
        let n = (t.max(0.0) * self.fps.0 as f64 / self.fps.1 as f64 + 1e-9).floor() as u64;
        n.min(self.frames.saturating_sub(1))
    }
}

/// What to draw ahead: the first frame of every graphic, then seconds of the
/// cut spread over it, [`PREWARM_FRAMES`] at most.
fn prewarm_plan(mut starts: Vec<u64>, seconds: &[u64]) -> Vec<u64> {
    fn spread(frames: &[u64], room: usize) -> Vec<u64> {
        if frames.len() <= room {
            return frames.to_vec();
        }
        (0..room).map(|i| frames[i * frames.len() / room]).collect()
    }
    starts.sort_unstable();
    starts.dedup();
    let mut frames = spread(&starts, PREWARM_FRAMES);
    frames.extend(spread(seconds, PREWARM_FRAMES - frames.len()));
    frames.sort_unstable();
    frames.dedup();
    frames
}

/// Whether a frame being drawn ahead gives way: the page `playing`, or a
/// `want`ed frame that is not the exact one `shown`.
fn ahead_yields(playing: bool, want: Option<u64>, shown: Option<(u64, bool)>) -> bool {
    playing || want.is_some_and(|want| shown != Some((want, true)))
}

#[cfg(test)]
mod ahead_tests {
    use super::{ahead_yields, prewarm_plan, PREWARM_FRAMES};

    #[test]
    fn drawing_ahead_is_bounded_and_keeps_every_graphic_start() {
        // A short cut: all of it.
        assert_eq!(prewarm_plan(vec![30, 10, 10], &[0, 10, 20]), vec![0, 10, 20, 30]);
        // An hour with a few graphics: the cap, every graphic start in it,
        // the rest spread to the cut's end.
        let seconds: Vec<u64> = (0..3600).map(|s| s * 60).collect();
        let starts = vec![7, 1001, 200_003];
        let plan = prewarm_plan(starts.clone(), &seconds);
        assert!(plan.len() <= PREWARM_FRAMES);
        assert!(starts.iter().all(|start| plan.contains(start)));
        assert!(*plan.iter().rev().nth(1).unwrap() > 3000 * 60);
        // More graphics than the cap: still the cap.
        let starts: Vec<u64> = (0..1000).collect();
        assert_eq!(prewarm_plan(starts, &seconds).len(), PREWARM_FRAMES);
    }

    #[test]
    fn drawing_ahead_gives_way_to_play_and_to_a_frame_asked_for() {
        // Idle on the exact frame asked for: drawing ahead goes on.
        assert!(!ahead_yields(false, Some(10), Some((10, true))));
        // The page plays: every frame goes to the playhead.
        assert!(ahead_yields(true, Some(10), Some((10, true))));
        // A seek elsewhere, a stand-in shown, nothing shown yet.
        assert!(ahead_yields(false, Some(11), Some((10, true))));
        assert!(ahead_yields(false, Some(10), Some((10, false))));
        assert!(ahead_yields(false, Some(10), None));
        // Nothing asked for.
        assert!(!ahead_yields(false, None, Some((10, true))));
    }
}

impl PreviewSource for CutPreview {
    fn set_externals(&mut self, externals: HashMap<String, crate::compose::ExternalFrame>, changed: bool) {
        self.engine.set_externals(externals);
        self.externals_new |= changed;
    }

    fn placed(&self) -> Vec<(String, [f64; 4])> {
        self.engine.placed()
    }

    fn film_load(&mut self, node: &str) -> Option<crate::graphic::FilmLoad> {
        self.engine.film_load(node)
    }

    fn graphic_errors(&self) -> Vec<String> {
        self.engine.graphic_errors()
    }

    fn seek(&mut self, t: f64) {
        let n = self.frame_of(t);
        if self.want != Some(n) {
            self.error = None;
        }
        self.want = Some(n);
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.engine.handle_event(cx, event);
        self.step_sound(cx);
    }

    fn set_playing(&mut self, playing: Option<(f64, f64)>) {
        self.playing = playing;
        // A stop is heard at once, even with no event to come (a viewer
        // covered by another view).
        if let (None, Some(sound)) = (playing, &mut self.sound) {
            sound.follow(None);
        }
    }

    fn set_stand_ins(&mut self, pictures: HashMap<MediaRef, (Texture, u32, u32)>) {
        self.engine.set_stand_ins(pictures);
    }

    fn set_live_literals(&mut self, on: bool) {
        self.engine.set_live_literals(on);
    }

    fn retime(&mut self, doc: std::sync::Arc<makepad_story_cut::CutDoc>) {
        self.engine.retime(doc);
        if let Some(shown) = &mut self.shown {
            shown.2 = false;
        }
    }

    fn replace_texts(&mut self, graphics: Vec<(AssetRef, String)>, modules: Vec<(AssetRef, String)>) {
        self.engine.replace_texts(graphics, modules);
        if let Some(shown) = &mut self.shown {
            shown.2 = false;
        }
    }

    fn recolour_copies(&mut self, map: &[([f32; 4], [f32; 4])]) -> usize {
        self.engine.recolour_copies(map)
    }

    fn gesture_end(&mut self) {
        self.engine.gesture_end();
    }

    fn literals_patched(&mut self) {
        self.engine.literals_patched();
        // The frame shown is drawn again (and what was drawn ahead is old).
        if let Some(shown) = &mut self.shown {
            shown.2 = false;
        }
    }

    fn sound_clock(&self) -> Option<f64> {
        self.sound.as_ref().and_then(|sound| sound.clock())
    }

    fn draw(&mut self, cx: &mut Cx2d) {
        if self.error.is_some() {
            return;
        }
        self.engine.warm(cx);
        if self.prewarming && self.recorded.is_some() && self.ahead_yields() {
            // A frame drawn ahead never holds up the frame asked for (a
            // play, a seek): it goes back to the plan, drawn at the next
            // idle moment.
            if let (Some((n, _)), Some(frames)) = (self.recorded.take(), self.prewarm.as_mut()) {
                frames.push_front(n);
            }
            self.prewarming = false;
            self.painting_draws = 0;
        }
        if self.prewarming && self.recorded.is_some() {
            // A frame drawn ahead: done once painted (whole or not), never shown.
            let done = self.engine.painted(cx) == Painted::Whole || self.engine.stale(cx) || {
                self.painting_draws += 1;
                self.painting_draws >= PREWARM_PATIENCE
            };
            if !done {
                self.engine.reattach(cx);
                return;
            }
            self.recorded = None;
            self.prewarming = false;
            self.painting_draws = 0;
        }
        if let Some((n, exact)) = self.recorded {
            match self.engine.painted(cx) {
                Painted::Whole => {
                    if let Some(texture) = self.engine.output() {
                        if exact && !self.shown.as_ref().is_some_and(|(_, _, exact)| *exact) && !self.first_logged {
                            self.first_logged = true;
                            log!("edit preview: first whole picture (frame {n}) {:.2} s after open{}", self.engine.since_open(),
                                if self.first_waits.is_empty() { String::new() } else { format!("; it waited for {}", self.first_waits.join(", ")) });
                        }
                        if exact && self.playing.is_some() && self.costs.len() < 1024 {
                            self.costs.push((n, Cx::monotonic_now() - self.recorded_at));
                        }
                        self.shown = Some((n, texture, exact));
                    }
                    self.again = false;
                }
                // Painted, but drawn before something it shows was ready (a
                // glyph, a shader) while the playhead has moved on: shown as
                // it is (not exact) and the newest want recorded. Recording
                // it again for a playhead that never waits would show nothing
                // while every frame brings something new (text at a zooming
                // camera's sizes).
                Painted::Painting if self.engine.stale(cx) && self.want.is_some_and(|want| want != n) => {
                    if let Some(texture) = self.engine.output() {
                        self.shown = Some((n, texture, false));
                    }
                    self.again = false;
                }
                // Recorded again into the other output: the shown picture
                // stays until this one is whole.
                Painted::Painting if self.engine.stale(cx) => self.again = true,
                Painted::Painting => {
                    // Shader compiles are waited for, however long.
                    if !cx.draw_shaders_pending() {
                        self.painting_draws += 1;
                    }
                    if self.painting_draws < PAINT_PATIENCE {
                        self.engine.reattach(cx);
                        return;
                    }
                    // A frame that does not report painted must not freeze
                    // the viewer: say why once, and record the newest want.
                    log!("edit preview: frame {n} did not report painted in {PAINT_PATIENCE} draws; recording anew ({})", self.engine.painting_report(cx));
                }
            }
            self.recorded = None;
            self.painting_draws = 0;
        }
        if !self.first_logged {
            let waiting = self.engine.waiting_for();
            let reason = if !waiting.is_empty() { waiting.to_string() } else if self.recorded.is_some() { "its first paint".to_string() } else { String::new() };
            if !reason.is_empty() && self.first_waits.last().is_none_or(|last| !last.ends_with(&format!("{reason})"))) && self.first_waits.len() < 12 {
                self.first_waits.push(format!("({:.2} s: {reason})", self.engine.since_open()));
            }
        }
        let Some(want) = self.want else { return };
        if self.shown.as_ref().is_some_and(|(n, _, exact)| *n == want && *exact) && !self.externals_new {
            // Idle: draw a frame ahead, if any is left. Never while the
            // page plays: every frame the renderer has goes to the
            // playhead, and a playing preview is idle only between frames.
            if self.playing.is_some() {
                return;
            }
            if self.prewarm.is_none() && self.engine.status() == EngineStatus::Ready {
                self.plan_prewarm();
            }
            // Nearest the playhead first: the plate under it is ready
            // before the rest of the cut.
            if self.prewarm.as_ref().is_some_and(|frames| frames.is_empty()) && !self.prewarm_deferred.is_empty() {
                let deferred = std::mem::take(&mut self.prewarm_deferred);
                self.prewarm.as_mut().unwrap().extend(deferred);
            }
            let nearest = self.prewarm.as_ref().and_then(|frames| (0..frames.len()).min_by_key(|&i| frames[i].abs_diff(want)));
            if let Some(ahead) = nearest.and_then(|i| self.prewarm.as_mut().and_then(|frames| frames.remove(i))) {
                self.engine.hold_output(self.shown.as_ref().map(|(_, texture, _)| texture.clone()));
                if let Ok(Some(_)) = self.engine.record_preview(cx, crate::audio::frame_time(ahead, self.fps)) {
                    self.recorded = Some((ahead, false));
                    self.prewarming = true;
                } else if self.prewarm_misses < 8 * self.prewarm_total {
                    // Waiting for something (a clip, a document): again
                    // once the others are drawn.
                    self.prewarm_misses += 1;
                    self.prewarm_deferred.push(ahead);
                }
            }
            return;
        }
        self.engine.hold_output(self.shown.as_ref().map(|(_, texture, _)| texture.clone()));
        match self.engine.record_preview(cx, crate::audio::frame_time(want, self.fps)) {
            Ok(Some((_, exact))) => {
                self.externals_new = false;
                self.recorded = Some((want, exact));
                self.recorded_at = Cx::monotonic_now();
            }
            Ok(None) => {}
            Err(error) => self.error = Some(error),
        }
    }

    /// The shown frame's time (on the frame grid) and texture.
    fn frame(&self) -> Option<(f64, Texture)> {
        self.shown.as_ref().map(|(n, texture, _)| (crate::audio::frame_time(*n, self.fps), texture.clone()))
    }

    fn exact(&self) -> bool {
        self.shown.as_ref().is_some_and(|(_, _, exact)| *exact)
    }

    fn take_frame_costs(&mut self) -> Vec<(u64, f64)> {
        std::mem::take(&mut self.costs)
    }

    fn progress(&self) -> Option<(f64, String)> {
        if self.error.is_some() {
            return None;
        }
        match self.engine.status() {
            EngineStatus::Loading => return Some((0.0, "loading the cut".to_string())),
            EngineStatus::Failed(_) => return None,
            EngineStatus::Ready => {}
        }
        // Until the first picture; drawing frames ahead after it is the
        // preview's own background work, nothing the viewer waits on.
        self.prewarm_progress().is_none().then(|| (0.0, "the first picture".to_string()))
    }

    fn waiting_for(&self) -> Option<String> {
        if !self.busy() {
            return None;
        }
        let waiting = self.engine.waiting_for();
        if !waiting.is_empty() {
            return Some(waiting.to_string());
        }
        // Shown with a stand-in: what the exact frame still waits for.
        let note = self.engine.waiting_note();
        (!note.is_empty()).then(|| note.join(", "))
    }

    fn status(&self) -> PreviewStatus {
        if let Some(error) = &self.error {
            return PreviewStatus::Failed(error.clone());
        }
        match self.engine.status() {
            EngineStatus::Loading => PreviewStatus::Loading,
            EngineStatus::Failed(error) => PreviewStatus::Failed(error),
            EngineStatus::Ready => match (self.recorded, self.want) {
                // Drawing frames ahead: the host keeps drawing until they are done.
                (_, Some(want)) if self.prewarm_left() > 0 && self.playing.is_none() => PreviewStatus::Drawing(crate::audio::frame_time(want, self.fps)),
                (Some((n, _)), _) => PreviewStatus::Drawing(crate::audio::frame_time(n, self.fps)),
                (None, Some(want)) if !self.shown.as_ref().is_some_and(|(n, _, exact)| *n == want && *exact) => PreviewStatus::Drawing(crate::audio::frame_time(want, self.fps)),
                _ => PreviewStatus::Idle,
            },
        }
    }
}

impl Drop for CutPreview {
    fn drop(&mut self) {
        self.engine.stop();
    }
}
