//! Dedicated acquisition workers and a single-slot phosphor mailbox.

use super::acquire::{AcquireIntake, AcquireStats, Acquisition, LaneFrame, LaneSetup, Published, TriggerState};
use fab_elec_bench::source::{ChanId, ChannelInfo, ClockKind, Intake, Layout, Quantity};
use super::phosphor::PhosphorMap;
use fab_elec_instr::{Signal, synth::Spread};
use super::spectrum::{SpectrumMeasurements, SpectrumResult};
use super::simfeed::{CaptureFeatures, MeasureSnapshot, MeasureWorker, least_swing};
use super::span::{SpanRequest, SpanStats};
use fab_elec_bench::trigger::{EdgeTrigger, Qualifier, Slope, Window};
use makepad_widgets_core::{Cx, SignalToUI, SpawnError};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SAMPLE_BATCH: usize = 64;
pub(super) const RETURN_SLOTS: usize = 2;

/// Source, acquisition, and timing settings for all members.
#[derive(Clone, Debug)]
pub struct FeedConfig {
    pub members: usize,
    pub source: Signal,
    pub spread: Spread,
    pub record_periods: usize,
    pub trigger: EdgeTrigger,
    pub window: Window,
    pub auto: Option<f64>,
    pub plot: (u32, u32),
    pub volts: (f64, f64),
    pub frame_every: Duration,
    /// Simulated seconds per wall second, or no limit.
    pub pace: Option<f64>,
}

/// Totals published by the workers at frame boundaries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedStats {
    pub waveforms: u64,
    /// Accepted trigger events; automatic free-running sweeps are excluded.
    pub trigger_events: u64,
    /// Nominal synthetic source clock in nanoseconds, for measured simulation/wall ratios.
    /// Zero when a feed's simulation ratio is supplied by its host instead.
    pub sim_time_ns: u64,
    pub samples: u64,
    /// Merged frames published to the UI mailbox.
    pub frames: u64,
    /// Stale frames and spent buffers that could not be returned.
    pub dropped: u64,
    /// Publications refused by a full UI mailbox; their sums are retained.
    pub refused: u64,
    /// At least one active simulation probe has an unknown net name.
    pub unresolved: bool,
}

/// Non-blocking acquisition and buffer recycling for a scope card.
pub trait ScopeFeed {
    fn take(&mut self) -> Option<LaneFrame>;
    /// Also filter frames retained by a busy view across a configuration or bracket edit.
    fn frame_is_current(&self, _frame: &LaneFrame) -> bool { true }
    /// A partially delivered envelope contains spent buffers of older sizes; only its epoch
    /// can be checked here. Each view still checks the size of its own pending picture.
    fn generation_is_current(&self, _generation: u64) -> bool { true }
    fn give_back(&mut self, hits: LaneFrame);
    fn reconfigure(&mut self, trigger: EdgeTrigger, qualifier: Qualifier, trigger_channel: usize, window: Window, lanes: Vec<LaneSetup>, auto: Option<f64>, zone: Option<super::acquire::Zone>);
    fn reconfigure_single(&mut self, plot: (u32, u32), volts: (f64, f64), trigger: EdgeTrigger, window: Window, auto: Option<f64>) {
        self.reconfigure(trigger, Qualifier::None, 0, window, vec![LaneSetup::time(PhosphorMap::screen(plot.0, plot.1, window.pre, window.pre + window.post, volts.0, volts.1))], auto, None);
    }
    fn reconfigure_scope(&mut self, plot: (u32, u32), settings: &super::scope::ScopeSettings) {
        self.reconfigure(settings.trigger(), settings.trigger.qualify, 0, settings.window(), vec![LaneSetup { map: settings.map(plot), spectrum: settings.spectrum().map(|fft| (fft, plot)), drawn: true }], settings.auto_after(), settings.trigger.zone);
    }
    fn stats(&self) -> FeedStats;
    fn trigger_state(&self) -> TriggerState { TriggerState::Waiting }
    fn stop(&mut self);
}

#[derive(Clone)]
pub(super) struct Settings {
    pub trigger: EdgeTrigger,
    pub qualifier: Qualifier,
    pub zone: Option<super::acquire::Zone>,
    pub trigger_channel: usize,
    pub window: Window,
    pub lanes: Vec<LaneSetup>,
    pub auto: Option<f64>,
}

impl Settings {
    pub(super) fn single(plot: (u32, u32), volts: (f64, f64), trigger: EdgeTrigger, window: Window, auto: Option<f64>) -> Self {
        Self { trigger, qualifier: Qualifier::None, zone: None, trigger_channel: 0, window, auto, lanes: vec![LaneSetup::time(PhosphorMap::screen(plot.0, plot.1, window.pre, window.pre + window.post, volts.0, volts.1))] }
    }
    pub(super) fn valid(&self) -> bool {
        self.trigger_channel < self.lanes.len()
            && fab_elec_bench::trigger::validate_qualifier(self.qualifier)
            && self.auto.is_none_or(|after| after.is_finite() && after >= 0.0)
            && self.window.pre.is_finite() && self.window.pre >= 0.0
            && self.window.post.is_finite() && self.window.post >= 0.0
            && (self.window.pre + self.window.post).is_finite() && self.window.pre + self.window.post > 0.0
            && self.trigger.level.is_finite() && self.trigger.hyst.is_finite() && self.trigger.hyst >= 0.0
            && self.trigger.holdoff.is_finite() && self.trigger.holdoff >= 0.0
            && self.lanes.iter().all(|lane| {
                let m = lane.map;
                m.v_top.is_finite() && m.v_px.is_finite() && m.v_px > 0.0 && m.t_left.is_finite() && m.t_px.is_finite() && m.t_px > 0.0
                    && (m.w as usize).checked_mul(m.h as usize).is_some_and(|n| n <= isize::MAX as usize / size_of::<f32>())
            })
    }
}

impl FeedConfig {
    fn settings(&self) -> Settings {
        Settings::single(self.plot, self.volts, self.trigger, self.window, self.auto)
    }

    fn valid(&self) -> bool {
        self.settings().valid()
            && self.members > 0
            && self.source.seed().checked_add(self.members as u64).is_some()
            && self.record_periods > 0
            && self.record_periods.checked_mul(256).is_some()
            && !self.frame_every.is_zero()
            && self.pace.is_none_or(|rate| rate.is_finite() && rate > 0.0)
            && self.source.valid(self.spread)
            // Orders 23 and 31 repeat after millions of bits, too long to pre-generate a whole pattern.
            && !matches!(&self.source, Signal::Prbs(prbs) if !matches!(prbs.order, 7 | 9 | 15))
            && (self.record_periods as f64 * self.source.period()).is_finite()
    }
}

/// A deterministic member that can run without a thread runtime.
pub struct Member {
    intake: AcquireIntake<fn(LaneFrame, AcquireStats) -> Published>,
    values_at: Vec<f32>,
    settings: Settings,
    resize: Option<Settings>,
    times: Vec<f64>,
    values: Vec<f32>,
    span: f64,
    pass: u64,
    cursor: usize,
    time: f64,
}

impl Member {
    /// Pre-generate whole source periods with independent member variation.
    pub fn new(config: &FeedConfig, index: usize) -> Self {
        assert!(config.valid(), "invalid feed configuration");
        assert!(index < config.members, "member index is out of range");
        let source = config.source.member(config.spread, index as u64);
        let span = config.record_periods as f64 * source.period();
        // record() takes a sample count, not a duration. Grow only at startup.
        let mut count = config.record_periods * 256;
        let (times, values) = loop {
            let (mut times, mut values) = source.record(count);
            if times.last().is_some_and(|&last| last >= span) {
                let end = times.partition_point(|&time| time < span);
                times.truncate(end);
                values.truncate(end);
                break (times, values);
            }
            count = count.checked_mul(2).expect("source record is too large");
        };
        let settings = config.settings();
        let mut acquisition = Acquisition::new(settings.trigger, settings.trigger_channel, settings.window, settings.lanes.clone()).with_qualifier(settings.qualifier);
        acquisition.set_auto(settings.auto);
        acquisition.set_zone(settings.zone);
        let mut intake = AcquireIntake::new(acquisition, Duration::MAX, keep_frame as fn(LaneFrame, AcquireStats) -> Published);
        intake.discard_marks();
        intake.begin(&synthetic_layout(0, settings.lanes.len()));
        Self { intake, values_at: vec![0.0; settings.lanes.len()], settings, resize: None, times, values, span, pass: 0, cursor: 0, time: 0.0 }
    }

    /// Samples in one replay, excluding the next period's first sample.
    pub fn record_len(&self) -> usize {
        self.times.len()
    }

    /// Replay accepted samples into the caller's current frame.
    pub fn run_for(&mut self, samples: usize, hits: &mut LaneFrame) {
        assert!(same_shape(hits, &self.settings.lanes), "member and frame dimensions must match");
        std::mem::swap(&mut self.intake.frame, hits);
        for _ in 0..samples {
            self.time = self.pass as f64 * self.span + self.times[self.cursor];
            self.values_at.fill(self.values[self.cursor]);
            self.intake.point(self.time, &self.values_at);
            self.cursor += 1;
            if self.cursor == self.times.len() {
                self.cursor = 0;
                self.pass += 1;
            }
        }
        std::mem::swap(&mut self.intake.frame, hits);
        if self.settings.lanes.iter().any(|lane| lane.spectrum.is_some()) {
            hits.generation = self.intake.span_generation;
            let generation = hits.generation;
            for lane in hits.pictures_mut() { lane.generation = generation; }
        }
    }

    fn configure(&mut self, settings: Settings) {
        if settings.lanes.len() == self.settings.lanes.len() && settings.lanes.iter().zip(&self.settings.lanes).all(|(a, b)| (a.map.w, a.map.h) == (b.map.w, b.map.h) && a.spectrum.map(|s| s.1) == b.spectrum.map(|s| s.1)) {
            self.intake.configure(settings.trigger, settings.qualifier, settings.trigger_channel, settings.window, settings.lanes.clone(), settings.auto, settings.zone);
            self.settings = settings;
            self.resize = None;
        } else {
            // Accepted strokes must finish into the old size. No finite f32
            // sample can cross this level, so no new strokes can start.
            let quiet = EdgeTrigger { level: f64::MAX, hyst: 0.0, slope: Slope::Rising, holdoff: 0.0 };
            self.intake.acquisition.set_trigger(quiet);
            self.intake.acquisition.set_auto(None);
            self.resize = Some(settings);
        }
    }

    fn finish_resize(&mut self, hits: &mut LaneFrame) {
        if self.intake.acquisition.open_strokes() == 0 {
            if let Some(settings) = self.resize.take() {
                self.intake.configure(settings.trigger, settings.qualifier, settings.trigger_channel, settings.window, settings.lanes.clone(), settings.auto, settings.zone);
                self.values_at.resize(settings.lanes.len(), 0.0);
                self.settings = settings;
                hits.reset(&self.settings.lanes, hits.generation);
            }
        }
    }
}

/// Sums member frames and publishes through a bounded single-slot mailbox.
pub struct Merger {
    next: LaneFrame,
    spares: Vec<LaneFrame>,
    pending: bool,
    ready: SyncSender<LaneFrame>,
    dropped: u64,
    refused: u64,
    lane_waveforms: Vec<u64>,
}

impl Merger {
    /// Create reusable storage and the UI's non-blocking receiver for one screen-sized lane.
    /// Member indices are zero-based; the nominal is a separate sink.
    pub fn new(plot: (u32, u32)) -> (Self, Receiver<LaneFrame>) {
        let lanes = [LaneSetup::time(PhosphorMap::screen(plot.0, plot.1, 0.0, 1.0, 0.0, 1.0))];
        Self::with_lanes(&lanes)
    }

    /// Create reusable storage for these lanes and the UI's non-blocking receiver.
    pub fn with_lanes(lanes: &[LaneSetup]) -> (Self, Receiver<LaneFrame>) {
        let (send, receive) = sync_channel(1);
        (Self::with_sender(lanes, send), receive)
    }

    fn with_sender(lanes: &[LaneSetup], ready: SyncSender<LaneFrame>) -> Self {
        Self { next: LaneFrame::new(lanes), spares: (0..RETURN_SLOTS).map(|_| LaneFrame::new(lanes)).collect(), pending: false, ready, dropped: 0, refused: 0, lane_waveforms: vec![0; lanes.len()] }
    }

    /// Sum a matching frame lane by lane and return its storage to its member. Frames of
    /// different generations are never summed: a new generation discards the pending sum.
    pub fn add(&mut self, frame: LaneFrame) -> LaneFrame {
        if frame.same_shape(&self.next) {
            if self.next.generation != frame.generation {
                self.dropped += u64::from(self.pending);
                self.next.clear();
                self.next.generation = frame.generation;
                for lane in self.next.pictures_mut() {
                    lane.generation = frame.generation;
                }
            }
            self.next.add(&frame);
            for (count, lane) in self.lane_waveforms.iter_mut().zip(&frame.lanes) { *count += lane.waveforms; }
            self.pending = true;
        } else {
            self.dropped += 1;
        }
        frame
    }

    /// Publish once, retaining the sum when the mailbox or spare storage is unavailable.
    pub fn publish(&mut self) -> bool {
        if !self.pending {
            return false;
        }
        let Some(spare) = self.spares.pop() else { return false };
        let mut frame = std::mem::replace(&mut self.next, spare);
        // Stamp before ownership transfers; a refused attempt clears it for the next retry.
        frame.published = Some(Instant::now());
        match self.ready.try_send(frame) {
            Ok(()) => {
                SignalToUI::set_ui_signal();
                self.next.clear();
                self.pending = false;
                true
            }
            Err(error) => {
                let mut frame = match error {
                    TrySendError::Full(frame) => { self.refused += 1; frame }
                    TrySendError::Disconnected(frame) => frame,
                };
                frame.published = None;
                self.spares.push(std::mem::replace(&mut self.next, frame));
                false
            }
        }
    }

    /// Reuse a spent UI buffer; excess returns are counted and discarded.
    pub fn recycle(&mut self, mut frame: LaneFrame) {
        frame.published = None;
        if self.spares.len() < RETURN_SLOTS {
            frame.lanes.resize_with(self.next.lanes.len(), || super::phosphor::Hits::new(0, 0));
            for (lane, next) in frame.lanes.iter_mut().zip(&self.next.lanes) {
                lane.reset(next.w, next.h);
            }
            frame.spectra.resize_with(self.next.spectra.len(), || None);
            for (lane, next) in frame.spectra.iter_mut().zip(&self.next.spectra) {
                if let Some(next) = next { lane.get_or_insert_with(|| super::phosphor::Hits::new(next.w, next.h)).reset(next.w, next.h); }
                else { *lane = None; }
            }
            self.spares.push(frame);
        } else {
            self.dropped += 1;
        }
    }

    /// Discard a pending sum of another shape and resize the reusable storage.
    pub fn set_lanes(&mut self, lanes: &[LaneSetup]) {
        self.lane_waveforms.resize(lanes.len(), 0);
        if same_shape(&self.next, lanes) {
            return;
        }
        self.dropped += u64::from(self.pending);
        self.pending = false;
        self.next.reset(lanes, 0);
        for spare in &mut self.spares {
            spare.reset(lanes, 0);
        }
    }

    /// `set_lanes` for one screen-sized lane.
    pub fn set_plot(&mut self, plot: (u32, u32)) {
        self.set_lanes(&[LaneSetup::time(PhosphorMap::screen(plot.0, plot.1, 0.0, 1.0, 0.0, 1.0))]);
    }

    /// Stale frames and excess spent buffers discarded by this merger.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn refused(&self) -> u64 { self.refused }
    pub fn lane_waveforms(&self) -> &[u64] { &self.lane_waveforms }
}

/// Whether a frame has these lanes' pixel sizes.
pub(super) fn same_shape(frame: &LaneFrame, lanes: &[LaneSetup]) -> bool {
    frame.lanes.len() == lanes.len() && frame.lanes.iter().zip(lanes).all(|(a, b)| (a.w, a.h) == (b.map.w, b.map.h))
        && frame.spectra.len() == lanes.len() && frame.spectra.iter().zip(lanes).all(|(a, b)| a.as_ref().map(|h| (h.w, h.h)) == b.spectrum.map(|s| s.1))
}

fn keep_frame(frame: LaneFrame, _: AcquireStats) -> Published { Published::Kept(frame) }
fn synthetic_layout(generation: u64, lanes: usize) -> Layout {
    Layout { generation, clock: ClockKind::Simulated, channels: (0..lanes).map(|c| ChannelInfo { id: ChanId(c as u32), name: format!("source {c}"), quantity: Quantity::Volt }).collect() }
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    trigger_state: AtomicU8,
    spectrum: Mutex<(u64, SpectrumResult)>,
    measurements: Arc<Mutex<MeasureSnapshot>>,
    span_request: Arc<SpanRequest>,
    configuration: AtomicU64,
    measurement_generation: AtomicU64,
    lanes: Mutex<Arc<Vec<LaneSetup>>>,
    waveforms: AtomicU64,
    trigger_events: AtomicU64,
    sim_time_ns: AtomicU64,
    samples: AtomicU64,
    frames: AtomicU64,
    dropped: AtomicU64,
    refused: AtomicU64,
}

struct Commands {
    send: SyncSender<(u64, Settings)>,
    pending: Option<(u64, Settings)>,
}

impl Commands {
    fn retry(&mut self) -> bool {
        let Some(settings) = self.pending.take() else { return true };
        match self.send.try_send(settings) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => true,
            Err(TrySendError::Full(settings)) => {
                self.pending = Some(settings);
                false
            }
        }
    }
}

/// The UI owns only non-blocking channels and atomic worker totals.
pub struct PhosphorFeed {
    shared: Arc<Shared>,
    commands: Vec<Commands>,
    ready: Receiver<LaneFrame>,
    returns: SyncSender<LaneFrame>,
}

impl PhosphorFeed {
    /// Start one dedicated worker per member and one merger.
    pub fn start(cx: &mut Cx, config: FeedConfig) -> Result<Self, SpawnError> {
        if !config.valid() {
            return Err(SpawnError::Backend("invalid phosphor feed configuration".into()));
        }
        let shared = Arc::new(Shared::default());
        shared.configuration.store(1, Ordering::Relaxed);
        *shared.lanes.lock().unwrap() = Arc::new(config.settings().lanes);
        let (frame_send, frame_receive) = sync_channel(config.members);
        let (ready_send, ready_receive) = sync_channel(1);
        let (return_send, return_receive) = sync_channel(RETURN_SLOTS);
        let mut feed = Self { shared, commands: Vec::with_capacity(config.members), ready: ready_receive, returns: return_send };
        let mut member_returns = Vec::with_capacity(config.members);
        for index in 0..config.members {
            let (send, commands) = sync_channel(1);
            let (recycle, returned) = sync_channel(1);
            feed.commands.push(Commands { send, pending: None });
            member_returns.push(recycle);
            let settings = config.clone();
            let shared = feed.shared.clone();
            let frames = frame_send.clone();
            // Dropping feed on any spawn failure stops all earlier workers.
            cx.spawn_worker(move || run_member(index, settings, commands, frames, returned, shared))?.detach();
        }
        drop(frame_send);
        let shared = feed.shared.clone();
        cx.spawn_worker(move || run_merger(config.plot, frame_receive, member_returns, ready_send, return_receive, shared))?.detach();
        Ok(feed)
    }

    /// Take at most one ready frame and retry pending settings without waiting.
    pub fn take(&mut self) -> Option<LaneFrame> {
        for commands in &mut self.commands {
            commands.retry();
        }
        let hits = self.ready.try_recv().ok()?;
        if self.frame_is_current(&hits) {
            Some(hits)
        } else {
            // A resize can race with a publication already in the mailbox.
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            self.give_back(hits);
            None
        }
    }

    /// Return spent storage without waiting; a full queue discards and counts it.
    pub fn give_back(&mut self, hits: LaneFrame) {
        if self.returns.try_send(hits).is_err() {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Send settings to every member, retaining the newest unsent command.
    pub fn reconfigure(&mut self, trigger: EdgeTrigger, qualifier: Qualifier, trigger_channel: usize, window: Window, lanes: Vec<LaneSetup>, auto: Option<f64>, zone: Option<super::acquire::Zone>) {
        let settings = Settings { trigger, qualifier, zone, trigger_channel, window, lanes, auto };
        assert!(settings.valid(), "invalid feed settings");
        *self.shared.lanes.lock().unwrap() = Arc::new(settings.lanes.clone());
        let configuration = self.shared.configuration.fetch_add(1, Ordering::Relaxed) + 1;
        let mut full = 0;
        for commands in &mut self.commands {
            commands.pending = Some((configuration, settings.clone()));
            full += usize::from(!commands.retry());
        }
        if full > 0 {
            makepad_widgets_core::log!("phosphor feed: {full} command queues full; settings retained for the next frame");
        }
    }

    /// The nominal member's latest FFT answers, without waiting on a concurrent publication.
    pub fn spectrum_measurements(&self) -> Option<SpectrumMeasurements> { self.spectrum_result().measurements }

    /// Includes a rejected gate, filtered to the current bracket without waiting.
    pub fn spectrum_result(&self) -> SpectrumResult {
        self.shared.spectrum.try_lock().ok().filter(|s| s.0 == self.shared.span_request.generation()).map_or_else(SpectrumResult::default, |s| s.1)
    }

    /// Latest nominal capture's features, shared without waiting or copying, for the current configuration.
    pub fn features(&self) -> Option<Arc<CaptureFeatures>> {
        if self.shared.measurement_generation.load(Ordering::Acquire) == 0 { return None; }
        let snapshot = self.shared.measurements.try_lock().ok()?;
        (snapshot.configuration == self.shared.configuration.load(Ordering::Relaxed)).then(|| snapshot.features.clone())
    }

    /// Latest-wins bracket in trigger-relative seconds; return its request generation without waiting.
    /// The nominal worker retains its latest capture even without a bracket, until replaced or stopped.
    pub fn set_span(&self, span: Option<(f64, f64)>) -> u64 { self.shared.span_request.set(span) }

    /// The answered request generation (one per set_span call, starting at one) and every lane.
    /// An older generation remains visible while a new span is being measured.
    pub fn spans(&self) -> Option<(u64, Arc<Vec<Option<SpanStats>>>)> {
        if self.shared.measurement_generation.load(Ordering::Acquire) == 0 { return None; }
        let snapshot = self.shared.measurements.try_lock().ok()?;
        let configuration = self.shared.configuration.load(Ordering::Relaxed);
        (snapshot.configuration == configuration && snapshot.features.configuration == configuration)
            .then(|| (snapshot.span_generation, snapshot.spans.clone()))
    }

    /// Read worker totals without a lock.
    pub fn stats(&self) -> FeedStats {
        FeedStats {
            waveforms: self.shared.waveforms.load(Ordering::Relaxed),
            trigger_events: self.shared.trigger_events.load(Ordering::Relaxed),
            sim_time_ns: self.shared.sim_time_ns.load(Ordering::Relaxed),
            samples: self.shared.samples.load(Ordering::Relaxed),
            frames: self.shared.frames.load(Ordering::Relaxed),
            dropped: self.shared.dropped.load(Ordering::Relaxed),
            refused: self.shared.refused.load(Ordering::Relaxed),
            unresolved: false,
        }
    }

    /// Ask every worker to exit at its next frame boundary.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for PhosphorFeed {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ScopeFeed for PhosphorFeed {
    fn frame_is_current(&self, frame: &LaneFrame) -> bool {
        let lanes = self.shared.lanes.lock().unwrap();
        same_shape(frame, &lanes) && (lanes.iter().all(|lane| lane.spectrum.is_none()) || frame.generation == self.shared.span_request.generation())
    }

    fn generation_is_current(&self, generation: u64) -> bool {
        self.shared.lanes.lock().unwrap().iter().all(|lane| lane.spectrum.is_none()) || generation == self.shared.span_request.generation()
    }

    fn take(&mut self) -> Option<LaneFrame> {
        PhosphorFeed::take(self)
    }

    fn give_back(&mut self, hits: LaneFrame) {
        PhosphorFeed::give_back(self, hits);
    }

    fn reconfigure(&mut self, trigger: EdgeTrigger, qualifier: Qualifier, trigger_channel: usize, window: Window, lanes: Vec<LaneSetup>, auto: Option<f64>, zone: Option<super::acquire::Zone>) {
        PhosphorFeed::reconfigure(self, trigger, qualifier, trigger_channel, window, lanes, auto, zone);
    }

    fn stats(&self) -> FeedStats {
        PhosphorFeed::stats(self)
    }

    fn trigger_state(&self) -> TriggerState {
        match self.shared.trigger_state.load(Ordering::Relaxed) { 1 => TriggerState::Fired, 2 => TriggerState::Automatic, _ => TriggerState::Waiting }
    }

    fn stop(&mut self) {
        PhosphorFeed::stop(self);
    }
}

fn run_member(
    index: usize,
    config: FeedConfig,
    commands: Receiver<(u64, Settings)>,
    frames: SyncSender<(usize, LaneFrame)>,
    returned: Receiver<LaneFrame>,
    shared: Arc<Shared>,
) {
    if shared.stop.load(Ordering::Relaxed) {
        return;
    }
    let mut member = Member::new(&config, index);
    member.intake.follow_span(shared.span_request.clone());
    let mut configuration = 1;
    let mut capture_configuration = 1;
    let mut measure = (index == 0).then(|| {
        let request = Arc::new(AtomicBool::new(true));
        let (send, captures) = sync_channel(1);
        member.intake.capture(request.clone(), send.clone(), configuration);
        (MeasureWorker::with_span_request(request.clone(), captures, shared.measurements.clone(), shared.span_request.clone(), Duration::from_millis(100)), request, send)
    });
    let mut current = LaneFrame::single(config.plot.0, config.plot.1);
    let mut spare = Some(LaneFrame::single(config.plot.0, config.plot.1));
    let started = Instant::now();
    let mut boundary = started;
    let mut pending = None;
    let mut samples = 0_u64;
    let mut counted_waveforms = 0;
    let mut counted_events = 0;
    loop {
        let now = Instant::now();
        if now.duration_since(boundary) >= config.frame_every {
            if index == 0 {
                shared.trigger_state.store(match member.intake.acquisition().trigger_state() { TriggerState::Waiting => 0, TriggerState::Fired => 1, TriggerState::Automatic => 2 }, Ordering::Relaxed);
                let spectrum = member.intake.acquisition().spectrum_result(0);
                *shared.spectrum.lock().unwrap() = (member.intake.span_generation, spectrum);
            }
            let events = member.intake.acquisition().events();
            shared.trigger_events.fetch_add(events.saturating_sub(counted_events), Ordering::Relaxed);
            counted_events = events;
            if index == 0 { shared.sim_time_ns.store((member.time * 1e9) as u64, Ordering::Relaxed); }
            shared.samples.fetch_add(samples, Ordering::Relaxed);
            shared.waveforms.fetch_add(current.waveforms().saturating_sub(counted_waveforms), Ordering::Relaxed);
            samples = 0;
            counted_waveforms = current.waveforms();
            if shared.stop.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(settings) = commands.try_recv() {
                pending = Some(settings);
            }
            if let Some((generation, settings)) = pending.take() {
                configuration = generation;
                member.configure(settings);
            }
            if spare.is_none() {
                spare = returned.try_recv().ok();
            }
            if let Some(replacement) = spare.take() {
                let hits = std::mem::replace(&mut current, replacement);
                match frames.try_send((index, hits)) {
                    Ok(()) => {
                        current.reset(&member.settings.lanes, current.generation);
                        member.finish_resize(&mut current);
                        counted_waveforms = 0;
                    }
                    Err(TrySendError::Full((_, hits))) => {
                        spare = Some(std::mem::replace(&mut current, hits));
                    }
                    Err(TrySendError::Disconnected((_, hits))) => {
                        current = hits;
                        break;
                    }
                }
            }
            if let Some((worker, request, send)) = &mut measure {
                if member.resize.is_none() {
                    if capture_configuration != configuration {
                        member.intake.capture(request.clone(), send.clone(), configuration);
                        request.store(true, Ordering::Relaxed);
                        capture_configuration = configuration;
                    }
                    let swings: Vec<_> = member.settings.lanes.iter().map(|lane| (least_swing((lane.map.v_top - lane.map.v_px * lane.map.h as f64, lane.map.v_top)), Quantity::Volt)).collect();
                    worker.poll_source(configuration, &swings, &member.settings.lanes, &shared.measurement_generation);
                }
            }
            boundary = now;
        }

        if let Some(rate) = config.pace {
            let ahead = member.time / rate - started.elapsed().as_secs_f64();
            if ahead > 0.0 {
                let remaining = config.frame_every.saturating_sub(boundary.elapsed());
                let wait = Duration::from_secs_f64(ahead.min(remaining.as_secs_f64()));
                if !wait.is_zero() {
                    // The command input also supplies the pacing sleep.
                    match commands.recv_timeout(wait) {
                        Ok(settings) => pending = Some(settings),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                continue;
            }
        }
        member.run_for(SAMPLE_BATCH, &mut current);
        samples += SAMPLE_BATCH as u64;
    }
    shared.trigger_events.fetch_add(member.intake.acquisition().events().saturating_sub(counted_events), Ordering::Relaxed);
    if index == 0 { shared.sim_time_ns.store((member.time * 1e9) as u64, Ordering::Relaxed); }
    shared.samples.fetch_add(samples, Ordering::Relaxed);
    shared.waveforms.fetch_add(current.waveforms().saturating_sub(counted_waveforms), Ordering::Relaxed);
}

fn run_merger(
    plot: (u32, u32),
    frames: Receiver<(usize, LaneFrame)>,
    member_returns: Vec<SyncSender<LaneFrame>>,
    ready: SyncSender<LaneFrame>,
    returned: Receiver<LaneFrame>,
    shared: Arc<Shared>,
) {
    let lanes = [LaneSetup::time(PhosphorMap::screen(plot.0, plot.1, 0.0, 1.0, 0.0, 1.0))];
    let mut merger = Merger::with_sender(&lanes, ready);
    let mut counted_drops = 0;
    while !shared.stop.load(Ordering::Relaxed) {
        let lanes = shared.lanes.lock().unwrap().clone();
        merger.set_lanes(&lanes);
        for _ in 0..RETURN_SLOTS {
            let Ok(hits) = returned.try_recv() else { break };
            merger.recycle(hits);
        }
        if merger.publish() {
            shared.frames.fetch_add(1, Ordering::Relaxed);
        }
        shared.refused.store(merger.refused(), Ordering::Relaxed);
        // A timeout also notices returned UI storage when members are quiet.
        match frames.recv_timeout(Duration::from_millis(1)) {
            Ok((index, hits)) => {
                let lanes = shared.lanes.lock().unwrap().clone();
                merger.set_lanes(&lanes);
                let hits = if lanes.iter().all(|lane| lane.spectrum.is_none()) || hits.generation == shared.span_request.generation() { merger.add(hits) } else { hits };
                if member_returns[index].try_send(hits).is_err() {
                    shared.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        shared.dropped.fetch_add(merger.dropped() - counted_drops, Ordering::Relaxed);
        counted_drops = merger.dropped();
    }
    shared.dropped.fetch_add(merger.dropped() - counted_drops, Ordering::Relaxed);
}
