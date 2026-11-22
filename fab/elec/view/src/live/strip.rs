//! The time strip (ARCHITECTURE 6.5): one strip, one cursor, for simulation runs, recorded
//! measurements and motion. Every pane draws the cursor's instant in the same frame (I16).
//!
//! ```text
//! [|<] [>] [||] [<|] [|>]  --|--o----- 1x  |----====#==========------|  t = 4.20 ms  sim 1 : 2 000  [A|B] [loop]
//! ```
//!
//! The mapping and the transport arithmetic live in [`StripState`], a plain struct without a
//! window, so they are tested directly. [`TimeStrip`] draws and routes input. It keeps a
//! [`TimeCursorView`] (the shared cursor: scrubbing, heard broadcasts, extrapolated playback) and
//! emits a [`TimeStripAction::Cursor`] whenever the cursor changes discontinuously (a scrub
//! sample, a button, a loop wrap). A playing cursor in between is extrapolated by every view from
//! the broadcast it last heard, so the strip emits nothing per frame while playback is smooth.
//!
//! In Electronics the strip draws seven transport and record buttons in a single well. Its
//! draw cost is constant. Space plays or pauses and Home returns to the start.
//!
//! Bench keys (strip or its pane focused): Space plays and pauses, Left and Right step by one frame of
//! the current rate's simulated time (rate / 60 s), S stops at the next trigger,
//! J slower, K pause, L faster (and play), [ and ] jump to the previous/next recorded trigger.
//! Rate slider: 1/1000x to 1000x on a log scale, the asked rate written beside it. A press or a drag
//! sets the rate under the pointer live (two significant digits); it rests at 1x within a few points
//! of its mark, and a double-click puts it back to 1x. A narrow strip drops the read-out and keeps
//! the slider while it has `SLIDER_MIN` points for it; narrower still, it folds the rate and the
//! time into one chip, whose click raises the rate one 1-2-5 step (shift-click lowers it).
//! Shift-drag on the bar marks a range; the loop chip loops it (the whole span when nothing is
//! marked).
//!
//! Cost: a draw is one quad per two device pixels of activity plus a few tens of quads and
//! strings, plus at most one trigger tick per pixel. Event counts use bounded index searches,
//! never a walk of the events, even when the whole run is visible. Open record rows cache their
//! columns by store pointer, length, span and width, reusing buffers. Unchanged frames reduce
//! nothing; at the live head each visible row reduces again: O(columns x log CHUNK) per row.
//! A closed record region reads and draws nothing.

use super::TimeCursorView;
use crate::canvas::looks::Tokens;
use crate::instruments::{events::EventList, look::InstrumentLook, scope::LabelCache, simfeed::EventSnapshot, trend::{TrendColumn, TrendQuantity, ViolationList}, trigger_ui::StopAtTrigger};
use crate::trace::columns::Columns;
use fab_elec_design::{CursorSource, TimeCursor};
use fab_elec_sim::trace::TraceStore;
use makepad_widgets_core::tip::TipAction;
use makepad_widgets_core::*;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    let StripIcon = set_type_default() do #(StripIcon::script_api(vm))

    set_type_default() do #(DrawStripIcon::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: #fff

        sd_bar: fn(p: vec2, half_size: vec2, radius: float) -> float {
            let q = abs(p) - half_size + vec2(radius, radius)
            return length(max(q, vec2(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - radius
        }

        // A right-pointing triangle, folded across its horizontal centre.
        sd_triangle: fn(p: vec2, half_size: vec2) -> float {
            let q = vec2(p.x + half_size.x, abs(p.y))
            let edge = vec2(2.0 * half_size.x, -half_size.y)
            let v = q - vec2(0.0, half_size.y)
            let slope = v - edge * clamp(dot(v, edge) / dot(edge, edge), 0.0, 1.0)
            let base = q - vec2(0.0, clamp(q.y, 0.0, half_size.y))
            let side = max(-q.x, dot(v, vec2(half_size.y, 2.0 * half_size.x)))
            return sqrt(min(dot(slope, slope), dot(base, base))) * sign(side)
        }

        sd_start: fn(p: vec2) -> float {
            let bar = self.sd_bar(p + vec2(5.0, 0.0), vec2(1.0, 6.0), 0.3)
            let triangle = self.sd_triangle(vec2(1.75 - p.x, p.y), vec2(4.25, 5.5))
            return min(bar, triangle)
        }

        sd_step_back: fn(p: vec2) -> float {
            let bar = self.sd_bar(p - vec2(4.5, 0.0), vec2(1.0, 6.0), 0.3)
            let triangle = self.sd_triangle(vec2(-1.25 - p.x, p.y), vec2(4.25, 4.5))
            return min(bar, triangle)
        }

        // Play, then a rising edge: run until the next trigger.
        sd_stop_at_trigger: fn(p: vec2) -> float {
            let play = self.sd_triangle(p + vec2(3.5, 0.0), vec2(2.75, 4.5))
            let low = self.sd_bar(p - vec2(1.5, 4.0), vec2(1.25, 0.6), 0.3)
            let rise = self.sd_bar(p - vec2(2.75, 0.0), vec2(0.6, 4.6), 0.3)
            let high = self.sd_bar(p - vec2(4.5, -4.0), vec2(1.75, 0.6), 0.3)
            return min(play, min(low, min(rise, high)))
        }

        sd_loop: fn(p: vec2) -> float {
            // A 300-degree arc, with a 60-degree gap on the right.
            let q = vec2(p.x, abs(p.y))
            var arc = abs(length(p) - 5.25) - 0.75
            if 0.5 * q.x > 0.8660254 * q.y {
                arc = length(q - vec2(4.546633, 2.625)) - 0.75
            }
            let tip = p - vec2(4.546633, -2.625)
            let head = vec2(dot(tip, vec2(0.5, 0.8660254)), dot(tip, vec2(-0.8660254, 0.5)))
            return min(arc, self.sd_triangle(head + vec2(0.3, 0.0), vec2(1.8, 1.65)))
        }

        sd_live: fn(p: vec2) -> float {
            let q = vec2(length(p), abs(p.y))
            let arcs = min(abs(q.x - 4.0), min(abs(q.x - 7.0), abs(q.x - 10.0))) - 0.7
            return max(arcs, q.y * 0.75 - p.x)
        }

        pixel: fn() {
            let p = (self.pos - vec2(0.5, 0.5)) * self.rect_size
            let distance = match self.icon {
                StripIcon.Start => self.sd_start(p)
                StripIcon.End => self.sd_start(vec2(-p.x, p.y))
                StripIcon.Play => self.sd_triangle(p - vec2(1.0, 0.0), vec2(4.5, 6.0))
                StripIcon.StopAtTrigger => self.sd_stop_at_trigger(p)
                StripIcon.Pause => self.sd_bar(vec2(abs(p.x) - 3.0, p.y), vec2(1.5, 6.0), 0.6)
                StripIcon.StepBack => self.sd_step_back(p)
                StripIcon.StepForward => self.sd_step_back(vec2(-p.x, p.y))
                StripIcon.Loop => self.sd_loop(p)
                StripIcon.Stop => self.sd_bar(p, vec2(4.5, 4.5), 0.8)
                StripIcon.RecLoop => min(length(p + vec2(6.0, 0.0)) - 4.2, self.sd_loop((p - vec2(5.0, 0.0)) * 1.25) / 1.25)
                StripIcon.RecContinuous => min(length(p + vec2(6.0, 0.0)) - 4.2, abs(length(vec2(abs(p.x - 5.0) - 2.35, p.y)) - 2.35) - 0.7)
                StripIcon.RecLive => min(length(p + vec2(6.0, 0.0)) - 4.2, self.sd_live(p - vec2(0.5, 0.0)))
                _ => 100.0
            }
            let coverage = clamp(0.5 - distance * self.draw_pass.dpi_factor, 0.0, 1.0)
            let alpha = coverage * self.color.a
            return vec4(self.color.rgb * alpha, alpha)
        }
    }

    mod.widgets.TimeStripBase = #(TimeStrip::register_widget(vm))
    mod.widgets.TimeStrip = set_type_default() do mod.widgets.TimeStripBase {
        width: Fill height: 28
        draw_text +: {color: theme.color_text text_style: theme.font_regular {font_size: theme.prop_font_size}}
    }
}

/// A transport icon's shape.
#[derive(Clone, Copy, Debug, PartialEq, Script, ScriptHook)]
#[repr(u32)]
pub enum StripIcon {
    #[pick]
    Start = 0,
    End = 1,
    Play = 2,
    Pause = 3,
    StepBack = 4,
    StepForward = 5,
    Loop = 6,
    StopAtTrigger = 7,
    Stop = 8,
    RecLoop = 9,
    RecContinuous = 10,
    RecLive = 11,
}

/// The strip's transport icons, measured in logical points.
#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawStripIcon {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    icon: StripIcon,
    #[live]
    color: Vec4f,
}

/// The rate menu, 1/1000x to 1000x in 1-2-5 steps. Literals, so every entry is exact.
pub const RATES: [f64; 19] = [
    0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0,
];

/// Frames a second the step buttons and keys count in.
pub const STEP_HZ: f64 = 60.0;

/// The rate slider's width and its knob's, in points. A narrow pane narrows it down to
/// `SLIDER_MIN` (about 12 points a decade), by the pane's width alone, so it never moves under a
/// pointer; a strip without that room folds the slider into the rate chip.
pub const SLIDER_W: f64 = 112.0;
pub const SLIDER_MIN: f64 = 80.0;
const KNOB_W: f64 = 8.0;
/// Minimum space for the Bench timeline between its controls.
const BAR_MIN: f64 = 48.0;
/// Within this many points of the 1x mark the slider rests at 1x.
pub const SLIDER_DETENT: f64 = 3.0;

/// Under this width (points) the rate and read-outs fold into one chip.
pub const COMPACT_WIDTH: f64 = 520.0;

const THIN: char = '\u{2009}';

fn tick_column(t: f64, span: (f64, f64), width: usize) -> usize {
    (((t - span.0) / (span.1 - span.0) * width as f64) as usize).min(width.saturating_sub(1))
}

/// Sorted finite event times grouped into pixel columns of the closed visible span.
/// The right endpoint belongs to the last column. Each occupied pixel uses index searches
/// bounded by the machine word size (see EventList), never a loop over its events.
pub fn tick_columns(events: &EventList, span: (f64, f64), width: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
    tick_ranges(events, span, width).map(|(x, i0, i1)| (x, i1 - i0))
}

fn tick_ranges(events: &EventList, span: (f64, f64), width: usize) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
    let valid = width > 0 && span.0.is_finite() && span.1.is_finite() && span.1 > span.0;
    let mut i = if valid { events.partition_point(|t| t < span.0) } else { events.len() };
    std::iter::from_fn(move || {
        let t = events.get(i)?;
        if t > span.1 { return None; }
        let x = tick_column(t, span, width);
        let start = i;
        // Searching the quantised column itself avoids round-off gaps at pixel boundaries.
        i = events.partition_point(|t| t <= span.1 && (t < span.0 || tick_column(t, span, width) <= x));
        Some((x, start, i))
    })
}

/// One tick per occupied device column, without walking the failures in a dense column.
pub fn violation_columns<'a>(events: &'a EventList, violations: &'a ViolationList, span: (f64, f64), width: usize) -> impl Iterator<Item = (usize, usize)> + 'a {
    let valid = width > 0 && span.0.is_finite() && span.1.is_finite() && span.1 > span.0;
    let first = events.partition_point(|t| t < span.0);
    let mut i = if valid { violations.partition_point(|v| v.event < first) } else { violations.len() };
    std::iter::from_fn(move || {
        let t = events.get(violations.get(i)?.event)?;
        if t > span.1 { return None; }
        let x = tick_column(t, span, width);
        let end = events.partition_point(|t| t <= span.1 && (t < span.0 || tick_column(t, span, width) <= x));
        let start = i;
        i = violations.partition_point(|v| v.event < end);
        Some((x, i - start))
    })
}

/// What the strip shows besides the cursor: the run's span and its activity per bin, from the host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripModel {
    /// Seconds; the bar maps it to its width.
    pub span: (f64, f64),
    /// 0..1 per bin across the span (edges, events), drawn as the bar's texture.
    pub activity: Vec<f32>,
    /// Times of failing bench assertions, marked in the violation colour.
    pub failures: Vec<f64>,
    /// Named markers.
    pub markers: Vec<(f64, String)>,
    /// Simulated seconds per real second reached (`LiveValues::ratio`), shown as `sim 1 : 2 000`.
    pub ratio: Option<f64>,
    /// A previous run exists.
    pub ghost: Option<GhostState>,
    /// The span's end is a running source's head (a live simulation): playback that reaches it
    /// keeps following it instead of stopping there.
    pub live: bool,
}

/// Which run is on top when a previous run exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GhostState {
    UnderLive,
    OnTop,
}

impl GhostState {
    pub fn flipped(self) -> GhostState {
        match self {
            GhostState::UnderLive => GhostState::OnTop,
            GhostState::OnTop => GhostState::UnderLive,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum TimeStripAction {
    /// The cursor changed: broadcast it.
    Cursor(TimeCursor),
    /// The bench takes an atomic trigger-event baseline at the instant of arming.
    StopAtTrigger,
    /// The ghost chip flipped which run is on top.
    Ghost(GhostState),
    /// The loop was switched on over a range, or off.
    Loop(Option<(f64, f64)>),
    /// A step forward past a live head, where no frame is recorded: the run should advance this
    /// many simulated seconds and pause (`StripState::step_ahead`).
    StepAhead(f64),
    /// Return the playhead to the start without changing playback.
    Start,
    /// Start a fresh recording from zero.
    Record(RunMode),
    /// Hold the recording and silence playback.
    Stop,
    #[default]
    None,
}

/// The strip's mapping and transport arithmetic, without a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StripState {
    pub span: (f64, f64),
    /// The range playback wraps within, when the loop is on.
    pub loop_range: Option<(f64, f64)>,
    /// The span's end is a running source's head (`StripModel::live`).
    pub live: bool,
}

impl Default for StripState {
    fn default() -> Self {
        StripState { span: (0.0, 0.0), loop_range: None, live: false }
    }
}

impl StripState {
    pub fn new(span: (f64, f64)) -> StripState {
        StripState { span, loop_range: None, live: false }
    }

    /// Nothing runs: the strip is a single device pixel line and takes no height.
    pub fn collapsed(&self) -> bool {
        !(self.span.0.is_finite() && self.span.1.is_finite() && self.span.1 > self.span.0)
    }

    /// A live cursor waits here until the source advances its head.
    pub fn at_head(&self, c: TimeCursor) -> bool { self.live && !self.collapsed() && c.t >= self.span.1 }

    /// The x of time `t` on a bar starting at `x0` that is `w` wide (unclamped).
    pub fn x_of(&self, t: f64, x0: f64, w: f64) -> f64 {
        if self.collapsed() {
            return x0;
        }
        x0 + (t - self.span.0) / (self.span.1 - self.span.0) * w
    }

    /// The time at `x` on that bar, clamped to the span.
    pub fn t_of(&self, x: f64, x0: f64, w: f64) -> f64 {
        if self.collapsed() || w <= 0.0 {
            return self.span.0;
        }
        (self.span.0 + (x - x0) / w * (self.span.1 - self.span.0)).clamp(self.span.0, self.span.1)
    }

    /// Nearest visible tick within `slop`, in bar-local device pixels. O(log N).
    /// Hit testing uses the same quantised columns as drawing, including the right endpoint.
    pub fn event_at(&self, events: &EventList, x: f64, width: usize, slop: f64) -> Option<f64> {
        if self.collapsed() || width == 0 { return None; }
        let t = self.t_of(x, 0.0, width as f64);
        let i = events.partition_point(|event| event < t);
        let before = i.checked_sub(1).and_then(|i| events.get(i));
        let after = events.get(i);
        before.into_iter().chain(after).filter(|&t| t >= self.span.0 && t <= self.span.1)
            .map(|t| (t, (tick_column(t, self.span, width) as f64 - x).abs()))
            .filter(|&(_, distance)| distance <= slop)
            .min_by(|a, b| a.1.total_cmp(&b.1)).map(|(t, _)| t)
    }

    /// Resolve a trend press in the same pixel columns as the ticks. `y` is 0 at the top
    /// and 1 at the bottom of the value track. The bar's upper half chooses its maximum.
    pub fn trend_at(&self, events: &EventList, column: &TrendColumn, x: f64, width: usize, y: f64, range: (f32, f32)) -> Option<f64> {
        if self.collapsed() || width == 0 || !x.is_finite() || x < 0.0 || x >= width as f64 || !y.is_finite() { return None; }
        let x = x.floor() as usize;
        let i0 = events.partition_point(|t| t < self.span.0 || t <= self.span.1 && tick_column(t, self.span, width) < x);
        let i1 = events.partition_point(|t| t < self.span.0 || t <= self.span.1 && tick_column(t, self.span, width) <= x);
        let (min, argmin, max, argmax) = column.extremes(i0, i1)?;
        let middle = (trend_y(min, range) + trend_y(max, range)) * 0.5;
        events.get(if y <= middle { argmax } else { argmin })
    }

    /// Strictly before/after the cursor, restricted to the visible record. O(log N).
    pub fn adjacent_event(&self, events: &EventList, t: f64, forward: bool) -> Option<f64> {
        if self.collapsed() || !t.is_finite() { return None; }
        let i = events.partition_point(|event| if forward { event < self.span.0 || event <= t } else { event <= self.span.1 && event < t });
        let event = if forward { events.get(i) } else { i.checked_sub(1).and_then(|i| events.get(i)) }?;
        (event >= self.span.0 && event <= self.span.1).then_some(self.clamp(event))
    }

    /// Exact occupied column; empty columns remain free scrubs.
    pub fn violation_at(&self, events: &EventList, violations: &ViolationList, x: f64, width: usize) -> Option<f64> {
        if self.collapsed() || width == 0 || !x.is_finite() || x < 0.0 || x >= width as f64 { return None; }
        let x = x.floor() as usize;
        let first = events.partition_point(|t| t < self.span.0 || t <= self.span.1 && tick_column(t, self.span, width) < x);
        let i = violations.partition_point(|v| v.event < first);
        let t = events.get(violations.get(i)?.event)?;
        (t >= self.span.0 && t <= self.span.1 && tick_column(t, self.span, width) == x).then_some(t)
    }

    pub fn adjacent_violation(&self, events: &EventList, violations: &ViolationList, t: f64, forward: bool) -> Option<f64> {
        if self.collapsed() || !t.is_finite() { return None; }
        let event = events.partition_point(|event| if forward { event < self.span.0 || event <= t } else { event <= self.span.1 && event < t });
        let i = violations.partition_point(|v| v.event < event);
        let violation = if forward { violations.get(i) } else { i.checked_sub(1).and_then(|i| violations.get(i)) }?;
        let t = events.get(violation.event)?;
        (t >= self.span.0 && t <= self.span.1).then_some(t)
    }

    /// Index of the menu rate nearest to `rate` (on a log scale).
    pub fn rate_index(rate: f64) -> usize {
        if rate == f64::INFINITY { return RATES.len() - 1; }
        let r = rate.abs().max(f64::MIN_POSITIVE).ln();
        let mut best = 0;
        let mut best_d = f64::INFINITY;
        for (i, m) in RATES.iter().enumerate() {
            let d = (m.ln() - r).abs();
            if d < best_d {
                best = i;
                best_d = d;
            }
        }
        best
    }

    /// Where `rate` sits on the rate slider, 0 (the slowest rate) to 1 (the fastest; max too), on a
    /// log scale.
    pub fn slider_of(rate: f64) -> f64 {
        if rate == f64::INFINITY { return 1.0; }
        let (lo, hi) = (RATES[0].ln(), RATES[RATES.len() - 1].ln());
        ((rate.abs().max(f64::MIN_POSITIVE).ln() - lo) / (hi - lo)).clamp(0.0, 1.0)
    }

    /// The rate at slider position `p` (0..1) on a slider `width` points long: 1x within
    /// [`SLIDER_DETENT`] points of its mark, otherwise the log-scale rate to two significant digits
    /// (`0.035`, `3.5`, `350`), held to the menu's ends.
    pub fn rate_at_slider(p: f64, width: f64) -> f64 {
        let p = p.clamp(0.0, 1.0);
        if ((p - StripState::slider_of(1.0)) * width).abs() <= SLIDER_DETENT { return 1.0; }
        let (lo, hi) = (RATES[0].ln(), RATES[RATES.len() - 1].ln());
        let rate = (lo + p * (hi - lo)).exp();
        // Divide by a power of ten rather than multiply by its inverse, so 35/1000 is exactly 0.035.
        let e = rate.log10().floor() as i32 - 1;
        let rate = if e >= 0 { (rate / 10f64.powi(e)).round() * 10f64.powi(e) } else { (rate * 10f64.powi(-e)).round() / 10f64.powi(-e) };
        rate.clamp(RATES[0], RATES[RATES.len() - 1])
    }

    /// The menu rate `steps` entries from `rate`'s, held at both ends.
    pub fn rate_step(rate: f64, steps: i32) -> f64 {
        let i = (StripState::rate_index(rate) as i32 + steps).clamp(0, RATES.len() as i32 - 1);
        RATES[i as usize]
    }

    /// A live source's rate menu: the same 1-2-5 steps, then `max` (as fast as it runs) above them.
    pub fn live_rate_step(rate: f64, steps: i32) -> f64 {
        let top = RATES.len() as i32;
        let i = if rate == f64::INFINITY { top } else { StripState::rate_index(rate) as i32 };
        match (i + steps).clamp(0, top) {
            i if i == top => f64::INFINITY,
            i => RATES[i as usize],
        }
    }

    /// One frame of simulated time at `rate`.
    pub fn step_size(rate: f64) -> f64 {
        rate.abs() / STEP_HZ
    }

    /// The cursor one frame forward (`dir` 1) or back (-1), paused, held inside the span.
    pub fn step(&self, c: TimeCursor, dir: i32) -> TimeCursor {
        if self.collapsed() { return TimeCursor { playing: false, ..c }; }
        let t = c.t + dir as f64 * StripState::step_size(c.rate);
        TimeCursor { t: self.clamp(t), playing: false, ..c }
    }

    /// A paused step forward that would pass a live head asks the run for one frame of simulated
    /// time (`Some(seconds)`): at the cursor's rate, or at max at the rate the run reached
    /// (`ratio`, 1x before it reports one). Anywhere else a step moves the cursor (`None`).
    pub fn step_ahead(&self, c: TimeCursor, ratio: Option<f64>) -> Option<f64> {
        let dt = if c.rate.is_finite() {
            StripState::step_size(c.rate)
        } else {
            ratio.filter(|r| r.is_finite() && *r > 0.0).unwrap_or(1.0) / STEP_HZ
        };
        (self.live && !c.playing && c.t + dt > self.span.1).then_some(dt)
    }

    /// Where play starts from `t`: at the span's end it starts over, except at a live head, where
    /// it goes on.
    pub fn play_from(&self, t: f64) -> f64 {
        if t >= self.span.1 && !self.live {
            self.span.0
        } else {
            t
        }
    }

    pub fn clamp(&self, t: f64) -> f64 {
        if self.collapsed() {
            t
        } else {
            t.clamp(self.span.0, self.span.1)
        }
    }

    /// A playing cursor moved from `prev` to `c.t`. Crossing the loop's end it comes back to the
    /// loop's start (by the overshoot); at the span's end without a loop it stops there, unless the
    /// end is a live head, which it keeps following. Returns the replacement cursor when it changed.
    pub fn settle(&self, prev: f64, c: TimeCursor) -> Option<TimeCursor> {
        if !c.playing || self.collapsed() {
            return None;
        }
        if let Some((a, b)) = self.loop_range {
            if b > a && prev <= b && c.t > b {
                let t = a + (c.t - b) % (b - a);
                return Some(TimeCursor { t, ..c });
            }
        }
        if c.t >= self.span.1 && !self.live {
            return Some(TimeCursor { t: self.span.1, playing: false, ..c });
        }
        None
    }

    /// Plays from `c` for `dt` seconds of wall time, loop and end included: the model of what the
    /// widget does every frame while playing.
    pub fn tick(&self, c: TimeCursor, dt: f64) -> TimeCursor {
        if !c.playing {
            return c;
        }
        let moved = TimeCursor { t: c.t + c.rate * dt, ..c };
        self.settle(c.t, moved).unwrap_or(moved)
    }
}

/// Engineering notation, three significant figures: `4.20 ms`, `12.5 us`, `0 s`.
pub fn format_time(t: f64) -> String {
    if !t.is_finite() {
        return "-- s".into();
    }
    if t == 0.0 {
        return "0 s".into();
    }
    const UNITS: [(f64, &str); 8] =
        [(1e-15, "fs"), (1e-12, "ps"), (1e-9, "ns"), (1e-6, "\u{b5}s"), (1e-3, "ms"), (1.0, "s"), (1e3, "ks"), (1e6, "Ms")];
    let a = t.abs();
    let mut i = UNITS.iter().rposition(|u| a >= u.0).unwrap_or(0);
    loop {
        let v = a / UNITS[i].0;
        let decimals = if v < 9.995 {
            2
        } else if v < 99.95 {
            1
        } else {
            0
        };
        let text = format!("{v:.decimals$}");
        // 999.6 ms rounds to 1000: it is 1.00 s.
        if text.parse::<f64>().map_or(false, |r| r >= 1000.0) && i + 1 < UNITS.len() {
            i += 1;
            continue;
        }
        return format!("{}{} {}", if t < 0.0 { "-" } else { "" }, text, UNITS[i].1);
    }
}

/// `sim 1 : 2 000` (thin space thousands): simulated time to real time. `ratio` is
/// `LiveValues::ratio`, simulated seconds per real second: 0.0005 reads `sim 1 : 2 000` (a
/// simulated second takes 2 000 real ones), 2 000 reads `sim 2 000 : 1`.
pub fn format_ratio(ratio: f64) -> String {
    if !ratio.is_finite() || ratio <= 0.0 {
        return String::new();
    }
    if ratio <= 1.0 {
        format!("sim 1 : {}", group((1.0 / ratio).round() as u64))
    } else {
        format!("sim {} : 1", group(ratio.round() as u64))
    }
}

/// A rate the run reached, as a factor of real time without its `×`: three significant digits,
/// as `format_time` writes the times beside it (`0.0123`, `2.50`, `23.4`), but two decimals from
/// a tenth up to one (`0.71`), so a run about real time reads with the same width either side of
/// it (`0.99`, `1.00`, `1.01`: the theme's digits are tabular) and moves nothing after it;
/// thousands grouped as `format_ratio` groups them (`1 230`). Every read-out of the reached rate
/// uses it: the transport's and the scope's source bar.
pub fn format_reached(x: f64) -> String {
    // rounded first, so 9.996 is `10.0`, not `10.00`
    let x = format!("{x:.2e}").parse::<f64>().unwrap_or(x);
    let decimals = if (0.1..1.0).contains(&x) { 2 } else { (2.0 - x.log10().floor()).clamp(0.0, 12.0) as usize };
    if decimals == 0 && x >= 1e3 && x < 1e15 {
        return group(x.round() as u64);
    }
    format!("{x:.decimals$}")
}

fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(THIN);
        }
        out.push(ch);
    }
    out
}

/// A sample rate in kHz.
pub fn format_sample_rate(hz: f64) -> String {
    format!("{} kHz", (hz / 10.0).round() / 100.0)
}

/// Playback speed as shown in the rate controls.
pub fn format_rate(rate: f64) -> String {
    if rate == f64::INFINITY {
        "max".into()
    } else if rate >= 1.0 && rate < 1e15 && rate.fract() == 0.0 {
        format!("{}x", group(rate.round() as u64))
    } else if rate >= 1e15 || rate < 1e-12 {
        format!("{rate:e}x")
    } else {
        let inv = 1.0 / rate;
        if (inv - inv.round()).abs() < 1e-6 && inv.round() >= 10.0 && inv < 1e15 {
            format!("1/{}x", group(inv.round() as u64))
        } else {
            format!("{rate}x")
        }
    }
}

/// What a record button captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    Loop,
    Continuous,
    Realtime,
}

/// The badge and transport buttons share the state of the same take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum TakeState {
    #[default]
    Stopped,
    Paused,
    Playing,
    Recording(RunMode),
    Live,
    Behind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Start,
    Play,
    Pause,
    Stop,
    StepBack,
    StepForward,
    Rate,
    Loop,
    Ghost,
    StopAtTrigger,
    RecLoop,
    RecContinuous,
    RecLive,
}

impl Part {
    fn name(self) -> &'static str {
        match self {
            Part::Start => "start",
            Part::Play => "play",
            Part::Pause => "pause",
            Part::Stop => "stop",
            Part::StopAtTrigger => "stop at next trigger",
            Part::StepBack => "step back",
            Part::StepForward => "step forward",
            Part::Rate => "rate",
            Part::Loop => "loop",
            Part::Ghost => "ghost",
            Part::RecLoop => "Record loop",
            Part::RecContinuous => "Record continuous",
            Part::RecLive => "Record live",
        }
    }
    fn icon(self) -> Option<StripIcon> {
        match self {
            Part::Start => Some(StripIcon::Start),
            Part::Play => Some(StripIcon::Play),
            Part::Pause => Some(StripIcon::Pause),
            Part::Stop => Some(StripIcon::Stop),
            Part::StopAtTrigger => Some(StripIcon::StopAtTrigger),
            Part::StepBack => Some(StripIcon::StepBack),
            Part::StepForward => Some(StripIcon::StepForward),
            Part::Loop => Some(StripIcon::Loop),
            Part::RecLoop => Some(StripIcon::RecLoop),
            Part::RecContinuous => Some(StripIcon::RecContinuous),
            Part::RecLive => Some(StripIcon::RecLive),
            Part::Rate | Part::Ghost => None,
        }
    }
    fn key(self) -> u64 {
        self as u64 + 1
    }
    fn from_key(k: u64) -> Option<Part> {
        [Part::Start, Part::Play, Part::Pause, Part::Stop, Part::StepBack, Part::StepForward, Part::Rate, Part::Loop, Part::Ghost, Part::StopAtTrigger, Part::RecLoop, Part::RecContinuous, Part::RecLive]
            .into_iter()
            .find(|p| p.key() == k)
    }
}

const BTN: f64 = 22.0;
const GAP: f64 = 4.0;
const FONT_H: f64 = 11.0;
const TREND_H: f64 = 14.0;

/// Smallest visible record row, in logical points.
pub const RECORD_ROW_MIN: f64 = 12.0;

/// Record height and the rows that fit without squeezing their labels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordLayout { pub height: f64, pub shown: usize, pub hidden: usize }

impl RecordLayout {
    pub fn new(record_h: f64, rows: usize, max_record_h: f64, released: bool) -> Self {
        let mut height = record_h.max(0.0).min(max_record_h.max(0.0));
        if released && height < RECORD_ROW_MIN { height = 0.0; }
        let shown = rows.min((height / RECORD_ROW_MIN) as usize);
        Self { height, shown, hidden: rows - shown }
    }

    pub fn bar_top(&self, top: f64) -> f64 { top + self.height }

    pub fn row_height(&self) -> f64 { if self.shown == 0 { 0.0 } else { self.height / self.shown as f64 } }
}

fn trend_y(value: f32, range: (f32, f32)) -> f64 {
    if range.0 == range.1 { 0.5 }
    else { (f64::from(range.1) - f64::from(value)) / (f64::from(range.1) - f64::from(range.0)) }
}

/// Pin identity and presentation supplied by the bench, independently of publication latency.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrendRow { pub lane: usize, pub quantity: TrendQuantity, pub label: String }

#[derive(Default)]
struct TrendRange { column: Option<usize>, key: Option<(u64, (f64, f64))>, range: Option<(f32, f32)> }

/// Store row and presentation in the host's lane order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordRow { pub row: usize, pub label: String, pub hue: Vec4f }

#[derive(Default)]
struct RecordCache {
    columns: Columns,
    key: Option<(usize, usize, (f64, f64), u32)>,
    range: Option<(f64, f64)>,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    /// The rate slider, laid out at `track`.
    Rate { track: Rect },
    Scrub,
    Mark { from: f64 },
    Record { y: f64, height: f64 },
}

/// One strip for simulation runs, recorded measurements and motion.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct TimeStrip {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_shape: DrawColor,
    #[live]
    draw_text: DrawText,
    #[rust]
    text_cache: LabelCache,
    #[rust]
    readout_key: Option<(f64, f64, Option<f64>)>,
    #[rust]
    time_text: String,
    #[rust]
    rate_text: String,
    #[rust]
    ratio_text: String,
    #[rust]
    compact_text: String,
    #[live]
    draw_icon: DrawStripIcon,
    /// The Electronics recording well. The Bench supplies its own controls.
    #[live]
    transport: bool,
    #[live]
    draw_face: DrawColor,
    #[live]
    face_selected: Vec4f,
    #[live]
    face_hover: Vec4f,
    #[live]
    ink_normal: Vec4f,
    #[live]
    ink_disabled: Vec4f,
    #[live]
    ink_play: Vec4f,
    #[live]
    ink_pause: Vec4f,
    #[live]
    ink_record: Vec4f,
    #[rust]
    take_state: TakeState,
    /// With auto re-record off, the changes waiting for the next record: the loop button's ●N.
    #[rust]
    waiting: u32,
    #[rust]
    can_play: bool,
    #[rust]
    tipped: Option<Part>,
    #[rust]
    area: Area,
    #[rust]
    view: Rect,
    #[rust]
    drawn_at: DVec2,
    #[rust(true)]
    dark: bool,
    /// The code font's cached advance (it is monospaced).
    #[rust(6.2)]
    char_w: f64,
    #[rust]
    model: StripModel,
    #[rust]
    events: Option<Arc<EventSnapshot>>,
    #[rust]
    violation_text: String,
    #[rust]
    event_channel: usize,
    #[rust]
    event_generation: u64,
    #[rust]
    trend_rows: Vec<TrendRow>,
    #[rust]
    trend_ranges: Vec<TrendRange>,
    #[rust]
    record: Option<Arc<TraceStore>>,
    #[rust]
    record_rows: Vec<RecordRow>,
    #[rust]
    record_cache: Vec<RecordCache>,
    #[rust]
    record_h: f64,
    #[rust]
    max_record_h: f64,
    #[rust]
    record_hidden: usize,
    #[rust]
    record_more: String,
    #[rust]
    state: StripState,
    #[rust]
    stop_at_trigger: StopAtTrigger,
    #[rust(TimeCursorView::new(0.0))]
    cursor: TimeCursorView,
    #[rust]
    now: f64,
    #[rust]
    prev_t: f64,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    drag: Option<Drag>,
    /// The range shift-drag marked, and whether the loop runs over it.
    #[rust]
    marked: Option<(f64, f64)>,
    #[rust]
    pointer: Option<DVec2>,
    #[rust]
    bar: Rect,
    #[rust]
    parts: Vec<(Part, Rect)>,
    #[rust]
    compact: bool,
    /// The rate shows as a slider (a wide strip, or a narrow one with room for it), not the chip.
    #[rust]
    slider_on: bool,
}

impl TimeStrip {
    /// Replace times and columns together; all immutable trees remain shared.
    pub fn set_events(&mut self, cx: &mut Cx, events: Option<Arc<EventSnapshot>>) {
        self.event_generation = events.as_ref().map_or(0, |events| events.generation);
        let count = events.as_ref().map_or(0, |events| events.violations.len());
        if count != self.events.as_ref().map_or(0, |events| events.violations.len()) {
            self.violation_text = if count == 0 { String::new() } else { format!("{count} out of bound") };
        }
        self.events = events;
        self.index_trend_rows();
        self.area.redraw(cx);
    }

    pub(crate) fn set_trend_rows(&mut self, cx: &mut Cx, rows: Vec<TrendRow>) {
        if self.trend_rows == rows { return; }
        self.trend_ranges = (0..rows.len()).map(|_| TrendRange::default()).collect();
        self.trend_rows = rows;
        self.index_trend_rows();
        self.area.redraw(cx);
    }

    pub(crate) fn set_record(&mut self, cx: &mut Cx, store: Option<Arc<TraceStore>>, rows: Vec<RecordRow>) {
        if self.record_rows != rows {
            self.record_cache.resize_with(rows.len(), RecordCache::default);
            for (index, row) in rows.iter().enumerate() {
                if self.record_rows.get(index).map(|row| row.row) != Some(row.row) {
                    self.record_cache[index].key = None;
                }
            }
            self.record_rows = rows;
            self.area.redraw(cx);
        }
        self.set_record_store(cx, store);
    }

    pub(crate) fn record_rows(&self) -> &[RecordRow] { &self.record_rows }

    /// A new publication keeps row labels and column buffers in place.
    pub(crate) fn set_record_store(&mut self, cx: &mut Cx, store: Option<Arc<TraceStore>>) {
        let same = match (&self.record, &store) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same { return; }
        self.record = store;
        for cache in &mut self.record_cache { cache.key = None; }
        if self.record_h > 0.0 { self.area.redraw(cx); }
    }

    fn record_layout(&self, released: bool) -> RecordLayout {
        RecordLayout::new(self.record_h, self.record_rows.len(), self.max_record_h, released)
    }

    fn bar_top(&self) -> f64 { self.record_layout(false).bar_top(self.view.pos.y) }

    fn record_edge_at(&self, p: DVec2) -> bool {
        self.view.contains(p) && p.y < self.view.pos.y + 4.0
    }

    fn record_at(&self, p: DVec2) -> bool {
        self.record_h > 0.0 && self.view.contains(p) && p.y < self.bar_top()
    }

    fn index_trend_rows(&mut self) {
        let mut columns = HashMap::new();
        if let Some(snapshot) = &self.events {
            for (index, series) in snapshot.columns.iter().enumerate() { columns.insert((series.lane, series.quantity), index); }
        }
        for (row, cache) in self.trend_rows.iter().zip(&mut self.trend_ranges) {
            cache.column = columns.get(&(row.lane, row.quantity)).copied();
            cache.key = None;
        }
    }

    pub fn height(&self) -> f64 { if self.state.collapsed() { 1.0 } else { 28.0 + TREND_H * self.trend_rows.len() as f64 + self.record_h } }

    fn trend_row_at(&self, p: DVec2) -> Option<usize> {
        let y = p.y - self.bar_top() - 28.0;
        let row = (y / TREND_H) as usize;
        (y >= 0.0 && row < self.trend_rows.len() && p.x >= self.bar.pos.x && p.x < self.bar.pos.x + self.bar.size.x).then_some(row)
    }

    fn trend_press(&self, p: DVec2, dpi: f64) -> Option<f64> {
        let index = self.trend_row_at(p)?;
        let snapshot = self.events.as_ref()?;
        let cache = &self.trend_ranges[index];
        let column = snapshot.columns.get(cache.column?)?;
        let span = self.state.span;
        let range = if cache.key == Some((self.event_generation, span)) { cache.range }
            else { column.values.extremes(snapshot.list.partition_point(|t| t < span.0), snapshot.list.partition_point(|t| t <= span.1)).map(|(min, _, max, _)| (min, max)) }?;
        let top = self.bar_top() + 28.0 + index as f64 * TREND_H + 2.0;
        self.state.trend_at(&snapshot.list, &column.values, (p.x - self.bar.pos.x) * dpi,
            (self.bar.size.x * dpi).floor() as usize, (p.y - top) / (TREND_H - 4.0), range)
    }

    pub(crate) fn set_event_channel(&mut self, cx: &mut Cx, channel: usize) {
        if self.event_channel == channel { return; }
        self.event_channel = channel;
        self.area.redraw(cx);
    }

    /// The host's view of the run changed.
    pub fn set_model(&mut self, cx: &mut Cx, model: StripModel) {
        let span_changed = self.state.span != model.span;
        self.state.span = model.span;
        self.state.live = model.live;
        if self.state.collapsed() {
            self.state.loop_range = None;
            self.marked = None;
        }
        self.model = model;
        if span_changed { self.wake(cx); }
        self.redraw_all(cx);
    }

    /// A cursor broadcast arrived (from this strip or another view).
    pub fn hear(&mut self, cx: &mut Cx, c: TimeCursor) {
        if !c.playing { self.stop_at_trigger.cancel(); }
        let now = cx.seconds_since_app_start();
        self.now = now;
        self.cursor.hear(c, now);
        self.prev_t = c.t;
        self.wake(cx);
        self.area.redraw(cx);
    }

    /// The shared cursor extrapolated to the strip's current time.
    pub fn cursor(&self) -> TimeCursor {
        self.cursor.cursor(self.now)
    }

    /// A rate control changed without moving the cursor, including while paused.
    pub fn set_rate(&mut self, cx: &mut Cx, rate: f64) {
        let cursor = self.current(cx);
        if cursor.rate != rate { self.hear(cx, TimeCursor { rate, ..cursor }); }
    }

    pub(super) fn set_transport(&mut self, cx: &mut Cx, state: TakeState, can_play: bool, waiting: u32) {
        if (self.take_state, self.can_play, self.waiting) != (state, can_play, waiting) {
            if self.can_play != can_play && self.tipped == Some(Part::Play) { self.tipped = None; }
            if self.waiting != waiting && self.tipped == Some(Part::RecLoop) { self.tipped = None; }
            self.take_state = state;
            self.can_play = can_play;
            self.waiting = waiting;
            self.area.redraw(cx);
            self.hover_transport(cx);
        }
    }

    fn tip(&self, part: Part) -> std::borrow::Cow<'static, str> {
        if part == Part::RecLoop && self.waiting > 0 {
            let n = self.waiting;
            return format!("Record a loop and play it round: {n} change{} wait{} for it (R)",
                if n == 1 { "" } else { "s" }, if n == 1 { "s" } else { "" }).into();
        }
        self.base_tip(part).into()
    }

    fn base_tip(&self, part: Part) -> &'static str {
        match part {
            Part::Start => "Back to the start (Home)",
            Part::Play if !self.can_play => "Nothing recorded yet: press Record",
            Part::Play => "Play the recording from the playhead (Space)",
            Part::Pause => "Pause here (Space)",
            Part::Stop => "Stop recording and hold here",
            Part::RecLoop => "Record a loop and play it round (R)",
            Part::RecContinuous => "Record continuously without sound",
            Part::RecLive => "Record live and hear it as it is made",
            _ => part.name(),
        }
    }

    fn hover_transport(&mut self, cx: &mut Cx) {
        if !self.transport { return; }
        let hit = self.pointer.and_then(|p| self.parts.iter().copied().find(|(_, r)| r.contains(p)));
        let part = hit.map(|(part, _)| part);
        if self.tipped != part {
            self.tipped = part;
            let tip = hit.map_or(TipAction::HoverOut, |(part, r)| TipAction::HoverIn(self.tip(part).into_owned(), r));
            cx.widget_action(self.uid, tip);
        }
    }

    pub fn state(&self) -> &StripState {
        &self.state
    }

    fn redraw_all(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }

    fn wake(&mut self, cx: &mut Cx) {
        let c = self.shown();
        if c.playing && !self.state.collapsed() && !self.state.at_head(c) && !cx.next_frame_is_pending(self.next_frame) {
            self.next_frame = cx.new_next_frame();
        }
    }

    fn shown(&self) -> TimeCursor {
        let mut c = self.cursor.cursor(self.now);
        if self.state.at_head(c) { c.t = self.state.span.1; }
        c
    }

    /// Hear and announce a cursor this strip made.
    fn emit(&mut self, cx: &mut Cx, c: TimeCursor) {
        if !c.playing { self.stop_at_trigger.cancel(); }
        let now = cx.seconds_since_app_start();
        self.now = now;
        self.cursor.hear(c, now);
        self.prev_t = c.t;
        self.wake(cx);
        cx.widget_action(self.uid, TimeStripAction::Cursor(c));
        self.area.redraw(cx);
    }

    pub fn waiting_for_trigger(&self) -> bool { self.stop_at_trigger.armed() }

    pub fn arm_next_trigger(&mut self, cx: &mut Cx, events: u64) {
        self.stop_at_trigger.arm(events);
        self.area.redraw(cx);
    }

    /// Called by the bench with the feed's atomic event counter; never waits on acquisition.
    pub fn observe_trigger(&mut self, cx: &mut Cx, events: u64) {
        if self.stop_at_trigger.observe(events) {
            let c = self.current(cx);
            self.emit(cx, TimeCursor { playing: false, ..c });
        }
    }

    /// Wait for the record to include the event, then use the ordinary cursor action.
    pub fn stop_on_violation(&mut self, cx: &mut Cx, t: f64) -> bool {
        if self.state.collapsed() || t < self.state.span.0 || t > self.state.span.1 { return false; }
        self.scrub_to(cx, t);
        self.cursor.release(self.now);
        true
    }

    fn current(&mut self, cx: &mut Cx) -> TimeCursor {
        self.now = cx.seconds_since_app_start();
        self.cursor.cursor(self.now)
    }

    fn press(&mut self, cx: &mut Cx, part: Part, shift: bool) {
        let c = self.current(cx);
        match part {
            Part::Start if self.transport => cx.widget_action(self.uid, TimeStripAction::Start),
            Part::Start => {
                let now = self.now;
                let c = self.cursor.scrub_to(self.state.span.0);
                self.cursor.release(now);
                self.emit(cx, c);
            }
            Part::Play => {
                if (self.transport && !self.can_play) || (self.state.collapsed() && !self.state.live) {
                    return;
                }
                let t = self.state.play_from(c.t);
                self.emit(cx, TimeCursor { t, playing: true, ..c });
            }
            Part::Pause => self.emit(cx, TimeCursor { playing: false, ..c }),
            Part::Stop => {
                // Hold the heard position and keep the recorded span.
                cx.widget_action(self.uid, TimeStripAction::Stop);
                self.hear(cx, TimeCursor { playing: false, ..c });
            }
            Part::StopAtTrigger => {
                if self.state.collapsed() { return; }
                cx.widget_action(self.uid, TimeStripAction::StopAtTrigger);
                let t = if c.playing { c.t } else { self.state.play_from(c.t) };
                self.emit(cx, TimeCursor { t, playing: true, ..c });
            }
            Part::StepBack => self.emit(cx, self.state.step(c, -1)),
            Part::StepForward => match self.state.step_ahead(c, self.model.ratio) {
                Some(dt) => cx.widget_action(self.uid, TimeStripAction::StepAhead(dt)),
                None => self.emit(cx, self.state.step(c, 1)),
            },
            Part::Rate => {
                let rate = self.rate_step(c.rate, if shift { -1 } else { 1 });
                self.emit(cx, TimeCursor { rate, ..c });
            }
            Part::RecLoop => cx.widget_action(self.uid, TimeStripAction::Record(RunMode::Loop)),
            Part::RecContinuous => cx.widget_action(self.uid, TimeStripAction::Record(RunMode::Continuous)),
            Part::RecLive => cx.widget_action(self.uid, TimeStripAction::Record(RunMode::Realtime)),
            Part::Loop => {
                self.state.loop_range = match self.state.loop_range {
                    Some(_) => None,
                    None if self.state.collapsed() => None,
                    None => Some(self.marked.unwrap_or(self.state.span)),
                };
                cx.widget_action(self.uid, TimeStripAction::Loop(self.state.loop_range));
                self.area.redraw(cx);
            }
            Part::Ghost => {
                if let Some(g) = self.model.ghost {
                    let g = g.flipped();
                    self.model.ghost = Some(g);
                    cx.widget_action(self.uid, TimeStripAction::Ghost(g));
                    self.area.redraw(cx);
                }
            }
        }
    }

    /// Transport keys, for the strip and for a pane that forwards its own key events. True when
    /// the key was one of the strip's.
    pub fn handle_key(&mut self, cx: &mut Cx, key: KeyCode, shift: bool) -> bool {
        if self.state.collapsed() && !self.state.live {
            return false;
        }
        let c = self.current(cx);
        match key {
            KeyCode::Space => {
                let running = if self.transport {
                    !matches!(self.take_state, TakeState::Stopped | TakeState::Paused)
                } else { c.playing };
                self.press(cx, if running { Part::Pause } else { Part::Play }, shift);
            }
            KeyCode::Home if self.transport => self.press(cx, Part::Start, shift),
            KeyCode::KeyR if self.transport => self.press(cx, Part::RecLoop, shift),
            _ if self.transport => return false,
            KeyCode::KeyS => self.press(cx, Part::StopAtTrigger, shift),
            KeyCode::ArrowLeft => self.press(cx, Part::StepBack, shift),
            KeyCode::ArrowRight => self.press(cx, Part::StepForward, shift),
            KeyCode::LBracket | KeyCode::RBracket => {
                if let Some(t) = self.events.as_ref().and_then(|events| if shift { self.state.adjacent_violation(&events.list, &events.violations, c.t, key == KeyCode::RBracket) }
                    else { self.state.adjacent_event(&events.list, c.t, key == KeyCode::RBracket) }) {
                    self.scrub_to(cx, t);
                    self.cursor.release(self.now);
                }
            }
            KeyCode::KeyJ => {
                let rate = self.rate_step(c.rate, -1);
                self.emit(cx, TimeCursor { rate, ..c });
            }
            KeyCode::KeyK => self.press(cx, Part::Pause, shift),
            KeyCode::KeyL => {
                let rate = if c.playing { self.rate_step(c.rate, 1) } else { c.rate };
                let t = self.state.play_from(c.t);
                self.emit(cx, TimeCursor { t, rate, playing: true, ..c });
            }
            _ => return false,
        }
        true
    }

    /// The rate slider's track under `p`, when the slider is drawn (a wide strip).
    fn slider_at(&self, p: DVec2) -> Option<Rect> {
        if !self.slider_on { return None; }
        self.parts.iter().find(|(part, r)| *part == Part::Rate && r.contains(p)).map(|(_, r)| *r)
    }

    /// The slider at pointer `x`: the rate there, announced when it changed (the transport sends it
    /// to the run, so a drag answers live).
    fn slide(&mut self, cx: &mut Cx, track: Rect, x: f64) {
        let inner = (track.size.x - KNOB_W).max(1.0);
        let rate = StripState::rate_at_slider((x - track.pos.x - KNOB_W * 0.5) / inner, inner);
        let c = self.current(cx);
        if c.rate != rate { self.emit(cx, TimeCursor { rate, ..c }); }
        self.area.redraw(cx);
    }

    /// The rate slider in `r`: a track, the 1x mark and the knob at `rate`.
    fn draw_slider(&mut self, cx: &mut Cx2d, r: Rect, rate: f64) {
        let tk = self.tokens();
        let hovered = self.pointer.is_some_and(|p| r.contains(p)) || matches!(self.drag, Some(Drag::Rate { .. }));
        let inner = r.size.x - KNOB_W;
        let x_of = |p: f64| r.pos.x + KNOB_W * 0.5 + p * inner;
        let cy = r.pos.y + r.size.y * 0.5;
        self.quad(cx, Rect { pos: dvec2(r.pos.x + KNOB_W * 0.5, cy - 1.0), size: dvec2(inner, 2.0) }, tk.block_frame);
        let one = x_of(StripState::slider_of(1.0));
        self.quad(cx, Rect { pos: dvec2(one - 0.5, cy - 5.0), size: dvec2(1.0, 10.0) }, tk.ink_dim);
        let knob = x_of(StripState::slider_of(rate));
        let fill = if hovered { tk.selection } else { tk.ink };
        self.quad(cx, Rect { pos: dvec2(knob - KNOB_W * 0.5, cy - 7.0), size: dvec2(KNOB_W, 14.0) }, fill);
        self.parts.push((Part::Rate, r));
    }

    fn rate_step(&self, rate: f64, steps: i32) -> f64 {
        if self.state.live { StripState::live_rate_step(rate, steps) } else { StripState::rate_step(rate, steps) }
    }

    fn scrub(&mut self, cx: &mut Cx, x: f64) {
        let t = self.state.t_of(x, self.bar.pos.x, self.bar.size.x);
        self.scrub_to(cx, t);
    }

    fn scrub_to(&mut self, cx: &mut Cx, t: f64) {
        let t = self.state.clamp(t);
        self.now = cx.seconds_since_app_start();
        self.stop_at_trigger.cancel();
        let c = self.cursor.scrub_to(t);
        self.prev_t = t;
        cx.widget_action(self.uid, TimeStripAction::Cursor(c));
        self.area.redraw(cx);
    }

    fn part_at(&self, p: DVec2) -> Option<Part> {
        self.parts.iter().find(|(_, r)| r.contains(p)).map(|(part, _)| *part)
    }

    fn tokens(&self) -> &'static Tokens {
        Tokens::for_theme(self.dark)
    }

    fn quad(&mut self, cx: &mut Cx2d, rect: Rect, c: [f32; 4]) {
        self.draw_shape.color = vec4(c[0], c[1], c[2], c[3]);
        self.draw_shape.draw_abs(cx, rect);
    }

    fn text(&mut self, cx: &mut Cx2d, x: f64, y: f64, s: &str, c: [f32; 4]) {
        self.draw_text.color = vec4(c[0], c[1], c[2], c[3]);
        let text = self.text_cache.layout(cx, &self.draw_text, s);
        let size = dvec2(f64::from(text.size_in_lpxs.width * self.draw_text.font_scale), f64::from(text.size_in_lpxs.height * self.draw_text.font_scale));
        self.draw_text.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos: dvec2(x, y), size }), &text);
    }

    fn text_w(&self, s: &str) -> f64 {
        s.chars().count() as f64 * self.char_w
    }
}

fn with_alpha(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], c[3] * a]
}

impl WidgetNode for TimeStrip {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn walk(&mut self, _: &mut Cx) -> Walk {
        self.walk
    }
    fn area(&self) -> Area {
        self.area
    }
    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }
    fn children(&self, _visit: &mut dyn FnMut(LiveId, WidgetRef)) {}
    fn find_widgets_from_point(&self, _cx: &Cx, _p: DVec2, _found: &mut dyn FnMut(&WidgetRef)) {}
}

impl Widget for TimeStrip {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        // A new run still offers the recording controls.
        let collapsed = self.state.collapsed() && !self.state.live && !self.transport;
        let walk = if collapsed {
            Walk { height: Size::Fixed(1.0), ..walk }
        } else {
            walk
        };
        let view = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, view);
        self.view = view;
        self.drawn_at = view.pos;
        self.parts.clear();
        cx.push_clip_rect(view);
        let c = self.shown();
        if self.transport {
            self.draw_transport(cx, view);
            self.bar = Rect::default();
            self.slider_on = false;
            cx.pop_clip_rect();
            return DrawStep::done();
        }
        self.dark = base_theme(cx) != BaseTheme::Light;
        self.text_cache.begin_frame(&self.draw_text, base_theme(cx));
        let advance = self.text_cache.get(cx, &self.draw_text, "0000000000").width / 10.0;
        if advance > 0.0 { self.char_w = advance; }
        let tk = self.tokens();
        if collapsed {
            let px = 1.0 / cx.current_dpi_factor();
            self.quad(cx, Rect { pos: view.pos, size: dvec2(view.size.x, px) }, tk.ink_dim);
            self.bar = Rect { pos: view.pos, size: dvec2(0.0, 0.0) };
            cx.pop_clip_rect();
            return DrawStep::done();
        }
        self.quad(cx, view, tk.canvas);
        self.compact = view.size.x < COMPACT_WIDTH;
        let cy = self.bar_top() + 14.0;
        let btn_y = cy - BTN * 0.5;
        let mut x = view.pos.x + GAP;
        let mut right = view.pos.x + view.size.x - GAP;

        // Left: transport buttons.
        let transport = &[Part::Start, Part::Play, Part::Pause, Part::StopAtTrigger, Part::StepBack, Part::StepForward];
        for &part in transport {
            let r = Rect { pos: dvec2(x, btn_y), size: dvec2(BTN, BTN) };
            let active = (part == Part::Play && c.playing) || (part == Part::Pause && !c.playing) || (part == Part::StopAtTrigger && self.stop_at_trigger.armed());
            self.draw_chip(cx, part, r, "", active);
            x += BTN + GAP;
        }

        // Right: ghost and loop chips.
        if self.model.ghost.is_some() {
            let w = self.text_w("A|B") + 12.0;
            right -= w;
            let r = Rect { pos: dvec2(right, btn_y), size: dvec2(w, BTN) };
            let on_top = self.model.ghost == Some(GhostState::OnTop);
            self.draw_chip(cx, Part::Ghost, r, "A|B", on_top);
            right -= GAP;
        }
        if !self.transport {
            let w = if self.compact { BTN } else { self.text_w("loop") + 12.0 };
            right -= w;
            let r = Rect { pos: dvec2(right, btn_y), size: dvec2(w, BTN) };
            self.draw_chip(cx, Part::Loop, r, "", self.state.loop_range.is_some());
            right -= GAP;
        }

        // Read-outs: the rate chip, `t = ..` and `sim 1 : N`; folded into one chip when narrow.
        if !self.violation_text.is_empty() {
            let text = std::mem::take(&mut self.violation_text);
            right -= self.text_w(&text);
            self.text(cx, right, cy - FONT_H * 0.5, &text, InstrumentLook::for_theme(self.dark).canvas.violation);
            right -= GAP * 2.0;
            self.violation_text = text;
        }
        let key = (c.t, c.rate, self.model.ratio);
        if self.readout_key != Some(key) {
            self.time_text = format!("t = {}", format_time(c.t));
            self.rate_text = format_rate(c.rate);
            self.ratio_text = self.model.ratio.map(format_ratio).unwrap_or_default();
            self.compact_text = format!("{}  {}", self.rate_text, self.time_text);
            self.readout_key = Some(key);
        }
        let time = std::mem::take(&mut self.time_text);
        let rate = std::mem::take(&mut self.rate_text);
        let ratio = std::mem::take(&mut self.ratio_text);
        let label = std::mem::take(&mut self.compact_text);
        // A narrow strip keeps the slider while it has room for one, without the read-out.
        let rw = self.text_w("1/1000x").max(self.text_w(&rate));
        let time_least = self.text_w("t = 000.0 ms");
        let room = right - x - (GAP + rw + GAP + BAR_MIN + time_least + GAP * 2.0);
        self.slider_on = !self.compact || room >= SLIDER_MIN;
        if self.compact && self.slider_on {
            let w = self.text_w(&time).max(time_least);
            right -= w;
            self.text(cx, right, cy - FONT_H * 0.5, &time, tk.ink);
            right -= GAP * 2.0;
            let sw = room.min(SLIDER_W);
            let r = Rect { pos: dvec2(x, btn_y), size: dvec2(sw, BTN) };
            self.draw_slider(cx, r, c.rate);
            x += sw + GAP;
            self.text(cx, x, cy - FONT_H * 0.5, &rate, tk.ink);
            x += rw + GAP * 2.0;
        } else if self.compact {
            let w = self.text_w(&label) + 12.0;
            right -= w;
            let r = Rect { pos: dvec2(right, btn_y), size: dvec2(w, BTN) };
            self.draw_chip(cx, Part::Rate, r, &label, false);
            right -= GAP;
        } else {
            let time_w = self.text_w(&time).max(self.text_w("t = 000.0 ms"));
            let sw = (right - x - (GAP + rw + GAP + GAP + BAR_MIN + time_w + GAP * 2.0)).clamp(SLIDER_MIN, SLIDER_W);
            if !ratio.is_empty() {
                let w = self.text_w(&ratio);
                right -= w;
                self.text(cx, right, cy - FONT_H * 0.5, &ratio, tk.ink_dim);
                right -= GAP * 2.0;
            }
            right -= time_w;
            self.text(cx, right, cy - FONT_H * 0.5, &time, tk.ink);
            right -= GAP * 2.0;
            let r = Rect { pos: dvec2(x, btn_y), size: dvec2(sw, BTN) };
            self.draw_slider(cx, r, c.rate);
            x += sw + GAP;
            self.text(cx, x, cy - FONT_H * 0.5, &rate, tk.ink);
            x += rw + GAP;
            x += GAP;
        }
        self.time_text = time;
        self.rate_text = rate;
        self.ratio_text = ratio;
        self.compact_text = label;

        // One icon batch above the row's chip backgrounds.
        self.draw_icon.begin_many_instances(cx);
        for &(part, r) in &self.parts {
            let Some(icon) = part.icon() else { continue };
            let active = (part == Part::Play && c.playing) || (part == Part::Pause && !c.playing) || (part == Part::StopAtTrigger && self.stop_at_trigger.armed()) || (part == Part::Loop && self.state.loop_range.is_some());
            let ink = if active { tk.ink } else { tk.ink_dim };
            self.draw_icon.icon = icon;
            self.draw_icon.color = vec4(ink[0], ink[1], ink[2], ink[3]);
            let size = dvec2(16.0, 16.0);
            self.draw_icon.draw_abs(cx, Rect { pos: r.pos + (r.size - size) * 0.5, size });
        }
        self.draw_icon.end_many_instances(cx);

        // The bar between.
        let bar = Rect { pos: dvec2(x, cy - 9.0), size: dvec2((right - x).max(0.0), 18.0) };
        self.bar = bar;
        if bar.size.x > 8.0 && !self.state.collapsed() {
            self.draw_record(cx, bar);
            self.draw_bar(cx, bar, c);
            self.draw_trends(cx, bar);
        }
        self.drawn_at = view.pos;
        cx.pop_clip_rect();
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Some(nf) = self.next_frame.is_event(event) {
            self.now = nf.time;
            let c = self.cursor.cursor(self.now);
            if let Some(s) = self.state.settle(self.prev_t, c) {
                self.emit(cx, s);
            } else {
                self.prev_t = c.t;
            }
            let c = self.shown();
            if c.playing && !self.state.collapsed() && !self.state.at_head(c) && !cx.next_frame_is_pending(self.next_frame) {
                self.next_frame = cx.new_next_frame();
            }
            self.area.redraw(cx);
        }
        let placed = self.area.rect(cx);
        if placed.size.x > 0.0 {
            // The parts and bar were laid out where the strip first drew; follow it if moved.
            let shift = placed.pos - self.drawn_at;
            if shift != dvec2(0.0, 0.0) {
                self.bar.pos += shift;
                for (_, r) in &mut self.parts {
                    r.pos += shift;
                }
                self.drawn_at = placed.pos;
            }
            self.view = placed;
        }
        if self.state.collapsed() && !self.state.live && !self.transport { return; }
        let spanned = !self.transport && !self.state.collapsed();
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                self.pointer = Some(e.abs);
                let hand = spanned && (self.bar.contains(e.abs) || self.record_at(e.abs) || self.trend_row_at(e.abs).is_some()) || self.part_at(e.abs).is_some();
                cx.set_cursor(if spanned && self.record_edge_at(e.abs) || matches!(self.drag, Some(Drag::Record { .. })) { MouseCursor::NsResize }
                    else if hand { MouseCursor::Hand } else { MouseCursor::Default });
                // Hover only recolours and names a marker; it never changes layout.
                self.area.redraw(cx);
                self.hover_transport(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.pointer = None;
                self.hover_transport(cx);
                cx.set_cursor(if matches!(self.drag, Some(Drag::Record { .. })) { MouseCursor::NsResize } else { MouseCursor::Default });
                self.area.redraw(cx);
            }
            Hit::FingerDown(e) if e.device.is_primary_hit() => {
                cx.set_key_focus(self.area);
                if spanned && self.record_edge_at(e.abs) {
                    if e.tap_count == 2 {
                        self.record_h = 0.0;
                        self.drag = None;
                    } else {
                        self.drag = Some(Drag::Record { y: e.abs.y, height: self.record_h });
                    }
                    cx.set_cursor(MouseCursor::NsResize);
                    self.area.redraw(cx);
                } else if let Some(track) = self.slider_at(e.abs) {
                    if e.tap_count == 2 {
                        let c = self.current(cx);
                        if c.rate != 1.0 { self.emit(cx, TimeCursor { rate: 1.0, ..c }); }
                        self.area.redraw(cx);
                    } else {
                        self.drag = Some(Drag::Rate { track });
                        self.slide(cx, track, e.abs.x);
                    }
                } else if let Some(part) = self.part_at(e.abs) {
                    self.press(cx, part, e.modifiers.shift);
                } else if !spanned {
                    // Nothing to scrub, mark or read before the run has a span.
                } else if let Some(t) = self.trend_press(e.abs, cx.get_dpi_factor_of(&self.area)) {
                    self.drag = Some(Drag::Scrub);
                    self.scrub_to(cx, t);
                    cx.set_cursor(MouseCursor::Hand);
                } else if self.bar.contains(e.abs) || self.record_at(e.abs) {
                    if e.modifiers.shift {
                        let t = self.state.t_of(e.abs.x, self.bar.pos.x, self.bar.size.x);
                        self.marked = Some((t, t));
                        self.drag = Some(Drag::Mark { from: t });
                    } else {
                        self.drag = Some(Drag::Scrub);
                        let dpi = cx.get_dpi_factor_of(&self.area);
                        let t = self.events.as_ref().and_then(|events| {
                            let x = (e.abs.x - self.bar.pos.x) * dpi;
                            let width = (self.bar.size.x * dpi).floor() as usize;
                            self.state.violation_at(&events.list, &events.violations, x, width)
                                .or_else(|| self.state.event_at(&events.list, x, width, 3.0 * dpi))
                        })
                            .unwrap_or_else(|| self.state.t_of(e.abs.x, self.bar.pos.x, self.bar.size.x));
                        self.scrub_to(cx, t);
                    }
                    cx.set_cursor(MouseCursor::Hand);
                }
            }
            Hit::FingerMove(e) => match self.drag {
                Some(Drag::Rate { track }) => self.slide(cx, track, e.abs.x),
                Some(Drag::Record { y, height }) => {
                    self.record_h = RecordLayout::new(height + y - e.abs.y, self.record_rows.len(), self.max_record_h, false).height;
                    cx.set_cursor(MouseCursor::NsResize);
                    self.area.redraw(cx);
                }
                Some(Drag::Scrub) => self.scrub(cx, e.abs.x),
                Some(Drag::Mark { from }) => {
                    let t = self.state.t_of(e.abs.x, self.bar.pos.x, self.bar.size.x);
                    self.marked = Some((from.min(t), from.max(t)));
                    if let Some(l) = &mut self.state.loop_range {
                        *l = self.marked.unwrap_or(*l);
                    }
                    self.area.redraw(cx);
                }
                None => {}
            },
            Hit::FingerUp(_) => match self.drag.take() {
                Some(Drag::Rate { .. }) => self.area.redraw(cx),
                Some(Drag::Record { .. }) => {
                    self.record_h = self.record_layout(true).height;
                    self.area.redraw(cx);
                }
                Some(Drag::Scrub) => {
                    self.now = cx.seconds_since_app_start();
                    self.cursor.release(self.now);
                    self.area.redraw(cx);
                }
                Some(Drag::Mark { .. }) => {
                    // A click without a drag marks nothing.
                    if matches!(self.marked, Some((a, b)) if b - a <= 0.0) {
                        self.marked = None;
                    }
                    if let (Some(l), Some(m)) = (self.state.loop_range, self.marked) {
                        if l != m {
                            self.state.loop_range = Some(m);
                        }
                    }
                    if self.state.loop_range.is_some() {
                        cx.widget_action(self.uid, TimeStripAction::Loop(self.state.loop_range));
                    }
                    self.area.redraw(cx);
                }
                None => {}
            },
            Hit::KeyDown(ke) => {
                self.handle_key(cx, ke.key_code, ke.modifiers.shift);
            }
            _ => {}
        }
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Slider;
        makepad_platform::a11y::set_string(&mut node.name, "time");
        if self.transport {
            node.role = a11y_tree::Role::Group;
            makepad_platform::a11y::set_string(&mut node.name, "Recording transport");
            node.range = None;
            return;
        }
        node.value.clear();
        let _ = write!(&mut node.value, "{}", format_time(self.cursor.cursor(self.now).t));
        node.range = Some(a11y_tree::RangeValue {
            min: self.state.span.0,
            max: self.state.span.1,
            now: self.cursor.cursor(self.now).t,
            step: StripState::step_size(self.cursor.cursor(self.now).rate),
        });
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let shift = self.area.rect(cx).pos - self.drawn_at;
        let c = self.cursor.cursor(self.now);
        for (part, rect) in &self.parts {
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Button,
                bounds: makepad_platform::a11y::semantic_rect(Rect { pos: rect.pos + shift, ..*rect }),
                name: part.name().to_string(),
                actions: a11y_tree::Actions::PRESS,
                ..Default::default()
            };
            let on = match part {
                Part::Play => c.playing,
                Part::Pause => !c.playing,
                Part::StopAtTrigger => self.stop_at_trigger.armed(),
                Part::Loop => self.state.loop_range.is_some(),
                Part::Ghost => self.model.ghost == Some(GhostState::OnTop),
                _ => false,
            };
            node.states.set(a11y_tree::States::SELECTED, if self.transport { self.lit(*part) } else { on });
            if self.transport {
                node.description = self.tip(*part).into_owned();
                node.states.set(a11y_tree::States::DISABLED, *part == Part::Play && !self.can_play);
            }
            if *part == Part::Rate && self.slider_on {
                // The rate slider: its asked rate, settable, stepping through the 1-2-5 menu.
                node.role = a11y_tree::Role::Slider;
                node.value = format_rate(c.rate);
                node.range = Some(a11y_tree::RangeValue { min: RATES[0], max: RATES[RATES.len() - 1], now: c.rate, step: 0.0 });
                node.actions = a11y_tree::Actions::PRESS | a11y_tree::Actions::SET_VALUE | a11y_tree::Actions::INCREMENT | a11y_tree::Actions::DECREMENT;
            }
            out.push((part.key(), None, node));
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionArg, ActionResult, Actions};
        let Some(part) = cx.a11y.action_part_key(request.node).and_then(Part::from_key) else {
            return ActionResult::Unhandled;
        };
        if self.transport && !self.parts.iter().any(|(p, _)| *p == part) { return ActionResult::Unhandled; }
        if self.transport && part == Part::Play && !self.can_play { return ActionResult::Disabled; }
        match (request.action, &request.arg, part) {
            (Actions::PRESS, _, _) => self.press(cx, part, false),
            (Actions::SET_VALUE, ActionArg::Number(rate), Part::Rate) => {
                if !rate.is_finite() || *rate <= 0.0 { return ActionResult::Unsupported; }
                let c = self.current(cx);
                self.emit(cx, TimeCursor { rate: rate.clamp(RATES[0], RATES[RATES.len() - 1]), ..c });
                self.area.redraw(cx);
            }
            (Actions::INCREMENT, _, Part::Rate) => self.press(cx, part, false),
            (Actions::DECREMENT, _, Part::Rate) => self.press(cx, part, true),
            _ => return ActionResult::Unhandled,
        }
        ActionResult::Handled
    }
}

impl TimeStrip {
    fn lit(&self, part: Part) -> bool {
        match part {
            Part::Play => matches!(self.take_state, TakeState::Playing | TakeState::Recording(RunMode::Loop)),
            Part::Pause => self.take_state == TakeState::Paused,
            Part::Stop => self.take_state == TakeState::Stopped,
            Part::RecLoop => self.take_state == TakeState::Recording(RunMode::Loop),
            Part::RecContinuous => self.take_state == TakeState::Recording(RunMode::Continuous),
            Part::RecLive => matches!(self.take_state, TakeState::Live | TakeState::Behind),
            _ => false,
        }
    }

    fn draw_transport(&mut self, cx: &mut Cx2d, well: Rect) {
        let mut x = well.pos.x;
        for part in [Part::Start, Part::Play, Part::Pause, Part::Stop, Part::RecLoop, Part::RecContinuous, Part::RecLive] {
            let record = matches!(part, Part::RecLoop | Part::RecContinuous | Part::RecLive);
            // ●N: the loop button widens for the changes waiting for the next record
            let count = (part == Part::RecLoop && self.waiting > 0).then(|| self.waiting.to_string());
            let count_w = count.as_ref().map_or(0.0, |n| self.label_w(cx, n) + 3.0);
            let width = if record { 36.0 + count_w } else { 28.0 };
            let r = Rect { pos: dvec2(x, well.pos.y), size: dvec2(width, well.size.y) };
            let selected = self.lit(part);
            let disabled = part == Part::Play && !self.can_play;
            if selected || (!disabled && self.pointer.is_some_and(|p| r.contains(p))) {
                self.draw_face.color = if selected && record {
                    Vec4f { w: self.ink_record.w * 0.30, ..self.ink_record }
                } else if selected { self.face_selected } else { self.face_hover };
                self.draw_face.draw_abs(cx, r);
            }
            self.parts.push((part, r));
            self.draw_icon.icon = part.icon().unwrap();
            self.draw_icon.color = match part {
                _ if disabled => self.ink_disabled,
                _ if record => self.ink_record,
                Part::Play if selected => self.ink_play,
                Part::Pause if selected => self.ink_pause,
                _ => self.ink_normal,
            };
            let size = dvec2(if record { 24.0 } else { 16.0 }, 16.0);
            let icon_box = Rect { pos: r.pos, size: dvec2(r.size.x - count_w, r.size.y) };
            self.draw_icon.draw_abs(cx, Rect { pos: icon_box.pos + (icon_box.size - size) * 0.5, size });
            if let Some(n) = &count {
                let ink = self.ink_record;
                let x = icon_box.pos.x + (icon_box.size.x + size.x) * 0.5;
                // level with the icon: a digit's middle sits about FONT_H under the text's top in
                // the strip's font (measured), so the number reads as ●N
                self.text(cx, x, r.pos.y + r.size.y * 0.5 - FONT_H, n, [ink.x, ink.y, ink.z, ink.w]);
            }
            // a small gap parts the plain transport from the three records
            x += width + if part == Part::Stop { 5.0 } else { 2.0 };
        }
    }

    /// A label's width in the strip's font, measured (the text cache keeps it for the frame).
    fn label_w(&mut self, cx: &mut Cx2d, s: &str) -> f64 {
        self.text_cache.get(cx, &self.draw_text, s).width
    }

    fn draw_chip(&mut self, cx: &mut Cx2d, part: Part, r: Rect, label: &str, active: bool) {
        let tk = self.tokens();
        let hovered = self.pointer.map_or(false, |p| r.contains(p));
        let fill = if active {
            with_alpha(tk.selection, 0.28)
        } else if hovered {
            with_alpha(tk.hover, 0.18)
        } else {
            tk.block_fill
        };
        self.quad(cx, r, fill);
        let border = if active { tk.selection } else { tk.block_frame };
        let px = 1.0;
        self.quad(cx, Rect { pos: r.pos, size: dvec2(r.size.x, px) }, border);
        self.quad(cx, Rect { pos: dvec2(r.pos.x, r.pos.y + r.size.y - px), size: dvec2(r.size.x, px) }, border);
        self.quad(cx, Rect { pos: r.pos, size: dvec2(px, r.size.y) }, border);
        self.quad(cx, Rect { pos: dvec2(r.pos.x + r.size.x - px, r.pos.y), size: dvec2(px, r.size.y) }, border);
        if !label.is_empty() {
            let w = self.label_w(cx, label);
            let ink = if active { tk.ink } else { tk.ink_dim };
            self.text(cx, r.pos.x + (r.size.x - w) * 0.5, r.pos.y + (r.size.y - FONT_H) * 0.5, label, ink);
        }
        self.parts.push((part, r));
    }

    fn draw_trends(&mut self, cx: &mut Cx2d, bar: Rect) {
        let snapshot = self.events.clone().unwrap_or_default();
        let events = &snapshot.list;
        let columns = &snapshot.columns;
        let dpi = cx.current_dpi_factor();
        let width = (bar.size.x * dpi).floor() as usize;
        let look = InstrumentLook::for_theme(self.dark);
        let span = self.state.span;
        let i0 = events.partition_point(|t| t < span.0);
        let i1 = events.partition_point(|t| t <= span.1);
        for index in 0..self.trend_rows.len() {
            let row = &self.trend_rows[index];
            let lane = row.lane;
            let label = row.label.clone();
            let top = self.bar_top() + 28.0 + index as f64 * TREND_H;
            cx.push_clip_rect(Rect { pos: dvec2(self.view.pos.x, top), size: dvec2((bar.pos.x - self.view.pos.x - GAP).max(0.0), TREND_H) });
            self.text(cx, self.view.pos.x + GAP, top + 1.0, &label, look.text_tertiary);
            cx.pop_clip_rect();
            let cache = &mut self.trend_ranges[index];
            let Some(column) = cache.column.and_then(|index| columns.get(index)) else { continue };
            if cache.key != Some((self.event_generation, span)) {
                cache.range = column.values.extremes(i0, i1).map(|(min, _, max, _)| (min, max));
                cache.key = Some((self.event_generation, span));
            }
            let Some(range) = cache.range else { continue };
            let ink = look.channel(lane);
            for (x, first, end) in tick_ranges(events, span, width) {
                let Some((min, _, max, _)) = column.values.extremes(first, end) else { continue };
                let y0 = top + 2.0 + trend_y(max, range) * (TREND_H - 4.0);
                let y1 = top + 2.0 + trend_y(min, range) * (TREND_H - 4.0);
                self.quad(cx, Rect { pos: dvec2(bar.pos.x + x as f64 / dpi, y0), size: dvec2(1.0 / dpi, (y1 - y0).max(1.0 / dpi)) }, ink);
            }
        }
    }

    fn draw_record(&mut self, cx: &mut Cx2d, bar: Rect) {
        if self.record_h == 0.0 { return; }
        let layout = self.record_layout(false);
        let plot = Rect { pos: dvec2(bar.pos.x, self.view.pos.y), size: dvec2(bar.size.x, bar.pos.y - self.view.pos.y) };
        self.draw_range(cx, plot);
        if layout.shown == 0 { return; }
        if self.record_hidden != layout.hidden {
            self.record_hidden = layout.hidden;
            self.record_more.clear();
            if layout.hidden > 0 { let _ = write!(&mut self.record_more, "+{} lanes", layout.hidden); }
        }
        let row_h = layout.row_height();
        let dpi = cx.current_dpi_factor();
        let px = 1.0 / dpi;
        let cols = (bar.size.x * dpi / 2.0).floor().max(1.0) as u32;
        let col_w = bar.size.x / f64::from(cols);
        let span = self.state.span;
        let store = self.record.clone();
        let key = store.as_ref().map(|store| (Arc::as_ptr(store) as usize, store.len(), span, cols));
        for index in 0..layout.shown {
            let top = self.view.pos.y + index as f64 * row_h;
            let row = &self.record_rows[index];
            let (signal, hue) = (row.row, row.hue);
            let label = std::mem::take(&mut self.record_rows[index].label);
            let more = std::mem::take(&mut self.record_more);
            let show_more = index + 1 == layout.shown && layout.hidden > 0;
            let label_right = bar.pos.x - GAP;
            let more_w = if show_more { self.text_w(&more) + GAP } else { 0.0 };
            let label_rect = Rect { pos: dvec2(self.view.pos.x, top), size: dvec2((label_right - more_w - self.view.pos.x).max(0.0), row_h) };
            cx.push_clip_rect(label_rect);
            self.text(cx, self.view.pos.x + GAP, top + (row_h - FONT_H) * 0.5, &label, [hue.x, hue.y, hue.z, hue.w]);
            cx.pop_clip_rect();
            if show_more {
                cx.push_clip_rect(Rect { pos: dvec2(self.view.pos.x, top), size: dvec2((label_right - self.view.pos.x).max(0.0), row_h) });
                self.text(cx, (label_right - more_w + GAP).max(self.view.pos.x + GAP), top + (row_h - FONT_H) * 0.5, &more, InstrumentLook::for_theme(self.dark).text_tertiary);
                cx.pop_clip_rect();
            }
            self.record_rows[index].label = label;
            self.record_more = more;
            let Some(store) = &store else { continue };
            let cache = &mut self.record_cache[index];
            if cache.key != key {
                cache.columns.reduce_trace_into(store, signal, span.0, span.1, cols);
                let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
                for column in &cache.columns.cols_data {
                    if column.lo.is_finite() && column.hi.is_finite() {
                        lo = lo.min(f64::from(column.lo));
                        hi = hi.max(f64::from(column.hi));
                    }
                }
                let margin = (hi - lo) * 0.06;
                cache.range = (lo <= hi).then_some((lo - margin, hi + margin));
                cache.key = key;
            }
            let Some((lo, hi)) = cache.range else { continue };
            let y_of = |value: f32| {
                let y = if lo == hi { 0.5 } else { (hi - f64::from(value)) / (hi - lo) };
                top + y * row_h
            };
            cx.push_clip_rect(Rect { pos: dvec2(bar.pos.x, top), size: dvec2(bar.size.x, row_h) });
            self.draw_shape.color = hue;
            for column in &cache.columns.cols_data {
                if !column.lo.is_finite() || !column.hi.is_finite() { continue; }
                let t = span.0 + (span.1 - span.0) * f64::from(column.x0.floor()) / f64::from(cols);
                let x = self.state.x_of(t, bar.pos.x, bar.size.x);
                let (y0, y1) = (y_of(column.hi), y_of(column.lo));
                let h = (y1 - y0).max(px);
                self.draw_shape.draw_abs(cx, Rect { pos: dvec2(x, (y0 + y1 - h) * 0.5), size: dvec2(col_w, h) });
            }
            cx.pop_clip_rect();
        }
    }

    fn draw_range(&mut self, cx: &mut Cx2d, bar: Rect) {
        let tk = self.tokens();
        let st = self.state;
        if let Some((a, b)) = self.state.loop_range.or(self.marked) {
            let x0 = st.x_of(a, bar.pos.x, bar.size.x).clamp(bar.pos.x, bar.pos.x + bar.size.x);
            let x1 = st.x_of(b, bar.pos.x, bar.size.x).clamp(bar.pos.x, bar.pos.x + bar.size.x);
            let alpha = if self.state.loop_range.is_some() { 0.30 } else { 0.14 };
            self.quad(cx, Rect { pos: dvec2(x0, bar.pos.y), size: dvec2((x1 - x0).max(1.0), bar.size.y) }, with_alpha(tk.selection, alpha));
        }
    }

    fn draw_bar(&mut self, cx: &mut Cx2d, bar: Rect, c: TimeCursor) {
        let tk = self.tokens();
        let st = self.state;
        self.quad(cx, bar, tk.part_fill);
        // The loop and the marked range sit under everything else.
        self.draw_range(cx, bar);
        // Activity as a strip of soft bars, one quad per two device pixels at most.
        let n = self.model.activity.len();
        if n > 0 {
            let dpi = cx.current_dpi_factor();
            let cols = ((bar.size.x * dpi / 2.0).floor() as usize).clamp(1, n);
            let col_w = bar.size.x / cols as f64;
            let h_max = bar.size.y - 6.0;
            for k in 0..cols {
                let lo = k * n / cols;
                let hi = (((k + 1) * n / cols).max(lo + 1)).min(n);
                let a = self.model.activity[lo..hi].iter().fold(0.0f32, |m, v| m.max(*v)).clamp(0.0, 1.0);
                if a <= 0.0 {
                    continue;
                }
                let h = (h_max * a as f64).max(1.0);
                let r = Rect { pos: dvec2(bar.pos.x + k as f64 * col_w, bar.pos.y + bar.size.y - 3.0 - h), size: dvec2(col_w.max(1.0), h) };
                self.quad(cx, r, with_alpha(tk.ink_dim, 0.45));
            }
        }
        // Record triggers: one device-pixel tick per occupied column, brighter with density.
        // Cloning the snapshot lets quads borrow self mutably without copying/allocating times.
        if let Some(snapshot) = self.events.clone() {
            let events = &snapshot.list;
            let dpi = cx.current_dpi_factor();
            let width = (bar.size.x * dpi).floor() as usize;
            let ink = InstrumentLook::for_theme(self.dark).channel(self.event_channel);
            for (x, count) in tick_columns(events, st.span, width) {
                let alpha = 0.18 + 0.62 * (1.0 - 1.0 / count as f32);
                self.quad(cx, Rect { pos: dvec2(bar.pos.x + x as f64 / dpi, bar.pos.y + bar.size.y - 5.0), size: dvec2(1.0 / dpi, 5.0) }, with_alpha(ink, alpha));
            }
            for (x, _) in violation_columns(events, &snapshot.violations, st.span, width) {
                self.quad(cx, Rect { pos: dvec2(bar.pos.x + x as f64 / dpi, bar.pos.y), size: dvec2(1.0 / dpi, bar.size.y - 5.0) }, InstrumentLook::for_theme(self.dark).canvas.violation);
            }
        }
        // Markers (ink ticks) and failures (violation ticks).
        for i in 0..self.model.markers.len() {
            let x = st.x_of(self.model.markers[i].0, bar.pos.x, bar.size.x);
            if x < bar.pos.x || x > bar.pos.x + bar.size.x {
                continue;
            }
            self.quad(cx, Rect { pos: dvec2(x - 0.5, bar.pos.y), size: dvec2(1.0, bar.size.y * 0.5) }, tk.ink);
        }
        for i in 0..self.model.failures.len() {
            let x = st.x_of(self.model.failures[i], bar.pos.x, bar.size.x);
            if x < bar.pos.x || x > bar.pos.x + bar.size.x {
                continue;
            }
            self.quad(cx, Rect { pos: dvec2(x - 1.0, bar.pos.y + bar.size.y - 6.0), size: dvec2(2.0, 6.0) }, tk.violation);
        }
        // A hovered marker names itself beside its tick.
        if let Some(p) = self.pointer.filter(|p| bar.contains(*p)) {
            let hit = self
                .model
                .markers
                .iter()
                .filter(|m| (st.x_of(m.0, bar.pos.x, bar.size.x) - p.x).abs() <= 4.0)
                .map(|m| (st.x_of(m.0, bar.pos.x, bar.size.x), m.1.clone()))
                .next();
            if let Some((x, name)) = hit {
                let w = self.text_w(&name);
                let tx = if x + 6.0 + w > bar.pos.x + bar.size.x { x - 6.0 - w } else { x + 6.0 };
                self.text(cx, tx, bar.pos.y + 2.0, &name, tk.ink);
            }
        }
        // The cursor: a 2 px accent line with a knob.
        let accent = match c.source {
            CursorSource::Sim | CursorSource::Replay => tk.selection,
            CursorSource::Motion => tk.ai,
        };
        let x = st.x_of(c.t, bar.pos.x, bar.size.x).clamp(bar.pos.x, bar.pos.x + bar.size.x);
        let (top, height) = if self.record_h > 0.0 { (self.view.pos.y, bar.pos.y + bar.size.y - self.view.pos.y) } else { (bar.pos.y, bar.size.y) };
        self.quad(cx, Rect { pos: dvec2(x - 1.0, top), size: dvec2(2.0, height) }, accent);
        self.quad(cx, Rect { pos: dvec2(x - 4.0, bar.pos.y - 1.0), size: dvec2(8.0, 8.0) }, accent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets_core::makepad_draw::cx_draw::CxDraw;
    use makepad_widgets_core::makepad_script::script;
    use std::cell::Cell;

    /// One frame of `strip` into a window-less pass; the pass and list stay alive with the strip's area.
    fn draw(cx: &mut Cx, strip: &WidgetRef, size: DVec2) -> (DrawPass, DrawList2d) {
        let pass = DrawPass::new(cx);
        let mut list = DrawList2d::new(cx);
        pass.set_size(cx, size);
        let event = DrawEvent::default();
        let mut draw = CxDraw::new(cx, &event);
        let mut cx = Cx2d::new(&mut draw);
        cx.begin_pass(&pass, None);
        list.begin_always(&mut cx);
        cx.begin_root_turtle(size, Layout::flow_down());
        strip.draw_all(&mut cx, &mut Scope::empty());
        cx.end_pass_sized_turtle();
        list.end(&mut cx);
        cx.end_pass(&pass);
        (pass, list)
    }

    /// With no samples the Bench can start a live run. Electronics requires Record.
    #[test]
    fn an_empty_live_strip_plays_in_the_bench_and_records_in_electronics() {
        for recording_well in [false, true] {
            let mut cx = Cx::new(Box::new(|_, _| {}));
            cx.init_cx_os();
            let strip = cx.with_vm(|vm| {
                makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
                crate::live::script_mod(vm);
                script_mod(vm);
                let value = vm.eval(script! {
                    use mod.prelude.widgets_internal.*
                    use mod.widgets.*
                    TimeStrip{width: Fill height: 28}
                });
                WidgetRef::script_from_value(vm, value)
            });
            strip.borrow_mut::<TimeStrip>().expect("a time strip")
                .set_model(&mut cx, StripModel { span: (0.0, 0.0), live: true, ..Default::default() });
            let press_part = |cx: &mut Cx, part| {
                let point = strip.borrow::<TimeStrip>().unwrap().parts.iter().find(|(p, _)| *p == part)
                    .map(|(_, r)| r.pos + r.size * 0.5).expect("the control has a hit rect");
                let press = Event::MouseDown(MouseDownEvent {
                    abs: point,
                    button: MouseButton::PRIMARY,
                    window_id: WindowId(1, 1),
                    modifiers: KeyModifiers::default(),
                    handled: Cell::new(Area::Empty),
                    time: 0.0,
                });
                cx.capture_actions(|cx| strip.handle_event(cx, &press, &mut Scope::empty()))
            };
            strip.borrow_mut::<TimeStrip>().unwrap().transport = recording_well;
            let _frame = draw(&mut cx, &strip, dvec2(800.0, 28.0));
            if recording_well {
                let disabled = strip.borrow::<TimeStrip>().unwrap().tip(Part::Play);
                assert_eq!(disabled, "Nothing recorded yet: press Record");
                let blocked = cx.capture_actions(|cx| strip.borrow_mut::<TimeStrip>().unwrap().press(cx, Part::Play, false));
                assert!(blocked.is_empty(), "Play cannot start a run without samples");
                let actions = press_part(&mut cx, Part::RecLoop);
                assert!(actions.iter().filter_map(|a| a.as_widget_action()).any(|a|
                    matches!(a.cast::<TimeStripAction>(), TimeStripAction::Record(RunMode::Loop))),
                    "Record can start a fresh Loop take");
                continue;
            }
            let actions = press_part(&mut cx, Part::Play);
            let played = actions.iter().filter_map(|action| action.as_widget_action())
                .map(|action| action.cast::<TimeStripAction>())
                .any(|action| matches!(action, TimeStripAction::Cursor(c) if c.playing));
            assert!(played, "the Bench can start its own live run");
        }
    }
}
