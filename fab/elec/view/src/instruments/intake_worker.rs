//! A scope's acquisition on its own worker. A simulation's sink still maps each accepted point to
//! lane values on the solver thread, then hands them on in batches: triggering, the open windows'
//! strokes and the publications run beside the solver instead of inside its step.
//!
//! - One worker per scope feed, started with it and fed by one bounded channel; the feed's nominal
//!   intakes run there. A link ships its intake with its first batch, so a sink that never runs
//!   installs nothing.
//! - Order: a link's operations reach its intake in the order they were made.
//! - A full channel makes the solver wait for the worker: a simulation never drops a point.
//! - A link that drops leaves its intake to the worker, which drops it (an intake publishes what
//!   it holds on drop) once every batch the link sent has been applied.

use fab_elec_bench::source::{Block, ChanData, Clipped, Gap, Intake, Layout, Mark};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Batches in the channel to the worker; a full channel makes the solver wait.
pub const SLOTS: usize = 8;
/// A batch leaves when it holds this many points, or once its first operation is this old.
const BATCH_POINTS: usize = 512;
const BATCH_AGE: Duration = Duration::from_millis(2);
/// How often an idle worker looks for dropped links and the feed's stop.
const SWEEP: Duration = Duration::from_millis(100);

static NEXT_LINK: AtomicU64 = AtomicU64::new(1);

enum Op {
    Begin(Layout),
    /// A point's time and how many of the batch's values are its own.
    Point(f64, u32),
    Block(OwnedBlock),
    Mark(f64, Mark),
    Gap(f64, f64, Gap),
    Idle,
    Steady(f64),
}

enum OwnedChan {
    F32(Vec<f32>),
    I16 { raw: Vec<i16>, scale: f32, offset: f32 },
    U8 { raw: Vec<u8>, scale: f32, offset: f32 },
    I8 { raw: Vec<i8>, scale: f32, offset: f32 },
}

struct OwnedBlock {
    index: u64,
    t0: f64,
    dt: f64,
    trigger: Option<f64>,
    clipped: Vec<Clipped>,
    chans: Vec<OwnedChan>,
}

impl OwnedBlock {
    fn new(block: &Block) -> OwnedBlock {
        let chans = block.chans.iter().map(|chan| match *chan {
            ChanData::F32(raw) => OwnedChan::F32(raw.to_vec()),
            ChanData::I16 { raw, scale, offset } => OwnedChan::I16 { raw: raw.to_vec(), scale, offset },
            ChanData::U8 { raw, scale, offset } => OwnedChan::U8 { raw: raw.to_vec(), scale, offset },
            ChanData::I8 { raw, scale, offset } => OwnedChan::I8 { raw: raw.to_vec(), scale, offset },
        }).collect();
        OwnedBlock { index: block.index, t0: block.t0, dt: block.dt, trigger: block.trigger, clipped: block.clipped.to_vec(), chans }
    }

    fn apply(&self, intake: &mut dyn Intake) {
        let chans: Vec<ChanData> = self.chans.iter().map(|chan| match chan {
            OwnedChan::F32(raw) => ChanData::F32(raw),
            OwnedChan::I16 { raw, scale, offset } => ChanData::I16 { raw, scale: *scale, offset: *offset },
            OwnedChan::U8 { raw, scale, offset } => ChanData::U8 { raw, scale: *scale, offset: *offset },
            OwnedChan::I8 { raw, scale, offset } => ChanData::I8 { raw, scale: *scale, offset: *offset },
        }).collect();
        intake.block(&Block { index: self.index, t0: self.t0, dt: self.dt, chans: &chans, trigger: self.trigger, clipped: &self.clipped });
    }
}

/// What the link and the worker share: batches not yet applied, whether the intake holds output
/// its consumer refused after the last one, and whether the link has dropped.
#[derive(Default)]
struct LinkState {
    inflight: AtomicU32,
    pending: AtomicBool,
    closed: AtomicBool,
}

/// The intake itself, sent with a link's first batch.
struct Install {
    intake: Box<dyn Intake>,
    state: Arc<LinkState>,
    spares: SyncSender<Batch>,
}

/// Operations of one link, in order, with every point's values back to back.
pub struct Batch {
    link: u64,
    install: Option<Install>,
    ops: Vec<Op>,
    values: Vec<f32>,
}

impl Batch {
    fn new(link: u64) -> Batch {
        Batch { link, install: None, ops: Vec::new(), values: Vec::new() }
    }
}

/// The solver's side: an intake that hands its operations to the worker running the real one.
pub struct IntakeLink {
    id: u64,
    send: SyncSender<Batch>,
    install: Option<Install>,
    batch: Batch,
    points: usize,
    since: Option<Instant>,
    spares: Receiver<Batch>,
    state: Arc<LinkState>,
}

impl IntakeLink {
    pub fn new(send: &SyncSender<Batch>, intake: impl Intake + 'static) -> IntakeLink {
        let id = NEXT_LINK.fetch_add(1, Ordering::Relaxed);
        let (spares_send, spares) = sync_channel(SLOTS + 1);
        let state = Arc::new(LinkState::default());
        let install = Install { intake: Box::new(intake), state: state.clone(), spares: spares_send };
        IntakeLink { id, send: send.clone(), install: Some(install), batch: Batch::new(id), points: 0, since: None, spares, state }
    }

    fn push(&mut self, op: Op) {
        if self.batch.ops.is_empty() {
            self.since = Some(Instant::now());
        }
        self.batch.ops.push(op);
    }

    fn flush(&mut self) {
        if self.batch.ops.is_empty() {
            return;
        }
        let next = self.spares.try_recv().unwrap_or_else(|_| Batch::new(self.id));
        let mut batch = std::mem::replace(&mut self.batch, next);
        batch.install = self.install.take();
        (self.points, self.since) = (0, None);
        self.state.inflight.fetch_add(1, Ordering::AcqRel);
        // Waits while the worker is behind. An error is a stopped feed: its scope is gone, and the
        // batch drops here with the intake it may carry.
        if self.send.send(batch).is_err() {
            self.state.inflight.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Intake for IntakeLink {
    fn begin(&mut self, layout: &Layout) {
        self.push(Op::Begin(layout.clone()));
    }
    fn point(&mut self, t: f64, y: &[f32]) {
        self.push(Op::Point(t, y.len() as u32));
        self.batch.values.extend_from_slice(y);
        self.points += 1;
        if self.points >= BATCH_POINTS || self.since.is_some_and(|since| since.elapsed() >= BATCH_AGE) {
            self.flush();
        }
    }
    fn block(&mut self, block: &Block) {
        self.push(Op::Block(OwnedBlock::new(block)));
        self.flush();
    }
    fn mark(&mut self, t: f64, kind: Mark) {
        self.push(Op::Mark(t, kind));
    }
    fn gap(&mut self, from: f64, to: f64, why: Gap) {
        self.push(Op::Gap(from, to, why));
    }
    fn idle(&mut self) {
        self.push(Op::Idle);
        self.flush();
    }
    fn steady(&mut self, at: f64) {
        self.push(Op::Steady(at));
        self.flush();
    }
    /// Until the worker has applied every batch, and while the intake holds refused output: a
    /// resting run then offers `idle` again, so nothing stays unpublished.
    fn pending(&self) -> bool {
        !self.batch.ops.is_empty() || self.state.inflight.load(Ordering::Acquire) > 0 || self.state.pending.load(Ordering::Acquire)
    }
}

impl Drop for IntakeLink {
    /// The last operations go too (a link holds operations only on the solver thread that made
    /// them); the worker drops the intake once it has applied them.
    fn drop(&mut self) {
        self.flush();
        self.state.closed.store(true, Ordering::Release);
    }
}

struct Running {
    link: u64,
    intake: Box<dyn Intake>,
    state: Arc<LinkState>,
    spares: SyncSender<Batch>,
}

fn apply(running: &mut Vec<Running>, mut batch: Batch) {
    if let Some(Install { intake, state, spares }) = batch.install.take() {
        running.push(Running { link: batch.link, intake, state, spares });
    }
    let Some(run) = running.iter_mut().find(|run| run.link == batch.link) else { return };
    let mut at = 0;
    for op in batch.ops.drain(..) {
        match op {
            Op::Begin(layout) => run.intake.begin(&layout),
            Op::Point(t, n) => {
                let end = at + n as usize;
                run.intake.point(t, &batch.values[at..end]);
                at = end;
            }
            Op::Block(block) => block.apply(&mut *run.intake),
            Op::Mark(t, kind) => run.intake.mark(t, kind),
            Op::Gap(from, to, why) => run.intake.gap(from, to, why),
            Op::Idle => run.intake.idle(),
            Op::Steady(at) => run.intake.steady(at),
        }
    }
    batch.values.clear();
    run.state.pending.store(run.intake.pending(), Ordering::Release);
    run.state.inflight.fetch_sub(1, Ordering::AcqRel);
    let _ = run.spares.try_send(batch);
}

/// The worker: applies batches until every sender is gone or `stopped` says the feed has stopped.
pub fn run(batches: Receiver<Batch>, stopped: impl Fn() -> bool) {
    let mut running = Vec::<Running>::new();
    loop {
        match batches.recv_timeout(SWEEP) {
            Ok(batch) => apply(&mut running, batch),
            Err(RecvTimeoutError::Timeout) => {
                if stopped() {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // A link that closed before this check queued its last batch before it: apply what is
        // queued, then drop its intake.
        if running.iter().any(|run| run.state.closed.load(Ordering::Acquire)) {
            let gone: Vec<u64> = running.iter().filter(|run| run.state.closed.load(Ordering::Acquire)).map(|run| run.link).collect();
            while let Ok(batch) = batches.try_recv() {
                apply(&mut running, batch);
            }
            running.retain(|run| !gone.contains(&run.link));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_bench::source::{ChanId, ChannelInfo, ClockKind, Quantity};
    use std::sync::Mutex;
    use std::thread::ThreadId;

    #[derive(Debug, PartialEq)]
    enum Seen { Begin(u64), Point(f64, Vec<f32>), Mark(f64), Gap(f64, f64), Idle, Steady(f64), Dropped }

    struct Recorder { seen: Arc<Mutex<Vec<(Seen, ThreadId)>>> }
    impl Recorder {
        fn note(&self, seen: Seen) {
            self.seen.lock().unwrap().push((seen, std::thread::current().id()));
        }
    }
    impl Intake for Recorder {
        fn begin(&mut self, layout: &Layout) { self.note(Seen::Begin(layout.generation)); }
        fn point(&mut self, t: f64, y: &[f32]) { self.note(Seen::Point(t, y.to_vec())); }
        fn block(&mut self, _: &Block) {}
        fn mark(&mut self, t: f64, _: Mark) { self.note(Seen::Mark(t)); }
        fn gap(&mut self, from: f64, to: f64, _: Gap) { self.note(Seen::Gap(from, to)); }
        fn idle(&mut self) { self.note(Seen::Idle); }
        fn steady(&mut self, at: f64) { self.note(Seen::Steady(at)); }
    }
    impl Drop for Recorder {
        fn drop(&mut self) { self.note(Seen::Dropped); }
    }

    /// Every operation reaches the intake in order, on the worker, across batch boundaries and
    /// through the link's drop; the intake drops there only after the link's last batch, and the
    /// worker ends with its senders.
    #[test]
    fn a_link_hands_every_operation_to_its_intake_in_order_on_the_worker() {
        let (send, batches) = sync_channel(SLOTS);
        let worker = std::thread::spawn(move || run(batches, || false));
        let worker_id = worker.thread().id();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut link = IntakeLink::new(&send, Recorder { seen: seen.clone() });
        drop(send);
        let mut want = vec![Seen::Begin(7)];
        link.begin(&Layout { generation: 7, clock: ClockKind::Simulated, channels: vec![ChannelInfo { id: ChanId(0), name: "a".into(), quantity: Quantity::Volt }] });
        // Several full batches and a partial one, with marks between points.
        for i in 0..(3 * BATCH_POINTS + 17) {
            let (t, y) = (i as f64, vec![i as f32, -(i as f32)]);
            link.point(t, &y);
            want.push(Seen::Point(t, y));
            if i % 300 == 0 {
                link.mark(t, Mark::Solver);
                want.push(Seen::Mark(t));
            }
        }
        link.gap(1e4, 2e4, Gap::Blind);
        link.idle();
        link.steady(2e4);
        // Points after the last flush leave with the link's drop.
        link.point(3e4, &[1.0, 2.0]);
        drop(link);
        want.extend([Seen::Gap(1e4, 2e4), Seen::Idle, Seen::Steady(2e4), Seen::Point(3e4, vec![1.0, 2.0]), Seen::Dropped]);
        worker.join().expect("the worker ends once its senders are gone");
        let seen = std::mem::take(&mut *seen.lock().unwrap());
        assert!(seen.iter().all(|(_, thread)| *thread == worker_id && *thread != std::thread::current().id()),
            "the intake runs on the worker, never on the solver's thread");
        assert_eq!(seen.into_iter().map(|(seen, _)| seen).collect::<Vec<_>>(), want, "in order, nothing lost, dropped last");
    }
}
