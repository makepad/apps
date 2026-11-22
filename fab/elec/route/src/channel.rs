//! The route worker's two ends (E6 8). The UI side (`RouteHandle`) never waits: a sample is a
//! field swap into a latest-value slot, which cannot fill; a command goes into a bounded queue
//! and comes back when the queue is full, for the caller to retry on a later frame (AGENTS
//! threading rule); events are taken with `try_recv`. The worker side (`RouteInbox`) blocks its
//! own long-lived thread until there is work. It takes queued bases first, in their order (a
//! sample may be ahead of the worker's base, and that base must come first), then the newest
//! sample and the other commands in turn: after a sample, one command goes before the next
//! sample, so a drag that keeps the worker busy never starves a cancel or a gesture's end.
//!
//! Superseding: proposals and refusals for one gesture replace each other; the higher seq wins,
//! and an equal seq replaces the untaken answer (a later tier of the same sample). A job's newer
//! proposal replaces its untaken proposal (each is complete against its base), so a canvas that
//! drains once per frame sees the newest answer and nothing piles up while it does not draw.
//! Explanations queue in order.
//! A job's status replaces only its untaken status, without another UI wake while that wake is pending.
//!
//! Locks are held for a scan, a push, a pop or a swap; discarded event drops and the UI wake callback
//! run outside them.

use crate::gesture::{GestureId, GestureSample};
use crate::proposal::{ProposalSource, RouteEvent};
use crate::request::RouteRequest;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Commands the queue holds before `request` hands one back. A full queue means the worker is
/// busy with long commands; the caller keeps the command and retries next frame.
pub const QUEUE_DEPTH: usize = 256;

struct Inbox {
    sample: Option<GestureSample>,
    cmds: VecDeque<RouteRequest>,
    /// The last work taken was a sample: a waiting command goes next.
    after_sample: bool,
    closed: bool,
}

struct Shared {
    inbox: Mutex<Inbox>,
    ready: Condvar,
    outbox: Mutex<VecDeque<RouteEvent>>,
    wake_ui: Box<dyn Fn() + Send + Sync>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A new worker channel. `wake_ui` is called on the publishing worker or pool thread after
/// each queued event except a replacement status, so the host can request a frame.
pub fn route_channel(wake_ui: impl Fn() + Send + Sync + 'static) -> (RouteHandle, RouteInbox) {
    let shared = Arc::new(Shared {
        inbox: Mutex::new(Inbox { sample: None, cmds: VecDeque::new(), after_sample: false, closed: false }),
        ready: Condvar::new(),
        outbox: Mutex::new(VecDeque::new()),
        wake_ui: Box::new(wake_ui),
    });
    (RouteHandle { shared: shared.clone() }, RouteInbox { shared })
}

/// The UI side. Dropping it closes the channel; the worker's `next` then returns `Closed`.
pub struct RouteHandle {
    shared: Arc<Shared>,
}

impl RouteHandle {
    /// Write the newest sample of the current gesture (replaces an untaken older one).
    pub fn sample(&self, s: GestureSample) {
        lock(&self.shared.inbox).sample = Some(s);
        self.shared.ready.notify_one();
    }

    /// Queue a command; `Err` gives it back when the queue is full (retry next frame).
    pub fn request(&self, r: RouteRequest) -> Result<(), RouteRequest> {
        {
            let mut inbox = lock(&self.shared.inbox);
            if inbox.cmds.len() >= QUEUE_DEPTH {
                return Err(r);
            }
            inbox.cmds.push_back(r);
        }
        self.shared.ready.notify_one();
        Ok(())
    }

    /// The next event, oldest first; never waits.
    pub fn try_recv(&self) -> Option<RouteEvent> {
        lock(&self.shared.outbox).pop_front()
    }
}

impl Drop for RouteHandle {
    fn drop(&mut self) {
        lock(&self.shared.inbox).closed = true;
        self.shared.ready.notify_one();
    }
}

/// What the worker takes next.
#[derive(Debug)]
pub enum Work {
    Request(RouteRequest),
    Sample(GestureSample),
    Closed,
}

/// The worker side.
pub struct RouteInbox {
    shared: Arc<Shared>,
}

impl RouteInbox {
    /// Block the worker's thread until there is work: the oldest queued base, then the newest
    /// sample and the other commands in turn (one command after each sample when one waits).
    pub fn next(&self) -> Work {
        let mut inbox = lock(&self.shared.inbox);
        loop {
            // O(QUEUE_DEPTH) at most: the scan for a base.
            if let Some(i) = inbox.cmds.iter().position(|r| matches!(r, RouteRequest::Base(_))) {
                return Work::Request(inbox.cmds.remove(i).expect("the base found is queued"));
            }
            if !(inbox.after_sample && !inbox.cmds.is_empty()) {
                if let Some(s) = inbox.sample.take() {
                    inbox.after_sample = true;
                    return Work::Sample(s);
                }
            }
            if let Some(r) = inbox.cmds.pop_front() {
                inbox.after_sample = false;
                return Work::Request(r);
            }
            if inbox.closed {
                return Work::Closed;
            }
            inbox = self.shared.ready.wait(inbox).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// The sample waiting in the inbox, by gesture and seq (a look: it stays there).
    pub fn waiting_sample(&self) -> Option<(GestureId, u64)> {
        lock(&self.shared.inbox).sample.as_ref().map(|s| (s.id, s.seq))
    }

    /// Work is waiting: a running second tier yields at its next item boundary (E6 3.4).
    pub fn preempted(&self) -> bool {
        let inbox = lock(&self.shared.inbox);
        inbox.sample.is_some() || !inbox.cmds.is_empty() || inbox.closed
    }

    /// Publish an event to the canvas, superseding an untaken answer from the same source.
    pub fn publish(&self, e: RouteEvent) {
        self.publisher().publish(e);
    }

    /// A handle through which jobs on other threads publish to this channel's outbox.
    pub fn publisher(&self) -> Publisher {
        Publisher { shared: self.shared.clone() }
    }
}

/// A cloneable event publisher shared by the worker and its pool jobs.
#[derive(Clone)]
pub struct Publisher {
    shared: Arc<Shared>,
}

impl Publisher {
    /// Supersede untaken answers from the same source, then wake the UI outside the lock.
    /// Older gesture answers are dropped and replacement statuses reuse the pending UI wake.
    pub fn publish(&self, e: RouteEvent) {
        let (superseded, wake) = {
            let mut out = lock(&self.shared.outbox);
            let previous = if let Some((id, seq)) = gesture_key(&e) {
                out.iter().enumerate().find_map(|(index, old)| {
                    let (old_id, old_seq) = gesture_key(old)?;
                    (old_id == id).then_some((index, old_seq > seq))
                })
            } else if let RouteEvent::Proposal(p) = &e {
                let key = source_key(&p.source);
                out.iter().position(|old| {
                    matches!(old, RouteEvent::Proposal(o) if source_key(&o.source) == key)
                }).map(|index| (index, false))
            } else if let RouteEvent::Status(status) = &e {
                out.iter().position(|old| {
                    matches!(old, RouteEvent::Status(old) if old.job == status.job)
                }).map(|index| (index, false))
            } else {
                None
            };
            // Publishing keeps at most one untaken answer for each source.
            if matches!(previous, Some((_, true))) {
                (Some(e), false)
            } else {
                let previous = previous.and_then(|(index, _)| out.remove(index));
                let wake = !matches!(&e, RouteEvent::Status(_)) || previous.is_none();
                out.push_back(e);
                (previous, wake)
            }
        };
        if wake {
            (self.shared.wake_ui)();
        }
        drop(superseded);
    }
}

/// A proposal or refusal answering a gesture sample.
fn gesture_key(e: &RouteEvent) -> Option<(GestureId, u64)> {
    match e {
        RouteEvent::Proposal(p) => match p.source {
            ProposalSource::Gesture { id, seq, .. } => Some((id, seq)),
            ProposalSource::Job { .. } => None,
        },
        RouteEvent::Refused(r) => Some((r.id, r.seq)),
        RouteEvent::Explain(_) | RouteEvent::Status(_) => None,
    }
}

/// Proposals from one gesture, or one job, supersede each other.
fn source_key(s: &ProposalSource) -> (u8, u32) {
    match *s {
        ProposalSource::Gesture { id, .. } => (0, id.0),
        ProposalSource::Job { job, .. } => (1, job.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gesture::{Gesture, GestureId, GestureKind};
    use crate::progress::JobPhase;
    use crate::proposal::{Explanation, JobId, JobStatus, ProposalId, RouteProposal, SampleCause, SampleRefused, Tier};
    use crate::request::{Base, Effort, Engine, Method, RouteJob, RouteRequest, RouteScope};
    use crate::solution::RouteMetrics;
    use fab_elec_board::zone::fill_all;
    use fab_elec_board::{connect, resolve_rules, Anchor, Board};
    use fab_elec_core::{BoardId, LayerId, NetId, P};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn sample(seq: u64) -> GestureSample {
        GestureSample { id: GestureId(1), seq, gesture: Gesture::new(GestureKind::Route, BoardId(0), 1, Anchor::Free, P::ZERO, LayerId::F_CU) }
    }
    fn proposal(source: ProposalSource) -> RouteEvent {
        RouteEvent::Proposal(RouteProposal {
            id: ProposalId(0),
            source,
            unit: BoardId(0),
            base_generation: 1,
            base_hash: Default::default(),
            ops: Vec::new(),
            checked: None,
            metrics: RouteMetrics::default(),
            verified: true,
            collisions: Vec::new(),
            open: Vec::new(),
            head: None,
            solve_us: 0,
            last: false,
            label: String::new(),
        })
    }
    fn refused(id: GestureId, seq: u64) -> RouteEvent {
        RouteEvent::Refused(SampleRefused {
            id, seq, unit: BoardId(0), cause: SampleCause::Unsupported, text: "gesture routing is not available yet".into(),
        })
    }

    #[test]
    fn statuses_replace_untaken_statuses_without_waking_again() {
        let wakes = Arc::new(AtomicU32::new(0));
        let w = wakes.clone();
        let (ui, worker) = route_channel(move || {
            w.fetch_add(1, Ordering::Relaxed);
        });
        let status = |job, net| RouteEvent::Status(JobStatus {
            job: JobId(job),
            unit: BoardId(0),
            phase: JobPhase::Nets,
            current: Some(NetId(net)),
            routed: 0,
            failed: 0,
            total: 3,
            attempt: 0,
            retry: None,
            blocked_by: None,
        });
        let a = status(1, 4);
        let b = status(1, 7);
        let c = status(2, 8);
        let proposal = proposal(ProposalSource::Job { job: JobId(1), step: 1 });
        worker.publish(a);
        worker.publish(b.clone());
        worker.publish(c.clone());
        worker.publish(proposal.clone());

        assert_eq!(wakes.load(Ordering::Relaxed), 3, "status: a replacement reuses the untaken status's wake");
        assert_eq!(ui.try_recv(), Some(b), "status: the newest status replaces the same job's untaken status");
        assert_eq!(ui.try_recv(), Some(c), "status: another job's status remains in order");
        assert_eq!(ui.try_recv(), Some(proposal), "status: the same job's proposal queues separately");
        assert_eq!(ui.try_recv(), None, "status: no superseded event remains queued");
    }

    #[test]
    fn gesture_answers_supersede_by_seq_across_kinds() {
        let wakes = Arc::new(AtomicU32::new(0));
        let w = wakes.clone();
        let (ui, worker) = route_channel(move || {
            w.fetch_add(1, Ordering::Relaxed);
        });
        let pending = || lock(&ui.shared.outbox).iter().cloned().collect::<Vec<_>>();
        let id = GestureId(1);
        let g = |seq| proposal(ProposalSource::Gesture { id, seq, tier: Tier::Stretch });
        let other = proposal(ProposalSource::Gesture { id: GestureId(2), seq: 1, tier: Tier::Full });
        let other_refused = refused(GestureId(3), 1);
        let job = proposal(ProposalSource::Job { job: JobId(1), step: 0 });
        let explain = |net| RouteEvent::Explain(Explanation {
            unit: BoardId(0), net: NetId(net), reason: None, blockers: Vec::new(), hints: Vec::new(),
        });
        let first = explain(0);
        let second = explain(1);
        worker.publish(g(5));
        worker.publish(first.clone());
        worker.publish(other.clone());
        worker.publish(job.clone());
        worker.publish(other_refused.clone());
        worker.publish(second.clone());

        worker.publish(refused(id, 4));
        assert_eq!(
            pending(),
            vec![g(5), first.clone(), other.clone(), job.clone(), other_refused.clone(), second.clone()],
            "older refusal: seq 4 leaves the seq 5 proposal as the gesture's only untaken answer"
        );
        worker.publish(refused(id, 6));
        assert_eq!(
            pending(),
            vec![first.clone(), other.clone(), job.clone(), other_refused.clone(), second.clone(), refused(id, 6)],
            "newer refusal: seq 6 replaces the seq 5 proposal and moves the answer to the back"
        );
        worker.publish(g(6));
        let got: Vec<_> = std::iter::from_fn(|| ui.try_recv()).collect();
        assert_eq!(
            got,
            vec![first.clone(), other.clone(), job.clone(), other_refused.clone(), second.clone(), g(6)],
            "equal seq proposal: seq 6 replaces the refusal as the gesture's only untaken answer"
        );
        assert!(got.contains(&other) && got.contains(&other_refused),
            "other gestures: their proposal and refusal remain untouched");
        assert!(got.contains(&job), "job with the same numeric id: its proposal remains untouched");
        let explanations: Vec<_> = got.iter().filter(|e| matches!(e, RouteEvent::Explain(_))).collect();
        assert_eq!(explanations, vec![&first, &second], "explanations: both remain in publication order");

        worker.publish(g(7));
        let full = proposal(ProposalSource::Gesture { id, seq: 7, tier: Tier::Full });
        let before = wakes.load(Ordering::Relaxed);
        worker.publish(full.clone());
        assert_eq!(pending(), vec![full],
            "a later tier of the same seq replaces its untaken Stretch answer, so a reader that waits for a sample's answer must count either tier");
        assert_eq!(wakes.load(Ordering::Relaxed), before + 1,
            "publishing the Full answer wakes the UI once so a reader blocked on the wake sees it");
    }

    #[test]
    fn slot_queue_and_outbox_rules() {
        let wakes = Arc::new(AtomicU32::new(0));
        let w = wakes.clone();
        let (ui, worker) = route_channel(move || {
            w.fetch_add(1, Ordering::Relaxed);
        });
        ui.sample(sample(1));
        ui.sample(sample(2));
        ui.request(RouteRequest::Cancel(JobId(7))).unwrap();
        assert!(worker.preempted(), "waiting work preempts a running tier");
        assert!(matches!(worker.next(), Work::Sample(s) if s.seq == 2), "the newest sample only, before a command");
        assert!(matches!(worker.next(), Work::Request(RouteRequest::Cancel(JobId(7)))), "then the command");
        assert!(!worker.preempted());
        for _ in 0..QUEUE_DEPTH {
            ui.request(RouteRequest::Cancel(JobId(0))).unwrap();
        }
        assert!(ui.request(RouteRequest::Cancel(JobId(1))).is_err(), "a full queue hands the command back");
        for _ in 0..QUEUE_DEPTH {
            worker.next();
        }
        let g = |seq, tier| ProposalSource::Gesture { id: GestureId(1), seq, tier };
        worker.publish(proposal(g(1, Tier::Stretch)));
        worker.publish(proposal(ProposalSource::Job { job: JobId(3), step: 0 }));
        worker.publish(proposal(g(1, Tier::Full)));
        worker.publish(RouteEvent::Explain(Explanation { unit: BoardId(0), net: NetId(0), reason: None, blockers: Vec::new(), hints: Vec::new() }));
        worker.publish(proposal(ProposalSource::Job { job: JobId(3), step: 1 }));
        let got: Vec<RouteEvent> = std::iter::from_fn(|| ui.try_recv()).collect();
        let sources: Vec<Option<ProposalSource>> = got.iter().map(|e| if let RouteEvent::Proposal(p) = e { Some(p.source) } else { None }).collect();
        assert_eq!(sources, vec![Some(g(1, Tier::Full)), None, Some(ProposalSource::Job { job: JobId(3), step: 1 })], "untaken proposals of one source are superseded; explanations queue");
        assert_eq!(wakes.load(Ordering::Relaxed), 5, "every publish wakes the UI");
        drop(ui);
        assert!(matches!(worker.next(), Work::Closed), "dropping the handle closes the channel");
    }

    #[test]
    fn bases_then_the_sample_and_commands_in_turn() {
        let board = Arc::new(Board::default());
        let rules = Arc::new(resolve_rules(&board, None));
        let fills = Arc::new(fill_all(&board, &rules));
        let conn = Arc::new(connect(&board, &fills));
        let b1 = Base::new(board, rules, conn, fills);
        let mut board = (*b1.board).clone();
        board.generation += 1;
        let b2 = Base::new(Arc::new(board), b1.rules.clone(), b1.conn.clone(), b1.fills.clone());
        let route = RouteRequest::Route(RouteJob {
            job: JobId(1),
            unit: b1.unit,
            scope: RouteScope::All,
            method: Method::Interactive,
            engine: Engine::Grid,
            style: None,
            effort: Effort(1),
            layers: None,
            reroute: false,
        });
        let cancel = RouteRequest::Cancel(JobId(1));
        let explain = RouteRequest::Explain { unit: b1.unit, net: NetId(0) };
        let (ui, worker) = route_channel(|| {});
        for request in [route.clone(), cancel.clone(), RouteRequest::Base(b1.clone()), explain.clone(), RouteRequest::Base(b2.clone())] {
            ui.request(request).unwrap();
        }
        ui.sample(sample(7));
        let request = |work: Work| match work {
            Work::Request(request) => request,
            _ => panic!("a command was due"),
        };
        let seq = |work: Work| match work {
            Work::Sample(sample) => sample.seq,
            _ => panic!("the sample was due"),
        };
        assert_eq!(request(worker.next()), RouteRequest::Base(b1), "queued bases first, the oldest first");
        assert_eq!(request(worker.next()), RouteRequest::Base(b2), "then the next base");
        assert_eq!(seq(worker.next()), 7, "then the sample, ahead of older commands");
        ui.sample(sample(8));
        assert_eq!(request(worker.next()), route, "after a sample, one command before the next sample");
        assert_eq!(seq(worker.next()), 8, "then the waiting sample");
        ui.sample(sample(9));
        assert_eq!(request(worker.next()), cancel, "a drag that keeps sending samples cannot starve a command");
        assert_eq!(seq(worker.next()), 9);
        assert_eq!(request(worker.next()), explain, "the commands keep their order");
        assert!(!worker.preempted(), "nothing waits");
    }
}
