//! E-5: expectations judged at every accepted step of a running simulation.

use std::collections::VecDeque;

use fab_elec_core::diag::Diag;
use fab_elec_core::ids::{CompRef, NetRef, PinRef};
use fab_elec_core::sel::Sel;

use crate::ValueIndex;
use crate::edges::Threshold;
use crate::measure::Slope;
use crate::timing::{DelayTracker, EdgeMeasures, TransitionTracker};
use crate::uart::{self, Frame, UartDecoder, UartFormat};

#[derive(Clone, Debug, PartialEq)]
pub enum Probe {
    V(NetRef),
    VDiff(NetRef, NetRef),
    I(PinRef),
    P(CompRef),
}

impl Probe {
    /// `V` and `VDiff` are in volts ("V"); `I` is in amperes ("A"), positive into
    /// the pin; `P` is dissipated power in watts ("W"), negative for a delivering part.
    pub fn unit(&self) -> &'static str {
        match self {
            Self::V(_) | Self::VDiff(_, _) => "V",
            Self::I(_) => "A",
            Self::P(_) => "W",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Measure {
    Now,
    Mean,
    Rms,
    Min,
    Max,
    PeakToPeak,
    /// Reciprocal mean period over complete rising-to-rising cycles, in Hz.
    Frequency(Threshold),
    /// Mean high time / mean period over complete cycles, in percent.
    Duty(Threshold),
    /// Mean complete high or low pulse width, in seconds.
    Width { threshold: Threshold, high: bool },
    /// Mean 10%-90% transition time, in seconds; hysteresis is 5% of the swing.
    Rise { base: f64, top: f64 },
    /// Mean 90%-10% transition time, in seconds.
    Fall { base: f64, top: f64 },
    EdgeCount { threshold: Threshold, slope: Slope },
    /// Each source edge to the next matching target edge (including a simultaneous edge), in seconds.
    Delay { threshold: Threshold, from: Slope, to: Probe, to_slope: Slope },
    /// Decoded text as rendered by `uart::text`, judged by a text bound.
    Uart(UartFormat),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    Range { min: f64, max: f64 },
    TextEquals(String),
    TextContains(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum When {
    /// A trailing window in simulated seconds, ignored for `Now`. Nothing is judged
    /// before `after`; a windowed check is judged at every accepted point once
    /// `t - window >= after` and a whole window has been sampled.
    Trailing { window: f64, after: f64 },
    /// Absolute simulated times. A windowed measure is judged once over `[from, to]`
    /// at the first accepted point with `t >= to`, and is final then. `Now` judges
    /// every accepted point with `from <= t <= to` and is final at the first `t >= to`.
    Span { from: f64, to: f64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expect {
    pub name: String,
    pub probe: Probe,
    pub measure: Measure,
    /// When and over what the check is judged.
    pub when: When,
    pub bound: Bound,
}

impl Expect {
    /// The measure's bench name: `now`, `mean`, `rms`, `min`, `max`, `pp`, `freq`, `duty`,
    /// `width`, `rise`, `fall`, `edges`, `delay` or `uart`.
    pub fn measure_name(&self) -> &'static str {
        match self.measure {
            Measure::Now => "now",
            Measure::Mean => "mean",
            Measure::Rms => "rms",
            Measure::Min => "min",
            Measure::Max => "max",
            Measure::PeakToPeak => "pp",
            Measure::Frequency(_) => "freq",
            Measure::Duty(_) => "duty",
            Measure::Width { .. } => "width",
            Measure::Rise { .. } => "rise",
            Measure::Fall { .. } => "fall",
            Measure::EdgeCount { .. } => "edges",
            Measure::Delay { .. } => "delay",
            Measure::Uart(_) => "uart",
        }
    }

    /// The judged value's unit: the probe's for a level, `Hz`, `%` or `s` for a timing, none
    /// for a count or text.
    pub fn unit(&self) -> &'static str {
        match self.measure {
            Measure::Now | Measure::Mean | Measure::Rms | Measure::Min | Measure::Max | Measure::PeakToPeak => self.probe.unit(),
            Measure::Frequency(_) => "Hz",
            Measure::Duty(_) => "%",
            Measure::Width { .. } | Measure::Rise { .. } | Measure::Fall { .. } | Measure::Delay { .. } => "s",
            Measure::EdgeCount { .. } | Measure::Uart(_) => "",
        }
    }

    /// The numeric bounds, or None for a text bound.
    pub fn range(&self) -> Option<(f64, f64)> {
        match self.bound {
            Bound::Range { min, max } => Some((min, max)),
            Bound::TextEquals(_) | Bound::TextContains(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pending,
    Pass,
    Fail,
}

/// A counted run's final verdict per check, read at its stop time. A check never
/// judged by then (an unresolved probe, a span past the stop, or an empty span)
/// is `NotJudged`, never `Pass`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    NotJudged,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckState {
    /// Of the latest judged value.
    pub verdict: Verdict,
    /// Final and not judged again until `reset`: a `Span` once the run reaches `to`
    /// (immediately for an invalid span or bound); valid `Trailing` checks stay live.
    pub done: bool,
    /// The latest judged value (NaN while pending) and the time it was judged at.
    pub value: f64,
    pub at: f64,
    /// The first failure (time, value), kept after the check passes again.
    /// Text equality records a mismatching frame at its stop centre, even before
    /// a span is judged; a short matching prefix fails only at judgment.
    pub failed: Option<(f64, f64)>,
    /// The largest distance outside [min, max] of any judged value (0 when never outside).
    /// Text checks use 1 for a failed judgment and 0 otherwise.
    pub worst: f64,
    /// V: [Net]; VDiff: [Net a, Net b]; I: [Pin, Comp of that pin]; P: [Comp].
    pub sel: Vec<Sel>,
}

impl CheckState {
    /// The counted run's final verdict at its stop time: any recorded failure wins,
    /// otherwise a judged check passes. A never-judged check is `NotJudged`.
    pub fn outcome(&self) -> Outcome {
        if self.failed.is_some() {
            Outcome::Fail
        } else if self.verdict != Verdict::Pending {
            Outcome::Pass
        } else {
            Outcome::NotJudged
        }
    }
}

/// One accepted solver point: net voltages, pin currents (into the pin) and component powers,
/// in the `ValueIndex` numbering.
pub struct Sample<'a> {
    pub t: f64,
    pub net_v: &'a [f64],
    pub pin_i: &'a [f64],
    pub part_p: &'a [f64],
}

/// Resolved checks and their histories. Each step costs O(checks) amortised;
/// numeric spans use constant space. UART retains text; timing windows retain
/// edges and completed intervals. Neither history is capped.
#[derive(Clone)]
pub struct Expectations {
    expects: Vec<Expect>,
    states: Vec<CheckState>,
    checks: Vec<Check>,
    previous: Option<f64>,
}

impl Expectations {
    /// Resolves every probe once through `index`. An unresolved probe gives one error and
    /// stays Pending forever; `step` skips it. Invalid spans also stay Pending and
    /// are immediately done; their error is reported only if the probe resolves.
    /// Delay targets resolve through the same index. Incompatible bounds produce
    /// an error and stay Pending, including after reset.
    pub fn new(list: Vec<Expect>, index: &ValueIndex) -> (Expectations, Vec<Diag>) {
        let mut states = Vec::with_capacity(list.len());
        let mut checks = Vec::with_capacity(list.len());
        let mut diags = Vec::new();
        for expect in &list {
            let sel = selection(&expect.probe);
            let probe = ResolvedProbe::new(&expect.probe, index);
            let target = if let Measure::Delay { to, .. } = &expect.measure { ResolvedProbe::new(to, index) } else { None };
            let invalid_target = matches!(expect.measure, Measure::Delay { .. }) && target.is_none();
            let invalid_bound = matches!(expect.measure, Measure::Uart(_)) == matches!(expect.bound, Bound::Range { .. });
            let invalid_span = match expect.when {
                When::Trailing { .. } => false,
                When::Span { from, to } => {
                    !from.is_finite() || !to.is_finite()
                        || if expect.measure == Measure::Now { from > to } else { from >= to }
                }
            };
            if probe.is_none() {
                diags.push(Diag::error(
                    format!("{}: {:?} is not in the simulated scope", expect.name, expect.probe),
                    sel.clone(),
                ));
            } else if invalid_target {
                if let Measure::Delay { to, .. } = &expect.measure {
                    diags.push(Diag::error(format!("{}: {:?} is not in the simulated scope", expect.name, to), selection(to)));
                }
            } else if invalid_bound {
                diags.push(Diag::error(format!("{}: bound does not match the measure", expect.name), sel.clone()));
            } else if invalid_span {
                if let When::Span { from, to } = expect.when {
                    diags.push(Diag::error(
                        format!("{}: span from {from} to {to} is empty or not finite", expect.name),
                        sel.clone(),
                    ));
                }
            }
            let probe = if invalid_target { None } else { probe };
            let invalid = invalid_span || invalid_bound;
            states.push(CheckState {
                verdict: Verdict::Pending,
                done: invalid,
                value: f64::NAN,
                at: f64::NAN,
                failed: None,
                worst: 0.0,
                sel,
            });
            checks.push(Check {
                probe, target, invalid, window: Window::default(), span: Span::default(),
                stream: Stream::new(expect), first: None,
            });
        }
        (Expectations { expects: list, states, checks, previous: None }, diags)
    }

    /// Feed one accepted point. Times must increase; a point at or before the previous time
    /// is ignored. Trailing checks wait for a whole window; spans finish at their end.
    pub fn step(&mut self, s: &Sample<'_>) {
        if !s.t.is_finite() || self.previous.is_some_and(|t| s.t <= t) {
            return;
        }
        self.previous = Some(s.t);
        for ((expect, state), check) in self.expects.iter().zip(&mut self.states).zip(&mut self.checks) {
            if state.done {
                continue;
            }
            state.done = matches!(expect.when, When::Span { to, .. } if s.t >= to);
            let Some(probe) = &check.probe else { continue };
            let value = probe.read(s);
            if let Some(stream) = &mut check.stream {
                check.first.get_or_insert(s.t);
                let (from, to, ready, at) = match expect.when {
                    When::Span { from, to } => (from, to, state.done, to),
                    When::Trailing { window, after } => {
                        let from = s.t - window;
                        let ready = window.is_finite() && window > 0.0 && from >= after
                            && s.t - check.first.unwrap() >= window;
                        (from, s.t, ready, s.t)
                    }
                };
                let target = check.target.as_ref().map_or(f64::NAN, |probe| probe.read(s));
                stream.push(s.t, value, target, from, to, expect, state);
                if ready {
                    if let Stream::Text(text) = stream {
                        text.judge(expect, state, at);
                    } else if let Some(value) = stream.value(&expect.measure) {
                        judge(expect, state, at, value);
                    } else {
                        state.verdict = Verdict::Pending;
                        state.value = f64::NAN;
                        state.at = f64::NAN;
                    }
                }
                continue;
            }
            let (judged, at) = match expect.when {
                When::Trailing { window, after } => {
                    let judged = if expect.measure == Measure::Now {
                        (s.t >= after).then_some(value)
                    } else {
                        check.window.push(s.t, value, window);
                        if s.t - window >= after {
                            check.window.measure(&expect.measure, window)
                        } else {
                            None
                        }
                    };
                    (judged, s.t)
                }
                When::Span { from, to } => {
                    if expect.measure == Measure::Now {
                        ((from <= s.t && s.t <= to).then_some(value), s.t)
                    } else {
                        check.span.push(s.t, value, from, to);
                        let judged = if state.done {
                            check.span.measure(&expect.measure, to)
                        } else {
                            None
                        };
                        (judged, to)
                    }
                }
            };
            if let Some(value) = judged {
                judge(expect, state, at, value);
            }
        }
    }

    /// Clear every history and state (a restart or a replaced circuit); resolution is kept.
    /// Valid spans may be judged again; invalid spans and bounds remain done.
    pub fn reset(&mut self) {
        self.previous = None;
        for ((state, check), expect) in self.states.iter_mut().zip(&mut self.checks).zip(&self.expects) {
            state.verdict = Verdict::Pending;
            state.done = check.invalid;
            state.value = f64::NAN;
            state.at = f64::NAN;
            state.failed = None;
            state.worst = 0.0;
            check.window = Window::default();
            check.span = Span::default();
            check.stream = Stream::new(expect);
            check.first = None;
        }
    }

    pub fn expects(&self) -> &[Expect] {
        &self.expects
    }

    pub fn states(&self) -> &[CheckState] {
        &self.states
    }
}

fn judge(expect: &Expect, state: &mut CheckState, at: f64, value: f64) {
    let Bound::Range { min, max } = expect.bound else { return };
    state.value = value;
    state.at = at;
    state.verdict = if value.is_finite() && min <= value && value <= max {
        Verdict::Pass
    } else {
        Verdict::Fail
    };
    if state.verdict == Verdict::Fail && state.failed.is_none() {
        state.failed = Some((at, value));
    }
    let distance = if value.is_nan() {
        f64::INFINITY
    } else {
        (min - value).max(value - max).max(0.0)
    };
    state.worst = state.worst.max(distance);
}

#[derive(Clone)]
enum ResolvedProbe {
    V(usize),
    VDiff(usize, usize),
    I(usize),
    P(usize),
}

impl ResolvedProbe {
    fn new(probe: &Probe, index: &ValueIndex) -> Option<Self> {
        Some(match *probe {
            Probe::V(net) => Self::V(index.net(net)?),
            Probe::VDiff(a, b) => Self::VDiff(index.net(a)?, index.net(b)?),
            Probe::I(pin) => Self::I(index.pin(pin)?),
            Probe::P(comp) => Self::P(index.comp(comp)?),
        })
    }

    fn read(&self, s: &Sample<'_>) -> f64 {
        let read = |values: &[f64], i: usize| values.get(i).copied().unwrap_or(f64::NAN);
        match *self {
            Self::V(i) => read(s.net_v, i),
            Self::VDiff(a, b) => read(s.net_v, a) - read(s.net_v, b),
            Self::I(i) => read(s.pin_i, i),
            Self::P(i) => read(s.part_p, i),
        }
    }
}

#[derive(Clone)]
struct Check {
    probe: Option<ResolvedProbe>,
    target: Option<ResolvedProbe>,
    invalid: bool,
    stream: Option<Stream>,
    first: Option<f64>,
    window: Window,
    span: Span,
}

#[derive(Clone, Default)]
pub(crate) struct Span {
    previous: Option<(f64, f64)>,
    start: Option<f64>,
    integral: f64,
    squared: f64,
    extrema: Option<(f64, f64)>,
    nan: usize,
    positive_infinite: usize,
    negative_infinite: usize,
}

impl Span {
    fn include(&mut self, value: f64) {
        if value.is_nan() {
            self.nan = self.nan.saturating_add(1);
            return;
        }
        if value == f64::INFINITY {
            self.positive_infinite = self.positive_infinite.saturating_add(1);
        } else if value == f64::NEG_INFINITY {
            self.negative_infinite = self.negative_infinite.saturating_add(1);
        }
        self.extrema = Some(match self.extrema {
            Some((min, max)) => (min.min(value), max.max(value)),
            None => (value, value),
        });
    }

    pub(crate) fn push(&mut self, t: f64, v: f64, from: f64, to: f64) {
        if let Some((tp, vp)) = self.previous {
            let lo = tp.max(from);
            let hi = t.min(to);
            if hi > lo {
                let (a, b) = if vp.is_finite() && v.is_finite() {
                    let interpolate = |at| {
                        if at == tp || vp == v {
                            vp
                        } else if at == t {
                            v
                        } else {
                            let fraction = (at - tp) / (t - tp);
                            vp * (1.0 - fraction) + v * fraction
                        }
                    };
                    let (a, b) = (interpolate(lo), interpolate(hi));
                    let (area, square) = integrals(hi - lo, a, b);
                    self.integral += area;
                    self.squared += square;
                    (a, b)
                } else {
                    // As for a trailing window, counts decide nonfinite results;
                    // do not contaminate the finite integrals with these segments.
                    (vp, v)
                };
                self.start.get_or_insert(lo);
                self.include(a);
                self.include(b);
            } else if t == from {
                // The left endpoint also contributes when its overlap has zero length.
                self.start.get_or_insert(t);
                self.include(v);
            }
        } else if from <= t && t <= to {
            self.start = Some(t);
            self.include(v);
        }
        self.previous = Some((t, v));
    }

    pub(crate) fn measure(&self, measure: &Measure, to: f64) -> Option<f64> {
        let length = to - self.start?;
        if length <= 0.0 {
            return None;
        }
        if self.nan != 0 {
            return Some(f64::NAN);
        }
        Some(match measure {
            Measure::Now => self.previous?.1,
            Measure::Mean => {
                if self.positive_infinite != 0 && self.negative_infinite != 0 {
                    f64::NAN
                } else if self.positive_infinite != 0 {
                    f64::INFINITY
                } else if self.negative_infinite != 0 {
                    f64::NEG_INFINITY
                } else {
                    self.integral / length
                }
            }
            Measure::Rms => {
                if self.positive_infinite != 0 || self.negative_infinite != 0 {
                    f64::INFINITY
                } else {
                    let mean_square = self.squared / length;
                    if mean_square < 0.0 { 0.0 } else { mean_square.sqrt() }
                }
            }
            Measure::Min => self.extrema?.0,
            Measure::Max => self.extrema?.1,
            Measure::PeakToPeak => {
                let (min, max) = self.extrema?;
                max - min
            }
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
struct Point {
    t: f64,
    v: f64,
    integral: f64,
    squared: f64,
}

#[derive(Clone, Default)]
struct Window {
    points: VecDeque<Point>,
    min: VecDeque<(f64, f64)>,
    max: VecDeque<(f64, f64)>,
    pops: usize,
    nan: usize,
    positive_infinite: usize,
    negative_infinite: usize,
}

impl Window {
    fn nonfinite_count(&mut self, value: f64) -> Option<&mut usize> {
        if value.is_nan() {
            Some(&mut self.nan)
        } else if value == f64::INFINITY {
            Some(&mut self.positive_infinite)
        } else if value == f64::NEG_INFINITY {
            Some(&mut self.negative_infinite)
        } else {
            None
        }
    }

    fn push(&mut self, t: f64, v: f64, width: f64) {
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        let (integral, squared) = if let Some(last) = self.points.back() {
            // Keep nonfinite samples out of the prefixes: their counts determine the result
            // while present, and finite windows can recover once they leave.
            if last.v.is_finite() && v.is_finite() {
                let (area, square) = integrals(t - last.t, last.v, v);
                (last.integral + area, last.squared + square)
            } else {
                (last.integral, last.squared)
            }
        } else {
            (0.0, 0.0)
        };
        self.points.push_back(Point { t, v, integral, squared });
        if let Some(count) = self.nonfinite_count(v) {
            *count += 1;
        }

        if !v.is_nan() {
            while self.min.back().is_some_and(|&(_, old)| old >= v) {
                self.min.pop_back();
            }
            while self.max.back().is_some_and(|&(_, old)| old <= v) {
                self.max.pop_back();
            }
            self.min.push_back((t, v));
            self.max.push_back((t, v));
        }
        while self.min.front().is_some_and(|&(time, _)| t - time > width) {
            self.min.pop_front();
        }
        while self.max.front().is_some_and(|&(time, _)| t - time > width) {
            self.max.pop_front();
        }
        // Retain the sample at or just before the left edge for interpolation.
        while self.points.get(1).is_some_and(|point| t - point.t >= width) {
            let removed = self.points.pop_front().unwrap();
            if let Some(count) = self.nonfinite_count(removed.v) {
                *count -= 1;
            }
            self.pops += 1;
        }
        if self.pops > self.points.len() {
            let base = self.points[0];
            for point in &mut self.points {
                point.integral -= base.integral;
                point.squared -= base.squared;
            }
            self.pops = 0;
        }
    }

    fn measure(&self, measure: &Measure, width: f64) -> Option<f64> {
        if !width.is_finite() || width <= 0.0 {
            return Some(f64::NAN);
        }
        let first = self.points.front()?;
        let last = self.points.back()?;
        if last.t - first.t < width {
            return None;
        }
        if self.nan != 0 {
            return Some(f64::NAN);
        }
        let next = self.points.get(1)?;
        // Compute the partial segment from ages, not the absolute timestamp t - width:
        // subtracting a millisecond from a large simulated time rounds the window's length.
        let partial = width - (last.t - next.t);
        let fraction = partial / (next.t - first.t);
        let edge = if fraction >= 1.0 || first.v == next.v {
            first.v
        } else if fraction <= 0.0 {
            next.v
        } else {
            first.v * fraction + next.v * (1.0 - fraction)
        };
        let value = match measure {
            Measure::Now => last.v,
            Measure::Mean => {
                if self.positive_infinite != 0 && self.negative_infinite != 0 {
                    f64::NAN
                } else if self.positive_infinite != 0 {
                    f64::INFINITY
                } else if self.negative_infinite != 0 {
                    f64::NEG_INFINITY
                } else {
                    let (area, _) = integrals(partial, edge, next.v);
                    (last.integral - next.integral + area) / width
                }
            }
            Measure::Rms => {
                if self.positive_infinite != 0 || self.negative_infinite != 0 {
                    f64::INFINITY
                } else {
                    let (_, square) = integrals(partial, edge, next.v);
                    let mean_square = (last.squared - next.squared + square) / width;
                    // Roundoff can make the integral of a zero signal slightly negative.
                    if mean_square < 0.0 { 0.0 } else { mean_square.sqrt() }
                }
            }
            Measure::Min => edge.min(self.min.front()?.1),
            Measure::Max => edge.max(self.max.front()?.1),
            Measure::PeakToPeak => edge.max(self.max.front()?.1) - edge.min(self.min.front()?.1),
            _ => return None,
        };
        Some(value)
    }
}

pub(crate) fn integrals(dt: f64, a: f64, b: f64) -> (f64, f64) {
    (dt * (a * 0.5 + b * 0.5), dt * (a * a + a * b + b * b) / 3.0)
}

fn selection(probe: &Probe) -> Vec<Sel> {
    match probe {
        Probe::V(net) => vec![Sel::Net(*net)],
        Probe::VDiff(a, b) => vec![Sel::Net(*a), Sel::Net(*b)],
        Probe::I(pin) => vec![Sel::Pin(*pin), Sel::Comp(pin.comp_ref())],
        Probe::P(comp) => vec![Sel::Comp(*comp)],
    }
}

#[derive(Clone)]
enum Stream {
    Edges(EdgeMeasures),
    Transition(TransitionTracker),
    Delay(DelayTracker),
    Text(Text),
}

impl Stream {
    fn new(expect: &Expect) -> Option<Self> {
        let windowed = matches!(expect.when, When::Trailing { .. });
        Some(match &expect.measure {
            Measure::Frequency(threshold) | Measure::Duty(threshold)
            | Measure::Width { threshold, .. } | Measure::EdgeCount { threshold, .. } => {
                Self::Edges(EdgeMeasures::new(*threshold, windowed))
            }
            Measure::Rise { base, top } | Measure::Fall { base, top } => {
                let slope = if matches!(expect.measure, Measure::Rise { .. }) { Slope::Rising } else { Slope::Falling };
                Self::Transition(TransitionTracker::new(*base, *top, 0.1, 0.9, slope, windowed))
            }
            Measure::Delay { threshold, from, to_slope, .. } => {
                Self::Delay(DelayTracker::new(*threshold, *from, *to_slope, windowed))
            }
            Measure::Uart(format) => Self::Text(Text::new(*format, &expect.bound)),
            _ => return None,
        })
    }

    fn push(&mut self, t: f64, y: f64, target: f64, from: f64, to: f64, expect: &Expect, state: &mut CheckState) {
        match self {
            Self::Edges(edges) => edges.push(t, y, from, to),
            Self::Transition(transition) => transition.push(t, y, from, to),
            Self::Delay(delay) => delay.push(t, y, target, from, to),
            Self::Text(text) => text.push(t, y, from, to, expect, state),
        }
    }

    fn value(&self, measure: &Measure) -> Option<f64> {
        match self {
            Self::Edges(edges) => match measure {
                Measure::Frequency(_) => edges.timing().frequency,
                Measure::Duty(_) => edges.timing().duty,
                Measure::Width { high, .. } => if *high { edges.timing().width_high } else { edges.timing().width_low },
                Measure::EdgeCount { slope, .. } => Some(edges.count(*slope)),
                _ => None,
            }
            Self::Transition(transition) => transition.times.mean(),
            Self::Delay(delay) => delay.times.mean(),
            Self::Text(_) => None,
        }
    }
}

/// KMP keeps substring/equality judgments O(1) per sample, amortised over the
/// decoded bytes. Absolute byte offsets also allow matches to expire with frames.
#[derive(Clone)]
struct Text {
    decoder: UartDecoder,
    frames: VecDeque<(Frame, String)>,
    pattern: Vec<u8>,
    fallback: Vec<usize>,
    matched: usize,
    bytes: usize,
    removed: usize,
    last_match: Option<usize>,
}

impl Text {
    fn new(format: UartFormat, bound: &Bound) -> Self {
        let pattern = match bound {
            Bound::TextEquals(text) | Bound::TextContains(text) => text.as_bytes().to_vec(),
            Bound::Range { .. } => Vec::new(),
        };
        let mut fallback = vec![0; pattern.len()];
        for i in 1..pattern.len() {
            let mut j = fallback[i - 1];
            while j > 0 && pattern[i] != pattern[j] { j = fallback[j - 1]; }
            if pattern[i] == pattern[j] { j += 1; }
            fallback[i] = j;
        }
        Self { decoder: UartDecoder::new(format), frames: VecDeque::new(), pattern, fallback,
            matched: 0, bytes: 0, removed: 0, last_match: None }
    }

    fn push(&mut self, t: f64, y: f64, from: f64, to: f64, expect: &Expect, state: &mut CheckState) {
        if let Some(frame) = self.decoder.push(t, y).filter(|frame| frame.t0 >= from && frame.t1 <= to) {
            let mut text = String::new();
            uart::append_text(&mut text, frame);
            if matches!(expect.when, When::Span { .. }) && matches!(expect.bound, Bound::TextEquals(_))
                && state.failed.is_none() && self.pattern.get(self.bytes..self.bytes + text.len()) != Some(text.as_bytes()) {
                state.failed = Some((frame.t1, (self.frames.len() + 1) as f64));
            }
            for byte in text.bytes() {
                if !self.pattern.is_empty() {
                    while self.matched > 0 && (self.matched == self.pattern.len() || self.pattern[self.matched] != byte) {
                        self.matched = self.fallback[self.matched - 1];
                    }
                    if self.pattern[self.matched] == byte { self.matched += 1; }
                    if self.matched == self.pattern.len() { self.last_match = Some(self.bytes + 1); }
                }
                self.bytes += 1;
            }
            self.frames.push_back((frame, text));
        }
        if matches!(expect.when, When::Trailing { .. }) {
            while self.frames.front().is_some_and(|(frame, _)| frame.t0 < from) {
                self.removed += self.frames.pop_front().unwrap().1.len();
            }
        }
    }

    fn judge(&self, expect: &Expect, state: &mut CheckState, at: f64) {
        if self.frames.is_empty() {
            state.verdict = Verdict::Pending;
            state.value = f64::NAN;
            state.at = f64::NAN;
            return;
        }
        let len = self.bytes - self.removed;
        let pass = match &expect.bound {
            Bound::TextEquals(_) => len == self.pattern.len() && self.last_match == Some(self.bytes),
            Bound::TextContains(_) => self.pattern.is_empty()
                || self.last_match.is_some_and(|end| end - self.pattern.len() >= self.removed),
            Bound::Range { .. } => return,
        };
        state.value = self.frames.len() as f64;
        state.at = at;
        state.verdict = if pass { Verdict::Pass } else { Verdict::Fail };
        if !pass {
            state.worst = 1.0;
            if state.failed.is_none() {
                let mut failure = (at, state.value);
                if matches!(expect.bound, Bound::TextEquals(_)) {
                    // Scan at most once per run: attribute the first wrong byte to
                    // its frame, including when it preceded the first full window.
                    let mut offset = 0;
                    for (i, (frame, text)) in self.frames.iter().enumerate() {
                        if self.pattern.get(offset..offset + text.len()) != Some(text.as_bytes()) {
                            failure = (frame.t1, (i + 1) as f64);
                            break;
                        }
                        offset += text.len();
                    }
                }
                state.failed = Some(failure);
            }
        }
    }
}
