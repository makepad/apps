//! `Progress`: how a running engine reports and is stopped (C4, E6 1.3). Shared by `Arc`
//! between the coordinator, its pool tasks and the route worker.
//!
//! - Counted work and the open-net count are atomics the canvas can read at any time.
//! - `cancel` is a flag an engine checks at transaction boundaries.
//! - At each transaction boundary, `due(step)` says whether to build and offer the legal solution
//!   after transaction `step`. The partial is a function of the problem and `step` only.
//!   A sink-less consumer requests copies with `want_offer` and reads them with `take_offer`.
//!   A consumer can instead supply a sink and cadence: a wanted partial (once at least `floor` has
//!   passed since the last copy) or an elapsed interval copies the solution and calls the sink on
//!   the engine's thread. Wall time only picks which
//!   steps are copied, never their content (E6 9.4). The first partial is always wanted.
//!   A replay names the steps instead (`at_steps`): exactly those are copied.
//!
//! The slot lock covers only field reads or swaps; copies, old partial drops and sink
//! callbacks happen outside them. No UI thread waits for an engine or a callback.
//!
//! Job status is stored in atomics. Its optional sink runs on the engine's thread, outside
//! every lock, on each `begin_net`, `retry_net`, `block_net` or `set_phase`. A retry shows in the
//! status (`attempt`, `retry`, then `blocked_by` when the net stays blocked): offers stay the
//! settled, legal states.

use crate::solution::RouteSolution;
use fab_elec_core::NetId;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a running job is doing, for the status line.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum JobPhase {
    /// Routing nets one transaction at a time; `NetStatus::current` names the net.
    Nets,
    /// Cleanup passes over routed copper; `NetStatus::current` names the unit's first net.
    Cleanup,
    /// Relaxing all copper; `step` is the job's current step (no current net).
    Relax { step: u32 },
}

/// Why an engine routes its current net again.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum RetryWhy {
    /// Its route cut another net's pad off its pour; it routes again with that pour's neck kept
    /// open.
    PourNeck,
    /// Its route was blocked by other nets' copper; those nets were ripped up and it routes again.
    RipUp,
    /// It was ripped up to make room for another net and routes again.
    Ripped,
}

impl RetryWhy {
    /// The status line's words for this retry.
    pub fn label(self) -> &'static str {
        match self {
            RetryWhy::PourNeck => "routing again around a pour neck",
            RetryWhy::RipUp => "routing again after ripping up the nets in its way",
            RetryWhy::Ripped => "routing again after it was ripped up for another net",
        }
    }

    fn code(self) -> u32 {
        match self {
            RetryWhy::PourNeck => 0,
            RetryWhy::RipUp => 1,
            RetryWhy::Ripped => 2,
        }
    }

    fn from_code(code: u32) -> Option<RetryWhy> {
        match code {
            0 => Some(RetryWhy::PourNeck),
            1 => Some(RetryWhy::RipUp),
            2 => Some(RetryWhy::Ripped),
            _ => None,
        }
    }
}

/// A running job's progress: published on each new net, retry, block and phase change.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct NetStatus {
    pub phase: JobPhase,
    pub current: Option<NetId>,
    /// Nets of the job's problem routed and left open in the newest offered partial: its
    /// `metrics.nets_routed` without the `repaired` nets outside the problem (which `total` does
    /// not count either), and `open.len()`.
    pub routed: u32,
    pub failed: u32,
    /// Nets in the job's problem (`set_total`).
    pub total: u32,
    /// The current net's attempt: 0 for its first route, `n` after `n` retries.
    pub attempt: u32,
    /// Why the current net is routed again (`attempt` > 0).
    pub retry: Option<RetryWhy>,
    /// The current net stays open: routing it would cut this net off.
    pub blocked_by: Option<NetId>,
}

/// Counted work, job status, cooperative cancellation and consumer-controlled partial solutions.
pub struct Progress {
    work: AtomicU64,
    open: AtomicU32,
    current: AtomicU32,
    phase: AtomicU32,
    relax_step: AtomicU32,
    attempt: AtomicU32,
    retry: AtomicU32,
    blocked_by: AtomicU32,
    routed: AtomicU32,
    total: AtomicU32,
    cancel: AtomicBool,
    want: AtomicBool,
    slot: Mutex<Option<(u32, Arc<RouteSolution>)>>,
    sink: Option<Sink>,
    status_sink: Option<Box<dyn Fn(NetStatus) + Send + Sync>>,
}

struct Sink {
    when: When,
    publish: Box<dyn Fn(u32, Arc<RouteSolution>) + Send + Sync>,
}

/// Which offers a sink receives.
#[derive(Debug)]
enum When {
    /// Wanted offers at least `floor` after the last copy, and any offer at least `every` after it.
    Every { every: Duration, floor: Duration, origin: Instant, last: AtomicU64 },
    /// Exactly these steps, sorted: the steps a recorded stream published.
    Steps(Vec<u32>),
}

impl fmt::Debug for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Progress")
            .field("work", &self.work)
            .field("open", &self.open)
            .field("current", &self.current)
            .field("phase", &self.phase)
            .field("relax_step", &self.relax_step)
            .field("attempt", &self.attempt)
            .field("retry", &self.retry)
            .field("blocked_by", &self.blocked_by)
            .field("routed", &self.routed)
            .field("total", &self.total)
            .field("cancel", &self.cancel)
            .field("want", &self.want)
            .field("slot", &self.slot)
            .field("sink", &self.sink)
            .field("status_sink", &self.status_sink.is_some())
            .finish()
    }
}

impl fmt::Debug for Sink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sink").field("when", &self.when).finish_non_exhaustive()
    }
}

impl Default for Progress {
    fn default() -> Self {
        Progress::new()
    }
}

impl Progress {
    /// Progress in `JobPhase::Nets` with a latest-offer slot and its first partial requested.
    pub fn new() -> Progress {
        Progress {
            work: AtomicU64::new(0),
            open: AtomicU32::new(0),
            current: AtomicU32::new(u32::MAX),
            phase: AtomicU32::new(0),
            relax_step: AtomicU32::new(0),
            attempt: AtomicU32::new(0),
            retry: AtomicU32::new(u32::MAX),
            blocked_by: AtomicU32::new(u32::MAX),
            routed: AtomicU32::new(0),
            total: AtomicU32::new(0),
            cancel: AtomicBool::new(false),
            want: AtomicBool::new(true),
            slot: Mutex::new(None),
            sink: None,
            status_sink: None,
        }
    }

    /// Deliver wanted partials and those at least `every` after the last copy to `sink`.
    /// The sink runs synchronously on the engine's thread, outside every progress lock.
    pub fn with_sink(
        every: Duration,
        sink: impl Fn(u32, Arc<RouteSolution>) + Send + Sync + 'static,
    ) -> Progress {
        Progress {
            sink: Some(Sink {
                when: When::Every { every, floor: Duration::ZERO, origin: Instant::now(), last: AtomicU64::new(u64::MAX) },
                publish: Box::new(sink),
            }),
            ..Progress::new()
        }
    }

    /// Deliver exactly the partials after the transactions in `steps` to `sink`, whatever the
    /// timing: a replay of the steps a recorded stream published. The sink runs as for `with_sink`.
    pub fn at_steps(
        mut steps: Vec<u32>,
        sink: impl Fn(u32, Arc<RouteSolution>) + Send + Sync + 'static,
    ) -> Progress {
        steps.sort_unstable();
        steps.dedup();
        Progress { sink: Some(Sink { when: When::Steps(steps), publish: Box::new(sink) }), ..Progress::new() }
    }

    /// Copy a wanted partial only once `min_gap` has passed since the last copy, so a consumer that
    /// wants every partial gets at most one per `min_gap`. No effect on a replay (`at_steps`).
    pub fn with_floor(mut self, min_gap: Duration) -> Progress {
        if let Some(Sink { when: When::Every { floor, .. }, .. }) = &mut self.sink {
            *floor = min_gap;
        }
        self
    }

    /// Publish status on each new net, retry, block or phase, on the engine's thread outside
    /// every lock.
    pub fn with_status(mut self, sink: impl Fn(NetStatus) + Send + Sync + 'static) -> Progress {
        self.status_sink = Some(Box::new(sink));
        self
    }

    /// Set the number of nets in the job's problem.
    pub fn set_total(&self, n: u32) {
        self.total.store(n, Ordering::Relaxed);
    }

    /// Begin work on `net` (its first attempt) in the current phase and publish the current
    /// status.
    pub fn begin_net(&self, net: NetId) {
        self.current.store(net.0, Ordering::Relaxed);
        self.attempt.store(0, Ordering::Relaxed);
        self.retry.store(u32::MAX, Ordering::Relaxed);
        self.blocked_by.store(u32::MAX, Ordering::Relaxed);
        self.publish();
    }

    /// Route `net` again for `why` (its next attempt) and publish the current status.
    pub fn retry_net(&self, net: NetId, why: RetryWhy) {
        self.current.store(net.0, Ordering::Relaxed);
        self.attempt.fetch_add(1, Ordering::Relaxed);
        self.retry.store(why.code(), Ordering::Relaxed);
        self.publish();
    }

    /// `net` stays open because routing it would cut `by` off, and publish the current status.
    pub fn block_net(&self, net: NetId, by: NetId) {
        self.current.store(net.0, Ordering::Relaxed);
        self.blocked_by.store(by.0, Ordering::Relaxed);
        self.publish();
    }

    fn publish(&self) {
        if let Some(sink) = &self.status_sink {
            sink(self.status());
        }
    }

    /// Change phase (only this method does), clear the current net and publish the current status.
    pub fn set_phase(&self, phase: JobPhase) {
        let phase = match phase {
            JobPhase::Nets => 0,
            JobPhase::Cleanup => 1,
            JobPhase::Relax { step } => {
                self.relax_step.store(step, Ordering::Relaxed);
                2
            }
        };
        self.phase.store(phase, Ordering::Relaxed);
        self.current.store(u32::MAX, Ordering::Relaxed);
        self.attempt.store(0, Ordering::Relaxed);
        self.retry.store(u32::MAX, Ordering::Relaxed);
        self.blocked_by.store(u32::MAX, Ordering::Relaxed);
        self.publish();
    }

    /// Read the job's phase, current net and latest offered counts using atomics only.
    pub fn status(&self) -> NetStatus {
        let phase = match self.phase.load(Ordering::Relaxed) {
            0 => JobPhase::Nets,
            1 => JobPhase::Cleanup,
            _ => JobPhase::Relax { step: self.relax_step.load(Ordering::Relaxed) },
        };
        let current = self.current.load(Ordering::Relaxed);
        let blocked_by = self.blocked_by.load(Ordering::Relaxed);
        NetStatus {
            phase,
            current: (current != u32::MAX).then_some(NetId(current)),
            routed: self.routed.load(Ordering::Relaxed),
            failed: self.open.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            attempt: self.attempt.load(Ordering::Relaxed),
            retry: RetryWhy::from_code(self.retry.load(Ordering::Relaxed)),
            blocked_by: (blocked_by != u32::MAX).then_some(NetId(blocked_by)),
        }
    }

    /// Add `n` units of counted work (expansions, embeds).
    pub fn add_work(&self, n: u64) {
        self.work.fetch_add(n, Ordering::Relaxed);
    }
    /// The engine's counted work so far.
    pub fn work(&self) -> u64 {
        self.work.load(Ordering::Relaxed)
    }
    /// The number of nets still open, as the engine last said.
    pub fn set_open(&self, n: u32) {
        self.open.store(n, Ordering::Relaxed);
    }
    /// The most recently reported number of open nets.
    pub fn open(&self) -> u32 {
        self.open.load(Ordering::Relaxed)
    }

    /// Ask the engine to stop at its next transaction boundary; what it returns is legal.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    /// Whether the consumer has requested cancellation.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// Whether an offer at `step` would copy now, without consuming a request or changing cadence.
    pub fn due(&self, step: u32) -> bool {
        match &self.sink {
            Some(Sink { when: When::Every { every, floor, origin, last }, .. }) => {
                let last = last.load(Ordering::Relaxed);
                if last == u64::MAX {
                    return true;
                }
                let since = origin.elapsed().saturating_sub(Duration::from_nanos(last));
                since >= *every || (self.want.load(Ordering::Acquire) && since >= *floor)
            }
            Some(Sink { when: When::Steps(steps), .. }) => steps.binary_search(&step).is_ok(),
            None => self.want.load(Ordering::Acquire),
        }
    }

    /// The engine offers its legal solution after transaction `step` (a transaction boundary).
    /// Copy when wanted or when the sink's cadence is due; the first offer is always wanted.
    pub fn offer(&self, step: u32, partial: &RouteSolution) {
        self.open.store(partial.open.len() as u32, Ordering::Relaxed);
        let repaired = u32::try_from(partial.repaired.len()).unwrap_or(u32::MAX);
        self.routed.store(partial.metrics.nets_routed.saturating_sub(repaired), Ordering::Relaxed);
        if !self.due(step) {
            return;
        }
        self.want.store(false, Ordering::Release);
        let copy = Arc::new(partial.clone());
        if let Some(sink) = &self.sink {
            if let When::Every { origin, last, .. } = &sink.when {
                // Reserve u64::MAX for a cadence that has never copied.
                let nanos = origin.elapsed().as_nanos().min(u128::from(u64::MAX - 1)) as u64;
                last.store(nanos, Ordering::Relaxed);
            }
            (sink.publish)(step, copy);
        } else {
            let old = self.slot.lock().unwrap_or_else(|e| e.into_inner()).replace((step, copy));
            drop(old);
        }
    }

    /// The consumer asks for the next partial (its own cadence, e.g. every 100 ms).
    pub fn want_offer(&self) {
        self.want.store(true, Ordering::Release);
    }

    /// The newest partial not yet taken, with its step; only used without a sink.
    pub fn take_offer(&self) -> Option<(u32, Arc<RouteSolution>)> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solution::Reason;

    #[test]
    fn due_answers_the_offer_rule_without_consuming_want() {
        let solution = RouteSolution::default();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let output = seen.clone();
        let progress = Progress::with_sink(Duration::from_secs(3600), move |step, _| {
            output.lock().unwrap().push(step);
        });
        assert!(progress.due(1), "cadence: the first offer is due before any copy");
        progress.offer(1, &solution);
        assert_eq!(*seen.lock().unwrap(), vec![1], "cadence: the first due offer is copied");
        assert!(!progress.due(2), "cadence: a later offer is not due before the interval elapses");
        progress.want_offer();
        assert!(progress.due(2), "request: a wanted offer is due before the interval elapses");
        assert!(progress.due(2), "request: querying due does not consume want");
        progress.offer(2, &solution);
        assert_eq!(*seen.lock().unwrap(), vec![1, 2], "request: the wanted offer is copied");
        assert!(!progress.due(3), "request: copying consumes want and restarts the interval");

        let replay = Progress::at_steps(vec![2, 5], |_, _| {});
        assert!(replay.due(2), "replay: the first listed step is due");
        assert!(replay.due(5), "replay: the second listed step is due");
        assert!(!replay.due(3), "replay: an unlisted step is not due even when wanted");

        let slot = Progress::new();
        assert!(slot.due(1), "slot: the first offer is wanted");
        slot.offer(1, &solution);
        assert!(!slot.due(2), "slot: copying consumes the first request");
        slot.want_offer();
        assert!(slot.due(2), "slot: a new request makes the next offer due");
    }

    #[test]
    fn status_tracks_nets_phases_and_counts() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let output = seen.clone();
        let progress = Progress::new().with_status(move |status| {
            output.lock().unwrap().push(status);
        });
        progress.set_total(3);
        progress.begin_net(NetId(4));
        progress.retry_net(NetId(4), RetryWhy::PourNeck);
        progress.block_net(NetId(4), NetId(9));
        let mut solution = RouteSolution::default();
        // One problem net routed, and net 9 outside the problem repaired: the metric counts both.
        solution.metrics.nets_routed = 2;
        solution.repaired.push(NetId(9));
        solution.open.push((NetId(4), Reason::NotTried));
        progress.offer(1, &solution);
        progress.begin_net(NetId(7));
        progress.set_phase(JobPhase::Cleanup);
        progress.begin_net(NetId(9));
        progress.set_phase(JobPhase::Relax { step: 2 });

        let statuses = seen.lock().unwrap();
        assert_eq!(statuses.len(), 7, "status: only begin_net, retry_net, block_net and set_phase publish");
        assert_eq!(
            statuses.iter().map(|status| (status.phase, status.current)).collect::<Vec<_>>(),
            vec![
                (JobPhase::Nets, Some(NetId(4))),
                (JobPhase::Nets, Some(NetId(4))),
                (JobPhase::Nets, Some(NetId(4))),
                (JobPhase::Nets, Some(NetId(7))),
                (JobPhase::Cleanup, None),
                (JobPhase::Cleanup, Some(NetId(9))),
                (JobPhase::Relax { step: 2 }, None),
            ],
            "status: nets publish in order, keep the phase and clear on a phase change"
        );
        assert_eq!(
            statuses.iter().map(|status| (status.attempt, status.retry, status.blocked_by)).collect::<Vec<_>>(),
            vec![
                (0, None, None),
                (1, Some(RetryWhy::PourNeck), None),
                (1, Some(RetryWhy::PourNeck), Some(NetId(9))),
                (0, None, None),
                (0, None, None),
                (0, None, None),
                (0, None, None),
            ],
            "status: a retry counts its attempt and why, a block names the cut net, a new net or phase starts clean"
        );
        assert_eq!(
            (statuses[3].routed, statuses[3].failed, statuses[3].total),
            (1, 1, 3),
            "status: the next net carries the latest offered counts of problem nets (a repaired net outside the problem is not routed of the total) and the problem total"
        );
        assert_eq!(progress.status(), statuses[6], "status: atomic reads match the last publication");
    }

    #[test]
    fn offers_are_copied_only_when_wanted() {
        let p = Progress::new();
        let mut s = RouteSolution::default();
        s.metrics.nets_routed = 1;
        p.offer(1, &s);
        s.metrics.nets_routed = 2;
        p.offer(2, &s);
        assert_eq!(p.status().routed, 2, "status: an uncopied offer still updates the routed count");
        let (n, first) = p.take_offer().expect("the first partial is always wanted");
        assert_eq!((n, first.metrics.nets_routed), (1, 1), "an unwanted later offer is not copied");
        assert!(p.take_offer().is_none(), "taken once");
        p.want_offer();
        s.metrics.nets_routed = 3;
        p.offer(3, &s);
        assert_eq!(p.take_offer().map(|(n, s)| (n, s.metrics.nets_routed)), Some((3, 3)), "the next wanted offer carries its step");
        p.add_work(5);
        p.add_work(7);
        p.cancel();
        assert_eq!((p.work(), p.cancelled()), (12, true));
    }

    #[test]
    fn sink_cadence_selects_offers() {
        for (every, expected) in [(Duration::MAX, vec![(1, 1)]), (Duration::ZERO, vec![(1, 1), (2, 2), (3, 3)])] {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let output = seen.clone();
            let progress = Progress::with_sink(every, move |step, partial| {
                output.lock().unwrap().push((step, partial.metrics.nets_routed));
            });
            let mut solution = RouteSolution::default();
            for step in 1..=3 {
                solution.metrics.nets_routed = step;
                progress.offer(step, &solution);
            }
            let actual = std::mem::take(&mut *seen.lock().unwrap());
            assert_eq!(actual.first(), Some(&(1, 1)), "the first partial is always offered");
            assert_eq!(actual, expected, "the sink receives selected steps in order for {every:?}");
            assert!(progress.take_offer().is_none(), "a sink does not retain a second copy in the slot");
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let output = seen.clone();
        let progress = Progress::at_steps(vec![3, 1, 3], move |step, partial| {
            output.lock().unwrap().push((step, partial.metrics.nets_routed));
        });
        let mut solution = RouteSolution::default();
        for step in 1..=4 {
            solution.metrics.nets_routed = step;
            progress.offer(step, &solution);
        }
        assert_eq!(*seen.lock().unwrap(), vec![(1, 1), (3, 3)], "a replay copies exactly the named steps, once each");
    }
}
