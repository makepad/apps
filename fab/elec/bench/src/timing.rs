//! Shared streaming timing accumulators. Only trailing checks retain completed events.

use std::collections::VecDeque;
use crate::edges::{Edge, EdgeTracker, Threshold};
use crate::measure::{Pulse, Slope, Timing};

/// A completed interval contributes only while both endpoints are in the range.
/// Two sums let a cycle carry its period and high time in the same operation order
/// for offline and streaming callers.
#[derive(Clone)]
pub(crate) struct Series {
    pub(crate) entries: Option<VecDeque<(f64, [f64; 2])>>,
    pub count: usize,
    pub sum: [f64; 2],
    square: f64,
}

impl Series {
    pub fn new(windowed: bool) -> Self {
        Self { entries: windowed.then(VecDeque::new), count: 0, sum: [0.0; 2], square: 0.0 }
    }

    pub fn add(&mut self, start: f64, values: [f64; 2], from: f64) {
        if start < from { return; }
        self.count += 1;
        self.sum[0] += values[0];
        self.sum[1] += values[1];
        self.square += values[0] * values[0];
        if let Some(entries) = &mut self.entries { entries.push_back((start, values)); }
    }

    pub fn expire(&mut self, from: f64) {
        if let Some(entries) = &mut self.entries {
            while entries.front().is_some_and(|&(start, _)| start < from) {
                let (_, values) = entries.pop_front().unwrap();
                self.count -= 1;
                self.sum[0] -= values[0];
                self.sum[1] -= values[1];
                self.square -= values[0] * values[0];
            }
            if self.count == 0 { self.sum = [0.0; 2]; self.square = 0.0; }
        }
    }

    pub fn mean(&self) -> Option<f64> {
        (self.count != 0).then(|| self.sum[0] / self.count as f64)
    }

    fn sigma(&self) -> Option<f64> {
        let mean = self.mean()?;
        Some((self.square / self.count as f64 - mean * mean).max(0.0).sqrt())
    }
}

#[derive(Clone, Default)]
pub(crate) struct PulseTracker {
    previous: Option<Edge>,
    period: Option<f64>,
    rising: Option<f64>,
    falling: Option<f64>,
}

impl PulseTracker {
    pub fn push(&mut self, edge: Edge) -> Option<Pulse> {
        let pulse = self.previous.map(|start| Pulse {
            t: start.t, width: edge.t - start.t, high: start.dir == Slope::Rising, period: self.period,
        });
        let same = if edge.dir == Slope::Rising { &mut self.rising } else { &mut self.falling };
        self.period = same.map(|start| edge.t - start);
        *same = Some(edge.t);
        self.previous = Some(edge);
        pulse
    }
}

#[derive(Clone)]
pub(crate) struct EdgeMeasures {
    line: EdgeTracker,
    pulses: PulseTracker,
    rising: Option<f64>,
    falling: Option<f64>,
    pub(crate) cycles: Series,
    pub(crate) high: Series,
    pub(crate) low: Series,
    pub(crate) up: Series,
    pub(crate) down: Series,
}

impl EdgeMeasures {
    pub fn new(threshold: Threshold, windowed: bool) -> Self {
        Self {
            line: EdgeTracker::new(threshold), pulses: PulseTracker::default(), rising: None, falling: None,
            cycles: Series::new(windowed), high: Series::new(windowed), low: Series::new(windowed),
            up: Series::new(windowed), down: Series::new(windowed),
        }
    }

    pub fn push(&mut self, t: f64, y: f64, from: f64, to: f64) {
        if let Some(edge) = self.line.push(t, y).filter(|edge| edge.t >= from && edge.t <= to) {
            if let Some(pulse) = self.pulses.push(edge) {
                let series = if pulse.high { &mut self.high } else { &mut self.low };
                series.add(pulse.t, [pulse.width, 0.0], from);
            }
            if edge.dir == Slope::Rising {
                self.up.add(edge.t, [1.0, 0.0], from);
                if let (Some(start), Some(end)) = (self.rising, self.falling) {
                    self.cycles.add(start, [edge.t - start, end - start], from);
                }
                self.rising = Some(edge.t);
                self.falling = None;
            } else {
                self.down.add(edge.t, [1.0, 0.0], from);
                self.falling = Some(edge.t);
            }
        }
        for series in [&mut self.cycles, &mut self.high, &mut self.low, &mut self.up, &mut self.down] {
            series.expire(from);
        }
    }

    pub(crate) fn break_stream(&mut self) {
        self.line.reset();
        self.pulses = PulseTracker::default();
        self.rising = None;
        self.falling = None;
    }

    pub fn timing(&self) -> Timing {
        Timing {
            frequency: self.cycles.mean().filter(|&p| p > 0.0).map(|p| 1.0 / p),
            duty: (self.cycles.count != 0).then(|| 100.0 * self.cycles.sum[1] / self.cycles.sum[0]),
            width_high: self.high.mean(), width_low: self.low.mean(),
            n_cycles: self.cycles.count, sigma_period: self.cycles.sigma(),
        }
    }

    pub fn count(&self, slope: Slope) -> f64 {
        (match slope {
            Slope::Rising => self.up.count,
            Slope::Falling => self.down.count,
            Slope::Either => self.up.count + self.down.count,
        }) as f64
    }
}

#[derive(Clone)]
pub(crate) struct TransitionTracker {
    low: EdgeTracker,
    high: EdgeTracker,
    slope: Slope,
    start: Option<f64>,
    pub times: Series,
}

impl TransitionTracker {
    pub fn new(base: f64, top: f64, lo: f64, hi: f64, slope: Slope, windowed: bool) -> Self {
        let swing = top - base;
        Self {
            low: EdgeTracker::new(Threshold { level: base + lo * swing, hysteresis: 0.05 * swing }),
            high: EdgeTracker::new(Threshold { level: base + hi * swing, hysteresis: 0.05 * swing }),
            slope, start: None, times: Series::new(windowed),
        }
    }

    pub(crate) fn break_stream(&mut self) {
        self.low.reset(); self.high.reset(); self.start = None;
    }

    pub fn push(&mut self, t: f64, y: f64, from: f64, to: f64) {
        let mut edges = [(self.low.push(t, y), false), (self.high.push(t, y), true)];
        if edges[0].0.zip(edges[1].0).is_some_and(|(a, b)| a.t > b.t) { edges.swap(0, 1); }
        for (edge, upper) in edges {
            let Some(edge) = edge.filter(|edge| edge.t >= from && edge.t <= to) else { continue };
            if edge.dir != self.slope {
                self.start = None;
            } else if upper == (self.slope == Slope::Falling) {
                self.start = Some(edge.t);
            } else if let Some(start) = self.start.take() {
                self.times.add(start, [edge.t - start, 0.0], from);
            }
        }
        self.times.expire(from);
    }
}

/// Pending source edges are compressed to a count and sum in spans. Trailing
/// windows retain their starts so incomplete pairs can expire too.
#[derive(Clone)]
struct Sources {
    entries: Option<VecDeque<f64>>,
    count: usize,
    sum: f64,
    origin: f64,
}

impl Sources {
    fn new(windowed: bool) -> Self {
        Self { entries: windowed.then(VecDeque::new), count: 0, sum: 0.0, origin: 0.0 }
    }

    fn add(&mut self, start: f64) {
        if self.count == 0 { self.origin = start; }
        self.count += 1;
        self.sum += start - self.origin;
        if let Some(entries) = &mut self.entries { entries.push_back(start); }
    }

    fn expire(&mut self, from: f64) {
        if let Some(entries) = &mut self.entries {
            while entries.front().is_some_and(|&t| t < from) {
                self.sum -= entries.pop_front().unwrap() - self.origin;
                self.count -= 1;
            }
            if self.count == 0 { self.sum = 0.0; }
        }
    }

    fn append(&mut self, other: &mut Self) {
        if self.count == 0 { self.origin = other.origin; }
        self.sum += other.sum + other.count as f64 * (other.origin - self.origin);
        self.count += other.count;
        if let (Some(a), Some(b)) = (&mut self.entries, &mut other.entries) { a.append(b); }
        other.count = 0;
        other.sum = 0.0;
    }

    fn finish(&mut self, end: f64, times: &mut Series, from: f64) {
        if let Some(entries) = &mut self.entries {
            while let Some(start) = entries.pop_front() { times.add(start, [end - start, 0.0], from); }
        } else {
            times.count += self.count;
            times.sum[0] += self.count as f64 * (end - self.origin) - self.sum;
        }
        self.count = 0;
        self.sum = 0.0;
    }
}

#[derive(Clone)]
pub(crate) struct DelayTracker {
    source: EdgeTracker,
    target: EdgeTracker,
    from_slope: Slope,
    to_slope: Slope,
    waiting: Sources,
    after_target: Sources,
    target_candidate: Option<f64>,
    source_candidate: Option<f64>,
    first_target: Option<f64>,
    pub times: Series,
}

impl DelayTracker {
    pub fn new(threshold: Threshold, from: Slope, to: Slope, windowed: bool) -> Self {
        Self { source: EdgeTracker::new(threshold), target: EdgeTracker::new(threshold),
            from_slope: from, to_slope: to, waiting: Sources::new(windowed), after_target: Sources::new(windowed),
            target_candidate: None, source_candidate: None, first_target: None, times: Series::new(windowed) }
    }

    pub fn push(&mut self, t: f64, source: f64, target: f64, from: f64, to: f64) {
        self.waiting.expire(from);
        self.after_target.expire(from);
        self.times.expire(from);
        if !source.is_finite() || !target.is_finite() { return; }
        let matches = |edge: &Edge, slope| edge.t >= from && edge.t <= to && (slope == Slope::Either || edge.dir == slope);
        let a = self.source.push(t, source).filter(|edge| matches(edge, self.from_slope));
        let b = self.target.push(t, target).filter(|edge| matches(edge, self.to_slope));
        let target_candidate = self.target.pending().filter(|edge| matches(edge, self.to_slope)).map(|edge| edge.t);
        if b.is_none() && self.target_candidate != target_candidate {
            // Cancellation or a later re-crossing moves all previously deferred
            // sources before the new candidate. No edge history is needed in a span.
            self.waiting.append(&mut self.after_target);
        }
        let dividing = b.map(|edge| edge.t).or(target_candidate);
        if let Some(edge) = a {
            if self.source_candidate == Some(edge.t) && self.first_target.is_some() {
                self.times.add(edge.t, [self.first_target.unwrap() - edge.t, 0.0], from);
            } else if dividing.is_some_and(|at| edge.t > at) {
                self.after_target.add(edge.t);
            } else {
                self.waiting.add(edge.t);
            }
        }
        if let Some(edge) = b {
            self.waiting.finish(edge.t, &mut self.times, from);
            self.waiting.append(&mut self.after_target);
        }
        let source_candidate = self.source.pending().filter(|edge| matches(edge, self.from_slope)).map(|edge| edge.t);
        if source_candidate != self.source_candidate {
            self.first_target = None;
        }
        if let (Some(start), Some(edge)) = (source_candidate, b) {
            if edge.t >= start { self.first_target.get_or_insert(edge.t); }
        }
        self.source_candidate = source_candidate;
        self.target_candidate = target_candidate;
    }
}
