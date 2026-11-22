//! Data-parallel maps over layers: every index runs exactly once, on the
//! caller and on helpers that claim indices from one atomic counter
//! (dynamic balancing; layer costs vary 100x). Results land at their index,
//! so the output never depends on the thread count or the claim order.
//!
//! Helpers come from a [`FanOut`]: scoped std threads by default (the CLI
//! and tests), the platform task pool in the app (`set_fan_out`). A map
//! honours a [`Ctl`]: its cancel check runs before every index, and its
//! focus makes the indices nearest the focus go first.


pub use makepad_fab_slicer::par::{budget_threads, plate_span, progress, record_spans, set_fan_out, set_settling, set_threads, take_spans, threads, crew, Crew, FanOut, Lane, ScopedThreads, Span, NO_LAYER, NO_PART};
use makepad_fab_slicer::par::{fan, tick};

/// Cancellation and priority for the maps of one slice.
#[derive(Clone, Copy, Default)]
pub struct Ctl<'a> {
    /// True once the work is no longer wanted: remaining indices are skipped.
    pub cancel: Option<&'a (dyn Fn() -> bool + Sync)>,
    /// Layers nearest this index are claimed first.
    pub focus: Option<usize>,
    /// Told about results as they are made (progressive display).
    pub emit: Option<&'a (dyn Fn(crate::Emit) + Sync)>,
    /// Told how many units of work just finished (a map: one per index), for
    /// progress within a stage.
    pub tick: Option<&'a (dyn Fn(usize) + Sync)>,
    /// Told what a stage that names its own steps is at: the step ("planning trees"), its place
    /// in its count when it has one (12 of 48), and how much of the stage is done (0..=1).
    pub phase: Option<&'a (dyn Fn(&str, Option<(u32, u32)>, f64) + Sync)>,
}

impl Ctl<'_> {
    pub fn cancelled(&self) -> bool {
        self.cancel.is_some_and(|c| c())
    }

    /// `n` units of work finished.
    pub fn done(&self, n: usize) {
        if let Some(t) = self.tick {
            t(n);
        }
    }

    /// The stage is at `what` (step `step`), `fraction` of it done.
    pub fn phase(&self, what: &str, step: Option<(u32, u32)>, fraction: f64) {
        if let Some(p) = self.phase {
            p(what, step, fraction);
        }
    }
}

/// A step of a stage that is a map (or any count of units): told as `what` with its count, done
/// and of all, its fraction of the stage going from `lo` to `hi` as the units finish, never back
/// (the map's helpers finish in any order). Its [`tick`](Self::tick) is the map's own
/// (`Ctl { tick: Some(&|k| step.tick(k)), ..ctl }`).
pub struct Step<'a> {
    ctl: Ctl<'a>,
    what: &'a str,
    lo: f64,
    hi: f64,
    units: usize,
    /// The units done, and the count last told.
    done: std::sync::Mutex<(usize, usize)>,
    /// A helper is telling the phase: no other tells meanwhile, so what goes out never goes back,
    /// and the phase is told with no lock held (its listener may take its own).
    telling: std::sync::atomic::AtomicBool,
}

/// Lets go of [`Step::telling`] when its teller is done, a panicking one too.
struct Telling<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for Telling<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl<'a> Step<'a> {
    /// The step begins: told at `lo`, none of its `units` done.
    pub fn new(ctl: Ctl<'a>, what: &'a str, lo: f64, hi: f64, units: usize) -> Self {
        ctl.phase(what, Some((0, units as u32)), lo);
        Step { ctl, what, lo, hi, units, done: std::sync::Mutex::new((0, 0)), telling: std::sync::atomic::AtomicBool::new(false) }
    }

    /// `k` more of its units finished. One helper at a time tells the newest count, with no lock
    /// held: a helper that finds another telling leaves its count to that one, which looks again
    /// once it is done.
    pub fn tick(&self, k: usize) {
        use std::sync::atomic::Ordering::SeqCst;
        {
            let mut done = self.done.lock().unwrap();
            done.0 = (done.0 + k).min(self.units);
        }
        while self.telling.compare_exchange(false, true, SeqCst, SeqCst).is_ok() {
            let telling = Telling(&self.telling);
            let due = {
                let mut done = self.done.lock().unwrap();
                (done.0 > done.1).then(|| {
                    done.1 = done.0;
                    done.0
                })
            };
            if let Some(done) = due {
                self.ctl.phase(self.what, Some((done as u32, self.units as u32)), self.lo + (self.hi - self.lo) * done as f64 / self.units.max(1) as f64);
            }
            drop(telling);
            let done = self.done.lock().unwrap();
            if done.0 == done.1 {
                return;
            }
        }
    }
}

/// The map was cancelled before every index ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

/// Claim order: by distance from the focus (ties: lower index first).
fn order(n: usize, focus: Option<usize>) -> Option<Vec<usize>> {
    let f = focus?.min(n.saturating_sub(1));
    let mut v: Vec<usize> = (0..n).collect();
    v.sort_by_key(|&i| (i.abs_diff(f), i));
    Some(v)
}

/// `(0..n).map(f).collect()`, in parallel, under `ctl`.
pub fn try_map<T: Send, F: Fn(usize) -> T + Sync>(ctl: Ctl, n: usize, f: F) -> Result<Vec<T>, Cancelled> {
    try_map_some(ctl, n, f).into_iter().map(|v| v.ok_or(Cancelled)).collect()
}

/// [`try_map`] that keeps what ran: after a cancel, the indices that finished hold their
/// results and the others None (work a later call can resume instead of redoing).
pub fn try_map_some<T: Send, F: Fn(usize) -> T + Sync>(ctl: Ctl, n: usize, f: F) -> Vec<Option<T>> {
    let ord = order(n, ctl.focus);
    let index = |k: usize| ord.as_ref().map_or(k, |o| o[k]);
    // On one thread too through the fan-out: its caller checkpoints before every index, so a
    // single worker's map yields its token as several workers' maps do.
    let slots: Vec<std::sync::Mutex<Option<T>>> = (0..n).map(|_| std::sync::Mutex::new(None)).collect();
    fan(n, &|k| {
        if ctl.cancelled() { return; }
        let i = index(k);
        tick();
        let v = f(i);
        *slots[i].lock().unwrap() = Some(v);
        ctl.done(1);
    });
    slots.into_iter().map(|s| s.into_inner().unwrap()).collect()
}

/// `(a(), b())`, side by side, under `ctl`'s cancel (the two sides tell their own progress).
pub fn try_join<A: Send, B: Send>(ctl: Ctl, a: impl FnOnce() -> A + Send, b: impl FnOnce() -> B + Send) -> Result<(A, B), Cancelled> {
    let (a, b) = (std::sync::Mutex::new(Some(a)), std::sync::Mutex::new(Some(b)));
    let (ra, rb) = (std::sync::Mutex::new(None), std::sync::Mutex::new(None));
    try_map(Ctl { cancel: ctl.cancel, ..Ctl::default() }, 2, |i| {
        if i == 0 {
            let f = a.lock().unwrap().take().expect("each side once");
            *ra.lock().unwrap() = Some(f());
        } else {
            let f = b.lock().unwrap().take().expect("each side once");
            *rb.lock().unwrap() = Some(f());
        }
    })?;
    Ok((ra.into_inner().unwrap().expect("ran"), rb.into_inner().unwrap().expect("ran")))
}

/// [`try_map`] without cancellation.
pub fn map<T: Send, F: Fn(usize) -> T + Sync>(n: usize, f: F) -> Vec<T> {
    try_map(Ctl::default(), n, f).expect("not cancellable")
}

/// `items.into_iter().enumerate().map(f).collect()`, in parallel: each item
/// is moved into the call for its index.
pub fn try_map_vec<I: Send, T: Send, F: Fn(usize, I) -> T + Sync>(ctl: Ctl, items: Vec<I>, f: F) -> Result<Vec<T>, Cancelled> {
    let slots: Vec<std::sync::Mutex<Option<I>>> = items.into_iter().map(|v| std::sync::Mutex::new(Some(v))).collect();
    try_map(ctl, slots.len(), |i| {
        let item = slots[i].lock().unwrap().take().expect("each item once");
        f(i, item)
    })
}

pub fn map_vec<I: Send, T: Send, F: Fn(usize, I) -> T + Sync>(items: Vec<I>, f: F) -> Vec<T> {
    try_map_vec(Ctl::default(), items, f).expect("not cancellable")
}
