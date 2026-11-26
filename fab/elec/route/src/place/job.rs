//! `elec.place`'s job (`PlaceMode::Place`), on a pool thread. The parts the job brings are added
//! to a copy of the base board, every part in scope is seeded by routing demand
//! (`seed_watched`), and each seeding kept becomes a proposal: the brought parts' adds, the pose
//! fields of the parts that moved, the removal of the unlocked copper that served their pads, and
//! the reference labels near them arranged again as a drop arranges them (`arrange_references`).
//! The removal is the `branches` from each track a moved pad anchors: a part whose pose changes
//! voids the routes laid to its old spot, and the router lays them again, while the rest of each
//! net, locked copper and pours stay. Hand-drawn copper goes too, counted by part
//! (`PlaceMetrics::hand`), and the commit is one undo step, which brings it back. The worker then
//! removes the router's copper the parts' new spots collide with, by the same walk (see
//! `RouteWorker`'s `publish_place`). The first seeding is offered as soon as it stands, each demand
//! round kept replaces it, then each group of repeated blocks the seed arranges alike (its conform),
//! and the last is offered again as final, with the conform's rows (`PlaceMetrics::conformed`).
//! The case's optional plan checks the final offer. A refusal allows up to two more complete
//! seeds with the plan's revised rooms. Those trials stay private until their winner is known.
//! While conform runs the job's phase names the group (`JobPhase::Match`). A cancel ends the
//! rounds or groups at the next one and the final is the last offered: every offer is legal.
//!
//! Cost: one board clone with the brought parts added, the problem's build and the seed's own (see
//! `seed`); each offer adds O(movables + tracks + vias) for its ops, O(tracks) per part placed
//! again for its branches, O(pins) for its wirelength, and one more board clone with the ops
//! applied for the label pass near the parts it moved.

use super::{seed_watched, ConformRow, Cost, PlaceProblem, Pose, SeedOpts, SeedRound, SeedWatch};
use crate::progress::{JobPhase, Progress};
use crate::request::{PlaceJob, PlacePlanner, PlanReport, PlanVerdict};
use crate::solution::{Conformed, HandCopper, PlaceMetrics};
use fab_elec_board::placement::placed_courtyard;
use fab_elec_board::text::{arrange_references, LABEL_NEAR};
use fab_elec_board::{Anchor, Board, BoardOp, Field, Item, ItemKind, Origin, Placed, PortRoom};
use fab_elec_core::{BBox, BlockId, CompId, ItemId, PadIx};
use fab_elec_circuit::{Circuit, Intent, Target};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// One proposal of a place job: its ops against the base and its metrics.
#[derive(Clone, Debug)]
pub(crate) struct PlaceOffer {
    pub ops: Vec<BoardOp>,
    pub metrics: PlaceMetrics,
    /// A winning re-place carries the geometry it used for the worker's final checks.
    pub rooms: Option<Vec<PortRoom>>,
}

impl PlaceJob {
    /// Apply a placement proposal to a scratch board and check its carried keepouts at the
    /// proposed poses. The published edit contains only the user's board operations.
    pub fn check_proposal(&self, base: &Board, ops: &[BoardOp], state: &crate::check::State<'_>)
        -> Result<(Board, crate::check::Outcome), fab_elec_board::OpError>
    {
        self.check_with_rooms(base, ops, state, &self.rooms)
    }

    pub(crate) fn check_with_rooms(&self, base: &Board, ops: &[BoardOp], state: &crate::check::State<'_>, rooms: &[PortRoom])
        -> Result<(Board, crate::check::Outcome), fab_elec_board::OpError>
    {
        use crate::check::{check, new_or_worse, settle, FindingNets, Outcome, Settled};
        use fab_elec_board::drc::drc_indexed;
        use fab_elec_board::placement::projected_carried_keepouts;
        use fab_elec_board::BoardDelta;
        use fab_elec_circuit::CheckKind;
        use fab_elec_core::Sel;

        let mut board = base.clone();
        if !rooms.iter().any(|p| p.keepout.is_some()) {
            return check(&mut board, ops, state).map(|out| (board, out))
        }
        let Settled { applied, fills, mut index, mut change, conn } = settle(&mut board, ops, state)?;
        let moving = PlaceProblem::new(&board, &self.scope).movables.iter().map(|p| p.item).collect();
        let (projected, removed) = projected_carried_keepouts(&board, &moving, rooms);
        let mut owners = BTreeMap::new();
        let mut updates: Vec<_> = removed.into_iter().map(BoardOp::Remove).collect();
        for (owner, k) in projected {
            owners.insert(k.id, owner);
            match board.locate(k.id) {
                Some((ItemKind::Keepout, i)) if board.keepouts[i] == k => {}
                Some(_) => updates.push(BoardOp::Replace(Item::Keepout(k))),
                None => updates.push(BoardOp::Add(Item::Keepout(k))),
            }
        }
        // These areas forbid only bodies. They change the spatial index and the region to
        // recheck, while the placement's fills and connectivity are already exact.
        let generation = board.generation;
        let projected = board.apply_batch(&updates, true).map_err(|(_, e)| e)?.delta;
        board.generation = generation;
        change.absorb(&index.update(&board, &fills, &projected, &[]));
        let region = BoardDelta { bbox: applied.delta.bbox.union(projected.bbox), global: applied.delta.global, ..Default::default() };
        let mut violations = drc_indexed(state.violations, &index, &board, &conn, state.rules, &region, &[]);
        // On this snapshot each carried area has a board item id of its own. As in the
        // placement index, its owner may stand in it. All other bodies still count.
        violations.retain(|v| v.kind != CheckKind::Keepout || !v.items.iter().any(|s| {
            let Sel::Item(_, id) = s else { return false };
            owners.get(id).is_some_and(|&owner| v.items.contains(&Sel::Item(board.unit, owner)))
        }));
        let new = new_or_worse(state.violations, &FindingNets::of(base, state.conn), &violations, &FindingNets::of(&board, &conn));
        Ok((board, Outcome { applied, fills, index, change, conn, violations, new }))
    }
}

/// Run `job` on `base` (the unit's committed board), handing each proposal to `offer` with its
/// step (the seeding's round) and whether it is the final one. The brought parts take ids from the
/// base's `next_item` on, and one the base already places is left out: the host made them on the
/// design as it resolved the call, and the base is the committed board when the job starts. `Err`
/// when a brought part still cannot go on the base; nothing is offered then.
pub(crate) fn run(
    base: &Board, job: &PlaceJob, progress: &Arc<Progress>, offer: &mut dyn FnMut(u32, Arc<PlaceOffer>, bool), pool: Arc<dyn crate::worker::Spawn>,
) -> Result<(), String> {
    let mut board = base.clone();
    let mut add = Vec::with_capacity(job.add.len());
    for part in job.add.iter().filter(|p| base.placed_of(p.comp).is_none()) {
        let mut part = part.clone();
        part.id = board.alloc_id();
        board.apply(&BoardOp::Add(Item::Placed(part.clone())))
            .map_err(|why| format!("{} cannot go on the board: {why}", part.refdes))?;
        add.push(part);
    }
    let problem = PlaceProblem::new(&board, &job.scope);
    let start: Vec<Pose> = problem.movables.iter().map(|m| m.start).collect();
    progress.set_total(problem.movables.len() as u32);
    progress.set_phase(JobPhase::Seed { round: 0 });
    let mut watch = Watch { base, held: job.held, add: &add, problem: &problem, start: &start, progress, offer,
        last: None, matching: false, replacing: false };
    let mut pipeline = |rooms: Vec<PortRoom>, pass: u32| {
        watch.matching = false;
        watch.replacing = pass > 0;
        if watch.replacing { progress.set_phase(JobPhase::Replace { pass }); }
        let mut poses = start.clone();
        let opts = SeedOpts { demand: true, rooms, ..SeedOpts::default() };
        let report = seed_watched(&board, &job.circuit, &problem, &mut poses, &opts, &mut watch, Some(&pool));
        let (step, mut last) = watch.last.take()?;
        if !report.conformed.is_empty() {
            let metrics = &mut Arc::make_mut(&mut last).metrics;
            metrics.conformed = report.conformed.iter().map(conformed).collect();
            metrics.seat_guard = report.seat_guard.as_ref().map(|g| g.explanation());
        }
        if pass > 0 { Arc::make_mut(&mut last).rooms = Some(opts.rooms); }
        Some((step, last))
    };
    let mut best = pipeline(job.rooms.clone(), 0).ok_or("placement cancelled before its first proposal")?;
    if let Some(plan) = job.plan.as_ref().filter(|_| !progress.cancelled()) {
        let mut report = PlanReport::default();
        let checked = plan.open().and_then(|mut planner|
            guard(base, job, progress, &mut *planner, &mut best, &mut report, &mut pipeline));
        if let Err(why) = checked {
            report.note = Some(format!("the case plan could not check the placement: {}", why.lines().collect::<Vec<_>>().join(" ")));
        }
        Arc::make_mut(&mut best.1).metrics.case_plan = Some(report);
    }
    if best.1.rooms.is_some() {
        progress.set_placed(best.1.metrics.placed, best.1.metrics.no_room.len() as u32);
    }
    (watch.offer)(best.0, best.1, true);
    Ok(())
}

/// The successful first check returns the seed's own offer. After a refusal each trial
/// starts from the same base with the previous verdict's rooms. Only a checked winner is offered.
fn guard(
    base: &Board, job: &PlaceJob, progress: &Progress, planner: &mut dyn PlacePlanner,
    best: &mut (u32, Arc<PlaceOffer>), report: &mut PlanReport,
    pipeline: &mut impl FnMut(Vec<PortRoom>, u32) -> Option<(u32, Arc<PlaceOffer>)>,
) -> Result<(), String> {
    let (board, mut verdict) = plan_offer(base, job, &best.1, planner, report)?;
    if verdict.refused_pipes > 0 {
        let missing = best.1.metrics.no_room.clone();
        let broken = broken_nears(&board, &job.circuit);
        let refused = verdict.refused_pipes;
        let mut score = (missing.len(), broken, refused, best.1.metrics.overflow);
        let mut rooms = std::mem::take(&mut verdict.rooms);
        let mut error = None;
        for pass in 1..=2 {
            if progress.cancelled() { break }
            report.replaces = pass;
            let Some(trial) = pipeline(rooms, pass) else { break };
            if progress.cancelled() { break }
            let (board, mut next) = match plan_offer(base, job, &trial.1, planner, report) {
                Ok(result) => result,
                Err(why) => { error = Some(why); break }
            };
            rooms = std::mem::take(&mut next.rooms);
            let metrics = &trial.1.metrics;
            let near = broken_nears(&board, &job.circuit);
            let rank = (metrics.no_room.len(), near, next.refused_pipes, metrics.overflow);
            // No improvement may sacrifice a part or increase either kind of failure over the first run.
            if rank < score && near <= broken && next.refused_pipes <= refused
                && metrics.no_room.iter().all(|id| missing.contains(id)) {
                *best = trial;
                score = rank;
                verdict = next;
            }
            if verdict.refused_pipes == 0 { break }
        }
        let count = verdict.refused_pipes;
        let noun = if count == 1 { "pipe" } else { "pipes" };
        let action = if report.replaces > 0 { "placed again with the case's rooms: " } else { "" };
        let mut note = format!("{action}{count} refused {noun} left");
        if let Some(why) = error { verdict.refused.push(format!("The case plan could not check the re-place: {why}")); }
        if !verdict.refused.is_empty() {
            note.push_str(". ");
            note.push_str(&verdict.refused.join(" "));
        }
        report.note = Some(note.lines().collect::<Vec<_>>().join(" "));
    }
    report.made_pipes = Some(verdict.made_pipes);
    report.refused_pipes = Some(verdict.refused_pipes);
    Ok(())
}

/// Ask about the actual offer, including tray poses for parts that could not be placed.
fn plan_offer(base: &Board, job: &PlaceJob, offer: &PlaceOffer, planner: &mut dyn PlacePlanner, report: &mut PlanReport)
    -> Result<(Board, PlanVerdict), String>
{
    let mut board = base.clone();
    board.apply_all(&offer.ops).map_err(|error| error.to_string())?;
    report.calls += 1;
    let verdict = planner.plan(&board, &job.circuit)?;
    Ok((board, verdict))
}

/// Count near intents on a finished board, using the same courtyard boxes and pad centres as
/// the seats comparison. A block end can satisfy the intent through any of its placed members.
fn broken_nears(board: &Board, circuit: &Circuit) -> usize {
    let body = |comp| {
        let p = board.placed_of(comp)?;
        let bounds = fab_elec_board::placement::body_at(p, p.at, p.rot, p.side, true).bbox();
        Some(if bounds.is_empty() { BBox::point(p.at) } else { bounds })
    };
    let end = |target: &Target| -> Vec<BBox> {
        match *target {
            Target::Block(id) => circuit.block_comps(id).into_iter().filter_map(body).collect(),
            Target::Comp(id) => body(id).into_iter().collect(),
            Target::Pin(id, pin) => {
                let point = board.placed_of(id).and_then(|p| {
                    let no = circuit.comps.get(id.ix())?.part.pins.get(pin.ix())?.pads.first()?;
                    let index = p.footprint.pads.iter().position(|pad| &pad.no == no)?;
                    p.pad_at(PadIx(index as u16))
                });
                point.map(BBox::point).into_iter().collect()
            }
        }
    };
    circuit.intents.iter().filter(|intent| {
        let Intent::Near { a, b, max } = intent else { return false };
        let (a, b) = (end(a), end(b));
        !a.is_empty() && !b.is_empty() && !a.iter().any(|a| b.iter().any(|b| {
            // A closest point in b to a, independently on each axis.
            let point = fab_elec_core::P::new(a.min.x.clamp(b.min.x, b.max.x), a.min.y.clamp(b.min.y, b.max.y));
            *max >= 0 && a.dist2_to(point) <= i128::from(*max).pow(2)
        }))
    }).count()
}

/// The job's ear on its seed: each seeding kept becomes an offer, and the job's phase says which
/// round, or which group of repeated blocks the seed is matching.
struct Watch<'a> {
    base: &'a Board,
    held: u32,
    add: &'a [Placed],
    problem: &'a PlaceProblem,
    start: &'a [Pose],
    progress: &'a Arc<Progress>,
    offer: &'a mut dyn FnMut(u32, Arc<PlaceOffer>, bool),
    last: Option<(u32, Arc<PlaceOffer>)>,
    /// Conform has started: a group kept leaves the phase on the matching.
    matching: bool,
    /// Only the ranked winner of a re-place is eligible for publication.
    replacing: bool,
}

impl SeedWatch for Watch<'_> {
    fn cancelled(&self) -> bool { self.progress.cancelled() }
    fn comparison_progress(&self) -> Option<Arc<Progress>> {
        Some(if self.replacing { self.progress.private_trial() } else { self.progress.clone() })
    }

    fn kept(&mut self, round: &SeedRound) -> bool {
        let made = Arc::new(proposal(self.base, self.add, self.problem, self.start, round, self.held));
        let step = round.round as u32;
        if !self.replacing {
            self.progress.set_placed(made.metrics.placed, made.metrics.no_room.len() as u32);
            (self.offer)(step, made.clone(), false);
        }
        self.last = Some((step, made));
        if !self.matching && !self.replacing {
            self.progress.set_phase(JobPhase::Seed { round: step + 1 });
        }
        !self.progress.cancelled()
    }

    fn matching(&mut self, group: usize, of: usize, first: BlockId) -> bool {
        self.matching = true;
        if !self.replacing { self.progress.set_phase(JobPhase::Match { group: group as u32, of: of as u32, first }); }
        !self.progress.cancelled()
    }
}

/// A conform row as the job's metrics keep it: its decisions, without its timings.
fn conformed(row: &ConformRow) -> Conformed {
    Conformed {
        copies: row.blocks.clone(), parts_each: row.members_per_copy as u32, like: row.exemplar.map(|i| i as u32),
        status: row.status, overflow: row.delta_over, wirelength: row.delta_wirelength, moved: row.moved as u32,
        farthest: row.farthest_move,
    }
}

/// The ops and metrics of one seeding kept: the brought parts first (each on its spot, or without
/// room where the host put it, in the row under the board), then the pose fields of each part
/// that moved, then the removal of the unlocked copper their pads anchor (`branches`), then the
/// reference labels within `LABEL_NEAR` of the parts placed.
fn proposal(base: &Board, add: &[Placed], problem: &PlaceProblem, start: &[Pose], round: &SeedRound, held: u32) -> PlaceOffer {
    let no_room: BTreeSet<usize> = round.unplaced.iter().copied().collect();
    let movable = |id| problem.movables.iter().position(|m| m.item == id);
    let mut ops = Vec::with_capacity(add.len());
    for part in add {
        let mut part = part.clone();
        if let Some(m) = movable(part.id).filter(|m| !no_room.contains(m)) {
            let pose = round.poses[m];
            (part.at, part.rot, part.side) = (pose.at, pose.rot, pose.side);
        }
        ops.push(BoardOp::Add(Item::Placed(part)));
    }
    let mut moved: Vec<_> = add.iter().map(|p| p.id).collect();
    let mut again: Vec<(ItemId, CompId)> = Vec::new();
    for (m, (from, to)) in start.iter().zip(round.poses).enumerate() {
        let item = problem.movables[m].item;
        let Some(placed) = base.placed_item(item) else { continue };
        if from == to || no_room.contains(&m) { continue }
        moved.push(item);
        again.push((item, placed.comp));
        if from.side != to.side {
            ops.push(BoardOp::SetField { item, field: Field::Side(to.side) });
        }
        if from.rot != to.rot {
            ops.push(BoardOp::SetField { item, field: Field::Rot(to.rot) });
        }
        if from.at != to.at {
            ops.push(BoardOp::SetField { item, field: Field::At(to.at) });
        }
    }
    // Part by part, so each hand-drawn piece is counted against the part it was laid to.
    let mut rip: BTreeSet<ItemId> = BTreeSet::new();
    let mut hand = Vec::new();
    for &(item, comp) in &again {
        let anchored = base.tracks.iter()
            .filter(|t| t.ends.iter().any(|e| matches!(e, Anchor::Pad { placed, .. } if *placed == item)))
            .map(|t| t.id);
        let mine: Vec<ItemId> = branches(base, anchored, Takes::Unlocked).into_iter().filter(|id| !rip.contains(id)).collect();
        let (mut tracks, mut vias) = (0, 0);
        for &id in &mine {
            match base.locate(id) {
                Some((ItemKind::Track, i)) if matches!(base.tracks[i].origin, Origin::Authored) => tracks += 1,
                Some((ItemKind::Via, i)) if matches!(base.vias[i].origin, Origin::Authored) => vias += 1,
                _ => {}
            }
        }
        if tracks + vias > 0 {
            hand.push(HandCopper { comp, tracks, vias });
        }
        rip.extend(mine);
    }
    let ripped = rip.len() as u32;
    ops.extend(rip.into_iter().map(BoardOp::Remove));
    let mut after = base.clone();
    if after.apply_all(&ops).is_ok() {
        let near: Vec<_> = moved.iter().filter_map(|&id| after.placed_item(id)).map(|p| p.bbox().inflate(LABEL_NEAR)).collect();
        let parts: Vec<_> = after.placed.iter().filter(|p| near.iter().any(|b| b.intersects(&p.bbox()))).map(|p| p.id).collect();
        ops.extend(arrange_references(&after, &parts, &[]).ops);
    }
    let courts = after.placed.iter().fold(BBox::EMPTY, |b, p| b.union(placed_courtyard(p).bbox()));
    let area = |b: BBox| i128::from(b.width()) * i128::from(b.height());
    let outline = area(after.outline_bbox());
    let share = (1000 * area(courts) + outline / 2).checked_div(outline).unwrap_or(0);
    let metrics = PlaceMetrics {
        held,
        box_permille: u32::try_from(share).unwrap_or(u32::MAX),
        placed: (problem.movables.len() - no_room.len()) as u32,
        no_room: no_room.iter().map(|&m| problem.movables[m].comp).collect(),
        wirelength: Cost::new(problem, round.poses).terms().wirelength,
        overflow: round.overflow.unwrap_or(0),
        rounds: round.round as u32,
        ripped,
        hand,
        fix_rounds: 0,
        // the job adds the conform's rows to its final offer
        conformed: Vec::new(),
        seat_guard: None,
        case_plan: None,
    };
    PlaceOffer { ops, metrics, rooms: None }
}

/// Which copper a walk of `branches` may remove: unlocked, and of an origin it names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Takes {
    /// The router's copper only.
    Solved,
    /// The router's and the author's: copper anchored to a part placed again was laid for its
    /// old spot, whoever laid it.
    Unlocked,
}

/// The copper that serves only what `seeds` start: each seed track or via that `takes` allows
/// and, from it, the copper its removal would leave hanging: the via at a track's end that at
/// most one other track meets (a via the route passes through, or a stub) with that track, and
/// the tracks anchored on a removed track. A via that two or more other tracks still meet, all on
/// one layer, joins nothing across layers any more: it goes alone, and those tracks stay joined
/// where they meet. A pad, a via that still joins layers, a host track the branch joins, and
/// copper `takes` does not allow end the walk, so the rest of each net stays. O(tracks) for the
/// anchor maps, then O(the branches' items) lookups.
pub(crate) fn branches(board: &Board, seeds: impl IntoIterator<Item = ItemId>, takes: Takes) -> BTreeSet<ItemId> {
    let allowed = |locked: bool, origin: &Origin| !locked && match takes {
        Takes::Solved => matches!(origin, Origin::Solved { .. }),
        Takes::Unlocked => matches!(origin, Origin::Solved { .. } | Origin::Authored),
    };
    let rippable = |id: ItemId| match board.locate(id) {
        Some((ItemKind::Track, i)) => allowed(board.tracks[i].locked, &board.tracks[i].origin),
        Some((ItemKind::Via, i)) => allowed(board.vias[i].locked, &board.vias[i].origin),
        _ => false,
    };
    let layer = |id: ItemId| match board.locate(id) {
        Some((ItemKind::Track, i)) => Some(board.tracks[i].layer),
        _ => None,
    };
    let (mut on_via, mut on_track) = (BTreeMap::<ItemId, Vec<ItemId>>::new(), BTreeMap::<ItemId, Vec<ItemId>>::new());
    for t in &board.tracks {
        for end in t.ends {
            match end {
                Anchor::Via(v) => on_via.entry(v).or_default().push(t.id),
                Anchor::Track { id, .. } => on_track.entry(id).or_default().push(t.id),
                _ => {}
            }
        }
    }
    let mut out = BTreeSet::new();
    let mut stack: Vec<ItemId> = seeds.into_iter().filter(|&id| rippable(id)).collect();
    loop {
        while let Some(id) = stack.pop() {
            if !out.insert(id) { continue }
            match board.locate(id) {
                Some((ItemKind::Track, _)) => stack.extend(on_track.get(&id).into_iter().flatten().copied().filter(|&s| rippable(s))),
                Some((ItemKind::Via, _)) => stack.extend(on_via.get(&id).into_iter().flatten().copied().filter(|&t| rippable(t))),
                _ => {}
            }
        }
        // Each via a removed track met, judged by the tracks left on it once the walk settles (a
        // later branch can take one of them): repeated until no via changes.
        let met: BTreeSet<ItemId> = out.iter().filter_map(|&id| match board.locate(id) {
            Some((ItemKind::Track, i)) => Some(board.tracks[i].ends),
            _ => None,
        }).flatten().filter_map(|end| match end { Anchor::Via(v) => Some(v), _ => None })
            .filter(|v| !out.contains(v) && rippable(*v)).collect();
        let mut changed = false;
        for v in met {
            let left: Vec<ItemId> = on_via.get(&v).into_iter().flatten().copied().filter(|o| !out.contains(o)).collect();
            if left.len() <= 1 {
                if left.iter().all(|&o| rippable(o)) {
                    stack.push(v);
                    changed = true;
                }
            } else if left.iter().all(|&o| layer(o) == layer(left[0])) {
                out.insert(v);
                changed = true;
            }
        }
        if !changed { return out }
    }
}
