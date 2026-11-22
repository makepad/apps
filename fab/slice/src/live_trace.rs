//! A live slice's run as a future: every poll is a pool job at the urgency of the run's phase,
//! and the run yields to change urgency. Its parts' layers are cut on the cores.
use super::*;
use std::{future::Future, pin::Pin, sync::{Weak, atomic::{AtomicBool, AtomicU8}}, task::{Context, Poll, Wake, Waker}};

/// A run's continuations: from its input's checkpoint, the spawn every poll of that run goes
/// through, told each poll's urgency ([`LiveEngine::set_resume`]).
pub type ResumeHook = Arc<dyn Fn(Checkpoint) -> Spawn + Send + Sync>;

/// The running driver, and how runs reach the pool.
#[derive(Default)]
pub(super) struct Drive {
    pub resume: Option<ResumeHook>,
    pub driver: Option<Arc<Driver>>,
}

impl LiveEngine {
    /// The engine's scheduling state in one line: the input and served revisions, whether a
    /// driver runs, and the driver's phase and flags. A slice that does not go on shows here why.
    pub fn state_line(&self) -> String {
        let state = {
            let s = self.shared.state.lock().unwrap();
            format!("revision {} served {} running {} enabled {} pending {} input {} moving {} settle {:?} stale {}",
                s.revision, s.served, s.running, s.enabled, s.pending.is_some(), s.input.is_some(), s.moving, s.settle, (s.checkpoint)())
        };
        let driver = self.shared.drive.lock().unwrap().driver.clone();
        let driver = driver.map_or_else(|| "none".to_string(), |d| format!("revision {} phase {} cancelled {} queued {} finished {} future {} stale {}",
            d.revision, d.phase.load(Ordering::Acquire), d.cancelled.load(Ordering::Acquire), d.queued.load(Ordering::Acquire),
            d.finished.load(Ordering::Acquire), d.future.try_lock().map_or("polling", |f| if f.is_some() { "parked" } else { "none" }), (d.checkpoint)()));
        format!("{state}; driver: {driver}")
    }

    /// Blocks until no driver runs: every revision asked for was served, or slicing is off. A
    /// wait on the engine's own change notification, not on the clock.
    pub fn wait_idle(&self) {
        self.shared.wait_for(|| !self.shared.state.lock().unwrap().running);
    }

    /// The newest input's revision and the revision its slice was served for: neither moves
    /// while nothing asks for a new slice.
    pub fn revisions(&self) -> (u64, u64) {
        let s = self.shared.state.lock().unwrap();
        (s.revision, s.served)
    }

    /// How each run's polls reach the pool: a host captures the run's input ticket here, so a
    /// newer input skips a stale run's polls by that input's own ticket. Unset: the engine's spawn.
    pub fn set_resume(&self, resume: ResumeHook) {
        self.shared.drive.lock().unwrap().resume = Some(resume);
    }
}

/// Part `i`'s layers `ks` cut on the cores, the viewed layer's first: whether every one was
/// (false: `cancel` stopped it).
pub(super) fn trace(shared: &Shared, slot: &Slot, t: &mut Trace, mut ks: Vec<usize>, cancel: Option<&(dyn Fn() -> bool + Sync)>, meter: &Meter<'_>, work: &Arc<PartWork>) -> bool {
    ks.retain(|&k| t.todo(k));
    if ks.is_empty() { return true }
    let zs: Vec<_> = ks.iter().map(|&k| t.zs[k]).collect();
    let focus = t.focus.map(|f| ks.partition_point(|&k| k < f));
    let tick = |n: usize| meter.done(n as f64);
    let traced = {
        let _scope = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::Contours);
        contours_some(par::Ctl { cancel, focus, emit: None, tick: Some(&tick), phase: None }, &*slot.src, &zs)
    };
    let mut whole = true;
    for (k, r) in ks.into_iter().zip(traced) {
        if let Some(r) = r {
            shared.traced.fetch_add(1, Ordering::Relaxed);
            work.traced.fetch_add(1, Ordering::Relaxed);
            t.traced[k] = Some(CutLayer { regions: Regions::from(r) });
        } else { whole = false; }
    }
    whole
}

/// The driver's phase ([`run`] lowers it): its edited parts' viewed layers, which a newer
/// input lets finish; the other parts' viewed layers; the rest.
pub(super) const PROTECTED: u8 = 0;
pub(super) const FOCUS: u8 = 1;
pub(super) const REST: u8 = 2;

/// Ends a poll, so that the next one is spawned at the urgency the phase has now.
#[derive(Default)]
pub(super) struct Yield(bool);
impl Future for Yield {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 { return Poll::Ready(()) }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;
pub(super) struct Driver {
    shared: Weak<Shared>,
    future: Mutex<Option<Job>>,
    queued: AtomicBool,
    cancelled: AtomicBool,
    finished: AtomicBool,
    revision: u64,
    settle: Option<u64>,
    checkpoint: Checkpoint,
    spawn: Spawn,
    phase: Arc<AtomicU8>,
}
struct DriverWake(Weak<Driver>);
impl Wake for DriverWake {
    fn wake(self: Arc<Self>) { if let Some(driver) = self.0.upgrade() { driver.schedule(); } }
}
impl Driver {
    pub fn start(shared: Arc<Shared>, snap: Snapshot) {
        let resume = shared.drive.lock().unwrap().resume.clone();
        let spawn = resume.map_or_else(|| shared.spawn.clone(), |r| r(snap.checkpoint.clone()));
        let phase = Arc::new(AtomicU8::new(PROTECTED));
        let driver = Arc::new(Self { shared: Arc::downgrade(&shared), future: Mutex::new(None), queued: AtomicBool::new(false),
            cancelled: AtomicBool::new(false), finished: AtomicBool::new(false), revision: snap.revision, settle: snap.settle,
            checkpoint: snap.checkpoint.clone(), spawn, phase: phase.clone() });
        let s = shared.clone();
        *driver.future.lock().unwrap() = Some(Box::pin(async move { run(&s, &snap, &phase).await; }));
        shared.drive.lock().unwrap().driver = Some(driver.clone());
        shared.note_change();
        // Superseded before it began (while its input was built): like a driver superseded
        // later, it ends past what it has in hand.
        if shared.state.lock().unwrap().revision != driver.revision { driver.cancel(); } else { driver.poll(); }
    }
    #[cfg(test)]
    pub fn revision(&self) -> u64 { self.revision }
    /// Its next poll is asked for and not run yet.
    #[cfg(test)]
    pub fn waiting(&self) -> bool {
        self.queued.load(Ordering::Acquire) && self.future.try_lock().is_ok_and(|f| f.is_some())
    }
    fn urgency(&self) -> Urgency {
        match self.phase.load(Ordering::Acquire) {
            PROTECTED => Urgency::Viewed,
            FOCUS => Urgency::Focus,
            _ => Urgency::Rest,
        }
    }
    /// Cancelled, and past its edited parts' viewed layers (which a newer input lets finish).
    fn dropped(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) && self.phase.load(Ordering::Acquire) != PROTECTED
    }
    fn schedule(self: &Arc<Self>) {
        if !self.queued.swap(true, Ordering::AcqRel) {
            let driver = self.clone();
            (self.spawn)(self.urgency(), Box::new(move || driver.poll()));
            if let Some(shared) = self.shared.upgrade() { shared.note_change(); }
        }
    }
    fn poll(self: &Arc<Self>) {
        // Only the swap is locked, never planning. A wake during a poll is
        // remembered and resubmitted once the future is available again.
        let Some(mut future) = self.future.lock().unwrap().take() else { return };
        self.queued.store(false, Ordering::Release);
        if self.dropped() { drop(future); self.finish(false); return }
        let waker = Waker::from(Arc::new(DriverWake(Arc::downgrade(self))));
        let result = catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut Context::from_waker(&waker))));
        match result {
            Ok(Poll::Pending) if !self.dropped() => {
                *self.future.lock().unwrap() = Some(future);
                if self.dropped() { self.cancel(); }
                else if self.queued.swap(false, Ordering::AcqRel) { self.schedule(); }
                // Parked again: a wait for the driver's next poll looks now.
                if let Some(shared) = self.shared.upgrade() { shared.note_change(); }
            }
            Ok(_) => { drop(future); self.finish(false); }
            Err(p) => {
                drop(future);
                if let Some(shared) = self.shared.upgrade() { panic_failed(&shared, self.revision, self.settle, p); }
                self.finish(true);
            }
        }
    }
    /// Input `revision` came: a driver of an older one stops past its edited parts' viewed
    /// layers; one of `revision` (or newer) runs that input already and goes on.
    pub fn supersede(self: &Arc<Self>, revision: u64) {
        if self.revision < revision { self.cancel(); }
    }
    fn cancel(self: &Arc<Self>) {
        self.cancelled.store(true, Ordering::Release);
        // Cancellation cleanup itself has no stale ticket: it must release the
        // suspended state and start the newest input, even when a pool skips a job.
        if let Some(shared) = self.shared.upgrade() {
            let driver = self.clone();
            (shared.spawn)(Urgency::Viewed, Box::new(move || driver.poll()));
        }
    }
    fn finish(&self, failed: bool) {
        if self.finished.swap(true, Ordering::AcqRel) { return }
        if let Some(shared) = self.shared.upgrade() {
            shared.drive.lock().unwrap().driver = None;
            shared.note_change();
            drive_finished(shared, self.revision, self.settle, &self.checkpoint, failed);
        }
    }
}
