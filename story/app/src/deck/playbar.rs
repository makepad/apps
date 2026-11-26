//! The play bar: a slim strip under the DECK picture that shows what a
//! slide is made of in time. A ruler for the slide's length with a playhead,
//! a marker for every hold (a beat Next releases) or, on an app slide, for
//! every step of its demo; play or pause and a loop toggle; and small
//! badges (how many steps, animating, app, static).
//!
//! It is the presenter's: drawn by the deck view
//! in a strip of its own, never on the picture, never in the picture-alone
//! window and never in the output window. It owns no clock: the model is
//! read from the player at each draw, a press becomes an [`Action`] the
//! view runs as a player command, and the bar redraws only when the view
//! does (the player's own frames while it plays, a pointer event on the bar).

use super::shapes::DrawDeckUi;
use makepad_widgets::*;

/// The bar's height; the view keeps a strip this tall plus
/// [`super::shapes::STRIP_GAP`] under the picture.
pub const HEIGHT: f64 = 26.0;
const PAD: f64 = 6.0;
const GAP: f64 = 4.0;
const BUTTON_HEIGHT: f64 = 20.0;
const TRACK_HEIGHT: f64 = 6.0;
const MARKER: f64 = 8.0;
/// A press this near a marker (px along the ruler) is the marker's.
const MARKER_REACH: f64 = 7.0;
/// A press on a marker that moves this far (px) is a scrub instead.
const DRAG_SLOP: f64 = 3.0;
/// Below this ruler length (px) the badges give way.
const MIN_TRACK: f64 = 120.0;
const TEXT_WIDTH: f64 = 5.6;
const READOUT_WIDTH: f64 = 92.0;

/// Where the shown slide's story is, for the play button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Story {
    /// The timeline runs.
    Moving,
    /// Stopped by the bar (a pause, or a scrub).
    Paused,
    /// At a beat, waiting for Next to release it.
    Holding,
    /// Every beat is released and the cut has reached its held end.
    Ended,
}

/// A slide's demo, as the bar needs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DemoInput {
    pub step: usize,
    pub total: usize,
    pub playing: bool,
    pub seeking: Option<usize>,
}

/// What the player says of the shown slide.
#[derive(Clone, Copy, Debug)]
pub struct Input<'a> {
    /// The cut's length in seconds (None: not known yet).
    pub duration: Option<f64>,
    pub beats: &'a [f64],
    /// The story time now.
    pub time: f64,
    pub story: Story,
    pub looped: bool,
    /// Whether the picture can change with time: Some(false) it can,
    /// Some(true) it cannot, None not known yet.
    pub independent: Option<bool>,
    /// The slide hosts a live app.
    pub app: bool,
    pub demo: Option<DemoInput>,
    /// The deck is being presented: scrubbing would move the output.
    pub presenting: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The beginning of a slide.
    Start,
    /// The settled final frame of a slide.
    End,
    /// The hold of beat `n` (0-based).
    Beat(usize),
    /// The start of demo step `n` (0-based).
    Step(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marker {
    /// Where it is on the ruler, in the ruler's units (seconds, or steps).
    pub at: f64,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    Steps(usize),
    Animating,
    App,
    Static,
}

impl Badge {
    fn text(self) -> String {
        match self {
            Badge::Steps(1) => "1 step".into(),
            Badge::Steps(n) => format!("{n} steps"),
            Badge::Animating => "animating".into(),
            Badge::App => "app".into(),
            Badge::Static => "static".into(),
        }
    }
}

/// What the play button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayButton {
    Pause,
    Play,
    /// Release the beat the story waits at (what Next does there).
    Release,
    Replay,
}

impl PlayButton {
    fn text(self) -> &'static str {
        match self {
            PlayButton::Pause => "Pause",
            PlayButton::Play | PlayButton::Release => "Play",
            PlayButton::Replay => "Replay",
        }
    }
}

/// Everything the bar draws and answers presses with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    /// The ruler's length in its units (seconds, or the demo's steps);
    /// 0: nothing to measure yet.
    pub span: f64,
    pub markers: Vec<Marker>,
    pub head: f64,
    /// The marker the story stands at or last passed.
    pub current: Option<usize>,
    pub button: Option<PlayButton>,
    pub looped: bool,
    pub demo: bool,
    pub badges: Vec<Badge>,
    /// Presenting: the ruler cannot be scrubbed (the markers still jump).
    pub locked: bool,
    /// The story was moving (a scrub resumes it on release).
    pub moving: bool,
}

pub fn model(input: &Input) -> Model {
    let mut badges = Vec::new();
    let mut model = Model { locked: input.presenting, ..Model::default() };
    if let Some(demo) = input.demo {
        let total = demo.total;
        model.demo = true;
        model.span = total as f64;
        model.markers = (0..total).map(|step| Marker { at: step as f64, kind: Kind::Step(step) }).collect();
        model.head = demo.seeking.unwrap_or(demo.step).min(total) as f64;
        model.current = (total > 0).then(|| demo.step.min(total - 1));
        model.moving = demo.playing;
        model.button = Some(if demo.playing { PlayButton::Pause } else if demo.step >= total { PlayButton::Replay } else { PlayButton::Play });
        if total > 0 { badges.push(Badge::Steps(total)); }
    } else {
        let span = input.duration.unwrap_or(0.0).max(0.0);
        model.span = span;
        model.head = input.time.clamp(0.0, span);
        // The start and the end are stops of a slide, like its beats.
        if span > 0.0 {
            if input.beats.first().is_none_or(|first| *first > 1e-6) { model.markers.push(Marker { at: 0.0, kind: Kind::Start }); }
            model.markers.extend(input.beats.iter().enumerate().map(|(index, at)| Marker { at: *at, kind: Kind::Beat(index) }));
            if input.beats.last().is_none_or(|last| *last < span - 1e-6) { model.markers.push(Marker { at: span, kind: Kind::End }); }
        } else {
            model.markers.extend(input.beats.iter().enumerate().map(|(index, at)| Marker { at: *at, kind: Kind::Beat(index) }));
        }
        if !input.beats.is_empty() {
            badges.push(Badge::Steps(model.markers.iter().filter(|marker| marker.kind != Kind::End).count()));
        }
        model.current = model.markers.iter().rposition(|marker| marker.at <= model.head + 1e-6);
        model.moving = input.story == Story::Moving;
        model.button = (span > 0.0).then_some(match input.story {
            Story::Moving => PlayButton::Pause,
            Story::Paused => PlayButton::Play,
            Story::Holding => PlayButton::Release,
            Story::Ended => PlayButton::Replay,
        });
        model.looped = input.looped;
    }
    if input.app { badges.push(Badge::App); }
    match input.independent {
        Some(true) => badges.push(Badge::Static),
        Some(false) if model.span > 0.0 => badges.push(Badge::Animating),
        _ => {}
    }
    model.badges = badges;
    model
}

/// The ruler's x for `at` (in the ruler's units) on `track`.
pub fn x_of(track: Rect, span: f64, at: f64) -> f64 {
    if span <= 0.0 { return track.pos.x; }
    track.pos.x + track.size.x * (at / span).clamp(0.0, 1.0)
}

/// The ruler's units at pointer x on `track` (clamped to the ruler).
pub fn at_of(track: Rect, span: f64, x: f64) -> f64 {
    if track.size.x <= 0.0 { return 0.0; }
    ((x - track.pos.x) / track.size.x).clamp(0.0, 1.0) * span
}

/// The demo step the ruler position `at` (in steps) is in.
pub fn step_at(at: f64, total: usize) -> usize {
    if total == 0 { return 0; }
    (at.floor().max(0.0) as usize).min(total - 1)
}

/// The marker nearest pointer x within reach, if any.
pub fn marker_near(model: &Model, track: Rect, x: f64) -> Option<usize> {
    model.markers.iter().enumerate()
        .map(|(index, marker)| (index, (x_of(track, model.span, marker.at) - x).abs()))
        .filter(|(_, distance)| *distance <= MARKER_REACH)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Play,
    Loop,
}

/// Where the bar's parts are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub bar: Rect,
    pub play: Option<Rect>,
    pub looper: Option<Rect>,
    pub track: Rect,
    pub readout: Rect,
    pub badges: Vec<(Badge, Rect)>,
}

fn badge_width(badge: Badge) -> f64 {
    badge.text().chars().count() as f64 * TEXT_WIDTH + 12.0
}

pub fn layout(bar: Rect, model: &Model) -> Layout {
    let row_y = bar.pos.y + (bar.size.y - BUTTON_HEIGHT) * 0.5;
    let mut left = bar.pos.x + PAD;
    let mut play = None;
    let mut looper = None;
    if model.button.is_some() && !model.locked {
        play = Some(Rect { pos: dvec2(left, row_y), size: dvec2(46.0, BUTTON_HEIGHT) });
        left += 46.0 + GAP;
        if !model.demo {
            looper = Some(Rect { pos: dvec2(left, row_y), size: dvec2(38.0, BUTTON_HEIGHT) });
            left += 38.0 + GAP;
        }
    }
    let mut right = bar.pos.x + bar.size.x - PAD;
    let mut badges = Vec::new();
    for badge in model.badges.iter().rev() {
        let width = badge_width(*badge);
        // The ruler keeps its room: badges give way from the quietest.
        if right - width - GAP - READOUT_WIDTH - (left + GAP) < MIN_TRACK { break; }
        right -= width;
        badges.push((*badge, Rect { pos: dvec2(right, row_y + 1.0), size: dvec2(width, BUTTON_HEIGHT - 2.0) }));
        right -= GAP;
    }
    badges.reverse();
    right -= READOUT_WIDTH;
    let readout = Rect { pos: dvec2(right, bar.pos.y), size: dvec2(READOUT_WIDTH, bar.size.y) };
    right -= GAP;
    let track_x = left + MARKER * 0.5;
    let track_end = (right - MARKER * 0.5).max(track_x);
    let track = Rect { pos: dvec2(track_x, bar.pos.y + (bar.size.y - TRACK_HEIGHT) * 0.5), size: dvec2(track_end - track_x, TRACK_HEIGHT) };
    Layout { bar, play, looper, track, readout, badges }
}

/// What a press on the bar asks of the view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// The play button.
    Play(PlayButton),
    Loop(bool),
    /// Jump to marker `n` of the model.
    Marker(usize),
    /// The playhead is dragged to this place on the ruler (its units).
    Scrub(f64),
    /// The drag ended at this place; `resume`: the story was moving when it began.
    Release { at: f64, resume: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Press {
    Part(Part),
    /// On marker `n`, at pointer x.
    Marker(usize, f64),
    Scrub { resume: bool },
}

/// What the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hot {
    Part(Part),
    Marker(usize),
}

/// The bar: the parts as last drawn and the pointer's state.
#[derive(Default)]
pub struct PlayBar {
    laid: Layout,
    model: Model,
    shown: bool,
    hover: Option<Vec2d>,
    press: Option<Press>,
    /// While dragging: the playhead the pointer holds, in ruler units.
    dragging: Option<f64>,
}

impl PlayBar {
    fn part_at(&self, at: Vec2d) -> Option<Part> {
        if self.laid.play.is_some_and(|rect| rect.contains(at)) { return Some(Part::Play); }
        if self.laid.looper.is_some_and(|rect| rect.contains(at)) { return Some(Part::Loop); }
        None
    }

    /// The ruler's hit area: the track with room for the playhead's grip.
    fn ruler_contains(&self, at: Vec2d) -> bool {
        let track = self.laid.track;
        let area = Rect { pos: dvec2(track.pos.x - MARKER * 0.5, self.laid.bar.pos.y), size: dvec2(track.size.x + MARKER, self.laid.bar.size.y) };
        self.model.span > 0.0 && area.contains(at)
    }

    fn hot(&self, at: Vec2d) -> Option<Hot> {
        if let Some(part) = self.part_at(at) { return Some(Hot::Part(part)); }
        if self.ruler_contains(at) {
            return marker_near(&self.model, self.laid.track, at.x).map(Hot::Marker);
        }
        None
    }

    /// Whether `at` is on the bar as last drawn: the picture's pointer
    /// handling leaves it alone.
    pub fn captures(&self, at: Vec2d) -> bool {
        self.shown && self.laid.bar.contains(at)
    }

    pub fn cursor(&self, at: Vec2d) -> Option<MouseCursor> {
        self.captures(at).then(|| if self.hot(at).is_some() || (self.ruler_contains(at) && !self.model.locked) { MouseCursor::Hand } else { MouseCursor::Default })
    }

    /// Where marker `index` of the model drawn last is, and what it is.
    pub fn marker_at(&self, index: usize) -> Option<(f64, Kind)> {
        self.model.markers.get(index).map(|marker| (marker.at, marker.kind))
    }

    pub fn pressing(&self) -> bool {
        self.press.is_some()
    }

    /// The pointer over the view (None: it left). True when what is lit changed.
    pub fn hover(&mut self, at: Option<Vec2d>) -> bool {
        let before = self.hover.and_then(|at| self.hot(at));
        self.hover = at;
        before != at.and_then(|at| self.hot(at))
    }

    /// A press at `at`: whether the bar took it, and what it asks.
    pub fn press(&mut self, at: Vec2d) -> (bool, Option<Action>) {
        if !self.captures(at) { return (false, None); }
        self.press = None;
        if let Some(part) = self.part_at(at) {
            self.press = Some(Press::Part(part));
        } else if self.ruler_contains(at) {
            if let Some(index) = marker_near(&self.model, self.laid.track, at.x) {
                self.press = Some(Press::Marker(index, at.x));
            } else if !self.model.locked {
                self.press = Some(Press::Scrub { resume: self.model.moving });
                return (true, Some(self.scrub_to(at.x)));
            }
        }
        (true, None)
    }

    fn scrub_to(&mut self, x: f64) -> Action {
        let at = at_of(self.laid.track, self.model.span, x);
        self.dragging = Some(at);
        Action::Scrub(at)
    }

    /// The pointer moved with the button down.
    pub fn drag(&mut self, at: Vec2d) -> Option<Action> {
        match self.press? {
            Press::Marker(_, x0) if !self.model.locked && (at.x - x0).abs() > DRAG_SLOP => {
                self.press = Some(Press::Scrub { resume: self.model.moving });
                Some(self.scrub_to(at.x))
            }
            Press::Scrub { .. } => Some(self.scrub_to(at.x)),
            _ => None,
        }
    }

    /// The release: the action when it ended where it began (a part), on the
    /// marker, or a drag's end.
    pub fn release(&mut self, at: Vec2d) -> Option<Action> {
        let press = self.press.take()?;
        let dragged = self.dragging.take();
        match press {
            Press::Part(part) if self.part_at(at) == Some(part) => match part {
                Part::Play => self.model.button.map(Action::Play),
                Part::Loop => Some(Action::Loop(!self.model.looped)),
            },
            Press::Marker(index, _) if self.hot(at) == Some(Hot::Marker(index)) => Some(Action::Marker(index)),
            Press::Scrub { resume } => Some(Action::Release { at: dragged.unwrap_or_else(|| at_of(self.laid.track, self.model.span, at.x)), resume }),
            _ => None,
        }
    }

    /// The bar is not drawn this pass: the pointer reaches the picture.
    pub fn hide(&mut self) {
        self.shown = false;
        self.press = None;
        self.dragging = None;
    }

    /// Draw the bar in `bar`. `ui` draws the filled shapes, `ink` the text
    /// in white and `dim` in a quieter tone.
    pub fn draw(&mut self, cx: &mut Cx2d, ui: &mut DrawDeckUi, ink: &mut DrawText, dim: &mut DrawText, bar: Rect, model: Model) {
        let palette = makepad_look::palette::get(cx);
        let accent = Vec4f::from_u32(palette.accent);
        self.laid = layout(bar, &model);
        self.model = model;
        self.shown = true;
        let model = &self.model;
        let laid = &self.laid;
        let hot = self.hover.and_then(|at| self.hot(at));
        ui.fill(cx, bar, vec4f(0.04, 0.04, 0.05, 0.82), 8.0);
        for (rect, part) in [(laid.play, Part::Play), (laid.looper, Part::Loop)] {
            let Some(rect) = rect else { continue };
            let lit = hot == Some(Hot::Part(part)) || self.press == Some(Press::Part(part));
            let on = part == Part::Loop && model.looped;
            let fill = if on { accent } else if lit { vec4f(1.0, 1.0, 1.0, 0.28) } else { vec4f(1.0, 1.0, 1.0, 0.12) };
            ui.fill(cx, rect, fill, 5.0);
            let text = match part {
                Part::Play => model.button.map_or("", PlayButton::text),
                Part::Loop => "Loop",
            };
            let width = text.chars().count() as f64 * 6.0;
            let pos = dvec2(rect.pos.x + (rect.size.x - width) * 0.5, rect.pos.y + 4.0);
            if on { ink.draw_abs(cx, pos, text) } else { dim.draw_abs(cx, pos, text) }
        }
        // The ruler.
        let track = laid.track;
        ui.fill(cx, track, vec4f(1.0, 1.0, 1.0, 0.16), 3.0);
        let mid = track.pos.y + track.size.y * 0.5;
        let head = self.dragging.unwrap_or(model.head);
        if model.span > 0.0 && track.size.x > 1.0 {
            let head_x = x_of(track, model.span, head);
            let played = Rect { pos: track.pos, size: dvec2(head_x - track.pos.x, track.size.y) };
            if played.size.x > 0.5 { ui.fill(cx, played, fade(accent, 0.8), 3.0); }
            for (index, marker) in model.markers.iter().enumerate() {
                let x = x_of(track, model.span, marker.at);
                let current = model.current == Some(index);
                let lit = hot == Some(Hot::Marker(index));
                let passed = marker.at <= head + 1e-6;
                let color = if current { vec4f(1.0, 1.0, 1.0, 1.0) } else if lit { vec4f(1.0, 1.0, 1.0, 0.9) } else if passed { vec4f(1.0, 1.0, 1.0, 0.65) } else { vec4f(1.0, 1.0, 1.0, 0.4) };
                let size = if current || lit { MARKER + 2.0 } else { MARKER };
                // A current marker is ringed in the accent.
                if current { ui.fill(cx, Rect { pos: dvec2(x - size * 0.5 - 1.5, mid - size * 0.5 - 1.5), size: dvec2(size + 3.0, size + 3.0) }, accent, 3.0); }
                ui.fill(cx, Rect { pos: dvec2(x - size * 0.5, mid - size * 0.5), size: dvec2(size, size) }, color, 2.0);
            }
            ui.fill(cx, Rect { pos: dvec2(head_x - 1.0, bar.pos.y + 3.0), size: dvec2(2.0, bar.size.y - 6.0) }, vec4f(1.0, 1.0, 1.0, 1.0), 1.0);
        }
        // The readout: the hovered marker, else where the playhead is.
        let readout = match hot {
            Some(Hot::Marker(index)) => marker_text(model, index),
            _ if model.demo => if model.span > 0.0 { format!("step {} of {}", model.current.map_or(0, |step| step + 1), model.markers.len()) } else { String::new() },
            _ if model.span > 0.0 => format!("{:.1} / {:.1} s", head, model.span),
            _ => "loading".to_string(),
        };
        let width = readout.chars().count() as f64 * TEXT_WIDTH;
        dim.draw_abs(cx, dvec2(laid.readout.pos.x + (laid.readout.size.x - width).max(0.0), bar.pos.y + 7.0), &readout);
        for (badge, rect) in &laid.badges {
            let loud = matches!(badge, Badge::Animating);
            ui.fill(cx, *rect, if loud { fade(accent, 0.5) } else { vec4f(1.0, 1.0, 1.0, 0.1) }, 8.0);
            let text = badge.text();
            let text_width = text.chars().count() as f64 * TEXT_WIDTH;
            let pos = dvec2(rect.pos.x + (rect.size.x - text_width) * 0.5, rect.pos.y + 3.0);
            if loud { ink.draw_abs(cx, pos, &text) } else { dim.draw_abs(cx, pos, &text) }
        }
    }
}

fn fade(color: Vec4f, alpha: f32) -> Vec4f {
    vec4f(color.x, color.y, color.z, alpha)
}

/// What a marker is, for the readout while the pointer is on it.
pub fn marker_text(model: &Model, index: usize) -> String {
    let Some(marker) = model.markers.get(index) else { return String::new() };
    match marker.kind {
        Kind::Start => "step 1 · start".to_string(),
        Kind::End => "end".to_string(),
        Kind::Beat(_) => format!("step {} · {:.1} s", index + 1, marker.at),
        Kind::Step(step) => format!("demo step {}", step + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(beats: &'a [f64]) -> Input<'a> {
        Input { duration: Some(8.0), beats, time: 0.0, story: Story::Moving, looped: false, independent: Some(false), app: false, demo: None, presenting: false }
    }

    fn bar() -> Rect {
        Rect { pos: dvec2(100.0, 500.0), size: dvec2(800.0, HEIGHT) }
    }

    /// A bar as the view draws it, without a draw pass.
    fn bar_of(model: Model) -> PlayBar {
        let laid = layout(bar(), &model);
        PlayBar { laid, model, shown: true, ..PlayBar::default() }
    }

    #[test]
    fn markers_are_the_start_and_every_hold() {
        let model = model(&input(&[2.0, 4.5, 7.0]));
        let kinds: Vec<_> = model.markers.iter().map(|m| (m.at, m.kind)).collect();
        assert_eq!(kinds, [(0.0, Kind::Start), (2.0, Kind::Beat(0)), (4.5, Kind::Beat(1)), (7.0, Kind::Beat(2)), (8.0, Kind::End)]);
        assert_eq!(model.span, 8.0);
        assert!(model.badges.contains(&Badge::Steps(4)));
        // A hold at 0 is the start itself; a slide without holds has no markers.
        let at_zero = super::model(&input(&[0.0, 3.0]));
        assert_eq!(at_zero.markers.len(), 3);
        // A slide without holds has its start and its end.
        let plain: Vec<_> = super::model(&input(&[])).markers.iter().map(|m| (m.at, m.kind)).collect();
        assert_eq!(plain, [(0.0, Kind::Start), (8.0, Kind::End)]);
        assert!(!super::model(&input(&[])).badges.iter().any(|b| matches!(b, Badge::Steps(_))));
    }

    #[test]
    fn the_current_step_is_the_last_marker_the_playhead_has_reached() {
        let mut state = input(&[2.0, 4.5, 7.0]);
        for (time, current) in [(0.0, 0), (1.9, 0), (2.0, 1), (4.0, 1), (4.5, 2), (7.5, 3)] {
            state.time = time;
            assert_eq!(model(&state).current, Some(current), "{time}");
        }
    }

    #[test]
    fn the_arrows_stops_show_on_the_bar_for_a_slide_sitting_at_its_start_or_its_end() {
        // The arrows walk the start, every beat and the end; the head stands on the
        // marker of the stop the story sits at, a looping slide sitting at its end too.
        let mut state = input(&[2.0, 4.5]);
        let stops: Vec<f64> = model(&state).markers.iter().map(|m| m.at).collect();
        assert_eq!(stops, [0.0, 2.0, 4.5, 8.0]);
        for (index, at) in stops.iter().enumerate() {
            state.time = *at;
            state.story = if index == stops.len() - 1 { Story::Ended } else { Story::Holding };
            let shown = model(&state);
            assert_eq!((shown.head, shown.current), (*at, Some(index)), "stop {index}");
        }
        state.looped = true;
        let sitting = model(&state);
        assert_eq!((sitting.markers[sitting.current.unwrap()].kind, sitting.button), (Kind::End, Some(PlayButton::Replay)));
        // Left from the end moves the head back one marker at a time.
        state.story = Story::Holding;
        state.time = 4.5;
        let back = model(&state);
        assert_eq!(back.markers[back.current.unwrap()].kind, Kind::Beat(1));
    }

    #[test]
    fn an_app_slide_shows_its_demo_steps_as_markers() {
        let mut state = input(&[]);
        state.app = true;
        state.demo = Some(DemoInput { step: 2, total: 5, playing: false, seeking: None });
        let model = model(&state);
        assert_eq!(model.span, 5.0);
        assert_eq!(model.markers.iter().map(|m| m.kind).collect::<Vec<_>>(), (0..5).map(Kind::Step).collect::<Vec<_>>());
        assert_eq!((model.head, model.current, model.button), (2.0, Some(2), Some(PlayButton::Play)));
        assert!(model.badges.contains(&Badge::App) && model.badges.contains(&Badge::Steps(5)));
        state.demo = Some(DemoInput { step: 5, total: 5, playing: false, seeking: None });
        let done = super::model(&state);
        assert_eq!((done.current, done.button), (Some(4), Some(PlayButton::Replay)));
        state.demo = Some(DemoInput { step: 1, total: 5, playing: true, seeking: Some(3) });
        let seeking = super::model(&state);
        assert_eq!((seeking.head, seeking.button), (3.0, Some(PlayButton::Pause)));
        assert_eq!((step_at(0.0, 5), step_at(2.99, 5), step_at(5.0, 5), step_at(9.0, 5)), (0, 2, 4, 4));
    }

    #[test]
    fn badges_say_static_animating_and_app() {
        let mut state = input(&[1.0]);
        assert!(model(&state).badges.contains(&Badge::Animating));
        state.independent = Some(true);
        let still = model(&state);
        assert!(still.badges.contains(&Badge::Static) && !still.badges.contains(&Badge::Animating));
        state.independent = None;
        let unknown = model(&state);
        assert!(!unknown.badges.contains(&Badge::Static) && !unknown.badges.contains(&Badge::Animating), "not claimed before it is known");
        state.app = true;
        assert!(model(&state).badges.contains(&Badge::App));
    }

    #[test]
    fn the_play_button_follows_the_story() {
        let mut state = input(&[2.0]);
        for (story, button) in [(Story::Moving, PlayButton::Pause), (Story::Paused, PlayButton::Play), (Story::Holding, PlayButton::Release), (Story::Ended, PlayButton::Replay)] {
            state.story = story;
            assert_eq!(model(&state).button, Some(button));
        }
        state.duration = None;
        assert_eq!(model(&state).button, None, "no cut to play yet");
    }

    #[test]
    fn time_and_x_map_both_ways() {
        let track = Rect { pos: dvec2(100.0, 0.0), size: dvec2(400.0, 6.0) };
        assert_eq!(x_of(track, 8.0, 0.0), 100.0);
        assert_eq!(x_of(track, 8.0, 2.0), 200.0);
        assert_eq!(x_of(track, 8.0, 8.0), 500.0);
        assert_eq!(x_of(track, 8.0, 99.0), 500.0);
        assert_eq!(at_of(track, 8.0, 300.0), 4.0);
        assert_eq!(at_of(track, 8.0, 0.0), 0.0);
        assert_eq!(at_of(track, 8.0, 900.0), 8.0);
        for t in [0.0, 1.3, 4.5, 8.0] {
            assert!((at_of(track, 8.0, x_of(track, 8.0, t)) - t).abs() < 1e-9);
        }
    }

    #[test]
    fn the_parts_sit_in_the_bar_and_the_ruler_keeps_its_room() {
        let mut state = input(&[2.0, 4.5]);
        state.app = true;
        state.independent = Some(true);
        let model = model(&state);
        for width in [900.0, 500.0, 330.0] {
            let rect = Rect { pos: dvec2(10.0, 20.0), size: dvec2(width, HEIGHT) };
            let laid = layout(rect, &model);
            let mut parts: Vec<Rect> = [laid.play, laid.looper].into_iter().flatten().chain(laid.badges.iter().map(|b| b.1)).collect();
            parts.push(laid.track);
            parts.push(laid.readout);
            for (i, a) in parts.iter().enumerate() {
                assert!(a.pos.x >= rect.pos.x - 0.01 && a.pos.x + a.size.x <= rect.pos.x + rect.size.x + 0.01, "{width}: {a:?}");
                for b in &parts[i + 1..] {
                    assert!(a.pos.x + a.size.x <= b.pos.x + 0.01 || b.pos.x + b.size.x <= a.pos.x + 0.01, "{width}: {a:?} {b:?}");
                }
            }
            assert!(laid.track.size.x >= MIN_TRACK - 20.0, "{width}: {:?}", laid.track);
        }
        // A narrow bar drops badges rather than the ruler.
        assert!(layout(Rect { pos: dvec2(0.0, 0.0), size: dvec2(330.0, HEIGHT) }, &model).badges.len() < model.badges.len());
    }

    #[test]
    fn a_click_on_a_marker_jumps_to_it() {
        let model = model(&input(&[2.0, 4.5, 7.0]));
        let mut bar = bar_of(model.clone());
        let track = bar.laid.track;
        let on = |t: f64| dvec2(x_of(track, 8.0, t), track.pos.y + 3.0);
        assert_eq!(bar.press(on(4.5)), (true, None));
        assert_eq!(bar.release(on(4.5)), Some(Action::Marker(2)));
        // Near, not exactly on: still the marker's.
        assert_eq!(bar.press(on(4.5) + dvec2(3.0, 0.0)).1, None);
        assert_eq!(bar.release(on(4.5) + dvec2(3.0, 0.0)), Some(Action::Marker(2)));
        // The start marker.
        bar.press(on(0.0));
        assert_eq!(bar.release(on(0.0)), Some(Action::Marker(0)));
    }

    #[test]
    fn a_press_between_markers_scrubs_and_a_drag_follows_the_pointer() {
        let mut state = input(&[2.0, 4.5, 7.0]);
        state.story = Story::Moving;
        let mut bar = bar_of(model(&state));
        let track = bar.laid.track;
        let on = |t: f64| dvec2(x_of(track, 8.0, t), track.pos.y + 3.0);
        let (took, action) = bar.press(on(3.3));
        assert!(took);
        assert!(matches!(action, Some(Action::Scrub(t)) if (t - 3.3).abs() < 1e-6));
        assert!(matches!(bar.drag(on(5.5)), Some(Action::Scrub(t)) if (t - 5.5).abs() < 1e-6));
        assert!(matches!(bar.release(on(6.1)), Some(Action::Release { at, resume: true }) if (at - 5.5).abs() < 1e-6));
        // A drag that starts on a marker is a scrub once it moves.
        bar.press(on(2.0));
        assert!(matches!(bar.drag(on(2.0) + dvec2(20.0, 0.0)), Some(Action::Scrub(_))));
        assert!(matches!(bar.release(on(3.0)), Some(Action::Release { .. })));
    }

    #[test]
    fn presenting_locks_the_ruler_but_not_the_markers() {
        let mut state = input(&[2.0, 4.5]);
        state.presenting = true;
        let mut bar = bar_of(model(&state));
        assert!(bar.laid.play.is_none(), "no play button over the output's picture");
        let track = bar.laid.track;
        let on = |t: f64| dvec2(x_of(track, 8.0, t), track.pos.y + 3.0);
        assert_eq!(bar.press(on(3.3)), (true, None), "the bar takes the press, the slide does not move");
        assert_eq!(bar.release(on(3.3)), None);
        bar.press(on(4.5));
        assert_eq!(bar.drag(on(6.0)), None);
        assert_eq!(bar.release(on(4.5)), Some(Action::Marker(2)));
    }

    #[test]
    fn the_buttons_answer_a_press_that_ends_on_them() {
        let mut state = input(&[2.0]);
        state.story = Story::Paused;
        let mut bar = bar_of(model(&state));
        let play = bar.laid.play.unwrap();
        let looper = bar.laid.looper.unwrap();
        bar.press(play.pos + dvec2(4.0, 4.0));
        assert_eq!(bar.release(play.pos + dvec2(4.0, 4.0)), Some(Action::Play(PlayButton::Play)));
        bar.press(looper.pos + dvec2(4.0, 4.0));
        assert_eq!(bar.release(looper.pos + dvec2(4.0, 4.0)), Some(Action::Loop(true)));
        // Slid off the button: nothing.
        bar.press(play.pos + dvec2(4.0, 4.0));
        assert_eq!(bar.release(dvec2(0.0, 0.0)), None);
        // Off the bar the press is not the bar's.
        assert_eq!(bar.press(dvec2(0.0, 0.0)), (false, None));
    }

    #[test]
    fn hover_reports_only_a_change_of_what_is_lit() {
        let mut bar = bar_of(model(&input(&[2.0, 4.5])));
        let track = bar.laid.track;
        let marker = dvec2(x_of(track, 8.0, 2.0), track.pos.y);
        assert!(bar.hover(Some(marker)));
        assert!(!bar.hover(Some(marker + dvec2(1.0, 0.0))), "still the same marker");
        assert!(bar.hover(Some(dvec2(x_of(track, 8.0, 3.2), track.pos.y))));
        assert!(!bar.hover(Some(dvec2(x_of(track, 8.0, 3.4), track.pos.y))), "nothing lit either way");
    }

    #[test]
    fn marker_names_count_steps() {
        let model = model(&input(&[2.0, 4.5]));
        assert_eq!(marker_text(&model, 0), "step 1 · start");
        assert_eq!(marker_text(&model, 1), "step 2 · 2.0 s");
        assert_eq!(marker_text(&model, 2), "step 3 · 4.5 s");
    }
}
