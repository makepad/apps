//! The CUT page's timeline (design §4.5): a projection of the
//! open cut drawn as an audio-style display on the `wave_*` roles.
//!
//! Top to bottom: a 28 px timecode ruler (tick ladder, dimmed zero groups,
//! render bar, in/out, markers, playhead handle, skim bubble), then CC,
//! video tracks from the highest down to V1 (MAIN) with an Fx rail above
//! any track that has an effect, a 4 px gap, the audio tracks and the
//! voiceover track. Every track has a 116 px header with its role bar,
//! name and eye/lock or M/S/lock chips (editor-session state).
//!
//! Clips are drawn by role (design §2.1, [`CutRole`]): a wash body, a
//! 16 px name strip, a 1 px role edge, a filmstrip sampled across the
//! source, a linked audio band with the real waveform, badges (STALE, FX,
//! REMOVED, APP RUN, speed), fades as ramps with round handles, ducking as
//! a dashed level with a DUCK chip. Transitions are neutral tabs on the
//! edit point; the selection is an accent outline outside the body.
//!
//! The widget owns only gesture and session state. A click selects, a drag
//! on the ruler or an empty track scrubs, a drag of a block moves it (to
//! another track too), of either end trims it, of a fade handle fades it;
//! each gesture sends one action when released and the parent answers with
//! the next projection. Moves and trims snap to edges, the playhead,
//! markers and the in/out points; the snap is drawn and named.

use super::clip_films::ClipFilms;
use super::ops::Lane;
use makepad_library_browser::draw::{wave_height, CutRole, DrawEditBlock, DrawEditPicture, Filmstrip, Peaks};
use super::project::{Block, BlockKind, Projection};
use makepad_widgets::badge::advance;
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;

const RULER: f64 = 28.0;
const HEADER: f64 = 116.0;
const GAP: f64 = 4.0;
const STRIP: f64 = 16.0;
const LINKED_AUDIO: f64 = 16.0;
const EDGE_GRAB: f64 = 8.0;
const MIN_PX_PER_SEC: f64 = 2.0;
const MAX_PX_PER_SEC: f64 = 1200.0;
const SNAP_PX: f64 = 8.0;
const FADE_GRAB: f64 = 7.0;
/// The empty track above the picture and below the sound, to drop onto.
const EMPTY_ROW: f64 = 24.0;

/// A block's clip role colours (design §2.1).
pub fn role_of(block: &Block) -> CutRole {
    if block.broken {
        return CutRole::BROKEN;
    }
    match block.kind {
        BlockKind::Clip if block.app_run => CutRole::APP_RUN,
        BlockKind::Clip | BlockKind::Still => CutRole::VIDEO,
        BlockKind::Title => CutRole::TITLE,
        BlockKind::Effect => CutRole::EFFECT,
        BlockKind::Audio => CutRole::AUDIO,
        BlockKind::Voice if block.stale => CutRole::STALE,
        BlockKind::Voice => CutRole::VOICE,
        BlockKind::Caption => CutRole::CAPTION,
    }
}

pub fn role_of_lane(lane: Lane) -> CutRole {
    match lane {
        Lane::Video => CutRole::VIDEO,
        Lane::Audio => CutRole::AUDIO,
        Lane::Voice => CutRole::VOICE,
        Lane::Captions => CutRole::CAPTION,
    }
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.CutTimelineBase = #(CutTimeline::register_widget(vm))
    mod.widgets.CutTimeline = set_type_default() do mod.widgets.CutTimelineBase{
        width: Fill height: Fill
        draw_bg +: {color: stage.wave_bg}
        draw_rect +: {color: stage.wave_rim}
        draw_text +: {color: stage.wave_label text_style: theme.font_regular{font_size: 7.5}}
        draw_name +: {color: stage.glass_ink text_style: theme.font_bold{font_size: 8}}
        draw_track +: {color: stage.wave_label text_style: theme.font_bold{font_size: 8}}
        draw_badge +: {color: stage.on_accent text_style: theme.font_bold{font_size: 7}}
        icon_eye +: {svg: crate_resource("self:resources/icons/edit/eye.svg")}
        icon_eye_off +: {svg: crate_resource("self:resources/icons/edit/eye_off.svg")}
        icon_lock +: {svg: crate_resource("self:resources/icons/edit/lock.svg")}
        icon_lock_open +: {svg: crate_resource("self:resources/icons/edit/lock_open.svg")}
        icon_xfade +: {svg: crate_resource("self:resources/icons/ui/xfade.svg")}
        icon_image +: {svg: crate_resource("self:resources/icons/image.svg")}
        icon_app +: {svg: crate_resource("self:resources/icons/edit/app_run.svg")}
        icon_title +: {svg: crate_resource("self:resources/icons/edit/title.svg")}
        icon_marker +: {svg: crate_resource("self:resources/icons/edit/marker.svg")}
        icon_edit +: {svg: crate_resource("self:resources/icons/edit/timeline.svg")}
    }
}

#[derive(Clone, Debug, Default)]
pub enum TimelineAction {
    #[default]
    None,
    /// A click: the block under it (or none) and the time it was at.
    Select(Option<String>, f64),
    Scrub(f64),
    /// The pointer skims the tracks at this time (`None`: it left them).
    Skim(Option<f64>),
    Move { node: String, start: f64, track: u32 },
    Trim { node: String, start: f64, length: f64 },
    /// An audio block's fade handles, dragged.
    Fade { node: String, fade_in: f64, fade_out: f64 },
    Key(KeyEvent),
    /// The zoom changed from the timeline itself (cmd-scroll).
    Zoomed(f64),
    /// What the last drag snapped to, for the status line.
    Snapped(String),
    /// A marquee's blocks.
    SelectMany(Vec<String>),
    /// A double click on a compound clip: open its cut.
    Open(String),
    /// Text pasted while the timeline has the keys (⌘V): a web address
    /// becomes a grab (WB1).
    Paste(String),
}

/// Where a point of the timeline is: its time, row and what is under it.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineHit {
    pub t: f64,
    pub lane: Lane,
    pub track: u32,
    pub block: Option<String>,
}

/// One segment of the generate bar under the ruler.
#[derive(Clone, Debug, PartialEq)]
pub enum BarState {
    NeedsGeneration,
    /// 0..1
    Generating(f64),
    Generated,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarSegment {
    pub t0: f64,
    pub t1: f64,
    pub state: BarState,
}

/// Generative segments' hue (violet).
const GENERATIVE: u32 = 0xa77bffff;

/// A library item's landing place while it is dragged over the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct DropHint {
    pub lane: Lane,
    pub track: u32,
    pub start: f64,
    pub length: f64,
    /// Over a block (an effect) or two (a transition): outline those.
    pub targets: Vec<String>,
    pub label: String,
    /// Refused here, and why.
    pub invalid: Option<String>,
    /// A transition: drawn as its tab on the cut point.
    pub tab: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    Ruler,
    Body,
    Start,
    End,
    FadeIn,
    FadeOut,
    /// A drag on empty track space: a selection rectangle.
    Marquee,
}

#[derive(Clone, Debug)]
struct Drag {
    grab: Grab,
    node: Option<String>,
    origin: Vec2d,
    /// Block span (or fades) and track when the drag began.
    span: (f64, f64),
    lane: Lane,
    track: u32,
    moved: bool,
    /// The marquee's other corner.
    corner: Vec2d,
    /// Let go with an action sent: drawn at its landing place until the
    /// edit answers.
    released: bool,
    /// The preview: span start, length and track.
    preview: Option<(f64, f64, u32)>,
}

/// A track's editor-session state (design §4.5 track header).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackState {
    pub hidden: bool,
    pub locked: bool,
    pub muted: bool,
    pub solo: bool,
}

/// Track heights (design §2.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrackHeight {
    Small,
    #[default]
    Medium,
    Large,
}

impl TrackHeight {
    fn of(self, kind: RowKind) -> f64 {
        let (s, m, l) = match kind {
            RowKind::Captions => (22.0, 24.0, 28.0),
            RowKind::Video(_) => (40.0, 64.0, 96.0),
            RowKind::FxRail(_) => (12.0, 14.0, 16.0),
            RowKind::Gap => (GAP, GAP, GAP),
            RowKind::Audio(_) => (36.0, 48.0, 64.0),
            RowKind::Voice(_) => (36.0, 44.0, 56.0),
        };
        match self {
            Self::Small => s,
            Self::Medium => m,
            Self::Large => l,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Small => Self::Medium,
            Self::Medium => Self::Large,
            Self::Large => Self::Small,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RowKind {
    Captions,
    Video(u32),
    FxRail(u32),
    Gap,
    Audio(u32),
    Voice(u32),
}

impl RowKind {
    fn lane_track(self) -> Option<(Lane, u32)> {
        match self {
            Self::Captions => Some((Lane::Captions, 0)),
            Self::Video(track) => Some((Lane::Video, track)),
            Self::Audio(track) => Some((Lane::Audio, track)),
            Self::Voice(track) => Some((Lane::Voice, track)),
            Self::FxRail(_) | Self::Gap => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Row {
    kind: RowKind,
    top: f64,
    h: f64,
}

#[derive(Script, ScriptHook, Widget)]
pub struct CutTimeline {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_rect: DrawColor,
    #[live]
    draw_block: DrawEditBlock,
    #[live]
    draw_picture: DrawEditPicture,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_name: DrawText,
    #[live]
    draw_track: DrawText,
    #[live]
    draw_badge: DrawText,
    #[live]
    icon_eye: DrawSvg,
    #[live]
    icon_eye_off: DrawSvg,
    #[live]
    icon_lock: DrawSvg,
    #[live]
    icon_lock_open: DrawSvg,
    #[live]
    icon_xfade: DrawSvg,
    #[live]
    icon_image: DrawSvg,
    #[live]
    icon_app: DrawSvg,
    #[live]
    icon_title: DrawSvg,
    #[live]
    icon_marker: DrawSvg,
    #[live]
    icon_edit: DrawSvg,
    #[rust]
    projection: Projection,
    /// Filmstrips by asset id.
    #[rust]
    films: HashMap<String, Filmstrip>,
    /// Graphics' and nested cuts' own rendered strips (`clip_films.rs`).
    #[rust]
    pub clip_films: ClipFilms,
    #[rust]
    peaks: HashMap<String, Arc<Peaks>>,
    #[rust]
    selected: Option<String>,
    /// More selected blocks (a marquee).
    #[rust]
    also: Vec<String>,
    /// The selection a marquee started from, and 0 replace / 1 add / 2 toggle.
    #[rust]
    marquee: (Vec<String>, u8),
    /// A keyboard zoom easing in (design §2.5): from, to, anchor time and
    /// x, and when it began.
    #[rust]
    zoom_anim: Option<(f64, f64, f64, f64, f64)>,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    hovered: Option<(String, Grab)>,
    #[rust]
    playhead: f64,
    #[rust]
    skim: Option<f64>,
    #[rust]
    marks: (Option<f64>, Option<f64>),
    #[rust]
    markers: Vec<f64>,
    /// A render of the saved cut: `Some(true)` fresh, `Some(false)` stale.
    #[rust]
    render_bar: Option<bool>,
    /// The render cache's segments, when it has planned them.
    #[rust]
    render_segments: Vec<BarSegment>,
    #[rust(60.0)]
    fps: f64,
    #[rust(40.0)]
    px_per_sec: f64,
    /// Seconds at the left edge of the track area.
    #[rust]
    scroll: f64,
    /// Points scrolled down through the tracks.
    #[rust]
    scroll_y: f64,
    #[rust]
    drag: Option<Drag>,
    /// The time the drag snapped to, drawn while it holds.
    #[rust]
    snapped: Option<f64>,
    #[rust]
    snap_name: Option<String>,
    #[rust]
    drop_hint: Option<DropHint>,
    /// A fade drag's preview: the block and its fades.
    #[rust]
    fade_preview: Option<(String, f64, f64)>,
    #[rust]
    tracks: HashMap<(u8, u32), TrackState>,
    #[rust]
    height: TrackHeight,
    #[rust]
    snapping: bool,
    #[rust]
    skimming: bool,
    #[rust]
    rect: Rect,
    /// Fit the whole edit on the next draw, once the width is known.
    #[rust]
    fit_pending: bool,
    #[rust]
    pending_scroll: Option<f64>,
    /// The playhead the view last paged for.
    #[rust]
    followed: Option<f64>,
    #[rust]
    initialized: bool,
    /// No cut open: only the ground.
    #[rust]
    blank: bool,
    #[rust]
    later_tags: Vec<(Vec2d, String, u32, u32)>,
    #[rust]
    pointer: Option<Vec2d>,
    #[rust(1.0)]
    film_alpha: f32,
}

fn lane_key(lane: Lane) -> u8 {
    match lane {
        Lane::Video => 0,
        Lane::Audio => 1,
        Lane::Voice => 2,
        Lane::Captions => 3,
    }
}

/// `HH:MM:SS:FF` at `fps`: every time on the page.
pub fn timecode_full(t: f64, fps: f64) -> String {
    let fps = fps.max(1.0);
    let frames = (t.max(0.0) * fps + 1e-6).floor() as u64;
    let per_second = fps.round().max(1.0) as u64;
    let (ff, seconds) = (frames % per_second, frames / per_second);
    let (hh, mm, ss) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    format!("{hh:02}:{mm:02}:{ss:02}:{ff:02}")
}

/// A timecode split into its leading zero groups (dimmed on the ruler) and
/// its significant tail.
pub fn timecode_parts(t: f64, fps: f64) -> (String, String) {
    let full = timecode_full(t, fps);
    let groups: Vec<&str> = full.split(':').collect();
    let lead = groups.iter().take(3).take_while(|group| **group == "00").count();
    let lead = lead.min(3);
    let head = groups[..lead].iter().map(|group| format!("{group}:")).collect::<String>();
    let tail = groups[lead..].join(":");
    (head, tail)
}

/// The ruler's major and minor steps (design §4.5), in seconds.
pub fn tick_steps(px_per_sec: f64, fps: f64) -> (f64, f64) {
    let f = 1.0 / fps.max(1.0);
    let ladder = [f, 2.0 * f, 5.0 * f, 10.0 * f, 15.0 * f, 30.0 * f, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0];
    let major = ladder.iter().copied().find(|step| step * px_per_sec >= 96.0).unwrap_or(3600.0);
    let minor = ladder.iter().copied().filter(|step| *step < major)
        .filter(|step| {
            let n = major / step;
            (n - n.round()).abs() < 1e-6 && n <= 10.0 + 1e-9 && step * px_per_sec >= 8.0
        })
        .next()
        .unwrap_or(major);
    (major, minor)
}

impl CutTimeline {
    fn x_of(&self, t: f64) -> f64 {
        self.rect.pos.x + HEADER + (t - self.scroll) * self.px_per_sec
    }

    fn t_of(&self, x: f64) -> f64 {
        (x - self.rect.pos.x - HEADER) / self.px_per_sec + self.scroll
    }

    fn rows(&self) -> Vec<Row> {
        let p = &self.projection;
        let fx = p.fx_tracks();
        let mut kinds = Vec::new();
        if p.caption_tracks > 0 {
            kinds.push(RowKind::Captions);
        }
        for track in (0..p.video_tracks).rev() {
            if fx.contains(&track) {
                kinds.push(RowKind::FxRail(track));
            }
            kinds.push(RowKind::Video(track));
        }
        kinds.push(RowKind::Gap);
        for track in 0..p.audio_tracks {
            kinds.push(RowKind::Audio(track));
        }
        for track in 0..p.voice_tracks {
            kinds.push(RowKind::Voice(track));
        }
        let mut top = self.rect.pos.y + RULER - self.scroll_y;
        kinds.into_iter().map(|kind| {
            // A track of titles only is a title track; an empty track (the
            // one to drop onto) is drawn small.
            let on = |lane: Lane, track: u32| p.blocks.iter().filter(move |block| block.lane == lane && block.track == track);
            let h = match kind {
                _ if p.blocks.is_empty() => self.height.of(kind),
                _ if self.is_drop_row(kind) => EMPTY_ROW,
                RowKind::Video(track) if on(Lane::Video, track).all(|block| block.kind == BlockKind::Title && !block.graphic) => match self.height {
                    TrackHeight::Small => 28.0,
                    TrackHeight::Medium => 36.0,
                    TrackHeight::Large => 44.0,
                },
                // One row per effect level on the rail.
                RowKind::FxRail(track) => self.height.of(kind) * p.fx_levels(track).max(1) as f64,
                _ => self.height.of(kind),
            };
            let row = Row { kind, top, h };
            top += h;
            row
        }).collect()
    }

    /// The empty track above the picture or below the sound, there to drop
    /// onto: no name, no buttons.
    fn is_drop_row(&self, kind: RowKind) -> bool {
        let p = &self.projection;
        let empty = |lane: Lane, track: u32| !p.blocks.iter().any(|block| block.lane == lane && block.track == track) && !p.bands.iter().any(|band| lane == Lane::Video && band.tracks.1 == track);
        !p.blocks.is_empty() && match kind {
            RowKind::Video(track) => track + 1 == p.video_tracks && track > 0 && empty(Lane::Video, track),
            RowKind::Audio(track) => track + 1 == p.audio_tracks && track > 0 && empty(Lane::Audio, track),
            _ => false,
        }
    }

    fn row_of(&self, lane: Lane, track: u32) -> Option<Row> {
        self.rows().into_iter().find(|row| row.kind.lane_track() == Some((lane, track)))
    }

    fn row_at(&self, y: f64) -> Option<Row> {
        self.rows().into_iter().find(|row| y >= row.top && y < row.top + row.h)
    }

    fn block_rect(&self, lane: Lane, track: u32, start: f64, length: f64) -> Rect {
        let Some(row) = self.row_of(lane, track) else { return Rect::default() };
        let (x0, x1) = (self.x_of(start), self.x_of(start + length));
        // A 1 px gap between adjacent clips: the cut point shows.
        Rect { pos: dvec2(x0 + 0.5, row.top + 2.0), size: dvec2((x1 - x0 - 1.0).max(3.0), row.h - 4.0) }
    }

    /// An effect's band in the Fx rail above its clip's track.
    /// `level` 0 (next to the clip) is the rail's bottom row.
    fn band_rect(&self, track: u32, level: u32, start: f64, length: f64) -> Rect {
        let Some(row) = self.rows().into_iter().find(|row| row.kind == RowKind::FxRail(track)) else { return Rect::default() };
        let levels = self.projection.fx_levels(track).max(1);
        let h = row.h / levels as f64;
        let top = row.top + (levels - 1 - level.min(levels - 1)) as f64 * h;
        let (x0, x1) = (self.x_of(start), self.x_of(start + length));
        Rect { pos: dvec2(x0 + 0.5, top + 1.0), size: dvec2((x1 - x0 - 1.0).max(3.0), h - 2.0) }
    }

    /// A transition's tab: on the cut point (same track), or straddling
    /// the boundary between the two tracks.
    fn tab_rect(&self, tracks: (u32, u32), start: f64, length: f64) -> Rect {
        let x0 = self.x_of(start);
        let w = (length * self.px_per_sec).max(20.0);
        let x = x0 + (length * self.px_per_sec - w) * 0.5;
        if tracks.0 == tracks.1 {
            let r = self.block_rect(Lane::Video, tracks.0, start, length);
            return Rect { pos: dvec2(x, r.pos.y), size: dvec2(w, STRIP + 8.0) };
        }
        let (upper, lower) = (self.block_rect(Lane::Video, tracks.1, start, length), self.block_rect(Lane::Video, tracks.0, start, length));
        let boundary = (upper.pos.y + upper.size.y + lower.pos.y) * 0.5;
        Rect { pos: dvec2(x, boundary - 9.0), size: dvec2(w, 18.0) }
    }

    fn track_state(&self, lane: Lane, track: u32) -> TrackState {
        self.tracks.get(&(lane_key(lane), track)).copied().unwrap_or_default()
    }

    /// Where `abs` is on the timeline, and the block (or transition) under it.
    pub fn hit(&self, abs: Vec2d) -> Option<TimelineHit> {
        if !self.rect.contains(abs) || abs.x < self.rect.pos.x + HEADER {
            return None;
        }
        let t = self.t_of(abs.x).max(0.0);
        let row = self.row_at(abs.y)?;
        let (lane, track) = match row.kind {
            RowKind::FxRail(track) => (Lane::Video, track),
            kind => kind.lane_track()?,
        };
        Some(TimelineHit { t, lane, track, block: self.block_at(abs).map(|(node, _)| node) })
    }

    /// A library item's drop place: the hit, its time snapped to the
    /// edges within reach (drawn while the drag holds).
    pub fn hit_snapped(&mut self, abs: Vec2d) -> Option<TimelineHit> {
        let mut hit = self.hit(abs)?;
        hit.t = self.snap(hit.t, None, false).max(0.0);
        self.snap_name = None;
        Some(hit)
    }

    fn block_at(&self, abs: Vec2d) -> Option<(String, Grab)> {
        for band in &self.projection.bands {
            let r = self.tab_rect(band.tracks, band.start, band.length);
            if r.contains(abs) {
                // Its edges make it longer or shorter.
                let grab = if r.size.x > 24.0 && abs.x - r.pos.x <= 6.0 { Grab::Start } else if r.size.x > 24.0 && r.pos.x + r.size.x - abs.x <= 6.0 { Grab::End } else { Grab::Body };
                return Some((band.node.clone(), grab));
            }
        }
        let edges = |rect: Rect| {
            if rect.size.x > EDGE_GRAB * 3.0 && abs.x - rect.pos.x <= EDGE_GRAB {
                Grab::Start
            } else if rect.size.x > EDGE_GRAB * 3.0 && rect.pos.x + rect.size.x - abs.x <= EDGE_GRAB {
                Grab::End
            } else {
                Grab::Body
            }
        };
        for block in self.projection.blocks.iter().filter(|block| block.kind == BlockKind::Effect) {
            let rect = self.band_rect(block.track, block.level, block.start, block.length);
            if rect.contains(abs) {
                return Some((block.node.clone(), edges(rect)));
            }
        }
        self.projection.blocks.iter().rev().filter(|block| block.kind != BlockKind::Effect).find_map(|block| {
            let rect = self.block_rect(block.lane, block.track, block.start, block.length);
            if !rect.contains(abs) {
                return None;
            }
            if let (Some(sound), true) = (&block.sound, matches!(block.kind, BlockKind::Audio | BlockKind::Clip)) {
                let band = self.sound_rect(block, rect);
                let (fin, fout) = (band.pos.x + sound.fade_in * self.px_per_sec, band.pos.x + band.size.x - sound.fade_out * self.px_per_sec);
                if (abs.y - band.pos.y).abs() <= FADE_GRAB {
                    if (abs.x - fin).abs() <= FADE_GRAB {
                        return Some((block.node.clone(), Grab::FadeIn));
                    }
                    if (abs.x - fout).abs() <= FADE_GRAB {
                        return Some((block.node.clone(), Grab::FadeOut));
                    }
                }
            }
            let grab = match block.kind {
                BlockKind::Voice | BlockKind::Caption => Grab::Body,
                _ => edges(rect),
            };
            Some((block.node.clone(), grab))
        })
    }

    /// Where a block's sound is drawn: an audio block under its strip, or a
    /// clip's linked audio band at its bottom.
    fn sound_rect(&self, block: &Block, rect: Rect) -> Rect {
        if block.lane == Lane::Video {
            Rect { pos: dvec2(rect.pos.x, rect.pos.y + rect.size.y - LINKED_AUDIO), size: dvec2(rect.size.x, LINKED_AUDIO) }
        } else {
            Rect { pos: dvec2(rect.pos.x, rect.pos.y + STRIP), size: dvec2(rect.size.x, rect.size.y - STRIP) }
        }
    }

    /// Snap `t` within reach; remembers where and names it.
    fn snap(&mut self, t: f64, except: Option<&str>, invert: bool) -> f64 {
        let reach = SNAP_PX / self.px_per_sec;
        let mut best: Option<(f64, f64, String)> = None;
        let mut edges: Vec<(f64, String)> = vec![(self.playhead, "playhead".into()), (0.0, "the start".into())];
        edges.extend(self.marks.0.map(|t| (t, "in".to_string())));
        edges.extend(self.marks.1.map(|t| (t, "out".to_string())));
        edges.extend(self.markers.iter().map(|t| (*t, "a marker".to_string())));
        for block in self.projection.blocks.iter().filter(|block| Some(block.node.as_str()) != except && block.kind != BlockKind::Caption) {
            edges.push((block.start, format!("{} start", block.label)));
            edges.push((block.start + block.length, format!("{} end", block.label)));
        }
        if self.snapping != invert {
            for (edge, name) in edges {
                let distance = (edge - t).abs();
                if distance <= reach && best.as_ref().is_none_or(|best| distance < best.1) {
                    best = Some((edge, distance, name));
                }
            }
        }
        match best {
            Some((edge, _, name)) => {
                if self.snapped != Some(edge) {
                    self.snapped = Some(edge);
                    self.snap_name = Some(name);
                }
                edge
            }
            None => {
                self.snapped = None;
                (t * self.fps).round() / self.fps
            }
        }
    }

    fn content_seconds(&self) -> f64 {
        self.projection.duration.max(1.0)
    }

    fn fit_seconds(&self) -> f64 {
        if self.projection.content_end > 0.0 { self.projection.content_end * 1.08 + 0.5 } else { self.content_seconds() }
    }

    fn clamp_scroll(&mut self) {
        let visible = ((self.rect.size.x - HEADER) / self.px_per_sec).max(0.0);
        let max = (self.content_seconds() + 2.0 - visible).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
        let height: f64 = self.rows().iter().map(|row| row.h).sum::<f64>() + self.scroll_y;
        let max_y = (height - (self.rect.size.y - RULER)).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
    }

    fn zoom_about(&mut self, px_per_sec: f64, anchor_x: f64) {
        let anchor_t = self.t_of(anchor_x);
        self.px_per_sec = px_per_sec.clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
        self.scroll = anchor_t - (anchor_x - self.rect.pos.x - HEADER) / self.px_per_sec;
        self.clamp_scroll();
    }

    fn playhead_anchor(&self) -> f64 {
        let x = self.x_of(self.playhead);
        if x >= self.rect.pos.x + HEADER && x <= self.rect.pos.x + self.rect.size.x { x } else { self.rect.pos.x + (HEADER + self.rect.size.x) * 0.5 }
    }

    pub fn set_projection(&mut self, cx: &mut Cx, projection: Projection) {
        if self.projection != projection {
            self.projection = projection;
            self.settle(cx);
            self.redraw(cx);
        }
    }

    /// The released gesture was answered: stop drawing its landing place.
    pub fn settle(&mut self, cx: &mut Cx) {
        if self.drag.as_ref().is_some_and(|drag| drag.released) {
            self.drag = None;
            self.snapped = None;
            self.fade_preview = None;
            self.redraw(cx);
        }
    }

    pub fn set_media(&mut self, cx: &mut Cx, films: HashMap<String, Filmstrip>, peaks: HashMap<String, Arc<Peaks>>) {
        self.films = films;
        self.peaks = peaks;
        self.redraw(cx);
    }

    pub fn set_selected(&mut self, cx: &mut Cx, selected: Option<String>, also: Vec<String>) {
        if self.selected != selected || self.also != also {
            self.selected = selected;
            self.also = also;
            self.redraw(cx);
        }
    }

    fn is_selected(&self, node: &str) -> bool {
        self.selected.as_deref() == Some(node) || self.also.iter().any(|other| other == node)
    }

    /// The selection a marquee makes, with the one it started from.
    fn marquee_selection(&self, a: Vec2d, b: Vec2d) -> Vec<String> {
        let hits = self.marquee_hits(a, b);
        let (base, mode) = &self.marquee;
        match mode {
            1 => base.iter().cloned().chain(hits.into_iter().filter(|hit| !base.contains(hit))).collect(),
            2 => base.iter().filter(|node| !hits.contains(node)).cloned().chain(hits.iter().filter(|hit| !base.contains(hit)).cloned()).collect(),
            _ => hits,
        }
    }

    /// Blocks the marquee touches.
    fn marquee_hits(&self, a: Vec2d, b: Vec2d) -> Vec<String> {
        let r = Rect { pos: dvec2(a.x.min(b.x), a.y.min(b.y)), size: dvec2((a.x - b.x).abs(), (a.y - b.y).abs()) };
        let touches = |q: Rect| q.pos.x < r.pos.x + r.size.x && q.pos.x + q.size.x > r.pos.x && q.pos.y < r.pos.y + r.size.y && q.pos.y + q.size.y > r.pos.y;
        // Every lane, VO and CC included (review 2, B7).
        self.projection.blocks.iter()
            .filter(|block| {
                let q = if block.kind == BlockKind::Effect { self.band_rect(block.track, block.level, block.start, block.length) } else { self.block_rect(block.lane, block.track, block.start, block.length) };
                touches(q)
            })
            .map(|block| block.node.clone()).collect()
    }

    pub fn set_playhead(&mut self, cx: &mut Cx, t: f64) {
        if self.playhead != t {
            self.playhead = t;
            self.redraw(cx);
        }
    }

    /// While playing, page when the playhead reaches 85 % of the width.
    pub fn follow(&mut self, cx: &mut Cx) {
        // Not laid out yet (a hidden page), or a kept scroll still to come.
        if self.rect.size.x <= HEADER || self.pending_scroll.is_some() {
            return;
        }
        // Only a playhead that moved pages the view (not a relayout).
        if self.followed == Some(self.playhead) {
            return;
        }
        self.followed = Some(self.playhead);
        let x = self.x_of(self.playhead);
        let (left, width) = (self.rect.pos.x + HEADER, self.rect.size.x - HEADER);
        if x > left + width * 0.85 || x < left {
            self.scroll = self.playhead - 0.1 * width / self.px_per_sec;
            self.clamp_scroll();
            self.redraw(cx);
        }
    }

    pub fn set_render_segments(&mut self, cx: &mut Cx, segments: Vec<BarSegment>) {
        if self.render_segments != segments {
            self.render_segments = segments;
            self.redraw(cx);
        }
    }

    pub fn set_marks(&mut self, cx: &mut Cx, marks: (Option<f64>, Option<f64>), markers: Vec<f64>, fps: f64, render_bar: Option<bool>) {
        if self.marks != marks || self.fps != fps || self.markers != markers || self.render_bar != render_bar {
            self.marks = marks;
            self.markers = markers;
            self.fps = fps;
            self.render_bar = render_bar;
            self.redraw(cx);
        }
    }

    pub fn set_drop_hint(&mut self, cx: &mut Cx, hint: Option<DropHint>) {
        if hint.is_none() && self.drag.is_none() {
            self.snapped = None;
        }
        if self.drop_hint != hint {
            self.drop_hint = hint;
            self.redraw(cx);
        }
    }

    pub fn set_blank(&mut self, cx: &mut Cx, blank: bool) {
        if self.blank != blank {
            self.blank = blank;
            self.redraw(cx);
        }
    }

    pub fn set_modes(&mut self, cx: &mut Cx, snapping: bool, skimming: bool, height: TrackHeight) {
        if (self.snapping, self.skimming, self.height) != (snapping, skimming, height) {
            self.snapping = snapping;
            self.skimming = skimming;
            self.height = height;
            self.redraw(cx);
        }
    }

    /// A sound track that is not heard: muted, or another is soloed.
    fn silent(&self, lane: Lane, track: u32) -> bool {
        let state = self.track_state(lane, track);
        let soloed = self.tracks.iter().any(|((key, _), state)| (*key == lane_key(Lane::Audio) || *key == lane_key(Lane::Voice)) && state.solo);
        state.muted || (soloed && !state.solo)
    }

    pub fn is_locked(&self, lane: Lane, track: u32) -> bool {
        self.track_state(lane, track).locked
    }

    /// Points per second, about the playhead (or the view's middle).
    /// Points per second now.
    pub fn px_per_sec(&self) -> f64 {
        self.px_per_sec
    }

    pub fn set_zoom(&mut self, cx: &mut Cx, px_per_sec: f64) {
        let anchor = self.playhead_anchor();
        self.zoom_about(px_per_sec, anchor);
        self.redraw(cx);
    }

    /// A keyboard or button zoom: eased over 160 ms about the playhead.
    pub fn zoom(&mut self, cx: &mut Cx, factor: f64) -> f64 {
        let from = self.zoom_anim.map_or(self.px_per_sec, |anim| anim.1);
        let to = (from * factor).clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
        let anchor_x = self.playhead_anchor();
        self.zoom_anim = Some((self.px_per_sec, to, self.t_of(anchor_x), anchor_x, Cx::time_now()));
        self.next_frame = cx.new_next_frame();
        to
    }

    fn step_zoom(&mut self, cx: &mut Cx) {
        let Some((from, to, anchor_t, anchor_x, began)) = self.zoom_anim else { return };
        let u = ((Cx::time_now() - began) / 0.16).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - u).powi(3);
        // Eased on a log scale, the anchor held still.
        self.px_per_sec = (from.ln() + (to.ln() - from.ln()) * eased).exp();
        self.scroll = anchor_t - (anchor_x - self.rect.pos.x - HEADER) / self.px_per_sec;
        self.clamp_scroll();
        if u < 1.0 {
            self.next_frame = cx.new_next_frame();
        } else {
            self.zoom_anim = None;
        }
        self.redraw(cx);
    }

    /// Show the whole cut.
    pub fn fit(&mut self, cx: &mut Cx) {
        self.fit_pending = true;
        self.redraw(cx);
    }

    pub fn focus(&self, cx: &mut Cx) {
        cx.set_key_focus(self.draw_bg.area());
    }

    /// Zoom and horizontal scroll (a draft keeps them).
    pub fn view(&self) -> (f64, f64) {
        (self.px_per_sec, self.scroll)
    }

    /// Back to a kept zoom and scroll, instead of fitting the cut.
    pub fn set_view(&mut self, cx: &mut Cx, px_per_sec: f64, scroll: f64, playhead: f64) {
        self.fit_pending = false;
        self.playhead = playhead;
        self.followed = Some(playhead);
        self.zoom_anim = None;
        self.px_per_sec = px_per_sec.clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
        // The scroll waits for the cut to be laid out (it clamps to it).
        self.pending_scroll = Some(scroll.max(0.0));
        self.redraw(cx);
    }

    fn fit_now(&mut self) {
        self.fit_pending = false;
        let width = (self.rect.size.x - HEADER - 24.0).max(80.0);
        self.px_per_sec = (width / self.fit_seconds()).clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
        self.scroll = 0.0;
    }

    fn color(value: u32, alpha: f32) -> Vec4f {
        let mut color = Vec4f::from_u32(value);
        color.w *= alpha;
        color
    }

    /// Text clipped to `width` with an ellipsis, measured.
    fn fit_text(draw: &DrawText, cx: &mut Cx2d, text: &str, width: f64) -> Option<String> {
        if width < 10.0 {
            return None;
        }
        if advance(draw, cx, text) <= width {
            return Some(text.to_string());
        }
        let chars: Vec<char> = text.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let candidate = format!("{}…", chars[..mid].iter().collect::<String>());
            if advance(draw, cx, &candidate) <= width { lo = mid } else { hi = mid - 1 }
        }
        (lo > 0).then(|| format!("{}…", chars[..lo].iter().collect::<String>()))
    }

    fn text(&mut self, cx: &mut Cx2d, pos: Vec2d, text: &str, width: f64, color: Vec4f) -> f64 {
        let Some(text) = Self::fit_text(&self.draw_text, cx, text, width) else { return 0.0 };
        self.draw_text.color = color;
        self.draw_text.draw_abs(cx, pos, &text);
        advance(&self.draw_text, cx, &text)
    }

    fn name(&mut self, cx: &mut Cx2d, pos: Vec2d, text: &str, width: f64, color: Vec4f) -> f64 {
        let Some(text) = Self::fit_text(&self.draw_name, cx, text, width) else { return 0.0 };
        self.draw_name.color = color;
        self.draw_name.draw_abs(cx, pos, &text);
        advance(&self.draw_name, cx, &text)
    }

    fn rect(&mut self, cx: &mut Cx2d, rect: Rect, color: Vec4f) {
        if rect.size.x > 0.0 && rect.size.y > 0.0 {
            self.draw_rect.color = color;
            self.draw_rect.draw_abs(cx, rect);
        }
    }

    fn shape(&mut self, cx: &mut Cx2d, rect: Rect, shape: f32, fill: Vec4f, edge: Vec4f, stroke: f32, radius: f32) {
        if rect.size.x <= 0.0 || rect.size.y <= 0.0 {
            return;
        }
        self.draw_block.fill = fill;
        self.draw_block.edge = edge;
        self.draw_block.stroke = stroke;
        self.draw_block.radius = radius;
        self.draw_block.shape = shape;
        self.draw_block.draw_abs(cx, rect);
    }

    /// A badge on a strip, right-aligned at `right`; returns its left.
    fn badge(&mut self, cx: &mut Cx2d, right: f64, y: f64, text: &str, fill: u32, ink: u32) -> f64 {
        let w = advance(&self.draw_badge, cx, text) + 8.0;
        let r = Rect { pos: dvec2(right - w, y), size: dvec2(w, 14.0) };
        self.shape(cx, r, 0.0, Self::color(fill, 1.0), Self::color(fill, 1.0), 0.0, 2.0);
        self.draw_badge.color = Self::color(ink, 1.0);
        self.draw_badge.draw_abs(cx, dvec2(r.pos.x + 4.0, y + 1.5), text);
        r.pos.x
    }

    /// The block's place: its own, or the drag's preview of it.
    fn placed(&self, block: &Block) -> (f64, f64, u32) {
        match self.drag.as_ref().and_then(|drag| Some((drag.node.as_ref()?, drag.preview?))) {
            Some((node, (start, length, track))) if *node == block.node || block.wraps.as_ref() == Some(node) => (start, length, track),
            _ => (block.start, block.length, block.track),
        }
    }

    fn fades(&self, block: &Block) -> (f64, f64) {
        match (&self.fade_preview, &block.sound) {
            (Some((node, fade_in, fade_out)), _) if *node == block.node => (*fade_in, *fade_out),
            (_, Some(sound)) => (sound.fade_in, sound.fade_out),
            _ => (0.0, 0.0),
        }
    }

    /// A filmstrip across `rect`: each tile shows the source frame at its
    /// left time, at its true aspect, with 1 px gaps.
    fn filmstrip(&mut self, cx: &mut Cx2d, block: &Block, rect: Rect, start: f64, clip: (f64, f64)) {
        // A graphic's or nested cut's own rendered strip (clip-local
        // times), else its library picture until that arrives.
        self.clip_films.mark_visible(&block.node);
        let rendered = self.clip_films.film(&block.node).cloned();
        let (asset, source_in, speed) = match (&rendered, block.picture.clone()) {
            (Some(_), _) => (String::new(), 0.0, 1.0),
            (None, Some(picture)) => picture,
            (None, None) => return,
        };
        let Some(film) = rendered.or_else(|| self.films.get(&asset).cloned()) else {
            self.rect(cx, Rect { pos: dvec2(rect.pos.x.max(clip.0), rect.pos.y), size: dvec2(rect.size.x.min(clip.1 - rect.pos.x.max(clip.0)), rect.size.y) }, Self::color(0x0f1217ff, 1.0));
            return;
        };
        let Some((_, first)) = film.first() else { return };
        let (tw, th) = first.get_format(cx).vec_width_height().unwrap_or((16, 9));
        // Portrait sources tile as centre squares; landscape keep their aspect.
        let portrait = th > tw;
        let tile = if portrait { rect.size.y } else { (rect.size.y * tw.max(1) as f64 / th.max(1) as f64).max(8.0) };
        let crop = if portrait { tw.max(1) as f64 / th.max(1) as f64 } else { 1.0 };
        let (v0, v1) = ((1.0 - crop) * 0.5, (1.0 + crop) * 0.5);
        let mut x = rect.pos.x;
        while x < rect.pos.x + rect.size.x && x < clip.1 {
            let w = tile.min(rect.pos.x + rect.size.x - x);
            let (a, b) = (x.max(clip.0), (x + w).min(clip.1));
            if b > a {
                let t = source_in + (self.t_of(x) - start).max(0.0) * speed;
                let texture = film.iter().min_by(|p, q| (p.0 - t).abs().total_cmp(&(q.0 - t).abs())).map(|(_, texture)| texture.clone()).unwrap();
                let (u0, u1) = ((a - x) / tile, (b - x) / tile);
                self.draw_picture.draw_vars.set_texture(0, &texture);
                self.draw_picture.opacity = self.film_alpha;
                self.draw_picture.uv = vec4(u0 as f32, v0 as f32, u1 as f32, v1 as f32);
                self.draw_picture.draw_abs(cx, Rect { pos: dvec2(a, rect.pos.y), size: dvec2((b - a - 1.0).max(0.5), rect.size.y) });
            }
            x += tile;
        }
    }

    /// A block's sound: a mirrored waveform (at its ducked and faded gain),
    /// the fades as filled ramps with round handles, and ducking as a
    /// dashed level with its chip.
    fn waveform(&mut self, cx: &mut Cx2d, block: &Block, band: Rect, start: f64, clip: (f64, f64), dim: f32) {
        let p = makepad_look::palette::get(cx);
        let Some(sound) = block.sound.clone() else { return };
        let hue = role_of(block).hue;
        let mid = band.pos.y + band.size.y * 0.5;
        let fades = self.fades(block);
        let peaks = sound.media.as_ref().and_then(|media| self.peaks.get(media)).cloned();
        let (a, b) = (band.pos.x.max(clip.0), (band.pos.x + band.size.x).min(clip.1));
        let duck_gain = 10f64.powf(sound.duck_db / 20.0);
        self.rect(cx, Rect { pos: dvec2(a, mid), size: dvec2((b - a).max(0.0), 1.0) }, Self::color(0xffffffff, 0.1 * dim));
        if let Some(peaks) = &peaks {
            let hue = makepad_look::palette::on_wave(cx, hue);
            // Two tones (review 2, B1): the peak envelope in the hue at
            // 45 %, the RMS envelope at full; a thin band (linked audio,
            // S height) draws only the RMS, bottom-up.
            let peak_ink = Self::color(hue, 0.45 * dim);
            let rms_ink = Self::color(hue, dim);
            let thin = band.size.y < 20.0;
            let half = band.size.y * 0.46;
            let base = band.pos.y + band.size.y - 1.0;
            let mut x = a;
            while x < b {
                let t = self.t_of(x) - start;
                let (s0, s1) = (sound.source_in + t * sound.speed, sound.source_in + (self.t_of(x + 1.0) - start) * sound.speed);
                let fade = if t < fades.0 { t / fades.0.max(1e-6) } else if t > block.length - fades.1 { (block.length - t) / fades.1.max(1e-6) } else { 1.0 };
                let at = start + t;
                let duck = if sound.ducked.iter().any(|(d0, d1)| at >= *d0 && at < *d1) { duck_gain } else { 1.0 };
                let gain = fade.clamp(0.0, 1.0) * duck;
                let rms = wave_height(peaks.rms_over(s0, s1) * gain);
                if thin {
                    let h = rms * (band.size.y - 2.0);
                    if h > 0.0 {
                        self.rect(cx, Rect { pos: dvec2(x, base - h), size: dvec2(1.0, h.max(1.0)) }, rms_ink);
                    }
                } else {
                    let p = wave_height(peaks.over(s0, s1) * gain) * half;
                    if p > 0.0 {
                        self.rect(cx, Rect { pos: dvec2(x, mid - p), size: dvec2(1.0, (p * 2.0).max(1.0)) }, peak_ink);
                    }
                    let r = rms * half;
                    if r > 0.0 {
                        self.rect(cx, Rect { pos: dvec2(x, mid - r), size: dvec2(1.0, (r * 2.0).max(1.0)) }, rms_ink);
                    }
                }
                x += 1.0;
            }
        }
        // Ducking: a dashed amber level and a chip at its start.
        for (index, (d0, d1)) in sound.ducked.clone().iter().enumerate() {
            let (x0, x1) = (self.x_of(*d0).max(a), self.x_of(*d1).min(b));
            if x1 <= x0 {
                continue;
            }
            let y = mid - band.size.y * 0.4 * duck_gain;
            let mut x = x0;
            while x < x1 {
                self.rect(cx, Rect { pos: dvec2(x, y), size: dvec2(4.0f64.min(x1 - x), 1.0) }, Self::color(p.amber, 0.8));
                x += 7.0;
            }
            let _ = index;
            if x1 - x0 > 70.0 {
                let text = format!("DUCK {:.0} dB", sound.duck_db).replace('-', "−");
                let w = advance(&self.draw_badge, cx, &text) + 10.0;
                let r = Rect { pos: dvec2(x0 + 3.0, band.pos.y + band.size.y - 16.0), size: dvec2(w, 13.0) };
                self.shape(cx, r, 0.0, Self::color(p.raised, 1.0), Self::color(p.amber, 0.5), 1.0, 2.0);
                self.draw_badge.color = Self::color(p.amber, 1.0);
                self.draw_badge.draw_abs(cx, dvec2(r.pos.x + 4.0, r.pos.y + 1.0), &text);
            }
        }
        if block.kind == BlockKind::Voice {
            return;
        }
        // Fades: a filled ramp over the waveform and a ramp line.
        let (fin, fout) = (band.pos.x + fades.0 * self.px_per_sec, band.pos.x + band.size.x - fades.1 * self.px_per_sec);
        for (from, to) in [(band.pos.x, fin), (band.pos.x + band.size.x, fout)] {
            let steps = ((to - from).abs()).ceil() as usize;
            for i in 0..steps {
                let x = from + (to - from).signum() * i as f64;
                if x < a || x > b {
                    continue;
                }
                let u = i as f64 / steps.max(1) as f64;
                let top = band.pos.y + (band.size.y) * (1.0 - u);
                self.rect(cx, Rect { pos: dvec2(x, band.pos.y), size: dvec2(1.0, top - band.pos.y) }, Self::color(p.wave_bg, 0.7));
                self.rect(cx, Rect { pos: dvec2(x, top), size: dvec2(1.0, 1.0) }, Self::color(p.glass_ink, 0.7));
            }
        }
        let hovered = self.hovered.as_ref().is_some_and(|(node, _)| *node == block.node);
        let selected = self.is_selected(&block.node);
        // A clip's linked band shows its handles only under the pointer
        // (review 2); a sound block when hovered, selected or faded.
        let over_band = self.pointer.is_some_and(|at| Rect { pos: band.pos - dvec2(6.0, 6.0), size: band.size + dvec2(12.0, 12.0) }.contains(at));
        let show = if block.lane == Lane::Video { over_band } else { hovered || selected || fades.0 > 0.0 || fades.1 > 0.0 };
        if show {
            for x in [fin, fout] {
                if x >= a - 4.0 && x <= b + 4.0 {
                    self.shape(cx, Rect { pos: dvec2(x - 4.0, band.pos.y - 4.0), size: dvec2(8.0, 8.0) }, 3.0, Self::color(p.glass_ink, 1.0), Self::color(0x000000ff, 1.0), 1.0, 0.0);
                }
            }
        }
    }

    fn draw_header(&mut self, cx: &mut Cx2d, row: Row, p: &makepad_look::palette::Palette) {
        let rect = Rect { pos: dvec2(self.rect.pos.x, row.top), size: dvec2(HEADER, row.h) };
        let drop_row = self.is_drop_row(row.kind);
        self.rect(cx, rect, Self::color(p.panel, if drop_row { 0.6 } else { 1.0 }));
        self.rect(cx, Rect { pos: dvec2(rect.pos.x + HEADER - 1.0, row.top), size: dvec2(1.0, row.h) }, Self::color(p.separator, 1.0));
        if drop_row {
            let (lane, track) = row.kind.lane_track().unwrap_or((Lane::Video, 0));
            let hinted = self.drop_hint.as_ref().is_some_and(|hint| hint.lane == lane && hint.track == track)
                || self.drag.as_ref().is_some_and(|drag| drag.moved && drag.lane == lane && drag.preview.is_some_and(|preview| preview.2 == track));
            if hinted {
                let text = if lane == Lane::Audio { "New audio track" } else { "New video track" };
                self.text(cx, dvec2(rect.pos.x + HEADER + 8.0, row.top + (row.h - 11.0) * 0.5), text, 200.0, Self::color(p.muted, 1.0));
            }
            return;
        }
        let Some((lane, track)) = row.kind.lane_track() else {
            if let RowKind::FxRail(_) = row.kind {
                self.text(cx, dvec2(rect.pos.x + 12.0, row.top + (row.h - 11.0) * 0.5), "fx", 30.0, Self::color(p.muted, 1.0));
            }
            return;
        };
        let moving_here = self.drag.as_ref().is_some_and(|drag| drag.grab == Grab::Body && drag.moved && drag.preview.is_some_and(|preview| preview.2 != drag.track && preview.2 == track) && drag.lane == lane);
        if moving_here {
            self.rect(cx, rect, Self::color(p.accent_wash, 1.0));
        }
        let role = role_of_lane(lane);
        self.rect(cx, Rect { pos: dvec2(rect.pos.x, row.top + 1.0), size: dvec2(3.0, row.h - 2.0) }, Self::color(role.hue, 1.0));
        let name = match lane {
            Lane::Video => format!("V{}", track + 1),
            Lane::Audio => format!("A{}", track + 1),
            Lane::Voice => "VO".to_string(),
            Lane::Captions => "CC".to_string(),
        };
        let main = lane == Lane::Video && track == 0;
        let locked = self.track_state(lane, track).locked;
        self.draw_track.color = Self::color(if locked { p.muted } else if main { p.text_hi } else { p.wave_label }, 1.0);
        let y = row.top + (row.h - 12.0) * 0.5;
        self.draw_track.draw_abs(cx, dvec2(rect.pos.x + 12.0, y), &name);
        if main {
            self.text(cx, dvec2(rect.pos.x + 34.0, y + 1.0), "MAIN", 40.0, Self::color(p.muted, 1.0));
        }
        if row.h < 30.0 {
            return;
        }
        // Chips, right to left: lock, then eye (picture) or S and M (sound).
        let state = self.track_state(lane, track);
        let chip = |i: usize| Rect { pos: dvec2(rect.pos.x + HEADER - 8.0 - 18.0 * (i + 1) as f64 - 4.0 * i as f64, row.top + (row.h - 18.0) * 0.5), size: dvec2(18.0, 18.0) };
        let lock = chip(0);
        if state.locked {
            self.shape(cx, lock, 0.0, Self::color(p.raised, 1.0), Self::color(p.raised, 1.0), 0.0, 2.5);
        }
        let icon = if state.locked { &mut self.icon_lock } else { &mut self.icon_lock_open };
        icon.color = Self::color(if state.locked { p.label } else { p.faint }, 1.0);
        icon.draw_abs(cx, Rect { pos: dvec2(lock.pos.x + 4.0, lock.pos.y + 4.0), size: dvec2(10.0, 10.0) });
        match lane {
            Lane::Video | Lane::Captions => {
                let eye = chip(1);
                let icon = if state.hidden { &mut self.icon_eye_off } else { &mut self.icon_eye };
                icon.color = Self::color(if state.hidden { p.amber } else { p.faint }, 1.0);
                icon.draw_abs(cx, Rect { pos: dvec2(eye.pos.x + 3.0, eye.pos.y + 3.0), size: dvec2(12.0, 12.0) });
            }
            Lane::Audio | Lane::Voice => {
                for (i, letter, on, fill, ink) in [(1, "S", state.solo, p.found, 0x0a0d12ffu32), (2, "M", state.muted, p.amber, p.on_accent)] {
                    let r = chip(i);
                    self.shape(cx, r, 0.0, Self::color(if on { fill } else { p.wave_bg }, 1.0), Self::color(if on { fill } else { p.wave_rim }, 1.0), 1.0, 2.5);
                    self.draw_badge.color = Self::color(if on { ink } else { p.faint }, 1.0);
                    self.draw_badge.draw_abs(cx, dvec2(r.pos.x + 5.5, r.pos.y + 3.5), letter);
                }
            }
        }
    }

    /// A header chip under `abs`: toggles it.
    fn header_click(&mut self, abs: Vec2d) -> bool {
        let Some(row) = self.row_at(abs.y) else { return false };
        let Some((lane, track)) = row.kind.lane_track().filter(|_| !self.is_drop_row(row.kind)) else { return false };
        let x = abs.x - self.rect.pos.x;
        let index = ((HEADER - 8.0 - x) / 22.0).floor();
        if !(0.0..3.0).contains(&index) || x > HEADER - 8.0 {
            return false;
        }
        let state = self.tracks.entry((lane_key(lane), track)).or_default();
        match (index as usize, lane) {
            (0, _) => state.locked = !state.locked,
            (1, Lane::Video | Lane::Captions) => state.hidden = !state.hidden,
            (1, _) => state.solo = !state.solo,
            (2, Lane::Audio | Lane::Voice) => state.muted = !state.muted,
            _ => return false,
        }
        true
    }

    fn draw_ruler(&mut self, cx: &mut Cx2d, p: &makepad_look::palette::Palette) {
        let rect = self.rect;
        let (x0, x1) = (rect.pos.x + HEADER, rect.pos.x + rect.size.x);
        let ruler = Rect { pos: rect.pos, size: dvec2(rect.size.x, RULER) };
        self.rect(cx, ruler, Self::color(p.wave_ruler_bg, 1.0));
        self.rect(cx, Rect { pos: dvec2(rect.pos.x, rect.pos.y + RULER - 1.0), size: dvec2(rect.size.x, 1.0) }, Self::color(p.wave_rim, 1.0));
        self.rect(cx, Rect { pos: rect.pos, size: dvec2(HEADER, RULER) }, Self::color(p.panel, 1.0));
        let (major, minor) = tick_steps(self.px_per_sec, self.fps);
        let first = (self.scroll / minor).floor() as i64;
        let last = (self.t_of(x1) / minor).ceil() as i64;
        let mut label_end = f64::MIN;
        for n in first..=last {
            let t = n as f64 * minor;
            let x = self.x_of(t);
            if x < x0 || t < -1e-9 {
                continue;
            }
            let is_major = ((t / major) - (t / major).round()).abs() < 1e-6;
            if is_major {
                self.rect(cx, Rect { pos: dvec2(x, rect.pos.y + RULER - 11.0), size: dvec2(1.0, 10.0) }, Self::color(p.wave_label, 1.0));
                // Majors run through the tracks as faint grid lines.
                self.rect(cx, Rect { pos: dvec2(x, rect.pos.y + RULER), size: dvec2(1.0, rect.size.y - RULER) }, Self::color(p.wave_grid, 0.5));
                if x + 4.0 > label_end {
                    let (head, tail) = timecode_parts(t, self.fps);
                    let w_head = self.text(cx, dvec2(x + 4.0, rect.pos.y + 4.0), &head, 200.0, Self::color(p.wave_muted, 0.7));
                    let w_tail = self.text(cx, dvec2(x + 4.0 + w_head, rect.pos.y + 4.0), &tail, 200.0, Self::color(p.wave_label, 1.0));
                    label_end = x + 4.0 + w_head + w_tail + 8.0;
                }
            } else {
                self.rect(cx, Rect { pos: dvec2(x, rect.pos.y + RULER - 5.0), size: dvec2(1.0, 4.0) }, Self::color(p.wave_muted, 0.6));
            }
        }
        // The render bar, per segment: needs render amber, rendering an
        // amber that moves, rendered dim green; generative violet.
        let y = rect.pos.y + RULER - 4.0;
        let mut moving = false;
        for segment in self.render_segments.clone() {
            let (a, b) = (self.x_of(segment.t0).max(x0), self.x_of(segment.t1).min(x1));
            if b <= a {
                continue;
            }
            let (hue, alpha, progress) = match segment.state {
                BarState::NeedsGeneration => (GENERATIVE, 0.9, None),
                BarState::Generating(done) => (GENERATIVE, 0.45, Some(done)),
                BarState::Generated => (GENERATIVE, 0.4, None),
            };
            self.rect(cx, Rect { pos: dvec2(a + 0.5, y), size: dvec2((b - a - 1.0).max(1.0), 3.0) }, Self::color(hue, alpha));
            if let Some(done) = progress {
                // What is done, and a light running along it.
                moving = true;
                let end = (self.x_of(segment.t0 + (segment.t1 - segment.t0) * done.clamp(0.0, 1.0))).min(x1);
                if end > a {
                    self.rect(cx, Rect { pos: dvec2(a + 0.5, y), size: dvec2(end - a, 3.0) }, Self::color(hue, 1.0));
                }
                let phase = (Cx::time_now() * 1.2).fract();
                let x = a + (b - a) * phase;
                self.rect(cx, Rect { pos: dvec2((x - 6.0).max(a), y), size: dvec2(12.0_f64.min(b - (x - 6.0).max(a)), 3.0) }, Self::color(p.text_hi, 0.55));
            }
        }
        if moving {
            self.next_frame = cx.new_next_frame();
        }
        // (The library render's fresh/stale bar is gone: renders are
        // ephemeral, the segments above say it all.)
        // In/out: accent over the ruler with brackets.
        if let (Some(a), Some(b)) = self.marks {
            let (a, b) = (self.x_of(a.min(b)), self.x_of(a.max(b)));
            let (ca, cb) = (a.max(x0), b.min(x1));
            if cb > ca {
                self.rect(cx, Rect { pos: dvec2(ca, rect.pos.y), size: dvec2(cb - ca, RULER - 1.0) }, Self::color(p.accent, 0.22));
            }
            for (x, left) in [(a, true), (b, false)] {
                if x >= x0 - 2.0 && x <= x1 {
                    self.rect(cx, Rect { pos: dvec2(x - 1.0, rect.pos.y + RULER - 9.0), size: dvec2(2.0, 8.0) }, Self::color(p.accent, 1.0));
                    let foot = if left { x } else { x - 5.0 };
                    self.rect(cx, Rect { pos: dvec2(foot, rect.pos.y + RULER - 2.0), size: dvec2(5.0, 2.0) }, Self::color(p.accent, 1.0));
                }
            }
        } else {
            for mark in [self.marks.0, self.marks.1].into_iter().flatten() {
                let x = self.x_of(mark);
                if x >= x0 && x <= x1 {
                    self.rect(cx, Rect { pos: dvec2(x - 1.0, rect.pos.y + RULER - 9.0), size: dvec2(2.0, 8.0) }, Self::color(p.accent, 1.0));
                }
            }
        }
        // Markers: gold flags.
        for marker in self.markers.clone() {
            let x = self.x_of(marker);
            if x >= x0 && x <= x1 {
                self.shape(cx, Rect { pos: dvec2(x - 4.5, rect.pos.y + 2.0), size: dvec2(9.0, 11.0) }, 2.0, Self::color(CutRole::MARKER, 1.0), Self::color(CutRole::MARKER, 1.0), 0.0, 0.0);
            }
        }
    }

    fn draw_block(&mut self, cx: &mut Cx2d, block: &Block, clip: (f64, f64), p: &makepad_look::palette::Palette) {
        let (start, length, track) = self.placed(block);
        let r = self.block_rect(block.lane, track, start, length);
        let clipped = Rect { pos: dvec2(r.pos.x.max(clip.0), r.pos.y), size: dvec2((r.pos.x + r.size.x).min(clip.1) - r.pos.x.max(clip.0), r.size.y) };
        if clipped.size.x <= 0.0 || r.size.y <= 0.0 {
            return;
        }
        let role = role_of(block);
        let state = self.track_state(block.lane, block.track);
        let dim = if state.hidden || (self.silent(block.lane, block.track) && block.lane != Lane::Video) { 0.35 } else { 1.0 };
        let hovered = self.hovered.as_ref().is_some_and(|(node, _)| *node == block.node);
        let selected = self.is_selected(&block.node);
        let moving = self.drag.as_ref().is_some_and(|drag| drag.node.as_deref() == Some(block.node.as_str()) && drag.moved && drag.grab == Grab::Body);
        // The origin of a move, dashed.
        if moving {
            let origin = self.block_rect(block.lane, block.track, block.start, block.length);
            self.shape(cx, origin, 5.0, Self::color(0, 0.0), Self::color(role.hue, 0.5), 1.0, 4.0);
        }
        let alpha = dim * if moving { 0.85 } else { 1.0 };
        self.film_alpha = alpha;
        // In flight: a shadow under the moved clip (design §4.5).
        if moving {
            let p = makepad_look::palette::get(cx);
            self.shape(cx, Rect { pos: clipped.pos + dvec2(0.0, 4.0), size: clipped.size }, 0.0, Self::color(p.shadow, 0.5), Self::color(p.shadow, 0.0), 0.0, 5.0);
        }
        let wash = if hovered { role.hover } else { role.wash };
        // A compound clip: a stack edge above it (design §4.8).
        if block.nested && r.pos.y - 3.0 > self.rect.pos.y + RULER {
            let (a, b) = ((r.pos.x + 4.0).max(clip.0), (r.pos.x + r.size.x - 4.0).min(clip.1));
            if b > a {
                self.rect(cx, Rect { pos: dvec2(a, r.pos.y - 3.0), size: dvec2(b - a, 2.0) }, Self::color(role.strip, alpha));
            }
        }
        if block.kind == BlockKind::Caption {
            // A caption run.
            let provisional = block.stale;
            let fill = Self::color(role.wash, alpha);
            let edge = Self::color(role.hue, 0.35 * alpha);
            self.shape(cx, clipped, if provisional { 5.0 } else { 0.0 }, fill, edge, 1.0, 3.0);
            let text = if provisional { format!("~ {}", block.label) } else { block.label.clone() };
            self.text(cx, dvec2(clipped.pos.x + 6.0, r.pos.y + (r.size.y - 11.0) * 0.5), &text, clipped.size.x - 10.0, Self::color(if provisional { p.muted } else { p.glass_ink }, alpha));
            if selected {
                self.shape(cx, Rect { pos: clipped.pos - dvec2(1.5, 1.5), size: clipped.size + dvec2(3.0, 3.0) }, 0.0, Self::color(0, 0.0), Self::color(p.accent, 1.0), 2.0, 4.0);
            }
            return;
        }
        self.shape(cx, clipped, 0.0, Self::color(wash, alpha), Self::color(role.hue, if hovered { 1.0 } else { 0.7 } * alpha), 1.0, 4.0);
        let strip_h = if block.kind == BlockKind::Title && !block.graphic { 12.0 } else { STRIP };
        let body = Rect { pos: dvec2(clipped.pos.x + 1.0, r.pos.y + strip_h), size: dvec2(clipped.size.x - 2.0, r.size.y - strip_h - 1.0) };
        match block.kind {
            BlockKind::Clip | BlockKind::Still => {
                let band = if block.sound.is_some() { LINKED_AUDIO } else { 0.0 };
                let film = Rect { pos: dvec2(r.pos.x + 1.0, body.pos.y), size: dvec2(r.size.x - 2.0, (body.size.y - band).max(0.0)) };
                if self.height != TrackHeight::Small {
                    self.filmstrip(cx, block, film, start, (clipped.pos.x + 1.0, clipped.pos.x + clipped.size.x - 1.0));
                }
                if band > 0.0 {
                    let sound = self.sound_rect(block, r);
                    self.rect(cx, Rect { pos: dvec2(clipped.pos.x + 1.0, sound.pos.y), size: dvec2(clipped.size.x - 2.0, sound.size.y - 1.0) }, Self::color(CutRole::AUDIO.wash, alpha));
                    self.rect(cx, Rect { pos: dvec2(clipped.pos.x + 1.0, sound.pos.y), size: dvec2(clipped.size.x - 2.0, 1.0) }, Self::color(0x000000ff, 0.4));
                    let mut audio_view = block.clone();
                    audio_view.kind = BlockKind::Audio;
                    self.waveform(cx, &audio_view, sound, start, (clipped.pos.x, clipped.pos.x + clipped.size.x), dim);
                }
            }
            // A motion graphic: its document's thumbnail, tiled.
            BlockKind::Title if block.graphic => {
                if self.height != TrackHeight::Small {
                    self.filmstrip(cx, block, body, start, (clipped.pos.x + 1.0, clipped.pos.x + clipped.size.x - 1.0));
                }
            }
            BlockKind::Title => {
                self.text(cx, dvec2(body.pos.x + 6.0, body.pos.y + (body.size.y - 11.0) * 0.5), &block.label, body.size.x - 10.0, Self::color(p.glass_ink, alpha));
            }
            BlockKind::Audio | BlockKind::Voice => {
                if block.kind == BlockKind::Voice && block.stale {
                    // No take (or one for other words): hatched, a dotted
                    // centre line, a dashed estimated end.
                    self.shape(cx, body, 4.0, Self::color(0, 0.0), Self::color(role.hue, 0.25 * alpha), 0.0, 0.0);
                    let mid = body.pos.y + body.size.y * 0.5;
                    let mut x = body.pos.x;
                    while x < body.pos.x + body.size.x {
                        self.rect(cx, Rect { pos: dvec2(x, mid), size: dvec2(2.0, 1.0) }, Self::color(role.hue, 0.6 * alpha));
                        x += 5.0;
                    }
                    let end = r.pos.x + r.size.x - 1.0;
                    if end < clip.1 {
                        let mut y = r.pos.y;
                        while y < r.pos.y + r.size.y {
                            self.rect(cx, Rect { pos: dvec2(end, y), size: dvec2(1.0, 3.0) }, Self::color(role.hue, alpha));
                            y += 5.0;
                        }
                    }
                } else {
                    let sound = self.sound_rect(block, r);
                    self.waveform(cx, block, sound, start, (clipped.pos.x, clipped.pos.x + clipped.size.x), dim);
                }
            }
            _ => {}
        }
        // Locked tracks: a faint hatch over their clips.
        if state.locked {
            // Strong enough to read over a filmstrip (review 2, B9).
            self.shape(cx, clipped, 4.0, Self::color(0, 0.0), Self::color(0xffffffff, 0.18), 0.0, 0.0);
        }
        // The name strip, its text pinned to the visible left edge, badges
        // right-aligned and never under a transition tab.
        let strip = Rect { pos: dvec2(clipped.pos.x + 1.0, r.pos.y + 1.0), size: dvec2(clipped.size.x - 2.0, strip_h - 1.0) };
        self.rect(cx, strip, Self::color(role.strip, alpha));
        let tabs: Vec<Rect> = self.projection.bands.iter().filter(|band| band.tracks.0 == band.tracks.1 && band.tracks.0 == block.track && block.lane == Lane::Video)
            .map(|band| self.tab_rect(band.tracks, band.start, band.length)).collect();
        let mut text_left = strip.pos.x + 5.0;
        let mut badges_right = strip.pos.x + strip.size.x - 3.0;
        for tab in &tabs {
            if tab.pos.x <= text_left && tab.pos.x + tab.size.x > text_left {
                text_left = tab.pos.x + tab.size.x + 4.0;
            }
            if tab.pos.x < badges_right && tab.pos.x + tab.size.x > badges_right {
                badges_right = tab.pos.x - 4.0;
            }
        }
        if clipped.size.x < 24.0 {
            if selected {
                self.shape(cx, Rect { pos: clipped.pos - dvec2(1.5, 1.5), size: clipped.size + dvec2(3.0, 3.0) }, 0.0, Self::color(0, 0.0), Self::color(p.accent, 1.0), 2.0, 5.0);
            }
            return;
        }
        let badge_y = strip.pos.y + 0.5;
        let mut right = badges_right;
        if clipped.size.x >= 60.0 {
            if block.stale && block.kind == BlockKind::Voice {
                right = self.badge(cx, right, badge_y, "STALE", p.amber, p.on_accent) - 3.0;
            }
            if block.broken {
                right = self.badge(cx, right, badge_y, "REMOVED", p.danger, p.on_danger) - 3.0;
            }
            if block.fx {
                right = self.badge(cx, right, badge_y, "FX", CutRole::EFFECT.hue, 0x1a0b1aff) - 3.0;
            }
            if block.nested {
                right = self.badge(cx, right, badge_y, "CUT", p.raised, p.label) - 3.0;
            }
            if let Some((_, _, speed)) = block.picture {
                if block.kind == BlockKind::Clip && (speed - 1.0).abs() > 1e-6 {
                    let text = if speed >= 1.0 { format!("{speed}×") } else { format!("{:.0}%", speed * 100.0) };
                    right = self.badge(cx, right, badge_y, &text, p.raised, p.label) - 3.0;
                }
            }
        }
        let mut x = text_left;
        let glyph = match block.kind {
            BlockKind::Still => Some(&mut self.icon_image),
            BlockKind::Clip if block.app_run => Some(&mut self.icon_app),
            BlockKind::Clip if block.nested => Some(&mut self.icon_edit),
            BlockKind::Title => Some(&mut self.icon_title),
            _ => None,
        };
        if let Some(glyph) = glyph {
            glyph.color = Self::color(p.glass_ink, alpha);
            glyph.draw_abs(cx, Rect { pos: dvec2(x, strip.pos.y + (strip.size.y - 10.0) * 0.5), size: dvec2(10.0, 10.0) });
            x += 14.0;
        }
        let ink = if selected { p.text_max } else { p.glass_ink };
        // The meta (duration, speed) goes first when room runs short, then
        // the name is ellipsized against the measured width.
        let meta = (clipped.size.x >= 60.0 && block.kind != BlockKind::Voice).then(|| timecode_full(length, self.fps));
        let graphic_label = format!("GRAPHIC · {}", block.label);
        let full = advance(&self.draw_name, cx, if block.graphic { &graphic_label } else if block.kind == BlockKind::Title { "Title" } else { &block.label });
        let meta_w = meta.as_ref().map_or(0.0, |meta| advance(&self.draw_text, cx, meta) + 8.0);
        let meta = meta.filter(|_| x + full + meta_w <= right);
        let strip_label = if block.graphic { graphic_label.clone() } else if block.kind == BlockKind::Title { "Title".to_string() } else { block.label.clone() };
        let name_w = self.name(cx, dvec2(x, strip.pos.y + (strip.size.y - 12.0) * 0.5), &strip_label, right - x, Self::color(ink, alpha));
        if let Some(meta) = meta {
            let meta_x = x + name_w + 8.0;
            self.text(cx, dvec2(meta_x, strip.pos.y + (strip.size.y - 11.0) * 0.5), &meta, right - meta_x, Self::color(p.glass_ink_soft, alpha));
        }
        // Keys on the selected clip: a strip of diamonds at the bottom.
        if selected && !block.keys.is_empty() {
            let lane = Rect { pos: dvec2(clipped.pos.x + 1.0, r.pos.y + r.size.y - 13.0), size: dvec2(clipped.size.x - 2.0, 12.0) };
            self.rect(cx, lane, Self::color(0x000000ff, 0.45));
            for key in block.keys.clone() {
                let kx = self.x_of(key);
                if kx >= lane.pos.x && kx <= lane.pos.x + lane.size.x {
                    let on = (key - self.playhead).abs() < 0.5 / self.fps;
                    self.shape(cx, Rect { pos: dvec2(kx - 4.0, lane.pos.y + 2.0), size: dvec2(8.0, 8.0) }, 1.0, Self::color(if on { p.accent } else { p.glass_ink }, 1.0), Self::color(0x000000ff, 1.0), 1.0, 0.0);
                }
            }
        }
        // Hover trim handles, the trim bracket and tag.
        let trimming = self.drag.as_ref().filter(|drag| drag.node.as_deref() == Some(block.node.as_str()) && drag.moved && matches!(drag.grab, Grab::Start | Grab::End)).map(|drag| drag.grab);
        if (hovered || trimming.is_some()) && r.size.x > EDGE_GRAB * 3.0 && !matches!(block.kind, BlockKind::Voice | BlockKind::Caption) && !state.locked {
            for (x, end) in [(r.pos.x + 3.0, Grab::Start), (r.pos.x + r.size.x - 6.0, Grab::End)] {
                if x < clip.0 || x > clip.1 {
                    continue;
                }
                let active = trimming == Some(end);
                self.rect(cx, Rect { pos: dvec2(x, r.pos.y + 4.0), size: dvec2(3.0, r.size.y - 8.0) }, if active { Self::color(p.accent, 1.0) } else { Self::color(p.glass_ink, 0.8) });
            }
        }
        if let Some(grab) = trimming {
            let delta = if grab == Grab::Start { start - block.start } else { (start + length) - (block.start + block.length) };
            let sign = if delta < 0.0 { "−" } else { "+" };
            let tag = format!("{sign}{} · {}", timecode_full(delta.abs(), self.fps), timecode_full(length, self.fps));
            self.tag(cx, dvec2(if grab == Grab::Start { r.pos.x } else { r.pos.x + r.size.x }, 0.0), &tag, p.raised, p.label);
        }
        if moving {
            let row = match block.lane { Lane::Video => format!("V{}", track + 1), Lane::Audio => format!("A{}", track + 1), Lane::Voice => "VO".into(), Lane::Captions => "CC".into() };
            let tag = format!("{} · {row}", timecode_full(start, self.fps));
            self.tag(cx, dvec2(r.pos.x, 0.0), &tag, p.raised, p.label);
        }
        if selected {
            self.shape(cx, Rect { pos: clipped.pos - dvec2(1.5, 1.5), size: clipped.size + dvec2(3.0, 3.0) }, 0.0, Self::color(0, 0.0), Self::color(p.accent, 1.0), 2.0, 5.0);
        }
    }

    /// A floating tag (trim, move, drop).
    /// A floating tag, drawn over everything else at the end of the draw.
    fn tag(&mut self, _cx: &mut Cx2d, pos: Vec2d, text: &str, fill: u32, ink: u32) {
        self.later_tags.push((pos, text.to_string(), fill, ink));
    }

    /// In the ruler's lower half, centred on `pos.x` (review 2, B5): drag
    /// tags never cover track content; the labels under one are hidden.
    fn draw_tag(&mut self, cx: &mut Cx2d, pos: Vec2d, text: &str, fill: u32, ink: u32) {
        let p = makepad_look::palette::get(cx);
        let w = advance(&self.draw_text, cx, text) + 10.0;
        let (lo, hi) = (self.rect.pos.x + HEADER + 2.0, self.rect.pos.x + self.rect.size.x - w - 2.0);
        let x = (pos.x - w * 0.5).clamp(lo, hi.max(lo));
        let r = Rect { pos: dvec2(x, self.rect.pos.y + RULER * 0.5 - 1.0), size: dvec2(w, 14.0) };
        self.rect(cx, Rect { pos: dvec2(r.pos.x - 6.0, self.rect.pos.y), size: dvec2(r.size.x + 12.0, RULER - 3.0) }, Self::color(p.wave_ruler_bg, 1.0));
        self.shape(cx, Rect { pos: r.pos + dvec2(0.0, 2.0), size: r.size }, 0.0, Self::color(p.shadow, 0.6), Self::color(p.shadow, 0.0), 0.0, 3.0);
        self.shape(cx, r, 0.0, Self::color(fill, 1.0), Self::color(fill, 1.0), 0.0, 3.0);
        self.draw_text.color = Self::color(ink, 1.0);
        self.draw_text.draw_abs(cx, dvec2(r.pos.x + 5.0, r.pos.y + 1.0), text);
    }
}

impl Widget for CutTimeline {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let uid = self.widget_uid();
        if self.clip_films.handle_event(cx, event) {
            self.redraw(cx);
        }
        // A strip in the making keeps the timeline drawing (it records and
        // reads back from this widget's draw), unless the page plays.
        if self.clip_films.busy() && !self.clip_films.paused && matches!(event, Event::NextFrame(_)) {
            self.redraw(cx);
        }
        if self.next_frame.is_event(event).is_some() {
            self.step_zoom(cx);
            // The render bar's moving light.
            if self.render_segments.iter().any(|segment| matches!(segment.state, BarState::Generating(_))) || self.projection.blocks.iter().any(|block| block.progress.is_some()) {
                self.redraw(cx);
            }
        }
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                if self.pointer != Some(fe.abs) {
                    self.pointer = Some(fe.abs);
                    self.redraw(cx);
                }
                let over_tracks = fe.abs.x >= self.rect.pos.x + HEADER && fe.abs.y >= self.rect.pos.y + RULER;
                let skim = (over_tracks && self.skimming).then(|| self.t_of(fe.abs.x).clamp(0.0, self.content_seconds()));
                let hovered = if over_tracks { self.block_at(fe.abs) } else { None };
                let cursor = match &hovered {
                    Some((node, _)) if self.projection.block(node).is_some_and(|block| self.is_locked(block.lane, block.track)) => MouseCursor::NotAllowed,
                    Some((_, Grab::Start | Grab::End)) => MouseCursor::EwResize,
                    Some((_, Grab::FadeIn | Grab::FadeOut)) => MouseCursor::Hand,
                    Some(_) => MouseCursor::Default,
                    None => MouseCursor::Default,
                };
                cx.set_cursor(cursor);
                if hovered != self.hovered {
                    self.hovered = hovered;
                    self.redraw(cx);
                }
                if skim != self.skim && self.drag.is_none() {
                    self.skim = skim;
                    cx.widget_action(uid, TimelineAction::Skim(skim));
                    self.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                self.pointer = None;
                self.hovered = None;
                if self.skim.take().is_some() {
                    cx.widget_action(uid, TimelineAction::Skim(None));
                }
                self.redraw(cx);
            }
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                cx.set_key_focus(self.draw_bg.area());
                let origin = fe.abs;
                let scrub_drag = Drag { grab: Grab::Ruler, node: None, origin, span: (0.0, 0.0), lane: Lane::Video, track: 0, moved: false, released: false, preview: None, corner: origin };
                let t = self.t_of(fe.abs.x).clamp(0.0, self.content_seconds());
                if fe.abs.x < self.rect.pos.x + HEADER && fe.abs.y >= self.rect.pos.y + RULER {
                    if self.header_click(fe.abs) {
                        self.redraw(cx);
                    }
                    return;
                }
                if fe.abs.y < self.rect.pos.y + RULER {
                    self.drag = Some(scrub_drag);
                    cx.widget_action(uid, TimelineAction::Scrub(t));
                    return;
                }
                match self.block_at(fe.abs) {
                    Some((node, grab)) => {
                        let block = self.projection.block(&node).cloned();
                        let (span, lane, track) = block.as_ref()
                            .map(|block| ((block.start, block.length), block.lane, block.track))
                            .or_else(|| self.projection.bands.iter().find(|band| band.node == node).map(|band| ((band.start, band.length), Lane::Video, band.tracks.0)))
                            .unwrap_or(((0.0, 0.0), Lane::Video, 0));
                        let locked = self.is_locked(lane, track);
                        let movable = !locked && match block.as_ref().map(|block| block.kind) {
                            Some(BlockKind::Caption) => false,
                            // An effect has its clip's time: selected, not dragged.
                            Some(BlockKind::Effect) => false,
                            None => grab != Grab::Body,
                            _ => true,
                        };
                        let span = match (grab, block.as_ref().and_then(|block| block.sound.as_ref())) {
                            (Grab::FadeIn | Grab::FadeOut, Some(sound)) => (sound.fade_in, sound.fade_out),
                            _ => span,
                        };
                        self.selected = Some(node.clone());
                        cx.widget_action(uid, TimelineAction::Select(Some(node.clone()), t));
                        if fe.tap_count >= 2 && block.as_ref().is_some_and(|block| block.nested || block.graphic) {
                            cx.widget_action(uid, TimelineAction::Open(node.clone()));
                            self.redraw(cx);
                            return;
                        }
                        self.drag = movable.then_some(Drag { grab, node: Some(node), origin, span, lane, track, moved: false, released: false, preview: None, corner: origin });
                    }
                    None => {
                        // Empty track space: a click deselects and seeks, a
                        // drag draws a selection rectangle; shift adds to the
                        // selection, cmd toggles in it.
                        let base: Vec<String> = self.selected.iter().cloned().chain(self.also.iter().cloned()).collect();
                        self.marquee = if fe.modifiers.shift { (base, 1) } else if fe.modifiers.logo || fe.modifiers.control { (base, 2) } else { (Vec::new(), 0) };
                        if self.marquee.1 == 0 {
                            self.selected = None;
                            self.also.clear();
                            cx.widget_action(uid, TimelineAction::Select(None, t));
                        }
                        self.drag = Some(Drag { grab: Grab::Marquee, ..scrub_drag });
                    }
                }
                self.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                let Some(drag) = self.drag.clone().filter(|drag| !drag.released) else { return };
                if drag.grab == Grab::Ruler {
                    let t = self.t_of(fe.abs.x).clamp(0.0, self.content_seconds());
                    cx.widget_action(uid, TimelineAction::Scrub(t));
                    return;
                }
                if drag.grab == Grab::Marquee {
                    if let Some(active) = &mut self.drag {
                        active.moved = active.moved || (fe.abs - active.origin).length() >= 3.0;
                        active.corner = fe.abs;
                    }
                    let hits = self.marquee_selection(drag.origin, fe.abs);
                    self.selected = hits.first().cloned();
                    self.also = hits.into_iter().skip(1).collect();
                    self.redraw(cx);
                    return;
                }
                let dx = fe.abs.x - drag.origin.x;
                if !drag.moved && dx.abs() < 3.0 && (fe.abs.y - drag.origin.y).abs() < 3.0 {
                    return;
                }
                let invert = fe.modifiers.logo || fe.modifiers.control;
                let dt = dx / self.px_per_sec;
                let node = drag.node.clone();
                let node = node.as_deref();
                if let (Grab::FadeIn | Grab::FadeOut, Some(block)) = (drag.grab, node.and_then(|node| self.projection.block(node)).cloned()) {
                    let (fade_in, fade_out) = drag.span;
                    let fades = match drag.grab {
                        Grab::FadeIn => ((fade_in + dt).clamp(0.0, block.length - fade_out), fade_out),
                        _ => (fade_in, (fade_out - dt).clamp(0.0, block.length - fade_in)),
                    };
                    self.fade_preview = Some((block.node.clone(), fades.0, fades.1));
                    if let Some(active) = &mut self.drag {
                        active.moved = true;
                    }
                    self.redraw(cx);
                    return;
                }
                let voice = node.and_then(|node| self.projection.block(node)).is_some_and(|block| block.kind == BlockKind::Voice);
                let (start, length) = drag.span;
                let preview = match drag.grab {
                    Grab::Body => {
                        let head = self.snap(start + dt, node, invert);
                        let head_snap = self.snapped;
                        let tail = self.snap(start + length + dt, node, invert) - length;
                        let start = if head_snap.is_some() || self.snapped.is_none() {
                            self.snapped = head_snap;
                            head
                        } else {
                            tail
                        };
                        let track = self.row_at(fe.abs.y).and_then(|row| row.kind.lane_track()).filter(|(lane, track)| *lane == drag.lane && !voice && !self.is_locked(*lane, *track)).map_or(drag.track, |(_, track)| track);
                        (start.max(0.0), length, track)
                    }
                    Grab::Start => {
                        let end = start + length;
                        let new_start = self.snap(start + dt, node, invert).clamp(0.0, end - 1.0 / self.fps);
                        (new_start, end - new_start, drag.track)
                    }
                    Grab::End => {
                        let end = self.snap(start + length + dt, node, invert).max(start + 1.0 / self.fps);
                        (start, end - start, drag.track)
                    }
                    Grab::Ruler | Grab::FadeIn | Grab::FadeOut | Grab::Marquee => unreachable!(),
                };
                if let Some(active) = &mut self.drag {
                    active.moved = true;
                    active.preview = Some(preview);
                }
                self.redraw(cx);
            }
            Hit::FingerUp(fe) => {
                let Some(mut drag) = self.drag.take().filter(|drag| !drag.released) else { return };
                if drag.grab == Grab::Marquee {
                    if drag.moved {
                        let hits = self.marquee_selection(drag.origin, drag.corner);
                        cx.widget_action(uid, TimelineAction::SelectMany(hits));
                    } else {
                        let t = self.t_of(fe.abs.x).clamp(0.0, self.content_seconds());
                        cx.widget_action(uid, TimelineAction::Scrub(t));
                    }
                    self.redraw(cx);
                    return;
                }
                if let (Some(_), Some(name)) = (self.snapped.take(), self.snap_name.take()) {
                    cx.widget_action(uid, TimelineAction::Snapped(name));
                }
                if let Some((node, fade_in, fade_out)) = self.fade_preview.clone() {
                    if !fe.cancelled && drag.moved {
                        drag.released = true;
                        self.drag = Some(drag);
                        cx.widget_action(uid, TimelineAction::Fade { node, fade_in, fade_out });
                    } else {
                        self.fade_preview = None;
                    }
                    self.redraw(cx);
                    return;
                }
                if fe.cancelled || !drag.moved || drag.grab == Grab::Ruler {
                    self.redraw(cx);
                    return;
                }
                let (Some(node), Some((start, length, track))) = (drag.node.take(), drag.preview) else { return };
                let action = match drag.grab {
                    Grab::Body if (start - drag.span.0).abs() > 1e-9 || track != drag.track => Some(TimelineAction::Move { node: node.clone(), start, track }),
                    Grab::Start | Grab::End if (start - drag.span.0).abs() > 1e-9 || (length - drag.span.1).abs() > 1e-9 => Some(TimelineAction::Trim { node: node.clone(), start, length }),
                    _ => None,
                };
                if let Some(action) = action {
                    drag.released = true;
                    drag.node = Some(node);
                    self.drag = Some(drag);
                    cx.widget_action(uid, action);
                }
                self.redraw(cx);
            }
            Hit::FingerScroll(fe) => {
                if fe.modifiers.control || fe.modifiers.logo {
                    let step = if fe.scroll.y.abs() >= fe.scroll.x.abs() { fe.scroll.y } else { fe.scroll.x };
                    self.zoom_about(self.px_per_sec * (-step * 0.01).exp(), fe.abs.x);
                    cx.widget_action(uid, TimelineAction::Zoomed(self.px_per_sec));
                } else if fe.scroll.x.abs() > fe.scroll.y.abs() || fe.modifiers.shift {
                    let step = if fe.scroll.x.abs() > fe.scroll.y.abs() { fe.scroll.x } else { fe.scroll.y };
                    self.scroll += step / self.px_per_sec;
                    self.clamp_scroll();
                } else {
                    self.scroll_y += fe.scroll.y;
                    self.clamp_scroll();
                }
                self.redraw(cx);
            }
            Hit::KeyDown(ke) => cx.widget_action(uid, TimelineAction::Key(ke)),
            Hit::TextInput(te) if te.was_paste || te.input.contains("://") => cx.widget_action(uid, TimelineAction::Paste(te.input)),
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.rect = rect;
        self.clip_films.draw(cx);
        if self.clip_films.busy() && !self.clip_films.paused {
            self.next_frame = cx.new_next_frame();
        }
        if !self.initialized {
            self.initialized = true;
            self.snapping = true;
            self.skimming = true;
        }
        if self.fit_pending && rect.size.x > HEADER {
            self.fit_now();
        }
        if self.projection.duration > 0.0 && rect.size.x > HEADER {
            if let Some(scroll) = self.pending_scroll.take() {
                self.scroll = scroll;
            }
        }
        if self.zoom_anim.is_some() {
            self.next_frame = cx.new_next_frame();
        }
        self.clamp_scroll();
        let p = makepad_look::palette::get(cx);
        self.draw_bg.color = Self::color(if self.blank { p.panel } else { p.wave_bg }, 1.0);
        self.draw_bg.draw_abs(cx, rect);
        if self.blank {
            return DrawStep::done();
        }
        let (x0, x1) = (rect.pos.x + HEADER, rect.pos.x + rect.size.x);
        let tracks_top = rect.pos.y + RULER;
        let bottom = rect.pos.y + rect.size.y;
        let rows = self.rows();

        // Track grounds: alternate, V1 a little lighter; the gap topped by
        // a rim; muted tracks hatched.
        for (index, row) in rows.iter().enumerate() {
            if row.top > bottom || row.top + row.h < tracks_top {
                continue;
            }
            let ground = match row.kind {
                RowKind::Gap => p.wave_bg,
                RowKind::Video(0) => 0x12161cff,
                _ if index % 2 == 1 => 0x0f1217ff,
                _ => p.wave_bg,
            };
            self.rect(cx, Rect { pos: dvec2(x0, row.top), size: dvec2(x1 - x0, row.h) }, Self::color(ground, 1.0));
            if row.kind == RowKind::Gap {
                self.rect(cx, Rect { pos: dvec2(x0, row.top), size: dvec2(x1 - x0, 1.0) }, Self::color(p.wave_rim, 1.0));
            }
            if let Some((lane, track)) = row.kind.lane_track() {
                let state = self.track_state(lane, track);
                let _ = state;
                if self.silent(lane, track) && matches!(lane, Lane::Audio | Lane::Voice) && !self.is_drop_row(row.kind) {
                    self.shape(cx, Rect { pos: dvec2(x0, row.top), size: dvec2(x1 - x0, row.h) }, 4.0, Self::color(0, 0.0), Self::color(p.wave_grid, 1.0), 0.0, 0.0);
                }
            }
        }
        // In/out wash through the tracks.
        if let (Some(a), Some(b)) = self.marks {
            let (a, b) = (self.x_of(a.min(b)).max(x0), self.x_of(a.max(b)).min(x1));
            if b > a {
                self.rect(cx, Rect { pos: dvec2(a, tracks_top), size: dvec2(b - a, rect.size.y - RULER) }, Self::color(p.accent, 0.05));
            }
        }
        // Past the cut's end.
        let end = self.x_of(self.projection.duration);
        if end < x1 {
            self.rect(cx, Rect { pos: dvec2(end.max(x0), tracks_top), size: dvec2(x1 - end.max(x0), rect.size.y - RULER) }, Self::color(0x000000ff, 0.25));
        }
        // An empty edit: where to drop.
        if self.projection.blocks.is_empty() {
            for (lane, text) in [(Lane::Video, "Drag video, images or an app run here · or press E to add the selected item"), (Lane::Audio, "Drag music here")] {
                if let Some(row) = self.row_of(lane, 0) {
                    let zone = Rect { pos: dvec2(x0 + 8.0, row.top + 4.0), size: dvec2(x1 - x0 - 16.0, row.h - 8.0) };
                    self.shape(cx, zone, 5.0, Self::color(0, 0.0), Self::color(p.wave_grid, 1.0), 1.5, 4.0);
                    let w = advance(&self.draw_text, cx, text);
                    self.text(cx, dvec2(zone.pos.x + (zone.size.x - w) * 0.5, zone.pos.y + (zone.size.y - 11.0) * 0.5), text, zone.size.x, Self::color(p.secondary, 1.0));
                }
            }
        }

        // Clips, bottom to top: blocks, then Fx bands, then transition tabs.
        let clip = (x0, x1);
        let blocks = self.projection.blocks.clone();
        for block in blocks.iter().filter(|block| block.kind != BlockKind::Effect) {
            self.draw_block(cx, block, clip, &p);
        }
        for block in blocks.iter().filter(|block| block.kind == BlockKind::Effect) {
            let (start, length, track) = self.placed(block);
            let r = self.band_rect(track, block.level, start, length);
            let c = Rect { pos: dvec2(r.pos.x.max(x0), r.pos.y), size: dvec2((r.pos.x + r.size.x).min(x1) - r.pos.x.max(x0), r.size.y) };
            if c.size.x <= 0.0 {
                continue;
            }
            let selected = self.is_selected(&block.node);
            // A transition tab in this rail cuts the band (2 px each side):
            // the tab is always on top.
            let cuts: Vec<(f64, f64)> = self.projection.bands.iter().map(|band| self.tab_rect(band.tracks, band.start, band.length))
                .filter(|tab| tab.pos.y < c.pos.y + c.size.y && tab.pos.y + tab.size.y > c.pos.y)
                .map(|tab| (tab.pos.x - 2.0, tab.pos.x + tab.size.x + 2.0)).collect();
            let mut pieces = vec![(c.pos.x, c.pos.x + c.size.x)];
            for (a, b) in cuts {
                pieces = pieces.into_iter().flat_map(|(p0, p1)| {
                    if b <= p0 || a >= p1 { vec![(p0, p1)] } else { [(p0, a), (b, p1)].into_iter().filter(|(q0, q1)| q1 - q0 > 3.0).collect() }
                }).collect();
            }
            // A Generate's band is violet, filled as it generates.
            let (strip_hue, edge_hue) = if block.generate { (0x2a1f47ff, GENERATIVE) } else { (CutRole::EFFECT.strip, CutRole::EFFECT.hue) };
            for (p0, p1) in &pieces {
                self.shape(cx, Rect { pos: dvec2(*p0, c.pos.y), size: dvec2(p1 - p0, c.size.y) }, 0.0, Self::color(strip_hue, 1.0), Self::color(edge_hue, 1.0), 1.0, 3.0);
            }
            if let Some(done) = block.progress {
                let w = (c.size.x * done.clamp(0.0, 1.0)).max(0.0);
                self.rect(cx, Rect { pos: dvec2(c.pos.x + 1.0, c.pos.y + c.size.y - 3.0), size: dvec2(w, 2.0) }, Self::color(GENERATIVE, 1.0));
                self.next_frame = cx.new_next_frame();
            }
            let c = pieces.first().map_or(c, |(p0, p1)| Rect { pos: dvec2(*p0, c.pos.y), size: dvec2(p1 - p0, c.size.y) });
            let chip_right = self.badge(cx, c.pos.x + 22.0, c.pos.y + (c.size.y - 14.0) * 0.5, if block.generate { "GEN" } else { "FX" }, edge_hue, 0x1a0b1aff);
            let _ = chip_right;
            self.name(cx, dvec2(c.pos.x + 26.0, c.pos.y + (c.size.y - 12.0) * 0.5), &block.label, c.size.x - 30.0, Self::color(p.glass_ink, 1.0));
            if selected {
                self.shape(cx, Rect { pos: c.pos - dvec2(1.5, 1.5), size: c.size + dvec2(3.0, 3.0) }, 0.0, Self::color(0, 0.0), Self::color(p.accent, 1.0), 2.0, 4.0);
            }
        }
        for mut band in self.projection.bands.clone() {
            // A tab being dragged at its edge: drawn at its new length.
            if let Some((_, (start, length, _))) = self.drag.as_ref().and_then(|drag| Some((drag.node.as_ref()?, drag.preview?))).filter(|(node, _)| **node == band.node) {
                if band.tracks.0 == band.tracks.1 {
                    // On a cut both edges move together, about the centre.
                    let grow = if (start - band.start).abs() > 1e-9 { band.start - start } else { start + length - (band.start + band.length) };
                    let length = (band.length + 2.0 * grow).max(0.02);
                    band.start += (band.length - length) * 0.5;
                    band.length = length;
                } else {
                    band.start = start;
                    band.length = length;
                }
            }
            let r = self.tab_rect(band.tracks, band.start, band.length);
            let c = Rect { pos: dvec2(r.pos.x.max(x0), r.pos.y), size: dvec2((r.pos.x + r.size.x).min(x1) - r.pos.x.max(x0), r.size.y) };
            if c.size.x <= 0.0 {
                continue;
            }
            if band.tracks.0 != band.tracks.1 {
                // Ticks up and down to both clips.
                let upper = self.block_rect(Lane::Video, band.tracks.1, band.start, band.length);
                let lower = self.block_rect(Lane::Video, band.tracks.0, band.start, band.length);
                // From the tab's left end up into the upper clip, from its
                // right end down into the lower one.
                let hue = Self::color(CutRole::TRANSITION.hue, 1.0);
                let (left, right) = (r.pos.x + 2.0, r.pos.x + r.size.x - 3.0);
                if left >= x0 && left <= x1 {
                    let top = upper.pos.y + upper.size.y - 8.0;
                    self.rect(cx, Rect { pos: dvec2(left, top), size: dvec2(1.0, r.pos.y - top) }, hue);
                }
                if right >= x0 && right <= x1 {
                    let bottom = lower.pos.y + 8.0;
                    self.rect(cx, Rect { pos: dvec2(right, r.pos.y + r.size.y), size: dvec2(1.0, bottom - r.pos.y - r.size.y) }, hue);
                }
            }
            let selected = self.is_selected(&band.node);
            if c.size.x < 20.0 {
                self.shape(cx, Rect { pos: dvec2(c.pos.x + c.size.x * 0.5 - 3.0, c.pos.y + c.size.y * 0.5 - 3.0), size: dvec2(6.0, 6.0) }, 1.0, Self::color(CutRole::TRANSITION.hue, 1.0), Self::color(0x000000ff, 0.3), 1.0, 0.0);
            } else {
                self.shape(cx, c, 0.0, Self::color(CutRole::TRANSITION.hue, 0.9), Self::color(0x000000ff, 0.3), 1.0, 3.0);
                self.icon_xfade.color = Self::color(0x0a0d12ff, 1.0);
                self.icon_xfade.draw_abs(cx, Rect { pos: dvec2(c.pos.x + 4.0, c.pos.y + (c.size.y - 10.0) * 0.5), size: dvec2(10.0, 10.0) });
                if c.size.x >= 56.0 {
                    self.name(cx, dvec2(c.pos.x + 17.0, c.pos.y + (c.size.y - 12.0) * 0.5), &band.label, c.size.x - 20.0, Self::color(0x0a0d12ff, 1.0));
                }
            }
            if selected {
                self.shape(cx, Rect { pos: c.pos - dvec2(1.5, 1.5), size: c.size + dvec2(3.0, 3.0) }, 0.0, Self::color(0, 0.0), Self::color(p.accent, 1.0), 2.0, 4.0);
            }
        }

        // A library item's landing place.
        if let Some(hint) = self.drop_hint.clone() {
            let outline = if hint.tab { CutRole::TRANSITION.hue } else { CutRole::EFFECT.hue };
            for target in &hint.targets {
                if let Some(block) = self.projection.block(target).cloned() {
                    let r = self.block_rect(block.lane, block.track, block.start, block.length);
                    self.shape(cx, Rect { pos: r.pos - dvec2(1.5, 1.5), size: r.size + dvec2(3.0, 3.0) }, 5.0, Self::color(0, 0.0), Self::color(outline, 1.0), 2.0, 5.0);
                }
            }
            // A transition: its tab previewed on the cut point.
            if hint.tab && hint.invalid.is_none() {
                let tab = self.tab_rect((hint.track, hint.track), hint.start, hint.length);
                self.shape(cx, tab, 5.0, Self::color(CutRole::TRANSITION.hue, 0.55), Self::color(CutRole::TRANSITION.hue, 1.0), 1.5, 3.0);
                self.icon_xfade.color = Self::color(0x0a0d12ff, 1.0);
                self.icon_xfade.draw_abs(cx, Rect { pos: dvec2(tab.pos.x + 4.0, tab.pos.y + (tab.size.y - 10.0) * 0.5), size: dvec2(10.0, 10.0) });
            }
            let r = self.block_rect(hint.lane, hint.track, hint.start, hint.length);
            let role = role_of_lane(hint.lane);
            if hint.invalid.is_some() && r.size.x > 0.0 {
                self.shape(cx, r, 5.0, Self::color(p.danger_wash, 0.6), Self::color(p.danger, 1.0), 1.5, 4.0);
            }
            if hint.targets.is_empty() && r.size.x > 0.0 && hint.invalid.is_none() {
                self.shape(cx, r, 5.0, Self::color(role.wash, 1.0), Self::color(role.hue, 1.0), 1.5, 4.0);
                self.name(cx, dvec2(r.pos.x.max(x0) + 6.0, r.pos.y + 3.0), &hint.label, r.size.x - 10.0, Self::color(p.glass_ink, 1.0));
            }
            let row = match hint.lane { Lane::Video => format!("V{}", hint.track + 1), Lane::Audio => format!("A{}", hint.track + 1), Lane::Voice => "VO".into(), Lane::Captions => "CC".into() };
            match &hint.invalid {
                Some(reason) => self.tag(cx, dvec2(r.pos.x, 0.0), reason, p.danger_wash, p.text),
                None => self.tag(cx, dvec2(r.pos.x, 0.0), &format!("{} · {row}", timecode_full(hint.start, self.fps)), p.raised, p.label),
            }
        }
        // Marker lines through the tracks.
        for marker in self.markers.clone() {
            let x = self.x_of(marker);
            if x >= x0 && x <= x1 {
                let mut y = tracks_top;
                while y < bottom {
                    self.rect(cx, Rect { pos: dvec2(x, y), size: dvec2(1.0, 4.0) }, Self::color(CutRole::MARKER, 0.25));
                    y += 7.0;
                }
            }
        }
        // Track headers over the canvas (sticky left).
        for row in rows.clone() {
            if row.top > bottom || row.top + row.h < tracks_top {
                continue;
            }
            self.draw_header(cx, row, &p);
        }
        self.rect(cx, Rect { pos: dvec2(rect.pos.x, bottom - 1.0), size: dvec2(HEADER, 1.0) }, Self::color(p.separator, 1.0));
        // The ruler (sticky top) with its layers.
        self.draw_ruler(cx, &p);
        // The marquee.
        if let Some(drag) = self.drag.as_ref().filter(|drag| drag.grab == Grab::Marquee && drag.moved) {
            let (a, b) = (drag.origin, drag.corner);
            let r = Rect { pos: dvec2(a.x.min(b.x).max(x0), a.y.min(b.y).max(tracks_top)), size: dvec2((a.x - b.x).abs(), (a.y - b.y).abs()) };
            self.shape(cx, r, 0.0, Self::color(0xffffffff, 0.06), Self::color(p.glass_white, 0.6), 1.0, 0.0);
        }
        // Snap line and triangles.
        if let Some(t) = self.snapped {
            let x = self.x_of(t);
            if x >= x0 && x <= x1 {
                self.rect(cx, Rect { pos: dvec2(x - 0.5, tracks_top), size: dvec2(1.0, rect.size.y - RULER) }, Self::color(p.glass_ink, 1.0));
                for i in 0..5 {
                    let w = (5 - i) as f64 * 2.0;
                    self.rect(cx, Rect { pos: dvec2(x - w * 0.5, tracks_top - 6.0 + i as f64), size: dvec2(w, 1.0) }, Self::color(p.glass_ink, 1.0));
                }
            }
        }
        // Skimmer and its time bubble.
        if let Some(t) = self.skim.filter(|_| self.drag.is_none()) {
            let x = self.x_of(t);
            if x >= x0 && x <= x1 {
                self.rect(cx, Rect { pos: dvec2(x - 0.5, tracks_top), size: dvec2(1.0, rect.size.y - RULER) }, Self::color(p.wave_head, 0.45));
                let text = timecode_full(t, self.fps);
                let w = advance(&self.draw_text, cx, &text) + 10.0;
                let bubble = Rect { pos: dvec2((x - w * 0.5).clamp(x0, x1 - w), rect.pos.y + RULER - 16.0), size: dvec2(w, 14.0) };
                self.shape(cx, bubble, 0.0, Self::color(p.raised, 0.95), Self::color(p.raised, 1.0), 0.0, 3.0);
                self.text(cx, dvec2(bubble.pos.x + 5.0, bubble.pos.y + 1.5), &text, w, Self::color(p.label, 1.0));
            }
        }
        for (pos, text, fill, ink) in std::mem::take(&mut self.later_tags) {
            self.draw_tag(cx, pos, &text, fill, ink);
        }
        // The playhead over everything.
        let x = self.x_of(self.playhead);
        if x >= x0 - 7.0 && x <= x1 + 7.0 {
            let dragging = self.drag.as_ref().is_some_and(|drag| drag.grab == Grab::Ruler);
            self.rect(cx, Rect { pos: dvec2(x - 0.5, rect.pos.y + 12.0), size: dvec2(if dragging { 2.0 } else { 1.0 }, rect.size.y - 12.0) }, Self::color(p.accent, 1.0));
            self.shape(cx, Rect { pos: dvec2(x - 6.5, rect.pos.y + 1.0), size: dvec2(13.0, 15.0) }, 2.0, Self::color(p.accent, 1.0), Self::color(p.accent, 1.0), 0.0, 0.0);
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::{tick_steps, timecode_full, timecode_parts};

    #[test]
    fn timecodes_are_one_format() {
        assert_eq!(timecode_full(0.0, 60.0), "00:00:00:00");
        assert_eq!(timecode_full(1.5, 60.0), "00:00:01:30");
        assert_eq!(timecode_full(3600.0, 30.0), "01:00:00:00");
        assert_eq!(timecode_parts(1.0, 60.0), ("00:00:".to_string(), "01:00".to_string()));
        assert_eq!(timecode_parts(0.0, 60.0), ("00:00:00:".to_string(), "00".to_string()));
        assert_eq!(timecode_parts(125.0, 60.0), ("00:".to_string(), "02:05:00".to_string()));
    }

    #[test]
    fn the_tick_ladder_keeps_labels_apart() {
        // 60 px/s: majors every 2 s, minors every 15 f (design §4.5).
        let (major, minor) = tick_steps(60.0, 60.0);
        assert_eq!(major, 2.0);
        assert!((minor - 0.25).abs() < 1e-9);
        let (major, _) = tick_steps(200.0, 60.0);
        assert_eq!(major, 0.5);
        let (major, minor) = tick_steps(4.0, 60.0);
        assert_eq!((major, minor), (30.0, 5.0));
    }
}
