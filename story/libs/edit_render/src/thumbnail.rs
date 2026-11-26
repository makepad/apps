//! A cut's thumbnail from the renderer's own frame: the frame at `t`
//! (on the render's frame grid) drawn by the same engine as a render, so
//! compound clips, effects and captions are in it, read back by ticket (it
//! coexists with any other readback user of its process, Stage's effect
//! thumbnails included) and scaled to `width`.
//!
//! In-process use, like the preview: make one with [`CutThumbnail::new`],
//! call `handle_event` on every event and `draw` from the host's draw
//! (inside its window pass), keep drawing while [`CutThumbnail::busy`], then
//! take [`CutThumbnail::result`]. The standalone binary's `thumbnail`
//! subcommand does the same in its hidden window.

use crate::audio;
use crate::compose::Painted;
use crate::engine::{CutEngine, CutJob, EngineOptions, EngineStatus};
use crate::render::opaque_rgba;
use makepad_widgets::*;

/// A thumbnail: tight top-down RGBA, `width` x `height`.
#[derive(Clone, Debug)]
pub struct Thumbnail {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The cut time of the frame it shows.
    pub t: f64,
}

impl Thumbnail {
    /// The same pixels as `0xAARRGGBB` (what Stage's image textures take).
    pub fn argb(&self) -> Vec<u32> {
        self.rgba.chunks_exact(4).map(|p| 0xff00_0000 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect()
    }
}

enum Step {
    Recording,
    Painting,
    /// Reading the frame back strip by strip: the strips taken so far.
    Reading { targets: Vec<(Texture, u32)>, pieces: Vec<Vec<u8>>, ticket: Option<ReadbackTicket> },
    Done(Result<Thumbnail, String>),
}

pub struct CutThumbnail {
    engine: CutEngine,
    t: f64,
    size: (u32, u32),
    step: Step,
    readback_wait_logged: bool,
    /// The engine was stopped (a failure): no retarget.
    stopped: bool,
}

impl CutThumbnail {
    /// The thumbnail of `job`'s edit at `t` seconds (snapped to the frame it
    /// falls in), rendered at `width` pixels wide; the height follows the
    /// output's aspect. No full-resolution intermediate frame is allocated.
    pub fn new(cx: &mut Cx, job: CutJob, t: f64, width: u32) -> Self {
        Self::with_options(cx, job, t, width, EngineOptions::default())
    }

    /// Inside Stage, pass the decks' decoder budget as for the preview.
    pub fn with_options(cx: &mut Cx, job: CutJob, t: f64, width: u32, options: EngineOptions) -> Self {
        let fps = audio::fps_ratio(job.doc.output.fps);
        let frames = audio::frame_count(job.doc.duration, fps).max(1);
        let n = ((t.max(0.0) * fps.0 as f64 / fps.1 as f64 + 1e-9).floor() as u64).min(frames - 1);
        let width = width.max(1);
        let height = (width as f64 * job.doc.output.height as f64 / job.doc.output.width.max(1) as f64).round().max(1.0) as u32;
        let mut engine = CutEngine::with_options(cx, CutJob { audio: false, ..job }, options);
        engine.set_raster_size((width.max(1), height));
        engine.set_locked_time(true);
        Self { engine, t: audio::frame_time(n, fps), size: (width, height), step: Step::Recording, readback_wait_logged: false, stopped: false }
    }

    /// Draw as the preview draws (each graphic once a frame), not with a
    /// render's sampled shutter: a small picture that need not be a
    /// render's frame (the timeline's clip tiles).
    pub fn set_draft(&mut self, draft: bool) {
        self.engine.set_locked_time(!draft);
    }

    /// Graphic `node`'s document as the host has it loaded: not loaded again.
    pub fn give_graphic(&mut self, node: &str, doc: makepad_motion::MotionDoc) {
        self.engine.give_graphic(node, doc);
    }

    pub fn busy(&self) -> bool {
        !matches!(self.step, Step::Done(_))
    }

    /// The thumbnail, or why there is none, once done.
    pub fn result(&self) -> Option<&Result<Thumbnail, String>> {
        match &self.step {
            Step::Done(result) => Some(result),
            _ => None,
        }
    }

    fn done(&mut self, result: Result<Thumbnail, String>) {
        // A thumbnail keeps its engine for [`Self::retarget`], without its
        // clip loads; a failed one stops.
        match &result {
            Ok(_) => self.engine.release(),
            Err(_) => {
                self.engine.stop();
                self.stopped = true;
            }
        }
        self.step = Step::Done(result);
    }

    /// Take another time with the same engine (the cut stays loaded, its
    /// effects built): `t` snaps to its frame, the last result goes. Only once done, and not after a failure;
    /// false when refused.
    pub fn retarget(&mut self, t: f64) -> bool {
        if self.busy() || self.stopped {
            return false;
        }
        let fps = audio::fps_ratio(self.engine.doc().output.fps);
        let frames = audio::frame_count(self.engine.doc().duration, fps).max(1);
        let n = ((t.max(0.0) * fps.0 as f64 / fps.1 as f64 + 1e-9).floor() as u64).min(frames - 1);
        self.t = audio::frame_time(n, fps);
        self.step = Step::Recording;
        true
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if !self.busy() {
            return;
        }
        self.engine.handle_event(cx, event);
        if let EngineStatus::Failed(error) = self.engine.status() {
            return self.done(Err(error));
        }
        // A frame takes as long as it takes (a busy machine is slow, not
        // wrong); what ends the wait is the render saying it cannot draw it:
        // a graphic that does not load, a shader that does not compile.
        if matches!(self.step, Step::Recording | Step::Painting) {
            let errors = self.engine.graphic_errors();
            if !errors.is_empty() {
                return self.done(Err(errors.join("; ")));
            }
        }
        let Step::Reading { ticket: Some(ticket), .. } = &self.step else { return };
        let ticket = *ticket;
        let Some(result) = cx.try_take_texture_readbacks_for(&[ticket]).into_iter().next() else { return };
        let data = match result.data {
            Ok(data) => data,
            Err(error) => return self.done(Err(format!("readback: {error}"))),
        };
        let mut rgba = Vec::new();
        opaque_rgba(&data, &mut rgba);
        let Step::Reading { targets, pieces, ticket } = &mut self.step else { return };
        pieces.push(rgba);
        *ticket = None;
        if pieces.len() == targets.len() {
            let whole: Vec<u8> = pieces.concat();
            let (width, height) = self.size;
            let result = if whole.len() == width as usize * height as usize * 4 {
                Ok(Thumbnail { rgba: whole, width, height, t: self.t })
            } else { Err(format!("frame of {} bytes is not {width}x{height}", whole.len())) };
            self.done(result);
        }
    }

    /// Record, wait for paint, read back. From the host's draw.
    pub fn draw(&mut self, cx: &mut Cx2d) {
        // The host may redraw between paint completion and the readback's
        // admission. Keep the producer chain attached until its pixels arrive;
        // a dirty producer must never be orphaned while its ticket waits.
        if matches!(self.step, Step::Painting | Step::Reading { .. }) {
            self.engine.reattach(cx);
        }
        match &self.step {
            Step::Recording => {
                let (w, h) = (self.size.0 as usize, self.size.1 as usize);
                let row = (w * 4).div_ceil(4096) * 4096;
                let rows = (TEXTURE_READBACK_MAX_BYTES / row.max(1)).max(1);
                self.engine.set_readback_strips((rows < h).then_some(rows as u32));
                match self.engine.record(cx, self.t) {
                    Ok(Some(_)) => {
                        // A missing graphic can record an otherwise valid black
                        // output pass. Report its load/evaluation error before
                        // treating that empty composition as a cacheable frame.
                        let errors = self.engine.graphic_errors();
                        if errors.is_empty() { self.step = Step::Painting; }
                        else { self.done(Err(errors.join("; "))); }
                    }
                    Ok(None) => {}
                    Err(error) => self.done(Err(error)),
                }
            }
            Step::Painting => match self.engine.painted(cx) {
                // It will not paint whole (a render-kit draw dropped, or a
                // graphic drawn before its glyphs were ready): record it again.
                // The host keeps drawing while the thumbnail is busy; only
                // its own widget, never the whole window.
                Painted::Painting if self.engine.stale(cx) => self.step = Step::Recording,
                Painted::Painting => {}
                Painted::Whole => {
                    let targets = self.engine.readback_targets();
                    if targets.is_empty() {
                        return self.done(Err("nothing to read back".into()));
                    }
                    self.step = Step::Reading { targets, pieces: Vec::new(), ticket: None };
                }
            },
            Step::Reading { .. } => {}
            Step::Done(_) => {}
        }
        // Pin the painted allocation in the same draw that proves it whole,
        // rather than waiting for another host draw to request its readback.
        if let Step::Reading { targets, pieces, ticket: None } = &self.step {
            let texture = targets[pieces.len()].0.clone();
            match texture.read_back(cx, ReadbackRequest::default()) {
                Ok(requested) => {
                    self.readback_wait_logged = false;
                    if let Step::Reading { ticket, .. } = &mut self.step { *ticket = Some(requested); }
                }
                // The readback request limit is reached: retain this image and retry later.
                Err(ReadbackError::Backpressure) => {
                    if !self.readback_wait_logged {
                        log!("cut thumbnail: context readback request limit reached; thumbnail strip waits");
                        self.readback_wait_logged = true;
                    }
                }
                Err(error) => self.done(Err(format!("readback: {error}"))),
            }
        }
    }
}

/// `rgba` (`w` x `h`) area-averaged down to `width` wide (never up), the
/// height following the aspect.
pub fn scale(rgba: &[u8], w: u32, h: u32, width: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let (w, h) = (w as usize, h as usize);
    if rgba.len() != w * h * 4 || w == 0 || h == 0 {
        return Err(format!("frame of {} bytes is not {w}x{h}", rgba.len()));
    }
    let tw = (width as usize).clamp(1, w);
    let th = ((h * tw) as f64 / w as f64).round().max(1.0) as usize;
    let data: Vec<u32> = rgba.chunks_exact(4).map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]])).collect();
    let out = makepad_widgets::makepad_draw::image_cache::area_downscale(&data, w, h, tw, th).iter().flat_map(|p| p.to_le_bytes()).collect();
    Ok((out, tw as u32, th as u32))
}

#[cfg(test)]
mod tests {
    use super::scale;

    #[test]
    fn scaling_averages_areas_and_keeps_the_aspect() {
        // 4x2: left half white, right half black.
        let mut rgba = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 255 } else { 0 };
                rgba.extend([v, v, v, 255]);
            }
        }
        let (out, w, h) = scale(&rgba, 4, 2, 2).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, [255, 255, 255, 255, 0, 0, 0, 255]);
        // Never up.
        assert_eq!(scale(&rgba, 4, 2, 100).unwrap().1, 4);
        assert!(scale(&rgba, 5, 2, 2).is_err());
    }
}
