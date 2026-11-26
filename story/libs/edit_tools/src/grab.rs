//! Frames of a cut on the real GPU, for `frame`, `strip` and `onion`:
//! one renderer engine (`CutEngine`, the same locked-time path as a
//! render, nested cuts and captions included) records each asked time in
//! turn, waits until it has painted, reads it back by ticket (so it
//! coexists with any other readback user in the process), measures its
//! exposure at the output's own pixels and scales it. Each Scene3D in the
//! frame also draws its coverage id map, read back after the frame and
//! measured per node (an inspect's extra pass; a render never draws it).
//!
//! In process (Stage's AI), like the renderer's `CutThumbnail`: make one
//! with [`FrameGrabber::new`], call `handle_event` on every event and `draw`
//! from the host's draw inside its window pass, keep drawing while
//! [`FrameGrabber::busy`], then take [`FrameGrabber::result`]. Every step is
//! bounded per paint; decoding and effect builds run on the engine's
//! workers.

use crate::coverage::SceneCover;
use crate::exposure::Exposure;
use crate::image::{contact_sheet, onion, sheet_columns, Picture};
use crate::input::frame_of;
use makepad_story_edit_render::audio::{fps_ratio, frame_time};
use makepad_story_edit_render::compose::Painted;
use makepad_story_edit_render::coverage::SceneCoverage;
use makepad_story_edit_render::engine::{CutEngine, CutJob, EngineOptions, EngineStatus};
use makepad_story_edit_render::render::opaque_rgba;
use makepad_story_edit_render::thumbnail::scale;
use makepad_widgets::*;
use std::collections::VecDeque;

/// One grabbed frame, `width` wide (never wider than the output).
#[derive(Clone, Debug)]
pub struct GrabbedFrame {
    /// The frame's index on the cut's frame grid and its exact time.
    pub frame: u64,
    pub t: f64,
    pub picture: Picture,
    /// Measured on the whole frame as read back, before scaling.
    pub exposure: Exposure,
    /// Each Scene3D the frame drew: what its nodes cover.
    pub coverage: Vec<SceneCover>,
}

enum Step {
    Recording,
    Painting,
    /// The strips of the frame read back so far.
    Reading { targets: Vec<(Texture, u32)>, pieces: Vec<Vec<u8>>, ticket: Option<ReadbackTicket> },
    /// The frame, read; its scenes' coverage maps (with their graphic
    /// nodes) read back one by one.
    Covering { frame: Box<GrabbedFrame>, maps: Vec<(String, SceneCoverage)>, ticket: Option<ReadbackTicket> },
}

pub struct FrameGrabber {
    engine: CutEngine,
    /// Frames still to grab, in order.
    todo: VecDeque<u64>,
    fps: (u32, u32),
    width: u32,
    step: Step,
    frames: Vec<GrabbedFrame>,
    result: Option<Result<(), String>>,
    /// When the current frame was first asked for.
    since: f64,
    /// How long one frame may take before the grab fails.
    pub stall_seconds: f64,
    /// What the current frame's paint last waited on (for a failure's words).
    paint_state: String,
    readback_wait_logged: bool,
}

impl FrameGrabber {
    /// Grab `job`'s edit at `times` (each snapped to the frame it falls in;
    /// repeats are grabbed once), `width` pixels wide at most.
    pub fn new(cx: &mut Cx, job: CutJob, times: &[f64], width: u32) -> Self {
        Self::with_options(cx, job, times, width, EngineOptions::default())
    }

    /// Inside Stage, pass the decks' decoder budget as for the preview.
    pub fn with_options(cx: &mut Cx, job: CutJob, times: &[f64], width: u32, options: EngineOptions) -> Self {
        let (fps, duration) = (job.doc.output.fps, job.doc.duration);
        let mut todo: Vec<u64> = times.iter().map(|&t| frame_of(t, fps, duration)).collect();
        todo.dedup();
        let mut engine = CutEngine::with_options(cx, CutJob { audio: false, ..job }, options);
        // A render's picture: graphics sample their shutters (motion blur's
        // sub-frames) as `render` does, not one sample at the frame time.
        engine.set_locked_time(true);
        engine.set_scene_coverage(true);
        let result = todo.is_empty().then(|| Err("no times to grab".to_string()));
        Self {
            engine,
            todo: todo.into(),
            fps: fps_ratio(fps),
            width: width.max(1),
            step: Step::Recording,
            frames: Vec::new(),
            result,
            since: Cx::monotonic_now(),
            stall_seconds: 120.0,
            paint_state: String::new(),
            readback_wait_logged: false,
        }
    }

    pub fn busy(&self) -> bool {
        self.result.is_none()
    }

    /// The frames in the order asked, or why the grab failed, once done.
    pub fn result(&self) -> Option<Result<&[GrabbedFrame], &str>> {
        match &self.result {
            Some(Ok(())) => Some(Ok(&self.frames)),
            Some(Err(error)) => Some(Err(error)),
            None => None,
        }
    }

    /// The frames grabbed so far.
    pub fn frames(&self) -> &[GrabbedFrame] {
        &self.frames
    }

    fn fail(&mut self, error: String) {
        self.engine.stop();
        self.result = Some(Err(error));
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if !self.busy() {
            return;
        }
        self.engine.handle_event(cx, event);
        if let EngineStatus::Failed(error) = self.engine.status() {
            return self.fail(error);
        }
        if !self.readback_wait_logged && Cx::monotonic_now() - self.since > self.stall_seconds {
            let n = self.todo.front().copied().unwrap_or(0);
            // Recording waits name what the frame needs; painting ones
            // what has not painted.
            let what = match self.engine.waiting_for() {
                "" => format!("its paint: {}", self.paint_state),
                what => what.to_string(),
            };
            return self.fail(format!("frame {n}: nothing after {:.0} s (waiting for {what})", self.stall_seconds));
        }
        let (Step::Reading { ticket: Some(ticket), .. } | Step::Covering { ticket: Some(ticket), .. }) = &self.step else { return };
        let ticket = *ticket;
        let Some(result) = cx.try_take_texture_readbacks_for(&[ticket]).into_iter().next() else { return };
        let data = match result.data {
            Ok(data) => data,
            Err(error) => return self.fail(format!("readback: {error}")),
        };
        if let Step::Covering { frame, maps, ticket } = &mut self.step {
            let (graphic, map) = maps.remove(0);
            let cover = map.measure(&data, result.width, result.height);
            frame.coverage.push(SceneCover { graphic, scene: map.scene, nodes: map.nodes, cover });
            *ticket = None;
            if maps.is_empty() {
                let Step::Covering { frame, .. } = std::mem::replace(&mut self.step, Step::Recording) else { unreachable!() };
                self.frame_done(*frame);
            }
            return;
        }
        let mut rgba = Vec::new();
        opaque_rgba(&data, &mut rgba);
        let Step::Reading { targets, pieces, ticket } = &mut self.step else { return };
        pieces.push(rgba);
        *ticket = None;
        if pieces.len() < targets.len() {
            return;
        }
        let whole: Vec<u8> = pieces.concat();
        let (w, h) = (self.engine.doc().output.width, self.engine.doc().output.height);
        let n = self.todo.pop_front().unwrap_or(0);
        let exposure = Exposure::of_rgba(&whole);
        let frame = match scale(&whole, w, h, self.width) {
            Ok((rgba, width, height)) => GrabbedFrame { frame: n, t: frame_time(n, self.fps), picture: Picture { width, height, rgba }, exposure, coverage: Vec::new() },
            Err(error) => return self.fail(error),
        };
        let maps = self.engine.scene_coverage();
        if maps.is_empty() {
            self.step = Step::Recording;
            self.frame_done(frame);
        } else {
            self.step = Step::Covering { frame: Box::new(frame), maps, ticket: None };
        }
    }

    /// `frame` is complete: keep it, then grab the next or finish.
    fn frame_done(&mut self, frame: GrabbedFrame) {
        self.frames.push(frame);
        self.since = Cx::monotonic_now();
        if self.todo.is_empty() {
            // A graphic that could not draw is not a picture of the cut.
            let errors = self.engine.graphic_errors();
            if !errors.is_empty() {
                return self.fail(format!("a graphic did not draw: {}", errors.join("; ")));
            }
            self.engine.stop();
            self.result = Some(Ok(()));
        }
    }

    /// Ask for `texture`'s pixels: a frame strip or a coverage map.
    fn read_back(&mut self, cx: &mut Cx2d, texture: Texture) {
        match texture.read_back(cx, ReadbackRequest::default()) {
            Ok(requested) => {
                if self.readback_wait_logged { self.since = Cx::monotonic_now(); }
                self.readback_wait_logged = false;
                if let Step::Reading { ticket, .. } | Step::Covering { ticket, .. } = &mut self.step {
                    *ticket = Some(requested);
                }
            }
            // The readback request limit is reached: asked again on the next paint.
            Err(ReadbackError::Backpressure) => {
                if !self.readback_wait_logged {
                    log!("frame grab: context readback request limit reached; frame strip or coverage map waits");
                    self.readback_wait_logged = true;
                }
            }
            Err(error) => self.fail(format!("readback: {error}")),
        }
    }

    /// Record, wait for the paint, read back. From the host's draw.
    pub fn draw(&mut self, cx: &mut Cx2d) {
        if !self.busy() {
            return;
        }
        match &self.step {
            Step::Recording => {
                let Some(&n) = self.todo.front() else { return };
                let (w, h) = (self.engine.doc().output.width as usize, self.engine.doc().output.height as usize);
                // The platform's readback reservation of a row.
                let row = (w * 4).div_ceil(4096) * 4096;
                let rows = (TEXTURE_READBACK_MAX_BYTES / row.max(1)).max(1);
                self.engine.set_readback_strips((rows < h).then_some(rows as u32));
                match self.engine.record(cx, frame_time(n, self.fps)) {
                    Ok(Some(_)) => self.step = Step::Painting,
                    Ok(None) => {}
                    Err(error) => self.fail(error),
                }
            }
            Step::Painting => match self.engine.painted(cx) {
                // It will not paint whole (a render-kit draw dropped, or a
                // graphic drawn before its glyphs were ready): record it again.
                Painted::Painting if self.engine.stale(cx) => {
                    self.paint_state = self.engine.painting_report(cx);
                    self.step = Step::Recording;
                    cx.redraw_all();
                }
                Painted::Painting => {
                    self.paint_state = self.engine.painting_report(cx);
                    self.engine.reattach(cx)
                }
                Painted::Whole => {
                    let targets = self.engine.readback_targets();
                    if targets.is_empty() {
                        return self.fail("nothing to read back".into());
                    }
                    self.step = Step::Reading { targets, pieces: Vec::new(), ticket: None };
                }
            },
            Step::Reading { ticket: Some(_), .. } | Step::Covering { ticket: Some(_), .. } => {}
            Step::Reading { targets, pieces, ticket: None } => {
                let texture = targets[pieces.len()].0.clone();
                self.read_back(cx, texture);
            }
            Step::Covering { maps, ticket: None, .. } => {
                let texture = maps[0].1.map.clone();
                self.read_back(cx, texture);
            }
        }
    }
}

/// A labelled contact sheet of grabbed frames (`@frame  seconds` under
/// each), `columns` wide or roughly square.
pub fn strip_picture(frames: &[GrabbedFrame], columns: Option<usize>) -> Picture {
    let Some(first) = frames.first() else { return Picture::new(1, 1, [0, 0, 0, 255]) };
    let columns = columns.unwrap_or_else(|| sheet_columns(frames.len(), first.picture.width, first.picture.height));
    let labelled: Vec<(Picture, String)> = frames.iter().map(|f| (f.picture.clone(), format!("@{}  {:.2}s", f.frame, f.t))).collect();
    contact_sheet(&labelled, columns)
}

/// The onion-skin trail of grabbed frames (the latest strongest).
pub fn onion_picture(frames: &[GrabbedFrame]) -> Result<Picture, String> {
    let pictures: Vec<Picture> = frames.iter().map(|f| f.picture.clone()).collect();
    onion(&pictures)
}
