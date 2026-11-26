//! A slide's engine and its last whole frame. The scheduler admits every
//! recording, including retries; a settled frame remains a poster.

use makepad_story_edit_render::audio::{fps_ratio, frame_count, frame_time};
use makepad_story_edit_render::compose::{ExternalFrame, Painted};
use makepad_story_edit_render::preview::PREVIEW_KEEP_BYTES;
use makepad_story_edit_render::{CutEngine, CutJob, EngineStatus, PreviewStatus};
use makepad_widgets::*;
use std::collections::HashMap;

pub struct SlideRenderer {
    engine: CutEngine,
    fps: (u32, u32),
    count: u64,
    wanted: u64,
    live: f64,
    pointer: Option<[f64; 2]>,
    recorded: Option<(u64, Option<u64>, bool)>,
    shown: Option<(u64, Option<u64>, Texture, bool)>,
    serial: u64,
    painting_draws: u32,
    externals_new: bool,
    /// The pixel size frames are recorded at (None: the deck's output size).
    raster: Option<(u32, u32)>,

    failed: Option<String>,
}

impl SlideRenderer {
    pub fn inspect_graphics(&mut self, cx: &mut Cx2d) -> Vec<makepad_story_edit_render::graphic::GraphicInspection> {
        let Some((index, _, _, _)) = &self.shown else { return Vec::new() };
        self.engine.inspect_graphics(cx, frame_time(*index, self.fps))
    }

    pub fn new(cx: &mut Cx, job: CutJob) -> Self {
        let fps = fps_ratio(job.doc.output.fps);
        let count = frame_count(job.doc.duration, fps);
        let mut engine = CutEngine::new(cx, job);
        engine.set_keep_frames(PREVIEW_KEEP_BYTES);
        
        Self { engine, fps, count, wanted: 0, live: 0.0, pointer: None, recorded: None, shown: None, serial: 0, painting_draws: 0, externals_new: false, raster: None, failed: None }
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.engine.handle_event(cx, event);
    }

    pub fn seek(&mut self, t: f64) {
        // A picture that cannot change with time is the picture of every
        // frame: once one is shown, the frame asked for stays (the slide
        // still plays; nothing on it moves, so nothing is recorded again).
        if keeps_frame(self.shown.is_some(), self.time_independent()) {
            return;
        }
        let wanted = frame_index(t, self.fps, self.count);
        if wanted != self.wanted { self.failed = None; }
        self.wanted = wanted;
    }

    /// The frame asked for last is the frame of slide time `t`: a slide
    /// that holds at `t` may sleep only on that picture, not on one drawn a
    /// moment before the hold was reached.
    pub fn wants_frame_of(&self, t: f64) -> bool {
        self.wanted == frame_index(t, self.fps, self.count)
    }

    /// Record at `size` pixels from now on: the picture is drawn at the
    /// pixels it is shown at, never scaled up from the output size. A new
    /// size records the frame again.
    pub fn set_raster(&mut self, size: Option<(u32, u32)>) {
        if self.raster != size {
            self.raster = size;
            self.engine.set_raster(size);
            self.externals_new = true;
        }
    }

    pub fn set_live(&mut self, live: f64) {
        self.live = live;
        self.engine.set_live(live);
    }

    /// The presenter's pointer over the picture, for graphics that read it.
    pub fn set_pointer(&mut self, pointer: Option<[f64; 2]>) {
        self.pointer = pointer;
        self.engine.set_pointer(pointer);
    }

    /// Load the slide's graphics for `lang` (the output's language).
    pub fn set_lang(&mut self, lang: Option<std::sync::Arc<str>>) {
        self.engine.set_lang(lang);
    }

    pub fn reads_live(&self) -> bool {
        self.engine.reads_live()
    }

    /// The picture is the same at every time of the cut and reads neither
    /// the ambient clock nor the pointer: no frame of it is due, whatever
    /// the timeline does. False while any of that is not known yet.
    pub fn time_independent(&self) -> bool {
        self.engine.time_independent() && !self.reads_live() && !self.reads_pointer()
    }

    /// Whether a graphic on screen reads the pointer.
    pub fn reads_pointer(&self) -> bool {
        self.engine.reads_pointer()
    }

    /// What the shown frame depends on besides its index: the ambient clock
    /// for a graphic that reads it, the pointer for one that reads that.
    fn live_key(&self) -> Option<u64> {
        let live = self.reads_live().then_some(self.live.to_bits());
        let pointer = self.reads_pointer().then(|| {
            let [x, y] = self.pointer.unwrap_or([-1.0, -1.0]);
            x.to_bits().rotate_left(21) ^ y.to_bits().wrapping_mul(0x9E37_79B9_7F4A_7C15)
        });
        match (live, pointer) {
            (None, None) => None,
            (live, pointer) => Some(live.unwrap_or(0) ^ pointer.unwrap_or(0).rotate_left(7) ^ u64::from(pointer.is_some())),
        }
    }

    /// Seconds from slide time `t` to the next frame of this render (the
    /// next `seek` that changes the frame asked for).
    pub fn next_frame_in(&self, t: f64) -> f64 {
        let next = frame_time(self.wanted + 1, self.fps);
        (next - t).max(0.0)
    }

    pub fn serial(&self) -> u64 {
        self.serial
    }

    fn settled(&self) -> bool {
        self.shown.as_ref().is_some_and(|(index, live, _, exact)| *index == self.wanted && *live == self.live_key() && *exact)
    }

    pub fn set_externals(&mut self, frames: HashMap<String, ExternalFrame>, changed: bool) {
        self.engine.set_externals(frames);
        self.externals_new |= changed;

    }

    pub fn frame(&self) -> Option<(f64, Texture)> {
        self.shown.as_ref().map(|(index, _, texture, _)| (frame_time(*index, self.fps), texture.clone()))
    }

    pub fn exact(&self) -> bool {
        self.shown.as_ref().is_some_and(|(_, _, _, exact)| *exact)
    }

    pub fn placed(&self) -> Vec<(String, [f64; 4])> {
        self.engine.placed()
    }

    pub fn status(&self) -> PreviewStatus {
        if let Some(error) = &self.failed { return PreviewStatus::Failed(error.clone()); }
        match self.engine.status() {
            EngineStatus::Loading => PreviewStatus::Loading,
            EngineStatus::Failed(error) => PreviewStatus::Failed(error),
            EngineStatus::Ready => {
                if self.externals_new || self.recorded.is_some() || !self.settled() {
                    PreviewStatus::Drawing(frame_time(self.wanted, self.fps))
                } else {
                    PreviewStatus::Idle
                }
            }
        }
    }

    pub fn draw(&mut self, cx: &mut Cx2d) {
        if self.failed.is_some() { return; }
        
        if let Some((index, live, exact)) = self.recorded {
            let painted = self.engine.painted(cx) == Painted::Whole;
            let stale = self.engine.stale(cx);
            if painted || stale && (self.wanted != index || self.live_key() != live) {
                if let Some(texture) = self.engine.output() {
                    self.shown = Some((index, live, texture, painted && exact));

                    self.serial = self.serial.wrapping_add(1);
                }
            } else if !stale {
                if !cx.draw_shaders_pending() { self.painting_draws += 1; }
                if self.painting_draws < super::player::PAINT_PATIENCE {
                    self.engine.reattach(cx);
                    return;
                }
                trace!("stage.deck", "deck: slide frame did not finish painting; recording again ({})", self.engine.painting_report(cx));
            }
            if !painted { self.externals_new = true; }
            self.recorded = None;
            self.painting_draws = 0;
        }
        if !self.externals_new && self.settled() {
            return;
        }
        self.engine.hold_output(self.shown.as_ref().map(|(_, _, texture, _)| texture.clone()));
        match self.engine.record_preview(cx, frame_time(self.wanted, self.fps)) {
            Ok(Some((_, exact))) => {
                self.recorded = Some((self.wanted, self.live_key(), exact));

                self.externals_new = false;
            }
            Ok(None) => {}
            Err(error) => self.failed = Some(error),
        }
    }
}

/// A shown picture that cannot change with time keeps the frame asked for.
fn keeps_frame(shown: bool, time_independent: bool) -> bool {
    shown && time_independent
}

/// The index of the frame shown at slide time `t` of a cut of `count` frames.
fn frame_index(t: f64, fps: (u32, u32), count: u64) -> u64 {
    ((t.max(0.0) * fps.0 as f64 / fps.1 as f64 + 1e-9).floor() as u64).min(count.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::{frame_index, keeps_frame};

    #[test]
    fn a_picture_independent_of_time_keeps_its_frame_once_it_has_one() {
        assert!(keeps_frame(true, true));
        assert!(!keeps_frame(false, true), "the first picture is still recorded");
        assert!(!keeps_frame(true, false), "a moving picture follows the clock");
    }

    #[test]
    fn a_hold_is_the_last_frame_up_to_it_not_the_one_before() {
        // 2.9 s at 60 fps is 173.99999999999997 frames: still frame 174.
        assert_eq!(frame_index(2.9, (60, 1), 360), 174);
        assert_eq!(frame_index(2.0 - 1.0 / 60.0 + 0.001, (60, 1), 360), 119);
        assert_eq!(frame_index(2.0, (60, 1), 360), 120);
        assert_eq!(frame_index(99.0, (60, 1), 360), 359, "the cut's last frame holds");
    }
}

impl Drop for SlideRenderer {
    fn drop(&mut self) {
        self.engine.stop();
    }
}
