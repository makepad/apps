//! The Scope tab's model (user, 11-17: "click a wire and see the scope at the bottom with proper
//! sync timescales", "logic analyser style"), without a window, so it is tested directly. The
//! widget is [`super::logic_view`].
//!
//! Time: every lane shares one [`Timebase`] (seconds per division over ten divisions, and a pan
//! as the lag of the window's end behind its anchor). The anchor is the newest instant shown (the
//! record's end, or a scrub's instant), or the shown trigger event, which sits at [`PRE`] of the
//! window. So the axis follows simulated time: a pause freezes every lane together, and a Stop
//! (no record) empties them.
//!
//! Sources: a lane names where its samples come from ([`LaneSource`]: a side of
//! `fab_elec_bench::source`, a source, a net or a channel) and reads them through
//! [`TraceSource`]: today the simulator's record (`SimLive::store`, the same store the mini
//! traces fill from, so the tab adds no capture path); a real analyser's capture or a replay is
//! one more `TraceSource` on the same axis, and a real lane can sit under the simulated lane of
//! the same net. Decoder rows go under the lanes in the same row stack.
//!
//! Digital lanes: a reduced window ([`Columns`]) becomes runs of logic levels ([`logic_runs`])
//! against [`Thresholds`] taken from the rail of the logic pin on the net; a stretch between the
//! thresholds is [`Level::Between`] (drawn hatched, so glitches and floating inputs show), a pixel
//! holding a whole transition is [`Level::Busy`]. Which nets are digital: [`NetFacts::kind`].
//! A net a pad reads has a logic trace too (`TraceStore::logic`: its edges, at the pad's own input
//! thresholds, kept much longer than the samples): its digital lane draws from that
//! ([`edge_runs`]), so a zoom out shows the whole run in a search per edge or column, and the
//! trigger and the cursors take its exact edge times ([`last_edge`], [`nearest_logic_edge`]).
//!
//! Trigger: [`last_crossing`] finds the newest crossing of a level with a slope through any
//! `TraceSource`, from a few reductions of the window at rising resolution, never a walk of the
//! samples; [`find_trigger`] applies the auto and normal modes.

use crate::trace::{columns::Columns, cursor::{eng, Cursors}, view::TraceSource};
use fab_elec_bench::source::{ChanId, Side, SourceId};
use fab_elec_bench::trigger::Slope;
use fab_elec_circuit::{Circuit, NetKind};
use fab_elec_core::ids::NetId;
use fab_elec_parts::{PartKind, PinKind};
use fab_elec_sim::trace::LogicTrace;
use makepad_widgets_core::makepad_micro_serde::*;

/// Divisions across the window.
pub const DIVS: f64 = 10.0;
/// Where a trigger event sits in the window, as a fraction of its width from the left.
pub const PRE: f64 = 0.5;
/// The finest and coarsest time per division.
pub const MIN_DIV: f64 = 1e-9;
pub const MAX_DIV: f64 = 10.0;

/// Where a lane's samples come from.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneSource {
    pub side: Side,
    /// The source on that side; None is the simulator's record.
    pub source: Option<SourceId>,
    pub of: LaneOf,
}

/// What a lane reads in its source.
#[derive(Clone, Debug, PartialEq)]
pub enum LaneOf {
    /// A circuit net by its lowered name (the probe's target).
    Net(String),
    /// A channel of an instrument that knows no nets.
    Channel(ChanId),
}

impl LaneSource {
    /// The simulator's record of a net.
    pub fn sim(net: &str) -> LaneSource {
        LaneSource { side: Side::Sim, source: None, of: LaneOf::Net(net.to_owned()) }
    }
}

/// The next 1-2-5 step `steps` away from `value` (positive is coarser).
pub fn step_125(value: f64, steps: i32) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return value;
    }
    let decade = 10f64.powf(value.log10().floor());
    let mantissa = value / decade;
    let digit = if mantissa < 1.5 { 0 } else if mantissa < 3.5 { 1 } else { 2 };
    let mut index = digit + 3 * (decade.log10().round() as i32);
    index += steps;
    let decade = 10f64.powi(index.div_euclid(3));
    decade * [1.0, 2.0, 5.0][index.rem_euclid(3) as usize]
}

/// What the window follows: the newest instant shown, the record's first, and the shown trigger.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub head: f64,
    pub start: f64,
    pub trigger: Option<f64>,
}

/// One time axis for every lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timebase {
    /// Simulated seconds per division, a 1-2-5 step.
    pub per_div: f64,
    /// Seconds the window's start trails its unpanned place: a pan to earlier times is positive.
    pub lag: f64,
}

impl Timebase {
    pub fn new(per_div: f64) -> Timebase {
        Timebase { per_div: step_125(per_div.clamp(MIN_DIV, MAX_DIV), 0), lag: 0.0 }
    }

    pub fn span(&self) -> f64 {
        self.per_div * DIVS
    }

    fn base(&self, anchor: &Anchor, span: f64) -> f64 {
        match anchor.trigger {
            Some(t) => t - PRE * span,
            None => anchor.head - span,
        }
    }

    /// Whether the window rolls with the head: untriggered and not panned.
    pub fn follows(&self, anchor: &Anchor) -> bool {
        anchor.trigger.is_none() && self.lag == 0.0
    }

    /// The shown half-open window. Rolling at the run's start (time zero), before the head has
    /// crossed a window, the record fills it from the left; a record that keeps only its newest
    /// stretch (the run's record length) still ends the window at the head.
    pub fn window(&self, anchor: &Anchor) -> (f64, f64) {
        let span = self.span();
        let mut t0 = self.base(anchor, span) - self.lag;
        if self.follows(anchor) {
            t0 = t0.max(anchor.start.min(0.0));
        }
        (t0, t0 + span)
    }

    /// Zooms by `steps` 1-2-5 steps (positive zooms out) about the instant at `fraction` of the
    /// window, which stays where it is; a window rolling with a playing run (`playing`) keeps its
    /// end on the head instead.
    pub fn zoom(&mut self, anchor: &Anchor, steps: i32, fraction: f64, playing: bool) {
        let (t0, t1) = self.window(anchor);
        let at = t0 + fraction.clamp(0.0, 1.0) * (t1 - t0);
        let rolling = playing && self.follows(anchor);
        self.per_div = step_125(self.per_div, steps).clamp(MIN_DIV, MAX_DIV);
        if rolling {
            return;
        }
        let span = self.span();
        self.lag = self.base(anchor, span) - (at - fraction.clamp(0.0, 1.0) * span);
    }

    /// Moves the window `by` seconds later (negative: earlier).
    pub fn pan(&mut self, by: f64) {
        if by.is_finite() {
            self.lag -= by;
        }
    }
}

/// Logic thresholds in volts: below `low` is 0, above `high` is 1, between is undefined.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    pub low: f64,
    pub high: f64,
}

impl Thresholds {
    /// CMOS input thresholds of a rail: 30 % and 70 % of it.
    pub fn of_rail(rail: f64) -> Thresholds {
        Thresholds { low: 0.3 * rail, high: 0.7 * rail }
    }

    /// The level halfway between: where a trigger on a digital lane fires.
    pub fn middle(&self) -> f64 {
        0.5 * (self.low + self.high)
    }
}

/// A logic level over a stretch of a lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Low,
    High,
    /// Between the thresholds: undefined (a glitch, a slow edge, a floating input).
    Between,
    /// A pixel holding at least one whole transition: activity too fine to draw edge by edge.
    Busy,
}

/// A run of one level between two positions, in columns of the reduced window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Run {
    pub level: Level,
    pub x0: f32,
    pub x1: f32,
}

fn level_of(y: f64, th: Thresholds) -> Level {
    if y <= th.low {
        Level::Low
    } else if y >= th.high {
        Level::High
    } else {
        Level::Between
    }
}

/// The logic runs of a reduced window, left to right, contiguous runs of a level merged; nothing
/// where the window has no samples (before the record or after its head). A straight piece
/// between two samples splits exactly where it crosses a threshold, so sparse edges land at their
/// interpolated place; a dense column takes the level of its whole span. O(columns).
pub fn logic_runs(cols: &Columns, th: Thresholds, out: &mut Vec<Run>) {
    out.clear();
    let n = cols.cols as f32;
    let mut push = |out: &mut Vec<Run>, level: Level, x0: f32, x1: f32| {
        let (x0, x1) = (x0.clamp(0.0, n), x1.clamp(0.0, n));
        if !(x1 > x0) {
            return;
        }
        if let Some(last) = out.last_mut() {
            // The level goes on; or a sliver between two samples just after a busy pixel (dense
            // columns are at least three samples, so their gaps are under half a column) is busy.
            if x0 - last.x1 < 1e-3 && (last.level == level || last.level == Level::Busy && x1 - x0 < 0.5) {
                last.x1 = last.x1.max(x1);
                return;
            }
        }
        out.push(Run { level, x0, x1 });
    };
    // A straight piece from a to b, split at the thresholds it crosses.
    let piece = |out: &mut Vec<Run>, push: &mut dyn FnMut(&mut Vec<Run>, Level, f32, f32), a: (f32, f32), b: (f32, f32)| {
        let (xa, ya, xb, yb) = (f64::from(a.0), f64::from(a.1), f64::from(b.0), f64::from(b.1));
        if !(xb > xa) || !ya.is_finite() || !yb.is_finite() {
            return;
        }
        let mut cuts = [xa, xb, xb, xb];
        let mut count = 1;
        for level in [th.low, th.high] {
            if (ya - level) * (yb - level) < 0.0 {
                cuts[count] = xa + (xb - xa) * (level - ya) / (yb - ya);
                count += 1;
            }
        }
        cuts[count] = xb;
        cuts[..=count].sort_by(f64::total_cmp);
        for w in cuts[..=count].windows(2) {
            if w[1] > w[0] {
                let mid = ya + (yb - ya) * ((0.5 * (w[0] + w[1]) - xa) / (xb - xa));
                push(out, level_of(mid, th), w[0] as f32, w[1] as f32);
            }
        }
    };
    let mut previous = cols.before;
    for c in &cols.cols_data {
        if let Some(p) = previous {
            piece(out, &mut push, p, (c.x0, c.y0));
        }
        if c.count == 2 {
            piece(out, &mut push, (c.x0, c.y0), (c.x1, c.y1));
        } else {
            let (lo, hi) = (f64::from(c.lo), f64::from(c.hi));
            let level = if lo <= th.low && hi >= th.high {
                Level::Busy
            } else if hi <= th.low {
                Level::Low
            } else if lo >= th.high {
                Level::High
            } else {
                Level::Between
            };
            // From its first sample to its last: a lone sample covers nothing by itself.
            push(out, level, c.x0, c.x1);
        }
        previous = Some((c.x1, c.y1));
    }
    if let (Some(p), Some(a)) = (previous, cols.after) {
        piece(out, &mut push, p, a);
    }
}

/// Whether a lane draws as logic levels or as a waveform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerRon, DeRon)]
pub enum LaneKind {
    Digital,
    Analog,
}

/// What the design says about a net: its kind, whether a logic pin is on it, and the supply of
/// the parts that have one there (their power pins' nets' rated volts).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct NetFacts {
    pub kind: NetKind,
    pub logic: bool,
    pub rail: Option<f64>,
}

fn logic_pin(kind: PinKind) -> bool {
    matches!(kind, PinKind::In | PinKind::Out | PinKind::Bidi | PinKind::TriState | PinKind::OpenCollector | PinKind::OpenEmitter)
}

/// The facts of `net` in `circuit`: an analog part's pins (`PartKind::Analog`, an op-amp's) are no
/// logic pins. O(connections).
pub fn net_facts(circuit: &Circuit, net: NetId) -> NetFacts {
    let Some(of) = circuit.nets.get(net.0 as usize) else { return NetFacts::default() };
    let mut facts = NetFacts { kind: of.kind, ..NetFacts::default() };
    for comp in &circuit.comps {
        let on = comp.part.kind != PartKind::Analog && comp.conns.iter().any(|&(pin, n)| n == net && comp.part.pins.get(pin.0 as usize).is_some_and(|p| logic_pin(p.kind)));
        if !on {
            continue;
        }
        facts.logic = true;
        for &(pin, n) in &comp.conns {
            if comp.part.pins.get(pin.0 as usize).is_some_and(|p| p.kind == PinKind::PowerIn) {
                if let Some(volts) = circuit.nets.get(n.0 as usize).and_then(|n| n.volts.as_ref()) {
                    facts.rail = Some(facts.rail.map_or(volts.max, |rail: f64| rail.max(volts.max)));
                }
            }
        }
    }
    facts
}

impl NetFacts {
    /// Digital when a logic pin reads or drives the net and it is no supply or ground; the rest
    /// (supplies, analog nodes such as an LED's anode) draw as waveforms.
    pub fn kind(&self) -> LaneKind {
        if self.logic && self.kind == NetKind::Signal { LaneKind::Digital } else { LaneKind::Analog }
    }

    /// The lane's thresholds: its logic supply's, else the record's highest value (`record_max`),
    /// at least a volt.
    pub fn thresholds(&self, record_max: f64) -> Thresholds {
        let rail = self.rail.filter(|rail| rail.is_finite() && *rail > 0.0)
            .unwrap_or(if record_max.is_finite() { record_max.max(1.0) } else { 1.0 });
        Thresholds::of_rail(rail)
    }
}

/// Whether a net with no logic pin swings like logic in the shown window, what an override-free
/// lane of such a net switches to once (the view keeps it). Its columns sort into near ground
/// (within 15% of the window's top either side: logic never sits far below ground, while an
/// AC-coupled line swings as far below it as above), near the top (at least a volt), a pulse inside
/// one column, or between. Nearly every column must sort, and the window must show a full pulse (two
/// changes) whose levels are held: a change counts when the level it reaches is held at least as
/// long as the columns it took to cross over (and the level it left as long), and a run of pulses
/// inside columns counts once the line settles at a level after it the same way. So a single step,
/// such as an output rising to its bias at power-up, is no logic, and neither is the start of a
/// drum hit on a quiet line out: its peak is passed, not held, and the line rings on below ground.
/// O(columns).
pub fn swings_like_logic(cols: &Columns) -> bool {
    let top = cols.cols_data.iter().map(|c| c.hi).fold(f32::NEG_INFINITY, f32::max);
    if !(top >= 1.0) || cols.cols_data.len() < 8 {
        return false;
    }
    let (low, high) = (0.15 * top, 0.85 * top);
    let ground = -low;
    let (mut sorted, mut changes, mut pulses, mut crossed) = (0, 0, 0, 0);
    // the level held now and for how many columns; the change into it (the columns crossed, the
    // run it left), or the pulses it settles after (how many, the columns crossed)
    let mut run: Option<(bool, usize)> = None;
    let (mut into, mut settle) = (None, None);
    for c in &cols.cols_data {
        let level = if c.lo >= ground && c.hi <= low {
            false
        } else if c.lo >= high {
            true
        } else if c.lo >= ground && c.lo <= low && c.hi >= high {
            // a run that leaves for a pulse is held if it lasts as long as the crossing after it
            changes += held_changes(run.take().filter(|r| crossed <= r.1), &mut into, &mut settle);
            pulses = if crossed == 0 { pulses + 1 } else { 1 };
            sorted += 1;
            crossed = 0;
            continue;
        } else {
            crossed += 1;
            continue;
        };
        sorted += 1;
        match run {
            Some((was, len)) if was == level => run = Some((was, len + 1)),
            _ => {
                if pulses > 0 {
                    settle = Some((pulses, crossed));
                    pulses = 0;
                } else if let Some(left) = run {
                    changes += held_changes(Some(left), &mut into, &mut settle);
                    into = Some((crossed, left.1));
                }
                run = Some((level, 1));
            }
        }
        crossed = 0;
    }
    changes += held_changes(run.filter(|r| crossed <= r.1), &mut into, &mut settle);
    changes >= 2 && sorted * 10 >= cols.cols_data.len() * 9
}

/// The changes a run of a level confirms once it ends (`None`: it was not held): the change into
/// it, when it lasted as long as the crossing and the run left lasted as long; the pulses before
/// it (two changes each), when it lasted as long as the crossing after them.
fn held_changes(run: Option<(bool, usize)>, into: &mut Option<(usize, usize)>, settle: &mut Option<(usize, usize)>) -> usize {
    let (into, settle) = (into.take(), settle.take());
    let Some((_, len)) = run else { return 0 };
    let into = into.is_some_and(|(crossed, left)| crossed <= left.min(len));
    let settled = settle.filter(|&(_, crossed)| crossed <= len).map_or(0, |(pulses, _)| 2 * pulses);
    usize::from(into) + settled
}

/// The trigger's mode: auto rolls freely when no event is in reach; normal holds the last event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrigMode {
    Auto,
    Normal,
}

const SEARCH_COLS: u32 = 512;

fn crossing(a: (f64, f64), b: (f64, f64), level: f64, slope: Slope) -> Option<f64> {
    let (rising, falling) = (a.1 < level && b.1 >= level, a.1 > level && b.1 <= level);
    let fits = match slope {
        Slope::Rising => rising,
        Slope::Falling => falling,
        Slope::Either => rising || falling,
    };
    (fits && b.0 > a.0).then(|| a.0 + (b.0 - a.0) * (level - a.1) / (b.1 - a.1))
}

/// The newest crossing of `level` with `slope` in trace `k` of `source` within `[t0, t1]`, its
/// time interpolated between the two samples around it. Reads the span in a few hundred columns;
/// a dense column that may hold the crossing is read again at the next resolution (bounded depth),
/// so a search costs a few reductions whatever the record's length.
pub fn last_crossing(source: &dyn TraceSource, k: usize, t0: f64, t1: f64, level: f64, slope: Slope) -> Option<f64> {
    search(source, k, t0, t1, level, slope, 0)
}

fn search(source: &dyn TraceSource, k: usize, t0: f64, t1: f64, level: f64, slope: Slope, depth: u32) -> Option<f64> {
    if !(t1 > t0) || !level.is_finite() {
        return None;
    }
    let mut cols = Columns::default();
    source.read(k, t0, t1, SEARCH_COLS, &mut cols);
    let time = |x: f32| t0 + f64::from(x) * (t1 - t0) / f64::from(SEARCH_COLS);
    let within = |t: f64| (t0..=t1).contains(&t);
    let point = |p: (f32, f32)| (time(p.0), f64::from(p.1));
    let data = &cols.cols_data;
    if let (Some(last), Some(after)) = (data.last(), cols.after) {
        if let Some(t) = crossing(point((last.x1, last.y1)), point(after), level, slope).filter(|&t| within(t)) {
            return Some(t);
        }
    }
    for i in (0..data.len()).rev() {
        let c = &data[i];
        if c.count == 2 {
            if let Some(t) = crossing(point((c.x0, c.y0)), point((c.x1, c.y1)), level, slope) {
                return Some(t);
            }
        } else if c.count > 2 && f64::from(c.lo) <= level && f64::from(c.hi) >= level {
            let (a, b) = (time(c.x0), time(c.x1));
            let inner = if depth < 6 && b > a { search(source, k, a, b.next_up(), level, slope, depth + 1) } else { None };
            if let Some(t) = inner.or_else(|| crossing(point((c.x0, c.y0)), point((c.x1, c.y1)), level, slope)) {
                return Some(t);
            }
        }
        let before = if i > 0 { Some((data[i - 1].x1, data[i - 1].y1)) } else { cols.before };
        if let Some(p) = before {
            if let Some(t) = crossing(point(p), point((c.x0, c.y0)), level, slope).filter(|&t| within(t)) {
                return Some(t);
            }
        }
    }
    None
}

/// The newest trigger event in a record, or None. A playing run takes only an event whose
/// post-trigger half window has arrived (or, in a record shorter than a window, half the record
/// has, so the event still shows with what follows it); a paused one takes the newest. Auto
/// searches the last few windows, normal the whole record.
pub fn find_trigger(source: &dyn TraceSource, k: usize, anchor: &Anchor, span: f64, level: f64, slope: Slope,
    mode: TrigMode, playing: bool) -> Option<f64> {
    let (t0, t1) = trigger_search(anchor, span, mode, playing);
    last_crossing(source, k, t0, t1, level, slope)
}

/// [`find_trigger`] on a logic trace: its edges, exact, and in normal mode its whole history, which
/// reaches back past the record's samples.
pub fn find_logic_trigger(trace: &LogicTrace, anchor: &Anchor, span: f64, slope: Slope, mode: TrigMode, playing: bool) -> Option<f64> {
    let (t0, t1) = trigger_search(anchor, span, mode, playing);
    let t0 = if mode == TrigMode::Normal { f64::NEG_INFINITY } else { t0 };
    last_edge(trace, t0, t1, slope)
}

/// The span a trigger searches, as [`find_trigger`] says.
fn trigger_search(anchor: &Anchor, span: f64, mode: TrigMode, playing: bool) -> (f64, f64) {
    let after = ((1.0 - PRE) * span).min(0.5 * (anchor.head - anchor.start).max(0.0));
    let t1 = if playing { anchor.head - after } else { anchor.head };
    let t0 = match mode {
        TrigMode::Auto => (anchor.head - AUTO_SPANS * span).max(anchor.start),
        TrigMode::Normal => anchor.start,
    };
    (t0, t1)
}

/// Windows an auto trigger looks back, and keeps showing its last event for, before it rolls.
pub const AUTO_SPANS: f64 = 4.0;

/// The acquisition a scope shows after searching its newest record (`found`: that record and the
/// event in it), as a real instrument holds its last triggered capture: a new event takes the
/// newest record; without one, normal mode keeps showing the capture it has, and auto keeps it
/// for [`AUTO_SPANS`] windows of the head, then rolls (None).
pub fn hold<T>(shown: Option<(T, f64)>, found: Option<(T, f64)>, mode: TrigMode, head: f64, span: f64) -> Option<(T, f64)> {
    match (found, mode) {
        (Some(found), _) => Some(found),
        (None, TrigMode::Normal) => shown,
        (None, TrigMode::Auto) => shown.filter(|(_, t)| head - t <= AUTO_SPANS * span),
    }
}

/// The trigger: a lane (by net), a slope and a mode; the level follows the lane (the middle of a
/// digital lane's thresholds, of an analog lane's record).
#[derive(Clone, Debug, PartialEq)]
pub struct Trigger {
    pub net: String,
    pub slope: Slope,
    pub mode: TrigMode,
}

/// A lane's trigger chip pressed, with `current` the trigger: on another lane (or none) the trigger
/// moves here, rising, its mode kept; here it steps rising, falling, either edge, then off.
pub fn next_trigger(current: Option<Trigger>, net: &str) -> Option<Trigger> {
    match current {
        Some(t) if t.net == net => match t.slope {
            Slope::Rising => Some(Trigger { slope: Slope::Falling, ..t }),
            Slope::Falling => Some(Trigger { slope: Slope::Either, ..t }),
            Slope::Either => None,
        },
        other => Some(Trigger { net: net.to_owned(), slope: Slope::Rising, mode: other.map_or(TrigMode::Auto, |t| t.mode) }),
    }
}

/// One of the two time cursors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Which {
    A,
    B,
}

/// The cursor a click at `t` places: B with Shift; else A until it is set, then B until it is
/// set, then the nearer of the two (so plain clicks set and adjust both).
pub fn cursor_for_click(cursors: &Cursors, t: f64, shift: bool) -> Which {
    match (shift, cursors.a, cursors.b) {
        (true, ..) | (false, Some(_), None) => Which::B,
        (false, None, _) => Which::A,
        (false, Some(a), Some(b)) => if (t - a).abs() <= (t - b).abs() { Which::A } else { Which::B },
    }
}

/// The logic runs of `trace` in the window `[t0, t1)` of `cols` columns, up to `head` (the newest
/// instant simulated), nothing before its first entry (when its level became known). A column
/// holding one edge splits at the edge's exact place; one holding more is [`Level::Busy`]. The
/// columns up to the next edge are one run, and a stretch whose edges are never half a column
/// apart is one busy run, each found by one search; a column's entries are a few steps on from
/// the last column's. A spilled chunk not in memory draws busy over its span, from its summary,
/// until a view's ask brings it back (`LogicTrace::spilled_in`). So O(min(edges in the window,
/// cols)) steps plus a search per jump, whatever the trace's length.
pub fn edge_runs(trace: &LogicTrace, t0: f64, t1: f64, cols: u32, head: f64, out: &mut Vec<Run>) {
    out.clear();
    let Some(first) = trace.first_time() else { return };
    if !(t1 > t0) || cols == 0 {
        return;
    }
    let n = f64::from(cols);
    let per_col = (t1 - t0) / n;
    let x = |t: f64| (t - t0) / per_col;
    let time = |c: f64| t0 + c * per_col;
    let end = x(head).min(n);
    let mut push = |level: Level, x0: f64, x1: f64| {
        let (x0, x1) = (x0.max(0.0), x1.min(end));
        if !(x1 > x0) {
            return;
        }
        let (x0, x1) = (x0 as f32, x1 as f32);
        if let Some(last) = out.last_mut() {
            if last.level == level && x0 - last.x1 < 1e-3 {
                last.x1 = last.x1.max(x1);
                return;
            }
        }
        out.push(Run { level, x0, x1 });
    };
    let level = |high: bool| if high { Level::High } else { Level::Low };
    let len = trace.len();
    let mut c = x(first).floor().max(0.0);
    // the entries at or before the column's start
    let mut i = trace.upto(time(c));
    while c < end {
        let known = i.checked_sub(1).and_then(|k| trace.get(k)).map(|e| level(e.high));
        // a dense stretch: every column inside it holds two edges or more
        if let Some(last) = i.checked_sub(1).and_then(|k| trace.dense_until(k, 0.5 * per_col, time(end))) {
            let ce = x(last).floor();
            if ce > c {
                push(Level::Busy, c, ce);
                c = ce;
                i = trace.upto_from(i, time(c));
                continue;
            }
        }
        if i == len {
            if let Some(l) = known {
                push(l, c, end);
            }
            break;
        }
        // a spilled chunk not in memory: busy over its span, whole columns
        if let Some((first, last)) = trace.away(i) {
            if let Some(l) = known {
                push(l, c, x(first));
            }
            let ce = x(last).ceil().max(c + 1.0);
            push(Level::Busy, x(first).max(c), ce);
            c = ce;
            i = trace.upto(time(c));
            continue;
        }
        let Some(next) = trace.get(i) else { break };
        let ce = x(next.t).floor().max(c);
        if ce > c {
            // whole columns without an edge
            if let Some(l) = known {
                push(l, c, ce);
            }
            c = ce;
            i = trace.upto_from(i, time(c));
            continue;
        }
        let j = trace.upto_from(i, time(c + 1.0));
        // a spilled chunk not in memory starting inside the column ends what is known of it
        let stop = trace.away(j).map(|(first, _)| x(first)).filter(|&at| at < c + 1.0).unwrap_or(c + 1.0);
        match j - i {
            0 => {
                if let Some(l) = known {
                    push(l, c, stop);
                }
            }
            1 => {
                if let Some(l) = known {
                    push(l, c, x(next.t));
                }
                push(level(next.high), x(next.t), stop);
            }
            _ => push(Level::Busy, c, stop),
        }
        if stop < c + 1.0 {
            push(Level::Busy, stop, c + 1.0);
        }
        c += 1.0;
        i = j;
    }
}

/// The newest edge of `trace` with `slope` within `[t0, t1]`, at its exact time. A trace's first
/// entry is no edge (its level became known there); spilled chunks answer from their summaries
/// (`LogicTrace::last_edge`). O(log n).
pub fn last_edge(trace: &LogicTrace, t0: f64, t1: f64, slope: Slope) -> Option<f64> {
    trace.last_edge(t0, t1, |high| match slope {
        Slope::Rising => high,
        Slope::Falling => !high,
        Slope::Either => true,
    })
}

/// The edge of `trace` nearest `t` within `within` seconds, at its exact time. O(log n).
pub fn nearest_logic_edge(trace: &LogicTrace, t: f64, within: f64) -> Option<f64> {
    let i = trace.upto(t);
    [i.checked_sub(1), Some(i)].into_iter().flatten().filter(|&k| k > 0).filter_map(|k| trace.get(k)).map(|e| e.t)
        .filter(|e| (e - t).abs() <= within).min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
}

/// The logic edge nearest column `x` among `lanes`' runs, within `within` columns: where a defined
/// level or the undefined band gives way to another (a busy pixel has no single edge). O(runs).
pub fn nearest_edge<'a>(lanes: impl IntoIterator<Item = &'a [Run]>, x: f32, within: f32) -> Option<f32> {
    let mut best: Option<(f32, f32)> = None;
    for runs in lanes {
        for pair in runs.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if a.level == b.level || a.level == Level::Busy || b.level == Level::Busy {
                continue;
            }
            let d = (b.x0 - x).abs();
            if d <= within && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((b.x0, d));
            }
        }
    }
    best.map(|(edge, _)| edge)
}

/// Significant digits that show an instant `t` to a hundredth of a division (`per_div`).
pub fn time_digits(t: f64, per_div: f64) -> usize {
    if !(t.is_finite() && per_div.is_finite() && per_div > 0.0) || t == 0.0 {
        return 3;
    }
    (t.abs().log10().floor() - (per_div / 100.0).log10().floor() + 1.0).clamp(3.0, 12.0) as usize
}

/// The cursors' readout: each set cursor's instant to a hundredth of a division, and with both
/// their difference and its reciprocal ("A 35.7925 s   B 35.8125 s   Δt 20.000 ms   1/Δt 50.000 Hz").
pub fn cursor_readout(cursors: &Cursors, per_div: f64) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(a) = cursors.a {
        parts.push(format!("A {}", eng(a, "s", time_digits(a, per_div))));
    }
    if let Some(b) = cursors.b {
        parts.push(format!("B {}", eng(b, "s", time_digits(b, per_div))));
    }
    if let (Some(a), Some(b)) = (cursors.a, cursors.b) {
        let delta = b - a;
        parts.push(format!("Δt {}   1/Δt {}", eng(delta, "s", 5), eng(1.0 / delta.abs(), "Hz", 5)));
    }
    parts.join("   ")
}

/// Cursors on a triggered display keep their place relative to the trigger, as an instrument's
/// do: a new capture whose event moved from `from` to `to` moves both by as much (their difference
/// stays). Without a trigger before or after, they stay at their instants.
pub fn follow_trigger(cursors: &mut Cursors, from: Option<f64>, to: Option<f64>) {
    if let (Some(from), Some(to)) = (from, to) {
        let by = to - from;
        for t in [&mut cursors.a, &mut cursors.b].into_iter().flatten() {
            *t += by;
        }
    }
}

/// Moves the item at `from` into the gap `to` of the list as it was (0 before the first item,
/// `len` after the last): a lane dragged by its name and dropped at the insertion line.
pub fn move_to_gap<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from >= items.len() {
        return;
    }
    let item = items.remove(from);
    let to = if to > from { to - 1 } else { to }.min(items.len());
    items.insert(to, item);
}

/// A lane's D/A chip pressed: the override it leaves. Without one the lane takes the kind it is
/// not by default; with one it goes back to its default (None).
pub fn next_kind(auto: LaneKind, overridden: Option<LaneKind>) -> Option<LaneKind> {
    match (overridden, auto) {
        (Some(_), _) => None,
        (None, LaneKind::Digital) => Some(LaneKind::Analog),
        (None, LaneKind::Analog) => Some(LaneKind::Digital),
    }
}
