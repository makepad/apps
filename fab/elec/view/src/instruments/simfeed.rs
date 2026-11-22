//! Live solver acquisitions sharing one merge worker and a UI mailbox.

use super::acquire::{AcquireIntake, AcquireStats, Acquisition, Capture, LaneFrame, LaneSetup, LaneSlot, NetProbe, Published, SimChannel, SimIntake, SimLayouts, TrendSlot, TriggerSlot, TriggerState};
use super::feed::{same_shape, FeedStats, Merger, ScopeFeed, Settings, RETURN_SLOTS};
use super::events::EventList;
use super::intake_worker::{self, Batch, IntakeLink};
use super::trend::{TrendBound, TrendQuantity, TrendSeries, Violation, ViolationList};
use fab_elec_bench::limited::{LimitedIntake, LimitedSlot};
pub use fab_elec_bench::limited::Limited as LimitedSpec;
use fab_elec_bench::source::{ChanId, Intake, Mark, Quantity};
use super::scope::{FftSettings, Measurement};
use super::spectrum::{SpectrumMeasurements, SpectrumResult};
use super::span::{span_stats, SpanRequest, SpanStats};
use super::scrub::{indexed_rows, record_index};
use fab_elec_bench::measure_index::RecordIndex;
use crate::live::sim::{SimAction, SinkSlot};
use crate::trace::{columns::Columns, spark::SPARK_COLS};
use fab_elec_bench::trigger::{EdgeTrigger, Qualifier, Window};
use fab_elec_sim::Sink;
use makepad_widgets_core::{Cx, SignalToUI, SpawnError};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);

/// The finest pixel column over a scope's lanes, simulated seconds (`SimAction::ShowScope`). A
/// change of a lane's time map makes a new acquisition, so this is sent once per shown scope.
fn finest_column(lanes: &[LaneSetup]) -> f64 {
    lanes.iter().map(|lane| lane.map.t_px).filter(|c| c.is_finite() && *c > 0.0).fold(f64::INFINITY, f64::min)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FeatureKind { Rise, Fall, Top, Base }

/// One feature of a lane in one capture, in trigger-relative seconds and lane units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Feature {
    pub kind: FeatureKind, pub t: f64, pub v: f64,
    /// Rise and fall: the 10, 50 and 90 % crossing times (t equals the 50 % one); None otherwise.
    pub crossings: Option<[f64; 3]>,
}

/// The features of every lane of one capture, ordered by time within a lane.
#[derive(Default)]
pub struct CaptureFeatures { pub configuration: u64, pub lanes: Vec<Vec<Feature>> }

/// Build one lane's features from its measurement index and the same finite, ordered samples,
/// in O(samples + events), with no event count limit.
/// `trigger` is in source time. Crossings follow amplitude order, so a fall has 90 before 50 before 10.
pub fn lane_features(t: &[f64], y: &[f32], index: &RecordIndex, trigger: f64) -> Vec<Feature> {
    if t.len() < 2 || t.len() != y.len() { return Vec::new(); }
    let mut rises = index.rises.items.iter().peekable();
    let mut falls = index.falls.items.iter().peekable();
    let mid = (index.top + index.base) * 0.5;
    let mut features = Vec::new();
    let (mut crossing_sample, mut level_sample) = (0, 0);
    let mut start = t[0];
    let mut last_kind = if y[0] as f64 > mid { FeatureKind::Top } else { FeatureKind::Base };
    // The stretches are disjoint and ordered. Integrate each segment at most once, with
    // interpolated boundaries, rather than taking a binary-search reading for every stretch.
    let mut stretch = |from: f64, to: f64, kind, features: &mut Vec<Feature>| {
        if to <= from { return; }
        while level_sample + 1 < t.len() && t[level_sample + 1] <= from { level_sample += 1; }
        let mut area = 0.0;
        let mut at = from;
        while level_sample + 1 < t.len() && at < to {
            let i = level_sample;
            let end = to.min(t[i + 1]);
            let value = |time| y[i] as f64 + (y[i + 1] as f64 - y[i] as f64) * ((time - t[i]) / (t[i + 1] - t[i]));
            area += (end - at) * (value(at) + value(end)) * 0.5;
            at = end;
            if end == t[i + 1] { level_sample += 1; }
        }
        features.push(Feature { kind, t: (from + (to - from) * 0.5) - trigger, v: area / (to - from), crossings: None });
    };
    while rises.peek().is_some() || falls.peek().is_some() {
        let rising = match (rises.peek(), falls.peek()) { (Some(a), Some(b)) => a.from <= b.from, (Some(_), None) => true, _ => false };
        let event = if rising { rises.next().unwrap() } else { falls.next().unwrap() };
        stretch(start, event.from, if rising { FeatureKind::Base } else { FeatureKind::Top }, &mut features);
        while crossing_sample + 1 < t.len() && t[crossing_sample + 1] < event.from { crossing_sample += 1; }
        let mut middle = None;
        while crossing_sample + 1 < t.len() && t[crossing_sample] <= event.to {
            let i = crossing_sample;
            let (a, b) = (y[i] as f64, y[i + 1] as f64);
            let crosses = if rising { a <= mid && b > mid } else { a >= mid && b < mid };
            if crosses {
                let at = t[i] + (t[i + 1] - t[i]) * ((mid - a) / (b - a));
                if at >= event.from && at <= event.to { middle = Some(at); }
            }
            if t[i + 1] >= event.to { break; }
            crossing_sample += 1;
        }
        if let Some(at) = middle {
            let crossings = if rising { [event.from - trigger, at - trigger, event.to - trigger] }
                else { [event.to - trigger, at - trigger, event.from - trigger] };
            features.push(Feature { kind: if rising { FeatureKind::Rise } else { FeatureKind::Fall }, t: at - trigger, v: mid, crossings: Some(crossings) });
        }
        start = event.to;
        last_kind = if rising { FeatureKind::Top } else { FeatureKind::Base };
    }
    stretch(start, t[t.len() - 1], last_kind, &mut features);
    features
}

fn next_id() -> u64 {
    NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)
}

/// Plot, acquisition, and publication settings for simulation probes.
#[derive(Clone, Copy, Debug)]
pub struct SimScopeConfig {
    pub plot: (u32, u32),
    pub volts: (f64, f64),
    pub trigger: EdgeTrigger,
    pub qualifier: Qualifier,
    pub zone: Option<super::acquire::Zone>,
    pub window: Window,
    pub auto: Option<f64>,
    pub frame_every: Duration,
}

impl SimScopeConfig {
    fn settings(self) -> Settings {
        Settings { qualifier: self.qualifier, zone: self.zone, ..Settings::single(self.plot, self.volts, self.trigger, self.window, self.auto) }
    }
}

struct Frame {
    hits: LaneFrame,
    stats: AcquireStats,
    generation: u64,
    sink: u64,
    alive: Weak<()>,
    returned: SyncSender<LaneFrame>,
}

enum Inbox { Frame(Frame), Returned(LaneFrame) }

/// A fixed configuration for additional sinks on the same merge worker.
/// Fetch a new handle after reconfiguration to make sinks with the new settings.
/// ADC limits follow the shared slot, including on previously fetched handles.
#[derive(Clone)]
pub struct ScopeSinks {
    channels: Vec<SimChannel>,
    lanes: Vec<LaneSetup>,
    trigger_channel: usize,
    config: SimScopeConfig,
    limited: bool,
    limited_slot: Arc<LimitedSlot>,
    generation: u64,
    frames: SyncSender<Inbox>,
    capture_request: Arc<AtomicBool>,
    captures: SyncSender<Capture>,
    trigger: Arc<TriggerSlot>,
    lane_slot: Arc<LaneSlot>,
    trend_slot: Arc<TrendSlot>,
    span_request: Arc<SpanRequest>,
    epochs: Arc<Mutex<HashMap<(u64, u64), u64>>>,
    layouts: Arc<Mutex<SimLayouts>>,
    /// The feed's intake worker: the nominal acquisition runs there, beside the solver.
    intakes: SyncSender<Batch>,
}

impl ScopeSinks {
    /// Build an independent acquisition for one simulation member.
    pub fn make(&self) -> Box<dyn Sink + Send> {
        self.make_intake(false)
    }

    // Only the nominal simulation shares the strip's record clock. Ensemble members must
    // not interleave their independent timelines into its ordered event list.
    fn make_intake(&self, nominal: bool) -> Box<dyn Sink + Send> {
        let settings = self.config.settings();
        let (returned, spares) = sync_channel(RETURN_SLOTS);
        let frames = self.frames.clone();
        let generation = self.generation;
        let sink = next_id();
        let epochs = self.epochs.clone();
        let mut epoch = None;
        let alive = Arc::new(());
        let mut full = false;
        let mut free = Vec::<LaneFrame>::new();
        let mut full_log = EpisodeLog::new("scope feed: frame queue full; hits retained for the next flush");
        let mut acquisition = Acquisition::new(settings.trigger, self.trigger_channel, settings.window, self.lanes.clone()).with_qualifier(settings.qualifier);
        acquisition.set_auto(settings.auto);
        acquisition.set_zone(settings.zone);
        let lanes = self.lanes.clone();
        let mut acquisition =
            AcquireIntake::new(acquisition, self.config.frame_every, move |hits: LaneFrame, stats: AcquireStats| {
                // Members share a tag for each source layout and gate within this configuration.
                // Cache it locally: only a layout or gate edit takes the short registry lock.
                let source_generation = hits.generation;
                let span = if lanes.iter().any(|lane| lane.spectrum.is_some()) { stats.spectrum_span } else { 0 };
                let tag = match epoch {
                    Some((source, gate, tag)) if source == source_generation && gate == span => tag,
                    _ => {
                        let mut epochs = epochs.lock().unwrap();
                        epochs.retain(|&(_, gate), _| gate >= span);
                        let tag = *epochs.entry((source_generation, span)).or_insert_with(next_id);
                        epoch = Some((source_generation, span, tag));
                        tag
                    }
                };
                let hits = retag(hits, tag);
                // Empty the return queue on every publication, sent or kept. Taking one spare per sent
                // frame would leave any storage allocated while the merger was busy circulating with
                // nowhere to go, and a kept frame would take none: the queue fills and the merger holds
                // each return for a retry. A new frame is allocated only when every frame of this sink
                // is in flight.
                free.extend(spares.try_iter());
                let frame = Frame { hits, stats, generation, sink, alive: Arc::downgrade(&alive), returned: returned.clone() };
                match frames.try_send(Inbox::Frame(frame)) {
                    Ok(()) => {
                        full = false;
                        Published::Taken(free.pop().unwrap_or_else(|| LaneFrame::new(&lanes)))
                    }
                    Err(TrySendError::Full(Inbox::Frame(frame))) => {
                        if !full {
                            full_log.begin();
                            full = true;
                        }
                        Published::Kept(retag(frame.hits, source_generation))
                    }
                    Err(TrySendError::Disconnected(Inbox::Frame(frame))) => Published::Kept(retag(frame.hits, source_generation)),
                    Err(_) => unreachable!("a sink only sends frames"),
                }
            });
        acquisition.discard_marks();
        if nominal { acquisition.record_events(); acquisition.follow_trends(self.trend_slot.clone()); }
        acquisition.capture(self.capture_request.clone(), self.captures.clone(), generation);
        acquisition.follow_span(self.span_request.clone());
        acquisition.follow_trigger(self.trigger.clone());
        acquisition.follow_lanes(self.lane_slot.clone());
        if self.limited {
            let (_, limited) = self.limited_slot.read().expect("limited feed needs acquisition limits");
            let mut acquisition = LimitedIntake::new(acquisition, limited.bits, limited.resolved, limited.trigger, limited.pre, limited.block_period);
            acquisition.follow(self.limited_slot.clone());
            return self.sim_intake(nominal, acquisition);
        }
        self.sim_intake(nominal, acquisition)
    }

    /// The solver's sink: lane values are taken on the solver thread. The nominal acquisition then
    /// runs on the feed's intake worker, so the scope's work leaves the solver's step; ensemble
    /// members, each on its own solver worker already, keep theirs there.
    fn sim_intake(&self, nominal: bool, acquisition: impl Intake + 'static) -> Box<dyn Sink + Send> {
        if nominal {
            let mut intake = SimIntake::new(self.channels.clone(), IntakeLink::new(&self.intakes, acquisition));
            intake.follow_layouts(self.layouts.clone());
            return Box::new(intake);
        }
        let mut intake = SimIntake::new(self.channels.clone(), acquisition);
        intake.follow_layouts(self.layouts.clone());
        Box::new(intake)
    }
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    lanes: Mutex<Arc<Vec<LaneSetup>>>,
    generation: AtomicU64,
    waveforms: AtomicU64,
    samples: AtomicU64,
    frames: AtomicU64,
    dropped: AtomicU64,
    refused: AtomicU64,
    lane_waveforms: Mutex<Vec<u64>>,
    unresolved: AtomicBool,
    measurement_generation: AtomicU64,
    trigger_state: AtomicU8,
    trigger_events: AtomicU64,
    events: Mutex<Arc<EventSnapshot>>,
    /// Per-lane edge measurement thresholds and physical quantities.
    swings: Mutex<Arc<Vec<(f64, Quantity)>>>,
}

/// One coherent nominal timeline: columns use the event list's indices.
#[derive(Clone, Debug, Default)]
pub struct EventSnapshot {
    pub configuration: u64,
    pub generation: u64,
    pub list: EventList,
    pub columns: Arc<Vec<TrendSeries>>,
    pub violations: ViolationList,
    /// Changes when acquisition restarts its event indices.
    pub event_epoch: u64,
    /// Arm request and its first matching failure.
    pub stopped: Option<(u64, Violation)>,
}

/// Frequency, duty, rise and overshoot need a swing of a tenth of a division (the screen is eight):
/// below that they measure noise or ripple, as on a DC rail, which reads only its mean and Vpp.
pub(crate) fn least_swing(volts: (f64, f64)) -> f64 {
    (volts.1 - volts.0).abs() / 80.0
}

/// Non-blocking UI access to a nominal simulation and any additional members.
pub struct SimScopeFeed {
    shared: Arc<Shared>,
    sinks: ScopeSinks,
    id: Option<u64>,
    ready: Receiver<LaneFrame>,
    returns: SyncSender<Inbox>,
    pending_returns: VecDeque<LaneFrame>,
    returns_full: bool,
    measurements: Arc<Mutex<MeasureSnapshot>>,
    span_request: Arc<SpanRequest>,
}

impl SimScopeFeed {
    /// Start one merger and ask the host to install the nominal acquisition.
    pub fn show(cx: &mut Cx, probe: NetProbe, config: SimScopeConfig) -> Result<Self, SpawnError> {
        Self::start(cx, probe, config, Duration::from_millis(100), None)
    }

    pub fn show_spectrum(cx: &mut Cx, probe: NetProbe, config: SimScopeConfig, spectrum: Option<FftSettings>) -> Result<Self, SpawnError> {
        Self::start(cx, probe, config, Duration::from_millis(100), spectrum)
    }

    /// Override the measurement cadence for deterministic acquisition tests.
    #[doc(hidden)]
    pub fn show_with_capture_interval(cx: &mut Cx, probe: NetProbe, config: SimScopeConfig, capture_every: Duration) -> Result<Self, SpawnError> {
        Self::start(cx, probe, config, capture_every, None)
    }

    fn start(cx: &mut Cx, probe: NetProbe, config: SimScopeConfig, capture_every: Duration, spectrum: Option<FftSettings>) -> Result<Self, SpawnError> {
        let mut lanes = config.settings().lanes;
        lanes[0].spectrum = spectrum.map(|fft| (fft, config.plot));
        Self::start_lanes(cx, vec![SimChannel::voltage(ChanId(0), probe)], config, 0, lanes, capture_every, None)
    }

    /// Several source channels on one detector and one publication clock.
    pub fn show_lanes(cx: &mut Cx, channels: Vec<SimChannel>, config: SimScopeConfig, trigger_channel: usize, lanes: Vec<LaneSetup>, limited: Option<LimitedSpec>) -> Result<Self, SpawnError> {
        Self::start_lanes(cx, channels, config, trigger_channel, lanes, Duration::from_millis(100), limited)
    }

    fn start_lanes(cx: &mut Cx, channels: Vec<SimChannel>, config: SimScopeConfig, trigger_channel: usize, lanes: Vec<LaneSetup>, capture_every: Duration, limited: Option<LimitedSpec>) -> Result<Self, SpawnError> {
        let settings = Settings { trigger: config.trigger, qualifier: config.qualifier, zone: config.zone, trigger_channel, window: config.window, lanes: lanes.clone(), auto: config.auto };
        if !settings.valid() || channels.len() != lanes.len() || config.frame_every.is_zero() || channels.iter().any(|c| !c.probe.scale.is_finite()) {
            return Err(SpawnError::Backend("invalid simulation scope configuration".into()));
        }
        let id = next_id();
        let shared = Arc::new(Shared::default());
        *shared.lanes.lock().unwrap() = Arc::new(lanes.clone());
        shared.generation.store(id, Ordering::Relaxed);
        *shared.swings.lock().unwrap() = Arc::new(lane_swings(&lanes, &channels));
        // A stalled merger leaves each producer accumulating in its own frame. UI returns
        // share the inbox so they wake a paused merger; retain both queues' former capacity.
        let (frames, receive) = sync_channel(RETURN_SLOTS + RETURN_SLOTS);
        let (merger, ready) = Merger::with_lanes(&lanes);
        let returns = frames.clone();
        let worker = shared.clone();
        let measurements = Arc::new(Mutex::new(MeasureSnapshot::default()));
        let (captures, captured) = sync_channel(1);
        let capture_request = Arc::new(AtomicBool::new(true));
        let span_request = Arc::new(SpanRequest::default());
        let measure = MeasureWorker::with_span_request(capture_request.clone(), captured, measurements.clone(), span_request.clone(), capture_every);
        cx.spawn_worker(move || run_merger(merger, receive, worker, config.frame_every, measure))?.detach();
        let (intakes, batches) = sync_channel(intake_worker::SLOTS);
        let stopped = shared.clone();
        cx.spawn_worker(move || intake_worker::run(batches, move || stopped.stop.load(Ordering::Relaxed)))?.detach();
        let lane_slot = Arc::new(LaneSlot::new(lanes.clone()));
        let limited_slot = Arc::new(LimitedSlot::default());
        let is_limited = limited.is_some();
        if let Some(limited) = limited { limited_slot.set(limited); }
        let sinks = ScopeSinks { channels, config, limited: is_limited, limited_slot, lanes, lane_slot, trend_slot: Arc::new(TrendSlot::default()), span_request: span_request.clone(), trigger_channel, generation: id, frames, capture_request, captures, trigger: Arc::new(TriggerSlot::default()), epochs: Arc::new(Mutex::new(HashMap::new())), layouts: Arc::new(Mutex::new(SimLayouts::default())), intakes };
        // Use the same action route for show, replacement, and drop to preserve order.
        Cx::post_action(SimAction::ShowScope { id, sink: SinkSlot::new(sinks.make_intake(true)), column: finest_column(&sinks.lanes) });
        Ok(Self { shared, sinks, id: Some(id), ready, returns, pending_returns: VecDeque::new(), returns_full: false, measurements, span_request })
    }

    /// Share this configuration with hosts that install ensemble acquisitions.
    pub fn sinks(&self) -> ScopeSinks {
        self.sinks.clone()
    }

    /// Take at most one frame, retrying spent storage without waiting.
    pub fn take(&mut self) -> Option<LaneFrame> {
        self.retry_returns();
        let hits = self.ready.try_recv().ok()?;
        if self.frame_is_current(&hits) {
            Some(hits)
        } else {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            self.give_back(hits);
            None
        }
    }

    /// Return spent storage, retaining it for the next frame if the queue is full.
    pub fn give_back(&mut self, hits: LaneFrame) {
        self.pending_returns.push_back(hits);
        self.retry_returns();
    }

    fn retry_returns(&mut self) {
        while let Some(hits) = self.pending_returns.pop_front() {
            match self.returns.try_send(Inbox::Returned(hits)) {
                Ok(()) => {}
                Err(TrySendError::Full(Inbox::Returned(hits))) => {
                    if !self.returns_full {
                        makepad_widgets_core::log!("scope feed: return queue full; storage retained for the next frame");
                    }
                    self.returns_full = true;
                    self.pending_returns.push_front(hits);
                    return;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.pending_returns.clear();
                    break;
                }
                Err(TrySendError::Full(Inbox::Frame(_))) => unreachable!("the UI only sends returns"),
            }
        }
        self.returns_full = false;
    }

    /// Running intakes take these limits at their next block boundary; later intakes start here.
    pub fn set_limited(&mut self, spec: LimitedSpec) {
        if self.sinks.limited { self.sinks.limited_slot.set(spec); }
    }

    /// A changed trigger starts a new acquisition and event list. Visibility, compatible value axes and
    /// auto-mode edits preserve the detector; none of them changes which edge rule fired.
    pub fn reconfigure(&mut self, trigger: EdgeTrigger, qualifier: Qualifier, trigger_channel: usize, window: Window, lanes: Vec<LaneSetup>, auto: Option<f64>, zone: Option<super::acquire::Zone>) {
        let settings = Settings { trigger, qualifier, zone, trigger_channel, window, lanes, auto };
        assert!(settings.valid(), "invalid simulation scope settings");
        assert_eq!(settings.lanes.len(), self.sinks.channels.len(), "reconfiguration keeps the channel layout");
        let Some(old) = self.id else { return };
        let c = self.sinks.config;
        if trigger == c.trigger && qualifier == c.qualifier && zone == c.zone && window == c.window && trigger_channel == self.sinks.trigger_channel && LaneSlot::compatible(&self.sinks.lanes, &settings.lanes) {
            if self.sinks.lanes != settings.lanes {
                self.sinks.lane_slot.set(settings.lanes.clone());
                self.sinks.lanes = settings.lanes;
                let lanes = Arc::new(self.sinks.lanes.clone());
                let swings = Arc::new(lane_swings(&self.sinks.lanes, &self.sinks.channels));
                let old = std::mem::replace(&mut *self.shared.lanes.lock().unwrap(), lanes);
                drop(old);
                let old = std::mem::replace(&mut *self.shared.swings.lock().unwrap(), swings);
                drop(old);
            }
            self.sinks.config = SimScopeConfig { trigger, qualifier, zone, auto, ..c };
            if trigger != c.trigger || qualifier != c.qualifier || zone != c.zone || auto != c.auto { self.sinks.trigger.set(trigger, qualifier, auto, zone); }
            return;
        }
        let id = next_id();
        let map = settings.lanes[0].map;
        self.sinks.config = SimScopeConfig { plot: (map.w, map.h), volts: (map.v_top - map.v_px * map.h as f64, map.v_top), trigger, qualifier, zone, window, auto, frame_every: c.frame_every };
        self.sinks.lanes = settings.lanes;
        self.sinks.lane_slot = Arc::new(LaneSlot::new(self.sinks.lanes.clone()));
        self.sinks.trigger = Arc::new(TriggerSlot::default());
        self.sinks.trigger_channel = trigger_channel;
        self.sinks.generation = id;
        self.sinks.epochs = Arc::new(Mutex::new(HashMap::new()));
        *self.shared.lanes.lock().unwrap() = Arc::new(self.sinks.lanes.clone());
        self.shared.generation.store(id, Ordering::Relaxed);
        *self.shared.swings.lock().unwrap() = Arc::new(lane_swings(&self.sinks.lanes, &self.sinks.channels));
        self.shared.unresolved.store(false, Ordering::Relaxed);
        self.shared.trigger_state.store(0, Ordering::Relaxed);
        Cx::post_action(SimAction::HideScope { id: old });
        Cx::post_action(SimAction::ShowScope { id, sink: SinkSlot::new(self.sinks.make_intake(true)), column: finest_column(&self.sinks.lanes) });
        self.id = Some(id);
        self.retry_returns();
    }

    /// Latest complete window; the legacy owned API copies outside the publication lock.
    pub fn measurements(&self) -> Option<(u64, Vec<Measurement>)> {
        self.measurement_snapshot().map(|(generation, values, _, _)| (generation, (*values).clone()))
    }

    /// The stack shares one snapshot across its lanes. An unchanged answer allocates nothing.
    pub(crate) fn measurement_snapshot(&self) -> Option<(u64, Arc<Vec<Measurement>>, Arc<Vec<Columns>>, Arc<Vec<Option<SpectrumMeasurements>>>)> {
        let snapshot = self.measurements.try_lock().ok()?;
        if snapshot.generation == 0 || snapshot.configuration != self.sinks.generation
            || self.sinks.lanes.iter().any(|lane| lane.spectrum.is_some()) && snapshot.spectrum_span != self.span_request.generation() { return None; }
        Some((snapshot.generation, snapshot.values.clone(), snapshot.sparks.clone(), snapshot.spectra.clone()))
    }

    /// Latest capture features for this configuration, shared without waiting or copying the list.
    pub fn features(&self) -> Option<Arc<CaptureFeatures>> {
        let snapshot = self.measurements.try_lock().ok()?;
        (snapshot.generation != 0 && snapshot.configuration == self.sinks.generation
            && snapshot.features.configuration == self.sinks.generation).then(|| snapshot.features.clone())
    }

    /// Latest-wins bracket in trigger-relative seconds; return its request generation without waiting.
    /// The worker retains the latest capture even without a bracket, so a paused source can answer.
    pub fn set_span(&self, span: Option<(f64, f64)>) -> u64 { self.span_request.set(span) }

    /// The answered request generation (one per set_span call, starting at one) and every lane.
    /// An older generation remains visible while a new span is being measured.
    pub fn spans(&self) -> Option<(u64, Arc<Vec<Option<SpanStats>>>)> {
        let snapshot = self.measurements.try_lock().ok()?;
        (snapshot.generation != 0 && snapshot.configuration == self.sinks.generation
            && snapshot.features.configuration == self.sinks.generation).then(|| (snapshot.span_generation, snapshot.spans.clone()))
    }

    /// Single-card readout convenience; measurements() retains all lane indices.
    pub fn measurements_lane(&self, lane: usize) -> Option<(u64, Vec<Measurement>)> {
        self.measurements().map(|(generation, mut rows)| { rows.retain(|row| row.lane == lane); (generation, rows) })
    }

    /// One atomic load per stack readout tick; unchanged snapshots need no allocation.
    pub(crate) fn measurement_generation(&self) -> u64 {
        self.shared.measurement_generation.load(Ordering::Acquire)
    }

    pub fn trigger_state(&self) -> TriggerState {
        match self.shared.trigger_state.load(Ordering::Relaxed) { 1 => TriggerState::Fired, 2 => TriggerState::Automatic, _ => TriggerState::Waiting }
    }

    /// Accepted trigger events only, including fires that finished between UI frames.
    pub fn trigger_events(&self) -> u64 { self.shared.trigger_events.load(Ordering::Relaxed) }

    /// O(1) snapshot pickup; the UI never waits, traverses chunks or copies event times.
    /// Return the new configuration's empty list immediately, even before its first frame.
    pub fn events(&self) -> Option<Arc<EventSnapshot>> {
        let snapshot = self.shared.events.try_lock().ok()?;
        if snapshot.configuration != self.sinks.generation {
            return Some(Arc::new(EventSnapshot { configuration: self.sinks.generation, generation: self.sinks.generation, ..EventSnapshot::default() }));
        }
        Some(snapshot.clone())
    }

    /// Latest-wins pins, taken by the nominal worker before its next sample. Retry false on the
    /// next UI frame with the current list; a contended slot never makes the UI wait.
    pub fn set_trends(&self, pins: &[(usize, TrendQuantity)]) -> bool {
        assert!(pins.iter().all(|&(lane, _)| lane < self.sinks.channels.len()), "trend pin must name a lane");
        self.sinks.trend_slot.set(pins)
    }

    /// Bounds also record unpinned quantities. Retry a busy slot on the next UI frame.
    pub fn set_bounds(&self, bounds: &[TrendBound]) -> bool {
        assert!(bounds.iter().all(|bound| bound.pin.0 < self.sinks.channels.len()), "trend bound must name a lane");
        self.sinks.trend_slot.set_bounds(bounds)
    }

    /// Each arm starts after the worker's accepted-event baseline.
    pub fn arm_stop_on_violation(&self, bounds: &[usize]) -> bool { self.sinks.trend_slot.arm_stop_on_violation(bounds) }

    pub fn violation_stop(&self, snapshot: &EventSnapshot) -> Option<(u64, f64)> {
        let (request, violation) = snapshot.stopped?;
        if snapshot.configuration != self.sinks.generation || Some(request) != self.sinks.trend_slot.armed_request() { return None; }
        Some((request, snapshot.list.get(violation.event)?))
    }

    /// Read worker totals without waiting.
    pub fn stats(&self) -> FeedStats {
        FeedStats {
            waveforms: self.shared.waveforms.load(Ordering::Relaxed),
            trigger_events: self.shared.trigger_events.load(Ordering::Relaxed),
            sim_time_ns: 0,
            samples: self.shared.samples.load(Ordering::Relaxed),
            frames: self.shared.frames.load(Ordering::Relaxed),
            dropped: self.shared.dropped.load(Ordering::Relaxed),
            refused: self.shared.refused.load(Ordering::Relaxed),
            unresolved: self.shared.unresolved.load(Ordering::Relaxed),
        }
    }

    /// Copy cumulative rastered waveforms per lane; sampled by the perf report once a second.
    pub fn lane_waveforms(&self, out: &mut Vec<u64>) {
        let counts = self.shared.lane_waveforms.lock().unwrap();
        out.clear();
        out.extend_from_slice(&counts);
    }

    /// Remove the nominal sink and stop the merger at its next wakeup.
    pub fn stop(&mut self) {
        if let Some(id) = self.id.take() {
            Cx::post_action(SimAction::HideScope { id });
        }
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for SimScopeFeed {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ScopeFeed for SimScopeFeed {
    fn frame_is_current(&self, frame: &LaneFrame) -> bool {
        same_shape(frame, &self.sinks.lanes) && self.generation_is_current(frame.generation)
    }

    fn generation_is_current(&self, generation: u64) -> bool {
        let span = if self.sinks.lanes.iter().any(|lane| lane.spectrum.is_some()) { self.span_request.generation() } else { 0 };
        self.sinks.epochs.lock().unwrap().iter().any(|(&(_, gate), &tag)| gate == span && tag == generation)
    }

    fn take(&mut self) -> Option<LaneFrame> {
        SimScopeFeed::take(self)
    }

    fn give_back(&mut self, hits: LaneFrame) {
        SimScopeFeed::give_back(self, hits);
    }

    fn reconfigure(&mut self, trigger: EdgeTrigger, qualifier: Qualifier, trigger_channel: usize, window: Window, lanes: Vec<LaneSetup>, auto: Option<f64>, zone: Option<super::acquire::Zone>) {
        SimScopeFeed::reconfigure(self, trigger, qualifier, trigger_channel, window, lanes, auto, zone);
    }

    fn stats(&self) -> FeedStats {
        SimScopeFeed::stats(self)
    }

    fn trigger_state(&self) -> TriggerState { SimScopeFeed::trigger_state(self) }

    fn stop(&mut self) {
        SimScopeFeed::stop(self);
    }
}

struct ReturnFrame {
    send: SyncSender<LaneFrame>,
    hits: LaneFrame,
}

#[derive(Default)]
pub(super) struct MeasureSnapshot {
    pub(super) generation: u64,
    pub(super) configuration: u64,
    // Only the Arc is copied under the lock; strings are cloned by the reader
    // afterwards, and the worker drops the replaced vector outside the lock.
    pub(super) values: Arc<Vec<Measurement>>,
    sparks: Arc<Vec<Columns>>,
    spectra: Arc<Vec<Option<SpectrumMeasurements>>>,
    spectrum_span: u64,
    pub(super) features: Arc<CaptureFeatures>,
    pub(super) span_generation: u64,
    pub(super) spans: Arc<Vec<Option<SpanStats>>>,
}

struct Spectra { configuration: u64, span: u64, values: Vec<SpectrumResult> }

pub(super) struct MeasureWorker {
    request: Arc<AtomicBool>,
    captures: Receiver<Capture>,
    snapshot: Arc<Mutex<MeasureSnapshot>>,
    every: Duration,
    last_request: Instant,
    spectrum: Option<Spectra>,
    spectrum_dirty: bool,
    time_rows: Vec<Measurement>,
    time_configuration: u64,
    sparks: Arc<Vec<Columns>>,
    features: Arc<CaptureFeatures>,
    span_request: Arc<SpanRequest>,
    span_generation: u64,
    spans: Arc<Vec<Option<SpanStats>>>,
    span_buffer: Vec<Option<SpanStats>>,
    // Keep the latest current-configuration samples even with no span. Return them on
    // replacement, configuration change or stop; a first bracket can measure a paused source.
    last_capture: Option<Capture>,
}

impl MeasureWorker {
    pub(super) fn new(request: Arc<AtomicBool>, captures: Receiver<Capture>, snapshot: Arc<Mutex<MeasureSnapshot>>, every: Duration) -> Self {
        Self::with_span_request(request, captures, snapshot, Arc::new(SpanRequest::default()), every)
    }

    pub(super) fn with_span_request(request: Arc<AtomicBool>, captures: Receiver<Capture>, snapshot: Arc<Mutex<MeasureSnapshot>>, span_request: Arc<SpanRequest>, every: Duration) -> Self {
        Self { request, captures, snapshot, every, last_request: Instant::now(), spectrum: None, spectrum_dirty: false, time_rows: Vec::new(), time_configuration: 0, sparks: Arc::default(), features: Arc::default(),
            span_request, span_generation: 0, spans: Arc::default(), span_buffer: Vec::new(), last_capture: None }
    }

    fn publish(&mut self, counter: &AtomicU64, configuration: u64, values: Vec<Measurement>) {
        let generation = next_id();
        let spectra = self.spectrum.as_ref().filter(|s| s.configuration == configuration).map_or_else(Arc::default, |s| Arc::new(s.values.iter().map(|s| s.measurements).collect()));
        let snapshot = MeasureSnapshot { generation, configuration, values: Arc::new(values),
            sparks: if configuration == self.time_configuration { self.sparks.clone() } else { Arc::default() },
            spectra, spectrum_span: self.spectrum.as_ref().filter(|s| s.configuration == configuration).map_or_else(|| self.span_request.generation(), |s| s.span),
            features: if configuration == self.time_configuration { self.features.clone() } else { Arc::default() },
            span_generation: self.span_generation,
            spans: if configuration == self.time_configuration { self.spans.clone() } else { Arc::default() } };
        let old = std::mem::replace(&mut *self.snapshot.lock().unwrap(), snapshot);
        drop(old);
        counter.store(generation, Ordering::Release);
        self.spectrum_dirty = false;
        SignalToUI::set_ui_signal();
    }

    fn until_request(&self) -> Duration {
        if self.every.is_zero() {
            // Tests request at each merge iteration without making the idle
            // worker spin while it waits for a solver frame.
            Duration::MAX
        } else {
            self.every.saturating_sub(self.last_request.elapsed())
        }
    }

    fn poll(&mut self, shared: &Shared) {
        if self.spectrum.as_ref().is_some_and(|s| s.span != self.span_request.generation()) { self.spectrum = None; self.spectrum_dirty = false; }
        let now = Instant::now();
        if now.duration_since(self.last_request) >= self.every {
            self.request.store(true, Ordering::Relaxed);
            self.last_request = now;
            if let Some(spectrum) = self.spectrum.as_ref().filter(|s| self.spectrum_dirty && s.configuration == shared.generation.load(Ordering::Relaxed)) {
                let mut rows = if spectrum.configuration == self.time_configuration { self.time_rows.clone() } else { Vec::new() };
                rows.extend(spectrum_rows(spectrum));
                self.publish(&shared.measurement_generation, spectrum.configuration, rows);
            }
        }
        let configuration = shared.generation.load(Ordering::Relaxed);
        if let Ok(capture) = self.captures.try_recv() {
            let swings = shared.swings.lock().unwrap().clone();
            let lanes = shared.lanes.lock().unwrap().clone();
            self.consume(capture, configuration, &swings, &lanes, &shared.measurement_generation);
        }
        self.poll_span(configuration, &shared.measurement_generation);
    }

    /// Used by source workers that already own acquisition; no new thread or UI work is needed.
    pub(super) fn poll_source(&mut self, configuration: u64, swings: &[(f64, Quantity)], lanes: &[LaneSetup], counter: &AtomicU64) {
        if self.last_request.elapsed() >= self.every {
            self.request.store(true, Ordering::Relaxed);
            self.last_request = Instant::now();
        }
        if let Ok(capture) = self.captures.try_recv() { self.consume(capture, configuration, swings, lanes, counter); }
        self.poll_span(configuration, counter);
    }

    fn update_spans(&mut self, capture: Option<&Capture>, generation: u64, span: Option<(f64, f64)>) {
        self.span_buffer.clear();
        for (lane, features) in self.features.lanes.iter().enumerate() {
            let stats = span.and_then(|(t0, t1)| {
                let capture = capture?;
                // A valid lane always has at least one stretch or transition feature. Empty
                // features mark a record rejected by record_index for bad values or times.
                if features.is_empty() { return None; }
                span_stats(&capture.samples.times, capture.samples.lanes.get(lane)?, capture.trigger, features, t0, t1)
            });
            self.span_buffer.push(stats);
        }
        // Reuse the calculation buffer; only the immutable published Arc<Vec<_>> allocates.
        if self.span_buffer != *self.spans { self.spans = Arc::new(self.span_buffer.clone()); }
        self.span_generation = generation;
    }

    fn poll_span(&mut self, configuration: u64, counter: &AtomicU64) {
        let (generation, span) = self.span_request.get();
        if self.last_capture.as_ref().is_some_and(|capture| capture.configuration != configuration) {
            if let Some(capture) = self.last_capture.take() { Self::return_capture(capture); }
        }
        if generation == self.span_generation || configuration != self.time_configuration { return; }
        if span.is_some() && self.last_capture.is_none() {
            // No capture has arrived for this configuration yet.
            self.request.store(true, Ordering::Relaxed);
            return;
        }
        let capture = self.last_capture.take();
        self.update_spans(capture.as_ref(), generation, span);
        self.last_capture = capture;
        // A paused bracket edit publishes only its answer; measurement strings, features and
        // sparks remain shared from that capture. No sample scan or final payload drop under lock.
        let publication = next_id();
        let old = {
            let mut snapshot = self.snapshot.lock().unwrap();
            if snapshot.configuration != configuration { return; }
            snapshot.generation = publication;
            snapshot.span_generation = self.span_generation;
            std::mem::replace(&mut snapshot.spans, self.spans.clone())
        };
        drop(old);
        counter.store(publication, Ordering::Release);
        SignalToUI::set_ui_signal();
    }

    fn consume(&mut self, capture: Capture, configuration: u64, swings: &[(f64, Quantity)], lanes: &[LaneSetup], counter: &AtomicU64) {
        if capture.configuration == configuration {
            if let Some(previous) = self.last_capture.take() { Self::return_capture(previous); }
            let t = &capture.samples.times;
            let mut values = Vec::new();
            let mut features = CaptureFeatures { configuration, lanes: Vec::with_capacity(capture.samples.lanes.len()) };
            let held = capture.samples.marks.iter().any(|&(_, kind)| kind == Mark::Held);
            for (lane, y) in capture.samples.lanes.iter().enumerate() {
                let Some(index) = record_index(t, y) else { features.lanes.push(Vec::new()); continue };
                features.lanes.push(lane_features(t, y, &index, capture.trigger));
                let Some(&(swing, quantity)) = swings.get(lane) else { continue };
                if lanes.get(lane).is_none() { continue; }
                let mut rows = indexed_rows(t, y, swing, &index);
                for row in &mut rows {
                    row.lane = lane;
                    if quantity == Quantity::Amp && row.unit == "V" { row.unit = "A".into(); if row.name == "Vpp" { row.name = "Ipp".into(); } }
                }
                if held { rows.push(Measurement::exact(lane, "held", 1.0, "")); }
                values.extend(rows);
            }
            self.features = Arc::new(features);
            // The spark travels beside the answers from this exact capture. Reduction is on the
            // worker and bounded to 48 columns per lane; the UI never reads raw capture samples.
            if let (Some(&first), Some(&last)) = (t.first(), t.last()) {
                self.sparks = Arc::new(capture.samples.lanes.iter().map(|y| {
                    let mut columns = Columns::default();
                    columns.reduce_into(t, y, first, last.next_up(), SPARK_COLS);
                    columns
                }).collect());
            }
            self.time_rows = values.clone();
            self.time_configuration = capture.configuration;
            let (generation, span) = self.span_request.get();
            self.update_spans(Some(&capture), generation, span);
            if let Some(spectra) = &self.spectrum {
                if spectra.configuration == capture.configuration && spectra.span == self.span_request.generation() {
                    values.extend(spectrum_rows(spectra));
                }
            }
            self.publish(counter, capture.configuration, values);
            self.last_capture = Some(capture);
            return;
        }
        Self::return_capture(capture);
    }

    fn return_capture(capture: Capture) {
        // A full return slot already holds a reusable pair for this sink. Any
        // excess storage is released here, away from the solver and UI threads.
        let _ = capture.returned.try_send(capture.samples);
    }
}

impl Drop for MeasureWorker {
    fn drop(&mut self) {
        if let Some(capture) = self.last_capture.take() { Self::return_capture(capture); }
    }
}

/// A recurring condition's log line: one when an episode starts, then at most one a second with the
/// episodes since the last line, so a queue that fills and drains every few frames stays readable.
struct EpisodeLog {
    what: &'static str,
    last: Option<Instant>,
    since: u32,
}

impl EpisodeLog {
    fn new(what: &'static str) -> Self {
        EpisodeLog { what, last: None, since: 0 }
    }

    fn begin(&mut self) {
        self.since += 1;
        if self.last.is_none_or(|last| last.elapsed() >= Duration::from_secs(1)) {
            match self.since {
                1 => makepad_widgets_core::log!("{}", self.what),
                n => makepad_widgets_core::log!("{} ({n} times since the last report)", self.what),
            }
            self.last = Some(Instant::now());
            self.since = 0;
        }
    }
}

fn run_merger(mut merger: Merger, inbox: Receiver<Inbox>, shared: Arc<Shared>, frame_every: Duration, mut measure: MeasureWorker) {
    let mut pending = VecDeque::<ReturnFrame>::new();
    let mut probes = HashMap::<u64, (u64, Weak<()>, bool)>::new();
    let mut counted_drops = 0;
    let mut counted_waveforms = Vec::new();
    let mut returns_full = false;
    let mut returns_log = EpisodeLog::new("scope feed: sink return queue full; storage retained for retry");
    while !shared.stop.load(Ordering::Relaxed) {
        measure.poll(&shared);
        for _ in 0..pending.len() {
            let Some(ReturnFrame { send, hits }) = pending.pop_front() else { break };
            match send.try_send(hits) {
                Ok(()) => {}
                Err(TrySendError::Full(hits)) => pending.push_back(ReturnFrame { send, hits }),
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
        if !pending.is_empty() && !returns_full {
            returns_log.begin();
        }
        returns_full = !pending.is_empty();
        let generation = shared.generation.load(Ordering::Relaxed);
        probes.retain(|_, (installed, alive, _)| *installed == generation && alive.strong_count() > 0);
        shared.unresolved.store(probes.values().any(|(_, _, unresolved)| *unresolved), Ordering::Relaxed);
        shared.dropped.fetch_add(merger.dropped() - counted_drops, Ordering::Relaxed);
        counted_drops = merger.dropped();

        // A UI return wakes publication immediately even when the solver has paused.
        match inbox.recv_timeout(frame_every.min(measure.until_request())) {
            Ok(Inbox::Returned(hits)) => merger.recycle(hits),
            Ok(Inbox::Frame(frame)) => {
                let lanes = shared.lanes.lock().unwrap().clone();
                merger.set_lanes(&lanes);
                shared.samples.fetch_add(frame.stats.samples, Ordering::Relaxed);
                shared.dropped.fetch_add(frame.stats.dropped, Ordering::Relaxed);
                shared.waveforms.fetch_add(frame.hits.waveforms(), Ordering::Relaxed);
                probes.insert(frame.sink, (frame.generation, frame.alive, frame.stats.unresolved));
                // Accepted source times are independent of the FFT gate; a stale gate can
                // reject its picture below without losing the nominal trigger snapshot.
                if frame.generation == shared.generation.load(Ordering::Relaxed) {
                    if let Some(list) = frame.stats.events {
                        let mut snapshot = shared.events.lock().unwrap();
                        let columns = frame.stats.columns;
                        let same_columns = snapshot.columns.len() == columns.len() && snapshot.columns.iter().zip(columns.iter())
                            .all(|(a, b)| a.lane == b.lane && a.quantity == b.quantity && a.values.same(&b.values));
                        let violations = frame.stats.violations;
                        let event_epoch = frame.stats.event_epoch;
                        let stopped = frame.stats.stopped;
                        if snapshot.configuration != frame.generation || snapshot.event_epoch != event_epoch || !snapshot.list.same(&list) || !same_columns || !snapshot.violations.same(&violations) || snapshot.stopped != stopped {
                            let old = std::mem::replace(&mut *snapshot, Arc::new(EventSnapshot { configuration: frame.generation, generation: next_id(), list, columns, violations, event_epoch, stopped }));
                            drop(snapshot);
                            drop(old);
                            SignalToUI::set_ui_signal();
                        }
                    }
                }
                let hits = if frame.generation == shared.generation.load(Ordering::Relaxed)
                    && (lanes.iter().all(|lane| lane.spectrum.is_none()) || frame.stats.spectrum_span == measure.span_request.generation()) {
                    shared.trigger_events.fetch_add(frame.stats.trigger_events, Ordering::Relaxed);
                    shared.trigger_state.store(match frame.stats.trigger_state { TriggerState::Waiting => 0, TriggerState::Fired => 1, TriggerState::Automatic => 2 }, Ordering::Relaxed);
                    // Measurements also complete when every card is hidden and no raster finishes.
                    if frame.stats.spectrum_updated && frame.stats.spectrum.iter().any(|s| s.measurements.is_some() || s.gate_too_short) {
                        measure.spectrum = Some(Spectra { configuration: frame.generation, span: frame.stats.spectrum_span, values: frame.stats.spectrum });
                        measure.spectrum_dirty = true;
                    }
                    merger.add(frame.hits)
                } else { frame.hits };
                pending.push_back(ReturnFrame { send: frame.returned, hits });
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let lanes = shared.lanes.lock().unwrap().clone();
        merger.set_lanes(&lanes);
        if merger.publish() {
            shared.frames.fetch_add(1, Ordering::Relaxed);
        }
        shared.refused.store(merger.refused(), Ordering::Relaxed);
        if counted_waveforms.as_slice() != merger.lane_waveforms() {
            counted_waveforms.clear();
            counted_waveforms.extend_from_slice(merger.lane_waveforms());
            let mut counts = shared.lane_waveforms.lock().unwrap();
            counts.clear();
            counts.extend_from_slice(&counted_waveforms);
        }
    }
    shared.dropped.fetch_add(merger.dropped() - counted_drops, Ordering::Relaxed);
}

/// A frame tagged with `generation`, on the frame and every lane: the merger never sums
/// frames of different tags. A frame kept for the next flush gets its source generation back.
fn retag(mut hits: LaneFrame, generation: u64) -> LaneFrame {
    hits.generation = generation;
    for lane in hits.pictures_mut() {
        lane.generation = generation;
    }
    hits
}

fn lane_swings(lanes: &[LaneSetup], channels: &[SimChannel]) -> Vec<(f64, Quantity)> {
    lanes.iter().zip(channels).map(|(lane, channel)| (least_swing((lane.map.v_top - lane.map.v_px * lane.map.h as f64, lane.map.v_top)), channel.info.quantity)).collect()
}

fn spectrum_rows(spectra: &Spectra) -> Vec<Measurement> {
    spectra.values.iter().enumerate().flat_map(|(lane, spectrum)|
        spectrum.rows().into_iter().map(move |mut row| { row.lane = lane; row })).collect()
}
