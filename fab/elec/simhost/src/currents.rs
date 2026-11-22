//! Segment currents from component pin currents, computed on one long-lived worker.
//! The UI queues the newest job without waiting and takes the latest published rows. The worker
//! wakes nobody: the host sends jobs on its frames and asks one more frame while a job is waiting
//! ([`CurrentsLink::waiting`]), so a result is taken on the next frame. A wake per result would
//! answer each job with a UI event that sends the next one, at the event loop's rate.

use crate::Spawn;
use fab_elec_view::live::segs::Resolved;
use fab_elec_sim::live::Latest;
use fab_elec_view::live::net::NetCurrents;
use std::sync::{mpsc::{sync_channel, Receiver, SyncSender, TrySendError}, Arc};

/// One computation: every registered tree with one set of pin currents (into each pin, by
/// `ValueIndex::pin` index, as `LiveValues::pin_mean` or a scrub sample).
#[derive(Clone, Debug)]
pub struct CurrentsJob {
    pub seq: u64,
    pub trees: Arc<Vec<Resolved>>,
    pub pin_i: Arc<[f32]>,
}

/// The rows of every tree in order, each with its segment's current in amperes (positive from
/// its first node to its second); `unconverged` is the scratch's cumulative count.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SegCurrents {
    pub seq: u64,
    pub rows: Vec<u32>,
    pub i: Vec<f32>,
    pub unconverged: u64,
}

/// Compute each tree into `scratch`, appending its rows and currents to the cleared `out`.
/// Mismatched row and segment counts contribute their common prefix. All storage is reused.
pub fn compute(nc: &mut NetCurrents, job: &CurrentsJob, scratch: &mut Vec<f32>, out: &mut SegCurrents) {
    out.seq = job.seq;
    out.rows.clear();
    out.i.clear();
    for resolved in job.trees.iter() {
        nc.currents(&resolved.tree, &job.pin_i, scratch);
        for (&row, &current) in resolved.rows.iter().zip(scratch.iter()) {
            out.rows.push(row);
            out.i.push(current);
        }
    }
    out.unconverged = nc.unconverged();
}

/// Block for a job, skip to the newest queued job, compute and publish; exit when senders leave.
pub fn run_currents(jobs: Receiver<CurrentsJob>, out: Arc<Latest<SegCurrents>>) {
    let mut nc = NetCurrents::new();
    let mut scratch = Vec::new();
    let mut result = SegCurrents::default();
    while let Ok(mut job) = jobs.recv() {
        while let Ok(newest) = jobs.try_recv() {
            job = newest;
        }
        compute(&mut nc, &job, &mut scratch, &mut result);
        // Recycle the replaced buffers when no reader still owns that publication.
        result = out.publish(Arc::new(result))
            .and_then(|old| Arc::try_unwrap(old).ok())
            .unwrap_or_default();
    }
}

/// The UI side of one worker, spawned once, with at most one job kept for a later frame.
pub struct CurrentsLink {
    jobs: SyncSender<CurrentsJob>,
    kept: Option<CurrentsJob>,
    out: Arc<Latest<SegCurrents>>,
    taken: u64,
    next_seq: u64,
    /// The newest job sent to the worker, and the job of the newest result taken.
    sent: u64,
    done: u64,
}

impl CurrentsLink {
    /// Spawn the long-lived worker as `elec currents`.
    pub fn new(spawn: &dyn Spawn) -> CurrentsLink {
        let (jobs, receiver) = sync_channel(2);
        let out = Arc::new(Latest::new());
        let worker_out = out.clone();
        spawn.worker("elec currents", Box::new(move || run_currents(receiver, worker_out)));
        CurrentsLink { jobs, kept: None, out, taken: 0, next_seq: 1, sent: 0, done: 0 }
    }

    /// Queue without waiting, replacing any kept job; retry a full channel at the next `pump`.
    /// Sequence numbers start at one and saturate at `u64::MAX`.
    pub fn submit(&mut self, trees: Arc<Vec<Resolved>>, pin_i: Arc<[f32]>) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.kept = Some(CurrentsJob { seq, trees, pin_i });
        self.pump();
        seq
    }

    /// Send a kept job if there is room now; a disconnected worker has nothing left to receive.
    pub fn pump(&mut self) {
        let Some(job) = self.kept.take() else { return };
        let seq = job.seq;
        match self.jobs.try_send(job) {
            Ok(()) => self.sent = seq,
            Err(TrySendError::Full(job)) => self.kept = Some(job),
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Whether a job is kept, waiting for room.
    pub fn backlogged(&self) -> bool {
        self.kept.is_some()
    }

    /// Whether a job is kept or sent with its result not yet taken: the host asks one more frame.
    /// The worker skips to the newest job it holds, so the newest sent job's result ends it.
    pub fn waiting(&self) -> bool {
        self.kept.is_some() || self.done < self.sent
    }

    /// Take the newest result once per publication, or `None` when nothing new was published.
    pub fn take(&mut self) -> Option<Arc<SegCurrents>> {
        if self.out.published() == self.taken {
            return None;
        }
        let (count, result) = self.out.latest()?;
        if count == self.taken {
            return None;
        }
        self.taken = count;
        self.done = result.seq;
        Some(result)
    }
}
