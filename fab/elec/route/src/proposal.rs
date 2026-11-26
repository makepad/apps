//! What the route worker publishes and the board canvas draws (E6 1.3, 3): proposals. A
//! proposal is board ops against a stated committed generation, its metrics, and whether it is
//! verified clean; an unverified one names its collisions so the canvas marks them. A preview
//! is never silently illegal.

use crate::gesture::GestureId;
use crate::progress::{JobPhase, RetryWhy};
use crate::solution::{Reason, RouteMetrics};
use fab_elec_board::{Anchor, Board, BoardOp, Connectivity};
use fab_elec_circuit::Violation;
use fab_elec_core::{
    canon_enum, canon_struct, canon_tags, BoardId, Canon, ContentHash, LayerId, NetId, Nm, Path, Sel, Writer, P,
};
use std::sync::Arc;

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
pub struct ProposalId(pub u32);

/// Names one engine job (route, restyle, place) from its request to its last proposal.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
pub struct JobId(pub u32);
canon_struct!(JobId { 0 });

impl JobId {
    /// The host tag of this job's samples: bit 63 set, so it never equals a gesture tag
    /// (`GestureId::tag` is below 2^32).
    pub fn tag(self) -> u64 {
        1 << 63 | u64::from(self.0)
    }
}

/// A running job's progress, for the status line; the newest per job replaces an untaken one.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct JobStatus {
    pub job: JobId,
    pub unit: BoardId,
    pub phase: JobPhase,
    pub current: Option<NetId>,
    pub routed: u32,
    pub failed: u32,
    pub total: u32,
    /// The current net's attempt (0: its first route), why it is routed again, and the net its
    /// route would cut off when it stays blocked (`NetStatus`).
    pub attempt: u32,
    pub retry: Option<RetryWhy>,
    pub blocked_by: Option<NetId>,
}


/// The two answers per sample (E6 3.4).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Tier {
    /// Walk or stretch only: published first.
    Stretch,
    /// The shove wave and the re-solve of solved nets, for the same sample.
    Full,
}
canon_tags!(Tier);


/// What a proposal answers.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ProposalSource {
    /// A gesture sample. The canvas compares `seq` with its newest sample's to know how stale
    /// the answer is.
    Gesture { id: GestureId, seq: u64, tier: Tier },
    /// A job's partial after its `step`-th transaction: its content is a function of the problem
    /// and `step` only; wall time picks only which steps are published (gate 3 change 6).
    Job { job: JobId, step: u32 },
}
canon_enum!(ProposalSource { 0 => Gesture { id, seq, tier }, 1 => Job { job, step } });


/// A collision an unverified proposal contains: between one of its own items and another.
#[derive(Clone, PartialEq, Debug)]
pub struct Collision {
    pub at: P,
    pub layer: LayerId,
    pub a: Sel,
    pub b: Sel,
    pub gap: Nm,
    pub need: Nm,
}
canon_struct!(Collision { at, layer, a, b, gap, need });


/// The proposal's ops applied to its base on the worker, with the checks run on the result: where
/// `verified`, the collisions and the metrics come from. The host's preview of the proposal derives
/// its own results through the one preview path, with the same checks. `violations` is the unit's
/// whole list, the base's merged with the proposal's `drc` delta.
#[derive(Clone, PartialEq, Debug)]
pub struct Checked {
    pub board: Arc<Board>,
    pub conn: Arc<Connectivity>,
    pub violations: Arc<Vec<Violation>>,
}
canon_struct!(Checked { board, conn, violations });


/// Why a head stops short of its target.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum BlockCause {
    /// Two obstacles the head passes on opposite sides leave less than `need` between them.
    Gap,
    /// The head's end lies in an obstacle's clearance.
    End,
    /// A via at a leg end has no legal place along its leg.
    Via,
    /// The gap fits, but the embedder found no octilinear pass through it: a quality bug, logged.
    Thread,
    /// The embedder passed its proof's step bound: a bug, logged once.
    Unsettled,
}
canon_tags!(BlockCause);


/// Where a head stops short of its target, and why (E6 3.1). `need` and `have` are exact for
/// `Gap` and `End` (`have < need`); for the other causes they are the gap the head needed and found
/// there, or 0 when no gap applies.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Blocked {
    pub at: P,
    pub cause: BlockCause,
    /// The gap the head needs there (its width plus the clearances on both sides).
    pub need: Nm,
    pub have: Nm,
}
canon_struct!(Blocked { at, cause, need, have });

impl Blocked {
    /// Canvas text, rounding available room down and required room up to hundredths of a mm.
    pub fn summary(&self) -> String {
        let mm = |value: Nm, up: bool| {
            let value = value as i64;
            let cents = if up { -(-value).div_euclid(10_000) } else { value.div_euclid(10_000) };
            let sign = if cents < 0 { "-" } else { "" };
            format!("{sign}{}.{:02}", cents.abs() / 100, cents.abs() % 100)
        };
        match self.cause {
            BlockCause::Gap => format!("gap {} mm, needs {} mm", mm(self.have, false), mm(self.need, true)),
            BlockCause::End => format!("end in clearance: {} of {} mm", mm(self.have, false), mm(self.need, true)),
            BlockCause::Via => "no room for the via on this leg".into(),
            BlockCause::Thread => "no 45-degree pass through this gap".into(),
            BlockCause::Unsettled => "the router could not settle this path".into(),
        }
    }
}

/// A gesture answer's head, summarised for the canvas (the cursor chip, the clearance halo, the via
/// key), so the pane never reads it out of the ops: the solved path from the head start, its layer
/// and width, the anchor it ends on, its length, what stops it short of the target, and the track a
/// dragged segment or copper drawn in Shove mode could not push.
#[derive(Clone, PartialEq, Debug)]
pub struct HeadAnswer {
    pub path: Path,
    pub layer: LayerId,
    pub w: Nm,
    /// The verified target, or `Anchor::Free` at the head's end.
    pub end: Anchor,
    pub length: Nm,
    pub blocked: Option<Blocked>,
    /// A dragged segment, or copper drawn in Shove mode, that would have pushed tracks but could
    /// not push this one (its net, and why): it walks round them as they are instead, as in Walk
    /// mode, or stays where it is (`blocked`).
    pub unpushed: Option<(NetId, Reason)>,
}
canon_struct!(HeadAnswer { path, layer, w, end, length, blocked, unpushed });


/// One proposal: the ops to apply to the committed board of generation `base_generation`, whose
/// content hash is `base_hash`. The host shows it only while that board is the committed one
/// (it drops a proposal whose `base_hash` differs), and commits it by applying the ops through the
/// one applier.
#[derive(Clone, PartialEq, Debug)]
pub struct RouteProposal {
    pub id: ProposalId,
    pub source: ProposalSource,
    pub unit: BoardId,
    pub base_generation: u64,
    /// The content hash of the board it was made against.
    pub base_hash: ContentHash,
    /// Complete against the base: a newer proposal from the same source replaces this one.
    pub ops: Vec<BoardOp>,
    /// The ops applied and checked on the worker. `None` for a gesture answer, which is verified
    /// pair by pair in its own overlay (the host's preview runs the full checks), and for a job
    /// proposal the worker could not apply (then `verified` is false and `collisions` says why).
    /// The source tells the two apart.
    pub checked: Option<Arc<Checked>>,
    pub metrics: RouteMetrics,
    /// Every item it adds passed the verifier (the pair and item checks DRC uses).
    pub verified: bool,
    /// When not verified: what collides, for the canvas to mark.
    pub collisions: Vec<Collision>,
    /// Nets still open, then the junctions the answer opened (`Reason::Unjoined`), then the
    /// sharp corners it laid (`Reason::SharpCorner`).
    pub open: Vec<(NetId, Reason)>,
    /// A gesture answer's head; `None` for a job's proposal.
    pub head: Option<HeadAnswer>,
    /// A pair route's coupled and escape measurements on this proposal.
    pub pair: Option<crate::pair::PairReport>,
    /// The worker's time from taking the sample to publishing this answer, in microseconds, for the
    /// pane's latency log; 0 for a job's proposal. Not content: `Canon` leaves it out.
    pub solve_us: u32,
    /// No further proposal follows from this source (a job is done; a rested sample's second
    /// tier is complete).
    pub last: bool,
    pub label: String,
}
/// Content only: `id` numbers publications and `solve_us` is wall time; both are left out, so a
/// replay of the same step writes the same bytes (E6 9.4).
impl Canon for RouteProposal {
    fn canon(&self, w: &mut Writer) {
        let RouteProposal {
            id: _,
            source,
            unit,
            base_generation,
            base_hash,
            ops,
            checked,
            metrics,
            verified,
            collisions,
            open,
            head,
            pair,
            solve_us: _,
            last,
            label,
        } = self;
        source.canon(w);
        unit.canon(w);
        base_generation.canon(w);
        base_hash.canon(w);
        ops.canon(w);
        checked.canon(w);
        metrics.canon(w);
        verified.canon(w);
        collisions.canon(w);
        open.canon(w);
        head.canon(w);
        last.canon(w);
        label.canon(w);
        if let Some(report) = pair { w.tag(1); report.canon(w); }
    }
}


/// `elec.route.explain`'s answer (E6 10): why a net is open and what would fix it.
#[derive(Clone, PartialEq, Debug)]
pub struct Explanation {
    pub unit: BoardId,
    pub net: NetId,
    /// `None` when the net is routed.
    pub reason: Option<Reason>,
    /// The items on its best corridor that block it.
    pub blockers: Vec<Sel>,
    /// Fix hints in words ("move C4 0.4 mm left", "allow layer 3").
    pub hints: Vec<String>,
}

/// Why the worker answers a gesture sample without a proposal.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SampleCause {
    /// The worker holds no base for the sample's unit yet.
    NoBase,
    /// A gesture kind or tier the worker cannot answer.
    Unsupported,
    /// A layer, net, width or anchor the base does not have.
    Invalid,
}

/// A gesture sample's answer when there is no proposal: its gesture, its seq and why.
#[derive(Clone, PartialEq, Debug)]
pub struct SampleRefused {
    pub id: GestureId,
    pub seq: u64,
    pub unit: BoardId,
    pub cause: SampleCause,
    /// Canvas text.
    pub text: String,
}

/// What the canvas receives from the route worker, in order.
#[derive(Clone, PartialEq, Debug)]
pub enum RouteEvent {
    Proposal(RouteProposal),
    Explain(Explanation),
    /// A running job's latest phase, current net and counts for the status line.
    Status(JobStatus),
    /// A gesture sample answered without a proposal, with its seq and the reason.
    Refused(SampleRefused),
}
