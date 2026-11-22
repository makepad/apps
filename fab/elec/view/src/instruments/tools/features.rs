//! Paged edges and complete pulses. Auto queries read only capture-time feature caches.

use fab_elec_bench::edges::{EdgeTracker, Threshold};
use fab_elec_bench::measure::Slope;
use fab_elec_bench::measure_index::RecordIndex;
use fab_elec_bench::source::Quantity;
use fab_elec_circuit::QtyUnit;
use super::call::{number, Arguments};
use super::capture::Capture;
use super::query::{bounds, channel, key, unit};
use super::reply::{self, list, Object, ToolReply};

#[derive(Clone, Copy)]
struct EdgeInfo {
    rising: bool,
    stream: usize,
    rise: Option<f64>,
}
#[derive(Clone, Copy)]
struct TimedEdge {
    t: f64,
    info: EdgeInfo,
}

/// Metadata aligned with RecordIndex::edges, including rank/select lists for each direction.
pub(super) struct EdgeCache {
    threshold: Threshold,
    info: Vec<EdgeInfo>,
    rising: Vec<usize>,
    falling: Vec<usize>,
    breaks: Vec<f64>,
}
impl EdgeCache {
    /// Retain direction and stream resets once, using exactly the index's Schmitt detector.
    pub(super) fn new(index: &RecordIndex, times: &[f64], values: &[f32], gaps: &[(f64, f64)], threshold: Threshold) -> Self {
        let mut cache = Self { threshold, info: Vec::new(), rising: Vec::new(), falling: Vec::new(), breaks: Vec::new() };
        let mut tracker = EdgeTracker::new(threshold);
        let mut gap = 0; let mut stream = 0;
        for (i, (&t, &y)) in times.iter().zip(values).enumerate() {
            let previous = if i > 0 { times[i - 1] } else { t };
            while gap < gaps.len() && gaps[gap].1 < previous { gap += 1; }
            let broken = (gap < gaps.len() && gaps[gap].0 < t && gaps[gap].1 > previous)
                || !y.is_finite() || (i > 0 && !values[i - 1].is_finite());
            if broken { tracker.reset(); stream += 1; cache.breaks.push(t); }
            if let Some(edge) = tracker.push(t, y as f64) {
                let rising = edge.dir == Slope::Rising;
                let at = cache.info.len();
                debug_assert_eq!(index.edges.items[at].from, edge.t, "edge metadata follows the measurement index");
                if rising { cache.rising.push(at); } else { cache.falling.push(at); }
                cache.info.push(EdgeInfo { rising, stream, rise: transition(index, edge.t, rising) });
            }
        }
        debug_assert_eq!(cache.info.len(), index.edges.items.len(), "every indexed edge has metadata");
        cache
    }
}

fn transition(index: &RecordIndex, t: f64, rising: bool) -> Option<f64> {
    let transitions = if rising { &index.rises.items } else { &index.falls.items };
    let end = transitions.partition_point(|e| e.from <= t);
    end.checked_sub(1).and_then(|i| transitions.get(i)).filter(|e| t <= e.to).map(|e| e.value)
}
/// Copy auto-threshold index edges and cached directions, in trigger-relative seconds.
pub(super) fn alignment_edges(capture: &Capture, c: usize, from: f64, to: f64) -> Vec<fab_elec_bench::align::Edge> {
    let feature = &capture.features[c]; let (first, end) = feature.index.edges.range(from, to);
    (first..end).map(|i| {
        fab_elec_bench::align::Edge { t: feature.index.edges.items[i].from - capture.t_trigger, rising: feature.edges.info[i].rising }
    }).collect()
}
fn parameter(args: &Arguments, name: &str, unit: &str, auto: f64) -> Result<f64, String> {
    let text = args.text(name).unwrap_or("").trim();
    let value = if text.is_empty() { auto } else { number(text, unit).map_err(|e| format!("{name}: {e}; supply a quantity or leave empty for auto"))? };
    if !value.is_finite() { return Err(format!("{name}: no finite auto value; supply a quantity or choose another channel")); }
    Ok(value)
}
fn threshold(capture: &Capture, c: usize, args: &Arguments) -> Result<Threshold, String> {
    let auto = capture.features[c].edges.threshold;
    let unit = match capture.layout.channels[c].quantity { Quantity::Volt => "V", Quantity::Amp => "A", Quantity::Logic => "" };
    let level = parameter(args, "threshold", unit, auto.level)?;
    let hysteresis = parameter(args, "hysteresis", unit, auto.hysteresis)?;
    if hysteresis < 0.0 { return Err("hysteresis: supply a nonnegative quantity or leave empty for auto".into()); }
    Ok(Threshold { level, hysteresis })
}
fn matches(rising: bool, kind: &str) -> bool { kind == "both" || rising == (kind == "rising") }
fn limit(args: &Arguments) -> usize { usize::try_from(args.int("limit").unwrap()).unwrap_or(usize::MAX) }
fn threshold_key(threshold: Threshold) -> String { format!("{:016x}/{:016x}", threshold.level.to_bits(), threshold.hysteresis.to_bits()) }

enum EdgeList<'a> {
    Cached { index: &'a RecordIndex, cache: &'a EdgeCache, first: usize, end: usize, selected: Option<&'a [usize]> },
    Scanned(Vec<TimedEdge>),
}
impl<'a> EdgeList<'a> {
    fn new(capture: &'a Capture, c: usize, from: f64, to: f64, threshold: Threshold, kind: &str) -> Self {
        let feature = &capture.features[c]; let cache = &feature.edges;
        if threshold == cache.threshold {
            let (first, end) = feature.index.edges.range(from, to);
            let selected = match kind { "rising" => Some(&cache.rising), "falling" => Some(&cache.falling), _ => None };
            let selected = selected.map(|indices| {
                let lo = indices.partition_point(|&i| i < first); let hi = indices.partition_point(|&i| i < end);
                &indices[lo..hi]
            });
            return Self::Cached { index: &feature.index, cache, first, end, selected };
        }
        let trace = &capture.trace;
        // Include the bracketing samples so an interpolated crossing on a span boundary survives.
        let first = trace.index_at(from).map_or(0, |i| i - trace.base).saturating_sub(1);
        let end = trace.index_at(to).map_or(0, |i| i - trace.base + 2).min(trace.len());
        let mut broken = cache.breaks.partition_point(|&t| t < trace.time.at(first));
        let mut tracker = EdgeTracker::new(threshold);
        let mut stream = 0; let mut edges = Vec::new();
        for i in first..end {
            let t = trace.time.at(i);
            if cache.breaks.get(broken).is_some_and(|&at| at == t) { tracker.reset(); stream += 1; broken += 1; }
            if let Some(edge) = tracker.push(t, trace.signals[c].at(i) as f64) {
                let rising = edge.dir == Slope::Rising;
                if edge.t >= from && edge.t <= to && matches(rising, kind) {
                    edges.push(TimedEdge { t: edge.t, info: EdgeInfo { rising, stream, rise: None } });
                }
            }
        }
        Self::Scanned(edges)
    }
    fn len(&self) -> usize {
        match self { Self::Cached { first, end, selected, .. } => selected.map_or(end - first, <[usize]>::len), Self::Scanned(edges) => edges.len() }
    }
    fn at(&self, i: usize) -> TimedEdge {
        match self {
            Self::Cached { index, cache, first, selected, .. } => {
                let i = selected.map_or(first + i, |indices| indices[i]);
                TimedEdge { t: index.edges.items[i].from, info: cache.info[i] }
            }
            Self::Scanned(edges) => edges[i],
        }
    }
    fn rise(&self, index: &RecordIndex, edge: TimedEdge) -> Option<f64> {
        match self { Self::Cached { .. } => edge.info.rise, Self::Scanned(_) => transition(index, edge.t, edge.info.rising) }
    }
}

/// Read cached edges in O(log events + page), or scan the requested samples for other thresholds.
pub(super) fn edges(capture: &Capture, args: &Arguments, budget: usize) -> Result<ToolReply, String> {
    let c = channel(capture, args)?; let (from, to) = bounds(capture, args)?;
    let threshold = threshold(capture, c, args)?; let kind = args.text("kind").unwrap();
    let edges = EdgeList::new(capture, c, from, to, threshold, kind); let total = edges.len();
    let key = key(capture, &format!("edges/{c}/{}/{kind}", threshold_key(threshold)), from, to);
    let start = reply::offset(args.text("after"), &key, total)?;
    reply::page_values(budget, start, total, limit(args), &key, |i| {
        let edge = edges.at(i);
        let mut out = Object::default(); out.num("t", Some(edge.t - capture.t_trigger), QtyUnit::Second);
        out.symbol("dir", if edge.info.rising { "rising" } else { "falling" });
        out.num("rise", edges.rise(&capture.features[c].index, edge), QtyUnit::Second); Ok(out.render())
    }, |items, next| {
        let mut out = Object::default(); out.field("items", list(items.iter().cloned()));
        let unit = unit(capture.layout.channels[c].quantity);
        out.num("threshold", Some(threshold.level), unit); out.num("hysteresis", Some(threshold.hysteresis), unit);
        out.int("total", total); out.next(next); out.render()
    })
}

struct Pulse {
    t: f64,
    width: f64,
    rising: bool,
    stream: usize,
    period: Option<f64>,
}
struct Clause<'a> {
    what: &'a str,
    op: &'a str,
    value: f64,
}
impl Clause<'_> {
    fn accepts(&self, pulse: &Pulse) -> bool {
        let value = if self.what == "width" { Some(pulse.width) } else { pulse.period };
        value.is_some_and(|v| match self.op { "<" => v < self.value, "<=" => v <= self.value, ">" => v > self.value, _ => v >= self.value })
    }
}
fn clauses(text: &str) -> Result<Vec<Clause<'_>>, String> {
    if text.trim().is_empty() { return Ok(Vec::new()); }
    let mut clauses = Vec::new();
    for clause in text.split(" and ") {
        let bad = || format!("where: clause {clause:?}; use width or period, < <= > >= and a time quantity, with at most two clauses joined by ' and '");
        let words: Vec<_> = clause.split_whitespace().collect();
        let [what, op, value] = words.as_slice() else { return Err(bad()) };
        if clauses.len() == 2 || !["width", "period"].contains(what) || !["<", "<=", ">", ">="].contains(op) { return Err(bad()); }
        let value = number(value, "s").map_err(|_| bad())?;
        clauses.push(Clause { what, op, value });
    }
    Ok(clauses)
}
fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() { return None; }
    let mid = values.len() / 2; let even = values.len() % 2 == 0;
    let (lower, value, _) = values.select_nth_unstable_by(mid, f64::total_cmp);
    Some(if even { (*value + lower.iter().copied().max_by(f64::total_cmp).unwrap()) * 0.5 } else { *value })
}

/// Pair complete edges, filter and find medians in linear time, then optionally sort by deviation.
pub(super) fn pulses(capture: &Capture, args: &Arguments, budget: usize) -> Result<ToolReply, String> {
    let c = channel(capture, args)?; let (from, to) = bounds(capture, args)?;
    let threshold = threshold(capture, c, args)?; let kind = args.text("kind").unwrap();
    let level = args.text("level").unwrap(); let order = args.text("order").unwrap();
    let clauses = clauses(args.text("where").unwrap_or(""))?;
    let filter = clauses.iter().map(|clause| format!("{}{}{:016x}", clause.what, clause.op, clause.value.to_bits())).collect::<Vec<_>>().join("&");
    let key = key(capture, &format!("pulses/{c}/{}/{kind}/{level}/{order}/{filter}", threshold_key(threshold)), from, to);
    let edges = EdgeList::new(capture, c, from, to, threshold, "both");
    let mut pulses = Vec::new();
    for i in 1..edges.len() {
        let a = edges.at(i - 1); let b = edges.at(i);
        if a.info.stream == b.info.stream && a.info.rising != b.info.rising && a.t < b.t {
            pulses.push(Pulse { t: a.t, width: b.t - a.t, rising: a.info.rising, stream: a.info.stream, period: None });
        }
    }
    // Period is a physical interval before filtering; no period crosses a stream break.
    let mut next: [Option<(f64, usize)>; 2] = [None, None];
    for pulse in pulses.iter_mut().rev() {
        let next = &mut next[usize::from(pulse.rising)];
        pulse.period = next.filter(|&(_, stream)| stream == pulse.stream).map(|(t, _)| t - pulse.t);
        *next = Some((pulse.t, pulse.stream));
    }
    pulses.retain(|p| matches(p.rising, kind) && (level == "both" || p.rising == (level == "high")) && clauses.iter().all(|c| c.accepts(p)));
    let mut high: Vec<_> = pulses.iter().filter(|p| p.rising).map(|p| p.width).collect();
    let mut low: Vec<_> = pulses.iter().filter(|p| !p.rising).map(|p| p.width).collect();
    let high = median(&mut high); let low = median(&mut low);
    if order == "deviation" {
        let deviation = |p: &Pulse| (p.width - if p.rising { high.unwrap() } else { low.unwrap() }).abs();
        pulses.sort_unstable_by(|a, b| deviation(b).total_cmp(&deviation(a)).then_with(|| a.t.total_cmp(&b.t)));
    }
    let total = pulses.len(); let start = reply::offset(args.text("after"), &key, total)?;
    reply::page_values(budget, start, total, limit(args), &key, |i| {
        let pulse = &pulses[i]; let mut out = Object::default();
        out.num("t", Some(pulse.t - capture.t_trigger), QtyUnit::Second); out.num("width", Some(pulse.width), QtyUnit::Second);
        out.symbol("level", if pulse.rising { "high" } else { "low" }); out.num("period", pulse.period, QtyUnit::Second); Ok(out.render())
    }, |items, next| {
        let mut out = Object::default(); out.field("items", list(items.iter().cloned()));
        let mut median = Object::default(); median.num("high", high, QtyUnit::Second); median.num("low", low, QtyUnit::Second);
        out.field("median", median.render()); out.int("total", total);
        out.next(next); out.render()
    })
}
