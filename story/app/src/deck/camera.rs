//! DECK's live camera layers: the cameras a deck's `Camera` nodes name.
//!
//! One [`Feed`] per `Camera` node of a slide that is shown or next. A feed
//! has the life the player gives every slide resource:
//!
//! - *prepare* (the slide is next): the device is found by the node's
//!   `device` and opened at the node's size and rate, but not streamed (the
//!   camera captures nothing and its light stays off);
//! - *activate* (the slide shows, including the incoming side of a slide
//!   change): the stream starts;
//! - *deactivate* (the slide has left the screen): the stream stops at once;
//! - *teardown* (the slide is neither shown nor next): the device is closed
//!   and the feed's picture and buffers are dropped.
//!
//! The platform decodes and converts on the capture thread and keeps only
//! the newest frame ([`CameraLatest`]); the draw takes it into one texture,
//! which is the node's picture for the compositor. Crop, turn, mirror and
//! the corrections are the compositor's shader. A test pattern generates
//! BGRA frames on demand and uses the same texture upload without a camera slot.

use makepad_story_cut::{CameraDevice, CameraSpec};
use makepad_story_edit_render::compose::ExternalFrame;
use makepad_widgets::makepad_platform::video::{camera_clock_ns, CameraBgraFrame, CameraFrameInputFn, CameraLatest, VideoFormat, VideoFormatId, VideoInputDesc, VideoInputId, VideoPixelFormat};
use makepad_widgets::*;
use std::sync::Arc;

/// What the deck asks of the platform's cameras: the list, the slots and what a slot's frames go to.
/// `Cx` is the real one; a test stands another in.
pub trait CameraPort {
    /// The cameras the platform has listed so far (None: not yet).
    fn listed(&self) -> Option<Vec<VideoInputDesc>>;
    /// Where a slot's frames go; this is also what makes the platform list the cameras the first time.
    fn watch(&mut self, slot: usize, frames: CameraFrameInputFn);
    fn open(&mut self, slot: usize, input: VideoInputId, format: VideoFormatId) -> Result<VideoFormat, String>;
    fn stream(&mut self, slot: usize, on: bool) -> Result<(), String>;
    fn close(&mut self, slot: usize);
    fn release_texture(&mut self, texture: Texture);
}

impl CameraPort for Cx {
    fn listed(&self) -> Option<Vec<VideoInputDesc>> {
        self.video_inputs()
    }
    fn watch(&mut self, slot: usize, frames: CameraFrameInputFn) {
        self.camera_frame_input_box(slot, frames);
    }
    fn open(&mut self, slot: usize, input: VideoInputId, format: VideoFormatId) -> Result<VideoFormat, String> {
        self.video_input_open(slot, input, format)
    }
    fn stream(&mut self, slot: usize, on: bool) -> Result<(), String> {
        self.video_input_stream(slot, on)
    }
    fn close(&mut self, slot: usize) {
        self.video_input_close(slot);
    }
    fn release_texture(&mut self, texture: Texture) {
        texture.release(self);
    }
}

/// Camera list revisions are numbered across every `Cameras` of the process, so the number a reader
/// saw from an earlier deck is never the number of a later one.
fn next_revision() -> u64 {
    static REVISIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    REVISIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// Camera slots the platform offers a deck (current and next slides).
const SLOTS: usize = 8;
/// Frame ages kept for the latency line.
const AGE_SAMPLES: usize = 600;
/// How often a streaming feed logs its latency (seconds).
const REPORT_EVERY: f64 = 10.0;

/// Where a feed is in its life.
#[derive(Clone, Debug)]
enum Stage {
    /// Waiting for the device list, or for the device to appear.
    Waiting,
    /// Open at the format, not streaming.
    Open(VideoFormat),
    Streaming(VideoFormat),
    /// The device could not be used, and why.
    Failed(String),
}

const BGRA_FOURCC: u32 = u32::from_le_bytes(*b"BGRA");

enum FeedSource {
    Camera { slot: usize, latest: Arc<CameraLatest> },
    TestPattern(TestPattern),
}

impl FeedSource {
    fn slot(&self) -> Option<usize> {
        match self {
            Self::Camera { slot, .. } => Some(*slot),
            Self::TestPattern(_) => None,
        }
    }
}

#[derive(Default)]
struct TestPattern {
    frame: CameraBgraFrame,
    last_frame: Option<f64>,
}

impl TestPattern {
    fn next_frame(&mut self, spec: &CameraSpec, now: f64, clock: Option<u64>) -> Option<CameraBgraFrame> {
        if self.last_frame.is_some_and(|last| now - last < 1.0 / spec.fps) {
            return None;
        }
        self.last_frame = Some(now);
        let (width, height) = (spec.width as usize, spec.height as usize);
        let marker_height = (height / 24).max(1);
        let marker_start = (height - marker_height) * width;
        let frame = &mut self.frame;
        if frame.pixels.is_empty() {
            const BARS: [u32; 7] = [0xffbfbfbf, 0xffbfbf00, 0xff00bfbf, 0xff00bf00, 0xffbf00bf, 0xffbf0000, 0xff0000bf];
            frame.width = width;
            frame.height = height;
            frame.pixels.resize(width * height, 0);
            for (y, row) in frame.pixels[..marker_start].chunks_exact_mut(width).enumerate() {
                for (x, pixel) in row.iter_mut().enumerate() {
                    *pixel = if y < height * 2 / 3 {
                        BARS[x * BARS.len() / width]
                    } else {
                        let grey = (255 * x / (width - 1).max(1)) as u32;
                        0xff000000 | (grey * 0x010101)
                    };
                }
            }
        }
        let marker_width = (width / 16).max(1);
        let x = (now.fract() * (width - marker_width + 1) as f64) as usize;
        for row in frame.pixels[marker_start..].chunks_exact_mut(width) {
            row[..x].fill(0xff000000);
            row[x..x + marker_width].fill(0xffffffff);
            row[x + marker_width..].fill(0xff000000);
        }
        frame.timestamp_ns = clock.unwrap_or(0);
        Some(std::mem::take(frame))
    }
}

/// One `Camera` node of one slide.
pub struct Feed {
    slide: usize,
    node: String,
    spec: CameraSpec,
    source: FeedSource,
    stage: Stage,
    /// The device chosen: its name and input.
    device: Option<String>,
    texture: Option<Texture>,
    size: (u32, u32),
    /// Bumped with every new picture.
    serial: u64,
    /// Frame ages (capture to the draw that takes the frame), milliseconds.
    ages: Vec<f64>,
    frames: u64,
    /// A cheap checksum of the last frame taken, and how many frames differed from the one before (a live
    /// sensor differs from frame to frame, even of a still scene).
    checksum: u64,
    differing: u64,
    reported_at: f64,
}

impl Feed {
    fn label(&self) -> String {
        self.device.clone().unwrap_or_else(|| self.spec.device.label())
    }

    /// What the presenter reads while the camera shows nothing.
    fn note(&self) -> String {
        match &self.stage {
            Stage::Waiting => format!("looking for camera {}", self.spec.device.label()),
            Stage::Open(_) => String::new(),
            Stage::Streaming(_) if self.texture.is_none() => format!("camera {} is not delivering frames yet (waiting)", self.label()),
            Stage::Streaming(_) => String::new(),
            Stage::Failed(why) => format!("camera {}: {why}", self.spec.device.label()),
        }
    }

    /// Median and 95th percentile of the frame ages, milliseconds.
    pub fn latency(&self) -> Option<(f64, f64)> {
        if self.ages.is_empty() {
            return None;
        }
        let mut sorted = self.ages.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        Some((sorted[sorted.len() / 2], sorted[((sorted.len() as f64 * 0.95) as usize).min(sorted.len() - 1)]))
    }

    fn upload(&mut self, cx: &mut Cx, frame: &mut CameraBgraFrame, clock: Option<u64>) {
        if let Some(now_ns) = clock.filter(|_| frame.timestamp_ns > 0) {
            if self.ages.len() == AGE_SAMPLES {
                self.ages.remove(0);
            }
            self.ages.push(now_ns.saturating_sub(frame.timestamp_ns) as f64 / 1.0e6);
        }
        self.frames += 1;
        let sum = frame.pixels.iter().step_by(61).fold(0u64, |sum, pixel| sum.wrapping_mul(31).wrapping_add(*pixel as u64));
        if sum != self.checksum {
            self.differing += 1;
            self.checksum = sum;
        }
        let (width, height) = (frame.width, frame.height);
        match &self.texture {
            Some(texture) if self.size == (width as u32, height as u32) => texture.swap_vec_u32(cx, &mut frame.pixels),
            Some(texture) => {
                texture.set_data_u32(cx, width, height, std::mem::take(&mut frame.pixels));
                self.size = (width as u32, height as u32);
            }
            None => {
                // Both buffers need the static picture before the first texture swap.
                let pixels = match self.source {
                    FeedSource::TestPattern(_) => frame.pixels.clone(),
                    FeedSource::Camera { .. } => std::mem::take(&mut frame.pixels),
                };
                self.texture = Some(Texture::new_with_format(
                    cx,
                    TextureFormat::VecBGRAu8_32 { width, height, data: Some(pixels), updated: TextureUpdated::Full },
                ));
                self.size = (width as u32, height as u32);
            }
        }
        self.serial += 1;
    }

    fn stats(&self) -> (u64, u64, u64, Option<String>) {
        match &self.source {
            FeedSource::Camera { latest, .. } => latest.stats(),
            FeedSource::TestPattern(_) => (self.serial, 0, 0, None),
        }
    }

    fn report(&mut self, why: &str) {
        let (received, replaced, failed, last_error) = self.stats();
        match self.latency() {
            Some((median, p95)) => log!(
                "deck camera {} ({why}): {} frames shown, age at draw median {median:.1} ms p95 {p95:.1} ms, {} differ from the frame before; {received} received, {replaced} replaced unseen, {failed} failed{}",
                self.label(),
                self.frames,
                self.differing,
                last_error.map(|error| format!(" ({error})")).unwrap_or_default()
            ),
            None => log!(
                "deck camera {} ({why}): {} frames shown, age unavailable, {} differ from the frame before; {received} received, {replaced} replaced unseen, {failed} failed{}",
                self.label(),
                self.frames,
                self.differing,
                last_error.map(|error| format!(" ({error})")).unwrap_or_default()
            ),
        }
        self.ages.clear();
        self.frames = 0;
        self.differing = 0;
    }
}

pub struct Cameras {
    feeds: Vec<Feed>,
    /// The system's cameras, once the platform has listed them.
    devices: Option<Vec<VideoInputDesc>>,
    in_use: [bool; SLOTS],
    /// Bumped whenever the list of cameras arrives or changes.
    revision: u64,
}

impl Cameras {
    /// The feeds of one deck. The platform lists its cameras once (and again when they change), which
    /// a deck opened later has missed: it starts from the list the platform already has.
    pub fn new(port: &impl CameraPort) -> Cameras {
        let devices = port.listed();
        Cameras { feeds: Vec::new(), devices, in_use: [false; SLOTS], revision: next_revision() }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The system's cameras, as the platform lists them (None: not yet).
    pub fn devices(&self) -> Option<&[VideoInputDesc]> {
        self.devices.as_deref()
    }

    pub fn feeds_of(&self, slide: usize) -> impl Iterator<Item = &Feed> {
        self.feeds.iter().filter(move |feed| feed.slide == slide)
    }

    /// The picture of a `Camera` node, once its first frame is in.
    pub fn external(&self, slide: usize, node: &str) -> Option<ExternalFrame> {
        let feed = self.feeds.iter().find(|feed| feed.slide == slide && feed.node == node)?;
        Some(ExternalFrame { texture: feed.texture.clone()?, size: feed.size, scale: [1.0, 1.0] })
    }

    /// Bumped whenever the node's picture changes.
    pub fn serial(&self, slide: usize, node: &str) -> u64 {
        self.feeds.iter().find(|feed| feed.slide == slide && feed.node == node).map_or(0, |feed| feed.serial)
    }

    /// The presenter's lines about cameras that show nothing.
    pub fn notes(&self, slide: usize) -> Vec<String> {
        self.feeds_of(slide).map(Feed::note).filter(|note| !note.is_empty()).collect()
    }

    /// The platform's list of cameras arrived (or changed): feeds that were
    /// waiting for a device look again.
    pub fn handle_event(&mut self, event: &Event) -> bool {
        let Event::VideoInputs(inputs) = event else { return false };
        self.devices = Some(inputs.descs.clone());
        self.revision = next_revision();
        for feed in &mut self.feeds {
            if matches!(feed.stage, Stage::Waiting | Stage::Failed(_)) {
                feed.stage = Stage::Waiting;
            }
        }
        true
    }

    /// The lifecycle: the `active` slides' cameras stream, the `prepared`
    /// slides' are open and silent, every other slide's are closed.
    /// `declared` are the slides' `Camera` nodes by slide index.
    pub fn reconcile(&mut self, cx: &mut impl CameraPort, declared: &[(usize, Vec<(String, CameraSpec)>)], active: &[usize], prepared: &[usize]) {
        // Teardown: the feeds of slides that are neither shown nor next, and
        // those whose node is gone or changed.
        let mut at = 0;
        while at < self.feeds.len() {
            let feed = &self.feeds[at];
            let wanted = (active.contains(&feed.slide) || prepared.contains(&feed.slide))
                && declared.iter().any(|(slide, nodes)| *slide == feed.slide && nodes.iter().any(|(node, spec)| *node == feed.node && *spec == feed.spec));
            if wanted {
                at += 1;
            } else {
                let mut feed = self.feeds.remove(at);
                self.teardown(cx, &mut feed);
            }
        }
        // Prepare: a feed for each camera node of the shown and next slides.
        for (slide, nodes) in declared {
            if !active.contains(slide) && !prepared.contains(slide) {
                continue;
            }
            for (node, spec) in nodes {
                if self.feeds.iter().any(|feed| feed.slide == *slide && feed.node == *node) {
                    continue;
                }
                let (source, stage) = if spec.device == CameraDevice::TestPattern {
                    let format = VideoFormat {
                        format_id: VideoFormatId::default(),
                        width: spec.width as usize,
                        height: spec.height as usize,
                        frame_rate: Some(spec.fps),
                        pixel_format: VideoPixelFormat::Unsupported(BGRA_FOURCC),
                    };
                    (FeedSource::TestPattern(TestPattern::default()), Stage::Open(format))
                } else {
                    let Some(slot) = self.in_use.iter().position(|used| !used) else {
                        log!("deck camera: no free camera slot for {node} of slide {slide}");
                        continue;
                    };
                    self.in_use[slot] = true;
                    log!("deck camera {}: slide {slide} entered, slot {slot} taken", spec.device.label());
                    let latest = CameraLatest::new();
                    // Installing the frame callback also makes the platform list its cameras.
                    cx.watch(slot, latest.input_fn());
                    (FeedSource::Camera { slot, latest }, Stage::Waiting)
                };
                self.feeds.push(Feed {
                    slide: *slide,
                    node: node.clone(),
                    spec: spec.clone(),
                    source,
                    stage,
                    device: None,
                    texture: None,
                    size: (0, 0),
                    serial: 0,
                    ages: Vec::with_capacity(AGE_SAMPLES),
                    frames: 0,
                    checksum: 0,
                    differing: 0,
                    reported_at: Cx::monotonic_now(),
                });
            }
        }
        // Prepare (open) / activate (stream) / deactivate (stop).
        let devices = self.devices.as_deref();
        for feed in &mut self.feeds {
            let slot = feed.source.slot();
            if let (Stage::Waiting, Some(slot), Some(devices)) = (&feed.stage, slot, devices) {
                Self::open(cx, feed, slot, devices);
            }
            let shown = active.contains(&feed.slide);
            match (&feed.stage, shown) {
                (Stage::Open(format), true) => {
                    let format = *format;
                    match slot.map_or(Ok(()), |slot| cx.stream(slot, true)) {
                        Ok(()) => {
                            feed.reported_at = Cx::monotonic_now();
                            feed.stage = Stage::Streaming(format);
                        }
                        Err(why) => {
                            log!("deck camera {}: stream failed: {why}", feed.label());
                            feed.stage = Stage::Failed(why);
                        }
                    }
                }
                (Stage::Streaming(format), false) => {
                    let format = *format;
                    // Stops before it returns: no frame arrives after this.
                    if let Some(slot) = slot {
                        let _ = cx.stream(slot, false);
                    }
                    feed.report("stopped");
                    feed.stage = Stage::Open(format);
                }
                _ => {}
            }
        }
    }

    /// Find the feed's device in the system's list and open it at the
    /// nearest format to the node's size and rate.
    fn open(cx: &mut impl CameraPort, feed: &mut Feed, slot: usize, devices: &[VideoInputDesc]) {
        let names: Vec<&str> = devices.iter().map(|desc| desc.name.as_str()).collect();
        let Some(at) = feed.spec.device.pick(names) else {
            let why = if devices.is_empty() { "no camera is connected".to_string() } else { format!("no camera matches; connected: {}", devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")) };
            log!("deck camera {}: {why}", feed.spec.device.label());
            feed.stage = Stage::Failed(why);
            return;
        };
        let desc = &devices[at];
        log!("deck camera {}: picked {:?} of {} listed ({})", feed.spec.device.label(), desc.name, devices.len(), devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", "));
        let Some(format) = best_format(&desc.formats, feed.spec.width, feed.spec.height, feed.spec.fps) else {
            let why = format!("{} offers no format a deck can show", desc.name);
            log!("deck camera: {why}");
            feed.stage = Stage::Failed(why);
            return;
        };
        match cx.open(slot, desc.input_id, format.format_id) {
            Ok(opened) => {
                log!("deck camera {}: open at {}x{} {:?} {:?} fps (asked {}x{} @ {})", desc.name, opened.width, opened.height, opened.pixel_format, opened.frame_rate, feed.spec.width, feed.spec.height, feed.spec.fps);
                feed.device = Some(desc.name.clone());
                feed.stage = Stage::Open(opened);
            }
            Err(why) => {
                log!("deck camera {}: open failed: {why}", desc.name);
                feed.stage = Stage::Failed(why);
            }
        }
    }

    fn teardown(&mut self, cx: &mut impl CameraPort, feed: &mut Feed) {
        let slot = feed.source.slot();
        log!("deck camera {}: slide {} left ({:?})", feed.label(), feed.slide, feed.stage);
        if matches!(feed.stage, Stage::Streaming(_)) {
            if let Some(slot) = slot {
                let _ = cx.stream(slot, false);
            }
            feed.report("torn down");
        }
        if let Some(slot) = slot {
            cx.close(slot);
            cx.watch(slot, Box::new(|_| {}));
            self.in_use[slot] = false;
        }
        if let Some(texture) = feed.texture.take() {
            cx.release_texture(texture);
        }
    }

    /// Quit every camera (the page closes, the document changes, Stage
    /// shuts down).
    pub fn shutdown(&mut self, cx: &mut impl CameraPort) {
        for mut feed in std::mem::take(&mut self.feeds) {
            self.teardown(cx, &mut feed);
        }
    }

    /// Take each streaming camera's newest frame into its picture. True when
    /// some picture changed.
    pub fn poll(&mut self, cx: &mut Cx) -> bool {
        let now = Cx::monotonic_now();
        let clock = camera_clock_ns();
        let mut changed = false;
        for feed in &mut self.feeds {
            if !matches!(feed.stage, Stage::Streaming(_)) {
                continue;
            }
            let frame = match &mut feed.source {
                FeedSource::Camera { latest, .. } => latest.take(),
                FeedSource::TestPattern(pattern) => pattern.next_frame(&feed.spec, now, clock),
            };
            if let Some(mut frame) = frame {
                feed.upload(cx, &mut frame, clock);
                match &mut feed.source {
                    FeedSource::Camera { latest, .. } => latest.recycle(frame),
                    FeedSource::TestPattern(pattern) => pattern.frame = frame,
                }
                changed = true;
            }
            if now - feed.reported_at >= REPORT_EVERY {
                feed.reported_at = now;
                feed.report("running");
            }
        }
        changed
    }
}

/// The format of a camera to open for a node asking `width` x `height` at
/// `fps`: the nearest size first, then a rate at or above the one asked
/// for (the lowest such), then the one that needs no JPEG decode.
pub fn best_format(formats: &[VideoFormat], width: u32, height: u32, fps: f64) -> Option<VideoFormat> {
    let usable = |format: &&VideoFormat| matches!(format.pixel_format, VideoPixelFormat::MJPEG | VideoPixelFormat::YUY2 | VideoPixelFormat::NV12 | VideoPixelFormat::YUV420);
    formats
        .iter()
        .filter(usable)
        .min_by(|a, b| format_key(a, width, height, fps).partial_cmp(&format_key(b, width, height, fps)).unwrap_or(std::cmp::Ordering::Equal))
        .copied()
}

fn format_key(format: &VideoFormat, width: u32, height: u32, fps: f64) -> (f64, f64, f64) {
    let size = (format.width as f64 - width as f64).abs() + (format.height as f64 - height as f64).abs();
    let rate = format.frame_rate.unwrap_or(fps);
    // At or above the rate asked for costs the surplus; under it costs
    // more than any surplus a camera offers.
    let rate_cost = if rate + 0.5 >= fps { rate - fps } else { 1000.0 + (fps - rate) };
    let decode = if format.pixel_format == VideoPixelFormat::MJPEG { 1.0 } else { 0.0 };
    (size, rate_cost, decode)
}

/// How a format reads in the picker: `1280x720 · 30 fps · MJPG`.
pub fn format_label(format: &VideoFormat) -> String {
    let pixels = match format.pixel_format {
        VideoPixelFormat::Unsupported(BGRA_FOURCC) => "BGRA",
        VideoPixelFormat::MJPEG => "MJPG",
        VideoPixelFormat::YUY2 => "YUYV",
        VideoPixelFormat::NV12 => "NV12",
        VideoPixelFormat::YUV420 => "I420",
        _ => "?",
    };
    match format.frame_rate {
        Some(fps) => format!("{}x{} · {} fps · {pixels}", format.width, format.height, fps.round()),
        None => format!("{}x{} · {pixels}", format.width, format.height),
    }
}

/// The formats of a camera a deck can show, largest first, fastest first.
pub fn usable_formats(formats: &[VideoFormat]) -> Vec<VideoFormat> {
    let mut usable: Vec<VideoFormat> = formats
        .iter()
        .filter(|format| matches!(format.pixel_format, VideoPixelFormat::MJPEG | VideoPixelFormat::YUY2 | VideoPixelFormat::NV12 | VideoPixelFormat::YUV420))
        .copied()
        .collect();
    usable.sort_by(|a, b| (b.width * b.height).cmp(&(a.width * a.height)).then(b.frame_rate.unwrap_or(0.0).total_cmp(&a.frame_rate.unwrap_or(0.0))));
    usable
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::makepad_platform::video::VideoInputsEvent;

    fn format(width: usize, height: usize, fps: f64, pixel_format: VideoPixelFormat) -> VideoFormat {
        VideoFormat { format_id: VideoFormatId::default(), width, height, frame_rate: Some(fps), pixel_format }
    }

    #[test]
    fn the_obsbot_formats_pick_720p_mjpeg_at_30() {
        let formats = [
            format(1920, 1080, 60.0, VideoPixelFormat::MJPEG),
            format(3840, 2160, 30.0, VideoPixelFormat::MJPEG),
            format(1280, 720, 60.0, VideoPixelFormat::MJPEG),
            format(1280, 720, 30.0, VideoPixelFormat::MJPEG),
            format(640, 360, 30.0, VideoPixelFormat::YUY2),
        ];
        let picked = best_format(&formats, 1280, 720, 30.0).unwrap();
        assert_eq!((picked.width, picked.height, picked.frame_rate), (1280, 720, Some(30.0)));
        let picked = best_format(&formats, 1280, 720, 60.0).unwrap();
        assert_eq!(picked.frame_rate, Some(60.0));
        let small = best_format(&formats, 640, 360, 30.0).unwrap();
        assert_eq!(small.pixel_format, VideoPixelFormat::YUY2, "no decode when the size and rate are the same");
    }

    #[test]
    fn a_rate_the_camera_lacks_takes_the_nearest_above_then_below() {
        let formats = [format(1280, 720, 15.0, VideoPixelFormat::YUY2), format(1280, 720, 60.0, VideoPixelFormat::MJPEG)];
        assert_eq!(best_format(&formats, 1280, 720, 30.0).unwrap().frame_rate, Some(60.0));
        let slow = [format(1280, 720, 15.0, VideoPixelFormat::YUY2), format(1280, 720, 10.0, VideoPixelFormat::YUY2)];
        assert_eq!(best_format(&slow, 1280, 720, 30.0).unwrap().frame_rate, Some(15.0));
    }

    #[test]
    fn formats_read_largest_and_fastest_first() {
        let formats = [format(640, 360, 30.0, VideoPixelFormat::YUY2), format(1920, 1080, 30.0, VideoPixelFormat::MJPEG), format(1920, 1080, 60.0, VideoPixelFormat::MJPEG), format(100, 100, 5.0, VideoPixelFormat::GRAY)];
        let listed: Vec<String> = usable_formats(&formats).iter().map(format_label).collect();
        assert_eq!(listed, ["1920x1080 · 60 fps · MJPG", "1920x1080 · 30 fps · MJPG", "640x360 · 30 fps · YUYV"]);
    }


    /// The platform as the deck sees it: a list of cameras it may not have listed yet, the slots (held,
    /// streaming, a frame callback each) and the one device behind them, which is wanted while any slot
    /// streams and counts every time it is opened and released.
    struct SimPort {
        list: Option<Vec<VideoInputDesc>>,
        held: [Option<(VideoInputId, VideoFormatId)>; SLOTS],
        streaming: [bool; SLOTS],
        callbacks: [Option<CameraFrameInputFn>; SLOTS],
        opens: u32,
        releases: u32,
        log: Vec<String>,
    }

    impl SimPort {
        fn new(list: Option<Vec<VideoInputDesc>>) -> SimPort {
            SimPort { list, held: [None; SLOTS], streaming: [false; SLOTS], callbacks: Default::default(), opens: 0, releases: 0, log: Vec::new() }
        }
        fn wanted(&self) -> bool {
            self.streaming.iter().any(|on| *on)
        }
        fn held_slots(&self) -> usize {
            self.held.iter().flatten().count()
        }
        fn streaming_slots(&self) -> usize {
            self.streaming.iter().filter(|on| **on).count()
        }
        /// The device makes one frame for every slot that streams.
        fn frame(&mut self) {
            let bytes = [16u8, 128, 235, 128, 16, 128, 235, 128];
            for slot in 0..SLOTS {
                if self.streaming[slot] {
                    if let Some(callback) = &mut self.callbacks[slot] {
                        let mut frame = makepad_widgets::makepad_platform::video::CameraFrameRef::empty();
                        frame.width = 2;
                        frame.height = 2;
                        frame.layout = makepad_widgets::makepad_platform::video::CameraFrameLayout::YUY2;
                        frame.plane_count = 1;
                        frame.planes[0] = makepad_widgets::makepad_platform::video::CameraFramePlaneRef { bytes: &bytes, row_stride: 4, pixel_stride: 2 };
                        callback(frame);
                    }
                }
            }
        }
        fn changed(&mut self, before: bool, what: String) {
            self.log.push(what);
            match (before, self.wanted()) {
                (false, true) => self.opens += 1,
                (true, false) => self.releases += 1,
                _ => {}
            }
        }
    }

    impl CameraPort for SimPort {
        fn listed(&self) -> Option<Vec<VideoInputDesc>> {
            self.list.clone()
        }
        fn watch(&mut self, slot: usize, frames: CameraFrameInputFn) {
            self.log.push(format!("watch {slot}"));
            self.callbacks[slot] = Some(frames);
        }
        fn open(&mut self, slot: usize, input: VideoInputId, format: VideoFormatId) -> Result<VideoFormat, String> {
            self.log.push(format!("open {slot}"));
            let list = self.list.as_ref().ok_or("the cameras are not listed")?;
            let found = list.iter().find(|desc| desc.input_id == input).and_then(|desc| desc.formats.iter().find(|f| f.format_id == format)).copied().ok_or("no such camera")?;
            self.held[slot] = Some((input, format));
            Ok(found)
        }
        fn stream(&mut self, slot: usize, on: bool) -> Result<(), String> {
            if self.held[slot].is_none() {
                self.log.push(format!("stream {slot} {on} without open"));
                return Err("no camera is open in this slot".into());
            }
            let before = self.wanted();
            self.streaming[slot] = on;
            self.changed(before, format!("stream {slot} {on}"));
            Ok(())
        }
        fn close(&mut self, slot: usize) {
            let before = self.wanted();
            self.held[slot] = None;
            self.streaming[slot] = false;
            self.changed(before, format!("close {slot}"));
        }
        fn release_texture(&mut self, _texture: Texture) {}
    }

    fn obsbot() -> VideoInputDesc {
        let format = |id: u64, width: usize, height: usize| VideoFormat { format_id: VideoFormatId(LiveId(id)), width, height, frame_rate: Some(30.0), pixel_format: VideoPixelFormat::YUY2 };
        VideoInputDesc { input_id: VideoInputId(LiveId(7)), name: "OBSBOT Meet 2".into(), sensor_orientation: 0, formats: vec![format(1, 1920, 1080), format(2, 1280, 720)] }
    }

    /// The AI hub talk: slides 3 and 5 show the OBSBOT, 4 and 6 do not.
    fn hub_talk() -> Vec<(usize, Vec<(String, CameraSpec)>)> {
        let spec = CameraSpec { device: makepad_story_cut::CameraDevice::Name("OBSBOT".into()), width: 1920, height: 1080, fps: 30.0, ..Default::default() };
        (0..8).map(|slide| (slide, if slide == 3 || slide == 5 { vec![("cam".to_string(), spec.clone())] } else { Vec::new() })).collect()
    }

    fn received(cameras: &Cameras, slide: usize) -> u64 {
        cameras.feeds_of(slide).map(|feed| feed.stats().0).sum()
    }

    #[test]
    fn every_visit_to_the_camera_slide_streams_five_times_in_a_deck_opened_again_each_time() {
        let declared = hub_talk();
        let mut sim = SimPort::new(Some(vec![obsbot()]));
        for round in 0..5u32 {
            // the deck is opened (again): a new player and its own feeds, the platform's list long since given
            let mut cameras = Cameras::new(&sim);
            cameras.reconcile(&mut sim, &declared, &[3], &[]);
            assert!(matches!(cameras.feeds_of(3).next().unwrap().stage, Stage::Streaming(_)), "round {round}: enters streaming");
            sim.frame();
            assert!(received(&cameras, 3) > 0, "round {round}: frames arrive");
            // leave: the device is released and no slot is held
            cameras.reconcile(&mut sim, &declared, &[4], &[]);
            assert!(!sim.wanted() && sim.held_slots() == 0, "round {round}: released on leave");
            // enter again in the same deck
            cameras.reconcile(&mut sim, &declared, &[3], &[]);
            sim.frame();
            assert!(received(&cameras, 3) > 0, "round {round}: streams again");
            // the deck goes (another deck, the page closes)
            cameras.shutdown(&mut sim);
            assert!(!sim.wanted() && sim.held_slots() == 0 && sim.streaming_slots() == 0, "round {round}: nothing held");
        }
        assert_eq!((sim.opens, sim.releases), (10, 10), "{:?}", sim.log);
    }

    #[test]
    fn a_deck_opened_before_the_cameras_are_listed_opens_when_the_list_arrives() {
        let declared = hub_talk();
        let mut sim = SimPort::new(None);
        let mut cameras = Cameras::new(&sim);
        let spec = CameraSpec { device: CameraDevice::TestPattern, width: 14, height: 6, fps: 4.0, ..Default::default() };
        let pattern_slide = [(0, vec![("bars".into(), spec.clone())])];
        cameras.reconcile(&mut sim, &pattern_slide, &[0], &[]);
        let feed = &mut cameras.feeds[0];
        let Stage::Streaming(format) = feed.stage else { panic!("pattern streams before the list arrives") };
        assert_eq!(format_label(&format), "14x6 · 4 fps · BGRA");
        let FeedSource::TestPattern(pattern) = &mut feed.source else { panic!("expected a generated feed") };
        let frame = pattern.next_frame(&spec, 0.0, Some(17)).unwrap();
        assert_eq!((frame.width, frame.height, frame.timestamp_ns), (14, 6, 17));
        let bars: Vec<_> = frame.pixels[..14].iter().step_by(2).copied().collect();
        assert_eq!(bars, [0xffbfbfbf, 0xffbfbf00, 0xff00bfbf, 0xff00bf00, 0xffbf00bf, 0xffbf0000, 0xff0000bf]);
        assert_eq!((frame.pixels[56], frame.pixels[69]), (0xff000000, 0xffffffff), "ramp from black to white");
        assert_eq!(frame.pixels[70], 0xffffffff, "marker starts at the left");
        pattern.frame = frame;
        assert!(pattern.next_frame(&spec, 0.125, None).is_none(), "wait for the requested frame interval");
        let frame = pattern.next_frame(&spec, 0.5, None).unwrap();
        assert_eq!(frame.timestamp_ns, 0);
        assert_eq!((frame.pixels[70], frame.pixels[77]), (0xff000000, 0xffffffff), "marker crosses the band");
        pattern.frame = frame;
        let frame = pattern.next_frame(&spec, 1.0, None).unwrap();
        assert_eq!(frame.pixels[70], 0xffffffff, "marker returns after one second");
        pattern.frame = frame;
        cameras.reconcile(&mut sim, &pattern_slide, &[], &[0]);
        assert!(matches!(cameras.feeds[0].stage, Stage::Open(_)), "prepared pattern stops streaming");
        assert!(cameras.in_use.iter().all(|used| !used), "pattern takes no platform slot");
        cameras.reconcile(&mut sim, &pattern_slide, &[], &[]);
        assert!(cameras.feeds.is_empty(), "pattern leaves with its slide");
        assert!(sim.log.is_empty(), "pattern lifecycle called the port: {:?}", sim.log);

        cameras.reconcile(&mut sim, &declared, &[3], &[]);
        assert!(matches!(cameras.feeds_of(3).next().unwrap().stage, Stage::Waiting));
        assert_eq!(sim.opens, 0);
        sim.list = Some(vec![obsbot()]);
        assert!(cameras.handle_event(&Event::VideoInputs(VideoInputsEvent { descs: vec![obsbot()] })));
        cameras.reconcile(&mut sim, &declared, &[3], &[]);
        sim.frame();
        assert!(received(&cameras, 3) > 0);
        assert_eq!(sim.opens, 1);
    }

    #[test]
    fn a_new_deck_does_not_share_a_revision_with_an_earlier_one() {
        let sim = SimPort::new(Some(vec![obsbot()]));
        let (first, second) = (Cameras::new(&sim), Cameras::new(&sim));
        assert_ne!(first.revision(), second.revision());
        assert_eq!(second.devices().map(|d| d.len()), Some(1), "the list the platform has is at hand");
    }

    #[test]
    fn rapid_current_next_and_away_navigation_ends_in_the_state_of_the_last_step() {
        let declared = hub_talk();
        let mut sim = SimPort::new(Some(vec![obsbot()]));
        let mut cameras = Cameras::new(&sim);
        // (shown, next) at each step, with no frames between
        let steps: [(&[usize], &[usize]); 12] = [
            (&[3], &[4]),
            (&[4], &[3]),
            (&[3], &[4]),
            (&[3], &[]),
            (&[2], &[3]),
            (&[3], &[4]),
            (&[4], &[5]),
            (&[5], &[]),
            (&[3], &[]),
            (&[3, 5], &[]),
            (&[4], &[3]),
            (&[3], &[4]),
        ];
        for (at, (shown, next)) in steps.iter().enumerate() {
            cameras.reconcile(&mut sim, &declared, shown, next);
            let cam = |slide: &usize| *slide == 3 || *slide == 5;
            let streaming = shown.iter().filter(|s| cam(s)).count();
            let held = shown.iter().chain(next.iter()).filter(|s| cam(s)).count();
            assert_eq!(sim.streaming_slots(), streaming, "step {at}: streaming; {:?}", sim.log);
            assert_eq!(sim.held_slots(), held, "step {at}: held; {:?}", sim.log);
            assert_eq!(sim.wanted(), streaming > 0, "step {at}: device");
        }
        sim.frame();
        assert!(received(&cameras, 3) > 0, "the last slide shown streams");
        // the device is opened once per time it is wanted, never twice while wanted
        assert_eq!(sim.opens, sim.releases + 1, "{:?}", sim.log);
        cameras.reconcile(&mut sim, &declared, &[4], &[]);
        assert!(!sim.wanted() && sim.held_slots() == 0);
        assert_eq!(sim.opens, sim.releases);
    }

    #[test]
    fn formats_a_deck_cannot_read_are_not_picked() {
        let formats = [format(1280, 720, 30.0, VideoPixelFormat::GRAY), format(640, 480, 30.0, VideoPixelFormat::Unsupported(7))];
        assert!(best_format(&formats, 1280, 720, 30.0).is_none());
    }
}
