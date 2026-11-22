//! A device worker and a bounded sample ring, independent of the scope's rendering feed.
use fab_elec_bench::source::*;
use makepad_widgets_core::{Cx, SpawnError, ThreadOptions, ThreadSpawner};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RING_SLOTS: usize = 4;
const CHECK: Duration = Duration::from_millis(10);

struct Published { state: State, resolved: Option<Arc<Resolved>>, error: Option<String> }
struct Shared {
    stop: AtomicBool, parked: AtomicBool, layout: AtomicU64, newest: AtomicU64, started: Instant, updated: AtomicU64, published: Mutex<Arc<Published>>,
    samples: AtomicU64, triggers: AtomicU64, captures: AtomicU64, coverage: AtomicU64, gaps: AtomicU64,
}
impl Shared {
    fn publish(&self, state: State, resolved: Option<Arc<Resolved>>, error: Option<String>) {
        let next = Arc::new(Published { state, resolved, error });
        // Only exchange a pointer under this lock; destruction happens after releasing it.
        let old = std::mem::replace(&mut *self.published.lock().unwrap(), next);
        drop(old);
    }
}
struct Request { version: u64, want: Setup }
struct Commands { send: SyncSender<Request>, pending: Option<Request> }
impl Commands {
    fn retry(&mut self) {
        if let Some(request) = self.pending.take() {
            if let Err(TrySendError::Full(request)) = self.send.try_send(request) { self.pending = Some(request); }
        }
    }
}
enum Attach { Add(AttachId, Box<dyn IntakeFactory + Send>), Remove(AttachId) }

/// The UI never calls apply/pump/stop, joins a worker, or locks worker-owned mutable data.
/// Metadata accessors on Driver must be ordinary in-memory copies at open.
pub struct DeviceSource<D: Driver> {
    info: SourceInfo, caps: Arc<Caps>, shared: Arc<Shared>, commands: RefCell<Commands>,
    cached: RefCell<Arc<Published>>, attaches: Sender<Attach>, wake: SyncSender<()>, next_id: u64,
    driver: PhantomData<fn() -> D>,
}
impl<D: Driver + 'static> DeviceSource<D> {
    /// Open a source without running driver I/O on the calling thread.
    pub fn open(cx: &mut Cx, driver: D) -> Result<Self, SpawnError> {
        Self::open_on(cx.thread_spawner(), driver)
    }
    /// Open from a host worker using the same platform spawner as its Cx.
    pub fn open_on(spawner: ThreadSpawner, driver: D) -> Result<Self, SpawnError> {
        let info = driver.info();
        let caps = Arc::new(driver.caps());
        let published = Arc::new(Published { state: State::Opening, resolved: None, error: None });
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), parked: AtomicBool::new(false), layout: AtomicU64::new(0), newest: AtomicU64::new(0), started: Instant::now(), updated: AtomicU64::new(0), published: Mutex::new(published.clone()),
            samples: AtomicU64::new(0), triggers: AtomicU64::new(0), captures: AtomicU64::new(0), coverage: AtomicU64::new(1_f64.to_bits()), gaps: AtomicU64::new(0) });
        let (send, requests) = sync_channel(1);
        let (blocks, receive) = sync_channel(RING_SLOTS);
        let (returns, returned) = sync_channel(RING_SLOTS + 1);
        // Only metadata uses this journal: an indefinitely stalled intake must still receive
        // every dropped span. Consecutive drops are one span, so the journal holds at most one
        // entry per retained block; sample storage stays bounded by RING_SLOTS + 2 buffers.
        let (events, journal) = channel();
        let (attaches, attached) = channel();
        let (wake, waking) = sync_channel(1);
        let source = Self { info, caps, shared: shared.clone(), commands: RefCell::new(Commands { send, pending: None }),
            cached: RefCell::new(published), attaches, wake: wake.clone(), next_id: 0, driver: PhantomData };
        let worker = shared.clone();
        spawner.spawn_worker(ThreadOptions::default(), move || acquire(receive, journal, attached, returns, waking, worker))?.detach();
        let output = Output { blocks, events, returned, wake, shared: shared.clone(), sequence: 0, overrun: None,
            spare: Vec::new(), previous: None, nominal_dt: 1.0, marked: None, active: false, metrics: Metrics::new(), step: Counts::default() };
        spawner.spawn_worker(ThreadOptions::default(), move || run(driver, requests, output, shared))?.detach();
        Ok(source)
    }
    /// Latest driver-announced layout generation, read without waiting for acquisition.
    pub fn layout_generation(&self) -> u64 {
        self.shared.layout.load(Ordering::Acquire)
    }
    /// Captures produced per wall second, including free-running software-trigger blocks.
    pub fn capture_rate(&self) -> f64 {
        let stale = self.shared.started.elapsed().as_nanos().saturating_sub(self.shared.updated.load(Ordering::Acquire) as u128) >= 1_000_000_000;
        if stale { 0.0 } else { f64::from_bits(self.shared.captures.load(Ordering::Relaxed)) }
    }
    /// Suspend pumping without closing the worker or losing attachments; request resumes it.
    pub fn park(&mut self) {
        self.shared.parked.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
    }
    fn snapshot(&self) -> Arc<Published> {
        self.commands.borrow_mut().retry();
        // A concurrent pointer swap never makes the UI wait; use the previous snapshot once.
        if let Ok(current) = self.shared.published.try_lock() {
            let next = current.clone();
            drop(current);
            *self.cached.borrow_mut() = next;
        }
        self.cached.borrow().clone()
    }
}
impl<D: Driver + 'static> Source for DeviceSource<D> {
    fn info(&self) -> &SourceInfo { &self.info }
    fn caps(&self) -> Arc<Caps> { self.caps.clone() }
    fn resolve(&self, want: &Setup) -> Resolved { resolve(&self.caps, want, self.snapshot().resolved.as_deref()) }
    fn request(&mut self, want: Setup) {
        self.shared.parked.store(false, Ordering::Release);
        let version = self.shared.newest.fetch_add(1, Ordering::AcqRel) + 1;
        let commands = self.commands.get_mut();
        commands.pending = Some(Request { version, want });
        commands.retry();
    }
    fn status(&self) -> Status {
        let snapshot = self.snapshot();
        let stale = self.shared.started.elapsed().as_nanos().saturating_sub(self.shared.updated.load(Ordering::Acquire) as u128) >= 1_000_000_000;
        Status { state: snapshot.state, resolved: snapshot.resolved.clone(), error: snapshot.error.clone(),
            sample_rate: if stale { 0.0 } else { f64::from_bits(self.shared.samples.load(Ordering::Relaxed)) },
            trigger_rate: if stale { 0.0 } else { f64::from_bits(self.shared.triggers.load(Ordering::Relaxed)) },
            coverage: if stale { 1.0 } else { f64::from_bits(self.shared.coverage.load(Ordering::Relaxed)) as f32 },
            gaps: self.shared.gaps.load(Ordering::Relaxed) }
    }
    fn attach(&mut self, intake: Box<dyn IntakeFactory + Send>) -> AttachId {
        self.commands.get_mut().retry();
        self.next_id += 1;
        let id = AttachId(self.next_id);
        let _ = self.attaches.send(Attach::Add(id, intake));
        let _ = self.wake.try_send(());
        id
    }
    fn detach(&mut self, id: AttachId) {
        self.commands.get_mut().retry();
        let _ = self.attaches.send(Attach::Remove(id));
        let _ = self.wake.try_send(());
    }
}
impl<D: Driver> Drop for DeviceSource<D> {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
    }
}

#[derive(Default)]
struct Buffer { index: u64, t0: f64, dt: f64, trigger: Option<f64>, clipped: Vec<Clipped>, columns: Vec<Vec<f32>>, point: Vec<f32>, is_point: bool }
struct Packet { sequence: u64, buffer: Buffer }
enum Event { Layout(Layout), Mark(f64, Mark), Gap(f64, f64, Gap), Idle, Done }
struct Annotation { sequence: u64, event: Event }
#[derive(Default)]
struct Counts { produced: f64, retained: f64, samples: u64, triggers: u64, captures: u64 }
struct Metrics { buckets: VecDeque<(Instant, Counts)>, started: Instant }
impl Metrics {
    fn new() -> Self { Self { buckets: VecDeque::new(), started: Instant::now() } }
    fn update(&mut self, counts: Counts, shared: &Shared) {
        let now = Instant::now();
        while self.buckets.front().is_some_and(|(t, _)| now.duration_since(*t) >= Duration::from_secs(1)) { self.buckets.pop_front(); }
        if self.buckets.back().is_none_or(|(t, _)| now.duration_since(*t) >= CHECK) { self.buckets.push_back((now, Counts::default())); }
        let bucket = &mut self.buckets.back_mut().unwrap().1;
        bucket.produced += counts.produced; bucket.retained += counts.retained;
        bucket.samples += counts.samples; bucket.triggers += counts.triggers; bucket.captures += counts.captures;
        let mut total = Counts::default();
        for (_, b) in &self.buckets { total.produced += b.produced; total.retained += b.retained; total.samples += b.samples; total.triggers += b.triggers; total.captures += b.captures; }
        let seconds = now.duration_since(self.started).as_secs_f64().min(1.0).max(1e-9);
        shared.samples.store((total.samples as f64 / seconds).to_bits(), Ordering::Relaxed);
        shared.captures.store((total.captures as f64 / seconds).to_bits(), Ordering::Relaxed);
        shared.triggers.store((total.triggers as f64 / seconds).to_bits(), Ordering::Relaxed);
        shared.coverage.store((if total.produced > 0.0 { (total.retained / total.produced).clamp(0.0, 1.0) } else { 1.0 }).to_bits(), Ordering::Relaxed);
        shared.updated.store(now.duration_since(shared.started).as_nanos() as u64, Ordering::Release);
    }
}
struct Output {
    blocks: SyncSender<Packet>, events: Sender<Annotation>, returned: Receiver<Buffer>, wake: SyncSender<()>, shared: Arc<Shared>,
    sequence: u64,
    /// The span of the blocks dropped since the last one the ring took: one gap however long
    /// the intake stalls, sent just before the next retained block, layout, mark, gap or the end.
    /// An idle does not flush it: the intake needs the gap only before the data that follows it.
    overrun: Option<(f64, f64)>,
    spare: Vec<Buffer>, previous: Option<f64>, nominal_dt: f64, marked: Option<f64>, active: bool, metrics: Metrics, step: Counts,
}
impl Output {
    fn event(&mut self, event: Event) {
        self.flush_overrun();
        self.post(event);
    }
    fn post(&mut self, event: Event) {
        let _ = self.events.send(Annotation { sequence: self.sequence, event });
        self.sequence += 1;
        let _ = self.wake.try_send(());
    }
    fn flush_overrun(&mut self) {
        if let Some((from, to)) = self.overrun.take() { self.post(Event::Gap(from, to, Gap::Overrun)); }
    }
    fn buffer(&mut self) -> Buffer {
        while let Ok(buffer) = self.returned.try_recv() { self.spare.push(buffer); }
        self.spare.pop().unwrap_or_default()
    }
    fn send(&mut self, buffer: Buffer, from: f64, to: f64, duration: f64, samples: u64) -> bool {
        self.step.produced += duration;
        // A pending overrun gap takes the sequence number before this block's.
        let sequence = self.sequence + self.overrun.is_some() as u64;
        match self.blocks.try_send(Packet { sequence, buffer }) {
            Ok(()) => {
                self.flush_overrun();
                // Only retained data makes the next idle worth sending, so a stall adds no entries.
                self.active = true;
                self.sequence += 1; self.step.retained += duration; self.step.samples += samples;
                let _ = self.wake.try_send(());
                true
            }
            Err(TrySendError::Full(packet) | TrySendError::Disconnected(packet)) => {
                self.spare.push(packet.buffer);
                self.marked = None;
                match &mut self.overrun {
                    Some(span) => span.1 = span.1.max(to),
                    None => { self.shared.gaps.fetch_add(1, Ordering::Relaxed); self.overrun = Some((from, to)); }
                }
                false
            }
        }
    }
    fn idle(&mut self) { if self.active { self.post(Event::Idle); self.active = false; } }
    fn stats(&mut self) { self.metrics.update(std::mem::take(&mut self.step), &self.shared); }
}
impl BlockOut for Output {
    fn layout(&mut self, layout: &Layout) {
        self.shared.layout.store(layout.generation, Ordering::Release);
        self.previous = None; self.marked = None; self.event(Event::Layout(layout.clone()));
    }
    fn block(&mut self, block: &Block) {
        self.step.captures += 1;
        let n = block.chans.first().map_or(0, ChanData::len);
        assert!(block.dt.is_finite() && block.dt > 0.0 && block.t0.is_finite(), "device block clock must increase");
        assert!(block.chans.iter().all(|c| c.len() == n), "device block columns must have equal lengths");
        let mut buffer = self.buffer();
        buffer.is_point = false; buffer.index = block.index; buffer.t0 = block.t0; buffer.dt = block.dt; buffer.trigger = block.trigger;
        buffer.clipped.clear(); buffer.clipped.extend_from_slice(block.clipped);
        buffer.columns.resize_with(block.chans.len(), Vec::new);
        for (values, channel) in buffer.columns.iter_mut().zip(block.chans) {
            values.clear(); values.extend((0..n).map(|i| channel.value(i)));
        }
        let to = block.t0 + n as f64 * block.dt;
        let sent = self.send(buffer, block.t0, to, n as f64 * block.dt, n as u64);
        self.previous = (n > 0).then(|| block.t0 + (n - 1) as f64 * block.dt);
        self.marked = if sent { self.previous } else { None };
        if sent && block.trigger.is_some() { self.step.triggers += 1; }
    }
    fn point(&mut self, t: f64, values: &[f32]) {
        assert!(t.is_finite(), "device point time must be finite");
        let mut buffer = self.buffer();
        buffer.is_point = true; buffer.t0 = t; buffer.point.clear(); buffer.point.extend_from_slice(values);
        let from = self.previous.filter(|&p| p <= t).unwrap_or(t);
        let duration = if t > from { t - from } else { self.nominal_dt };
        self.previous = Some(t);
        self.marked = if self.send(buffer, from, t, duration, 1) { Some(t) } else { None };
    }
    fn mark(&mut self, t: f64, kind: Mark) {
        // A lost point has no matching intake point; its mark must not rejoin the stream.
        if self.marked == Some(t) {
            if kind == Mark::HardwareTrigger { self.step.triggers += 1; }
            self.event(Event::Mark(t, kind));
        }
    }
    fn gap(&mut self, from: f64, to: f64, why: Gap) {
        assert!(from.is_finite() && to.is_finite() && from <= to, "device gap must be a finite forward interval");
        self.shared.gaps.fetch_add(1, Ordering::Relaxed);
        self.marked = None;
        if why != Gap::Overrun { self.previous = None; }
        self.event(Event::Gap(from, to, why));
    }
}

fn run<D: Driver>(mut driver: D, requests: Receiver<Request>, mut out: Output, shared: Arc<Shared>) {
    let mut resolved: Option<Arc<Resolved>> = None;
    shared.publish(State::Idle, None, None);
    let mut request = None;
    while !shared.stop.load(Ordering::Acquire) {
        // There is only one queued request. Never apply an obsolete queued setup while a
        // newer one is retained by the UI for its next nonblocking retry.
        if let Ok(next) = requests.try_recv() { request = Some(next); }
        if let Some(next) = request.take().filter(|r| r.version == shared.newest.load(Ordering::Acquire)) {
            match driver.apply(&next.want) {
                Ok(mut answer) => {
                    answer.restart = resolved.as_ref().is_none_or(|old| answer.needs_restart(old));
                    if answer.restart && resolved.is_some() {
                        let t = out.previous.unwrap_or(0.0); out.gap(t, t, Gap::Restart);
                    }
                    out.nominal_dt = 1.0 / answer.rate;
                    let state = if answer.mode == Mode::Stream { State::Streaming } else { State::Armed };
                    resolved = Some(Arc::new(answer));
                    shared.publish(state, resolved.clone(), None);
                }
                Err(DriverError::Gone(error)) => { shared.publish(State::Gone, resolved.clone(), Some(error)); break; }
                Err(DriverError::Refused(error) | DriverError::Io(error)) => { shared.publish(State::Idle, resolved.clone(), Some(error)); }
            }
        }
        let pump = if resolved.is_some() && !shared.parked.load(Ordering::Acquire) { driver.pump(&mut out) } else { Pump::Idle(CHECK) };
        out.stats();
        match pump {
            Pump::Data => (),
            Pump::Idle(wait) => {
                out.idle();
                if let Ok(next) = requests.recv_timeout(wait.min(CHECK)) { request = Some(next); }
            }
            Pump::Gone(error) => { shared.publish(State::Gone, resolved.clone(), Some(error)); break; }
        }
    }
    out.stats();
    // Done follows every accepted block and every loss annotation, even on unplug.
    out.event(Event::Done);
    driver.stop();
}
struct Attached { id: AttachId, factory: Box<dyn IntakeFactory + Send>, intake: Option<Box<dyn Intake>> }
fn acquire(blocks: Receiver<Packet>, journal: Receiver<Annotation>, attached: Receiver<Attach>, returns: SyncSender<Buffer>, waking: Receiver<()>, shared: Arc<Shared>) {
    let mut intakes: Vec<Attached> = Vec::new();
    let mut layout: Option<Layout> = None;
    let mut block: Option<Packet> = None;
    let mut annotation: Option<Annotation> = None;
    let mut sequence = 0;
    loop {
        for _ in 0..64 {
            let Ok(command) = attached.try_recv() else { break };
            match command {
                Attach::Add(id, mut factory) => {
                    let intake = layout.as_ref().map(|layout| { let mut intake = factory.make(layout); intake.begin(layout); intake });
                    intakes.push(Attached { id, factory, intake });
                }
                Attach::Remove(id) => {
                    if let Some(i) = intakes.iter().position(|a| a.id == id) {
                        let mut removed = intakes.swap_remove(i);
                        if let Some(intake) = &mut removed.intake { intake.idle(); }
                    }
                }
            }
        }
        if block.is_none() { block = blocks.try_recv().ok(); }
        if annotation.is_none() { annotation = journal.try_recv().ok(); }
        if block.as_ref().is_some_and(|b| b.sequence == sequence) {
            let buffer = block.take().unwrap().buffer;
            if buffer.is_point {
                for a in &mut intakes { if let Some(i) = &mut a.intake { i.point(buffer.t0, &buffer.point); } }
            } else {
                let chans: Vec<_> = buffer.columns.iter().map(|v| ChanData::F32(v)).collect();
                let b = Block { index: buffer.index, t0: buffer.t0, dt: buffer.dt, chans: &chans, trigger: buffer.trigger, clipped: &buffer.clipped };
                for a in &mut intakes { if let Some(i) = &mut a.intake { i.block(&b); } }
            }
            let _ = returns.try_send(buffer);
            sequence += 1;
        } else if annotation.as_ref().is_some_and(|a| a.sequence == sequence) {
            let event = annotation.take().unwrap().event;
            match event {
                Event::Layout(next) => {
                    for a in &mut intakes {
                        if let Some(i) = &mut a.intake { i.idle(); }
                        let mut intake = a.factory.make(&next); intake.begin(&next); a.intake = Some(intake);
                    }
                    layout = Some(next);
                }
                Event::Mark(t, kind) => for a in &mut intakes { if let Some(i) = &mut a.intake { i.mark(t, kind); } },
                Event::Gap(from, to, why) => for a in &mut intakes { if let Some(i) = &mut a.intake { i.gap(from, to, why); } },
                Event::Idle => for a in &mut intakes { if let Some(i) = &mut a.intake { i.idle(); } },
                Event::Done => break,
            }
            sequence += 1;
        } else {
            if shared.stop.load(Ordering::Acquire) { break; }
            let _ = waking.recv_timeout(CHECK);
        }
    }
    for a in &mut intakes { if let Some(i) = &mut a.intake { i.idle(); } }
}
