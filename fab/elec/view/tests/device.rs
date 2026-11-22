use fab_elec_bench::source::*;
use fab_elec_bench::trigger::{EdgeTrigger, Slope, Window};
use fab_elec_view::instruments::acquire::{AcquireIntake, Acquisition, LaneFrame, LaneSetup, Published};
use fab_elec_view::instruments::device::DeviceSource;
use fab_elec_view::instruments::phosphor::PhosphorMap;
use makepad_widgets_core::Cx;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How many more pumps the device worker gets to make a polled property true. Waits are bounded by
/// the worker's progress or by its own signals, never by a clock, so a loaded machine only makes a
/// test slower.
const PUMPS: u64 = 50;
enum Step { Stall(Sender<()>, Receiver<()>), Burst(usize, Sender<()>), Gone }
struct Controlled { steps: Receiver<Step>, applied: Sender<f64>, stopped: Sender<()>, index: u64, announced: bool, gone_rate: Option<f64>, pumps: Arc<AtomicU64> }
impl Driver for Controlled {
    fn info(&self) -> SourceInfo { SourceInfo { id: SourceId(1), side: Side::Real, name: "controlled".into(), serial: None, clock: ClockKind::Device } }
    fn caps(&self) -> Caps {
        Caps { generation: 0, channels: vec![ChannelCaps { id: ChanId(0), name: "A".into(), kind: ChanKind::Analog, quantity: Quantity::Volt,
            ranges: Ranges::Any, bits: 0, coupling: vec![Coupling::Dc], attenuation: Atten::Free, bandwidth: None, bw_limits: Vec::new() }],
            rate: RateCaps::Any, depth: Depth::Unbounded, modes: Modes { stream: true, ..Modes::default() }, stream_rate: None,
            trigger: TriggerCaps::default(), powers: Powers::default(), outputs: Vec::new() }
    }
    fn apply(&mut self, want: &Setup) -> Result<Resolved, DriverError> {
        let answer = resolve(&self.caps(), want, None);
        if self.gone_rate == Some(answer.rate) { return Err(DriverError::Gone("unplugged during apply".into())); }
        self.applied.send(answer.rate).unwrap();
        Ok(answer)
    }
    fn pump(&mut self, out: &mut dyn BlockOut) -> Pump {
        self.pumps.fetch_add(1, Ordering::Release);
        if !self.announced {
            out.layout(&Layout { generation: 1, clock: ClockKind::Device, channels: vec![ChannelInfo { id: ChanId(0), name: "A".into(), quantity: Quantity::Volt }] });
            self.announced = true;
        }
        match self.steps.try_recv() {
            Ok(Step::Stall(entered, release)) => {
                entered.send(()).unwrap();
                // Opens only on release (or when the test drops the gate): no clock decides it.
                if release.recv().is_err() { return Pump::Gone("test dropped the pump gate".into()); }
                Pump::Idle(Duration::from_millis(1))
            }
            Ok(Step::Burst(count, done)) => {
                for _ in 0..count {
                    out.block(&Block { index: self.index, t0: self.index as f64 * 0.001, dt: 0.001,
                        chans: &[ChanData::F32(&[0.0, 2.0, 0.0, 2.0])], clipped: &[], trigger: None });
                    out.mark(self.index as f64 * 0.001 + 3.0 * 0.001, Mark::HardwareTrigger);
                    self.index += 4;
                }
                done.send(()).unwrap();
                Pump::Data
            }
            Ok(Step::Gone) => Pump::Gone("unplugged".into()),
            Err(_) => Pump::Idle(Duration::from_millis(1)),
        }
    }
    fn stop(&mut self) { let _ = self.stopped.send(()); }
}
fn driver() -> (Controlled, Sender<Step>, Receiver<f64>, Receiver<()>) {
    let (send, steps) = channel(); let (applied, answers) = channel(); let (stopped, stopped_rx) = channel();
    (Controlled { steps, applied, stopped, index: 0, announced: false, gone_rate: None, pumps: Arc::new(AtomicU64::new(0)) }, send, answers, stopped_rx)
}
fn setup() -> Setup {
    Setup { channels: vec![ChannelSetup { id: ChanId(0), on: true, window: (-0.5, 2.5), coupling: Coupling::Dc,
        probe: Probe::default(), bw_limit: None, invert: false }],
        time: TimeSetup { span: 0.01, pre: 0.0, rate: RateWant::Exact(100.0), depth: DepthWant::Auto, plot_px: 32 },
        trigger: TriggerSpec { kind: TriggerKind::Edge, channel: ChanId(0), level: 1.0, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0, run: TriggerRun::Normal },
        mode: ModeWant::Auto, drives: Vec::new() }
}
/// Polls `condition` until it holds; fails once the worker has pumped PUMPS times since the wait
/// began with it still false. A worker the machine does not schedule only makes this slower.
fn until(pumps: &AtomicU64, property: &str, mut condition: impl FnMut() -> bool) {
    let start = pumps.load(Ordering::Acquire);
    loop {
        let now = pumps.load(Ordering::Acquire);
        if condition() { return; }
        assert!(now < start + PUMPS, "{property}");
        std::thread::yield_now();
    }
}
fn burst(send: &Sender<Step>, count: usize) {
    let (done, ready) = channel(); send.send(Step::Burst(count, done)).unwrap();
    ready.recv().expect("device must finish producing while acquisition is stalled");
}

#[test]
fn stalled_pump_does_not_block_ui_and_only_newest_request_applies() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let (driver, steps, applied, stopped) = driver();
    let pumps = driver.pumps.clone();
    let (entered, stalled) = channel(); let (release, released) = channel();
    steps.send(Step::Stall(entered, released)).unwrap();
    let mut source = DeviceSource::open(&mut cx, driver).unwrap();
    let mut want = setup();
    source.request(want.clone());
    stalled.recv().expect("device reaches controlled pump stall");
    assert_eq!(applied.recv().unwrap(), 100.0, "initial setup applies before stalled pump");
    for rate in [200.0, 300.0, 400.0] {
        want.time.rate = RateWant::Exact(rate);
        source.request(want.clone());
        let status = source.status();
        assert_eq!(status.resolved.as_ref().unwrap().rate, 100.0, "stalled worker leaves its applied answer intact");
    }
    // The pump is still held at its gate, which only the release below opens: had request or status
    // waited on the worker, this line would never be reached.
    let status = source.status();
    assert!(status.state != State::Gone && status.error.is_none(), "request and status return while the pump is still held at its gate");
    assert!(applied.try_recv().is_err(), "no request is applied inside blocked pump");
    release.send(()).unwrap();
    until(&pumps, "latest request eventually applies after status retries", || source.status().resolved.is_some_and(|r| r.rate == 400.0));
    assert_eq!(applied.try_iter().collect::<Vec<_>>(), vec![400.0], "obsolete queued and pending setups are never applied after the newer request");
    let preview = source.resolve(&want);
    assert!(!preview.restart, "UI preview compares against the actual applied answer");
    steps.send(Step::Gone).unwrap();
    until(&pumps, "unplug is visible through nonblocking status", || source.status().state == State::Gone);
    stopped.recv().expect("device worker stops after unplug");
}

#[derive(Clone, Debug, PartialEq)]
enum Seen { Begin, Block(u64), Point(f64), Mark(f64, Mark), Gap(f64, f64, Gap), Idle, Dropped }
struct Factory { seen: Sender<Seen>, frame: Arc<Mutex<Option<LaneFrame>>>, gate: Option<(Sender<()>, Receiver<()>)> }
impl IntakeFactory for Factory {
    fn make(&mut self, _: &Layout) -> Box<dyn Intake> {
        let lanes = vec![LaneSetup::time(PhosphorMap::screen(32, 16, 0.0, 0.002, -0.5, 2.5))];
        let acquisition = Acquisition::new(EdgeTrigger { level: 1.0, hyst: 0.1, slope: Slope::Rising, holdoff: 0.0 },
            0, Window { pre: 0.0, post: 0.002 }, lanes.clone());
        let frame = self.frame.clone();
        let intake = AcquireIntake::new(acquisition, Duration::MAX, move |hits, _| {
            *frame.lock().unwrap() = Some(hits);
            Published::Taken(LaneFrame::new(&lanes))
        });
        Box::new(Recording { seen: self.seen.clone(), intake: Box::new(intake), gate: self.gate.take() })
    }
}
struct Recording { seen: Sender<Seen>, intake: Box<dyn Intake>, gate: Option<(Sender<()>, Receiver<()>)> }
impl Intake for Recording {
    fn begin(&mut self, layout: &Layout) { self.intake.begin(layout); self.seen.send(Seen::Begin).unwrap(); }
    fn block(&mut self, block: &Block) {
        if let Some((entered, release)) = self.gate.take() {
            entered.send(()).unwrap(); release.recv().expect("test releases acquisition gate");
        }
        self.intake.block(block); self.seen.send(Seen::Block(block.index)).unwrap();
    }
    fn point(&mut self, t: f64, y: &[f32]) { self.intake.point(t, y); self.seen.send(Seen::Point(t)).unwrap(); }
    fn mark(&mut self, t: f64, kind: Mark) { self.intake.mark(t, kind); self.seen.send(Seen::Mark(t, kind)).unwrap(); }
    fn gap(&mut self, from: f64, to: f64, why: Gap) { self.intake.gap(from, to, why); self.seen.send(Seen::Gap(from, to, why)).unwrap(); }
    fn idle(&mut self) { self.intake.idle(); let _ = self.seen.send(Seen::Idle); }
}
impl Drop for Recording { fn drop(&mut self) { let _ = self.seen.send(Seen::Dropped); } }
fn finish(events: &Receiver<Seen>) -> Vec<Seen> {
    let mut seen = Vec::new();
    loop {
        let event = events.recv().expect("acquisition drains and finishes after unplug");
        let done = event == Seen::Dropped; seen.push(event);
        if done { return seen; }
    }
}

#[test]
fn full_ring_keeps_pumping_and_delivers_every_dropped_span() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let (driver, steps, _applied, stopped) = driver();
    let pumps = driver.pumps.clone();
    let mut source = DeviceSource::open(&mut cx, driver).unwrap();
    let (seen, events) = channel(); let frame = Arc::new(Mutex::new(None));
    let (entered, stalled) = channel(); let (release, released) = channel();
    source.attach(Box::new(Factory { seen, frame: frame.clone(), gate: Some((entered, released)) }));
    source.request(setup());
    burst(&steps, 1);
    stalled.recv().expect("intake holds first buffer without draining ring");
    burst(&steps, 20);
    burst(&steps, 20);
    until(&pumps, "all full-ring blocks are counted without waiting for intake", || { let s = source.status(); s.gaps == 1 && (s.coverage - 5.0 / 41.0).abs() < 1e-6 });
    let status = source.status();
    assert_eq!(status.gaps, 1, "the 36 blocks dropped while the first is held are one gap"); 
    assert!((status.coverage - 5.0 / 41.0).abs() < 1e-6, "coverage measures retained sample time, including the first held block");
    assert!(status.sample_rate > 0.0, "accepted samples contribute to acquisition throughput");
    assert!(status.trigger_rate > 0.0, "retained source trigger marks contribute to trigger throughput");
    release.send(()).unwrap();
    steps.send(Step::Gone).unwrap();
    let seen = finish(&events);
    stopped.recv().expect("device stops after producing all loss annotations");
    let blocks: Vec<_> = seen.iter().filter_map(|e| if let Seen::Block(index) = e { Some(*index) } else { None }).collect();
    assert_eq!(blocks, vec![0, 4, 8, 12, 16], "ring preserves FIFO order and its bounded capacity");
    let gaps: Vec<_> = seen.iter().filter_map(|e| if let Seen::Gap(from, to, why) = e { Some((*from, *to, *why)) } else { None }).collect();
    assert_eq!(gaps, vec![(20.0 * 0.001, 160.0 * 0.001 + 4.0 * 0.001, Gap::Overrun)], "consecutive dropped blocks reach the intake as one gap over their exact span, so a stalled intake costs one journal entry");
    assert_eq!(seen.iter().filter(|e| matches!(e, Seen::Mark(_, Mark::HardwareTrigger))).count(), 5, "marks on dropped blocks cannot rejoin missing data");
    assert!(matches!(&seen[seen.len() - 2], Seen::Idle), "acquisition sends final idle before dropping the intake");
    assert!(frame.lock().unwrap().is_some(), "retained data publishes despite later dropped spans");
}

#[test]
fn unplug_flushes_and_keeps_the_last_frame() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let (driver, steps, _applied, stopped) = driver();
    let pumps = driver.pumps.clone();
    let mut source = DeviceSource::open(&mut cx, driver).unwrap();
    let (seen, events) = channel(); let frame = Arc::new(Mutex::new(None));
    source.attach(Box::new(Factory { seen, frame: frame.clone(), gate: None }));
    source.request(setup());
    burst(&steps, 1);
    steps.send(Step::Gone).unwrap();
    let seen = finish(&events);
    until(&pumps, "unplug publishes State::Gone", || source.status().state == State::Gone);
    let status = source.status();
    assert_eq!(status.error.as_deref(), Some("unplugged"), "unplug retains its driver reason");
    assert!(status.resolved.is_some(), "unplug retains the last applied answer");
    assert_eq!(seen.iter().filter(|e| **e == Seen::Block(0)).count(), 1, "last source block is consumed once");
    assert!(matches!(&seen[seen.len() - 2], Seen::Idle), "intake gets a final idle on unplug");
    let last = frame.lock().unwrap().clone().expect("final idle publishes the last frame");
    assert!(last.lanes[0].waveforms > 0, "the last frame contains an acquired waveform");
    stopped.recv().expect("unplug stops device worker");
    drop(source);
    assert_eq!(frame.lock().unwrap().as_ref(), Some(&last), "dropping a gone source leaves the last published frame available as a still");
}

#[test]
fn unplug_during_apply_finishes_attached_intakes() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let (mut driver, steps, _applied, stopped) = driver();
    let pumps = driver.pumps.clone();
    driver.gone_rate = Some(200.0);
    let mut source = DeviceSource::open(&mut cx, driver).unwrap();
    let (seen, events) = channel(); let frame = Arc::new(Mutex::new(None));
    source.attach(Box::new(Factory { seen, frame: frame.clone(), gate: None }));
    let mut want = setup();
    source.request(want.clone());
    burst(&steps, 1);
    want.time.rate = RateWant::Exact(200.0);
    source.request(want);
    until(&pumps, "apply error publishes Gone", || source.status().state == State::Gone);
    let seen = finish(&events);
    let status = source.status();
    assert_eq!(status.resolved.as_ref().unwrap().rate, 100.0, "failed apply preserves the prior applied answer");
    assert_eq!(status.error.as_deref(), Some("unplugged during apply"), "apply error retains its reason");
    assert!(matches!(&seen[seen.len() - 2], Seen::Idle), "apply error sends final idle to attached intake");
    assert!(frame.lock().unwrap().is_some(), "apply error preserves acquired data");
    stopped.recv().expect("apply unplug stops the device worker");
}
