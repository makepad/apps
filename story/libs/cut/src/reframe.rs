//! The automatic camera (EDITS.md §7.2): turns a focus track into crop keys
//! for a `Reframe{mode: @auto}`. Pure: no frames are read, only rectangles.
//!
//! Per frame the solver takes the weighted union of the active focus rects,
//! pads it, keeps it out of the platform's safe-zone bands, grows it to the
//! output aspect and clamps its size between the output size (never
//! upscale) and the whole frame. A critically damped camera follows that
//! goal; it holds while typing or while the target stays in the inner 70%
//! of the framing, turns a jump of more than one crop width into a cut, and
//! never cuts sooner than `min_shot` after the previous cut. Pinned keys are
//! met exactly, blended into the path around them.

use crate::model::{Ease, KeyPoint, Keys};

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn from_array(v: [f64; 4]) -> Self {
        Self::new(v[0], v[1], v[2], v[3])
    }
    pub fn to_array(self) -> [f64; 4] {
        [self.x, self.y, self.w, self.h]
    }
    pub fn centre(&self) -> (f64, f64) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn union(&self, other: &Rect) -> Rect {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Rect::new(x, y, self.right().max(other.right()) - x, self.bottom().max(other.bottom()) - y)
    }
    pub fn contains(&self, other: &Rect) -> bool {
        const SLACK: f64 = 1e-6;
        other.x >= self.x - SLACK && other.y >= self.y - SLACK && other.right() <= self.right() + SLACK && other.bottom() <= self.bottom() + SLACK
    }
    /// The rect scaled by `f` about its centre.
    pub fn scaled(&self, f: f64) -> Rect {
        let (cx, cy) = self.centre();
        Rect::new(cx - self.w * f * 0.5, cy - self.h * f * 0.5, self.w * f, self.h * f)
    }
}

/// One frame of an app run's focus track (§7.1), in source pixels.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FocusSample {
    pub frame: u64,
    pub rects: Vec<(Rect, f64)>,
    /// A `type` call is in progress: the camera holds unless the text
    /// leaves the framing.
    pub typing: bool,
}

/// The bands a platform covers with its own UI, as fractions of the output
/// frame: `top`, `bottom`, `left`, `right`. Focus is kept out of them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SafeBands {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

/// A platform's safe-zone preset.
///
/// Band values (fractions of a 9:16 frame; 1080×1920 pixels in brackets),
/// from the platforms' published creator guidance, rounded outward:
/// - `@reels` (Instagram Reels): top 0.14 (269), bottom 0.35 (672),
///   left 0.06 (65), right 0.06 (65).
/// - `@tiktok`: top 0.08 (154), bottom 0.23 (442), left 0.055 (59),
///   right 0.11 (119, the action buttons).
/// - `@shorts` (YouTube Shorts): top 0.08 (154), bottom 0.20 (384),
///   left 0.05 (54), right 0.13 (140, the action buttons).
/// - `@none`: no bands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SafeZone {
    #[default]
    None,
    Reels,
    TikTok,
    Shorts,
}

impl SafeZone {
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "none" => Self::None,
            "reels" => Self::Reels,
            "tiktok" => Self::TikTok,
            "shorts" => Self::Shorts,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Reels => "reels",
            Self::TikTok => "tiktok",
            Self::Shorts => "shorts",
        }
    }

    pub fn bands(self) -> SafeBands {
        let (top, bottom, left, right) = match self {
            Self::None => (0.0, 0.0, 0.0, 0.0),
            Self::Reels => (0.14, 0.35, 0.06, 0.06),
            Self::TikTok => (0.08, 0.23, 0.055, 0.11),
            Self::Shorts => (0.08, 0.20, 0.05, 0.13),
        };
        SafeBands { top, bottom, left, right }
    }

    /// The part of `crop` a viewer sees unobstructed (what the preview
    /// outlines).
    pub fn safe_rect(self, crop: &Rect) -> Rect {
        let bands = self.bands();
        Rect::new(
            crop.x + crop.w * bands.left,
            crop.y + crop.h * bands.top,
            crop.w * (1.0 - bands.left - bands.right),
            crop.h * (1.0 - bands.top - bands.bottom),
        )
    }
}

/// An author-moved crop key: the solver passes exactly through it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PinnedKey {
    pub frame: u64,
    pub crop: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReframeParams {
    /// Padding around the focus, as a fraction of the crop per side.
    pub pad: f64,
    /// Seconds the camera takes to settle within 2% of a new framing.
    pub settle: f64,
    /// The inner fraction of the framing the target may move in without
    /// the camera following.
    pub hold_inner: f64,
    /// Shortest shot between cuts, in seconds.
    pub min_shot: f64,
    /// Rects weaker than this fraction of the strongest active weight are
    /// left out of the union (explicit focus beats automatic).
    pub weight_floor: f64,
    /// A held framing re-targets when the wanted crop is this factor
    /// larger or smaller (a zoom).
    pub zoom_change: f64,
    /// Key reduction tolerance, in source pixels.
    pub tolerance: f64,
}

impl Default for ReframeParams {
    fn default() -> Self {
        Self { pad: 0.08, settle: 0.4, hold_inner: 0.7, min_shot: 0.8, weight_floor: 0.5, zoom_change: 1.33, tolerance: 1.0 }
    }
}

#[derive(Clone, Debug)]
pub struct ReframeInput<'a> {
    pub track: &'a [FocusSample],
    /// Source frame size in pixels.
    pub source: (f64, f64),
    /// Output size in pixels: its aspect is the crop's aspect, and the crop
    /// is never smaller than it (output pixel ≤ source pixel).
    pub output: (f64, f64),
    pub fps: f64,
    /// Frames to solve; 0 solves through the track's last frame.
    pub frames: u64,
    pub safe: SafeZone,
    pub pinned: &'a [PinnedKey],
    pub params: ReframeParams,
}

/// One crop key, `t` in seconds of the source (frame / fps). `pinned`
/// keys come from `ReframeInput::pinned` and are written with `@pin`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropKey {
    pub t: f64,
    pub crop: Rect,
    pub ease: Ease,
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Solution {
    /// The crop at every frame.
    pub frames: Vec<Rect>,
    /// Frames that begin a new shot (a cut lands on them).
    pub cuts: Vec<u64>,
    /// The reduced keys `crop: keys(...)` is written from.
    pub keys: Vec<CropKey>,
    /// The source is smaller than the output: the crop had to upscale.
    pub upscaled: bool,
}

/// 2% settling of a critically damped spring: (1 + ωt)e^(-ωt) = 0.02.
const SETTLE_OMEGA_T: f64 = 5.834;

/// Size limits of the crop at the output aspect: (min width, max width).
fn width_limits(input: &ReframeInput) -> (f64, f64, bool) {
    let aspect = input.output.0 / input.output.1;
    let (sw, sh) = input.source;
    let max_w = if sw / sh > aspect { sh * aspect } else { sw };
    let min_w = input.output.0.max(input.output.1 * aspect);
    if min_w > max_w { (max_w, max_w, true) } else { (min_w, max_w, false) }
}

/// Place a crop of width `w` so `target`'s centre sits at the centre of the
/// padded safe region, clamped into the source.
fn frame_around(input: &ReframeInput, target: &Rect, w: f64) -> Rect {
    let aspect = input.output.0 / input.output.1;
    let bands = input.safe.bands();
    let h = w / aspect;
    let (cx, cy) = target.centre();
    let x = cx - w * (1.0 + bands.left - bands.right) * 0.5;
    let y = cy - h * (1.0 + bands.top - bands.bottom) * 0.5;
    clamp_into(input, Rect::new(x, y, w, h))
}

fn clamp_into(input: &ReframeInput, rect: Rect) -> Rect {
    let (sw, sh) = input.source;
    Rect::new(rect.x.clamp(0.0, (sw - rect.w).max(0.0)), rect.y.clamp(0.0, (sh - rect.h).max(0.0)), rect.w, rect.h)
}

/// The crop that frames `target`: padding and bands around it, grown to
/// the aspect, within the zoom limits.
pub fn wanted_crop(input: &ReframeInput, target: &Rect) -> Rect {
    let aspect = input.output.0 / input.output.1;
    let bands = input.safe.bands();
    let pad = input.params.pad;
    let usable_x = (1.0 - 2.0 * pad - bands.left - bands.right).max(0.05);
    let usable_y = (1.0 - 2.0 * pad - bands.top - bands.bottom).max(0.05);
    let w = (target.w / usable_x).max(target.h / usable_y * aspect);
    let (min_w, max_w, _) = width_limits(input);
    frame_around(input, target, w.clamp(min_w, max_w))
}

/// The weighted union of one sample's rects, or `None` when it has none.
fn focus_target(sample: &FocusSample, floor: f64) -> Option<Rect> {
    let strongest = sample.rects.iter().map(|(_, weight)| *weight).fold(0.0, f64::max);
    if strongest <= 0.0 {
        return None;
    }
    sample.rects.iter()
        .filter(|(rect, weight)| *weight > 0.0 && *weight >= strongest * floor && rect.w > 0.0 && rect.h > 0.0)
        .map(|(rect, _)| *rect)
        .reduce(|a, b| a.union(&b))
}

/// The region of `crop` the target must stay in for the camera to hold.
fn hold_region(input: &ReframeInput, crop: &Rect, typing: bool) -> Rect {
    let safe = input.safe.safe_rect(crop);
    if typing { safe } else { safe.scaled(input.params.hold_inner) }
}

/// Critically damped camera over (centre x, centre y, width).
#[derive(Clone, Copy)]
struct Camera {
    pos: [f64; 3],
    vel: [f64; 3],
}

impl Camera {
    fn at(rect: &Rect) -> Self {
        let (cx, cy) = rect.centre();
        Self { pos: [cx, cy, rect.w], vel: [0.0; 3] }
    }

    fn step(&mut self, goal: &Rect, omega: f64, dt: f64) {
        let (gx, gy) = goal.centre();
        let goal = [gx, gy, goal.w];
        let decay = (-omega * dt).exp();
        for i in 0..3 {
            // Exact solution of x'' = -2ωx' - ω²(x - g) over one frame.
            let y = self.pos[i] - goal[i];
            let a = self.vel[i] + omega * y;
            self.pos[i] = goal[i] + (y + a * dt) * decay;
            self.vel[i] = (self.vel[i] - omega * a * dt) * decay;
        }
    }

    fn rect(&self, aspect: f64) -> Rect {
        let [cx, cy, w] = self.pos;
        let h = w / aspect;
        Rect::new(cx - w * 0.5, cy - h * 0.5, w, h)
    }
}

/// Why `solve` refused its input.
#[derive(Clone, Debug, PartialEq)]
pub enum ReframeError {
    /// `fps` is not a positive finite number.
    Fps(f64),
    /// The source or output size is not positive and finite.
    Size { source: (f64, f64), output: (f64, f64) },
    /// A parameter is not finite, or out of its range.
    Param(&'static str),
}

impl std::fmt::Display for ReframeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fps(fps) => write!(f, "fps {fps} is not a positive rate"),
            Self::Size { source, output } => write!(f, "source {source:?} or output {output:?} is not a positive size"),
            Self::Param(name) => write!(f, "reframe parameter `{name}` is out of range"),
        }
    }
}

fn validate(input: &ReframeInput) -> Result<(), ReframeError> {
    let positive = |v: f64| v.is_finite() && v > 0.0;
    if !positive(input.fps) {
        return Err(ReframeError::Fps(input.fps));
    }
    if ![input.source.0, input.source.1, input.output.0, input.output.1].into_iter().all(positive) {
        return Err(ReframeError::Size { source: input.source, output: input.output });
    }
    let p = &input.params;
    for (name, value, ok) in [
        ("pad", p.pad, (0.0..0.5).contains(&p.pad)),
        ("settle", p.settle, p.settle >= 0.0),
        ("hold_inner", p.hold_inner, (0.0..=1.0).contains(&p.hold_inner)),
        ("min_shot", p.min_shot, p.min_shot >= 0.0),
        ("weight_floor", p.weight_floor, (0.0..=1.0).contains(&p.weight_floor)),
        ("zoom_change", p.zoom_change, p.zoom_change >= 1.0),
        ("tolerance", p.tolerance, p.tolerance >= 0.0),
    ] {
        if !(value.is_finite() && ok) {
            return Err(ReframeError::Param(name));
        }
    }
    Ok(())
}

pub fn solve(input: &ReframeInput) -> Result<Solution, ReframeError> {
    validate(input)?;
    let params = input.params;
    let aspect = input.output.0 / input.output.1;
    let (min_w, max_w, upscaled) = width_limits(input);
    let frames = if input.frames > 0 {
        input.frames
    } else {
        input.track.iter().map(|sample| sample.frame.saturating_add(1)).max().unwrap_or(0)
    };
    let dt = 1.0 / input.fps;
    let omega = SETTLE_OMEGA_T / params.settle.max(dt);
    let min_shot_frames = (params.min_shot * input.fps).ceil() as u64;
    let wide = frame_around(input, &Rect::new(0.0, 0.0, input.source.0, input.source.1), max_w);

    let mut track = input.track.iter().collect::<Vec<_>>();
    track.sort_by_key(|sample| sample.frame);
    let mut next_sample = 0;
    let mut sample: Option<&FocusSample> = None;

    let mut out = Vec::new();
    let mut cuts = Vec::new();
    let mut goal: Option<Rect> = None;
    let mut camera = Camera::at(&wide);
    let mut shot_start = 0u64;
    let mut pending_cut: Option<Rect> = None;
    for frame in 0..frames {
        while next_sample < track.len() && track[next_sample].frame <= frame {
            sample = Some(track[next_sample]);
            next_sample += 1;
        }
        let target = sample.and_then(|sample| focus_target(sample, params.weight_floor));
        let typing = sample.is_some_and(|sample| sample.typing);
        match (goal, target) {
            (None, target) => {
                // The first framing is placed, not flown to.
                let first = target.map_or(wide, |target| wanted_crop(input, &target));
                goal = Some(first);
                camera = Camera::at(&first);
            }
            (Some(current), Some(target)) => {
                let wanted = wanted_crop(input, &target);
                let zoomed = wanted.w > current.w * params.zoom_change || wanted.w * params.zoom_change < current.w;
                let held = hold_region(input, &current, typing).contains(&target) && (typing || !zoomed);
                if held {
                    pending_cut = None;
                } else {
                    let view = camera.rect(aspect);
                    let (vx, vy) = view.centre();
                    let (wx, wy) = wanted.centre();
                    if (wx - vx).hypot(wy - vy) > view.w {
                        pending_cut = Some(wanted);
                    } else {
                        pending_cut = None;
                        goal = Some(wanted);
                    }
                }
            }
            (Some(_), None) => {}
        }
        if let Some(cut_to) = pending_cut {
            // A jump: cut once the running shot is long enough; until then
            // the camera stays where it is.
            if frame >= shot_start + min_shot_frames || frame == 0 {
                camera = Camera::at(&cut_to);
                goal = Some(cut_to);
                if frame > 0 {
                    cuts.push(frame);
                }
                shot_start = frame;
                pending_cut = None;
            }
        } else if let Some(goal) = goal {
            if frame > 0 {
                camera.step(&goal, omega, dt);
            }
        }
        let mut rect = camera.rect(aspect);
        rect.w = rect.w.clamp(min_w, max_w);
        rect.h = rect.w / aspect;
        out.push(clamp_into(input, rect));
    }

    apply_pins(input, &mut out, &mut cuts, min_w, max_w, min_shot_frames);
    let pinned = input.pinned.iter().map(|pin| pin.frame).collect::<Vec<_>>();
    let keys = reduce_keeping(&out, &cuts, &pinned, input.fps, params.tolerance);
    Ok(Solution { frames: out, cuts, keys, upscaled })
}

/// Meet every pinned key exactly: the difference is blended in and out over
/// one settle time, never across a cut. A pin far from the solved framing
/// (more than one crop width) is reached by a cut instead, unless that cut
/// would make a shot shorter than `min_shot`: then it is blended too.
fn apply_pins(input: &ReframeInput, frames: &mut [Rect], cuts: &mut Vec<u64>, min_w: f64, max_w: f64, min_shot: u64) {
    let aspect = input.output.0 / input.output.1;
    let window = (input.params.settle * input.fps).round().max(1.0) as i64;
    let mut pins = input.pinned.iter().filter(|pin| (pin.frame as usize) < frames.len()).copied().collect::<Vec<_>>();
    pins.sort_by_key(|pin| pin.frame);
    for (index, pin) in pins.iter().enumerate() {
        let at = pin.frame as usize;
        let solved = frames[at];
        let far = {
            let (a, b) = (solved.centre(), pin.crop.centre());
            (a.0 - b.0).hypot(a.1 - b.1) > solved.w
        };
        let crowded = pin.frame < min_shot
            || pin.frame + min_shot > frames.len() as u64
            || cuts.iter().any(|&cut| cut != pin.frame && cut.abs_diff(pin.frame) < min_shot);
        if far && !crowded && !cuts.contains(&pin.frame) {
            cuts.push(pin.frame);
            cuts.sort();
        }
        let delta = [pin.crop.x - solved.x, pin.crop.y - solved.y, pin.crop.w - solved.w];
        // Neighbouring pins bound the window, and so do cuts.
        let lo = index.checked_sub(1).map_or(0, |prev| pins[prev].frame as i64 + 1);
        let hi = pins.get(index + 1).map_or(frames.len() as i64 - 1, |next| next.frame as i64 - 1);
        for offset in -window..=window {
            let frame = pin.frame as i64 + offset;
            if frame < lo || frame > hi || frame < 0 || frame >= frames.len() as i64 {
                continue;
            }
            let crosses = cuts.iter().any(|&cut| {
                let cut = cut as i64;
                if offset < 0 { cut > frame && cut <= pin.frame as i64 } else { cut > pin.frame as i64 && cut <= frame }
            });
            if crosses {
                continue;
            }
            let u = 1.0 - offset.unsigned_abs() as f64 / (window + 1) as f64;
            let weight = u * u * (3.0 - 2.0 * u);
            let rect = &mut frames[frame as usize];
            if offset == 0 {
                *rect = pin.crop;
                continue;
            }
            let w = (rect.w + delta[2] * weight).clamp(min_w, max_w);
            let moved = Rect::new(rect.x + delta[0] * weight, rect.y + delta[1] * weight, w, w / aspect);
            *rect = clamp_into(input, moved);
        }
    }
}

/// Reduce per-frame crops to keys: shots split at cuts (a cut key holds the
/// previous framing until it, `@hold`), and within a shot the fewest linear
/// keys that stay within `tolerance` pixels (Ramer–Douglas–Peucker).
pub fn reduce(frames: &[Rect], cuts: &[u64], fps: f64, tolerance: f64) -> Vec<CropKey> {
    reduce_keeping(frames, cuts, &[], fps, tolerance)
}

/// `reduce`, always keeping a key on each of the `keep_frames` (pins); those
/// keys are `pinned`.
pub fn reduce_keeping(frames: &[Rect], cuts: &[u64], keep_frames: &[u64], fps: f64, tolerance: f64) -> Vec<CropKey> {
    let mut keys = Vec::new();
    let mut bounds = vec![0usize];
    bounds.extend(cuts.iter().map(|&cut| cut as usize).filter(|&cut| cut > 0 && cut < frames.len()));
    bounds.push(frames.len());
    for (shot, pair) in bounds.windows(2).enumerate() {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        let mut keep = vec![false; end - start];
        keep[0] = true;
        keep[end - start - 1] = true;
        for &frame in keep_frames {
            if (start..end).contains(&(frame as usize)) {
                keep[frame as usize - start] = true;
            }
        }
        // Simplify between every pair of forced keys.
        let forced = keep.iter().enumerate().filter(|(_, keep)| **keep).map(|(index, _)| index).collect::<Vec<_>>();
        for pair in forced.windows(2) {
            simplify(&frames[start..end], pair[0], pair[1], tolerance, &mut keep);
        }
        for (offset, _) in keep.iter().enumerate().filter(|(_, keep)| **keep) {
            let frame = start + offset;
            let ease = if shot > 0 && offset == 0 { Ease::Hold } else { Ease::Linear };
            let pinned = keep_frames.contains(&(frame as u64));
            keys.push(CropKey { t: frame as f64 / fps, crop: frames[frame], ease, pinned });
        }
    }
    keys
}

/// Ramer–Douglas–Peucker with an explicit work stack: a long monotone
/// settle can split next to an end every time, which would recurse once per
/// frame.
fn simplify(frames: &[Rect], first: usize, last: usize, tolerance: f64, keep: &mut [bool]) {
    let mut work = vec![(first, last)];
    while let Some((first, last)) = work.pop() {
        if last <= first + 1 {
            continue;
        }
        let (a, b) = (frames[first].to_array(), frames[last].to_array());
        let (mut worst, mut worst_at) = (0.0, first);
        for index in first + 1..last {
            let u = (index - first) as f64 / (last - first) as f64;
            let p = frames[index].to_array();
            let error = (0..4).map(|i| (p[i] - (a[i] + (b[i] - a[i]) * u)).abs()).fold(0.0, f64::max);
            if error > worst {
                worst = error;
                worst_at = index;
            }
        }
        if worst > tolerance {
            keep[worst_at] = true;
            work.push((first, worst_at));
            work.push((worst_at, last));
        }
    }
}

/// Key times from source seconds to seconds since the Reframe's span start,
/// for a clip that shows source time `source_in` at its span start and runs
/// at `speed`. Keys before the span keep only their last one.
pub fn retime(keys: &[CropKey], source_in: f64, speed: f64) -> Vec<CropKey> {
    let mapped = keys.iter().map(|key| CropKey { t: (key.t - source_in) / speed, ..*key }).collect::<Vec<_>>();
    let first_inside = mapped.iter().position(|key| key.t >= 0.0).unwrap_or(mapped.len());
    mapped[first_inside.saturating_sub(1)..].to_vec()
}

/// The keys as the model's `Keys<4>` (`[x, y, w, h]`).
pub fn to_keys(keys: &[CropKey]) -> Keys<4> {
    Keys { points: keys.iter().map(|key| KeyPoint { t: key.t, v: key.crop.to_array(), ease: key.ease, pinned: key.pinned }).collect() }
}

/// Splash for `crop:`: `keys([t, [x, y, w, h]], [t, [...], @hold, @pin], ...)`,
/// whole pixels and microsecond times.
pub fn keys_splash(keys: &[CropKey]) -> String {
    let mut out = String::from("keys(");
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        // Width rounds up (never below the solved size, so never a
        // sub-pixel upscale) and height follows the crop's own aspect.
        let crop = key.crop;
        let (x, y, w) = (crop.x.round(), crop.y.round(), crop.w.ceil());
        let h = (w * crop.h / crop.w).round();
        out.push_str(&format!("[{}, [{x}, {y}, {w}, {h}]", fmt_time(key.t, key.ease == Ease::Hold)));
        if key.ease != Ease::Linear {
            out.push_str(", @");
            out.push_str(key.ease.id());
        }
        if key.pinned {
            out.push_str(", @pin");
        }
        out.push(']');
    }
    out.push(')');
    out
}

/// Microseconds: a key lands on its own frame at any frame rate. A `@hold`
/// key (a cut) rounds down so the frame it starts on already shows it.
fn fmt_time(t: f64, floor: bool) -> String {
    let t = if floor { (t * 1e6 + 1e-6).floor() / 1e6 } else { t };
    let text = format!("{:.6}", t);
    let text = text.trim_end_matches('0');
    if text.ends_with('.') { format!("{text}0") } else { text.to_string() }
}
