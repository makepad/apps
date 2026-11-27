//! Document proposals live on the strand. Their pool work shares the verified-preview
//! adoption path, while their identity, document diffs and decisions survive gestures.
//!
//! Planning shares source texts and copies replacement bytes on the strand. Verification
//! evaluates changed units, compares boards and builds bench views on the existing pool.
//! Publishing a verified candidate runs the same changed-board pass and design-wide ERC as
//! a verified gesture. Later snapshots share one view Arc per visible proposal. A decision
//! compares the input stamp and installs the carried results without another evaluation.
use super::*;
use crate::{ProposalChecks, ProposalDoc, ProposalView};
use fab_elec_core::Diag;
use fab_elec_lang::SpanEdit;
use fab_elec_req::{Family, Measure, Status};
use fab_elec_view::snapshot::{ProposalBoard, ReqSummary};

type Plan = (Sources, SpanEdit);

pub(super) struct Candidate {
    pub(super) carry: Carry,
    pub(super) profiles: Option<ResolvedProfiles>,
    pub(super) design: Arc<Design>,
    pub(super) results: DesignResults,
    pub(super) sheets: Vec<Sheet>,
    pub(super) created: Vec<Sel>,
}

impl Candidate {
    /// Read all present requirements, including documents the proposal did not touch. Reuse
    /// the same unaggregated findings as the check counts. Only DRC and connectivity on a
    /// board awaiting copper work depend on that work; ERC and other boards stay measurable.
    fn requirements(&self, findings: &[Violation], awaiting: &[u64]) -> Option<Arc<ReqSummary>> {
        let start = Instant::now();
        let docs: Vec<_> = self.carry.sources.all_docs().iter().filter(|d| d.present && d.kind == DocKind::Requirements)
            .map(|d| fab_elec_req::read(d.id, &d.text).0).collect();
        if docs.is_empty() { return None; }

        let facts = crate::req_actions::measured_facts(&self.design, findings, awaiting, Vec::new());
        let pending: HashSet<_> = self.results.boards.iter().filter(|b| b.pending.whole || !b.pending.zones.is_empty())
            .filter_map(|b| self.design.unit(b.unit).map(|u| u.name.as_str())).collect();
        let mut boards = facts.units.iter().filter(|u| u.board.is_some());
        let only_board_pending = matches!((boards.next(), boards.next()), (Some(u), None) if pending.contains(u.unit.as_str()));
        let report = fab_elec_req::judge_with_unavailable(&docs, &facts, |measure| match measure {
            Measure::CaseFits(_) | Measure::Bench(_) | Measure::BenchExpect { .. } | Measure::PrintTime | Measure::PrintFilament =>
                Some("not judged for a proposal"),
            Measure::BoardUnrouted(None) if only_board_pending => Some("board checks are still pending"),
            Measure::BoardUnrouted(Some(u)) if pending.contains(u.as_str()) => Some("board checks are still pending"),
            Measure::Checks { family, unit, .. } if family.counts(Family::Drc) &&
                unit.as_ref().map_or(!pending.is_empty(), |u| pending.contains(u.as_str())) => Some("board checks are still pending"),
            _ => None,
        });
        let mut summary = ReqSummary { pass: report.pass, fail: report.fail, unknown: report.unknown, waived: report.waived,
            ..Default::default() };
        for row in report.rows {
            match row.status {
                Status::Fail => summary.failures.push((row.name, format!("{}, required {}", row.value.unwrap_or(row.measure), row.bound))),
                Status::Unknown(why) => summary.unjudged.push((row.name, why)),
                Status::Pass | Status::Waived { .. } => {}
            }
        }
        let elapsed = start.elapsed();
        if elapsed > std::time::Duration::from_millis(2) {
            makepad_widgets_core::log!("electronics: judging proposal requirements took {:.2} ms", elapsed.as_secs_f64() * 1000.);
        }
        Some(Arc::new(summary))
    }
}

fn proposal_board(board: &BoardResults) -> ProposalBoard {
    ProposalBoard { unit: board.unit, fills: board.fills.clone(),
        pending_zones: board.pending.zones.iter().map(|&(id, _)| id).collect() }
}

enum HeldState {
    Unchecked(Plan),
    /// Some until the single proposal job slot takes the newest plan.
    Checking(Option<Plan>),
    Ready(Box<Candidate>),
    /// The snapshot carries the terminal state and its reason.
    Unusable,
}

pub(super) struct Held {
    pub(super) view: Arc<ProposalView>,
    set: Vec<DocSet>,
    base: Option<BaseStamp>,
    // Includes the existing boards removed implicitly by a circuit deletion.
    touched: Vec<(DocId, u64)>,
    summary_change: String,
    state: HeldState,
    ticket: Option<Ticket>,
    presses: Vec<(u64, Option<Ticket>)>,
}

pub(super) struct ProposalJob {
    pub(super) job: u64,
    id: ProposalId,
    before: Sources,
    libs: Arc<Libs>,
}

/// Revisions catch an edit followed by undo. Model-only changes (including derived
/// keepouts) also matter: a carry must never replace a newer design.
struct BaseStamp {
    revision: u64,
    design: Weak<Design>,
    libs: Weak<Libs>,
    factories: Weak<Factories>,
}

impl BaseStamp {
    fn new(engine: &Engine, docs: &Docs) -> Self {
        Self { revision: docs.revision(), design: Arc::downgrade(&engine.design),
            libs: Arc::downgrade(docs.libs()), factories: Arc::downgrade(&engine.factories) }
    }

    fn matches(&self, engine: &Engine, revision: u64) -> bool {
        self.revision == revision && self.design.as_ptr() == Arc::as_ptr(&engine.design)
            && self.factories.as_ptr() == Arc::as_ptr(&engine.factories)
            && engine.docs.as_ref().is_some_and(|d| self.libs.as_ptr() == Arc::as_ptr(d.libs()))
    }
}

fn next_id() -> ProposalId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    ProposalId(NEXT.fetch_add(1, SeqCst))
}

/// Include the board deletions implicit in a circuit removal. Shared texts keep publication
/// O(changed documents) after the plan. A creation has no prior text, a removal no next text.
fn documents(before: &Sources, after: &Sources, edit: &SpanEdit) -> Vec<ProposalDoc> {
    edit.edits.iter().map(|e| e.doc).chain(edit.present.iter().map(|p| p.0)).map(|id| {
        let doc = &after.all_docs()[id.ix()];
        ProposalDoc { name: doc.name.clone(), before: before.doc(id).map(|d| d.text.clone()),
            after: after.doc(id).map(|d| d.text.clone()) }
    }).collect()
}

fn plan(docs: Option<&Docs>, set: &[DocSet], by: &ProposalAuthor) -> Result<(Plan, Vec<ProposalDoc>), Diag> {
    let docs = docs.ok_or_else(|| Diag::error("the engine has no project documents", vec![]))?;
    for change in set {
        let name = match change {
            DocSet::Add { name, .. } | DocSet::Replace { name, .. } | DocSet::Remove { name, .. } => name,
            DocSet::Asset { .. } => return Err(Diag::error("a document proposal cannot register assets", vec![])),
        };
        if DocKind::of_name(name) == Some(DocKind::Firmware) {
            return Err(Diag::error("firmware needs the app's compile check before it can be proposed", vec![]));
        }
    }
    let (sources, mut edit) = docs.plan_set(set)?;
    let changes = documents(docs.sources(), &sources, &edit);
    // Only the person waives. A tool's text and a peer's text may propose a waiver, never grant one;
    // the check reads the requirements text and runs nothing, so it applies to a peer's text too.
    let not_the_person = match by { ProposalAuthor::Local(author) => author.is_tool(), ProposalAuthor::Peer { .. } => true };
    check_requirements(docs.sources(), &sources, &changes, not_the_person)?;
    // A set has at most one replacement per document. Its start is unchanged in the new text.
    edit.written = edit.edits.iter().map(|e| SourceRange { doc: e.doc, span: e.span.start..e.span.start + e.text.len() as u32 })
        .chain(edit.present.iter().filter(|(_, present)| *present).map(|&(doc, _)| SourceRange {
            doc, span: 0..sources.all_docs()[doc.ix()].text.len() as u32,
        })).collect();
    Ok(((sources, edit), changes))
}

fn check_requirements(before: &Sources, after: &Sources, changes: &[ProposalDoc], tool: bool) -> Result<(), Diag> {
    for change in changes {
        let sources = if change.after.is_none() { before } else { after };
        if let Some(id) = sources.find(&change.name).filter(|_| DocKind::of_name(&change.name) == Some(DocKind::Requirements)) {
            crate::req_actions::check_text(id, &change.name, change.before.as_deref().unwrap_or(""),
                change.after.as_deref().unwrap_or(""), tool)
                .map_err(|why| Diag::error(why, vec![Sel::Source { doc: id, start: 0, end: 0 }]))?;
        }
    }
    Ok(())
}

impl Held {
    fn clear_design(&mut self) -> &mut ProposalView {
        let view = Arc::make_mut(&mut self.view);
        view.design = None;
        view.boards = Arc::default();
        view.sheets = Arc::default();
        view.created = Arc::default();
        view
    }

    /// A superseded verification may still be running. Its clock stays with that job.
    fn checking(&mut self, plan: Option<Plan>) {
        self.state = HeldState::Checking(plan);
        let view = self.clear_design();
        view.run = Arc::default();
        view.state = ProposalState::Checking;
    }

    /// Checks and requirements are one decision revision, including a change only to a reason.
    fn show_summaries(&mut self, checks: Arc<ProposalChecks>, requirements: Option<Arc<ReqSummary>>) -> bool {
        let requirements_changed = self.view.requirements != requirements;
        if !requirements_changed && self.view.checks.as_ref() == Some(&checks) { return false; }
        let view = Arc::make_mut(&mut self.view);
        self.summary_change = if let Some(old) = &view.checks {
            let mut differences = Vec::new();
            for key in old.counts.keys().chain(checks.counts.keys()).collect::<BTreeSet<_>>() {
                let before = old.counts.get(key).copied().unwrap_or(0);
                let after = checks.counts.get(key).copied().unwrap_or(0);
                if before != after { differences.push(format!("{:?} {:?} {before} to {after}", key.0, key.1)); }
            }
            if old.pending != checks.pending { differences.push(format!("pending {} to {}", old.pending, checks.pending)); }
            if requirements_changed { differences.push("requirement results changed".into()); }
            differences.join(", ")
        } else {
            match &view.by {
                ProposalAuthor::Peer { .. } => "the peer text now has its first verified checks",
                ProposalAuthor::Local(_) => "the proposed text now has its first verified checks",
            }.into()
        };
        view.shown += 1;
        view.checks = Some(checks);
        view.requirements = requirements;
        true
    }
}

impl Engine {
    fn proposal_refusal(&mut self, label: String, diag: Diag, ticket: Option<Ticket>,
        rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) {
        let site = self.docs.as_ref().and_then(|docs| docs.site_of(&diag));
        rejected.push(self.refuse(label, "proposal", None, Refused { text: diag.message, site }));
        self.refusal_outcome(ticket, outcomes);
    }

    pub(super) fn propose(&mut self, slot: String, set: Vec<DocSet>, label: String, by: ProposalAuthor,
        ticket: Ticket, rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) {
        let (plan, docs) = match plan(self.docs.as_ref(), &set, &by) {
            Ok(p) => p,
            Err(d) => return self.proposal_refusal(label, d, Some(ticket), rejected, outcomes),
        };
        if let Some(i) = self.proposals.iter().position(|p| *p.view.slot == slot) {
            self.end_proposal(i, rejected, outcomes);
        }
        let id = next_id();
        let peer = matches!(by, ProposalAuthor::Peer { .. });
        let state = if peer { ProposalState::Unchecked } else { ProposalState::Checking };
        let view = Arc::new(ProposalView {
            id, shown: 1, slot: slot.into(), label: label.into(), by, state, run: Arc::default(), docs: Arc::new(docs),
            design: None, boards: Arc::default(), sheets: Arc::default(), created: Arc::default(), checks: None, requirements: None,
        });
        if peer {
            outcomes.push((ticket, Outcome::Proposed { id, shown: view.shown, checks: None, requirements: None }));
        }
        let docs = self.docs.as_ref().unwrap();
        let touched = view.docs.iter().filter(|d| d.before.is_some()).filter_map(|d|
            docs.view().texts.iter().find(|t| t.name == d.name).map(|t| (t.doc, t.rev))).collect();
        let base = Some(BaseStamp::new(self, docs));
        let state = if peer { HeldState::Unchecked(plan) } else { HeldState::Checking(Some(plan)) };
        self.proposals.push(Held { view, set, base, touched, summary_change: String::new(), state,
            ticket: (!peer).then_some(ticket), presses: vec![] });
    }

    fn end_proposal(&mut self, i: usize, rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) {
        let held = self.proposals.remove(i);
        for ticket in held.ticket.into_iter().map(Some).chain(held.presses.into_iter().map(|p| p.1)) {
            self.proposal_refusal(held.view.label.to_string(), Diag::error("this proposal has ended", vec![]), ticket, rejected, outcomes);
        }
    }

    pub(super) fn check_proposal(&mut self, id: ProposalId, ticket: Option<Ticket>,
        rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) {
        let Some(p) = self.proposals.iter_mut().find(|p| p.view.id == id) else {
            return self.proposal_refusal("Check proposal".into(), Diag::error("this proposal has ended", vec![]), ticket, rejected, outcomes);
        };
        let HeldState::Unchecked((sources, _)) = &p.state else {
            let label = p.view.label.to_string();
            return self.proposal_refusal(label, Diag::error("this proposal is no longer unchecked", vec![]), ticket, rejected, outcomes);
        };
        if let Err(diag) = check_requirements(self.docs.as_ref().expect("a proposal has documents").sources(), sources, &p.view.docs, true) {
            let label = p.view.label.to_string();
            Arc::make_mut(&mut p.view).state = ProposalState::Stale(diag.message.clone());
            p.state = HeldState::Unusable;
            return self.proposal_refusal(label, diag, ticket, rejected, outcomes);
        }
        let HeldState::Unchecked(plan) = std::mem::replace(&mut p.state, HeldState::Checking(None)) else { unreachable!() };
        p.checking(Some(plan));
        p.ticket = ticket;
    }

    pub(super) fn schedule_proposal(&mut self, jobs: &mut Vec<EvalJob>) {
        if self.proposal_job.is_some() { return; }
        let Some((id, (sources, edit), run)) = self.proposals.iter_mut().find_map(|p| match &mut p.state {
            HeldState::Checking(plan) => plan.take().map(|plan| (p.view.id, plan, p.view.run.clone())),
            _ => None,
        }) else { return };
        let docs = self.docs.as_ref().expect("a proposal has documents");
        self.evals += 1;
        let job = EvalJob {
            id: self.evals, preview: None, sources: docs.sources().clone(), libs: docs.libs().clone(),
            prev: Arc::new(docs.prev(&self.design)), benches: docs.bench_views().clone(),
            verify: None, text: None, place: vec![], plan: Some((sources, edit, run)),
        };
        self.proposal_job = Some(ProposalJob { job: job.id, id, before: job.sources.clone(), libs: job.libs.clone() });
        jobs.push(job);
    }

    pub(super) fn finish_proposal(&mut self, done: EvalDone, queue: &mut VecDeque<Queued>,
        rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) {
        let job = self.proposal_job.take().expect("matched a running proposal");
        let Some(i) = self.proposals.iter().position(|p| p.view.id == job.id) else { return };
        let held = &self.proposals[i];
        if !matches!(held.state, HeldState::Checking(None)) { return; }
        if self.eval_blocked() || !held.base.as_ref().is_some_and(|b| b.matches(self, self.docs.as_ref().unwrap().revision())) {
            let p = &mut self.proposals[i];
            if p.base.take().is_some() { p.checking(None); }
            return;
        }
        let EvalOut::Verified(result) = done.out else { unreachable!("a planned document set is verified") };
        let (verified, inverse) = match result {
            Ok(ok) => ok,
            Err(d) => {
                self.unusable_proposal(i, ProposalState::Refused(d.message.clone()), d, rejected, outcomes);
                return;
            }
        };
        let (candidate, _) = self.verified_candidate(verified, inverse, job.before, job.libs, done.benches, &done.same_boards, done.placed, true);
        let (checks, requirements) = self.candidate_summaries(&candidate);
        let p = &mut self.proposals[i];
        p.show_summaries(checks.clone(), requirements);
        let view = Arc::make_mut(&mut p.view);
        view.design = Some(candidate.design.clone());
        view.boards = Arc::new(candidate.results.boards.iter().map(proposal_board).collect());
        view.sheets = Arc::new(candidate.sheets.iter().map(|s| (s.unit, s.index.clone())).collect());
        view.created = Arc::new(candidate.created.clone());
        view.state = ProposalState::Ready;
        p.state = HeldState::Ready(Box::new(candidate));
        if let Some(ticket) = p.ticket.take() {
            outcomes.push((ticket, Outcome::Proposed { id: job.id, shown: view.shown, checks: Some(checks), requirements: view.requirements.clone() }));
        }
        for (shown, ticket) in p.presses.drain(..).rev() {
            queue.push_front(Queued { cmd: Cmd::Decide { id: job.id, shown, accept: true, ticket }, sample: None, seq: None });
        }
    }

    fn candidate_summaries(&self, candidate: &Candidate) -> (Arc<ProposalChecks>, Option<Arc<ReqSummary>>) {
        let (findings, awaiting) = self.shown_findings(&candidate.design, Some(&candidate.carry.sources), &candidate.results, &[], candidate.profiles.as_ref(), true);
        let requirements = candidate.requirements(&findings, &awaiting);
        let pending = candidate.results.boards.iter().any(|b| b.pending.whole || !b.pending.zones.is_empty());
        let mut counts = BTreeMap::new();
        for v in findings.into_iter().filter(|v| awaiting.binary_search(&v.id).is_err()) {
            *counts.entry((v.kind, v.severity)).or_default() += 1;
        }
        (Arc::new(ProposalChecks { counts, pending }), requirements)
    }

    /// A fill or whole-board check that merged on a shared board also settles that board
    /// in the held results. Case findings can change even when no document moved.
    pub(super) fn merged_proposal_checks(&mut self, cases_changed: bool) -> bool {
        let mut changed = false;
        for i in 0..self.proposals.len() {
            let p = &mut self.proposals[i];
            let HeldState::Ready(candidate) = &mut p.state else { continue };
            let mut merged = false;
            for (index, b) in candidate.results.boards.iter_mut().enumerate() {
                if !b.pending.whole && b.pending.zones.is_empty() { continue; }
                let board = |d: &Design| d.unit(b.unit).and_then(|u| u.board.as_ref()).map(Arc::as_ptr);
                if board(&candidate.design) != board(&self.design) { continue; }
                let Some(current) = self.results.boards.iter().find(|r| r.unit == b.unit) else { continue };
                let fills_changed = !Arc::ptr_eq(&b.fills, &current.fills);
                if Arc::ptr_eq(&b.rules, &current.rules) && (fills_changed || b.pending != current.pending) {
                    if fills_changed || !b.pending.zones.iter().map(|p| p.0).eq(current.pending.zones.iter().map(|p| p.0)) {
                        let view = Arc::make_mut(&mut p.view);
                        Arc::make_mut(&mut view.boards)[index] = proposal_board(current);
                        changed = true;
                    }
                    *b = current.clone();
                    merged = true;
                }
            }
            if merged || cases_changed {
                let HeldState::Ready(candidate) = &self.proposals[i].state else { unreachable!() };
                let (checks, requirements) = self.candidate_summaries(candidate);
                changed |= self.proposals[i].show_summaries(checks, requirements);
            }
        }
        changed
    }

    fn unusable_proposal(&mut self, i: usize, state: ProposalState, diag: Diag, rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) {
        let p = &mut self.proposals[i];
        let label = p.view.label.to_string();
        let ticket = p.ticket.take();
        let presses = std::mem::take(&mut p.presses);
        let view = p.clear_design();
        view.state = state;
        view.checks = None;
        view.requirements = None;
        p.state = HeldState::Unusable;
        if ticket.is_some() { self.proposal_refusal(label.clone(), diag.clone(), ticket, rejected, outcomes); }
        for (_, ticket) in presses { self.proposal_refusal(label.clone(), diag.clone(), ticket, rejected, outcomes); }
    }

    /// Only steps that move an input or finish a job visit the pending proposals. Re-plan on
    /// the newest settled base. One running job can be overtaken without starting another.
    pub(super) fn refresh_proposals(&mut self, rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) -> bool {
        let Some(revision) = self.docs.as_ref().map(Docs::revision) else { return false };
        let blocked = !self.held.is_empty() || self.eval_blocked();
        let mut changed = false;
        for i in (0..self.proposals.len()).rev() {
            let p = &self.proposals[i];
            if matches!(p.state, HeldState::Unusable) || p.base.as_ref().is_some_and(|b| b.matches(self, revision)) {
                continue;
            }
            let docs = self.docs.as_ref().unwrap();
            let result = if let Some((doc, _)) = p.touched.iter().find(|(id, rev)|
                !docs.view().texts.iter().any(|t| t.doc == *id && t.rev == *rev)) {
                Err(Diag::error("a proposed document changed since it was read; regenerate the proposal",
                    vec![Sel::Source { doc: *doc, start: 0, end: 0 }]))
            } else if blocked {
                // The previous lowering still owns the circuit-to-board cascade. Re-plan only
                // once that lowering follows the latest documents. A peer still needs its press.
                let p = &mut self.proposals[i];
                if p.base.take().is_none() { continue; } // Already waiting on this evaluation.
                if !matches!(p.state, HeldState::Unchecked(_)) {
                    p.checking(None);
                }
                changed = true;
                continue;
            } else {
                plan(Some(docs), &p.set, &p.view.by).and_then(|(plan, changes)| {
                    if changes == *p.view.docs { Ok(plan) }
                    else { Err(Diag::error("the documents this proposal would change moved; regenerate the proposal", vec![])) }
                })
            };
            match result {
                Err(d) => self.unusable_proposal(i, ProposalState::Stale(d.message.clone()), d, rejected, outcomes),
                Ok(plan) => {
                    let base = BaseStamp::new(self, docs);
                    let p = &mut self.proposals[i];
                    p.base = Some(base);
                    if matches!(p.state, HeldState::Unchecked(_)) {
                        p.state = HeldState::Unchecked(plan);
                    } else {
                        p.checking(Some(plan));
                    }
                }
            }
            changed = true;
        }
        changed
    }

    pub(super) fn decide(&mut self, id: ProposalId, shown: u64, accept: bool, ticket: Option<Ticket>,
        rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) -> bool {
        let Some(i) = self.proposals.iter().position(|p| p.view.id == id) else {
            self.proposal_refusal("Proposal".into(), Diag::error("this proposal has ended", vec![]), ticket, rejected, outcomes);
            return false;
        };
        let held = &self.proposals[i];
        let reason = if held.view.shown != shown {
            Some(format!("the proposal changed ({}); review revision {}", held.summary_change, held.view.shown))
        } else if !accept {
            self.end_proposal(i, rejected, outcomes);
            if let Some(ticket) = ticket { outcomes.push((ticket, Outcome::Declined { id })); }
            return false;
        } else {
            match &held.view.state {
                ProposalState::Refused(_) => Some("this proposal is not usable".into()),
                ProposalState::Stale(reason) => Some(reason.clone()),
                ProposalState::Unchecked => Some("check it first".into()),
                ProposalState::Checking => {
                    self.proposals[i].presses.push((shown, ticket));
                    return false;
                }
                ProposalState::Ready => None,
            }
        };
        if let Some(reason) = reason {
            let label = held.view.label.to_string();
            self.proposal_refusal(label, Diag::error(reason, vec![]), ticket, rejected, outcomes);
            return false;
        }
        let docs = self.docs.as_ref().unwrap();
        let HeldState::Ready(candidate) = &held.state else { unreachable!("checked the visible state") };
        let carry = &candidate.carry;
        if !held.base.as_ref().is_some_and(|b| b.matches(self, docs.revision())) || !docs.same_texts(&carry.sources_before)
            || !Arc::ptr_eq(docs.libs(), &carry.libs) || !Arc::ptr_eq(&self.factories, &carry.factories) {
            self.proposals[i].presses.push((shown, ticket));
            return false;
        }
        let held = self.proposals.remove(i);
        let HeldState::Ready(candidate) = held.state else { unreachable!("checked the carry stamp") };
        let Candidate { design, results, sheets, created, carry, profiles } = *candidate;
        let by = match &held.view.by { ProposalAuthor::Local(by) => *by, ProposalAuthor::Peer { .. } => By::Person };
        let circuit = if self.design.units.len() != design.units.len() || design.units.iter().any(|u|
            self.design.unit(u.id).is_none_or(|old| !Arc::ptr_eq(&u.circuit, &old.circuit))) {
            CircuitChange::Structure
        } else { CircuitChange::None };
        self.design = design;
        self.results = results;
        self.sheets = sheets;
        let docs = self.docs.as_mut().unwrap();
        for unit in carry.replaced { docs.bump_epoch(unit); }
        docs.adopt_evaluated(Some(carry.sources), carry.evaluated, carry.benches);
        if let Some(resolved) = profiles {
            self.profiles = resolved.profiles;
            self.factories = resolved.factories;
            self.profile_diags = resolved.diags;
            self.profile_inputs = docs.view().texts.iter().filter(|t| DocKind::of_name(&t.name) == Some(DocKind::Profile))
                .map(|t| (t.doc, t.rev)).collect();
            self.profile_view = Arc::downgrade(docs.view());
        }
        let seq = self.steps.fetch_add(1, SeqCst) + 1;
        if let (Some(ticket), Some(journal)) = (ticket, &self.journal) {
            let author = match &held.view.by {
                ProposalAuthor::Local(By::Person) => "person",
                ProposalAuthor::Local(By::Ai { .. }) => "ai",
                ProposalAuthor::Local(By::Agent { .. }) => "agent",
                ProposalAuthor::Local(By::Disk) => "disk",
                ProposalAuthor::Peer { .. } => "peer",
            };
            let mut args = vec![("proposal".into(), id.0.to_string()), ("slot".into(), held.view.slot.to_string()),
                ("label".into(), held.view.label.to_string()), ("author".into(), author.into())];
            if let ProposalAuthor::Peer { label } = &held.view.by {
                args.push(("peer".into(), label.to_string()));
            }
            if let Some(reply) = journal.decision(ticket, "elec.step.accept", &args) {
                self.gesture_replies.insert(ticket, reply);
            }
        }
        self.log.push(OpEntry { seq, by, label: held.view.label.to_string(), ops: vec![], inverse: vec![],
            text: carry.edit, text_inverse: carry.inverse, epochs: vec![], kind: EntryKind::Eval, group: None });
        self.logged.fetch_max(seq, SeqCst);
        self.typing = None;
        self.last_refusal = None;
        self.committed(None, ticket, created, carry.sites, Some(circuit), outcomes);
        true
    }
}
