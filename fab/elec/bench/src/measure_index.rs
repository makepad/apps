//! Immutable capture measurements. Build once with the expectation machines; query prefixes
//! and interval trees, never scan a record. Timing intervals require both endpoints in a span.

use std::sync::Arc;
use crate::edges::Threshold;
use crate::expect::{integrals, Measure, Span};
use crate::measure::{self, Slope};
use crate::timing::{EdgeMeasures, Series, TransitionTracker};

/// One complete timing event, in source time.
#[derive(Clone, Copy, Debug)]
pub struct Event {
    /// Start time.
    pub from: f64,
    /// End time.
    pub to: f64,
    /// Period, width or transition duration in seconds (one for an edge).
    pub value: f64,
    /// High duration for a cycle, zero for other events.
    pub high: f64,
}

/// Aggregate over complete events or a numeric span.
#[derive(Clone, Copy, Debug)]
pub struct Reading {
    /// Mean value, in SI units except duty, which is in percent like Expect.
    pub value: f64,
    /// Number of intervals or samples contributing.
    pub n: usize,
    /// Population deviation in the same units.
    pub sigma: f64,
    /// Minimum value and its source time.
    pub min: (f64, f64),
    /// Maximum value and its source time.
    pub max: (f64, f64),
}

#[derive(Clone, Copy)]
struct Bounds {
    min: (f64, f64),
    max: (f64, f64),
}
impl Bounds {
    fn empty() -> Self { Self { min: (f64::INFINITY, 0.0), max: (f64::NEG_INFINITY, 0.0) } }
    fn one(v: f64, t: f64) -> Self { Self { min: (v, t), max: (v, t) } }
    fn merge(self, b: Self) -> Self {
        Self { min: if self.min.0 <= b.min.0 { self.min } else { b.min }, max: if self.max.0 >= b.max.0 { self.max } else { b.max } }
    }
}
struct Tree {
    size: usize,
    nodes: Vec<Bounds>,
}
impl Tree {
    fn new(values: impl Iterator<Item = (f64, f64)>, n: usize) -> Self {
        let size = n.max(1).next_power_of_two();
        let mut nodes = vec![Bounds::empty(); size * 2];
        for (i, (v, t)) in values.enumerate() { nodes[size + i] = Bounds::one(v, t); }
        for i in (1..size).rev() { nodes[i] = nodes[i * 2].merge(nodes[i * 2 + 1]); }
        Self { size, nodes }
    }
    fn get(&self, mut lo: usize, mut hi: usize) -> Bounds {
        lo += self.size; hi += self.size;
        let mut bounds = Bounds::empty();
        while lo < hi {
            if lo % 2 == 1 { bounds = bounds.merge(self.nodes[lo]); lo += 1; }
            if hi % 2 == 1 { hi -= 1; bounds = bounds.merge(self.nodes[hi]); }
            lo /= 2; hi /= 2;
        }
        bounds
    }
}

/// Cached event list with prefix moments and extrema; queries take O(log events).
pub struct Events {
    /// Complete events in start order.
    pub items: Vec<Event>,
    sums: Vec<[f64; 3]>,
    bounds: Tree,
    duties: Vec<[f64; 2]>,
    duty_bounds: Tree,
}
impl Events {
    fn new(items: Vec<Event>) -> Self {
        let mut sums = vec![[0.0; 3]];
        for e in &items {
            let s = *sums.last().unwrap();
            sums.push([s[0] + e.value, s[1] + e.high, s[2] + e.value * e.value]);
        }
        let bounds = Tree::new(items.iter().map(|e| (e.value, e.from)), items.len());
        let mut duties = vec![[0.0; 2]];
        for e in &items {
            let d = 100.0 * e.high / e.value;
            let s = *duties.last().unwrap(); duties.push([s[0] + d, s[1] + d * d]);
        }
        let duty_bounds = Tree::new(items.iter().map(|e| (100.0 * e.high / e.value, e.from)), items.len());
        Self { items, sums, bounds, duties, duty_bounds }
    }
    fn series(series: Series, point: bool) -> Self {
        Self::new(series.entries.into_iter().flatten().map(|(from, [value, high])| Event {
            from, to: if point { from } else { from + value }, value, high,
        }).collect())
    }
    /// Half-open event indices wholly within the given source-time span.
    pub fn range(&self, from: f64, to: f64) -> (usize, usize) {
        let lo = self.items.partition_point(|e| e.from < from);
        let hi = self.items.partition_point(|e| e.to <= to);
        (lo.min(hi), hi)
    }
    /// Aggregate a contiguous interval of events, without walking them.
    pub fn reading(&self, lo: usize, hi: usize, what: &str) -> Option<Reading> {
        if hi <= lo { return None; }
        let n = hi - lo;
        let a = self.sums[lo]; let b = self.sums[hi];
        let mean = (b[0] - a[0]) / n as f64;
        let mut sigma = ((b[2] - a[2]) / n as f64 - mean * mean).max(0.0).sqrt();
        let bounds = self.bounds.get(lo, hi);
        let (value, min, max) = match what {
            "frequency" => {
                sigma /= mean * mean;
                (1.0 / mean, (1.0 / bounds.max.0, bounds.max.1), (1.0 / bounds.min.0, bounds.min.1))
            }
            "duty" => {
                // The aggregate value is the bench's ratio of sums, not a mean of ratios.
                let value = 100.0 * (b[1] - a[1]) / (b[0] - a[0]);
                let a = self.duties[lo]; let b = self.duties[hi];
                let mean = (b[0] - a[0]) / n as f64;
                let sigma = ((b[1] - a[1]) / n as f64 - mean * mean).max(0.0).sqrt();
                let bounds = self.duty_bounds.get(lo, hi);
                return Some(Reading { value, n, sigma, min: bounds.min, max: bounds.max });
            }
            "edges" => (n as f64, (1.0, self.items[lo].from), (1.0, self.items[hi - 1].from)),
            _ => (mean, bounds.min, bounds.max),
        };
        value.is_finite().then_some(Reading { value, n, sigma, min, max })
    }
}

/// One channel's numeric and timing index. The stored samples are only read at query boundaries.
pub struct RecordIndex {
    times: Arc<[f64]>,
    values: Arc<[f32]>,
    sums: Vec<[f64; 2]>,
    bounds: Tree,
    steps: Tree,
    whole: Vec<(&'static str, f64)>,
    invalid: Vec<(f64, f64)>,
    /// Rising-to-rising cycles from the expectation timing machine.
    pub cycles: Events,
    /// Complete high pulses.
    pub widths: Events,
    /// Complete 10%-90% transitions.
    pub rises: Events,
    /// Complete 90%-10% transitions.
    pub falls: Events,
    /// Both directions' confirmed mid-swing edges.
    pub edges: Events,
    /// Whole-record mid-swing threshold and ten-percent hysteresis used by the edges.
    pub threshold: Threshold,
    /// Histogram upper mode.
    pub top: f64,
    /// Histogram lower mode.
    pub base: f64,
}
impl RecordIndex {
    /// Build once on the completing worker. Points must be finite, ordered and have no gaps.
    pub fn new(times: Arc<[f64]>, values: Arc<[f32]>) -> Self {
        Self::with_breaks(times, values, &[])
    }
    /// Build with source gaps; neither timing machines nor numeric queries bridge a gap.
    pub fn with_breaks(times: Arc<[f64]>, values: Arc<[f32]>, gaps: &[(f64, f64)]) -> Self {
        assert_eq!(times.len(), values.len(), "measurement index needs matching columns");
        assert!(!times.is_empty(), "measurement index needs at least one point");
        assert!(times.windows(2).all(|p| p[0] < p[1]), "measurement index time must increase");
        let lo = values.iter().copied().fold(f32::INFINITY, f32::min) as f64;
        let hi = values.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
        let (top, base) = measure::top_base(&times, &values).unwrap_or((hi, lo));
        let threshold = Threshold { level: (hi + lo) * 0.5, hysteresis: (hi - lo) * 0.1 };
        let mut edge = EdgeMeasures::new(threshold, true);
        let mut rises = TransitionTracker::new(base, top, 0.1, 0.9, Slope::Rising, true);
        let mut falls = TransitionTracker::new(base, top, 0.1, 0.9, Slope::Falling, true);
        let mut span = Span::default();
        let mut sums = vec![[0.0; 2]];
        let mut invalid = Vec::new();
        let mut gap = 0;
        for (i, (&t, &y)) in times.iter().zip(values.iter()).enumerate() {
            let previous = if i > 0 { times[i - 1] } else { t };
            while gap < gaps.len() && gaps[gap].1 < previous { gap += 1; }
            let broken = (gap < gaps.len() && gaps[gap].0 < t && gaps[gap].1 > previous)
                || !y.is_finite() || (i > 0 && !values[i - 1].is_finite());
            if broken {
                edge.break_stream(); rises.break_stream(); falls.break_stream();
                invalid.push((previous, t));
            }
            span.push(t, y as f64, times[0], *times.last().unwrap());
            edge.push(t, y as f64, f64::NEG_INFINITY, f64::INFINITY);
            rises.push(t, y as f64, f64::NEG_INFINITY, f64::INFINITY);
            falls.push(t, y as f64, f64::NEG_INFINITY, f64::INFINITY);
            if i > 0 {
                let (a, b) = if broken { (0.0, 0.0) } else { integrals(t - times[i - 1], values[i - 1] as f64, y as f64) };
                let s = *sums.last().unwrap(); sums.push([s[0] + a, s[1] + b]);
            }
        }
        let whole = [("mean", Measure::Mean), ("rms", Measure::Rms), ("min", Measure::Min), ("max", Measure::Max), ("pp", Measure::PeakToPeak)]
            .into_iter().filter_map(|(name, m)| span.measure(&m, *times.last().unwrap()).map(|v| (name, v))).collect();
        let mut edges = Events::series(edge.up, true).items;
        edges.extend(Events::series(edge.down, true).items);
        edges.sort_by(|a, b| a.from.total_cmp(&b.from));
        let bounds = Tree::new(values.iter().zip(times.iter()).map(|(&v, &t)| (v as f64, t)), values.len());
        let steps = Tree::new(times.iter().enumerate().map(|(i, &t)| (if i == 0 { 0.0 } else { t - times[i - 1] }, t)), times.len());
        Self { times, values, sums, bounds, steps, whole, invalid, top, base, threshold,
            cycles: Events::series(edge.cycles, false), widths: Events::series(edge.high, false),
            rises: Events::series(rises.times, false), falls: Events::series(falls.times, false), edges: Events::new(edges) }
    }
    /// Mean and largest original sample intervals touching a span, and whether they are uniform.
    pub fn steps(&self, from: f64, to: f64) -> (f64, f64, bool) {
        if self.times.len() < 2 { return (0.0, 0.0, true); }
        let lo = self.times.partition_point(|&t| t <= from).saturating_sub(1).min(self.times.len() - 2);
        let hi = self.times.partition_point(|&t| t < to).max(lo + 1).min(self.times.len() - 1);
        let mean = (self.times[hi] - self.times[lo]) / (hi - lo) as f64;
        let bounds = self.steps.get(lo + 1, hi + 1);
        let tolerance = mean.abs() * 1e-8 + from.abs().max(to.abs()) * f64::EPSILON * 4.0;
        (mean, bounds.max.0, bounds.max.0 - bounds.min.0 <= tolerance)
    }
    /// Event index for a timing measure; other measures have no event decomposition.
    pub fn events(&self, what: &str) -> Option<&Events> {
        match what { "frequency" | "duty" => Some(&self.cycles), "width" => Some(&self.widths),
            "rise" => Some(&self.rises), "fall" => Some(&self.falls), "edges" => Some(&self.edges), _ => None }
    }
    fn at(&self, t: f64) -> (f64, [f64; 2]) {
        let i = self.times.partition_point(|&at| at <= t).saturating_sub(1);
        let a = self.values[i] as f64;
        if i + 1 == self.times.len() || t == self.times[i] { return (a, self.sums[i]); }
        let b = self.values[i + 1] as f64;
        let v = a + (b - a) * ((t - self.times[i]) / (self.times[i + 1] - self.times[i]));
        let (area, square) = integrals(t - self.times[i], a, v);
        (v, [self.sums[i][0] + area, self.sums[i][1] + square])
    }
    /// Read a numeric or timing measure in O(log samples + log events).
    pub fn reading(&self, what: &str, from: f64, to: f64) -> Option<Reading> {
        if let Some(events) = self.events(what) {
            let (lo, hi) = events.range(from, to);
            if what == "edges" && lo == hi { return Some(Reading { value: 0.0, n: 0, sigma: 0.0, min: (0.0, from), max: (0.0, to) }); }
            return events.reading(lo, hi, what);
        }
        if from > to || from < self.times[0] || to > *self.times.last()? { return None; }
        let invalid = self.invalid.partition_point(|&(_, b)| b <= from);
        if self.invalid.get(invalid).is_some_and(|&(a, _)| a < to) { return None; }
        let (a, sa) = self.at(from); let (b, sb) = self.at(to);
        let lo = self.times.partition_point(|&t| t < from);
        let hi = self.times.partition_point(|&t| t <= to);
        let bounds = self.bounds.get(lo, hi).merge(Bounds::one(a, from)).merge(Bounds::one(b, to));
        let dt = to - from;
        let mut value = match what {
            "now" => b, "mean" if dt > 0.0 => (sb[0] - sa[0]) / dt,
            "rms" if dt > 0.0 => ((sb[1] - sa[1]) / dt).max(0.0).sqrt(),
            "min" => bounds.min.0, "max" => bounds.max.0, "pp" => bounds.max.0 - bounds.min.0,
            _ => return None,
        };
        if from == self.times[0] && to == *self.times.last()? {
            if let Some((_, exact)) = self.whole.iter().find(|(name, _)| *name == what) { value = *exact; }
        }
        let sigma = if dt > 0.0 {
            let mean = (sb[0] - sa[0]) / dt;
            ((sb[1] - sa[1]) / dt - mean * mean).max(0.0).sqrt()
        } else { 0.0 };
        value.is_finite().then_some(Reading { value, n: hi - lo, sigma, min: bounds.min, max: bounds.max })
    }
}
