//! Pinned answer quantities and their immutable, event-indexed extrema pyramid.

use super::events::{ChunkList, ChunkLog, ChunkValue};
use super::distribution::Distribution;
use fab_elec_bench::{edges::{EdgeTracker, Threshold}, trigger::Slope};
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrendQuantity { Freq, Duty, Width, Vpp, Mean }

impl TrendQuantity {
    pub fn from_name(name: &str) -> Option<Self> {
        match name { "freq" => Some(Self::Freq), "duty" => Some(Self::Duty), "width" => Some(Self::Width), "Vpp" => Some(Self::Vpp), "mean" => Some(Self::Mean), _ => None }
    }
    pub fn name(self) -> &'static str {
        match self { Self::Freq => "freq", Self::Duty => "duty", Self::Width => "width", Self::Vpp => "Vpp", Self::Mean => "mean" }
    }
}

/// Bench identity survives display reordering; only the feed uses lane indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrendPin { pub net: String, pub quantity: TrendQuantity }

#[derive(Clone, Debug, PartialEq)]
pub struct TrendBound { pub pin: (usize, TrendQuantity), pub min: Option<f64>, pub max: Option<f64> }

/// Bound indices refer to the bounds installed when the event was judged.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Violation { pub event: usize, pub bound: usize, pub value: f64 }

impl ChunkValue for Violation {
    type Summary = ();
    fn summarize(_: &[Self]) {}
    fn shift(_: (), _: usize) {}
    fn merge(_: (), _: ()) {}
}

/// Event-ordered failures; clones share sealed chunks and the published tail.
#[derive(Clone, Debug, Default)]
pub struct ViolationList(ChunkList<Violation>);

impl ViolationList {
    pub fn len(&self) -> usize { self.0.len() }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
    pub fn same(&self, other: &Self) -> bool { self.0.same(&other.0) }
    pub fn get(&self, index: usize) -> Option<Violation> { self.0.get(index) }
    pub fn partition_point(&self, before: impl FnMut(Violation) -> bool) -> usize { self.0.partition_point(before) }
}

impl ChunkValue for f32 {
    type Summary = Option<(f32, usize, f32, usize)>;
    fn summarize(values: &[Self]) -> Self::Summary {
        values.iter().enumerate().filter(|(_, value)| value.is_finite())
            .fold(None, |summary, (i, &value)| Self::merge(summary, Some((value, i, value, i))))
    }
    fn shift(summary: Self::Summary, offset: usize) -> Self::Summary {
        summary.map(|(min, argmin, max, argmax)| (min, argmin + offset, max, argmax + offset))
    }
    fn merge(left: Self::Summary, right: Self::Summary) -> Self::Summary {
        match (left, right) {
            (Some((a, ai, b, bi)), Some((c, ci, d, di))) => {
                let (min, argmin) = if c < a { (c, ci) } else { (a, ai) };
                let (max, argmax) = if d > b { (d, di) } else { (b, bi) };
                Some((min, argmin, max, argmax))
            }
            (left, right) => left.or(right),
        }
    }
}

/// One f32 per completed event. An unobserved prefix (pinned partway through a run) is NaN,
/// represented by its length alone. Clones share the tree and the last published open tail.
#[derive(Clone, Debug, Default)]
pub struct TrendColumn { start: usize, values: ChunkList<f32> }

impl TrendColumn {
    pub fn len(&self) -> usize { self.start + self.values.len() }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
    pub fn get(&self, index: usize) -> Option<f32> {
        if index < self.start { Some(f32::NAN) } else { self.values.get(index - self.start) }
    }
    pub fn same(&self, other: &Self) -> bool { self.start == other.start && self.values.same(&other.values) }
    /// Finite extrema of [i0, i1), with event indices; ties select the earliest event.
    /// O(log N), including at most three bounded partial-chunk scans.
    pub fn extremes(&self, i0: usize, i1: usize) -> Option<(f32, usize, f32, usize)> {
        f32::shift(self.values.summary(i0.saturating_sub(self.start), i1.saturating_sub(self.start)), self.start)
    }
}

#[derive(Default)]
pub(super) struct TrendLog { start: usize, values: ChunkLog<f32>, distribution: Distribution, range: Option<(f32, f32)> }

impl TrendLog {
    pub fn new(start: usize) -> Self { Self { start, ..Self::default() } }
    pub fn push(&mut self, index: usize, value: f32) {
        if index < self.start { return; }
        debug_assert_eq!(index, self.start + self.values.len(), "trend values follow accepted event indices");
        self.values.push(value);
        self.distribution.push(value);
        if value.is_finite() { self.range = Some(self.range.map_or((value, value), |(lo, hi)| (lo.min(value), hi.max(value)))); }
    }
    pub fn snapshot(&mut self) -> TrendColumn { TrendColumn { start: self.start, values: self.values.snapshot() } }
}

/// The nominal intake's column identity travels with its values through publication.
/// The histogram counts every measured event; range retains exact finite extrema for labels.
#[derive(Clone, Debug)]
pub struct TrendSeries { pub lane: usize, pub quantity: TrendQuantity, pub values: TrendColumn, pub distribution: Distribution, pub range: Option<(f32, f32)> }

/// No samples are retained: each span carries extrema, an integral and three crossings.
struct Span {
    index: usize,
    start: f64,
    end: f64,
    min: f64,
    max: f64,
    area: f64,
    covered: f64,
    edges: Option<EdgeTracker>,
    rise: Option<f64>,
    fall: Option<f64>,
    next: Option<f64>,
}

impl Span {
    fn new(start: f64, post: f64, threshold: Option<Threshold>, previous: Option<(f64, f64)>) -> Self {
        let mut edges = threshold.map(EdgeTracker::new);
        if let (Some(edges), Some((t, y))) = (&mut edges, previous) { edges.push(t, y); }
        Self { index: 0, start, end: start + post, min: f64::INFINITY, max: f64::NEG_INFINITY, area: 0.0, covered: 0.0, edges, rise: None, fall: None, next: None }
    }

    fn point(&mut self, previous: Option<(f64, f64)>, t: f64, y: f64) {
        let Some((tp, yp)) = previous else { return };
        if tp >= self.end || t < self.start { return; }
        let from = tp.max(self.start);
        let to = t.min(self.end);
        let value = |at| yp + (y - yp) * ((at - tp) / (t - tp));
        if yp.is_finite() && y.is_finite() && from <= to {
            let (a, b) = (value(from), value(to));
            self.min = self.min.min(a).min(b);
            self.max = self.max.max(a).max(b);
            self.area += (a + b) * 0.5 * (to - from);
            self.covered += to - from;
        }
        if let Some(edges) = &mut self.edges {
            if !yp.is_finite() || !y.is_finite() { edges.reset(); return; }
            if let Some(edge) = edges.push(to, value(to)).filter(|edge| edge.t >= self.start && edge.t <= self.end) {
                match edge.dir {
                    Slope::Rising if self.rise.is_none() => self.rise = Some(edge.t),
                    Slope::Rising if self.next.is_none() => self.next = Some(edge.t),
                    Slope::Falling if self.rise.is_some() && self.fall.is_none() => self.fall = Some(edge.t),
                    _ => {}
                }
            }
        }
    }

    fn range(&self) -> Option<(f64, f64)> { (self.min.is_finite() && self.max.is_finite()).then_some((self.min, self.max)) }

    fn value(&self, quantity: TrendQuantity) -> f64 {
        let width = self.rise.zip(self.fall).map(|(a, b)| b - a).filter(|&dt| dt > 0.0);
        let period = self.rise.zip(self.next).map(|(a, b)| b - a).filter(|&dt| dt > 0.0);
        let value = match quantity {
            TrendQuantity::Width => width,
            TrendQuantity::Freq => period.map(|dt| 1.0 / dt),
            TrendQuantity::Duty => width.zip(period).map(|(w, p)| 100.0 * w / p),
            TrendQuantity::Vpp => self.range().map(|(min, max)| max - min),
            TrendQuantity::Mean => (self.covered > 0.0 && (self.covered - (self.end - self.start)).abs() <= (self.end - self.start) * 1e-9).then(|| self.area / self.covered),
        };
        value.unwrap_or(f64::NAN)
    }
}

struct Track {
    lane: usize,
    columns: Vec<(TrendQuantity, TrendLog)>,
    previous: Option<(f64, f64)>,
    completed: Option<(f64, f64)>,
    open: VecDeque<Span>,
    pending: [Option<Span>; 2],
}

impl Track {
    fn open(&mut self, at: f64, post: f64, trigger: Option<f64>, t: f64, y: f64) -> Span {
        // The trigger lane has a known level even on its first span; before a completed
        // range exists its band is zero. Other lanes have no timing threshold yet.
        let threshold = trigger.or_else(|| self.completed.map(|(min, max)| (min + max) * 0.5))
            .map(|level| Threshold { level, hysteresis: self.completed.map_or(0.0, |(min, max)| (max - min) * 0.1) });
        let mut span = Span::new(at, post, threshold, self.previous);
        span.point(self.previous, t, y);
        span
    }

    fn accept(&mut self, t: f64, y: f64, index: usize, at: f64, post: f64, trigger: Option<f64>) {
        let side = self.pending.iter().position(|span| span.as_ref().is_some_and(|span| span.start == at));
        let mut span = side.and_then(|side| self.pending[side].take()).unwrap_or_else(|| self.open(at, post, trigger, t, y));
        span.index = index;
        self.open.push_back(span);
    }
}

/// One accumulator per open span per recorded lane, shared by its quantities and bounds.
#[derive(Default)]
pub(super) struct TrendRecorder {
    pins: Vec<(usize, TrendQuantity)>, tracks: Vec<Track>, locations: Vec<(usize, usize)>,
    bounds: Vec<TrendBound>, bound_locations: Vec<usize>, bound_starts: Vec<usize>,
    violations: ChunkLog<Violation>, stop_bounds: Vec<bool>, stop_request: u64, stop_start: usize,
    stopped: Option<(u64, Violation)>,
    epoch: u64,
    published: Arc<Vec<TrendSeries>>,
}

impl TrendRecorder {
    pub fn set(&mut self, pins: &[(usize, TrendQuantity)], bounds: &[TrendBound], start: usize) {
        self.published = Arc::default();
        if self.bounds != bounds {
            self.bound_starts = bounds.iter().map(|bound| self.bounds.iter().position(|old| old == bound).map_or(start, |i| self.bound_starts[i])).collect();
            self.bounds = bounds.to_vec();
        }
        self.pins.clear();
        for pin in pins.iter().copied().chain(bounds.iter().map(|bound| bound.pin)) { if !self.pins.contains(&pin) { self.pins.push(pin); } }
        self.tracks.retain(|track| self.pins.iter().any(|&(lane, _)| lane == track.lane));
        for track in &mut self.tracks { track.columns.retain(|(quantity, _)| self.pins.contains(&(track.lane, *quantity))); }
        self.locations.clear();
        for &(lane, quantity) in &self.pins {
            let index = self.tracks.iter().position(|track| track.lane == lane).unwrap_or_else(|| {
                self.tracks.push(Track { lane, columns: Vec::new(), previous: None, completed: None, open: VecDeque::new(), pending: [None, None] });
                self.tracks.len() - 1
            });
            let columns = &mut self.tracks[index].columns;
            let column = columns.iter().position(|&(q, _)| q == quantity).unwrap_or_else(|| {
                columns.push((quantity, TrendLog::new(start)));
                columns.len() - 1
            });
            self.locations.push((index, column));
        }
        self.bound_locations = bounds.iter().map(|bound| self.locations[self.pins.iter().position(|pin| *pin == bound.pin).unwrap()].0).collect();
        self.locations = pins.iter().map(|pin| self.locations[self.pins.iter().position(|kept| kept == pin).unwrap()]).collect();
    }
    pub fn arm(&mut self, request: u64, bounds: &[usize], start: usize) {
        if self.stop_request == request { return; }
        self.stop_request = request; self.stop_start = start;
        self.stop_bounds = vec![false; self.bounds.len()];
        for &bound in bounds { if let Some(on) = self.stop_bounds.get_mut(bound) { *on = true; } }
        self.stopped = None;
    }
    pub fn restart(&mut self, continuous: bool) {
        self.epoch += 1;
        self.violations.restart(); self.bound_starts.fill(0); self.stop_start = 0; self.stopped = None;
        self.published = Arc::default();
        for track in &mut self.tracks {
            for (_, log) in &mut track.columns { *log = TrendLog::default(); }
            if !continuous { track.previous = None; }
            track.completed = None;
            track.open.clear(); track.pending = [None, None];
        }
    }
    pub fn is_empty(&self) -> bool { self.tracks.is_empty() }
    pub fn push(&mut self, t: f64, y: &[f32], accepted: Option<f64>, index: usize, pending: [Option<f64>; 2], post: f64, trigger_channel: usize, level: f64) {
        for track in &mut self.tracks {
            for span in track.open.iter_mut().chain(track.pending.iter_mut().flatten()) { span.point(track.previous, t, f64::from(y[track.lane])); }
        }
        if let Some(at) = accepted {
            self.finish(at);
            for track in &mut self.tracks { track.accept(t, f64::from(y[track.lane]), index, at, post, (track.lane == trigger_channel).then_some(level)); }
        }
        for (side, at) in pending.into_iter().enumerate() {
            if let Some(at) = at { self.finish(at); }
            for track in &mut self.tracks {
                if track.pending[side].as_ref().map(|span| span.start) != at {
                    track.pending[side] = at.map(|at| track.open(at, post, (track.lane == trigger_channel).then_some(level), t, f64::from(y[track.lane])));
                }
            }
        }
        self.finish(t);
        for track in &mut self.tracks { track.previous = Some((t, f64::from(y[track.lane]))); }
    }
    fn finish(&mut self, at: f64) {
        while let Some(index) = self.tracks.iter().filter_map(|track| track.open.front().filter(|span| span.end <= at).map(|span| span.index)).min() {
            // Judge before popping, in event/bound order even when one segment finishes many spans.
            for (bound_index, (bound, &lane)) in self.bounds.iter().zip(&self.bound_locations).enumerate() {
                if index < self.bound_starts[bound_index] { continue; }
                let Some(span) = self.tracks[lane].open.front().filter(|span| span.index == index && span.end <= at) else { continue };
                let value = span.value(bound.pin.1);
                if value.is_finite() && bound.min.is_none_or(f64::is_finite) && bound.max.is_none_or(f64::is_finite)
                    && (bound.min.is_some_and(|min| value < min) || bound.max.is_some_and(|max| value > max)) {
                    let violation = Violation { event: index, bound: bound_index, value };
                    self.violations.push(violation);
                    if self.stopped.is_none() && index >= self.stop_start && self.stop_bounds.get(bound_index) == Some(&true) { self.stopped = Some((self.stop_request, violation)); }
                }
            }
            for track in &mut self.tracks {
                if track.open.front().is_some_and(|span| span.index == index && span.end <= at) {
                    let span = track.open.pop_front().unwrap();
                    track.completed = span.range();
                    for (quantity, log) in &mut track.columns { log.push(span.index, span.value(*quantity) as f32); }
                }
            }
        }
    }
    pub fn violations(&mut self) -> ViolationList { ViolationList(self.violations.snapshot()) }
    pub fn stopped(&self) -> Option<(u64, Violation)> { self.stopped }
    pub fn epoch(&self) -> u64 { self.epoch }
    pub fn snapshot(&mut self) -> Arc<Vec<TrendSeries>> {
        // An accepted trigger may publish before its span completes. Keep the previous
        // columns and histograms shared until a value is appended or the pins are reset.
        if self.published.len() == self.locations.len() && self.locations.iter().zip(self.published.iter()).all(|(&(lane, column), series)| {
            let log = &self.tracks[lane].columns[column].1;
            (log.distribution.total, log.distribution.nonfinite) == (series.distribution.total, series.distribution.nonfinite)
        }) { return self.published.clone(); }
        let tracks = &mut self.tracks;
        self.published = Arc::new(self.locations.iter().map(|&(lane, column)| {
            let track = &mut tracks[lane];
            let (quantity, log) = &mut track.columns[column];
            TrendSeries { lane: track.lane, quantity: *quantity, values: log.snapshot(), distribution: log.distribution.clone(), range: log.range }
        }).collect());
        self.published.clone()
    }
}
