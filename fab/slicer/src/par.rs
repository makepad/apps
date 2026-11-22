//! The layer workers every slicing pass shares: how many threads a map may
//! use, where its helpers run (a crew of long-lived threads by default; the
//! app's task pool when it says so), and a progress count a watchdog can read.
//! `toolpath::par` builds its cancellable, focus-ordered maps on this.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

/// Runs `f(i)` for every `i in 0..len`, on the calling thread and helpers,
/// and returns when all have run. Calls never wait on each other, so a
/// helper that never starts only costs parallelism (the caller's own calls
/// finish the work).
pub trait FanOut: Send + Sync {
    fn run(&self, len: usize, f: &(dyn Fn(usize) + Sync));
    /// `lead` on the caller, and `helper` on one more thread each time a call recruits one
    /// ([`Crew::recruit`]); returns once every call has. A helper holds its thread (and in the
    /// app its CPU permit) only while it has work: one with nothing to do returns, and work that
    /// comes later recruits another.
    fn crew(&self, lead: &(dyn Fn(&dyn Crew) + Sync), helper: &(dyn Fn(&dyn Crew) + Sync));
}

/// A crew's calls' handle on it ([`crew`]).
pub trait Crew: Sync {
    /// Starts one more helper. The lead works too, so a crew needs none of its helpers to
    /// start before it finishes.
    fn recruit(&self);
    /// Runs `f`, which waits for the crew's helpers, without the caller's share of the cores
    /// (the app's CPU permit): hold no lock across it that a helper needs.
    fn wait(&self, f: &mut dyn FnMut());
    /// A helper asks this at each task boundary: true when it should give its thread back now
    /// (in the app, a higher-class job waits for a pool worker). It leaves as it does when no
    /// task is ready; the lead recruits again for the work left.
    fn leave(&self) -> bool;
}

/// The default fan-out ([`set_fan_out`]): a crew of long-lived std threads, started as maps first
/// ask for them (up to [`threads`] less the caller) and kept for the process, so no map spawns a
/// thread of its own (it spawned up to [`threads`] less one a map: 13.7k threads in a roadster's
/// slice, 11-19). A helper keeps its thread-local scratch (the clipper's pools) from one map to the
/// next, as the app's pool workers do. No call waits for a helper that has not started: a map that
/// finds every helper busy (a map inside a map) runs on its caller, and a crew's helpers that no
/// thread took run on its lead after it returns.
pub struct ScopedThreads;

/// What a call's helpers run: the map's indices, or the crew's helper.
enum Task {
    Map { next: AtomicUsize, len: usize, f: *const (dyn Fn(usize) + Sync + 'static) },
    Crew { helper: *const (dyn Fn(&dyn Crew) + Sync + 'static) },
}

/// One `run` or `crew` call, shared with the helpers that join it. The borrowed closures are
/// only called between a helper's entry (counted under the queue's lock) and its exit, and the
/// call returns only once no helper is inside and none of its offers is queued.
struct Call {
    task: Task,
    inside: AtomicUsize,
    exited: std::sync::Mutex<()>,
    exit: std::sync::Condvar,
    panic: std::sync::Mutex<Option<Box<dyn std::any::Any + Send>>>,
}

// SAFETY: the raw closure pointers are `Sync` closures, used only while their call waits (above).
unsafe impl Send for Call {}
unsafe impl Sync for Call {}

impl Call {
    fn new(task: Task) -> Arc<Call> {
        Arc::new(Call { task, inside: AtomicUsize::new(0), exited: std::sync::Mutex::new(()), exit: std::sync::Condvar::new(), panic: std::sync::Mutex::new(None) })
    }

    /// The task, on a helper or (a crew's unstarted helper) on the caller.
    fn work(self: &Arc<Call>) {
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &self.task {
            // SAFETY: inside the call (see `Call`).
            Task::Map { next, len, f } => loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= *len { return; }
                unsafe { (**f)(i) };
            },
            Task::Crew { helper } => unsafe { (**helper)(&CrewOf(self)) },
        }));
        if let Err(payload) = ran {
            self.panic.lock().unwrap().get_or_insert(payload);
        }
    }

    /// Offers the call to `n` more helpers, starting threads up to [`threads`] less one.
    fn offer(self: &Arc<Call>, n: usize) {
        let mut queue = CREW.queue.lock().unwrap();
        while queue.threads + 1 < threads() {
            queue.threads += 1;
            std::thread::Builder::new().name("fab-par".into()).spawn(helper_thread).expect("a fan-out helper thread");
        }
        for _ in 0..n {
            queue.offers.push_back(self.clone());
        }
        drop(queue);
        for _ in 0..n {
            CREW.wake.notify_one();
        }
    }

    /// Returns once no helper is inside the call and none of its offers is left: the offers no
    /// helper took are withdrawn, a crew's run here on the caller. Then a helper's panic unwinds here.
    fn finish(self: &Arc<Call>) {
        loop {
            let withdrawn = {
                let mut queue = CREW.queue.lock().unwrap();
                let before = queue.offers.len();
                queue.offers.retain(|c| !Arc::ptr_eq(c, self));
                if before == queue.offers.len() && self.inside.load(Ordering::SeqCst) == 0 {
                    break;
                }
                before - queue.offers.len()
            };
            if let Task::Crew { .. } = self.task {
                for _ in 0..withdrawn {
                    self.work();
                }
            }
            let mut exited = self.exited.lock().unwrap();
            while self.inside.load(Ordering::SeqCst) > 0 {
                exited = self.exit.wait(exited).unwrap();
            }
        }
        if let Some(payload) = self.panic.lock().unwrap().take() {
            std::panic::resume_unwind(payload);
        }
    }
}

/// The crew's helpers and the calls offered to them.
struct Helpers {
    queue: std::sync::Mutex<Queue>,
    wake: std::sync::Condvar,
}

struct Queue {
    offers: std::collections::VecDeque<Arc<Call>>,
    threads: usize,
}

static CREW: Helpers = Helpers { queue: std::sync::Mutex::new(Queue { offers: std::collections::VecDeque::new(), threads: 0 }), wake: std::sync::Condvar::new() };

/// A helper thread: the oldest offer, until the process ends.
fn helper_thread() {
    let mut queue = CREW.queue.lock().unwrap();
    loop {
        let Some(call) = queue.offers.pop_front() else {
            queue = CREW.wake.wait(queue).unwrap();
            continue;
        };
        // entered under the queue's lock, so a withdrawal sees every helper inside
        call.inside.fetch_add(1, Ordering::SeqCst);
        drop(queue);
        call.work();
        {
            let _exited = call.exited.lock().unwrap();
            call.inside.fetch_sub(1, Ordering::SeqCst);
            call.exit.notify_all();
        }
        drop(call);
        queue = CREW.queue.lock().unwrap();
    }
}

/// A crew's handle for its lead and helpers.
struct CrewOf<'c>(&'c Arc<Call>);

impl Crew for CrewOf<'_> {
    fn recruit(&self) {
        self.0.offer(1);
    }
    fn wait(&self, f: &mut dyn FnMut()) {
        f()
    }
    fn leave(&self) -> bool {
        false
    }
}

impl FanOut for ScopedThreads {
    fn run(&self, len: usize, f: &(dyn Fn(usize) + Sync)) {
        let helpers = threads().min(len).saturating_sub(1);
        if helpers == 0 {
            (0..len).for_each(f);
            return;
        }
        // SAFETY: `finish` returns only once no helper is inside the call.
        let f: *const (dyn Fn(usize) + Sync + 'static) = unsafe { std::mem::transmute(f) };
        let call = Call::new(Task::Map { next: AtomicUsize::new(0), len, f });
        call.offer(helpers);
        call.work();
        call.finish();
    }
    fn crew(&self, lead: &(dyn Fn(&dyn Crew) + Sync), helper: &(dyn Fn(&dyn Crew) + Sync)) {
        // SAFETY: as in `run`.
        let helper: *const (dyn Fn(&dyn Crew) + Sync + 'static) = unsafe { std::mem::transmute(helper) };
        let call = Call::new(Task::Crew { helper });
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lead(&CrewOf(&call))));
        call.finish();
        if let Err(payload) = ran {
            std::panic::resume_unwind(payload);
        }
    }
}

static THREADS: AtomicUsize = AtomicUsize::new(1);
static PROGRESS: AtomicU64 = AtomicU64::new(0);
static FAN_OUT: RwLock<Option<Arc<dyn FanOut>>> = RwLock::new(None);

thread_local!(static HELPER_NS: std::cell::Cell<Option<&'static AtomicU64>> = const { std::cell::Cell::new(None) });

/// `f`, with the time helpers spend on the fan-outs `f` starts on this
/// thread added to `ns` (the caller's own share is inside its wall time):
/// for a stage timer whose work fans out where it cannot open a scope.
pub fn timing_helpers<T>(ns: &'static AtomicU64, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<&'static AtomicU64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            HELPER_NS.with(|h| h.set(self.0));
        }
    }
    let _restore = Restore(HELPER_NS.with(|h| h.replace(Some(ns))));
    f()
}

/// Threads a map may use (1 = run on the caller only).
pub fn set_threads(n: usize) {
    THREADS.store(n.max(1), Ordering::Relaxed);
}

pub fn threads() -> usize {
    THREADS.load(Ordering::Relaxed)
}

/// The threads a run's maps may take of `machine` (what it would take of its box): fewer when
/// the run was given a CPU budget (`MAKEPAD_CPU_CORES`, a devhub lease's width, which its test
/// threads and JIT keep to too). Sized from the box, four Fab tests at once fanned out 8 each on
/// a lease told 4, and its box stood at 98% with 32 tasks waiting (11-19).
pub fn budget_threads(machine: usize) -> usize {
    std::env::var("MAKEPAD_CPU_CORES").ok().and_then(|v| v.trim().parse::<usize>().ok()).filter(|n| *n > 0).map_or(machine, |n| n.min(machine)).max(1)
}

/// Where helpers run (default: scoped std threads).
pub fn set_fan_out(f: Arc<dyn FanOut>) {
    *FAN_OUT.write().unwrap() = Some(f);
}

/// Indices started by every map so far (a hang shows as no change).
pub fn progress() -> u64 {
    PROGRESS.load(Ordering::Relaxed)
}

/// Count one started index.
pub fn tick() {
    PROGRESS.fetch_add(1, Ordering::Relaxed);
}

/// `f` for every index in `0..t`, on the caller and bounded helpers.
pub fn fan(t: usize, f: &(dyn Fn(usize) + Sync)) {
    let fan = FAN_OUT.read().unwrap().clone();
    let timer = HELPER_NS.with(|h| h.get()).map(|ns| (ns, std::thread::current().id()));
    // A task pool may swallow a helper's panic. Catch at the worker-job
    // boundary, join every helper, then unwind on
    // the caller so the live revision can fail and invalidate its caches.
    let panic = std::sync::Mutex::new(None);
    let work = |i| {
        let helper = timer.filter(|t| std::thread::current().id() != t.1).map(|t| (t.0, std::time::Instant::now()));
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(i))) {
            let mut first = panic.lock().unwrap();
            if first.is_none() {
                *first = Some(payload);
            }
        }
        if let Some((ns, t0)) = helper {
            ns.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
    };
    match fan.as_deref() {
        Some(fan) => fan.run(t, &work),
        None => ScopedThreads.run(t, &work),
    }
    if let Some(payload) = panic.into_inner().unwrap() {
        std::panic::resume_unwind(payload);
    }
}

/// `lead` on the caller and `helper` on every helper a call recruits ([`FanOut::crew`]), at
/// most as many as the work wants. A panic in any of them unwinds on the caller once all ended.
pub fn crew(lead: &(dyn Fn(&dyn Crew) + Sync), helper: &(dyn Fn(&dyn Crew) + Sync)) {
    let fan = FAN_OUT.read().unwrap().clone();
    let panic = std::sync::Mutex::new(None);
    let caught = |f: &(dyn Fn(&dyn Crew) + Sync), c: &dyn Crew| {
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(c))) {
            panic.lock().unwrap().get_or_insert(payload);
        }
    };
    let (lead, helper) = (|c: &dyn Crew| caught(lead, c), |c: &dyn Crew| caught(helper, c));
    match fan.as_deref() {
        Some(fan) => fan.crew(&lead, &helper),
        None => ScopedThreads.crew(&lead, &helper),
    }
    if let Some(payload) = panic.into_inner().unwrap() {
        std::panic::resume_unwind(payload);
    }
}

/// `(0..n).map(f).collect()`, in parallel; results at their index.
pub fn map<T: Send, F: Fn(usize) -> T + Sync>(n: usize, f: F) -> Vec<T> {
    // one thread too goes through the fan-out, which checkpoints before every index
    let slots: Vec<std::sync::Mutex<Option<T>>> = (0..n).map(|_| std::sync::Mutex::new(None)).collect();
    fan(n, &|i| {
        tick();
        let v = f(i);
        *slots[i].lock().unwrap() = Some(v);
    });
    slots.into_iter().map(|s| s.into_inner().unwrap().expect("every index ran")).collect()
}

/// Where a span ran: a pool worker (a dense per-thread index), the GPU, the encoder (a
/// submission's encoding) or a readback in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lane {
    Cpu(u16),
    Gpu,
    Encode,
    Readback,
}

/// The `layer` of a span that is not one layer's (a part's or the plate's).
pub const NO_LAYER: u32 = u32::MAX;

/// The `part` of a span that is the plate's (G-code, package, checks).
pub const NO_PART: u32 = u32::MAX;

/// One unit of work, for the bench's report. Times are ns on [`now_ns`]'s clock; `ready_ns` is
/// when it became runnable (every span of `after` ended, or it arrived), `after` every span it
/// waited on.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub id: u64,
    pub lane: Lane,
    pub ready_ns: u64,
    pub start_ns: u64,
    pub end_ns: u64,
    pub after: Vec<u64>,
    pub stage: &'static str,
    pub part: u32,
    pub layer: u32,
    pub revision: u64,
}

static RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SPANS: std::sync::Mutex<Vec<Span>> = std::sync::Mutex::new(Vec::new());
static SPAN_IDS: AtomicU64 = AtomicU64::new(1);
static WORKERS: AtomicUsize = AtomicUsize::new(0);
static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
static SETTLING: AtomicU64 = AtomicU64::new(0);

/// Record spans from now on (or stop): off by default, a check costs one relaxed load.
pub fn record_spans(on: bool) {
    EPOCH.get_or_init(std::time::Instant::now);
    RECORDING.store(on, Ordering::Relaxed);
}

/// Spans are being recorded.
pub fn recording() -> bool {
    RECORDING.load(Ordering::Relaxed)
}

/// The spans' clock: ns since recording was first switched on.
pub fn now_ns() -> u64 {
    EPOCH.get_or_init(std::time::Instant::now).elapsed().as_nanos() as u64
}

/// A new span id (never 0).
pub fn span_id() -> u64 {
    SPAN_IDS.fetch_add(1, Ordering::Relaxed)
}

/// The calling thread's lane: a dense index per thread, in the order threads first ask.
pub fn worker() -> Lane {
    thread_local!(static WORKER: u16 = WORKERS.fetch_add(1, Ordering::Relaxed) as u16);
    Lane::Cpu(WORKER.with(|w| *w))
}

/// Keep `span` (when recording).
pub fn push_span(span: Span) {
    if recording() {
        SPANS.lock().unwrap().push(span);
    }
}

/// The revision whose plate-wide work (merge, G-code, package) runs from now on: the one the
/// plate's own spans name ([`plate_span`]).
pub fn set_settling(revision: u64) {
    SETTLING.store(revision, Ordering::Relaxed);
}

/// `f` on this worker, recorded as one span of the plate's `stage` for the revision of
/// [`set_settling`] (ready when it starts, after nothing).
pub fn plate_span<T>(stage: &'static str, f: impl FnOnce() -> T) -> T {
    if !recording() {
        return f();
    }
    let start_ns = now_ns();
    let v = f();
    let revision = SETTLING.load(Ordering::Relaxed);
    push_span(Span { id: span_id(), lane: worker(), ready_ns: start_ns, start_ns, end_ns: now_ns(), after: Vec::new(), stage, part: NO_PART, layer: NO_LAYER, revision });
    v
}

/// The spans recorded since the last call.
pub fn take_spans() -> Vec<Span> {
    std::mem::take(&mut *SPANS.lock().unwrap())
}
