//! DeviceSource's scope adapter: acquisition stays on its worker, the UI takes one mailbox.
use super::{
    acquire::{AcquireIntake, Acquisition, LaneFrame, LaneSetup, Published, TriggerState},
    device::DeviceSource,
    feed::{FeedStats, ScopeFeed},
    spectrum::SpectrumMeasurements,
    scope::Measurement,
    simfeed::{MeasureSnapshot, MeasureWorker, least_swing},
};
use fab_elec_bench::{source::*, trigger::{EdgeTrigger, Qualifier, Window}};
use makepad_widgets_core::{Cx, SignalToUI, SpawnError};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering}, mpsc::{sync_channel, Receiver, SyncSender, TrySendError}};
use std::time::Duration;

#[derive(Default)]
struct Totals { trigger_events: AtomicU64, waveforms: AtomicU64, samples: AtomicU64, frames: AtomicU64, dropped: AtomicU64, state: AtomicU8, spectrum: Mutex<Option<SpectrumMeasurements>>, measurements: Arc<Mutex<MeasureSnapshot>>, measurement_generation: AtomicU64 }
struct Factory {
    lanes: Vec<LaneSetup>, trigger: EdgeTrigger, qualifier: Qualifier, channel: usize, window: Window, auto: Option<f64>, zone: Option<super::acquire::Zone>,
    epoch: u64, ready: SyncSender<(u64, LaneFrame)>, totals: Arc<Totals>, returned: Arc<Mutex<Receiver<LaneFrame>>>,
}
impl IntakeFactory for Factory {
    fn make(&mut self, layout: &Layout) -> Box<dyn Intake> {
        let mut acquisition = Acquisition::new(self.trigger, self.channel, self.window, self.lanes.clone()).with_qualifier(self.qualifier);
        acquisition.set_auto(self.auto);
        acquisition.set_zone(self.zone);
        let (ready, totals, lanes, epoch) = (self.ready.clone(), self.totals.clone(), self.lanes.clone(), self.epoch);
        let returned = self.returned.clone();
        let mut free = Vec::<LaneFrame>::new();
        let mut intake = AcquireIntake::new(acquisition, Duration::from_millis(16), move |frame: LaneFrame, stats: super::acquire::AcquireStats| {
            let waveforms = frame.waveforms();
            // Empty the return queue on every publication, sent or kept (the simulation feed's
            // rule): storage allocated while the UI was busy keeps circulating, and a frame is
            // allocated only when every frame of this intake is in flight.
            free.extend(returned.lock().unwrap().try_iter());
            match ready.try_send((epoch, frame)) {
                Ok(()) => {
                    totals.waveforms.fetch_add(waveforms, Ordering::Relaxed);
                    totals.trigger_events.fetch_add(stats.trigger_events, Ordering::Relaxed);
                    totals.samples.fetch_add(stats.samples, Ordering::Relaxed);
                    totals.frames.fetch_add(1, Ordering::Relaxed);
                    totals.state.store(match stats.trigger_state { TriggerState::Waiting => 0, TriggerState::Fired => 1, TriggerState::Automatic => 2 }, Ordering::Relaxed);
                    *totals.spectrum.lock().unwrap() = stats.spectrum.first().and_then(|s| s.measurements);
                    SignalToUI::set_ui_signal();
                    Published::Taken(free.pop().unwrap_or_else(|| LaneFrame::new(&lanes)))
                }
                Err(TrySendError::Full((_, frame)) | TrySendError::Disconnected((_, frame))) => Published::Kept(frame),
            }
        });
        intake.discard_marks();
        let request = Arc::new(AtomicBool::new(true));
        let (send, captures) = sync_channel(1);
        intake.capture(request.clone(), send, epoch);
        let swings = self.lanes.iter().zip(&layout.channels).map(|(lane, channel)|
            (least_swing((lane.map.v_top - lane.map.v_px * lane.map.h as f64, lane.map.v_top)), channel.quantity)).collect();
        Box::new(MeasuredIntake { inner: Box::new(intake),
            worker: MeasureWorker::new(request, captures, self.totals.measurements.clone(), Duration::from_millis(100)),
            epoch, lanes: self.lanes.clone(), swings, totals: self.totals.clone(), points: 0 })
    }
}

// Lives on DeviceSource's worker. Block sources poll once per block, point sources every batch.
struct MeasuredIntake {
    inner: Box<dyn Intake>, worker: MeasureWorker, epoch: u64, lanes: Vec<LaneSetup>,
    swings: Vec<(f64, Quantity)>, totals: Arc<Totals>, points: usize,
}
impl MeasuredIntake {
    fn poll(&mut self) {
        self.worker.poll_source(self.epoch, &self.swings, &self.lanes, &self.totals.measurement_generation);
    }
}
impl Intake for MeasuredIntake {
    fn begin(&mut self, layout: &Layout) { self.inner.begin(layout); }
    fn point(&mut self, t: f64, y: &[f32]) {
        self.inner.point(t, y);
        self.points += 1;
        if self.points == 64 { self.points = 0; self.poll(); }
    }
    fn block(&mut self, block: &Block) { self.inner.block(block); self.poll(); }
    fn mark(&mut self, t: f64, kind: Mark) { self.inner.mark(t, kind); }
    fn gap(&mut self, from: f64, to: f64, why: Gap) { self.inner.gap(from, to, why); }
    fn idle(&mut self) { self.inner.idle(); self.poll(); }
    fn steady(&mut self, at: f64) { self.inner.steady(at); self.poll(); }
    fn pending(&self) -> bool { self.inner.pending() }
}

/// A device-backed feed. Configuration is sent asynchronously; take/status never wait for I/O.
pub struct DeviceFeed<D: Driver> {
    source: DeviceSource<D>, ready: Receiver<(u64, LaneFrame)>, send: SyncSender<(u64, LaneFrame)>,
    returns: SyncSender<LaneFrame>, returned: Arc<Mutex<Receiver<LaneFrame>>>, totals: Arc<Totals>, attachment: Option<AttachId>, epoch: u64,
}
impl<D: Driver + 'static> DeviceFeed<D> {
    pub fn open(cx: &mut Cx, driver: D) -> Result<Self, SpawnError> {
        let source = DeviceSource::open(cx, driver)?;
        let (send, ready) = sync_channel(1);
        let (returns, returned) = sync_channel(3);
        Ok(Self { source, ready, send, returns, returned: Arc::new(Mutex::new(returned)), totals: Arc::new(Totals::default()), attachment: None, epoch: 0 })
    }
    pub fn source(&self) -> &DeviceSource<D> { &self.source }
    /// Metadata cached by the source at open, including its side.
    pub fn info(&self) -> &SourceInfo { self.source.info() }
    /// Latest time-domain rows for this setup, shared without waiting on the acquisition worker.
    pub fn measurements(&self) -> Option<Arc<Vec<Measurement>>> {
        if self.totals.measurement_generation.load(Ordering::Acquire) == 0 { return None; }
        let snapshot = self.totals.measurements.try_lock().ok()?;
        (snapshot.configuration == self.epoch).then(|| snapshot.values.clone())
    }
    /// Latest spectral readout, without waiting on a concurrent publication.
    pub fn spectrum_measurements(&self) -> Option<SpectrumMeasurements> { self.totals.spectrum.try_lock().ok().and_then(|s| *s) }
}
impl<D: Driver + 'static> ScopeFeed for DeviceFeed<D> {
    fn take(&mut self) -> Option<LaneFrame> {
        let (epoch, frame) = self.ready.try_recv().ok()?;
        if epoch == self.epoch { Some(frame) }
        else { self.totals.dropped.fetch_add(1, Ordering::Relaxed); self.give_back(frame); None }
    }
    fn give_back(&mut self, frame: LaneFrame) { let _ = self.returns.try_send(frame); }
    fn reconfigure(&mut self, trigger: EdgeTrigger, qualifier: Qualifier, channel: usize, window: Window, lanes: Vec<LaneSetup>, auto: Option<f64>, zone: Option<super::acquire::Zone>) {
        let caps = self.source.caps();
        if lanes.is_empty() || channel >= lanes.len() || lanes.len() != caps.channels.len() { return; }
        let span = window.pre + window.post;
        let mut want = Setup {
            channels: lanes.iter().zip(&caps.channels).map(|(lane, cap)| ChannelSetup {
                id: cap.id, on: true, window: (lane.map.v_top - lane.map.v_px * lane.map.h as f64, lane.map.v_top),
                coupling: Coupling::Dc, probe: Probe::default(), bw_limit: None, invert: false,
            }).collect(),
            time: TimeSetup { span, pre: window.pre, rate: RateWant::Auto { per_px: 2.0 }, depth: DepthWant::Auto, plot_px: lanes[0].map.w },
            trigger: TriggerSpec { kind: qualifier.kind(), channel: caps.channels[channel].id, level: trigger.level,
                hyst: trigger.hyst, slope: trigger.slope, holdoff: if zone.is_some() { 0.0 } else { trigger.holdoff }, run: auto.map_or(TriggerRun::Normal, TriggerRun::Auto) },
            mode: ModeWant::Auto, drives: Vec::new(),
        };
        // Software search needs context on both sides of an edge inside a free-running block.
        let mut resolved = self.source.resolve(&want);
        if matches!(resolved.trigger, TrigResolved::Software { over: Over::Blocks { .. } }) {
            want.time.span *= 3.0;
            want.time.rate = RateWant::Exact(resolved.rate);
            resolved = self.source.resolve(&want);
        }
        // Leave one confirming sample beyond the visible window so floating-point endpoint
        // rounding cannot keep a hardware window open across the next blind gap.
        let window = if resolved.mode != Mode::Stream {
            let visible = span.min(resolved.depth.saturating_sub(2).max(1) as f64 / resolved.rate);
            Window { pre: window.pre.min(visible), post: visible - window.pre.min(visible) }
        } else { window };
        if let Some(id) = self.attachment.take() { self.source.detach(id); }
        self.epoch += 1;
        self.attachment = Some(self.source.attach(Box::new(Factory { lanes, trigger, qualifier, channel, window, auto, zone,
            epoch: self.epoch, ready: self.send.clone(), totals: self.totals.clone(), returned: self.returned.clone() })));
        self.source.request(want);
    }
    fn stats(&self) -> FeedStats {
        FeedStats { trigger_events: self.totals.trigger_events.load(Ordering::Relaxed), sim_time_ns: 0, waveforms: self.totals.waveforms.load(Ordering::Relaxed), samples: self.totals.samples.load(Ordering::Relaxed),
            frames: self.totals.frames.load(Ordering::Relaxed), dropped: self.totals.dropped.load(Ordering::Relaxed), refused: 0, unresolved: false }
    }
    fn trigger_state(&self) -> TriggerState {
        match self.totals.state.load(Ordering::Relaxed) { 1 => TriggerState::Fired, 2 => TriggerState::Automatic, _ => TriggerState::Waiting }
    }
    fn stop(&mut self) { self.source.park(); }
}
