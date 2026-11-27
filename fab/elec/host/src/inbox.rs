//! The only input to the engine strand; a lock covers only an inbox operation, never engine work.

use fab_elec_board::{Factories, Item, PortRoom, RuleMatrix, ZoneFill};
use fab_elec_circuit::{Circuit, Violation};
use crate::EvalDone;
use fab_elec_core::{BoardId, ContentHash, DocId, ItemId, NetId, Sel};
use fab_elec_design::{By, Design, DesignOp, Place, TextEdit};
use fab_elec_route::{Reason, RouteProposal};
use fab_elec_view::{DesignSnapshot, Refusal};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// The prefix of a case keepout set's source: `FeatureRef::Panel("case:<record name>")`.
pub const CASE_KEEPOUT: &str = "case:";

/// A caller's identity for matching a command with its outcome.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Ticket(pub u64);

/// The original edit or the tagged preview the person released. An expired tag still
/// answers its press, without committing a newer sample from another pane.
pub enum PlannerEdit {
    Direct { edit: fab_elec_design::Resolved, circuits: Vec<(BoardId, Weak<Circuit>)> },
    Release { seq: Option<u64>, tag: u64 },
}

/// Ordered edits and control messages for the engine strand.
pub enum Cmd {
    Propose { slot: String, set: Vec<DocSet>, label: String, by: crate::ProposalAuthor, ticket: Ticket },
    Decide { id: crate::ProposalId, shown: u64, accept: bool, ticket: Option<Ticket> },
    CheckProposal { id: crate::ProposalId, ticket: Option<Ticket> },
    /// A view's identified planner press. The strand checks the proposed invocation against
    /// the actual edit (or the exact released sample) before linking the journal.
    Planner { call: fab_elec_view::snapshot::Invocation, edit: PlannerEdit, ticket: Ticket },
    /// Ops as one undo step. `expect` is the board content hash the ops were made against and
    /// `circuits` the unit circuits their ids name ([`circuits_named`]); a mismatch of either
    /// rejects the whole command, applying nothing and reporting it in `Step::rejected`.
    Commit { ops: Vec<DesignOp>, place: Vec<Place>, label: String, circuits: Vec<(BoardId, Weak<Circuit>)>, expect: Option<(BoardId, ContentHash)>, by: By, ticket: Option<Ticket> },
    /// Typed text (or a file changed on disk) of one document. Edits address revision `base_rev`
    /// (`DocsView.texts[..].rev`); a different current revision refuses the command and the
    /// editor reads the text again. `group`: edits of one gesture merge into one undo step. A
    /// refusal carries `tag`, so the editor knows its own.
    Text { doc: DocId, base_rev: u64, edits: Vec<TextEdit>, group: Option<u64>, label: String, tag: Option<u64>, by: By, ticket: Option<Ticket> },
    /// A caller's change of one document's text, committed only once it evaluates with no new
    /// mistake (`fab_elec_lang::text_verified`): the AI's `fab.elec_text`. Edits address revision
    /// `base_rev` as `Text`'s do. One undo step; Done's sites are its replacements and `created` what
    /// they define; a refusal is its first new mistake, located in the text after the change, and
    /// commits nothing. A person's typing is `Text`, committed first and evaluated after.
    CheckedText { doc: DocId, base_rev: u64, edits: Vec<TextEdit>, label: String, by: By, ticket: Ticket },
    /// Add, replace or remove documents, or register assets. Asset-only sets are not logged;
    /// the plate owns their bytes. Replacements and removals name the revision the caller read.
    Docs { set: Vec<DocSet>, label: String, by: By, ticket: Option<Ticket> },
    /// Newly available part library models, by project path and library file path.
    LibraryModels(Vec<(String, String)>),
    /// Part library models no longer to be used (their source was turned off), by the prefix of
    /// their project paths (`parts/lcsc/`): the parts naming them keep their own bodies again.
    LibraryDrop(String),
    /// Resolve a catalog action on the strand, then commit or preview its plan.
    Invoke { tool: String, args: Vec<(String, String)>, preview: bool, by: By, ticket: Ticket },
    /// The router asks for the committed base of `unit` (its first gesture there).
    /// Answered in `Step.bases` at the end of the step; afterwards every step whose commands
    /// or results moved that unit's committed board, rules, connectivity or fills sends its
    /// new base (O(routed units) pointer compares per step).
    RouteBase { unit: BoardId },
    /// Commit sample `seq` from the slot, else the applied preview of that seq. The preview
    /// ends in one commit or one refusal naming `tag` (the sample's): a sample already refused
    /// answers with its refusal, and one no longer held (replaced by another author's sample, or
    /// the engine reloaded) is refused as ended.
    CommitPreview { seq: u64, tag: u64 },
    /// The host refuses sample `seq` instead of committing it (the bed is changing): it ends
    /// like a cancelled one, with one refusal naming `tag` and `reason`.
    RefusePreview { seq: u64, tag: u64, reason: String },
    /// Discard this job's preview and roll back its bring if that is still the history head.
    DiscardJob { seq: u64, tag: u64 },
    /// End an applied preview and a waiting sample whose seq is at most `seq`.
    CancelPreview { seq: u64 },
    Undo,
    Redo,
    /// Factory capabilities by profile name, resolved by the host off the strand.
    SetFactories(Arc<Factories>),
    /// Mechanical case checks of `unit`, computed against that board unit's committed
    /// `generation` in the app's case check `run`; `cased`: the plate had case parts for it.
    /// Replace its previous case findings; ignore a result older than the held one. Snapshots say
    /// which run they hold (`DesignSnapshot::cases`). Not logged and not an undo step.
    /// O(C log C + waivers) for incoming and held case findings; each snapshot adds O(V + C) to
    /// merge with the other checks. Nothing runs per frame.
    CaseFindings { unit: BoardId, generation: u64, run: u64, cased: bool, findings: Vec<Violation> },
    /// The board space the plate's case keeps clear on `unit` (its parts' `m.keepouts`
    /// records naming this board), as item sets by record name. Each set replaces the unit's
    /// derived items from `FeatureRef::Panel("case:<name>")`; a `case:` set it does not name is
    /// removed, so an empty list clears them all. Not logged and not an undo step, and an
    /// unchanged set changes nothing (no board generation). Cost: one `Design` copy (O(units)
    /// `Arc`s) and O(keepouts) to find the held sets, plus `Design::set_derived` per set.
    /// `rooms` and `plan` replace the port geometry and placement rule for the evaluated board.
    /// Incompatible rules leave their reason for the next place answer.
    CaseKeepouts { unit: BoardId, generation: u64, sets: Vec<(String, Vec<Item>)>, rooms: Vec<PortRoom>, plan: Result<Option<Arc<dyn fab_elec_route::PlacePlan>>, String> },
    /// The design generation a model evaluation covered. Its records, when available,
    /// precede this outcome in the mailbox. An error leaves the last rooms available.
    CaseOutcome { unit: BoardId, stamp: u64, reason: Option<String> },
}

/// A whole-document or asset change supplied by another writer.
pub enum DocSet {
    /// Introduce a new document without an existing revision.
    Add { name: String, text: Arc<str> },
    /// Replace the document only at the revision the writer read.
    Replace { name: String, base_rev: u64, text: Arc<str> },
    /// Remove the document only at the revision the writer read.
    Remove { name: String, base_rev: u64 },
    /// Register an asset by its project-relative name and the MeshCache key holding its bytes.
    Asset { name: String, key: String },
}

/// Ops over the committed design, not logged. The newest sample replaces the waiting one.
#[derive(Clone)]
pub struct Sample {
    pub seq: u64,
    /// The pane's process-wide identity, echoed by snapshots and refusals.
    pub tag: u64,
    pub ops: Vec<DesignOp>,
    /// Footprints to place together with the sample's ops.
    pub place: Vec<Place>,
    pub label: String,
    /// The author supplied by the calling code path.
    pub by: By,
    /// The router proposal these ops come from; the snapshot echoes it as `proposal` while
    /// this sample is the live preview.
    pub proposal: Option<Arc<RouteProposal>>,
    /// The router's `Reason::SharpCorner` findings that outlive the answer as session
    /// notes on this edit. Empty for other previews.
    pub notes: Vec<(NetId, Reason)>,
    /// The unit circuits the ops' ids name, as the caller planned against them
    /// ([`circuits_named`]). Once one changes, the sample is refused with a reason and never
    /// evaluated or committed on the new circuit. Empty checks nothing.
    pub circuits: Vec<(BoardId, Weak<Circuit>)>,
    /// The board content hash the ops were made against (a router answer's base). A mismatch
    /// drops the sample without a refusal: the router answers again on the new base.
    pub expect: Option<(BoardId, ContentHash)>,
}

/// The committed circuit of each unit a circuit op of `ops` names, in `design` as the caller
/// planned against it. `CompId`, `PinIx` and `NetId` are positions in that circuit: after another
/// author's circuit commit the same ids may name other parts, pins or nets, so the host refuses
/// ops whose circuits are not these any more. A `Weak` keeps the allocation, never the circuit, so
/// a pointer comparison cannot match a later circuit. Board ops (stable `ItemId`s) and bench ops
/// (names) are not stamped. Cost: O(ops + units).
pub fn circuits_named(design: &Design, ops: &[DesignOp]) -> Vec<(BoardId, Weak<Circuit>)> {
    let mut out: Vec<(BoardId, Weak<Circuit>)> = Vec::new();
    for op in ops {
        let DesignOp::Circuit { unit, .. } = op else { continue };
        if out.iter().any(|(named, _)| named == unit) {
            continue;
        }
        if let Some(found) = design.unit(*unit) {
            out.push((*unit, Arc::downgrade(&found.circuit)));
        }
    }
    out
}

/// What a commit changes of what the bench simulates: nothing (a board, drawing or text edit),
/// part values only (a playing run takes them in where it hears them), or its structure (the run starts over).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CircuitChange {
    None,
    Values,
    Structure,
}

/// The engine's answer to a ticketed command.
pub enum Outcome {
    Proposed {
        id: crate::ProposalId,
        shown: u64,
        checks: Option<Arc<crate::ProposalChecks>>,
        requirements: Option<Arc<fab_elec_view::snapshot::ReqSummary>>,
    },
    Declined { id: crate::ProposalId },
    /// The logged edit, its author and any newly created items or affected source sites (a
    /// text commit's sites are the ranges it wrote, in the text after it). `targets` are what
    /// its ops edit in place and keep (a value's part, a moved item): the cue's exact sites.
    Done {
        seq: u64,
        label: String,
        created: Vec<Sel>,
        sites: Vec<Sel>,
        by: By,
        targets: Vec<Sel>,
        /// The effect of this commit on the simulated circuit.
        circuit: CircuitChange,
        /// Extra answer fields (key, Splash value text) supplied by this edit's resolver.
        notes: Vec<(String, String)>,
    },
    /// The sample accepted as the live preview.
    Previewed { tag: u64 },
    /// The same refusal shared with the published snapshot.
    Refused(Arc<Refusal>),
}

pub enum TaskResult {
    /// An evaluation job's result (`EvalJob::run`).
    Eval(Box<EvalDone>),
    /// A pool task's fill of `zone` over the unit's board at `generation`, with `rules` (kept, so
    /// the merge can tell they are still the unit's by `Arc` identity).
    Fill {
        unit: BoardId,
        zone: ItemId,
        generation: u64,
        rules: Arc<RuleMatrix>,
        fill: ZoneFill,
        /// The live preview's sample seq the fill was made for; None: the committed results.
        preview: Option<u64>,
    },
    /// A pool task's whole-board checks of the unit's board at `generation` over `rules` and
    /// `fills` (kept, so the merge can tell they are still the unit's by `Arc` identity).
    Checks {
        unit: BoardId,
        generation: u64,
        rules: Arc<RuleMatrix>,
        fills: Arc<Vec<ZoneFill>>,
        conn: Arc<fab_elec_board::Connectivity>,
        drc: Arc<Vec<Violation>>,
    },
}

#[derive(Default)]
pub struct Inbox {
    pub cmds: Vec<(Instant, Cmd)>,
    pub preview: Option<Sample>,
    pub results: Vec<TaskResult>,
    /// Snapshots the UI replaced, so the last reference drops on the worker.
    pub dead: Vec<Arc<DesignSnapshot>>,
    /// Router answers the UI replaced or ignored, so their last reference drops on the strand.
    pub dead_answers: Vec<Arc<RouteProposal>>,
    /// Samples a newer sample replaced before the strand took them.
    pub dead_samples: Vec<Sample>,
    /// Samples a queued `CommitPreview` names (seq and tag) that a newer sample then replaced in
    /// the slot before the strand took them: the commit still finds them, in its place in the
    /// queue. Each ends in that commit or, unclaimed, one "ended" refusal in the same step.
    pub committing: Vec<Sample>,
}

/// The shared inbox: every method holds one short lock, never across work.
#[derive(Clone, Default)]
pub struct Mailbox(Arc<Mutex<Inbox>>);

impl Mailbox {
    fn lock(&self) -> MutexGuard<'_, Inbox> {
        self.0.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn push(&self, cmd: Cmd) {
        self.lock().cmds.push((Instant::now(), cmd));
    }

    /// Replace the slot's sample. One the host already queued a `CommitPreview` for (same seq
    /// and tag) is kept for that commit; any other is retired for disposal on the strand.
    /// O(queued commands) when a sample is replaced.
    pub fn preview(&self, s: Sample) {
        let mut inbox = self.lock();
        if let Some(old) = inbox.preview.replace(s) {
            let named = inbox.cmds.iter().rev()
                .any(|(_, cmd)| matches!(cmd, Cmd::CommitPreview { seq, tag } | Cmd::Planner { edit: PlannerEdit::Release { seq: Some(seq), tag }, .. } if *seq == old.seq && *tag == old.tag));
            if named {
                inbox.committing.push(old);
            } else {
                inbox.dead_samples.push(old);
            }
        }
    }

    pub fn result(&self, r: TaskResult) {
        self.lock().results.push(r);
    }

    pub fn retire(&self, s: Arc<DesignSnapshot>) {
        self.lock().dead.push(s);
    }

    pub fn retire_answer(&self, a: Arc<RouteProposal>) {
        self.lock().dead_answers.push(a);
    }

    #[cfg(test)]
    pub(crate) fn peek<R>(&self, f: impl FnOnce(&Inbox) -> R) -> R {
        f(&self.lock())
    }

    pub fn take(&self) -> Inbox {
        std::mem::take(&mut *self.lock())
    }

    /// A command or a preview sample waits: work a person waits on, not only task results.
    pub fn has_commands(&self) -> bool {
        let inbox = self.lock();
        !inbox.cmds.is_empty() || inbox.preview.is_some()
    }

    /// Retired snapshots, answers and samples alone do not count as waiting work.
    pub fn is_empty(&self) -> bool {
        let inbox = self.lock();
        inbox.cmds.is_empty() && inbox.preview.is_none() && inbox.results.is_empty()
    }

    /// How long the oldest waiting command has waited (`None` when none waits).
    pub fn waited(&self, now: Instant) -> Option<Duration> {
        self.lock().cmds.first().map(|(at, _)| now.saturating_duration_since(*at))
    }
}
