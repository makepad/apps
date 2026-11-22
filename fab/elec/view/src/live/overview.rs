//! The recording clock, state and coverage. Drag the playhead to review or the window to pan.
//! A frame compares the published numbers and window. A redraw paints one track and three labels.

use super::sim::LiveStatus;
use super::strip::{RunMode, TakeState};
use crate::{ElecLive, ViewAction};
use fab_elec_design::{CursorSource, TimeCursor};
use fab_elec_lang::sys_sim::SAMPLE_RATES;
use fab_elec_sim::listen::KEEPS_UP;
use makepad_widgets_core::*;
use std::fmt::Write as _;
use std::sync::Arc;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    set_type_default() do #(DrawCoverage::script_shader(vm)) {
        ..mod.draw.DrawQuad
        ground: fab.color_input
        accent: fab.color_accent
        pixel: fn() {
            let p = self.pos * self.rect_size
            let sdf = Sdf2d.viewport(p)
            sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, 0.0)
            let stripe = clamp((abs(fract((p.x + p.y) / 5.0) - 0.5) - 0.25) * 5.0 * self.draw_pass.dpi_factor + 0.5, 0.0, 1.0)
            // the take so far: the accent at 40 % over the ground; what is still to come, hatched
            let fill = if self.pos.x < self.front {0.4} else {stripe * 0.55 * self.pending}
            sdf.fill(mix(self.ground, self.accent, fill))
            return sdf.result
        }
    }
    mod.widgets.ElecOverviewBase = #(ElecOverview::register_widget(vm))
    mod.widgets.ElecOverview = set_type_default() do mod.widgets.ElecOverviewBase {
        width: Fill height: 30
        draw_text +: {text_style: theme.font_regular{font_size: fab.font_size_ui}}
        // the clock: 19 px, its digits in a fixed slot
        draw_code +: {text_style: theme.font_code{font_size: 14.25}}
        draw_badge +: {text_style: theme.font_regular{font_size: 6.75}}
        draw_pill +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, self.rect_size.y * 0.5)
                sdf.fill(self.color)
                return sdf.result
            }
        }
        ink: fab.color_text_active
        dim: fab.color_text_dim
        ok: fab.color_ok
        warning: fab.color_warning
        error: fab.color_error
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawCoverage {
    #[deref] draw_super: DrawQuad,
    #[live] ground: Vec4f,
    #[live] accent: Vec4f,
    #[live] front: f32,
    #[live] pending: f32,
}

/// Scope publishes the visible interval when it draws.
#[derive(Default)]
pub struct ScopeWindowShown(pub Option<(f64, f64)>);

/// Move the Scope's visible interval.
#[derive(Clone, Copy, Debug)]
pub struct PanScopeWindow(pub f64, pub f64);

#[derive(Clone, Debug, Default, PartialEq)]
enum StatusLine {
    #[default]
    Empty,
    PressRecord,
    Recording,
    SecondsLeft(f64),
    AfterEdit(Arc<str>),
    /// The take renders again for a Listen the hearing recorder could not play, and why
    /// (`LiveStatus::rerender_reason`): "voice not recorded in this take (OSC)".
    Rerender(Arc<str>),
    Partial,
    PlayingRound,
    Continuous,
    /// Live with no speed reading yet: a new run is heard a little after its start (the run's
    /// set-up and its first round plan), so it claims neither real time nor BEHIND until the tap
    /// or the host has read its speed.
    Starting,
    Live,
    /// Live, slower than the player keeps up with ([`KEEPS_UP`]): the speed, and the highest
    /// listed sim rate at which the run might keep up (the present one times the speed), when one
    /// is below the present.
    Behind(f64, Option<f64>),
    /// The attached device's speaker is heard while the run is slower than real time: its sound
    /// plays in stretches (`fab_elec_sim::device_audio::DevicePlayer`), at this speed.
    Stretches(f64),
}

impl StatusLine {
    fn write(&self, s: &mut String) {
        s.clear();
        let text = match self {
            Self::Empty => "",
            Self::PressRecord => "Press Record",
            Self::Recording => "Recording",
            Self::SecondsLeft(n) => { let _ = write!(s, "About {n:.0} s left"); return; }
            Self::AfterEdit(edit) => { let _ = write!(s, "Re-recording after {edit}"); return; }
            Self::Rerender(why) => { let _ = write!(s, "Re-rendering: {why}"); return; }
            Self::Partial => "Stopped before the end · Record makes a whole take",
            Self::PlayingRound => "Playing round",
            Self::Continuous => "No sound in Continuous",
            Self::Starting => "Starting…",
            Self::Live => "Heard as it is made",
            Self::Behind(rate, lower) => {
                let _ = write!(s, "behind · ×{rate:.2}");
                if let Some(hz) = lower {
                    let _ = write!(s, " · try {}", super::strip::format_sample_rate(*hz));
                }
                return;
            }
            Self::Stretches(speed) => {
                let _ = write!(s, "Simulator slower than real time (×{speed:.2}): sound plays in stretches");
                return;
            }
        };
        s.push_str(text);
    }
}

/// The sim rate to suggest when a live run at `rate` (Hz) keeps up only `speed` of real time: the
/// highest listed rate at most `rate` times `speed` (a run's cost about follows its rate), else the
/// lowest listed one below `rate`; none when no listed rate is below it.
fn lower_rate(rate: f64, speed: f64) -> Option<f64> {
    let mut below = SAMPLE_RATES.iter().copied().filter(|&hz| hz < rate).peekable();
    let lowest = below.peek().copied();
    below.filter(|&hz| hz <= rate * speed).last().or(lowest)
}

/// One reading shared by the transport buttons, clock, badge and coverage.
#[derive(Clone, Default, PartialEq)]
pub(super) struct TakeReading {
    pub state: TakeState,
    pub can_play: bool,
    t: f64,
    axis: Option<(f64, f64, f64)>,
    line: StatusLine,
}

impl TakeReading {
    pub fn read(cx: &Cx, paused: bool) -> Self {
        let Some(live) = cx.get_global_ref::<ElecLive>() else {
            return Self { line: StatusLine::PressRecord, ..Default::default() };
        };
        let sim = &live.sim;
        let st = sim.status.clone();
        let (first, last) = sim.record;
        let has_samples = last > first || sim.heard.is_some();
        let axis = if st.continuous {
            (last > first).then_some((first, last, last))
        } else { st.take.filter(|end| *end > 0.0).map(|end| (0.0, end, last.min(end))) };
        let recorded = has_samples && (st.take.is_some() || st.continuous);
        let whole = st.take.is_some_and(|end| end - st.t <= 1e-9);
        let moving = sim.heard.map_or(sim.playing, |(_, moving)| moving);
        let rest = if paused && recorded { TakeState::Paused } else { TakeState::Stopped };
        let state = if st.realtime && st.playing {
            TakeState::Live
        } else if st.continuous && st.playing {
            TakeState::Recording(RunMode::Continuous)
        } else if st.playing && st.take.is_some() && !whole {
            TakeState::Recording(RunMode::Loop)
        } else if moving && recorded { TakeState::Playing } else { rest };
        let (state, line) = match state {
            // a Listen's re-render says why, ahead of every other take line, while it lasts
            _ if st.rerender_reason.is_some() => (state, StatusLine::Rerender(st.rerender_reason.clone().unwrap_or_default())),
            TakeState::Live => {
                // heard live, the run's own tap reads its speed, and without the ring the host's
                // rate over a full window: a new run has neither until it has stepped a while, and
                // reads as starting, not as the last run's speed or the host's window over its start
                let speed = match cx.get_global_ref::<super::listen::ElecAudio>().filter(|a| a.ring.is_open() && a.ring.realtime()) {
                    Some(audio) => Some(f64::from(audio.ring.play_status().speed)),
                    None => st.rate_achieved,
                };
                match speed.filter(|r| *r > 0.0) {
                    None => (state, StatusLine::Starting),
                    Some(r) if r < f64::from(KEEPS_UP) => (TakeState::Behind, StatusLine::Behind(r, lower_rate(st.sample_rate, r))),
                    Some(_) => (state, StatusLine::Live),
                }
            }
            TakeState::Recording(RunMode::Loop) => {
                let edit = st.rerender.and(sim.edit.as_ref());
                let line = match edit {
                    Some(name) => StatusLine::AfterEdit(name.clone()),
                    None => Self::eta(&st, sim.ratio),
                };
                (state, line)
            }
            TakeState::Recording(RunMode::Continuous) => (state, StatusLine::Continuous),
            _ if !recorded => (state, StatusLine::PressRecord),
            _ if st.realtime => (state, StatusLine::Live),
            _ if st.continuous => (state, StatusLine::Continuous),
            _ if !whole && state == TakeState::Stopped => (state, StatusLine::Partial),
            TakeState::Playing if whole => (state, StatusLine::PlayingRound),
            _ => (state, StatusLine::Empty),
        };
        // the attached device's speaker heard below real time says so, after only a re-render
        let line = match super::listen::speaker_speed(cx).filter(|r| *r > 0.0 && *r < KEEPS_UP) {
            Some(speed) if !matches!(line, StatusLine::Rerender(_)) => StatusLine::Stretches(f64::from(speed)),
            _ => line,
        };
        Self { state, line, axis, can_play: recorded, t: sim.scrub.or(sim.heard.map(|h| h.0)).unwrap_or(sim.t) }
    }

    fn eta(st: &LiveStatus, ratio: Option<f64>) -> StatusLine {
        match (st.take, st.rate_achieved.or(ratio).filter(|r| r.is_finite() && *r > 0.0)) {
            (Some(end), Some(rate)) => StatusLine::SecondsLeft(((end - st.t) / rate).ceil().max(1.0)),
            _ => StatusLine::Recording,
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ElecOverview {
    #[uid] uid: WidgetUid,
    #[source] script_source: ScriptObjectRef,
    #[walk] walk: Walk,
    /// The take's timeline (the recorder's top edge), else the read-out: the clock, the badge
    /// and the status line.
    #[live] track: bool,
    #[live] draw_quad: DrawColor,
    #[live] draw_track: DrawCoverage,
    #[live] draw_pill: DrawColor,
    #[live] draw_text: DrawText,
    #[live] draw_badge: DrawText,
    #[live] draw_code: DrawText,
    #[live] ink: Vec4f,
    #[live] dim: Vec4f,
    #[live] ok: Vec4f,
    #[live] warning: Vec4f,
    #[live] error: Vec4f,
    #[redraw] #[rust] area: Area,
    #[rust] reading: TakeReading,
    #[rust] window: Option<(f64, f64)>,
    #[rust] bar: Rect,
    #[rust] axis: (f64, f64),
    #[rust] drawn_at: DVec2,
    #[rust] scrubbing: bool,
    #[rust] grabbed: Option<(f64, (f64, f64))>,
    #[rust] scrubbed: Option<f64>,
    #[rust] clock: String,
    #[rust] status: String,
}

impl ElecOverview {
    pub(super) fn show(&mut self, cx: &mut Cx, reading: TakeReading) {
        if reading != self.reading || self.clock.is_empty() {
            if reading.t != self.reading.t || self.clock.is_empty() {
                self.clock.clear();
                let _ = write!(self.clock, "{:.3} s", reading.t);
            }
            if self.reading.line != reading.line { reading.line.write(&mut self.status); }
            self.reading = reading;
            self.area.redraw(cx);
        }
    }

    fn badge(&self) -> (&'static str, Vec4f) {
        match self.reading.state {
            TakeState::Recording(_) => ("RECORDING", self.error),
            TakeState::Playing => ("PLAYING", self.ok),
            TakeState::Paused => ("PAUSED", self.warning),
            TakeState::Stopped => ("STOPPED", self.dim),
            TakeState::Live => ("LIVE", self.ok),
            TakeState::Behind => ("BEHIND", self.error),
        }
    }

    fn quad(&mut self, cx: &mut Cx2d, r: Rect, color: Vec4f) {
        self.draw_quad.color = color;
        self.draw_quad.draw_abs(cx, r);
    }

    fn draw_bar(&mut self, cx: &mut Cx2d, view: Rect) {
        if self.track {
            self.draw_cover(cx, view);
            return;
        }
        self.bar = Rect::default();
        let (label, ink) = self.badge();
        let clock = self.draw_code.layout(cx, 0.0, 0.0, None, false, Align::default(), &self.clock);
        let clock_w = f64::from(clock.size_in_lpxs.width * self.draw_code.font_scale);
        let badge = self.draw_badge.layout(cx, 0.0, 0.0, None, false, Align::default(), label);
        let badge_w = f64::from(badge.size_in_lpxs.width * self.draw_badge.font_scale);
        // the clock right-aligned in its slot, so its digits never shift
        let slot = Rect { pos: view.pos, size: dvec2(CLOCK_SLOT.max(clock_w), view.size.y) };
        self.draw_code.color = ink;
        text_in(cx, &mut self.draw_code, slot, Align { x: 1.0, y: 0.5 }, &self.clock);
        let pill = Rect { pos: dvec2(slot.pos.x + slot.size.x + 10.0, view.pos.y + (view.size.y - PILL_H) * 0.5), size: dvec2(badge_w + 16.0, PILL_H) };
        self.draw_pill.color = Vec4f { w: ink.w * 0.18, ..ink };
        self.draw_pill.draw_abs(cx, pill);
        self.draw_badge.color = ink;
        text_in(cx, &mut self.draw_badge, pill, Align { x: 0.5, y: 0.5 }, label);
        let x = pill.pos.x + pill.size.x + 14.0;
        let line = Rect { pos: dvec2(x, view.pos.y), size: dvec2((view.pos.x + view.size.x - x).max(0.0), view.size.y) };
        self.draw_text.color = if matches!(self.reading.line, StatusLine::AfterEdit(_)) { self.warning } else { self.dim };
        text_in(cx, &mut self.draw_text, line, Align { x: 0.0, y: 0.5 }, &self.status);
    }

    /// The take's timeline along the recorder's top edge, square-ended: the coverage and the
    /// stretch still to render, the Scope's window, ticks at 1-2-5 steps along its lower edge and
    /// the playhead in the state's colour. No labels: the Scope's ruler and the length carry them.
    fn draw_cover(&mut self, cx: &mut Cx2d, bar: Rect) {
        self.bar = bar;
        let (origin, end, front) = self.reading.axis.unwrap_or_default();
        self.axis = (origin, end);
        if bar.size.x <= 0.0 { return; }
        let fraction = |t: f64| if end > origin { ((t - origin) / (end - origin)).clamp(0.0, 1.0) } else { 0.0 };
        self.draw_track.front = fraction(front) as f32;
        self.draw_track.pending = if self.reading.axis.is_some() { 1.0 } else { 0.0 };
        self.draw_track.draw_abs(cx, bar);
        if end <= origin { return; }
        let px = 1.0 / cx.current_dpi_factor();
        // ticks about 60 points apart, on a 1-2-5 step
        let step = tick_step((end - origin) * 60.0 / bar.size.x);
        let tick = Vec4f { w: self.dim.w * 0.6, ..self.dim };
        let mut k = (origin / step).ceil();
        while k * step <= end {
            let x = bar.pos.x + fraction(k * step) * bar.size.x;
            self.quad(cx, Rect { pos: dvec2(x, bar.pos.y + bar.size.y - 3.0), size: dvec2(px, 3.0) }, tick);
            k += 1.0;
        }
        if let Some((w0, w1)) = self.window {
            let a = bar.pos.x + fraction(w0) * bar.size.x;
            let w = (fraction(w1) - fraction(w0)) * bar.size.x;
            if w > 0.0 && w < bar.size.x - px {
                let (y, h) = (bar.pos.y + 1.0, bar.size.y - 2.0);
                for (dx, dy, rw, rh) in [(0.0, 0.0, w, px), (0.0, h - px, w, px), (0.0, 0.0, px, h), (w - px, 0.0, px, h)] {
                    self.quad(cx, Rect { pos: dvec2(a + dx, y + dy), size: dvec2(rw, rh) }, self.ink);
                }
            }
        }
        let (_, ink) = self.badge();
        let head = bar.pos.x + fraction(self.reading.t) * bar.size.x;
        self.quad(cx, Rect { pos: dvec2(head - 1.0, bar.pos.y), size: dvec2(2.0, bar.size.y) }, ink);
    }

    fn scrub(&mut self, cx: &mut Cx, x: f64) {
        let (origin, end) = self.axis;
        if self.bar.size.x <= 0.0 || end <= origin {
            return;
        }
        let t = origin + ((x - self.bar.pos.x) / self.bar.size.x).clamp(0.0, 1.0) * (end - origin);
        cx.action(ViewAction::Cursor(TimeCursor { t, playing: false, rate: 1.0, source: CursorSource::Sim, fw: None }));
        super::listen::scrub_audio(cx, Some(t));
        self.scrubbed = Some(t);
        self.area.redraw(cx);
    }

    /// The Scope's window as a box on the bar, and the window, while it is narrower than the axis
    /// (a box the whole bar wide leaves the bar to scrubbing).
    fn window_box(&self, cx: &Cx) -> Option<(Rect, (f64, f64))> {
        let (origin, end) = self.axis;
        let (w0, w1) = cx.get_global_ref::<ScopeWindowShown>()?.0?;
        if self.bar.size.x <= 0.0 || end <= origin || w1 - w0 >= (end - origin) * 0.98 {
            return None;
        }
        let x = |t: f64| self.bar.pos.x + ((t - origin) / (end - origin)).clamp(0.0, 1.0) * self.bar.size.x;
        Some((Rect { pos: dvec2(x(w0), self.bar.pos.y), size: dvec2((x(w1) - x(w0)).max(6.0), self.bar.size.y) }, (w0, w1)))
    }
}

/// The clock's slot: wide enough for its widest reading, so the digits never shift.
const CLOCK_SLOT: f64 = 92.0;
/// The badge's pill.
const PILL_H: f64 = 18.0;

/// `text` placed in `rect` by `align`: across by its width, down by its capitals (a row of texts
/// at align.y 0.5 shares one optical centre whatever their sizes).
fn text_in(cx: &mut Cx2d, draw: &mut DrawText, rect: Rect, align: Align, text: &str) {
    let laid = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
    let Some(row) = laid.rows.first() else { return };
    let scale = f64::from(draw.font_scale);
    let width = f64::from(laid.size_in_lpxs.width) * scale;
    // the capitals' middle on the rect's (align.y 0.5): the baseline sits half a cap height below
    let (baseline, cap) = (f64::from(row.origin_in_lpxs.y) * scale, f64::from(row.cap_height_in_lpxs) * scale);
    let x = rect.pos.x + (rect.size.x - width) * align.x;
    let y = rect.pos.y + rect.size.y * align.y + cap * 0.5 - baseline;
    draw.draw_abs(cx, dvec2(x, y), text);
}

/// A tick step on 1, 2 or 5 times a power of ten, at least `raw`.
fn tick_step(raw: f64) -> f64 {
    if !(raw.is_finite() && raw > 0.0) { return f64::INFINITY; }
    let decade = 10f64.powf(raw.log10().floor());
    let m = raw / decade;
    decade * if m <= 1.0 { 1.0 } else if m <= 2.0 { 2.0 } else if m <= 5.0 { 5.0 } else { 10.0 }
}

impl Widget for ElecOverview {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let view = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, view);
        self.drawn_at = view.pos;
        self.window = cx.get_global_ref::<ScopeWindowShown>().and_then(|w| w.0);
        cx.push_clip_rect(view);
        self.draw_bar(cx, view);
        cx.pop_clip_rect();
        DrawStep::done()
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Slider;
        makepad_platform::a11y::set_string(&mut node.name, "Recording coverage");
        node.value = format!("{} · {} · {}", self.clock, self.badge().0, self.status);
        let (origin, end) = self.axis;
        node.range = (end > origin).then(|| a11y_tree::RangeValue {
            min: origin, max: end, now: self.reading.t.clamp(origin, end), step: (end-origin)/100.0,
        });
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        if self.axis.1 <= self.axis.0 || self.bar.size.x <= 0.0 { return; }
        let rect = Rect { pos: self.bar.pos + self.area.rect(cx).pos - self.drawn_at, ..self.bar };
        out.push((live_id!(take_bar).0, None, a11y_tree::Node {
            name: "take bar".to_owned(),
            role: a11y_tree::Role::Slider,
            bounds: makepad_platform::a11y::semantic_rect(rect),
            actions: a11y_tree::Actions::SET_VALUE,
            range: Some(a11y_tree::RangeValue { min: self.axis.0, max: self.axis.1,
                now: self.reading.t.clamp(self.axis.0, self.axis.1), step: (self.axis.1 - self.axis.0) / 100.0 }),
            ..Default::default()
        }));
    }

    fn handle_action(&mut self, cx: &mut Cx, req: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionArg, ActionResult, Actions};
        let (origin, end) = self.axis;
        if req.action != Actions::SET_VALUE || end <= origin { return ActionResult::Unhandled; }
        let ActionArg::Number(t) = &req.arg else { return ActionResult::Unsupported };
        if !t.is_finite() { return ActionResult::Unsupported; }
        let t = t.clamp(origin, end);
        super::listen::seek_audio(cx, t);
        cx.action(ViewAction::Cursor(TimeCursor { t, playing: false, rate: 1.0, source: CursorSource::Sim, fw: None }));
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if self.window != cx.get_global_ref::<ScopeWindowShown>().and_then(|w| w.0) { self.area.redraw(cx); }
        let origin = self.area.rect(cx).pos;
        self.bar.pos += origin - self.drawn_at;
        self.drawn_at = origin;
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let over = self.bar.contains(e.abs);
                let boxed = self.window_box(cx).is_some_and(|(r, _)| r.contains(e.abs));
                cx.set_cursor(if boxed { MouseCursor::Grab } else if over { MouseCursor::Hand } else { MouseCursor::Default });
            }
            Hit::FingerDown(e) => {
                if let Some(window) = self.window_box(cx).filter(|(r, _)| r.contains(e.abs)).map(|(_, w)| w) {
                    // the Scope's window box, narrower than the axis: a drag pans the wave view
                    self.grabbed = Some((e.abs.x, window));
                    cx.set_cursor(MouseCursor::Grabbing);
                } else if self.bar.contains(e.abs) {
                    self.scrubbing = true;
                    self.scrub(cx, e.abs.x);
                }
            }
            Hit::FingerMove(e) if self.grabbed.is_some() => {
                let (origin, end) = self.axis;
                if let Some((x0, (w0, w1))) = self.grabbed.filter(|_| self.bar.size.x > 0.0 && end > origin) {
                    let dt = (e.abs.x - x0) / self.bar.size.x * (end - origin);
                    let dt = dt.clamp(origin - w0, end - w1);
                    cx.action(PanScopeWindow(w0 + dt, w1 + dt));
                    self.area.redraw(cx);
                }
            }
            Hit::FingerMove(e) if self.scrubbing => self.scrub(cx, e.abs.x),
            Hit::FingerUp(_) => {
                self.grabbed = None;
                if std::mem::take(&mut self.scrubbing) {
                    // let go: playing, the sound goes on from there and the line follows it
                    super::listen::scrub_audio(cx, None);
                    let playing = cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.playing);
                    if let Some(t) = self.scrubbed.take().filter(|_| playing || super::listen::plays_back(cx)) {
                        super::listen::seek_clock(cx, t);
                        cx.action(ViewAction::Cursor(TimeCursor { t, playing: true, rate: 1.0, source: CursorSource::Sim, fw: None }));
                    }
                }
            }
            _ => {}
        }
    }
}
