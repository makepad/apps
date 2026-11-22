//! Immutable captures and the source-neutral intake which produces them on acquisition workers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use fab_elec_bench::measure_index::RecordIndex;
use fab_elec_bench::source::*;
use fab_elec_bench::trigger::{Edge, EdgeDetector, EdgeTrigger, Qualifier, Slope};
use fab_elec_circuit::QtyUnit;
use fab_elec_sim::trace::{TraceStore, CHUNK};
use super::reply::{percent, qty};
use super::super::fw_events::{FwEvents, ScriptedFwEvents};

/// The trigger and relative recording window kept with the resolved source settings.
#[derive(Clone, Debug)]
pub struct CaptureSpec {
    /// Source id, currently sim or synth.
    pub source: String,
    /// Optional human name, defaulting to the assigned id.
    pub name: String,
    /// Relative window start and end.
    pub span: (f64, f64),
    /// edge, level, none, width or period.
    pub trigger: String,
    pub qualifier: Qualifier,
    /// Trigger channel's index in the layout.
    pub channel: usize,
    /// Probe id or label used to select the trigger channel.
    pub on: String,
    /// Trigger threshold in channel units.
    pub level: f64,
    /// Accepted crossing direction.
    pub slope: Slope,
    /// Minimum interval between accepted triggers.
    pub holdoff: f64,
}

/// A contiguous activity interval, in source time.
#[derive(Clone, Debug)]
pub struct Segment {
    /// First instant.
    pub from: f64,
    /// Last instant.
    pub to: f64,
    /// Signal class and measured engineering quantities.
    pub what: String,
}

/// Features built exactly once when a channel's record closes.
pub struct Features {
    /// Shared bench numeric and timing measurements.
    pub index: RecordIndex,
    /// Direction, continuity and transition metadata alongside the index's cached edges.
    pub(super) edges: super::features::EdgeCache,
    /// Whole-record signal classification.
    pub class: &'static str,
    /// Run-length activity, ordered by time.
    pub segments: Vec<Segment>,
    /// Largest observed sample step.
    pub max_step: f64,
    /// Average observed sample step.
    pub mean_step: f64,
    /// Whether the sample times are uniform within floating-point clock rounding.
    pub uniform: bool,
}

impl Features {
    fn build(times: Arc<[f64]>, values: Arc<[f32]>, quantity: Quantity, gaps: &[(f64, f64)]) -> Self {
        let index = RecordIndex::with_breaks(times.clone(), values.clone(), gaps);
        let from = times[0]; let to = *times.last().unwrap();
        let min = values.iter().copied().fold(f32::INFINITY, f32::min) as f64;
        let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
        let edges = super::features::EdgeCache::new(&index, &times, &values, gaps,
            fab_elec_bench::edges::Threshold { level: (max + min) * 0.5, hysteresis: (max - min) * 0.1 });
        let swing = max - min;
        let two_level = swing > 0.0 && values.iter().filter(|&&v| {
            (v as f64 - index.base).abs() <= swing * 0.1 || (v as f64 - index.top).abs() <= swing * 0.1
        }).count() as f64 >= values.len() as f64 * 0.9;
        let regular = index.reading("frequency", from, to).is_some_and(|f| f.sigma < f.value * 0.01);
        let duty = index.reading("duty", from, to).map(|r| r.value);
        let mut intervals: Vec<f64> = index.edges.items.windows(2).map(|p| p[1].from - p[0].from).collect();
        intervals.sort_by(f64::total_cmp);
        let edge_median = median(&intervals);
        let quiet = (4.0 * edge_median).max((to - from) * 0.01);
        let grouped = index.edges.items.windows(2).any(|p| p[1].from - p[0].from > quiet)
            || index.edges.items.first().is_some_and(|e| e.from - from > quiet)
            || index.edges.items.last().is_some_and(|e| to - e.from > quiet);
        let mean = index.reading("mean", from, to).map(|r| r.value);
        let rms = index.reading("rms", from, to).map(|r| r.value);
        let sine = mean.zip(rms).is_some_and(|(mean, rms)| {
            let ac = (rms * rms - mean * mean).max(0.0).sqrt();
            let expected = swing / (2.0 * 2.0_f64.sqrt());
            expected > 0.0 && (ac - expected).abs() <= expected * 0.1
        });
        let class = if values.iter().any(|v| !v.is_finite()) { "unknown" }
            else if index.edges.items.is_empty() { if quantity == Quantity::Logic { "idle" } else { "dc" } }
            else if two_level && grouped { "bursts" }
            else if two_level && regular { if duty.is_some_and(|d| (45.0..=55.0).contains(&d)) { "square" } else { "pwm" } }
            else if !two_level && regular && sine { "sine" } else { "unknown" };
        let mut segments = Vec::new();
        let idle = |at: f64| {
            let i = times.partition_point(|&t| t <= at).saturating_sub(1);
            if values[i] as f64 > (min + max) * 0.5 { "idle high" } else { "idle low" }
        };
        let activity = |a: f64, b: f64| {
            if class == "bursts" {
                let tenth = intervals.len().div_ceil(10).max(1).min(intervals.len());
                let bit = median(&intervals[..tenth]);
                if bit > 0.0 { format!("burst {} baud", qty(1.0 / bit, QtyUnit::Count)) } else { "burst".into() }
            } else {
                let mut what = class.to_owned();
                if let Some(f) = index.reading("frequency", a, b) { what.push(' '); what.push_str(&qty(f.value, QtyUnit::Hertz)); }
                if class == "pwm" {
                    if let Some(d) = index.reading("duty", a, b) { what.push(' '); what.push_str(&percent(d.value)); }
                }
                what
            }
        };
        if index.edges.items.is_empty() {
            segments.push(Segment { from, to, what: idle(from).into() });
        } else {
            let mut cursor = from;
            let mut active = from;
            let first = index.edges.items[0].from;
            if first - from > quiet {
                segments.push(Segment { from, to: first, what: idle(from).into() });
                active = first; cursor = first;
            }
            for pair in index.edges.items.windows(2) {
                let a = pair[0].from; let b = pair[1].from;
                if b - a > quiet {
                    if a > active { segments.push(Segment { from: active, to: a, what: activity(active, a) }); }
                    segments.push(Segment { from: a, to: b, what: idle((a + b) * 0.5).into() });
                    active = b; cursor = b;
                }
            }
            let last = index.edges.items.last().unwrap().from;
            if to - last > quiet {
                if last > active { segments.push(Segment { from: active, to: last, what: activity(active, last) }); }
                segments.push(Segment { from: last, to, what: idle((last + to) * 0.5).into() });
            } else if to > cursor {
                segments.push(Segment { from: active, to, what: activity(active, to) });
            }
        }
        let mean_step = if times.len() > 1 { (to - from) / (times.len() - 1) as f64 } else { 0.0 };
        let max_step = times.windows(2).map(|p| p[1] - p[0]).fold(0.0, f64::max);
        let tolerance = mean_step.abs() * 1e-8 + to.abs().max(from.abs()) * f64::EPSILON * 4.0;
        let uniform = times.windows(2).all(|p| ((p[1] - p[0]) - mean_step).abs() <= tolerance);
        Self { index, edges, class, segments, max_step, mean_step, uniform }
    }
}
fn median(values: &[f64]) -> f64 {
    if values.is_empty() { return 0.0; }
    let mid = values.len() / 2;
    if values.len() % 2 == 0 { (values[mid - 1] + values[mid]) * 0.5 } else { values[mid] }
}

/// A completed, immutable capture shared through Arc.
pub struct Capture {
    /// Monotonically assigned id, never reused by its store.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Computed, hardware or replayed origin.
    pub side: Side,
    /// Source id.
    pub source: String,
    /// Recorded channels and their physical quantities.
    pub layout: Layout,
    /// Source settings actually applied.
    pub resolved: Resolved,
    /// Requested trigger and window.
    pub spec: CaptureSpec,
    /// Trigger instant in source time.
    pub t_trigger: f64,
    /// Recorded source-time bounds.
    pub span: (f64, f64),
    /// Missing spans, in source time.
    pub gaps: Vec<(f64, f64, Gap)>,
    /// Source annotations in addition to TraceStore's mark times.
    pub marks: Vec<(f64, Mark)>,
    /// Chunked samples and their min/max pyramid (shared with the Scope tab when it shows an
    /// opened trace).
    pub trace: Arc<TraceStore>,
    /// Each channel's display name, the Scope lane's label (its `name` is the net), empty where
    /// it has none.
    pub labels: Vec<String>,
    /// Firmware events copied at finish; firmware is not saved in the v1 record yet.
    pub firmware: Option<ScriptedFwEvents>,
    /// Cached features in layout order.
    pub features: Vec<Features>,
}

impl Capture {
    /// Sample extrema from a closed chunk's four-way pyramid, or the cached coarse index when
    /// the span crosses chunks. Only the unaligned fringe reads individual samples.
    pub fn minmax(&self, channel: usize, first: usize, end: usize) -> Option<(f64, f64)> {
        if first >= end || end > self.trace.len() { return None; }
        let signal = self.trace.signals.get(channel)?;
        if first / CHUNK != (end - 1) / CHUNK || first / CHUNK >= signal.chunks.len() {
            let from = self.trace.time.at(first); let to = self.trace.time.at(end - 1);
            let index = &self.features[channel].index;
            return Some((index.reading("min", from, to)?.value, index.reading("max", from, to)?.value));
        }
        let bounds = crate::trace::store::range_minmax(signal, first, end)?;
        (bounds.min.is_finite() && bounds.max.is_finite()).then_some((bounds.min as f64, bounds.max as f64))
    }
    /// The self-describing little-endian record ([`trace_bytes`]).
    pub fn bytes(&self) -> Vec<u8> {
        let signals: Vec<usize> = (0..self.trace.signals.len()).collect();
        trace_bytes(&self.layout, &self.labels, &self.trace, &signals, self.t_trigger, &self.gaps, &self.marks)
    }

    /// A saved trace as a capture of its own (`elec.capture.open`): its record and features as a
    /// finished capture has them, its side Replay, its store numbered [`OPENED_GENERATION`].
    /// O(samples × channels).
    pub fn replayed(id: String, name: String, spec: CaptureSpec, resolved: Resolved, saved: ParsedTrace) -> Result<Capture, String> {
        let ParsedTrace { layout, labels, times, values, t_trigger, gaps, marks } = saved;
        if times.len() < 2 {
            return Err("the trace holds fewer than two samples".into());
        }
        let span = (times[0], times[times.len() - 1]);
        let mut trace = TraceStore::new(OPENED_GENERATION, &vec![false; layout.channels.len()]);
        trace.push_rows(&times, |i, k| f64::from(values[k][i]), &mut Vec::new());
        trace.marks.extend(marks.iter().map(|m| m.0));
        let times: Arc<[f64]> = times.into();
        let breaks: Vec<_> = gaps.iter().map(|&(a, b, _)| (a, b)).collect();
        let features = layout.channels.iter().zip(values).map(|(info, values)| Features::build(times.clone(), values.into(), info.quantity, &breaks)).collect();
        Ok(Capture { id, name, side: Side::Replay, source: "trace".into(), layout, resolved, spec, t_trigger, span, gaps, marks,
            trace: Arc::new(trace), labels, firmware: None, features })
    }
}

/// A trace's contents as its file holds them ([`parse_trace`]): channel k's samples at every
/// time, then its trigger instant, gaps and marks.
pub struct ParsedTrace {
    pub layout: Layout,
    pub labels: Vec<String>,
    pub times: Vec<f64>,
    pub values: Vec<Vec<f32>>,
    pub t_trigger: f64,
    pub gaps: Vec<(f64, f64, Gap)>,
    pub marks: Vec<(f64, Mark)>,
}

/// A saved trace (`.trace`, FABTRACE v2) as written ([`trace_bytes`]) and read back
/// ([`read_trace`]): its layout and display names, its samples in a store of its own (signal k
/// is channel k), its trigger instant, gaps and marks.
pub struct SavedTrace {
    pub layout: Layout,
    pub labels: Vec<String>,
    pub store: TraceStore,
    pub t_trigger: f64,
    pub gaps: Vec<(f64, f64, Gap)>,
    pub marks: Vec<(f64, Mark)>,
}

/// A saved trace shown in the Scope tab in place of the live run (`ElecLive::opened`): its
/// project-relative path, its channels and its record, numbered [`OPENED_GENERATION`].
pub struct OpenedTrace {
    pub path: String,
    pub layout: Layout,
    /// Each channel's display name (the Scope's lane label when saved), empty where it has none.
    pub labels: Vec<String>,
    pub store: Arc<TraceStore>,
}

/// The numbering an opened trace's record has: apart from every run's, so no row of the live
/// numbering is ever read in it.
pub const OPENED_GENERATION: u64 = u64::MAX;

const TRACE_MAGIC: &[u8; 8] = b"FABTRACE";

/// Where a trace named `name` goes in the project: `captures/<name>.trace`. A name that is not a
/// plain file name fails.
pub fn trace_path(name: &str) -> Result<String, String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return Err("name: expected a filename within captures".into());
    }
    Ok(format!("captures/{name}.trace"))
}

/// The record format's version: 2 names each channel by its net and gives its display name.
const TRACE_VERSION: u32 = 2;

/// The self-describing little-endian record of a trace (a capture, or the Scope's take), version
/// [`TRACE_VERSION`]: channel k of `layout` (its net as its name, `labels[k]` its display name,
/// empty for none) is signal `signals[k]` of `trace`, its samples written at every time of the
/// store (NaN where it holds none), then the trigger instant, gaps and marks, with explicit
/// quantity tags and lengths. O(samples).
pub fn trace_bytes(layout: &Layout, labels: &[String], trace: &TraceStore, signals: &[usize], t_trigger: f64, gaps: &[(f64, f64, Gap)], marks: &[(f64, Mark)]) -> Vec<u8> {
    fn string(out: &mut Vec<u8>, text: &str) {
        out.extend_from_slice(&(text.len() as u64).to_le_bytes()); out.extend_from_slice(text.as_bytes());
    }
    let mut out = TRACE_MAGIC.to_vec();
    out.extend_from_slice(&TRACE_VERSION.to_le_bytes());
    out.extend_from_slice(&layout.generation.to_le_bytes());
    out.push(match layout.clock { ClockKind::Simulated => 0, ClockKind::Device => 1 });
    out.extend_from_slice(&(layout.channels.len() as u32).to_le_bytes());
    for (k, c) in layout.channels.iter().enumerate() {
        out.extend_from_slice(&c.id.0.to_le_bytes()); string(&mut out, &c.name);
        out.push(match c.quantity { Quantity::Volt => 0, Quantity::Amp => 1, Quantity::Logic => 2 });
        string(&mut out, labels.get(k).map_or("", String::as_str));
    }
    for &k in signals {
        let signal = &trace.signals[k];
        out.extend_from_slice(&(trace.len() as u64).to_le_bytes());
        for i in 0..trace.len() {
            out.extend_from_slice(&trace.time.at(i).to_le_bytes()); out.extend_from_slice(&signal.at(i).to_le_bytes());
        }
    }
    out.extend_from_slice(&t_trigger.to_le_bytes());
    out.extend_from_slice(&(gaps.len() as u64).to_le_bytes());
    for &(from, to, why) in gaps {
        out.extend_from_slice(&from.to_le_bytes()); out.extend_from_slice(&to.to_le_bytes());
        out.push(match why { Gap::Overrun => 0, Gap::Rearm => 1, Gap::Blind => 2, Gap::Restart => 3 });
    }
    out.extend_from_slice(&(marks.len() as u64).to_le_bytes());
    for &(t, kind) in marks {
        out.extend_from_slice(&t.to_le_bytes()); out.push(match kind { Mark::Solver => 0, Mark::HardwareTrigger => 1, Mark::Segment => 2, Mark::Held => 3, Mark::BelowRes => 4 });
    }
    out
}

/// A saved trace read back from its bytes ([`trace_bytes`]), its store of generation
/// `generation`. O(bytes).
pub fn read_trace(bytes: &[u8], generation: u64) -> Result<SavedTrace, String> {
    let ParsedTrace { layout, labels, times, values, t_trigger, gaps, marks } = parse_trace(bytes)?;
    let mut store = TraceStore::new(generation, &vec![false; layout.channels.len()]);
    store.push_rows(&times, |i, k| f64::from(values[k][i]), &mut Vec::new());
    Ok(SavedTrace { layout, labels, store, t_trigger, gaps, marks })
}

/// A trace's contents from its bytes ([`trace_bytes`]). Every length is checked against the
/// bytes left before anything is kept, so a damaged file fails with what was wrong and never
/// allocates past its own size. O(bytes).
pub fn parse_trace(bytes: &[u8]) -> Result<ParsedTrace, String> {
    struct Reader<'a> { bytes: &'a [u8], at: usize }
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
            let end = self.at.checked_add(n).filter(|&end| end <= self.bytes.len()).ok_or_else(|| format!("the trace ends early (byte {})", self.at))?;
            let out = &self.bytes[self.at..end];
            self.at = end;
            Ok(out)
        }
        fn u8(&mut self) -> Result<u8, String> { Ok(self.take(1)?[0]) }
        fn u32(&mut self) -> Result<u32, String> { Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap())) }
        fn u64(&mut self) -> Result<u64, String> { Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap())) }
        fn f64(&mut self) -> Result<f64, String> { Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap())) }
        fn f32(&mut self) -> Result<f32, String> { Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap())) }
        /// A count of items of at least `each` bytes, no more than the bytes left hold.
        fn count(&mut self, each: usize) -> Result<usize, String> {
            let n = self.u64()?;
            usize::try_from(n).ok().filter(|n| n.saturating_mul(each) <= self.bytes.len() - self.at).ok_or_else(|| format!("the trace counts {n} items past its end (byte {})", self.at))
        }
        fn string(&mut self) -> Result<String, String> {
            let n = self.count(1)?;
            String::from_utf8(self.take(n)?.to_vec()).map_err(|_| format!("a name is not text (byte {})", self.at))
        }
    }
    let mut r = Reader { bytes, at: 0 };
    if r.take(8).ok() != Some(&TRACE_MAGIC[..]) {
        return Err("not a trace: it does not start with FABTRACE".into());
    }
    let version = r.u32()?;
    if version != TRACE_VERSION {
        return Err(format!("trace version {version}: this Fab reads version {TRACE_VERSION}; save the take again"));
    }
    let made = r.u64()?;
    let clock = match r.u8()? { 0 => ClockKind::Simulated, 1 => ClockKind::Device, k => return Err(format!("unknown clock {k}")) };
    let n = r.u32()? as usize;
    if n.saturating_mul(21) > bytes.len() - r.at {
        return Err(format!("the trace counts {n} channels past its end"));
    }
    let (mut channels, mut labels) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for _ in 0..n {
        let id = ChanId(r.u32()?);
        let name = r.string()?;
        let quantity = match r.u8()? { 0 => Quantity::Volt, 1 => Quantity::Amp, 2 => Quantity::Logic, k => return Err(format!("unknown quantity {k} of channel {name}")) };
        channels.push(ChannelInfo { id, name, quantity });
        labels.push(r.string()?);
    }
    let (mut times, mut values): (Vec<f64>, Vec<Vec<f32>>) = (Vec::new(), Vec::with_capacity(n));
    for c in &channels {
        let len = r.count(12)?;
        if !values.is_empty() && len != times.len() {
            return Err(format!("channel {} holds {len} samples, the others {}", c.name, times.len()));
        }
        let first = values.is_empty();
        let mut v = Vec::with_capacity(len);
        for i in 0..len {
            let t = r.f64()?;
            if first {
                if !t.is_finite() || times.last().is_some_and(|&last| t < last) {
                    return Err(format!("sample {i}'s time {t} is not after the one before"));
                }
                times.push(t);
            } else if t.to_bits() != times[i].to_bits() {
                return Err(format!("channel {}'s sample {i} is at another time than the first channel's", c.name));
            }
            v.push(r.f32()?);
        }
        values.push(v);
    }
    let t_trigger = r.f64()?;
    let mut gaps = Vec::new();
    for _ in 0..r.count(17)? {
        let (from, to) = (r.f64()?, r.f64()?);
        let why = match r.u8()? { 0 => Gap::Overrun, 1 => Gap::Rearm, 2 => Gap::Blind, 3 => Gap::Restart, k => return Err(format!("unknown gap {k}")) };
        gaps.push((from, to, why));
    }
    let mut marks = Vec::new();
    for _ in 0..r.count(9)? {
        let t = r.f64()?;
        let kind = match r.u8()? { 0 => Mark::Solver, 1 => Mark::HardwareTrigger, 2 => Mark::Segment, 3 => Mark::Held, 4 => Mark::BelowRes, k => return Err(format!("unknown mark {k}")) };
        marks.push((t, kind));
    }
    if r.at != bytes.len() {
        return Err(format!("{} bytes follow the trace's end", bytes.len() - r.at));
    }
    Ok(ParsedTrace { layout: Layout { generation: made, clock, channels }, labels, times, values, t_trigger, gaps, marks })
}

/// Worker-owned records; ids are reserved even when acquisition subsequently fails.
#[derive(Default)]
pub struct CaptureStore {
    next: u64,
    installed: BTreeSet<u64>,
    records: BTreeMap<u64, Arc<Capture>>,
    saved: BTreeMap<String, String>,
}
impl CaptureStore {
    /// Reserve the next session id.
    pub fn reserve(&mut self) -> String { self.next += 1; format!("cap{}", self.next) }
    /// Install a finalized capture whose id was reserved in this store.
    pub fn insert(&mut self, capture: Arc<Capture>) {
        let id = capture.id.strip_prefix("cap").and_then(|s| s.parse::<u64>().ok()).expect("store capture id");
        assert!(id > 0 && id <= self.next && self.installed.insert(id), "capture ids must be reserved and unique");
        self.records.insert(id, capture);
    }
    /// Find a completed capture.
    pub fn get(&self, id: &str) -> Option<&Arc<Capture>> {
        self.records.get(&id.strip_prefix("cap")?.parse::<u64>().ok()?)
    }
    /// Release one record; a future reservation cannot reuse its id.
    pub fn drop_capture(&mut self, id: &str) -> bool {
        self.saved.remove(id);
        id.strip_prefix("cap").and_then(|s| s.parse::<u64>().ok()).is_some_and(|id| self.records.remove(&id).is_some())
    }
    /// Completed records in creation order.
    pub fn captures(&self) -> impl Iterator<Item = &Arc<Capture>> { self.records.values() }
    /// The path from the most recent successful save.
    pub fn saved(&self, id: &str) -> Option<&str> { self.saved.get(id).map(String::as_str) }
    /// Remember a successful host write separately from the immutable record.
    pub fn set_saved(&mut self, id: String, path: String) { self.saved.insert(id, path); }
}

type Point = (f64, Vec<f32>);

/// Source-neutral single capture. Completion and feature construction happen on its input worker.
pub struct CaptureIntake {
    id: String,
    spec: CaptureSpec,
    side: Side,
    resolved: Resolved,
    firmware: Option<Arc<dyn FwEvents>>,
    layout: Option<Layout>,
    detector: EdgeDetector,
    history: VecDeque<Point>,
    points: Vec<Point>,
    trigger: Option<f64>,
    latest: Option<f64>,
    gaps: Vec<(f64, f64, Gap)>,
    marks: Vec<(f64, Mark)>,
    complete: Sender<Arc<Capture>>,
    cancelled: Arc<AtomicBool>,
    finished: bool,
    /// The channels' display names ([`Capture::labels`]).
    labels: Vec<String>,
}
impl CaptureIntake {
    /// Build an intake with the originating source's side; the receiver belongs to the tools worker, never the UI.
    pub fn new(id: String, spec: CaptureSpec, side: Side, resolved: Resolved, firmware: Option<Arc<dyn FwEvents>>, complete: Sender<Arc<Capture>>, cancelled: Arc<AtomicBool>) -> Self {
        let detector = EdgeDetector::with_qualifier(EdgeTrigger { level: spec.level, hyst: 0.0, slope: spec.slope, holdoff: spec.holdoff }, spec.qualifier);
        Self { id, spec, side, resolved, firmware, layout: None, detector, history: VecDeque::new(), points: Vec::new(), trigger: None, latest: None,
            gaps: Vec::new(), marks: Vec::new(), complete, cancelled, finished: false, labels: Vec::new() }
    }
    /// The channels' display names, in layout order (the probes' Scope labels).
    pub fn with_labels(mut self, labels: Vec<String>) -> Self {
        self.labels = labels;
        self
    }
    fn stopped(&self) -> bool { self.finished || self.cancelled.load(Ordering::Acquire) }
    fn finish(&mut self) {
        if self.stopped() || self.points.is_empty() { return; }
        let (Some(trigger), Some(layout)) = (self.trigger, self.layout.take()) else { return };
        self.finished = true;
        let mut trace = TraceStore::new(layout.generation, &vec![false; layout.channels.len()]);
        let mut closed = Vec::new();
        let mut values = vec![0.0; layout.channels.len()];
        for (t, y) in &self.points {
            for (out, &v) in values.iter_mut().zip(y) { *out = v as f64; }
            trace.push(*t, &values, &mut closed); closed.clear();
        }
        let span = (self.points[0].0, self.points.last().unwrap().0);
        let times: Arc<[f64]> = self.points.iter().map(|p| p.0).collect();
        self.gaps.sort_by(|a, b| a.0.total_cmp(&b.0));
        let breaks: Vec<_> = self.gaps.iter().map(|&(a, b, _)| (a, b)).collect();
        let features = layout.channels.iter().enumerate().map(|(c, info)| {
            let values: Arc<[f32]> = self.points.iter().map(|p| p.1[c]).collect();
            Features::build(times.clone(), values, info.quantity, &breaks)
        }).collect();
        let gaps = self.gaps.iter().filter(|&&(a, b, _)| b >= span.0 && a <= span.1)
            .map(|&(a, b, why)| (a.max(span.0), b.min(span.1), why)).collect();
        self.marks.retain(|&(t, _)| t >= span.0 && t <= span.1);
        trace.marks.extend(self.marks.iter().map(|m| m.0));
        let name = if self.spec.name.is_empty() { self.id.clone() } else { self.spec.name.clone() };
        let firmware = self.firmware.take().map(|store| ScriptedFwEvents::window(store.as_ref(), span.0, span.1));
        let mut labels = std::mem::take(&mut self.labels);
        labels.resize(layout.channels.len(), String::new());
        let capture = Arc::new(Capture { id: self.id.clone(), name, side: self.side, source: self.spec.source.clone(), layout,
            resolved: self.resolved.clone(), spec: self.spec.clone(), t_trigger: trigger, span, gaps,
            marks: std::mem::take(&mut self.marks), trace: Arc::new(trace), labels, firmware, features });
        self.points.clear(); self.history.clear();
        let _ = self.complete.send(capture);
    }
    fn full(&self) -> bool {
        self.trigger.is_some_and(|t| self.latest.is_some_and(|at| at >= t + self.spec.span.1))
    }
    fn record(&mut self, t: f64, y: &[f32]) {
        let Some(trigger) = self.trigger else { return };
        let from = trigger + self.spec.span.0; let to = trigger + self.spec.span.1;
        if t < from { return; }
        if self.points.last().is_some_and(|p| p.0 >= to) { return; }
        if t <= to { self.points.push((t, y.to_vec())); }
        else if let Some((at, previous)) = self.points.last() {
            if self.gaps.iter().rev().any(|&(a, b, _)| a < to && b > *at) { return; }
            let f = (to - at) / (t - at);
            let end = previous.iter().zip(y).map(|(&a, &b)| (a as f64 + f * (b as f64 - a as f64)) as f32).collect();
            self.points.push((to, end));
        }
    }
}
impl Intake for CaptureIntake {
    fn begin(&mut self, layout: &Layout) {
        if self.stopped() { return; }
        if self.layout.is_some() { self.finish(); }
        if self.stopped() { return; }
        assert!(self.spec.channel < layout.channels.len(), "trigger channel must be in capture layout");
        self.layout = Some(layout.clone()); self.history.clear(); self.detector.break_stream(); self.latest = None;
        self.gaps.clear(); self.marks.clear();
    }
    fn point(&mut self, t: f64, y: &[f32]) {
        if self.full() { self.finish(); }
        if self.stopped() || !t.is_finite() { return; }
        let Some(layout) = &self.layout else { return };
        assert_eq!(layout.channels.len(), y.len(), "capture point must contain every channel");
        if self.history.back().is_some_and(|p| p.0 >= t) {
            if self.trigger.is_some() { self.finish(); return; }
            self.history.clear(); self.detector.break_stream();
        }
        self.latest = Some(t);
        self.history.push_back((t, y.to_vec()));
        if self.trigger.is_none() {
            let event = match self.spec.trigger.as_str() {
                "none" => Some(self.history[0].0 - self.spec.span.0.min(0.0)),
                "level" => {
                    let value = y[self.spec.channel] as f64;
                    (value.is_finite() && match self.spec.slope {
                        Slope::Rising => value >= self.spec.level, Slope::Falling => value <= self.spec.level,
                        Slope::Either => value == self.spec.level,
                    }).then_some(t)
                }
                _ => match self.detector.push(t, y[self.spec.channel]) { Edge::Accepted(at) => Some(at), _ => None },
            };
            if let Some(at) = event.filter(|at| *at + self.spec.span.0 >= self.history[0].0) {
                self.trigger = Some(at);
                let from = at + self.spec.span.0;
                let history: Vec<_> = self.history.iter().cloned().collect();
                for pair in history.windows(2) {
                    if pair[0].0 < from && pair[1].0 > from {
                        let f = (from - pair[0].0) / (pair[1].0 - pair[0].0);
                        let y: Vec<_> = pair[0].1.iter().zip(&pair[1].1).map(|(&a, &b)| (a as f64 + f * (b as f64 - a as f64)) as f32).collect();
                        self.record(from, &y);
                    }
                }
                for (t, y) in history { self.record(t, &y); }
            }
        } else {
            let from = self.trigger.unwrap() + self.spec.span.0;
            if self.points.is_empty() && self.history.len() >= 2 {
                let prev = &self.history[self.history.len() - 2];
                if prev.0 < from && t > from {
                    let f = (from - prev.0) / (t - prev.0);
                    let y: Vec<_> = prev.1.iter().zip(y).map(|(&a, &b)| (a as f64 + f * (b as f64 - a as f64)) as f32).collect();
                    self.record(from, &y);
                }
            }
            self.record(t, y);
        }
        let mut oldest = t + self.spec.span.0.min(0.0);
        for pending in self.detector.pending().into_iter().flatten() { oldest = oldest.min(pending + self.spec.span.0); }
        while self.history.len() > 2 && self.history[1].0 < oldest { self.history.pop_front(); }
        let keep = self.history.front().map_or(t, |p| p.0);
        if self.trigger.is_none() { self.marks.retain(|&(t, _)| t >= keep); self.gaps.retain(|&(_, to, _)| to >= keep); }
    }
    fn block(&mut self, block: &Block) {
        let n = block.chans.first().map_or(0, ChanData::len);
        assert!(block.t0.is_finite() && block.dt.is_finite() && block.dt > 0.0, "capture block clock must increase");
        assert!(block.chans.iter().all(|c| c.len() == n), "capture block columns must have equal lengths");
        let mut values = vec![0.0; block.chans.len()];
        if self.trigger.is_none() {
            self.trigger = block.trigger;
            if let Some(t) = block.trigger { self.marks.push((t, Mark::HardwareTrigger)); }
        }
        for i in 0..n {
            for (v, c) in values.iter_mut().zip(block.chans) { *v = c.value(i); }
            self.point(block.t0 + i as f64 * block.dt, &values);
        }
    }
    fn mark(&mut self, t: f64, kind: Mark) {
        // a step below the time resolution is not an event: the trace's marks are events
        if kind != Mark::BelowRes && !self.stopped() { self.marks.push((t, kind)); }
    }
    fn gap(&mut self, from: f64, to: f64, why: Gap) {
        assert!(from.is_finite() && to.is_finite() && to >= from, "capture gap must be a finite forward interval");
        if !self.stopped() { self.gaps.push((from, to, why)); self.detector.break_stream(); self.history.clear(); }
    }
    fn idle(&mut self) { if self.full() { self.finish(); } }
}
impl Drop for CaptureIntake {
    fn drop(&mut self) { self.finish(); }
}
