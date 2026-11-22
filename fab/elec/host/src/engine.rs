//! The strand's state and its apply, held-results, and completed-fill passes.

use crate::docs::Docs;
use crate::eval::place_ops;
use crate::{circuits_named, CircuitChange, Cmd, CASE_KEEPOUT, DocSet, EvalDone, EvalJob, EvalOut, Inbox, Outcome, Plan, Resolver, RouteStart, Sample, TaskResult, Ticket};
use fab_elec_board::{zone, Board, BoardDelta, BoardOp, Factories, ItemKind, Origin, RuleMatrix, ZoneFill};
use fab_elec_circuit::{CheckKind, Circuit, CircuitOp, Violation};
use fab_elec_core::{BoardId, Canon, CompRef, ContentHash, DocId, FeatureRef, ItemId, Seg, Sel, Severity};
use fab_elec_design::engines::{self, BoardResults, DesignResults, FillTask};
use fab_elec_design::ops::{DesignApplied, DesignOpError};
use fab_elec_design::{AssetBytes, AssetStep, By, Design, DesignOp, EntryKind, FileOp, OpEntry, OpLog, Place, Resolved, TextEdit, TextStep};
use fab_elec_lang::{BenchView, DocKind, Evaluated, Libs, SourceMap, SourceRange, Sources, UnitState};
use fab_elec_explore::rows::SheetRows;
use fab_elec_route::{Base, JobId, Reason, RouteJob};
use fab_elec_view::snapshot::{next_tag, Committed};
use fab_elec_view::{DesignSnapshot, Refusal, SheetIndex, StageFlags, StateOf, Tagged};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::{AcqRel, SeqCst}};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;

pub struct Engine {
    design: Arc<Design>,
    results: DesignResults,
    /// Per unit the router holds: the base last sent to it.
    routed: BTreeMap<BoardId, Base>,
    /// Monotonic route job identities for this engine.
    jobs: u32,
    /// The newest route waiting for its unit's committed fills to become exact.
    waiting_route: Option<(Ticket, By, RouteJob)>,
    factories: Arc<Factories>,
    preview: Option<Live>,
    /// One circuit/bench preview runs independently of committed evaluation.
    preview_job: Option<PreviewJob>,
    /// Only the newest sample waits behind the running preview job.
    preview_next: Option<Sample>,
    /// Invoke callers wait until their evaluated sample is shown or refused; route callers
    /// keep their job tag until a commit or refusal answers them.
    preview_tickets: BTreeMap<u64, (Ticket, String)>,
    /// A later drop can report the exact refusal of this sample.
    refused_preview: Option<(u64, Arc<Refusal>)>,
    /// Shared with the driver's pane samples, so tool and pane previews never reuse a seq.
    preview_seq: Arc<AtomicU64>,
    resolvers: Vec<(&'static str, Resolver)>,
    /// Set on a refused commit, preview sample, undo or redo; cleared by the next accepted edit.
    /// Cost: one Arc allocation per refusal, O(1) to share in snapshots, nothing per frame.
    last_refusal: Option<Arc<Refusal>>,
    /// The newest logged edit; undo and redo do not replace this notification.
    last_commit: Option<Arc<Committed>>,
    /// Refusals so far: each refusal's `seq`. The project step counter stays for undo steps.
    refusals: u64,
    /// Stable rows and published indices, in committed unit order.
    sheets: Vec<Sheet>,
    /// Committed case findings per unit, sorted by content id.
    case: BTreeMap<BoardId, (u64, Arc<Vec<Violation>>)>,
    /// Globally sorted and deduplicated on updates, so snapshots only need a linear merge.
    case_sorted: Vec<Violation>,
    /// Session notes from committed edits, kept while their corners remain on the board.
    notes: BTreeMap<BoardId, Vec<Violation>>,
    /// The committed notes in finding id order for snapshots.
    notes_sorted: Vec<Violation>,
    /// Notes from the exact sample being committed, including while its commit waits.
    commit_notes: HashMap<u64, (BoardId, Vec<Violation>)>,
    log: OpLog,
    /// Numbers come from this shared counter; a reservation takes one early.
    steps: Arc<AtomicU64>,
    /// The newest entry logged by either history.
    logged: Arc<AtomicU64>,
    /// Redo stays valid while no entry was logged since the undo.
    redo_mark: u64,
    /// The fill job running per zone: the generation and rules it fills with.
    in_flight: HashMap<(BoardId, ItemId), (u64, Arc<RuleMatrix>)>,
    head: String,
    /// Weak avoids forcing `Arc::make_mut` to clone items; with only weak refs it moves the board, invalidating the cached pointer.
    board_hashes: std::cell::RefCell<Vec<(BoardId, Weak<Board>, ContentHash)>>,
    /// The documents the design is written in; None for a design given without them.
    docs: Option<Docs>,
    /// Project-relative asset names and their process MeshCache keys, owned by the plate.
    mesh_keys: BTreeMap<String, String>,
    /// The part library's models by project name, and the MeshCache key each body resolves to
    /// when the plate holds no file of that name (the plate's own file comes first).
    library_keys: BTreeMap<String, String>,
    /// New library names waiting to be included in a plain evaluation.
    library_changed: bool,
    /// The bytes of the available assets this history's steps added (an import, its redo), for
    /// the plate's write: the same allocations the steps and the plate hold, dropped here when a
    /// step makes the asset unavailable.
    own_assets: BTreeMap<String, AssetBytes>,
    /// Commands waiting for evaluation, in their original log order.
    held: VecDeque<Queued>,
    /// The single evaluation currently running against a document snapshot.
    eval: Option<InFlight>,
    /// Monotonic identities distinguish current and late pool results.
    evals: u64,
    /// The editor gesture whose last entry can still absorb typed text.
    typing: Option<Typing>,
}

/// A running job's base texts and, for verification, the commit to retry or log.
struct InFlight {
    /// The one result identity the engine will accept.
    id: u64,
    /// Base text identities detect edits made while the pool was working.
    sources: Sources,
    /// A library refresh adopts these inputs if the base still matches.
    library_sources: Option<Sources>,
    /// A verified commit's eventual log payload, also used for retries.
    verify: Option<Verify>,
}

/// Cost per started sample: O(documents + evaluation metadata + ops) snapshot clones.
struct PreviewJob {
    id: u64,
    sample: Sample,
    sources: Sources,
    libs: Arc<Libs>,
    asked: Instant,
}

/// Verified text and evaluation retained for committing a shown preview without another splice.
/// Carry cost: O(documents + units) for epoch bumps and document adoption, with no evaluation.
struct Carry {
    sources_before: Sources,
    libs: Arc<Libs>,
    factories: Arc<Factories>,
    sources: Sources,
    edit: TextStep,
    inverse: TextStep,
    evaluated: Evaluated,
    benches: Arc<Vec<BenchView>>,
    replaced: Vec<BoardId>,
    sites: Vec<Sel>,
}

/// Preserve a committed sample's identity while its command waits behind other edits.
struct Queued {
    cmd: Cmd,
    sample: Option<u64>,
    /// The reserved number of a commit that already had one.
    seq: Option<u64>,
}

/// A verified commit's authorship and caller survive pool adoption and retries.
struct Verify {
    seq: u64,
    ops: Vec<DesignOp>,
    place: Vec<Place>,
    /// The circuits the ops' ids name, kept for a retry over moved texts.
    circuits: Vec<(BoardId, Weak<Circuit>)>,
    label: String,
    by: By,
    ticket: Option<Ticket>,
    sample: Option<u64>,
    /// A checked text change (`Cmd::CheckedText`): its document, revision and edits; `ops` is empty.
    text: Option<(DocId, u64, Vec<TextEdit>)>,
}

/// The original text and history entry of a mergeable editor gesture.
struct Typing {
    /// The editor's gesture identity, scoped to its document.
    group: u64,
    /// The one document this gesture edits.
    doc: DocId,
    /// The original text for a trimmed forward and inverse step.
    before: Arc<str>,
    /// The history entry that must still be at the applied head.
    seq: u64,
}

/// Why the engine refused an edit, and where the source says it.
struct Refused {
    text: String,
    site: Option<Sel>,
}

struct Live {
    sample: Sample,
    /// Selections added by the sample; O(created selections) to clone into a snapshot.
    created: Vec<Sel>,
    /// The committed design the sample applies to.
    base: Arc<Design>,
    /// The sample's board deltas against `base`.
    deltas: Vec<(BoardId, BoardDelta)>,
    design: Arc<Design>,
    results: DesignResults,
    /// Where each board continued from that did not start from the committed results: an
    /// earlier sample's board results over `base` and that sample's delta against `base`.
    from: BTreeMap<BoardId, (BoardResults, BoardDelta)>,
    /// The boards a fill of this sample merged into.
    merged: BTreeSet<BoardId>,
    /// Synced once per evaluated preview; snapshots only share the indices.
    sheets: Vec<Sheet>,
    carry: Option<Carry>,
}

#[derive(Clone)]
struct Sheet {
    unit: BoardId,
    rows: Arc<SheetRows>,
    circuit: Arc<Circuit>,
    index: Arc<SheetIndex>,
}

/// A pure pool task: fill one zone over an immutable board snapshot.
/// Refills only the tiles the debt reaches; equal to a full fill.
#[derive(Clone)]
pub struct FillJob {
    pub task: FillTask,
    pub board: Arc<Board>,
    pub rules: Arc<RuleMatrix>,
    /// The results' piece index of `board`; held fills in it are not read.
    pub index: Arc<fab_elec_board::PieceIndex>,
    /// The zone's held fill in the results the task was made from.
    pub prev: ZoneFill,
    /// The unit's debt: every change since the results were last exact (covers every change since `prev`).
    pub delta: BoardDelta,
    /// The live preview's sample seq the fill was made for; None: the committed results.
    pub preview: Option<u64>,
}

impl FillJob {
    /// The job's tile plan: compute each tile with `tile` (any thread, any order), then `finish`.
    pub fn plan(&self) -> zone::FillPlan {
        zone::FillPlan::update(&self.board, &self.rules, &self.prev, &self.delta, &self.index)
    }

    /// Tile `i` of `plan`, over this job's board, rules and index.
    pub fn tile(&self, plan: &zone::FillPlan, i: usize) -> Arc<zone::FillTile> {
        plan.tile(&self.board, &self.rules, &self.index, i)
    }

    /// The task result from the plan and its computed tiles.
    pub fn finish(&self, plan: &zone::FillPlan, tiles: Vec<Arc<zone::FillTile>>) -> TaskResult {
        TaskResult::Fill {
            unit: self.task.unit,
            zone: self.task.zone,
            generation: self.task.generation,
            rules: self.rules.clone(),
            fill: plan.finish(tiles),
            preview: self.preview,
        }
    }

    /// Compute the job's plan in tile order and assemble its task result.
    pub fn run(&self) -> TaskResult {
        let plan = self.plan();
        let tiles = (0..plan.len()).map(|i| self.tile(&plan, i)).collect();
        self.finish(&plan, tiles)
    }

    /// Plan the job here, then compute its tiles as separate tasks handed to `spawn` (the last
    /// tile runs inline). The task that stores the last tile assembles the result and passes it to
    /// `done`; nothing waits on another task. Once `cancelled()` holds, tiles not yet started return
    /// at once and `done` is never called; the same holds when `spawn` refuses a task (returns
    /// false). An empty plan finishes inline. Wall time is planning plus the slowest tile.
    pub fn run_tiled<C, S, D>(self, cancelled: C, mut spawn: S, done: D)
    where
        C: Fn() -> bool + Send + Sync + 'static,
        S: FnMut(Box<dyn FnOnce() + Send>) -> bool,
        D: FnOnce(TaskResult) + Send + 'static,
    {
        let plan = self.plan();
        let Some(last) = plan.len().checked_sub(1) else {
            done(self.finish(&plan, Vec::new()));
            return;
        };
        let tiled = Arc::new(Tiled {
            slots: (0..plan.len()).map(|_| OnceLock::new()).collect(),
            left: AtomicUsize::new(plan.len()),
            job: self,
            plan,
            cancelled,
            done: Mutex::new(Some(done)),
        });
        for i in 0..last {
            let tiled = Arc::clone(&tiled);
            if !spawn(Box::new(move || tiled.tile(i))) {
                return;
            }
        }
        tiled.tile(last);
    }
}

/// One fill job's tiles in flight; the task that stores the last tile assembles the result.
struct Tiled<C, D> {
    job: FillJob,
    plan: zone::FillPlan,
    slots: Vec<OnceLock<Arc<zone::FillTile>>>,
    /// Tiles not yet stored.
    left: AtomicUsize,
    cancelled: C,
    /// Taken once, by the task that stores the last tile.
    done: Mutex<Option<D>>,
}

impl<C: Fn() -> bool, D: FnOnce(TaskResult)> Tiled<C, D> {
    fn tile(&self, i: usize) {
        if (self.cancelled)() {
            return;
        }
        // A slot is written once: each index is handed to exactly one task.
        let _ = self.slots[i].set(self.job.tile(&self.plan, i));
        if self.left.fetch_sub(1, AcqRel) != 1 {
            return;
        }
        let tiles = self.slots.iter()
            .map(|slot| Arc::clone(slot.get().expect("every tile is stored before the count reaches zero")))
            .collect();
        let result = self.job.finish(&self.plan, tiles);
        let done = self.done.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        if let Some(done) = done {
            done(result);
        }
    }
}

pub struct Step {
    /// At most one committed evaluation and one preview evaluation to start on the pool.
    pub evals: Vec<EvalJob>,
    /// The engine holds commands or runs an evaluation, so undo tops may still move; the host counts as busy.
    pub waiting: bool,
    /// Fill jobs to start on the pool (latest wins per zone).
    pub fills: Vec<FillJob>,
    /// Route jobs sent once the unit's committed fills are exact; a newer route replaces a parked one.
    pub routes: Vec<RouteStart>,
    /// Committed bases for the router: each unit `Cmd::RouteBase` asked for in this step, and each
    /// unit it holds whose committed board, rules, connectivity or fills moved since its last base
    /// (O(routed units) pointer compares per step).
    pub bases: Vec<Base>,
    /// Commands not applied, with why, in order: (label, reason).
    pub rejected: Vec<(String, String)>,
    /// Answers to ticketed commands, emitted when an edit is logged or refused.
    pub outcomes: Vec<(Ticket, Outcome)>,
    /// The newest undoable and redoable step numbers for the shared undo order.
    pub undo: Option<u64>,
    pub redo: Option<u64>,
    /// The project's files, when the step changed any text or the asset list (a commit, undo or
    /// redo that applied): the plate's copy to store. None when neither changed. Cost: O(documents
    /// + assets) Arc clones per such step.
    pub docs: Option<Written>,
}

/// What the plate stores after a step: the documents by file name and the asset names, with the
/// bytes of those this history's steps added (the plate holds the others' bytes). The bytes are
/// shared Arcs, never copies.
#[derive(Clone, Debug)]
pub struct Written {
    pub docs: Arc<BTreeMap<String, Arc<str>>>,
    pub assets: Arc<BTreeMap<String, Option<Arc<[u8]>>>>,
}

/// One warning per track in the sample's router notes, located at its first reported corner.
fn sharp_notes(sample: &Sample) -> Option<(BoardId, Vec<Violation>)> {
    let mut tracks = BTreeMap::new();
    for (_, reason) in &sample.notes {
        if let Reason::SharpCorner { track: Sel::Item(unit, id), at, .. } = reason {
            let (_, count) = tracks.entry((*unit, *id)).or_insert((*at, 0usize));
            *count += 1;
        }
    }
    // A gesture's ops and all its track selections belong to the same board, even on a label drop.
    let unit = tracks.keys().next()?.0;
    let mut notes: Vec<_> = tracks.into_iter().map(|((unit, id), (at, count))| {
        let mut message = String::from("turns sharper than a right angle here; no room to chamfer it");
        if count > 1 {
            message.push_str(&format!(" ({count} sharp corners on this track)"));
        }
        Violation::new(CheckKind::SharpCorner, Severity::Warning, Some(unit), vec![Sel::Item(unit, id)], "sharp corner", message)
            .at(at).hint("drag the part or the track to give the corner room")
    }).collect();
    notes.sort_unstable_by_key(|v| v.id);
    Some((unit, notes))
}

impl Engine {
    /// `steps` supplies numbers, including early reservations; `logged` tracks logged entries.
    /// Load is the one time the strand computes full results, including fills inline.
    pub fn new(design: Design, factories: Arc<Factories>, steps: Arc<AtomicU64>, logged: Arc<AtomicU64>, head: String) -> Engine {
        let results = engines::full_with(&design, &factories);
        let mut engine = Engine {
            design: Arc::new(design), results, routed: BTreeMap::new(), factories, preview: None, log: OpLog::default(),
            jobs: 0, waiting_route: None,
            preview_job: None, preview_next: None, preview_tickets: BTreeMap::new(), refused_preview: None,
            last_refusal: None, last_commit: None, refusals: 0,
            preview_seq: Arc::new(AtomicU64::new(0)), resolvers: Vec::new(),
            sheets: Vec::new(),
            case: BTreeMap::new(), case_sorted: Vec::new(), steps, logged, redo_mark: 0, in_flight: HashMap::new(), head,
            notes: BTreeMap::new(), notes_sorted: Vec::new(), commit_notes: HashMap::new(),
            mesh_keys: BTreeMap::new(), library_keys: BTreeMap::new(), library_changed: false, own_assets: BTreeMap::new(),
            board_hashes: std::cell::RefCell::new(Vec::new()),
            docs: None, held: VecDeque::new(), eval: None, evals: 0, typing: None,
        };
        engine.sync_sheets();
        engine
    }

    /// Put the documents `design` was evaluated from in front of the engine ([`Docs::load`]): board
    /// commits then splice into them, undo and redo apply their text steps, and snapshots carry
    /// the source map and the documents.
    pub fn with_docs(mut self, docs: Docs) -> Engine {
        self.docs = Some(docs);
        self
    }

    /// Keep the plate's registered mesh keys for every later circuit adoption.
    pub fn with_mesh_keys(mut self, keys: BTreeMap<String, String>) -> Engine {
        self.mesh_keys = keys;
        self
    }

    /// Keep the part library's models (`Sources::set_library`) for every later circuit adoption:
    /// a body the plate has no file for resolves to the library's.
    pub fn with_library_keys(mut self, keys: BTreeMap<String, String>) -> Engine {
        self.library_keys = keys;
        self
    }

    /// Fab's route job and the card, sheet, board, bench and system edits available through tool
    /// calls.
    pub fn with_app_tools(self) -> Engine {
        self.with_resolver("elec.route", crate::route_plan)
            .with_resolver("elec.value.set", crate::edit_actions::value_set)
            .with_resolver("elec.part.place", crate::edit_actions::part_place)
            .with_resolver("elec.part.body.set", crate::model_actions::body_set)
            .with_resolver("elec.factory.set", crate::edit_actions::factory_set)
            .with_resolver("elec.part.add", crate::sheet_actions::part_add)
            .with_resolver("elec.part.remove", crate::sheet_actions::part_remove)
            .with_resolver("elec.part.rename", crate::sheet_actions::part_rename)
            .with_resolver("elec.part.field", crate::sheet_actions::part_field)
            .with_resolver("elec.net.connect", crate::sheet_actions::net_connect)
            .with_resolver("elec.net.disconnect", crate::sheet_actions::net_disconnect)
            .with_resolver("elec.net.rename", crate::sheet_actions::net_rename)
            .with_resolver("elec.pin.nc", crate::sheet_actions::pin_nc)
            .with_resolver("elec.bench.supply", crate::bench_actions::bench_supply)
            .with_resolver("elec.bench.source", crate::bench_actions::bench_source)
            .with_resolver("elec.bench.probe", crate::bench_actions::bench_probe)
            .with_resolver("elec.link", crate::edit_actions::sys_edit)
            .with_resolver("elec.unit", crate::edit_actions::sys_edit)
    }

    /// Register or replace a prefix; O(prefixes) per call, O(registered prefix bytes) per dispatch.
    pub fn with_resolver(mut self, prefix: &'static str, f: Resolver) -> Engine {
        if let Some((_, resolver)) = self.resolvers.iter_mut().find(|(key, _)| *key == prefix) {
            *resolver = f;
        } else {
            self.resolvers.push((prefix, f));
        }
        self
    }

    /// Share the pane allocator before loading the engine; O(1) per load.
    pub(crate) fn with_preview_seq(mut self, seq: Arc<AtomicU64>) -> Engine {
        seq.fetch_max(self.preview_seq.load(SeqCst), SeqCst);
        self.preview_seq = seq;
        self
    }

    pub fn docs(&self) -> Option<&Docs> {
        self.docs.as_ref()
    }

    /// The current snapshot; immediately after load its results are full and exact.
    pub fn snapshot(&self) -> Arc<DesignSnapshot> {
        self.make_snapshot(StageFlags::NONE)
    }

    pub fn design(&self) -> &Arc<Design> {
        &self.design
    }

    pub fn results(&self) -> &DesignResults {
        &self.results
    }

    pub fn log(&self) -> &OpLog {
        &self.log
    }

    /// One strand job over a taken inbox; publish each snapshot as soon as it exists.
    pub fn step(&mut self, inbox: Inbox, publish: &mut dyn FnMut(Arc<DesignSnapshot>)) -> Step {
        let Inbox { cmds, mut preview, results, dead, dead_answers, dead_samples, mut committing } = inbox;
        if let Some(sample) = &preview {
            self.preview_seq.fetch_max(sample.seq, SeqCst);
        }
        let mut preview_ticket: Option<(u64, Ticket, String)> = None;
        let revision = self.docs.as_ref().map(Docs::revision);
        let preview_before = self.preview.as_ref().map(|p| p.sample.seq);
        let preview_eval_before = self.preview_job.is_some();
        let eval_before = self.eval_blocked();
        let refusal_before = self.last_refusal.clone();
        let commit_before = self.last_commit.clone();
        let mut rejected = Vec::new();
        let mut asked: Vec<BoardId> = Vec::new();
        let mut routes = Vec::new();
        let mut outcomes = Vec::new();
        let mut deltas = BTreeMap::new();
        let mut changed = false;
        let mut evals = Vec::new();
        let mut queue = std::mem::take(&mut self.held);
        queue.extend(cmds.into_iter().map(|(_, cmd)| Queued { cmd, sample: None, seq: None }));
        let mut fill_results = Vec::new();
        let mut evaluated = false;
        let mut preview_changed = false;
        // Adoption precedes commands and fills, so held commands can run in this step.
        for result in results {
            match result {
                TaskResult::Eval(done) => {
                    if self.preview_job.as_ref().is_some_and(|job| job.id == done.id && done.preview == Some(job.sample.seq)) {
                        preview_changed |= self.finish_preview(*done, &mut evals, &mut rejected, &mut outcomes);
                    } else if self.eval.as_ref().is_some_and(|job| job.id == done.id) {
                        evaluated = true;
                        changed |= self.finish_eval(*done, &mut queue, &mut deltas, &mut rejected, &mut outcomes);
                    }
                }
                fill => fill_results.push(fill),
            }
        }
        let mut refusal_changed = self.last_refusal != refusal_before;
        // The live preview's fills merge first, so a sample or a commit in this inbox continues
        // from them. A live preview issues its fills once, at its board generation; a fill of an
        // older build of the same sample (rebuilt over a new committed design) is not exact for it.
        let mut merged = false;
        let mut committed = Vec::with_capacity(fill_results.len());
        for result in fill_results {
            let TaskResult::Fill { unit, zone, generation, rules, fill, preview } = result else { unreachable!("evaluation results were handled first") };
            let Some(seq) = preview else {
                committed.push((unit, zone, generation, rules, fill));
                continue;
            };
            let Some(live) = self.preview.as_mut().filter(|live| live.sample.seq == seq) else { continue };
            if live.results.boards.iter().any(|b| b.unit == unit && b.generation == generation) {
                if let Some(results) = engines::merge_fill(&live.results, &live.design, unit, zone, generation, &rules, fill) {
                    live.results = results;
                    live.merged.insert(unit);
                    merged = true;
                }
            }
        }
        let mut case_changed = false;
        // A commit took its live preview's results over (and so changed no board results).
        let mut carried = false;
        while let Some(Queued { cmd, sample: tag, seq }) = queue.pop_front() {
            if matches!(&cmd, Cmd::Undo | Cmd::Redo) {
                self.typing = None;
            }
            let waits = match &cmd {
                Cmd::Undo | Cmd::Redo => !self.held.is_empty() || self.eval_blocked(),
                Cmd::Commit { ops, .. } => !self.held.is_empty() || (self.needs_verify(ops) && self.eval_blocked()),
                Cmd::Text { .. } => !self.held.is_empty(),
                Cmd::CheckedText { .. } => !self.held.is_empty() || self.eval_blocked(),
                Cmd::Docs { set, .. } => !self.held.is_empty() && set.iter().any(|change| !matches!(change, DocSet::Asset { .. })),
                // A tool resolves on the design its predecessors leave: its plan names parts by
                // the circuit it reads, so it waits behind them like an undo (a burst of circuit
                // edits would otherwise stamp each on a circuit the one before replaces).
                Cmd::Invoke { .. } => !self.held.is_empty() || self.eval_blocked(),
                Cmd::RouteBase { .. } | Cmd::CommitPreview { .. } | Cmd::CancelPreview { .. } | Cmd::RefusePreview { .. }
                | Cmd::SetFactories(_) | Cmd::CaseFindings { .. } | Cmd::CaseKeepouts { .. } | Cmd::LibraryModels(_) | Cmd::LibraryDrop(_) => false,
            };
            if waits {
                self.held.push_back(Queued { cmd, sample: tag, seq });
                continue;
            }
            match cmd {
                Cmd::Commit { ops, place, label, circuits, expect, by, ticket } => {
                    let op = op_kind(&ops);
                    if let Err(refused) = self.check_circuits(&circuits) {
                        rejected.push(self.refuse(label, op, tag, refused));
                        self.refusal_outcome(ticket, &mut outcomes);
                        refusal_changed = true;
                        continue;
                    }
                    if self.needs_verify(&ops) {
                        match self.check_expect(expect).and_then(|()| self.check_verify(&ops)) {
                            Ok(()) => {
                                let seq = seq.unwrap_or_else(|| self.steps.fetch_add(1, SeqCst) + 1);
                                evals.push(self.start_eval(Some(Verify { seq, ops, place, circuits, label, by, ticket, sample: tag, text: None })));
                            }
                            Err(refused) => {
                                rejected.push(self.refuse(label, op, tag, refused));
                                self.refusal_outcome(ticket, &mut outcomes);
                                refusal_changed = true;
                            }
                        }
                        continue;
                    }
                    match self.board_ops(ops, &place).and_then(|ops| self.commit(ops, label.clone(), expect, by)) {
                        Ok(batch) => {
                            let (created, sites) = self.board_created();
                            self.committed(tag, ticket, created, sites, None, &mut outcomes);
                            refusal_changed |= self.last_refusal.take().is_some();
                            changed |= batch.changed;
                            carried &= !batch.changed;
                            fold(&mut deltas, batch.deltas);
                        }
                        Err(refused) => {
                            rejected.push(self.refuse(label, op, tag, refused));
                            self.refusal_outcome(ticket, &mut outcomes);
                            refusal_changed = true;
                        }
                    }
                }
                Cmd::Text { doc, base_rev, edits, group, label, tag, by, ticket } => {
                    match self.text(doc, base_rev, edits, group, label.clone(), by) {
                        Ok((sites, circuit)) => {
                            self.committed(None, ticket, vec![], sites, Some(circuit), &mut outcomes);
                            refusal_changed |= self.last_refusal.take().is_some();
                        }
                        Err(refused) => {
                            rejected.push(self.refuse(label, "text", tag, refused));
                            self.refusal_outcome(ticket, &mut outcomes);
                            refusal_changed = true;
                        }
                    }
                }
                // Verified on the pool like a circuit commit; the person's `Text` above never is.
                Cmd::CheckedText { doc, base_rev, edits, label, by, ticket } => {
                    let checked = self.docs.as_ref().map_or_else(
                        || Err(Refused { text: "the engine has no project documents".into(), site: None }),
                        |docs| docs.check_text(doc, base_rev, &edits).map_err(|diag| Refused { site: docs.site_of(&diag), text: diag.message }),
                    );
                    match checked {
                        Ok(()) => {
                            let seq = seq.unwrap_or_else(|| self.steps.fetch_add(1, SeqCst) + 1);
                            evals.push(self.start_eval(Some(Verify {
                                seq, ops: vec![], place: vec![], circuits: vec![], label, by, ticket: Some(ticket), sample: None,
                                text: Some((doc, base_rev, edits)),
                            })));
                        }
                        Err(refused) => {
                            rejected.push(self.refuse(label, "text", None, refused));
                            self.refusal_outcome(Some(ticket), &mut outcomes);
                            refusal_changed = true;
                        }
                    }
                }
                Cmd::Invoke { tool, args, preview: want_preview, by, ticket } => {
                    let resolver = self.resolvers.iter().filter(|(prefix, _)| {
                        tool.strip_prefix(*prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
                    }).max_by_key(|(prefix, _)| prefix.len()).map(|(_, f)| *f);
                    let resolved = match resolver {
                        Some(resolve) => resolve(&self.design, self.docs.as_ref(), &tool, &args).map_err(|diag| Refused {
                            site: self.docs.as_ref().and_then(|docs| docs.site_of(&diag)), text: diag.message,
                        }),
                        None => {
                            let prefix = tool.rsplit_once('.').map_or(tool.as_str(), |(prefix, _)| prefix);
                            Err(Refused { text: format!("no plan function for {prefix} yet"), site: None })
                        }
                    };
                    match resolved {
                        Ok(Plan::Edit(Resolved { ops, place, label })) if want_preview => {
                            if let Some((tag, old_ticket, label)) = preview_ticket.take() {
                                rejected.push(self.refuse(label, "invoke", Some(tag), Refused {
                                    text: "the preview was replaced before it became live; ask again".into(), site: None,
                                }));
                                self.refusal_outcome(Some(old_ticket), &mut outcomes);
                                refusal_changed = true;
                            }
                            let seq = self.preview_seq.fetch_add(1, SeqCst) + 1;
                            let tag = next_tag();
                            preview_ticket = Some((tag, ticket, label.clone()));
                            let circuits = circuits_named(&self.design, &ops);
                            preview = Some(Sample { seq, tag, ops, place, label, by, proposal: None, notes: vec![], circuits, expect: None });
                        }
                        Ok(Plan::Edit(Resolved { ops, place, label })) => queue.push_front(Queued {
                            cmd: Cmd::Commit { circuits: circuits_named(&self.design, &ops), ops, place, label, expect: None, by, ticket: Some(ticket) },
                            sample: None, seq: None,
                        }),
                        Ok(Plan::Route(mut job)) => {
                            self.jobs += 1;
                            job.job = JobId(self.jobs);
                            let tag = job.job.tag();
                            self.preview_tickets.insert(tag, (ticket, tool.clone()));
                            let unit = job.unit;
                            match self.design.unit(unit).filter(|found| found.board.is_some()) {
                                Some(_) => {
                                    if let Some((old_ticket, _, old)) = self.waiting_route.take() {
                                        let (_, label) = self.preview_tickets.remove(&old.job.tag())
                                            .expect("a parked route retains its caller");
                                        rejected.push(self.refuse(label, "invoke", Some(old.job.tag()), Refused {
                                            text: "replaced by a newer route".into(), site: None,
                                        }));
                                        self.refusal_outcome(Some(old_ticket), &mut outcomes);
                                        refusal_changed = true;
                                    }
                                    // Parked until the end of the step: later commands of this step
                                    // and pass A move the committed board and fills it starts on.
                                    self.waiting_route = Some((ticket, by, job));
                                }
                                None => {
                                    self.preview_tickets.remove(&tag);
                                    rejected.push(self.refuse(tool, "invoke", Some(tag), Refused {
                                        text: format!("unit {} has no board", unit.0), site: None,
                                    }));
                                    self.refusal_outcome(Some(ticket), &mut outcomes);
                                    refusal_changed = true;
                                }
                            }
                        }
                        Err(refused) => {
                            rejected.push(self.refuse(tool, "invoke", None, refused));
                            self.refusal_outcome(Some(ticket), &mut outcomes);
                            refusal_changed = true;
                        }
                    }
                }
                Cmd::RouteBase { unit } => {
                    asked.push(unit);
                }
                Cmd::LibraryModels(models) => {
                    for (name, key) in models {
                        if !self.mesh_keys.contains_key(&name) && !self.library_keys.contains_key(&name) {
                            self.library_keys.insert(name, key);
                            self.library_changed = true;
                        }
                    }
                }
                Cmd::LibraryDrop(prefix) => {
                    let before = self.library_keys.len();
                    self.library_keys.retain(|name, _| !name.starts_with(&prefix));
                    self.library_changed |= self.library_keys.len() != before;
                }
                Cmd::Docs { set, label, by, ticket } => {
                    let documents = set.iter().any(|change| !matches!(change, DocSet::Asset { .. }));
                    let mut keys = BTreeMap::new();
                    let mut reason = None;
                    if self.docs.is_none() {
                        reason = Some("the engine has no project documents");
                    } else {
                        for change in &set {
                            let DocSet::Asset { name, key } = change else { continue };
                            if self.mesh_keys.get(name).or_else(|| keys.get(name)).is_some_and(|old| old != key) {
                                reason = Some("a changed asset reloads the bed's documents");
                                break;
                            }
                            keys.insert(name.clone(), key.clone());
                        }
                    }
                    if let Some(reason) = reason {
                        rejected.push(self.refuse(label, "docs", None, Refused {
                            text: reason.into(), site: None,
                        }));
                        self.refusal_outcome(ticket, &mut outcomes);
                        refusal_changed = true;
                        continue;
                    }
                    let docs = self.docs.as_mut().expect("document changes and asset registration have documents");
                    if documents {
                        let (text, text_inverse) = match docs.doc_set(&set) {
                            Ok(steps) => steps,
                            Err(diag) => {
                                let refused = Refused { site: docs.site_of(&diag), text: diag.message };
                                rejected.push(self.refuse(label, "docs", None, refused));
                                self.refusal_outcome(ticket, &mut outcomes);
                                refusal_changed = true;
                                continue;
                            }
                        };
                        let seq = self.steps.fetch_add(1, SeqCst) + 1;
                        self.log.push(OpEntry {
                            seq, by, label, ops: vec![], inverse: vec![], text, text_inverse,
                            epochs: vec![], kind: EntryKind::Eval, group: None,
                        });
                        self.logged.fetch_max(seq, SeqCst);
                        self.typing = None;
                    }
                    for (name, key) in keys {
                        docs.add_asset(name.clone());
                        self.mesh_keys.insert(name, key);
                    }
                    if documents {
                        self.committed(None, ticket, vec![], vec![], None, &mut outcomes);
                        refusal_changed |= self.last_refusal.take().is_some();
                    }
                }
                Cmd::CommitPreview { seq, tag } => {
                    let stash_notes = |engine: &mut Self, sample: &Sample| {
                        if let Some(notes) = sharp_notes(sample) {
                            engine.commit_notes.insert(sample.tag, notes);
                        }
                    };
                    // Its sample was replaced in the slot after this commit was queued: commit it
                    // here, in this command's place, as a plain commit of the sample (verified
                    // when needed, never with the newer preview's results).
                    if let Some(i) = committing.iter().position(|sample| sample.seq == seq && sample.tag == tag) {
                        let sample = committing.swap_remove(i);
                        if let Some(refused) = self.base_moved(&sample) {
                            self.refuse_preview(sample, refused, &mut rejected, &mut outcomes);
                            refusal_changed = true;
                            continue;
                        }
                        self.preview = None;
                        self.clear_preview_jobs(seq);
                        let ticket = self.preview_tickets.remove(&tag).map(|(ticket, _)| ticket);
                        let reserved = self.needs_verify(&sample.ops).then(|| self.steps.fetch_add(1, SeqCst) + 1);
                        stash_notes(self, &sample);
                        queue.push_front(Queued {
                            cmd: Cmd::Commit {
                                ops: sample.ops, place: sample.place, label: sample.label, circuits: sample.circuits, expect: sample.expect,
                                by: sample.by, ticket,
                            },
                            sample: Some(sample.tag),
                            seq: reserved,
                        });
                        continue;
                    }
                    if self.answer_refused(seq, &mut rejected) {
                        self.commit_notes.remove(&tag);
                        refusal_changed = true;
                        continue;
                    }
                    if !self.held.is_empty() {
                        let sample = if preview.as_ref().is_some_and(|s| s.seq == seq) {
                            preview.take()
                        } else if self.preview.as_ref().is_some_and(|p| p.sample.seq == seq) {
                            self.preview.take().map(|p| p.sample)
                        } else if self.preview_job.as_ref().is_some_and(|job| job.sample.seq == seq) {
                            self.preview_job.take().map(|job| job.sample)
                        } else if self.preview_next.as_ref().is_some_and(|sample| sample.seq == seq) {
                            self.preview_next.take()
                        } else {
                            None
                        };
                        if let Some(sample) = sample {
                            self.preview = None;
                            self.clear_preview_jobs(seq);
                            if preview.as_ref().is_some_and(|s| s.seq <= seq) {
                                preview = None;
                            }
                            let reserved = self.needs_verify(&sample.ops).then(|| self.steps.fetch_add(1, SeqCst) + 1);
                            stash_notes(self, &sample);
                            self.held.push_back(Queued {
                                cmd: Cmd::Commit {
                                    ops: sample.ops, place: sample.place, label: sample.label, circuits: sample.circuits, expect: sample.expect,
                                    by: sample.by, ticket: None,
                                },
                                sample: Some(sample.tag),
                                seq: reserved,
                            });
                        } else {
                            rejected.push(self.ended(tag));
                            refusal_changed = true;
                        }
                        continue;
                    }
                    // While the committed results still describe the committed design, the
                    // sample's live preview over it is exactly what the commit makes (the same ops
                    // on the same design): the commit takes its results over, held and merged fills
                    // included, so the drop shows no older fill and refills nothing it filled.
                    let current = !changed && deltas.is_empty();
                    let (sample, carry) = if preview.as_ref().is_some_and(|s| s.seq == seq) {
                        let sample = preview.take().expect("the slot holds this sample");
                        let old = self.preview.take();
                        if !current || self.needs_verify(&sample.ops) {
                            (Some(sample), None)
                        } else {
                            match self.live(sample, old) {
                                Ok(live) => (Some(live.sample), Some(live.results)),
                                Err((sample, ..)) => (Some(sample), None),
                            }
                        }
                    } else if self.preview.as_ref().is_some_and(|p| p.sample.seq == seq) {
                        let mut live = self.preview.take().expect("the live preview is this sample");
                        // Taking this live sample by seq fixes both its ops and placement.
                        let can_carry = current && Arc::ptr_eq(&live.base, &self.design)
                            && self.held.is_empty() && self.eval.is_none()
                            && live.carry.as_ref().is_some_and(|carry| {
                                Arc::ptr_eq(&carry.factories, &self.factories)
                                    && self.docs.as_ref().is_some_and(|docs| {
                                        docs.same_texts(&carry.sources_before) && Arc::ptr_eq(docs.libs(), &carry.libs)
                                    })
                            });
                        if can_carry {
                            let carry = live.carry.take().expect("this live sample has a verified carry");
                            let sample = live.sample;
                            stash_notes(self, &sample);
                            self.clear_preview_jobs(seq);
                            if preview.as_ref().is_some_and(|s| s.seq <= seq) {
                                preview = None;
                            }
                            let seq = self.steps.fetch_add(1, SeqCst) + 1;
                            self.design = live.design;
                            self.results = live.results;
                            // The preview's rows were synced over these same committed rows: keeping
                            // its index Arcs leaves sync_sheets nothing to do, so no view lays out again.
                            self.sheets = live.sheets;
                            carried = true;
                            changed = true;
                            self.follow_keys(&carry.edit.assets);
                            let docs = self.docs.as_mut().expect("a verified carry has documents");
                            for unit in carry.replaced {
                                docs.bump_epoch(unit);
                            }
                            docs.adopt_evaluated(Some(carry.sources), carry.evaluated, carry.benches);
                            self.log.push(OpEntry {
                                seq, by: sample.by, label: sample.label, ops: sample.ops, inverse: vec![],
                                text: carry.edit, text_inverse: carry.inverse, epochs: vec![],
                                kind: EntryKind::Eval, group: None,
                            });
                            self.logged.fetch_max(seq, SeqCst);
                            self.typing = None;
                            refusal_changed |= self.last_refusal.take().is_some();
                            self.committed(Some(sample.tag), None, live.created, carry.sites, None, &mut outcomes);
                            makepad_widgets_core::log!("electronics: preview {} committed without a second evaluation", sample.seq);
                            continue;
                        }
                        let carry = (current && Arc::ptr_eq(&live.base, &self.design)).then_some(live.results);
                        (Some(live.sample), carry)
                    } else if self.preview_job.as_ref().is_some_and(|job| job.sample.seq == seq) {
                        (self.preview_job.take().map(|job| job.sample), None)
                    } else if self.preview_next.as_ref().is_some_and(|sample| sample.seq == seq) {
                        (self.preview_next.take(), None)
                    } else {
                        (None, None)
                    };
                    if let Some(sample) = sample {
                        stash_notes(self, &sample);
                        self.preview = None;
                        self.clear_preview_jobs(seq);
                        if preview.as_ref().is_some_and(|s| s.seq <= seq) {
                            preview = None;
                        }
                        let op = op_kind(&sample.ops);
                        if self.needs_verify(&sample.ops) {
                            let reserved = self.steps.fetch_add(1, SeqCst) + 1;
                            queue.push_front(Queued {
                                cmd: Cmd::Commit {
                                    ops: sample.ops, place: sample.place, label: sample.label, circuits: sample.circuits, expect: sample.expect,
                                    by: sample.by, ticket: None,
                                },
                                sample: Some(sample.tag),
                                seq: Some(reserved),
                            });
                            continue;
                        }
                        match self.board_ops(sample.ops, &sample.place)
                            .and_then(|ops| self.commit(ops, sample.label.clone(), sample.expect, sample.by)) {
                            Ok(batch) => {
                                let (created, sites) = self.board_created();
                                self.committed(Some(sample.tag), None, created, sites, None, &mut outcomes);
                                refusal_changed |= self.last_refusal.take().is_some();
                                changed |= batch.changed;
                                match carry {
                                    Some(results) => {
                                        self.results = results;
                                        carried = true;
                                    }
                                    None => {
                                        carried &= !batch.changed;
                                        fold(&mut deltas, batch.deltas);
                                    }
                                }
                            }
                            Err(refused) => {
                                rejected.push(self.refuse(sample.label, op, Some(sample.tag), refused));
                                refusal_changed = true;
                            }
                        }
                    } else {
                        rejected.push(self.ended(tag));
                        refusal_changed = true;
                    }
                }
                // The host's refusal of a view's sample ends it (and older ones) as a cancel
                // does, with one refusal under its tag: the stored one if it was refused already.
                Cmd::RefusePreview { seq, tag, reason } => {
                    if self.answer_refused(seq, &mut rejected) {
                        refusal_changed = true;
                        continue;
                    }
                    let found = if preview.as_ref().is_some_and(|s| s.seq == seq) {
                        preview.take()
                    } else if self.preview.as_ref().is_some_and(|p| p.sample.seq == seq) {
                        self.preview.take().map(|p| p.sample)
                    } else if self.preview_job.as_ref().is_some_and(|job| job.sample.seq == seq) {
                        self.preview_job.take().map(|job| job.sample)
                    } else if self.preview_next.as_ref().is_some_and(|sample| sample.seq == seq) {
                        self.preview_next.take()
                    } else {
                        None
                    };
                    self.clear_preview_jobs(seq);
                    if self.preview.as_ref().is_some_and(|p| p.sample.seq <= seq) {
                        self.preview = None;
                    }
                    if preview.as_ref().is_some_and(|s| s.seq <= seq) {
                        preview = None;
                    }
                    let (label, op) = found.map_or_else(|| ("preview".to_string(), "preview"), |sample| {
                        let op = op_kind(&sample.ops);
                        (sample.label, op)
                    });
                    rejected.push(self.refuse(label, op, Some(tag), Refused { text: reason, site: None }));
                    if let Some((ticket, _)) = self.preview_tickets.remove(&tag) {
                        self.refusal_outcome(Some(ticket), &mut outcomes);
                    }
                    refusal_changed = true;
                }
                Cmd::CancelPreview { seq } => {
                    self.clear_preview_jobs(seq);
                    if self.refused_preview.as_ref().is_some_and(|(sample, _)| *sample <= seq) {
                        self.refused_preview = None;
                    }
                    if self.preview.as_ref().is_some_and(|p| p.sample.seq <= seq) {
                        self.preview = None;
                    }
                    if preview.as_ref().is_some_and(|s| s.seq <= seq) {
                        preview = None;
                    }
                }
                Cmd::Undo | Cmd::Redo => {
                    let undo = matches!(cmd, Cmd::Undo);
                    if self.logged.load(SeqCst) != self.redo_mark {
                        let label = self.log.entries.get(self.log.head).map(|entry| entry.label.clone()).unwrap_or_else(|| "Redo".into());
                        // A later undo must not revive redo killed by a commit in the other history.
                        self.log.entries.truncate(self.log.head);
                        if !undo {
                            rejected.push(self.refuse(label, "redo", None, Refused { text: "nothing to redo: an edit came after the undo".into(), site: None }));
                            refusal_changed = true;
                            continue;
                        }
                    }
                    let index = if undo { self.log.head.checked_sub(1) } else { Some(self.log.head) };
                    let Some(entry) = index.and_then(|i| self.log.entries.get(i)).cloned() else { continue };
                    let applied = self.history(&entry, undo);
                    match applied {
                        Ok(batch) => {
                            refusal_changed |= self.last_refusal.take().is_some();
                            changed |= batch.changed;
                            carried &= !batch.changed;
                            fold(&mut deltas, batch.deltas);
                            if undo {
                                self.log.head -= 1;
                                self.redo_mark = self.logged.load(SeqCst);
                            } else {
                                self.log.head += 1;
                            }
                        }
                        Err(refused) => {
                            let label = entry.label.clone();
                            rejected.push(self.refuse(label, if undo { "undo" } else { "redo" }, None, refused));
                            refusal_changed = true;
                        }
                    }
                }
                Cmd::SetFactories(factories) => {
                    for unit in &self.design.units {
                        let (Some(name), Some(board)) = (&unit.factory, &unit.board) else { continue };
                        let same = match (self.factories.get(name), factories.get(name)) {
                            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
                            (None, None) => true,
                            _ => false,
                        };
                        if !same {
                            engines::merge_delta(deltas.entry(unit.id).or_default(), &BoardDelta {
                                global: true, bbox: board.outline_bbox(), ..Default::default()
                            });
                        }
                    }
                    self.factories = factories;
                }
                Cmd::CaseFindings { unit, generation, mut findings } => {
                    if self.case.get(&unit).is_some_and(|(held, _)| *held > generation) {
                        continue;
                    }
                    let Some(board) = self.design.unit(unit).and_then(|u| u.board.as_ref()) else { continue };
                    findings.sort_by_key(|v| v.id);
                    let waived: HashSet<_> = board.rules.waivers.iter().map(|w| w.violation).collect();
                    findings.retain(|v| !waived.contains(&v.id));
                    self.case.insert(unit, (generation, Arc::new(findings)));
                    case_changed = true;
                }
                Cmd::CaseKeepouts { unit, sets } => {
                    let Some(board) = self.design.unit(unit).and_then(|u| u.board.as_ref()) else { continue };
                    let named = |name: &str| sets.iter().any(|(set, _)| set == name);
                    let mut gone: Vec<FeatureRef> = Vec::new();
                    for keepout in &board.keepouts {
                        if let Origin::Derived { from: from @ FeatureRef::Panel(source) } = &keepout.origin {
                            if source.strip_prefix(CASE_KEEPOUT).is_some_and(|name| !named(name)) && !gone.contains(from) {
                                gone.push(from.clone());
                            }
                        }
                    }
                    // A copy, so an unchanged publication leaves the committed design's Arc as it is.
                    let mut design = (*self.design).clone();
                    let mut moved = BoardDelta::default();
                    let mut any = false;
                    let sets = sets.into_iter().map(|(name, items)| (FeatureRef::Panel(format!("{CASE_KEEPOUT}{name}")), items));
                    for (from, items) in sets.chain(gone.into_iter().map(|from| (from, Vec::new()))) {
                        match design.set_derived(unit, &from, items) {
                            Ok(Some(delta)) => {
                                engines::merge_delta(&mut moved, &delta);
                                any = true;
                            }
                            Ok(None) => {}
                            Err(error) => makepad_widgets_core::log!("electronics: the case keepout {from:?} was not applied: {error:?}"),
                        }
                    }
                    if any {
                        self.design = Arc::new(design);
                        engines::merge_delta(deltas.entry(unit).or_default(), &moved);
                    }
                }
            }
        }

        // Commit (including preview), undo and redo all prune against the committed design.
        if changed {
            let still_sharp = |note: &Violation| {
                let ([Sel::Item(unit, id)], Some(at)) = (note.items.as_slice(), note.at) else { return false };
                let Some(board) = self.design.unit(*unit).and_then(|u| u.board.as_ref()) else { return false };
                let Some((ItemKind::Track, i)) = board.index.get(*id) else { return false };
                let Some(track) = board.tracks.get(i).filter(|t| t.id == *id) else { return false };
                track.path.segs.windows(2).enumerate().any(|(i, legs)| {
                    let [Seg::Line(corner), Seg::Line(end)] = legs else { return false };
                    if *corner != at { return false; }
                    let start = if i == 0 { track.path.start } else { track.path.segs[i - 1].end() };
                    let (ax, ay) = (at.x as f64 - start.x as f64, at.y as f64 - start.y as f64);
                    let (bx, by) = (end.x as f64 - at.x as f64, end.y as f64 - at.y as f64);
                    ax * bx + ay * by < 0.0
                })
            };
            let mut dropped = HashSet::new();
            self.notes.retain(|_, notes| {
                notes.retain(|note| {
                    let keep = still_sharp(note);
                    if !keep { dropped.insert(note.id); }
                    keep
                });
                !notes.is_empty()
            });
            if !dropped.is_empty() {
                self.notes_sorted.retain(|v| !dropped.contains(&v.id));
            }
            let before = self.case.len();
            self.case.retain(|unit, _| self.design.unit(*unit).is_some_and(|u| u.board.is_some()));
            case_changed |= self.case.len() != before;
            // A waiver committed with the rules applies to the held findings at once, not at the
            // next case rebuild (a rules edit moves no case input). O(C + waivers).
            for (unit, (_, findings)) in self.case.iter_mut() {
                let Some(board) = self.design.unit(*unit).and_then(|u| u.board.as_ref()) else { continue };
                let waived: HashSet<_> = board.rules.waivers.iter().map(|w| w.violation).collect();
                if findings.iter().any(|v| waived.contains(&v.id)) {
                    Arc::make_mut(findings).retain(|v| !waived.contains(&v.id));
                    case_changed = true;
                }
            }
        }
        if case_changed {
            self.case_sorted = self.case.values().flat_map(|(_, findings)| findings.iter()).cloned().collect();
            self.case_sorted.sort_by_key(|v| v.id);
            self.case_sorted.dedup_by_key(|v| v.id);
        }
        if changed {
            // Only commits, undo and redo move the committed design; the preview never does.
            self.sync_sheets();
            // Results a commit took over already describe its design; only later changes need pass A.
            if !(carried && deltas.is_empty()) {
                publish(self.make_snapshot(StageFlags::CONNECT.with(StageFlags::DRC)));
            }
        }

        let deltas: Vec<_> = deltas.into_iter().collect();
        let mut fills = Vec::new();
        // A job with only fill results (or nothing) runs no pass A and publishes only what merged.
        if (changed && !carried) || !deltas.is_empty() {
            self.results = engines::update_held(&self.results, &self.design, &deltas, &self.factories);
        }
        // A committed change rebuilds the live sample over the new committed results; factory
        // changes also require rebasing a live preview's effective rules.
        let rebase = changed || !deltas.is_empty();
        // A circuit result over old text stays visible until its replacement arrives. A running
        // job retries on completion; never queue the older visible sample behind a newer job.
        let text_moved = self.preview.as_ref().and_then(|live| live.carry.as_ref()).is_some_and(|carry| {
            self.docs.as_ref().is_some_and(|docs| !docs.same_texts(&carry.sources_before) || !Arc::ptr_eq(docs.libs(), &carry.libs))
        });
        // A sample planned on a board that changed since is refused under its tag; its drop
        // answers with that refusal (`refused_preview`).
        if let Some(refused) = preview.as_ref().and_then(|sample| self.base_moved(sample)) {
            let sample = preview.take().expect("the waiting sample's base moved");
            let ticket = preview_ticket.take_if(|(tag, ..)| *tag == sample.tag).map(|(_, ticket, _)| ticket);
            self.refuse_preview(sample, refused, &mut rejected, &mut outcomes);
            self.refusal_outcome(ticket, &mut outcomes);
            preview_changed = true;
        }
        let sample = preview.or_else(|| {
            ((rebase || text_moved) && self.preview_job.is_none() && self.preview_next.is_none())
                .then(|| self.preview.as_ref().map(|live| live.sample.clone())).flatten()
        });
        if let Some(sample) = sample {
            if let Some(refused) = self.base_moved(&sample) {
                self.refuse_preview(sample, refused, &mut rejected, &mut outcomes);
                preview_changed = true;
            } else if self.needs_verify(&sample.ops) {
                if preview_ticket.as_ref().is_some_and(|(tag, ..)| *tag == sample.tag) {
                    let (tag, ticket, label) = preview_ticket.take().expect("the sample has this ticket");
                    self.preview_tickets.insert(tag, (ticket, label));
                }
                self.queue_preview(sample, &mut evals, &mut rejected, &mut outcomes);
            } else {
                self.preview_job = None;
                self.preview_next = None;
                let old = self.preview.take().filter(|_| !rebase);
                match self.live(sample, old) {
                    Ok(live) => {
                        if preview_ticket.as_ref().is_some_and(|(tag, ..)| *tag == live.sample.tag) {
                            let (tag, ticket, _) = preview_ticket.take().expect("the live sample has this ticket");
                            outcomes.push((ticket, Outcome::Previewed { tag }));
                        }
                        refusal_changed |= self.last_refusal.take().is_some();
                        self.preview = Some(live);
                    }
                    Err((sample, refused)) => {
                        let ticket = if preview_ticket.as_ref().is_some_and(|(tag, ..)| *tag == sample.tag) {
                            preview_ticket.take().map(|(_, ticket, _)| ticket)
                        } else { None };
                        self.refuse_preview(sample, refused, &mut rejected, &mut outcomes);
                        self.refusal_outcome(ticket, &mut outcomes);
                        refusal_changed = true;
                    }
                }
                preview_changed = true;
            }
        }
        // Cancelling only the older in-flight sample may leave a newer waiting sample.
        if self.preview_job.is_none() {
            if let Some(sample) = self.preview_next.take() {
                self.queue_preview(sample, &mut evals, &mut rejected, &mut outcomes);
            }
        }
        if preview_changed {
            if let Some(live) = &self.preview {
                for task in engines::owed(&live.results) {
                    let Ok(i) = live.results.boards.binary_search_by_key(&task.unit, |b| b.unit) else { continue };
                    let results = &live.results.boards[i];
                    let Some(board) = live.design.unit(task.unit).and_then(|u| u.board.as_ref()) else { continue };
                    let Ok(f) = results.fills.binary_search_by_key(&task.zone, |f| f.zone) else { continue };
                    fills.push(FillJob {
                        task, board: board.clone(), rules: results.rules.clone(),
                        index: results.index.0.clone(),
                        prev: results.fills[f].clone(), delta: results.pending.debt.clone(),
                        preview: Some(live.sample.seq),
                    });
                }
            }
        }
        // A kept sample its commit did not claim ends here, so none outlives its step.
        for sample in committing {
            rejected.push(self.ended(sample.tag));
            refusal_changed = true;
        }
        // Every Invoke gets one answer, including a queued sample replaced or cancelled before
        // evaluation. Running samples keep their ticket until their result is shown or refused.
        // Job tags occupy the upper range and survive until an explicit commit or refusal.
        let ended: Vec<_> = self.preview_tickets.range(..JobId(0).tag()).map(|(tag, _)| *tag).filter(|tag| {
            !self.preview_job.as_ref().is_some_and(|job| job.sample.tag == *tag)
                && !self.preview_next.as_ref().is_some_and(|sample| sample.tag == *tag)
        }).collect();
        for tag in ended {
            let (ticket, label) = self.preview_tickets.remove(&tag).expect("the ended sample has a ticket");
            rejected.push(self.refuse(label, "invoke", Some(tag), Refused {
                text: "the preview ended before it became live; ask again".into(), site: None,
            }));
            self.refusal_outcome(Some(ticket), &mut outcomes);
            refusal_changed = true;
        }
        refusal_changed |= self.last_refusal != refusal_before;
        if let Some((tag, ticket, label)) = preview_ticket {
            rejected.push(self.refuse(label, "invoke", Some(tag), Refused {
                text: "the preview ended before it became live; ask again".into(), site: None,
            }));
            self.refusal_outcome(Some(ticket), &mut outcomes);
            refusal_changed = true;
        }
        if changed || preview_changed || case_changed || refusal_changed || self.last_commit != commit_before || !deltas.is_empty()
            || self.preview.as_ref().map(|p| p.sample.seq) != preview_before {
            publish(self.make_snapshot(StageFlags::NONE));
            merged = false;
        }

        for (unit, zone, generation, rules, fill) in committed {
            if self.in_flight.get(&(unit, zone)).is_some_and(|(g, r)| *g == generation && Arc::ptr_eq(r, &rules)) {
                self.in_flight.remove(&(unit, zone));
            }
            if let Some(results) = engines::merge_fill(
                &self.results, &self.design, unit, zone, generation, &rules, fill,
            ) {
                self.results = results;
                merged = true;
            }
        }
        if merged {
            publish(self.make_snapshot(StageFlags::NONE));
        }

        for task in engines::owed(&self.results) {
            let Ok(i) = self.results.boards.binary_search_by_key(&task.unit, |b| b.unit) else { continue };
            let results = &self.results.boards[i];
            let Ok(z) = results.pending.zones.binary_search_by_key(&task.zone, |(id, _)| *id) else { continue };
            let reached_at = results.pending.zones[z].1;
            // A running job still stands when it fills at or after the zone's last reach with the
            // unit's current rules.
            if self.in_flight.get(&(task.unit, task.zone)).is_some_and(|(g, r)| *g >= reached_at && Arc::ptr_eq(r, &results.rules)) {
                continue;
            }
            let Some(board) = self.design.unit(task.unit).and_then(|u| u.board.as_ref()) else { continue };
            let Ok(f) = results.fills.binary_search_by_key(&task.zone, |f| f.zone) else { continue };
            self.in_flight.insert((task.unit, task.zone), (task.generation, results.rules.clone()));
            fills.push(FillJob {
                task, board: board.clone(), rules: results.rules.clone(),
                index: results.index.0.clone(),
                prev: results.fills[f].clone(), delta: results.pending.debt.clone(),
                preview: None,
            });
        }

        if self.eval.is_none() && self.eval_blocked() {
            evals.push(self.start_eval(None));
        }
        // The pre-save check before the plate stores the texts this step changed: a mismatch of
        // the system text and the model is a located note (`Docs::check_system`). An adopted
        // evaluation replaces the units the system text lowers against, so it checks again too.
        let mut noted = false;
        if self.docs.as_ref().is_some_and(|docs| evaluated || Some(docs.revision()) != revision) {
            let design = self.design.clone();
            if let Some(docs) = self.docs.as_mut() {
                let view = docs.view().clone();
                docs.check_system(&design);
                noted = !Arc::ptr_eq(&view, docs.view());
            }
        }
        if noted || eval_before != self.eval_blocked() || preview_eval_before != self.preview_job.is_some()
            || self.docs.as_ref().map(Docs::revision) != revision {
            publish(self.make_snapshot(StageFlags::NONE));
        }
        let waiting = !self.held.is_empty() || self.eval.is_some();

        let undo = self.log.head.checked_sub(1).map(|i| self.log.entries[i].seq);
        let redo = if self.logged.load(SeqCst) == self.redo_mark {
            self.log.entries.get(self.log.head).map(|entry| entry.seq)
        } else {
            None
        };
        let docs = self.docs.as_ref().filter(|docs| Some(docs.revision()) != revision).map(|docs| Written {
            docs: docs.texts(),
            assets: Arc::new(docs.sources().assets().iter()
                .map(|name| (name.clone(), self.own_assets.get(name).map(|bytes| bytes.0.clone()))).collect()),
        });
        // A command may cancel a preview job started by a result earlier in this same step.
        evals.retain(|job| job.preview.is_none() || self.preview_job.as_ref().is_some_and(|running| running.id == job.id));
        // O(1) when no route is parked; otherwise its unit's committed base, once exact. Before the
        // bases below, so a started unit's base goes once, with its job.
        if let Some((_, _, job)) = &self.waiting_route {
            match self.base_of(job.unit) {
                Some(base) if base.exact => {
                    let (ticket, by, job) = self.waiting_route.take().expect("a route is parked");
                    self.routed.insert(job.unit, base.clone());
                    routes.push(RouteStart { ticket, by, job, base });
                }
                Some(_) => {}
                None => {
                    let (ticket, _, job) = self.waiting_route.take().expect("a route is parked");
                    let (_, label) = self.preview_tickets.remove(&job.job.tag()).expect("a parked route retains its caller");
                    rejected.push(self.refuse(label, "invoke", Some(job.job.tag()), Refused {
                        text: format!("unit {} has no board", job.unit.0), site: None,
                    }));
                    self.refusal_outcome(Some(ticket), &mut outcomes);
                    publish(self.make_snapshot(StageFlags::NONE));
                }
            }
        }
        let mut bases = Vec::new();
        for unit in &asked {
            match self.base_of(*unit) {
                Some(base) => {
                    self.routed.insert(*unit, base.clone());
                    bases.push(base);
                }
                None => rejected.push(("route base".into(), format!("unit {} has no board", unit.0))),
            }
        }
        let routed: Vec<_> = self.routed.keys().copied().filter(|unit| !asked.contains(unit)).collect();
        for unit in routed {
            match self.base_of(unit) {
                Some(base) => {
                    let sent = self.routed.get(&unit).expect("the unit has a sent base");
                    if !Arc::ptr_eq(&base.board, &sent.board) || !Arc::ptr_eq(&base.rules, &sent.rules)
                        || !Arc::ptr_eq(&base.conn, &sent.conn) || !Arc::ptr_eq(&base.fills, &sent.fills) {
                        self.routed.insert(unit, base.clone());
                        bases.push(base);
                    }
                }
                None => { self.routed.remove(&unit); }
            }
        }
        // Keep retired snapshots, answers and samples alive through every publish and all engine work.
        drop((dead, dead_answers, dead_samples));
        Step { fills, routes, bases, evals, waiting, rejected, outcomes, undo, redo, docs }
    }

    /// Evaluation serialises circuit/bench commits and history, but never blocks board previews.
    fn eval_blocked(&self) -> bool {
        self.docs.as_ref().is_some_and(|docs| self.eval.is_some() || self.library_changed || docs.behind())
    }

    /// Only document-backed circuit, bench and file commits go through pool verification.
    fn needs_verify(&self, ops: &[DesignOp]) -> bool {
        self.docs.is_some() && ops.iter().any(|op| matches!(op, DesignOp::Circuit { .. } | DesignOp::Bench { .. } | DesignOp::File(_)))
    }

    /// Follow a step's asset availability in the keys bodies resolve through and in the bytes the
    /// plate's write carries: an available asset names its bytes' key, an unavailable one frees its
    /// name. Cost: O(step assets x log keys).
    fn follow_keys(&mut self, assets: &[AssetStep]) {
        for asset in assets {
            if asset.available {
                self.mesh_keys.insert(asset.name.clone(), asset.key.clone());
                self.own_assets.insert(asset.name.clone(), asset.bytes.clone());
            } else if self.mesh_keys.get(&asset.name) == Some(&asset.key) {
                self.mesh_keys.remove(&asset.name);
                self.own_assets.remove(&asset.name);
            }
        }
    }

    /// Refuse mixed system commits and edits whose unit still has broken source text.
    fn check_verify(&self, ops: &[DesignOp]) -> Result<(), Refused> {
        if ops.iter().any(|op| matches!(op, DesignOp::Sys(_))) {
            return Err(Refused {
                text: "system ops cannot share a commit with circuit or bench ops until the system document is spliced (G1)".into(),
                site: None,
            });
        }
        for op in ops {
            if let DesignOp::File(FileOp::AddAsset { name, key, .. }) = op {
                if self.mesh_keys.get(name).is_some_and(|held| held != key) {
                    return Err(Refused { text: format!("{name} is in the project with other bytes: a changed asset reloads the bed's documents"), site: None });
                }
            }
        }
        let docs = self.docs.as_ref().expect("verification has documents");
        for op in ops {
            let unit = match op {
                DesignOp::Circuit { unit, .. } => Some(*unit),
                DesignOp::Bench { bench, .. } => docs.bench_unit(bench),
                _ => None,
            };
            if let Some(UnitState::Kept(diags) | UnitState::Failed(diags)) = unit.and_then(|unit| docs.unit_state(unit)) {
                return Err(Refused {
                    text: diags.first().map_or_else(|| "the unit's documents did not build".into(), |diag| diag.message.clone()),
                    site: diags.first().and_then(|diag| docs.site_of(diag)),
                });
            }
        }
        Ok(())
    }

    /// Snapshot the engine's own followed evaluation state for the one pool job.
    fn start_eval(&mut self, verify: Option<Verify>) -> EvalJob {
        let docs = self.docs.as_ref().expect("evaluation has documents");
        self.evals += 1;
        let mut job = EvalJob {
            id: self.evals, preview: None, sources: docs.sources().clone(), libs: docs.libs().clone(),
            prev: Arc::new(docs.prev(&self.design)), benches: docs.bench_views().clone(),
            verify: verify.as_ref().filter(|verify| verify.text.is_none()).map(|verify| verify.ops.clone()),
            text: verify.as_ref().and_then(|verify| verify.text.as_ref().map(|(_, _, edits)| edits.clone())),
            place: verify.as_ref().map_or_else(Vec::new, |verify| verify.place.clone()),
        };
        let sources = job.sources.clone();
        let library_sources = std::mem::take(&mut self.library_changed).then(|| {
            job.sources.set_library(self.library_keys.keys().cloned().collect());
            job.sources.clone()
        });
        self.eval = Some(InFlight { id: job.id, sources, library_sources, verify });
        job
    }

    /// Per sample: validate its ops; queue in O(1), or clone the documents and evaluation metadata.
    fn queue_preview(&mut self, sample: Sample, evals: &mut Vec<EvalJob>, rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) {
        // Every (re)issue of a circuit sample passes here: on arrival, after the texts moved and
        // when a committed change rebases the live one.
        if let Err(refused) = self.check_circuits(&sample.circuits).and_then(|()| self.check_verify(&sample.ops)) {
            self.preview_job = None;
            self.preview_next = None;
            self.refuse_preview(sample, refused, rejected, outcomes);
        } else if self.preview_job.is_some() {
            self.preview_next = Some(sample);
        } else {
            self.preview_next = None;
            evals.push(self.start_preview(sample));
        }
    }

    /// Per cancellation or drop: O(1) slot checks; late results no longer match a cleared job.
    fn clear_preview_jobs(&mut self, seq: u64) {
        if self.preview_job.as_ref().is_some_and(|job| job.sample.seq <= seq) {
            self.preview_job = None;
        }
        if self.preview_next.as_ref().is_some_and(|sample| sample.seq <= seq) {
            self.preview_next = None;
        }
    }

    /// Per started sample: O(documents + evaluation metadata + ops) clones, with no evaluation here.
    fn start_preview(&mut self, sample: Sample) -> EvalJob {
        let asked = Instant::now();
        let docs = self.docs.as_ref().expect("a circuit preview has documents");
        self.evals += 1;
        let job = EvalJob {
            id: self.evals, preview: Some(sample.seq), sources: docs.sources().clone(), libs: docs.libs().clone(),
            prev: Arc::new(docs.prev(&self.design)), benches: docs.bench_views().clone(), verify: Some(sample.ops.clone()), text: None,
            place: sample.place.clone(),
        };
        self.preview_job = Some(PreviewJob {
            id: job.id, sample, sources: job.sources.clone(), libs: job.libs.clone(), asked,
        });
        job
    }

    /// Per refused sample: one refusal allocation, shared with its caller and a later drop.
    fn refuse_preview(&mut self, sample: Sample, refused: Refused, rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) {
        self.preview = None;
        rejected.push(self.refuse(sample.label, op_kind(&sample.ops), Some(sample.tag), refused));
        let refusal = self.last_refusal.as_ref().expect("the sample was refused").clone();
        self.refused_preview = Some((sample.seq, refusal));
        if let Some((ticket, _)) = self.preview_tickets.remove(&sample.tag) {
            self.refusal_outcome(Some(ticket), outcomes);
        }
    }

    /// A drop or host refusal of a sample refused already: its stored refusal answers it once.
    fn answer_refused(&mut self, seq: u64, rejected: &mut Vec<(String, String)>) -> bool {
        if !self.refused_preview.as_ref().is_some_and(|(sample, _)| *sample == seq) {
            return false;
        }
        let (_, refusal) = self.refused_preview.take().expect("this sample was refused");
        rejected.push((refusal.label.clone(), refusal.text.clone()));
        self.last_refusal = Some(refusal);
        true
    }

    /// A drop of a sample the engine no longer holds (another author's sample replaced it, or
    /// the engine reloaded): one refusal under its tag, so the view still gets its answer.
    fn ended(&mut self, tag: u64) -> (String, String) {
        self.refuse("preview".into(), "preview", Some(tag), Refused {
            text: "this preview had already ended (replaced or reloaded): its drop applied nothing".into(), site: None,
        })
    }

    /// Per result: O(documents) base check, changed-board pass A, and O(changed circuit rows) sync.
    fn finish_preview(&mut self, done: EvalDone, evals: &mut Vec<EvalJob>, rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>) -> bool {
        let job = self.preview_job.take().expect("the result matches the preview job");
        let docs = self.docs.as_ref().expect("a circuit preview has documents");
        if !docs.same_texts(&job.sources) || !Arc::ptr_eq(docs.libs(), &job.libs) {
            let sample = self.preview_next.take().unwrap_or(job.sample);
            self.queue_preview(sample, evals, rejected, outcomes);
            return false;
        }
        match done.out {
            EvalOut::Verified(Err(diag)) => {
                let site = docs.site_of(&diag);
                self.refuse_preview(job.sample, Refused { text: diag.message, site }, rejected, outcomes);
            }
            EvalOut::Verified(Ok((mut verified, inverse))) => {
                let (mut created, sites) = created_in(docs.map(), &verified.evaluated.map, &verified.edit.written, &self.design, &verified.evaluated.design);
                created.extend(done.placed);
                let keys = with_assets(&self.mesh_keys, &verified.edit.assets);
                let (design, deltas, replaced) = swap_evaluated(
                    &self.design, std::mem::take(&mut verified.evaluated.design), &done.same_boards, [&*keys, &self.library_keys],
                );
                let results = engines::update_held(&self.results, &design, &deltas, &self.factories);
                let sheets = self.preview_sheets(&design);
                let seq = job.sample.seq;
                if let Some((ticket, _)) = self.preview_tickets.remove(&job.sample.tag) {
                    outcomes.push((ticket, Outcome::Previewed { tag: job.sample.tag }));
                }
                self.preview = Some(Live {
                    sample: job.sample, created, base: self.design.clone(), deltas, design, results,
                    from: BTreeMap::new(), merged: BTreeSet::new(), sheets,
                    carry: Some(Carry {
                        sources_before: job.sources, libs: job.libs, factories: self.factories.clone(), sources: verified.sources,
                        edit: verified.edit.into(), inverse, evaluated: verified.evaluated, benches: done.benches, replaced, sites,
                    }),
                });
                self.last_refusal = None;
                makepad_widgets_core::log!("electronics: circuit preview {seq} shown {:.1} ms after its sample", job.asked.elapsed().as_secs_f64() * 1000.0);
            }
            EvalOut::Evaluated(_) => unreachable!("a preview job always verifies its sample"),
        }
        if let Some(sample) = self.preview_next.take() {
            self.queue_preview(sample, evals, rejected, outcomes);
        }
        true
    }

    /// Adopt only the matching job on unchanged texts; stale canvas commits retry first.
    fn finish_eval(
        &mut self, done: EvalDone, queue: &mut VecDeque<Queued>, deltas: &mut BTreeMap<BoardId, BoardDelta>,
        rejected: &mut Vec<(String, String)>, outcomes: &mut Vec<(Ticket, Outcome)>,
    ) -> bool {
        if !self.eval.as_ref().is_some_and(|job| job.id == done.id) {
            return false;
        }
        let inflight = self.eval.take().expect("the result matches an in-flight evaluation");
        let same = self.docs.as_ref().is_some_and(|docs| docs.same_texts(&inflight.sources));
        if !same {
            self.library_changed |= inflight.library_sources.is_some();
            match inflight.verify {
                // Checked again over the moved texts: refused there if its own document moved.
                Some(Verify { seq, label, by, ticket: Some(ticket), text: Some((doc, base_rev, edits)), .. }) => {
                    queue.push_front(Queued { cmd: Cmd::CheckedText { doc, base_rev, edits, label, by, ticket }, sample: None, seq: Some(seq) });
                }
                Some(Verify { seq, ops, place, circuits, label, by, ticket, sample, .. }) => {
                    queue.push_front(Queued { cmd: Cmd::Commit { ops, place, label, circuits, expect: None, by, ticket }, sample, seq: Some(seq) });
                }
                None => {}
            }
            return false;
        }
        let refusals = self.refusals;
        let changed = match done.out {
            EvalOut::Evaluated(Ok(ev)) => self.adopt(inflight.library_sources, ev, done.benches, &done.same_boards, deltas),
            EvalOut::Evaluated(Err(diags)) => {
                self.docs.as_mut().expect("evaluation has documents").adopt_failed(inflight.library_sources, diags);
                false
            }
            EvalOut::Verified(Ok((verified, inverse))) => {
                let Verify { seq, ops, label, by, ticket, sample, text, .. } = inflight.verify.expect("verification has a canvas commit");
                // A checked text change moves the take as typing the same text would.
                let text_change = text.map(|(doc, ..)| {
                    let kind = self.docs.as_ref().and_then(|docs| docs.doc_kind(doc));
                    if kind == Some(DocKind::Circuit) { CircuitChange::Structure } else { CircuitChange::None }
                });
                let (mut created, sites) = created_in(
                    self.docs.as_ref().expect("verification has documents").map(),
                    &verified.evaluated.map, &verified.edit.written, &self.design, &verified.evaluated.design,
                );
                created.extend(done.placed);
                // A committed edit clears the previous refusal; its live sample rebases below.
                self.last_refusal = None;
                let text = TextStep::from(verified.edit);
                self.follow_keys(&text.assets);
                let changed = self.adopt(Some(verified.sources), verified.evaluated, done.benches, &done.same_boards, deltas);
                self.typing = None;
                // Keep each history's numbers increasing, which route relies on.
                let seq = if self.log.entries.last().is_some_and(|entry| entry.seq >= seq) {
                    self.steps.fetch_add(1, SeqCst) + 1
                } else {
                    seq
                };
                self.log.push(OpEntry {
                    seq, by, label, ops, inverse: vec![],
                    text, text_inverse: inverse, epochs: vec![],
                    kind: EntryKind::Eval, group: None,
                });
                self.logged.fetch_max(seq, SeqCst);
                self.committed(sample, ticket, created, sites, text_change, outcomes);
                changed
            }
            EvalOut::Verified(Err(diag)) => {
                let Verify { ops, label, ticket, sample, text, .. } = inflight.verify.expect("verification has a canvas commit");
                let site = self.docs.as_ref().and_then(|docs| docs.site_of(&diag));
                let op = if text.is_some() { "text" } else { op_kind(&ops) };
                self.refuse(label, op, sample, Refused { text: diag.message, site });
                self.refusal_outcome(ticket, outcomes);
                false
            }
        };
        if self.refusals != refusals {
            if let Some(refused) = &self.last_refusal {
                rejected.push((refused.label.clone(), refused.text.clone()));
            }
        }
        changed
    }

    /// Apply typed text and merge only the current gesture's still-applied history head.
    /// Returns the effect on the circuit and the written ranges in the text after the edits (the
    /// inverse's spans), so the answer names them even when the step merges into a typing entry.
    fn text(&mut self, doc: DocId, base_rev: u64, edits: Vec<TextEdit>, group: Option<u64>, label: String, by: By) -> Result<(Vec<Sel>, CircuitChange), Refused> {
        let docs = self.docs.as_mut().ok_or_else(|| Refused {
            text: "the engine has no project documents".into(), site: None,
        })?;
        let before = docs.text(doc).cloned();
        let (text, text_inverse) = docs.text_edit(doc, base_rev, edits).map_err(|diag| Refused {
            site: docs.site_of(&diag), text: diag.message,
        })?;
        let sites = text_inverse.edits.iter().map(|edit| Sel::Source { doc: edit.doc, start: edit.span.start, end: edit.span.end }).collect();
        let doc_kind = docs.doc_kind(doc);
        let kind = if doc_kind == Some(DocKind::System) { EntryKind::System } else { EntryKind::Eval };
        let reevaluate = doc_kind.is_some_and(DocKind::edit_reevaluates);
        let merge = self.typing.as_ref().filter(|typing| {
            group == Some(typing.group) && typing.doc == doc && self.log.head == self.log.entries.len()
                && self.log.entries.last().is_some_and(|entry| entry.seq == typing.seq)
        });
        if let Some(typing) = merge {
            let now = docs.text(doc).expect("an edited document is in the project");
            let entry = self.log.entries.last_mut().expect("the typing entry is at the head");
            entry.text = TextStep {
                edits: vec![TextEdit::trimmed(doc, &typing.before, now)], reevaluate,
                ..TextStep::default()
            };
            entry.text_inverse = TextStep {
                edits: vec![TextEdit::trimmed(doc, now, &typing.before)], reevaluate,
                ..TextStep::default()
            };
        } else {
            let seq = self.steps.fetch_add(1, SeqCst) + 1;
            self.log.push(OpEntry {
                seq, by, label, ops: vec![], inverse: vec![], text, text_inverse, epochs: vec![], kind, group,
            });
            self.logged.fetch_max(seq, SeqCst);
            self.typing = group.map(|group| Typing {
                group, doc, before: before.expect("an edited document had text"), seq,
            });
        }
        match kind {
            EntryKind::System => self.system_text(doc),
            _ => {}
        }
        let circuit = if doc_kind == Some(DocKind::Circuit) { CircuitChange::Structure } else { CircuitChange::None };
        Ok((sites, circuit))
    }

    /// The system document's typed text: ELANG's step 6 lowers it here at once (never behind,
    /// no pool job). Until then, links and mounts stay the model's (G1).
    fn system_text(&mut self, _doc: DocId) {}

    /// Dispatch history by entry kind; failed text restores the model before the head moves.
    /// A board entry whose units' epochs moved takes the text path, and an evaluation brings the
    /// board along; its system ops still go to the model (G1: no text holds them yet).
    fn history(&mut self, entry: &OpEntry, undo: bool) -> Result<Batch, Refused> {
        let (ops, text) = if undo { (&entry.inverse, &entry.text_inverse) } else { (&entry.ops, &entry.text) };
        if let Some(asset) = text.assets.iter().find(|asset| asset.available && self.mesh_keys.get(&asset.name).is_some_and(|held| *held != asset.key)) {
            return Err(Refused { text: format!("{} holds other bytes since this step: a changed asset reloads the bed's documents", asset.name), site: None });
        }
        let system_only = || ops.iter().filter(|op| matches!(op, DesignOp::Sys(_))).cloned().collect::<Vec<_>>();
        let (model, follows): (std::borrow::Cow<[DesignOp]>, bool) = match entry.kind {
            EntryKind::Board => {
                let current = self.docs.as_ref().is_none_or(|docs| entry.epochs.iter().all(|(unit, epoch)| docs.epoch(*unit) == *epoch));
                if current { (ops.as_slice().into(), true) } else { (system_only().into(), false) }
            }
            EntryKind::Eval => ((&[][..]).into(), false),
            // ELANG step 6 replaces this model path with text then lower.
            EntryKind::System => (ops.as_slice().into(), false),
        };
        let before = self.docs.is_some().then(|| self.design.clone());
        let batch = if model.is_empty() {
            Batch { changed: false, inverse: vec![], deltas: vec![] }
        } else {
            apply_ops(Arc::make_mut(&mut self.design), &model, true).map_err(|(i, error)| Refused {
                text: format!("{error:?}"), site: self.docs.as_ref().and_then(|docs| op_site(docs, &model[i])),
            })?
        };
        if let Some(docs) = &mut self.docs {
            if let Err(diag) = docs.apply(text, follows) {
                self.design = before.expect("document-backed history saved the model");
                return Err(Refused { site: docs.site_of(&diag), text: diag.message });
            }
        }
        self.follow_keys(&text.assets);
        Ok(batch)
    }

    /// Commit the shared swap; per result O(documents + metadata), plus the swap's changed-unit work.
    fn adopt(&mut self, sources: Option<Sources>, mut evaluated: Evaluated, benches: Arc<Vec<BenchView>>, same_boards: &[BoardId], deltas: &mut BTreeMap<BoardId, BoardDelta>) -> bool {
        let (design, changes, replaced) = swap_evaluated(
            &self.design, std::mem::take(&mut evaluated.design), same_boards, [&self.mesh_keys, &self.library_keys],
        );
        let changed = !Arc::ptr_eq(&design, &self.design);
        self.design = design;
        fold(deltas, changes);
        let docs = self.docs.as_mut().expect("adoption has documents");
        for unit in replaced {
            docs.bump_epoch(unit);
        }
        docs.adopt_evaluated(sources, evaluated, benches);
        changed
    }

    /// The live preview of `sample` over the committed design, never showing a fill older than
    /// the previous live preview `old` showed (on the same committed design only). A board a fill
    /// of `old` merged into continues from `old`'s results; a board none merged into continues
    /// from where `old` started it (the same fills, and its debt does not grow while its fills
    /// lag); other boards start from the committed results. A continued board's delta is its
    /// start's sample delta joined with this sample's, both against the committed design, so it
    /// covers every difference between the two boards.
    /// Cost: pass A over the boards either delta reaches, as for a sample over the committed
    /// results, plus O(units) results clones (`Arc` fields) when a board continues.
    fn live(&self, sample: Sample, old: Option<Live>) -> Result<Live, (Sample, Refused)> {
        let resolved;
        let ops = if sample.place.is_empty() {
            sample.ops.as_slice()
        } else {
            resolved = match self.board_ops(sample.ops.clone(), &sample.place) {
                Ok(ops) => ops,
                Err(refused) => return Err((sample, refused)),
            };
            &resolved
        };
        let mut design = self.design.clone();
        let batch = match apply_ops(Arc::make_mut(&mut design), ops, false) {
            Ok(batch) => batch,
            Err((i, error)) => {
                let site = self.docs.as_ref().and_then(|docs| op_site(docs, &ops[i]));
                return Err((sample, Refused { text: format!("{error:?}"), site }));
            }
        };
        let mut from = BTreeMap::new();
        if let Some(old) = old.filter(|old| Arc::ptr_eq(&old.base, &self.design)) {
            let Live { results, deltas, from: mut started, merged, .. } = old;
            for board in results.boards {
                let unit = board.unit;
                let start = if merged.contains(&unit) {
                    let delta = deltas.iter().find(|(u, _)| *u == unit).map_or_else(BoardDelta::default, |(_, d)| d.clone());
                    Some((board, delta))
                } else {
                    started.remove(&unit)
                };
                if let Some(start) = start {
                    from.insert(unit, start);
                }
            }
        }
        let results = if from.is_empty() {
            engines::update_held(&self.results, &design, &batch.deltas, &self.factories)
        } else {
            let mut prev = self.results.clone();
            let mut deltas: BTreeMap<_, _> = batch.deltas.iter().cloned().collect();
            for board in &mut prev.boards {
                let Some((start, delta)) = from.get(&board.unit) else { continue };
                *board = start.clone();
                if *delta != BoardDelta::default() {
                    engines::merge_delta(deltas.entry(board.unit).or_default(), delta);
                }
            }
            let deltas: Vec<_> = deltas.into_iter().collect();
            engines::update_held(&prev, &design, &deltas, &self.factories)
        };
        let created = ops.iter().filter_map(|op| match op {
            DesignOp::Board { unit, op: BoardOp::Add(item) } => Some(Sel::Item(*unit, item.id())),
            _ => None,
        }).collect();
        Ok(Live { sample, created, base: self.design.clone(), deltas: batch.deltas, design, results, from, merged: BTreeSet::new(), sheets: self.sheets.clone(), carry: None })
    }

    /// Append placements without changing the sample retained for later rebasing or dropping.
    fn board_ops(&self, mut ops: Vec<DesignOp>, place: &[Place]) -> Result<Vec<DesignOp>, Refused> {
        ops.extend(place_ops(&self.design, place, &[]).map_err(|diag| Refused {
            site: self.docs.as_ref().and_then(|docs| docs.site_of(&diag)).or_else(|| diag.sel.first().cloned()),
            text: diag.message,
        })?);
        Ok(ops)
    }

    fn refuse(&mut self, label: String, op: &'static str, sample: Option<u64>, refused: Refused) -> (String, String) {
        if let Some(tag) = sample {
            self.commit_notes.remove(&tag);
        }
        self.refusals += 1;
        let Refused { text, site } = refused;
        let reason = crate::refusal::reason(&text);
        self.last_refusal = Some(Arc::new(Refusal {
            seq: self.refusals, label: label.clone(), op, text: text.clone(), reason, site, sample,
        }));
        (label, text)
    }

    /// Share the exact published refusal with the command's caller.
    fn refusal_outcome(&self, ticket: Option<Ticket>, outcomes: &mut Vec<(Ticket, Outcome)>) {
        if let (Some(ticket), Some(refusal)) = (ticket, &self.last_refusal) {
            outcomes.push((ticket, Outcome::Refused(refusal.clone())));
        }
    }

    /// The board statements just written; O(added statements) per commit.
    fn board_created(&self) -> (Vec<Sel>, Vec<Sel>) {
        let Some(entry) = self.log.entries.last() else { return (vec![], vec![]) };
        entry.text.adds.iter().map(|(sel, doc, span)| {
            (sel.clone(), Sel::Source { doc: *doc, start: span.start, end: span.end })
        }).unzip()
    }

    /// Publish the entry and answer its caller. Typed edits supply their document's circuit
    /// change directly. Other ticketed commits classify the logged ops in one pass.
    fn committed(
        &mut self, tag: Option<u64>, ticket: Option<Ticket>, created: Vec<Sel>, sites: Vec<Sel>,
        text_change: Option<CircuitChange>, outcomes: &mut Vec<(Ticket, Outcome)>,
    ) {
        if let Some((unit, notes)) = tag.and_then(|tag| self.commit_notes.remove(&tag)) {
            let kept = self.notes.entry(unit).or_default();
            kept.retain(|v| notes.binary_search_by_key(&v.id, |note| note.id).is_err());
            kept.extend(notes);
            self.notes_sorted = self.notes.values().flatten().cloned().collect();
            self.notes_sorted.sort_unstable_by_key(|v| v.id);
            self.notes_sorted.dedup_by_key(|v| v.id);
        }
        let ticket = ticket.or_else(|| tag.filter(|tag| *tag >= JobId(0).tag())
            .and_then(|tag| self.preview_tickets.remove(&tag).map(|(ticket, _)| ticket)));
        let Some(entry) = self.log.entries.last() else { return };
        self.last_commit = Some(Arc::new(Committed { seq: entry.seq, tag, by: entry.by, created: created.clone() }));
        if let Some(ticket) = ticket {
            let targets = op_targets(&entry.ops);
            let circuit = text_change.unwrap_or_else(|| circuit_change(&entry.ops));
            outcomes.push((ticket, Outcome::Done { seq: entry.seq, label: entry.label.clone(), created, sites, by: entry.by, targets, circuit }));
        }
    }

    /// With documents: the model first (the validator), then the splice of the commit's board ops
    /// into the text. A splice that fails restores the design as it was: an O(1) rollback,
    /// generations and item allocator included. System ops stay model-only (gap G1).
    fn commit(&mut self, ops: Vec<DesignOp>, label: String, expect: Option<(BoardId, ContentHash)>, by: By) -> Result<Batch, Refused> {
        self.check_expect(expect)?;
        let before = self.docs.is_some().then(|| self.design.clone());
        let mut batch = apply_ops(Arc::make_mut(&mut self.design), &ops, false).map_err(|(i, error)| Refused {
            text: format!("{error:?}"), site: self.docs.as_ref().and_then(|d| op_site(d, &ops[i])),
        })?;
        let mut text = TextStep::default();
        let mut text_inverse = TextStep::default();
        let mut epochs = Vec::new();
        if let (Some(docs), Some(before)) = (&mut self.docs, before) {
            let is_board = |op: &DesignOp| matches!(op, DesignOp::Board { .. });
            let board: Option<Vec<DesignOp>> = (!ops.iter().all(is_board)).then(|| ops.iter().filter(|op| is_board(op)).cloned().collect());
            let board = board.as_deref().unwrap_or(&ops);
            if !board.is_empty() {
                match docs.commit(&before, board) {
                    Ok((forward, inverse)) => (text, text_inverse) = (forward, inverse),
                    Err(diag) => {
                        let site = docs.site_of(&diag);
                        self.design = before;
                        return Err(Refused { text: diag.message, site });
                    }
                }
                for op in board {
                    if let DesignOp::Board { unit, .. } = op {
                        if !epochs.iter().any(|(u, _)| u == unit) {
                            epochs.push((*unit, docs.epoch(*unit)));
                        }
                    }
                }
            }
        }
        self.typing = None;
        // A batch with board ops is a board entry (its system ops ride along on the model, G1);
        // only a batch of system ops alone is a system entry.
        let kind = if ops.iter().any(|op| matches!(op, DesignOp::Board { .. })) || ops.is_empty() { EntryKind::Board } else { EntryKind::System };
        let seq = self.steps.fetch_add(1, SeqCst) + 1;
        self.log.push(OpEntry {
            kind, group: None, by, seq,
            label, ops, inverse: std::mem::take(&mut batch.inverse),
            text, text_inverse, epochs,
        });
        self.logged.fetch_max(seq, SeqCst);
        Ok(batch)
    }

    /// Check a proposal's board snapshot before either the model or verification accepts it.
    /// Only samples naming an expected hash use the cache in [`Self::board_hash`].
    fn base_moved(&self, sample: &Sample) -> Option<Refused> {
        let (unit, _) = sample.expect?;
        self.check_expect(sample.expect).is_err().then(|| {
            let name = self.design.unit(unit).map_or_else(|| format!("unit {}", unit.0), |found| found.name.clone());
            Refused { text: format!("{name}'s board changed after this edit was planned: nothing was applied"), site: None }
        })
    }

    /// Cost: O(units) lookup; the hash is cached per board `Arc`. A gesture's base never waits for fills.
    fn base_of(&self, unit: BoardId) -> Option<Base> {
        let board = self.design.unit(unit)?.board.as_ref()?.clone();
        let results = self.results.boards.iter().find(|b| b.unit == unit)?;
        Some(Base {
            unit,
            board,
            rules: results.rules.clone(),
            conn: results.conn.clone(),
            fills: results.fills.clone(),
            delta: None,
            hash: self.board_hash(unit)?,
            exact: results.pending.zones.is_empty(),
        })
    }

    /// The committed board's hash: one pointer comparison when unchanged, after O(units) lookup.
    /// Each board change costs one `content_hash` (O(items)); the cache holds one entry per unit.
    fn board_hash(&self, unit: BoardId) -> Option<ContentHash> {
        let board = self.design.unit(unit)?.board.as_ref()?;
        let mut hashes = self.board_hashes.borrow_mut();
        if let Some(entry) = hashes.iter_mut().find(|(id, _, _)| *id == unit) {
            if entry.1.as_ptr() == Arc::as_ptr(board) {
                return Some(entry.2);
            }
            let hash = board.content_hash();
            *entry = (unit, Arc::downgrade(board), hash);
            return Some(hash);
        }
        let hash = board.content_hash();
        hashes.push((unit, Arc::downgrade(board), hash));
        Some(hash)
    }

    /// Refuse ops planned on unit circuits that are not the committed ones any more: their
    /// positional ids may name other parts, pins or nets now (Fable H4). O(stamped units).
    fn check_circuits(&self, circuits: &[(BoardId, Weak<Circuit>)]) -> Result<(), Refused> {
        for (unit, planned) in circuits {
            let found = self.design.unit(*unit);
            if found.is_none_or(|found| !std::ptr::eq(Arc::as_ptr(&found.circuit), planned.as_ptr())) {
                let name = found.map_or_else(|| format!("unit {}", unit.0), |found| found.name.clone());
                return Err(Refused {
                    text: format!("{name}'s circuit changed after this edit was planned, so its parts, pins and nets may be others now: nothing was applied"),
                    site: None,
                });
            }
        }
        Ok(())
    }

    fn check_expect(&self, expect: Option<(BoardId, ContentHash)>) -> Result<(), Refused> {
        if let Some((unit, expected)) = expect {
            let actual = self.board_hash(unit);
            if actual != Some(expected) {
                let text = format!("board {unit:?} content hash mismatch: expected {expected:?}, found {actual:?}");
                return Err(Refused { text, site: None });
            }
        }
        Ok(())
    }

    /// O(units) pointer comparisons when unchanged; only changed circuits issue rows.
    fn sync_sheets(&mut self) {
        if self.sheets.len() == self.design.units.len() && self.sheets.iter().zip(&self.design.units)
            .all(|(sheet, unit)| sheet.unit == unit.id && Arc::ptr_eq(&sheet.circuit, &unit.circuit)) {
            return;
        }
        let mut held: HashMap<_, _> = std::mem::take(&mut self.sheets).into_iter().map(|s| (s.unit, s)).collect();
        for unit in &self.design.units {
            let sheet = if let Some(mut sheet) = held.remove(&unit.id) {
                if !Arc::ptr_eq(&sheet.circuit, &unit.circuit) {
                    let rows = Arc::make_mut(&mut sheet.rows).sync(&unit.circuit);
                    sheet.index = Arc::new(SheetIndex { rows, count: sheet.rows.count() });
                    sheet.circuit = unit.circuit.clone();
                }
                sheet
            } else {
                let mut rows = SheetRows::new();
                let index = Arc::new(SheetIndex { rows: rows.sync(&unit.circuit), count: rows.count() });
                Sheet { unit: unit.id, rows: Arc::new(rows), circuit: unit.circuit.clone(), index }
            };
            self.sheets.push(sheet);
        }
        // Departed units and their historical keys drop here, on the strand.
    }

    /// Per result: O(units) shared metadata and O(changed units' rows) cloning/sync; no snapshot row work.
    fn preview_sheets(&self, design: &Design) -> Vec<Sheet> {
        design.units.iter().map(|unit| {
            let old = self.sheets.iter().find(|sheet| sheet.unit == unit.id);
            if let Some(sheet) = old.filter(|sheet| Arc::ptr_eq(&sheet.circuit, &unit.circuit)) {
                return sheet.clone();
            }
            let mut rows = old.map_or_else(SheetRows::new, |sheet| (*sheet.rows).clone());
            let index = Arc::new(SheetIndex { rows: rows.sync(&unit.circuit), count: rows.count() });
            Sheet { unit: unit.id, rows: Arc::new(rows), circuit: unit.circuit.clone(), index }
        }).collect()
    }

    /// Merge checks, case findings and session notes in linear time once the live notes are sorted.
    fn make_snapshot(&self, mut stale: StageFlags) -> Arc<DesignSnapshot> {
        if self.eval_blocked() || self.preview_job.is_some() {
            stale = stale.with(StageFlags::EVAL);
        }
        let mut conn = Vec::new();
        let mut rules = Vec::new();
        let mut fills = Vec::new();
        for b in &self.results.boards {
            conn.push(Tagged { unit: Some(b.unit), of: StateOf::Committed, generation: b.generation, value: b.conn.clone() });
            rules.push(Tagged { unit: Some(b.unit), of: StateOf::Committed, generation: b.generation, value: b.rules.clone() });
            fills.push(Tagged { unit: Some(b.unit), of: StateOf::Committed, generation: b.generation, value: b.fills.clone() });
            if !b.pending.zones.is_empty() {
                stale = stale.with(StageFlags::FILLS);
            }
        }
        if let Some(preview) = &self.preview {
            let mut committed = self.results.boards.iter().peekable();
            for b in &preview.results.boards {
                while committed.peek().is_some_and(|c| c.unit < b.unit) {
                    committed.next();
                }
                if committed.peek().is_some_and(|c| c.unit == b.unit && Arc::ptr_eq(&c.conn, &b.conn)) {
                    continue;
                }
                conn.push(Tagged { unit: Some(b.unit), of: StateOf::Preview, generation: b.generation, value: b.conn.clone() });
                rules.push(Tagged { unit: Some(b.unit), of: StateOf::Preview, generation: b.generation, value: b.rules.clone() });
                fills.push(Tagged { unit: Some(b.unit), of: StateOf::Preview, generation: b.generation, value: b.fills.clone() });
            }
        }
        let (of, results) = self.preview.as_ref().map_or((StateOf::Committed, &self.results), |p| (StateOf::Preview, &p.results));
        let mut awaiting: Vec<_> = results.boards.iter().flat_map(|b| {
            b.drc.iter().filter(move |v| engines::awaits_refill(b, v)).map(|v| v.id)
        }).collect();
        awaiting.sort_unstable();
        awaiting.dedup();
        let checks = engines::violations(results);
        let live = self.preview.as_ref().and_then(|p| sharp_notes(&p.sample));
        // Checks win ties. A live note for the same track shows its newest corner.
        let mut extra = [self.case_sorted.as_slice(), live.as_ref().map_or(&[][..], |(_, notes)| notes.as_slice()),
            self.notes_sorted.as_slice()];
        let count: usize = extra.iter().map(|list| list.len()).sum();
        let violations = if count == 0 {
            checks
        } else {
            let mut out = Vec::with_capacity(checks.len() + count);
            let mut checks = checks.into_iter().peekable();
            while let Some(finding) = extra.iter().filter_map(|list| list.first()).min_by_key(|v| v.id) {
                let id = finding.id;
                while let Some(check) = checks.next_if(|check| check.id < id) {
                    out.push(check);
                }
                if checks.peek().is_none_or(|check| check.id != id) {
                    out.push(finding.clone());
                }
                for list in &mut extra {
                    if list.first().is_some_and(|v| v.id == id) {
                        *list = &list[1..];
                    }
                }
            }
            out.extend(checks);
            out
        };
        Arc::new(DesignSnapshot {
            design: self.design.clone(),
            preview: self.preview.as_ref().map(|p| p.design.clone()),
            preview_tag: self.preview.as_ref().map(|p| p.sample.tag),
            preview_by: self.preview.as_ref().map(|p| p.sample.by),
            created: self.preview.as_ref().map_or_else(Vec::new, |live| live.created.clone()),
            last_commit: self.last_commit.clone(),
            sheets: self.sheets.iter().map(|sheet| Tagged {
                unit: Some(sheet.unit), of: StateOf::Committed, generation: self.design.generation, value: sheet.index.clone(),
            }).chain(self.preview.iter().flat_map(|preview| preview.sheets.iter().filter(|sheet| {
                self.design.unit(sheet.unit).is_none_or(|unit| !Arc::ptr_eq(&unit.circuit, &sheet.circuit))
            }).map(move |sheet| Tagged {
                unit: Some(sheet.unit), of: StateOf::Preview, generation: preview.design.generation, value: sheet.index.clone(),
            }))).collect(),
            conn, rules, fills,
            sysnets: Some(Tagged {
                unit: None, of: StateOf::Committed, generation: self.design.generation, value: self.results.sysnets.clone(),
            }),
            violations: Tagged { unit: None, of, generation: results.generation, value: Arc::new(violations) },
            awaiting: Arc::new(awaiting),
            last_refusal: self.last_refusal.clone(),
            export: vec![], proposal: self.preview.as_ref().and_then(|p| p.sample.proposal.clone()), head: self.head.clone(), stale,
            job: None,
            source_map: self.docs.as_ref().map(|d| d.map().clone()),
            benches: self.docs.as_ref().map_or_else(Default::default, |d| d.bench_views().clone()),
            docs: self.docs.as_ref().map(|d| d.view().clone()),
            catalog: self.docs.as_ref().map(|d| d.catalog().clone()),
        })
    }
}

/// Per result: unit lookups, changed components' mesh resolution, and O(items) per replaced board.
/// A changed unit's bodies resolve through `mesh_keys` in order: the plate's files, then the part
/// library's models (a resolved key is no project path, so a later map leaves it).
fn swap_evaluated(
    current: &Arc<Design>, mut evaluated: Design, same_boards: &[BoardId], mesh_keys: [&BTreeMap<String, String>; 2],
) -> (Arc<Design>, Vec<(BoardId, BoardDelta)>, Vec<BoardId>) {
    let mut design = (**current).clone();
    let mut changed = false;
    let mut replaced = Vec::new();
    let mut deltas = BTreeMap::new();
    for unit in &mut evaluated.units {
        if current.unit(unit.id).is_none_or(|old| !Arc::ptr_eq(&old.circuit, &unit.circuit)) {
            for keys in mesh_keys {
                unit.resolve_meshes(keys);
            }
        }
        if let Some(old) = design.units.iter_mut().find(|old| old.id == unit.id) {
            if !Arc::ptr_eq(&old.circuit, &unit.circuit) {
                old.circuit = unit.circuit.clone();
                changed = true;
            }
            if old.supplied != unit.supplied {
                old.supplied = std::mem::take(&mut unit.supplied);
                changed = true;
            }
            changed |= old.name != unit.name || old.kind != unit.kind;
            old.name = unit.name.clone();
            old.kind = unit.kind.clone();
            if !same_board(&old.board, &unit.board) && !same_boards.contains(&unit.id) {
                let delta = match (&old.board, &unit.board) {
                    (Some(old), Some(new)) => BoardDelta::between(old, new),
                    _ => BoardDelta {
                        global: true,
                        bbox: old.board.as_ref().map_or(Default::default(), |board| board.outline_bbox())
                            .union(unit.board.as_ref().map_or(Default::default(), |board| board.outline_bbox())),
                        ..Default::default()
                    },
                };
                let board = unit.board.as_ref().map(|board| {
                    let mut board = (**board).clone();
                    board.generation = old.board.as_ref().map_or(0, |old| old.generation + 1);
                    board.next_item = board.next_item.max(old.board.as_ref().map_or(0, |old| old.next_item));
                    Arc::new(board)
                });
                old.board = board;
                fold(&mut deltas, [(unit.id, delta)]);
                replaced.push(unit.id);
                changed = true;
            }
        } else {
            let mut unit = unit.clone();
            if let Some(board) = &mut unit.board {
                Arc::make_mut(board).generation = 0;
            }
            let bbox = unit.board.as_ref().map_or(Default::default(), |board| board.outline_bbox());
            fold(&mut deltas, [(unit.id, BoardDelta { global: true, bbox, ..Default::default() })]);
            replaced.push(unit.id);
            design.units.push(unit);
            changed = true;
        }
    }
    // A unit the evaluation no longer has lost its circuit document (removed, or its add undone):
    // it leaves the design. A unit whose text has mistakes is still in `evaluated` (its last good
    // lowering, `UnitState::Kept`). Links and mounts naming it stay with the carry below, so an
    // undo brings them back; validation names them while it is gone. O(units^2), a handful.
    let before = design.units.len();
    design.units.retain(|unit| evaluated.units.iter().any(|new| new.id == unit.id));
    changed |= design.units.len() != before;
    // THE CARRY (G1): step 6 deletes this block when system text is re-lowered here.
    // Links, assembly, bindings and each existing unit's factory remain the engine's.
    design.links = current.links.clone();
    design.assembly = current.assembly.clone();
    design.bindings = current.bindings.clone();
    for unit in &mut design.units {
        if let Some(old) = current.unit(unit.id) {
            unit.factory = old.factory.clone();
        }
    }
    // A re-lowered board keeps the derived items its old board had (a case's keepouts): no text
    // writes them, and their source replaces them when it next publishes. O(items) per replaced
    // board to find them, plus `Design::set_derived` per source.
    for &unit in &replaced {
        if design.unit(unit).is_none_or(|u| u.board.is_none()) {
            continue;
        }
        for from in current.derived_sources(unit) {
            match design.set_derived(unit, &from, current.derived(unit, &from)) {
                Ok(Some(delta)) => fold(&mut deltas, [(unit, delta)]),
                Ok(None) => {}
                Err(error) => makepad_widgets_core::log!("electronics: the derived items of {from:?} were not kept: {error:?}"),
            }
        }
    }
    if changed {
        design.generation += 1;
    }
    let design = if changed { Arc::new(design) } else { current.clone() };
    (design, deltas.into_iter().collect(), replaced)
}

struct Batch {
    changed: bool,
    inverse: Vec<DesignOp>,
    deltas: Vec<(BoardId, BoardDelta)>,
}

/// Whether `sel`, mapped in the new evaluation, names something the edit made. An evaluation
/// renumbers parts and nets (a part inserted mid-circuit takes an old part's id), so a part counts
/// by its path and a net by its name; anything else by its id. O(parts or nets) of one unit.
fn is_new(sel: &Sel, before: &SourceMap, old: &Design, new: &Design) -> bool {
    match sel {
        Sel::Comp(at) => match new.unit(at.board).and_then(|unit| unit.circuit.comps.get(at.comp.ix())) {
            Some(comp) => old.unit(at.board).is_none_or(|unit| unit.circuit.comp(&comp.path).is_none()),
            None => before.locate(sel).is_none(),
        },
        Sel::Net(at) => match new.unit(at.board).and_then(|unit| unit.circuit.nets.get(at.net.ix())) {
            Some(net) => old.unit(at.board).is_none_or(|unit| unit.circuit.net(&net.name).is_none()),
            None => before.locate(sel).is_none(),
        },
        _ => before.locate(sel).is_none(),
    }
}

/// Resolve new selections before adoption; O(written ranges + map entries of their documents) per
/// commit, and O(parts or nets) for each entry inside the written ranges.
fn created_in(before: &SourceMap, after: &SourceMap, written: &[SourceRange], old: &Design, new: &Design) -> (Vec<Sel>, Vec<Sel>) {
    let sites = written.iter().map(|range| Sel::Source {
        doc: range.doc, start: range.span.start, end: range.span.end,
    }).collect();
    let mut docs = Vec::<(DocId, Vec<&SourceRange>)>::new();
    let mut indices = HashMap::new();
    for range in written {
        let index = *indices.entry(range.doc).or_insert_with(|| {
            docs.push((range.doc, Vec::new()));
            docs.len() - 1
        });
        docs[index].1.push(range);
    }
    let mut created = Vec::new();
    for (doc, mut ranges) in docs {
        // Splices arrive in op order; the walk below needs them by start.
        ranges.sort_unstable_by_key(|range| range.span.start);
        let mut ranges = ranges.into_iter().peekable();
        let mut end = None::<u32>;
        for (sel, range) in after.of_doc(doc) {
            while ranges.peek().is_some_and(|written| written.span.start <= range.span.start) {
                let written = ranges.next().expect("the next written range starts before this entry");
                end = Some(end.map_or(written.span.end, |end| end.max(written.span.end)));
            }
            if end.is_some_and(|end| range.span.end <= end) && is_new(&sel, before, old, new) {
                created.push(sel);
            }
        }
    }
    (created, sites)
}

/// Board identity includes absence, so equal evaluated boards keep their engine allocation.
fn same_board(a: &Option<Arc<Board>>, b: &Option<Arc<Board>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// Where the statement a board op edits is written, as a `Sel::Source`: the item, else its board.
fn op_site(docs: &Docs, op: &DesignOp) -> Option<Sel> {
    let DesignOp::Board { unit, op } = op else { return None };
    let item = match op {
        BoardOp::Remove(id) | BoardOp::Detach(id) => Some(*id),
        BoardOp::Move { item, .. } | BoardOp::SetField { item, .. } => Some(*item),
        BoardOp::Replace(item) => Some(item.id()),
        _ => None,
    };
    item.and_then(|id| docs.site(&Sel::Item(*unit, id))).or_else(|| docs.site(&Sel::Unit(*unit)))
}

/// What `ops` edit in place and keep, each once: a part whose value, field or reference
/// changes, a board item moved, set, replaced or detached. Ops that add, remove or rewire are
/// left out: an evaluation may number parts and nets anew after them, and an added item is the
/// commit's `created`. O(ops).
fn op_targets(ops: &[DesignOp]) -> Vec<Sel> {
    let mut out: Vec<Sel> = Vec::new();
    for op in ops {
        let sel = match op {
            DesignOp::Circuit { unit, op: CircuitOp::SetValue { comp, .. } | CircuitOp::SetField { comp, .. } | CircuitOp::SetRef { comp, .. } } => {
                Sel::Comp(CompRef { board: *unit, comp: *comp })
            }
            DesignOp::Board { unit, op: BoardOp::Move { item, .. } | BoardOp::SetField { item, .. } | BoardOp::Detach(item) } => Sel::Item(*unit, *item),
            DesignOp::Board { unit, op: BoardOp::Replace(item) } => Sel::Item(*unit, item.id()),
            _ => continue,
        };
        if !out.contains(&sel) {
            out.push(sel);
        }
    }
    out
}

/// The largest change to the simulated circuit across the committed ops.
fn circuit_change(ops: &[DesignOp]) -> CircuitChange {
    let mut change = CircuitChange::None;
    for op in ops {
        match op {
            DesignOp::Board { .. } => {}
            DesignOp::Circuit {
                op: CircuitOp::Hint { .. } | CircuitOp::NetStyle { .. } | CircuitOp::SetField { .. } | CircuitOp::SetRef { .. } | CircuitOp::SetBody { .. }, ..
            } => {}
            DesignOp::Circuit { op: CircuitOp::SetValue { .. }, .. } => change = CircuitChange::Values,
            DesignOp::Circuit { .. } | DesignOp::Bench { .. } | DesignOp::Sys(_) | DesignOp::File(_) => return CircuitChange::Structure,
        }
    }
    change
}

/// The refused edit's kind; an import's files ride along with the edit that uses them.
fn op_kind(ops: &[DesignOp]) -> &'static str {
    match ops.iter().find(|op| !matches!(op, DesignOp::File(_))).or(ops.first()) {
        Some(DesignOp::Board { .. }) => "board",
        Some(DesignOp::Circuit { .. }) => "circuit",
        Some(DesignOp::Bench { .. }) => "bench",
        Some(DesignOp::Sys(_)) | None => "system",
        Some(DesignOp::File(_)) => "file",
    }
}

/// The host's keys with a verified step's added assets, so a preview resolves its new bodies
/// before they are committed: borrowed when the step adds none, else one O(keys) clone per
/// preview result (an import's preview).
fn with_assets<'a>(keys: &'a BTreeMap<String, String>, assets: &[AssetStep]) -> std::borrow::Cow<'a, BTreeMap<String, String>> {
    if !assets.iter().any(|asset| asset.available) {
        return std::borrow::Cow::Borrowed(keys);
    }
    let mut keys = keys.clone();
    for asset in assets.iter().filter(|asset| asset.available) {
        keys.insert(asset.name.clone(), asset.key.clone());
    }
    std::borrow::Cow::Owned(keys)
}

fn fold(into: &mut BTreeMap<BoardId, BoardDelta>, deltas: impl IntoIterator<Item = (BoardId, BoardDelta)>) {
    for (unit, delta) in deltas {
        engines::merge_delta(into.entry(unit).or_default(), &delta);
    }
}

/// Apply a command atomically (`Design::apply_all`): each run of board ops for one unit is one
/// batch, so `check`, a preview and the commit apply the same copper. Rollback restores
/// generations but preserves the monotonic item allocator; inverses are host ops, and their
/// deltas are discarded. An error names the index of the op that failed.
fn apply_ops(design: &mut Design, ops: &[DesignOp], host: bool) -> Result<Batch, (usize, DesignOpError)> {
    let done = design.apply_all(ops, host)?;
    let mut deltas = BTreeMap::new();
    for applied in &done {
        fold(&mut deltas, engines::deltas_of(design, applied));
    }
    Ok(Batch {
        changed: !ops.is_empty(),
        inverse: done.iter().rev().flat_map(DesignApplied::inverse).collect(),
        deltas: deltas.into_iter().collect(),
    })
}
