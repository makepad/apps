//! One cut engine's pictures: the frame of a time recorded, kept attached
//! while it paints and handed out once whole, as Stage's renderer takes
//! them (locked time: every graphic's shutter sampled, every exact clip
//! frame waited for) or as its preview shows them (the nearest frame a clip
//! has standing in, the last whole picture held meanwhile).

use makepad_story_edit_render::audio::{fps_ratio, frame_count, frame_time};
use makepad_story_edit_render::compose::Painted;
use makepad_story_edit_render::{CutEngine, CutJob, EngineStatus};
use makepad_widgets::*;

/// Records of one frame that make no progress before the player gives up on
/// it (a shutter gathering its sub-frames, hundreds of records for a fast
/// frame, is progress).
const MAX_RERECORDS: u32 = 120;

pub struct Picture {
    engine: CutEngine,
    locked: bool,
    pub fps: (u32, u32),
    pub frames: u64,
    /// The frame recorded and painting (its index), and how often it was
    /// recorded again.
    recorded: Option<u64>,
    rerecords: u32,
    /// The shutter's steps at the last record ([`CutEngine::shutter_progress`]).
    progress: u64,
    /// The last whole frame: its index and picture.
    shown: Option<(u64, Texture)>,
}

impl Picture {
    pub fn new(cx: &mut Cx, job: CutJob, locked: bool) -> Self {
        let fps = fps_ratio(job.doc.output.fps);
        let frames = frame_count(job.doc.duration, fps);
        let mut engine = CutEngine::new(cx, job);
        // Locked time draws a frame as a render does; else as the preview.
        engine.set_locked_time(locked);
        if !locked {
            engine.set_keep_frames(makepad_story_edit_render::preview::PREVIEW_KEEP_BYTES);
            engine.set_warm_loads(true);
        }
        Self { engine, locked, fps, frames, recorded: None, rerecords: 0, progress: 0, shown: None }
    }

    pub fn engine(&self) -> &CutEngine {
        &self.engine
    }

    /// The pixel size frames are recorded at from the next record on (None:
    /// the document's own).
    pub fn set_raster(&mut self, size: Option<(u32, u32)>) {
        self.engine.set_raster(size);
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.engine.handle_event(cx, event);
    }

    /// The frame index time `t` falls in (the cut's last frame holds).
    pub fn frame_of(&self, t: f64) -> u64 {
        ((t.max(0.0) * self.fps.0 as f64 / self.fps.1 as f64 + 1e-9).floor() as u64).min(self.frames.saturating_sub(1))
    }

    pub fn ready(&self) -> Result<bool, String> {
        match self.engine.status() {
            EngineStatus::Loading => Ok(false),
            EngineStatus::Ready => Ok(true),
            EngineStatus::Failed(error) => Err(error),
        }
    }

    /// The last whole picture.
    pub fn shown(&self) -> Option<&(u64, Texture)> {
        self.shown.as_ref()
    }

    /// Work toward frame `n`, inside the window's pass: `Ok(Some(texture))`
    /// once its picture is whole (locked), or the last whole picture while it
    /// paints (preview). One record per call.
    pub fn draw(&mut self, cx: &mut Cx2d, n: u64, live: Option<f64>) -> Result<Option<Texture>, String> {
        if !self.ready()? {
            return Ok(None);
        }
        if let Some(recorded) = self.recorded {
            match self.engine.painted(cx) {
                Painted::Painting if self.engine.stale(cx) => {
                    self.recorded = None;
                    let progress = self.engine.shutter_progress();
                    if progress == self.progress {
                        self.rerecords += 1;
                    }
                    self.progress = progress;
                    if self.rerecords > MAX_RERECORDS {
                        return Err(format!("frame {recorded} never painted whole: {}", self.engine.painting_report(cx)));
                    }
                }
                Painted::Painting => {
                    let errors = self.engine.graphic_errors();
                    if !errors.is_empty() {
                        return Err(format!("frame {recorded} cannot paint: {}", errors.join("; ")));
                    }
                    self.engine.reattach(cx);
                    return Ok(self.held(n));
                }
                Painted::Whole => {
                    self.recorded = None;
                    self.rerecords = 0;
                    if let Some(texture) = self.engine.output() {
                        self.shown = Some((recorded, texture));
                    }
                }
            }
        }
        if let Some((index, texture)) = &self.shown {
            if *index == n {
                return Ok(Some(texture.clone()));
            }
        }
        if let Some(live) = live {
            self.engine.set_live(live);
        }
        // The picture shown stays whole: the frame records into the other
        // of the output's two targets.
        self.engine.hold_output(self.shown.as_ref().map(|(_, texture)| texture.clone()));
        let t = frame_time(n, self.fps);
        let recorded = if self.locked { self.engine.record(cx, t)?.is_some() } else { self.engine.record_preview(cx, t)?.is_some() };
        if recorded {
            self.recorded = Some(n);
        }
        Ok(self.held(n))
    }

    fn held(&self, _n: u64) -> Option<Texture> {
        if self.locked {
            None
        } else {
            self.shown.as_ref().map(|(_, texture)| texture.clone())
        }
    }
}

impl Drop for Picture {
    fn drop(&mut self) {
        self.engine.stop();
    }
}
