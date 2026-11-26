//! The constructive placer: parts go on one at a time, each where what already stands pulls it,
//! at the rotation that keeps its nets shortest, on the nearest legal spot.
//!
//! Order: a part whose pull stands goes next (a decoupling capacitor right after its IC, a `near`
//! end after the other). Otherwise a cluster's parts go on together: the next part is the one of
//! the current cluster most connected to what stands (by net weight), then the most connected
//! part of any cluster, ties by more connected pads, then `CompId`. A part keeps its side unless a
//! `side` intent names one; a part the circuit fixes to a spot keeps its pose. Its wanted point: its first pull that stands (its IC
//! power pin for a decoupling capacitor, a `near` end), else the weighted centre of its nets'
//! standing pins, else its cluster's standing parts, else the middle of the board. Each of the
//! four rotations takes `Free::nearest` of that point; the one with the least wirelength on the
//! part's nets (standing pins only) wins, ties by distance, then rotation. A part keeps its side.
//!
//! Edge slots: a part the circuit fixes to an edge alone (a block's: its connector) goes on
//! first, turned to face out across the edge (`edge_rotation`), flush against it, and slides
//! along it to where what stands pulls it, the edge's middle when nothing does. It then pulls the
//! rest like any part fixed to a spot. A part that finds no room (a
//! display that fills half the board, placed last) goes first on a second pass; the pass that
//! leaves the fewest parts without room wins.
//!
//! Intents over room: a `near` end takes the nearest spot within the intent's max once its other
//! end stands (spaced if one is, else packed), and only where none is, the nearest at all.
//!
//! Spacing: with `SeedOpts::halos` each part keeps its halo clear round its courtyard (two
//! neighbours' halos add up), and takes the nearest legal spot where none has room for them. A
//! decoupling capacitor keeps no halo and minds none, and it is part of its IC's body: it keeps
//! the offset from its IC that the packed placement gave it, the IC goes only where its capacitors
//! fit, and others keep the IC's halo clear of the whole body.
//!
//! By demand (`SeedOpts::demand`): rounds of seeding, each reading the routing demand map
//! (`congest`) of the round before. A part no pull holds goes to the quietest point of its optimal
//! region (`quiet`): two-pin parts on a long net (pull-ups, series resistors) step out of the
//! traffic their net shares with others, while an IC, whose many nets pin its region to a point,
//! stays. And each part keeps a halo for the tracks that overflow where it stands (`raise`): room
//! goes where the demand is. When the median box stalls, the previous courtyard's overflow
//! pays for extra wire outside it. Both attempts start from the same settled round. Only a
//! strict fall in overflow with no more unplaced parts starts another round.
//!
//! Mates in repeated blocks with held anchors try their packed offsets on the bare panel.
//! When seats fit, run through conform with and without them. The finished layouts compete on
//! unplaced parts, broken nears, held groups alike, then overflow. Only the winner is offered.
//!
//! After the demand rounds, conform tries each copy of a freely movable repeated block as the exemplar for
//! the whole group. Each body stays near its existing anchor and preserves held near limits.
//! Among complete trials with no increase in broken nears or overflow, the least overflow,
//! then wirelength, then copy index wins. Each accepted group stands for the next group's trials.
//! For k copies this costs k trials of 4k nearest searches and one map each. The job pool and
//! the caller share one board, problem, near limits and kept halos cloned or resolved per pass,
//! and one pose snapshot per group. Each running trial owns its working poses, free space and
//! map. Only the group's poses survive scoring. No group work runs in the UI loop.
//!
//! Cost: O(n^2) to pick the order for n parts, plus four nearest searches per part. Each net keeps
//! the box of its standing pins, so a part's wirelength and pull cost O(its nets). A demand round
//! attempt adds one map (O(nets x cells in their boxes + pads)) and, per part, O(cells in its region x
//! cells under it); the rounds are few, since each must lower an integer overflow.

use super::cluster::{clusters, identical_instances, Clusters, Instance, Pull};
use crate::progress::{CompareStep, JobPhase, Progress};
use crate::worker::Spawn;
use super::congest::{congestion, default_cell, pitch, CongestionMap};
use super::free::Free;
use super::{PNet, PinRef, PlaceProblem, Pose};
use fab_elec_board::placement::{body_at, pads_box};
use fab_elec_board::{Board, Placed, PortRoom};
use fab_elec_circuit::{Circuit, Edge, Intent, Target};
use fab_elec_parts::{Body3d, BodyPortKind};
use fab_elec_core::{Angle, BBox, BlockId, CompId, ItemId, Nm, PadIx, Side, P};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc, Arc};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedOpts {
    /// The lattice the nearest-spot search falls back to.
    pub grid: Nm,
    /// Room a part keeps clear round its courtyard for the tracks that pass it, by board item
    /// (`Free::with_halo`); empty packs parts as tight as legal. A decoupling capacitor keeps none
    /// of its own and counts as part of its IC: others keep the IC's halo clear of it too.
    pub halos: BTreeMap<ItemId, Nm>,
    /// Seed again in rounds by routing demand (quiet points and halos from the map of the round
    /// before), while each lowers the map's overflow.
    pub demand: bool,
    /// Run the original plain chain as a control in the prefix test.
    #[cfg(test)]
    pub(super) plain_only: bool,
    /// Port geometry relative to each component's position in board axes.
    pub rooms: Vec<PortRoom>,
}

impl Default for SeedOpts {
    fn default() -> Self {
        Self {
            grid: 50_000, halos: BTreeMap::new(), demand: false, rooms: Vec::new(),
            #[cfg(test)]
            plain_only: false,
        }
    }
}

/// Demand passes after round zero. A failure is an attempted pass that was not kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DemandPasses {
    pub plain: usize,
    pub grown: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Movables with no legal spot: they keep the pose they came with.
    pub unplaced: Vec<usize>,
    /// The inferred decoupling pairs as (capacitor, IC).
    pub decoupling: Vec<(CompId, CompId)>,
    /// The demand rounds kept after the first seeding (`SeedOpts::demand`).
    pub rounds: usize,
    /// Work done by this pipeline, including unsuccessful attempts.
    pub passes: DemandPasses,
    /// The two finished layouts when any seats were proposed.
    pub seat_guard: Option<SeatGuard>,
    /// Decisions in group order, up to cancellation. Timings measure this run.
    pub conformed: Vec<ConformRow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConformStatus {
    Kept,
    RefusedNear,
    RefusedOver,
    NoSpot,
    SkippedUnplaced,
    SkippedMixedSides,
    SkippedFixed,
    HeldApart,
    RefusedAlike,
}
fab_elec_core::canon_tags!(ConformStatus);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundScore {
    /// Work done before conform in this layout. Only the end metrics decide the guard.
    pub passes: DemandPasses,
    pub unplaced: Vec<usize>,
    pub broken: usize,
    pub alike: usize,
    pub over: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatGuard {
    pub plain: RoundScore,
    pub seated: RoundScore,
}

impl SeatGuard {
    /// The first differing term decides. A tie keeps the seats.
    pub fn decision(&self) -> ConformStatus {
        let (a, b) = (&self.seated, &self.plain);
        for (order, refusal) in [
            (a.unplaced.len().cmp(&b.unplaced.len()), ConformStatus::SkippedUnplaced),
            (a.broken.cmp(&b.broken), ConformStatus::RefusedNear),
            (b.alike.cmp(&a.alike), ConformStatus::RefusedAlike),
            (a.over.cmp(&b.over), ConformStatus::RefusedOver),
        ] {
            match order {
                std::cmp::Ordering::Greater => return refusal,
                std::cmp::Ordering::Less => return ConformStatus::Kept,
                std::cmp::Ordering::Equal => {}
            }
        }
        ConformStatus::Kept
    }

    pub fn explanation(&self) -> String {
        let (name, other, a, b) = if self.decision() == ConformStatus::Kept {
            ("seats", "without seats", &self.seated, &self.plain)
        } else {
            ("plain", "with seats", &self.plain, &self.seated)
        };
        let reason = if a.unplaced.len() != b.unplaced.len() {
            format!("{} fewer unplaced", b.unplaced.len() - a.unplaced.len())
        } else if a.broken != b.broken {
            format!("{} fewer broken near intents", b.broken - a.broken)
        } else if a.alike != b.alike {
            let n = a.alike - b.alike;
            format!("{n} more {} alike", if n == 1 { "group" } else { "groups" })
        } else if a.over == b.over {
            "end scores tied".into()
        } else {
            "as many groups alike".into()
        };
        format!("{name} kept: {reason}; over {:.2} mm against {:.2} mm {other}", a.over as f64 / 1e6, b.over as f64 / 1e6)
    }
}

/// A distinct packed offset for a held group's free members, in member order and in the
/// anchor's frame. Only a complete fit satisfying all standing nears has a wirelength.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatCandidate {
    pub exemplar: usize,
    pub offsets: Vec<Pose>,
    pub copies_fit: usize,
    pub blocker: Option<SeatBlocker>,
    pub wirelength: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeatBlocker {
    /// The mate and the first item it meets (part, hole or keepout).
    Item(ItemId, ItemId),
    Edge(ItemId),
    Side(ItemId),
    Near,
}

/// The bare-panel decision. Best indexes the candidates: least wirelength among passes,
/// otherwise the first candidate fitting the most copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatTrial {
    pub candidates: Vec<SeatCandidate>,
    pub best: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConformRow {
    /// Each copy's block instance, in copy order: `exemplar` indexes it.
    pub blocks: Vec<BlockId>,
    pub copies: usize,
    pub members_per_copy: usize,
    /// Zero-based chosen copy, or the best refused copy. For a seat with no complete fit,
    /// this is the packed exemplar that fit the most copies.
    pub exemplar: Option<usize>,
    pub passed: usize,
    pub status: ConformStatus,
    /// Held mates tried before round zero, in the order of their first packed exemplar.
    pub seat_trial: Option<SeatTrial>,
    /// Signed nm, relative to the board before this group. For seats, both deltas compare the
    /// whole seated and plain end states. Includes refused complete trials.
    pub delta_over: Option<i64>,
    pub delta_wirelength: Option<i64>,
    /// Changed poses in that trial and the largest translation in nm.
    pub moved: usize,
    pub farthest_move: i64,
    pub wall_time: Duration,
    /// Sum of the individual trial durations, including overlapping work.
    pub trial_time: Duration,
}

impl ConformRow {
    fn new(copies: &[Instance]) -> Self {
        Self {
            blocks: copies.iter().map(|c| c.block).collect(), copies: copies.len(), members_per_copy: copies[0].members.len(),
            exemplar: None, passed: 0, status: ConformStatus::NoSpot, seat_trial: None, delta_over: None,
            delta_wirelength: None, moved: 0, farthest_move: 0, wall_time: Duration::ZERO, trial_time: Duration::ZERO,
        }
    }

    fn from_seat_trial(copies: &[Instance], trial: SeatTrial, elapsed: Duration) -> Self {
        let best = trial.best.map(|i| &trial.candidates[i]);
        let status = match best {
            Some(c) if c.wirelength.is_some() => ConformStatus::Kept,
            Some(c) if c.blocker == Some(SeatBlocker::Near) => ConformStatus::RefusedNear,
            Some(_) => ConformStatus::NoSpot,
            None => ConformStatus::SkippedUnplaced,
        };
        Self {
            status, exemplar: best.map(|c| c.exemplar),
            passed: trial.candidates.iter().filter(|c| c.wirelength.is_some()).count(),
            seat_trial: Some(trial), wall_time: elapsed, trial_time: elapsed, ..Self::new(copies)
        }
    }

    fn measure_move(&mut self, from: Pose, to: Pose) {
        if from != to {
            self.moved += 1;
            let dist = (from.at.dist2(to.at) as f64).sqrt().round() as i64;
            self.farthest_move = self.farthest_move.max(dist);
        }
    }
}

/// A seeding kept so far, as [`seed_watched`] tells it.
pub struct SeedRound<'a> {
    /// 0 for the first seeding, then each demand round kept. Kept groups repeat the last round.
    pub round: usize,
    /// One per movable: a legal spot, or for a part without room the pose it came with.
    pub poses: &'a [Pose],
    /// The movables without room.
    pub unplaced: &'a [usize],
    /// The demand map's overflow at these poses, with `SeedOpts::demand`.
    pub overflow: Option<i64>,
}

/// What a seed tells as it goes ([`seed_watched`]). A closure over [`SeedRound`] hears only the
/// seedings kept.
pub trait SeedWatch {
    /// Poll cancellation while comparing layouts, before offering either one.
    fn cancelled(&self) -> bool { false }
    /// The two pool workers can publish phases and poll this shared cancellation flag.
    /// Watches with private cancellation state keep the comparison on the caller.
    fn comparison_progress(&self) -> Option<Arc<Progress>> { None }
    /// The first seeding, each demand round kept and each conform group kept. False ends the seed
    /// at these poses.
    fn kept(&mut self, round: &SeedRound) -> bool;
    /// Conform starts on repeated-block group `group` (from 0) of `of`, whose first copy is the
    /// block `first`. False ends the seed before it, keeping the groups already kept.
    fn matching(&mut self, _group: usize, _of: usize, _first: BlockId) -> bool { true }
}

impl<F: FnMut(&SeedRound) -> bool> SeedWatch for F {
    fn comparison_progress(&self) -> Option<Arc<Progress>> { Some(Arc::new(Progress::new())) }
    fn kept(&mut self, round: &SeedRound) -> bool { self(round) }
}

/// Hold proposals until the guard knows which layout to show. Cancellation still reaches
/// each demand round and conform group, independently of the proposals.
enum SeedEvent {
    Kept { round: usize, poses: Vec<Pose>, unplaced: Vec<usize>, overflow: Option<i64> },
    Matching(usize, usize, BlockId),
}

struct Recorded {
    report: SeedReport,
    poses: Vec<Pose>,
    over: i64,
    events: Vec<SeedEvent>,
}

struct Recording<'a> {
    progress: &'a Progress,
    cancelled: &'a dyn Fn() -> bool,
    seated: bool,
    matching: bool,
    events: Vec<SeedEvent>,
}

impl SeedWatch for Recording<'_> {
    fn kept(&mut self, s: &SeedRound) -> bool {
        if !self.matching { self.progress.comparing(self.seated, CompareStep::Round(s.round as u32 + 1)); }
        if (self.cancelled)() { return false }
        self.events.push(SeedEvent::Kept { round: s.round, poses: s.poses.to_vec(), unplaced: s.unplaced.to_vec(), overflow: s.overflow });
        true
    }

    fn matching(&mut self, group: usize, of: usize, first: BlockId) -> bool {
        self.matching = true;
        self.progress.comparing(self.seated, CompareStep::Group(group as u32));
        if (self.cancelled)() { return false }
        self.events.push(SeedEvent::Matching(group, of, first));
        true
    }
}

fn replay(events: &[SeedEvent], report: &mut SeedReport, poses: &mut [Pose], watch: &mut dyn SeedWatch) {
    let mut rows = 0;
    let mut last = None;
    for event in events {
        let keep_going = match event {
            SeedEvent::Kept { round, poses: snapshot, unplaced, overflow } => {
                let s = SeedRound { round: *round, poses: snapshot, unplaced, overflow: *overflow };
                let more = watch.kept(&s);
                last = Some(s);
                more
            }
            &SeedEvent::Matching(group, of, first) => {
                let more = watch.matching(group, of, first);
                rows = group + usize::from(more);
                more
            }
        };
        if !keep_going {
            report.conformed.truncate(rows);
            report.seat_guard = None;
            break
        }
    }
    if let Some(s) = last {
        poses.copy_from_slice(s.poses);
        report.rounds = s.round;
        report.unplaced = s.unplaced.to_vec();
    }
}

/// Place every movable of `problem` afresh into `poses` (one per movable; a pose's side is kept
/// unless a `side` intent names one, its point and rotation are not). Parts the circuit fixes to a
/// spot keep their pose; those it fixes to an edge (`edge_slots`) take one on it. Locked parts are
/// no movables.
pub fn seed(board: &Board, circuit: &Circuit, problem: &PlaceProblem, poses: &mut [Pose], opts: &SeedOpts) -> SeedReport {
    seed_watched(board, circuit, problem, poses, opts, &mut |_: &SeedRound| true, None)
}

/// [`seed`], telling `watch` of the first seeding, each demand round and each conform group kept,
/// and of each conform group as it starts. With proposed seats these events come from the
/// winning pipeline. Cancellation during comparison leaves the input poses and offers nothing.
/// Returning false during an offer stops at those poses. A pool shares the exemplar trials
/// with the calling worker. A watch with shared progress also lets a free worker claim the
/// second pipeline. Without help the caller runs seated then plain and tries copies in order.
pub fn seed_watched(
    board: &Board, circuit: &Circuit, problem: &PlaceProblem, poses: &mut [Pose], opts: &SeedOpts,
    watch: &mut dyn SeedWatch, pool: Option<&Arc<dyn Spawn>>,
) -> SeedReport {
    assert_eq!(poses.len(), problem.movables.len(), "one pose per movable");
    let found = clusters(board, circuit, problem);
    let comps = |t: &Target| match *t {
        Target::Comp(c) | Target::Pin(c, _) => vec![c],
        Target::Block(b) => circuit.block_comps(b),
    };
    let mut stays = BTreeSet::new();
    let mut sides = BTreeMap::new();
    for intent in &circuit.intents {
        match intent {
            Intent::Fixed { t, edge, at, .. } if edge.is_none() || at.is_some() => stays.extend(comps(t)),
            Intent::Side { t, side } => sides.extend(comps(t).into_iter().map(|c| (c, *side))),
            _ => {}
        }
    }
    // A part fixed to a spot stands on it, turned as the spot says (`spots_of`, which DRC checks
    // as `Board::spots`): it keeps that pose as any part that stays does.
    let mut start_poses = poses.to_vec();
    for spot in &fab_elec_board::spots_of(circuit) {
        if let Some(m) = problem.movables.iter().position(|mv| mv.comp == spot.comp) {
            (start_poses[m].at, start_poses[m].rot) = spot.pose(start_poses[m].rot);
        }
    }
    let edges = edge_slots(board, circuit).into_iter().filter(|(comp, ..)| !stays.contains(comp)).filter_map(|(comp, edge, rot)| {
        let m = problem.movables.iter().position(|mv| mv.comp == comp)?;
        let part = board.placed_item(problem.movables[m].item)?;
        let side = sides.get(&comp).copied().unwrap_or(start_poses[m].side);
        let rot = rot.unwrap_or_else(|| edge_rotation(part, circuit.comps[comp.ix()].part.body.as_ref(), side, edge, start_poses[m].rot));
        Some((comp, (edge, rot)))
    }).collect();
    let intents = Intents { stays, sides, edges };
    let pass = SeedPass { board, circuit, problem, opts, found: &found, intents: &intents, start: &start_poses, pool };
    let none = Bodies::new();
    let plain = BTreeMap::new();
    let mut group_count = 0;
    let mut bodies = Bodies::new();
    let mut seats = BTreeMap::new();
    let mut seat_rows = BTreeMap::new();
    let mut held_groups = BTreeMap::new();
    let mut seat_search = None;
    let layout = (!opts.halos.is_empty() || opts.demand).then(|| GroupLayout::new(board, circuit, problem, &intents, &start_poses));
    if let Some(layout) = &layout {
        let groups = identical_instances(board, circuit);
        group_count = groups.len() as u32;
        let held: Vec<_> = groups.iter().filter_map(|copies| {
            let (GroupAnchor::Held(frames), mates) = layout.anchor(copies, problem, &start_poses).ok()? else { return None };
            Some((copies, HeldMates { frames, mates }))
        }).collect();
        let priority: Vec<_> = held.iter().flat_map(|(_, group)| group.mates.iter().flatten().copied()).collect();
        let (packed_report, packed) = pass.run(None, &BTreeMap::new(), &none, &priority, &plain);
        if opts.demand && !priority.is_empty() {
            let search = ConformSearch::new(board, circuit, problem, &packed_report, &BTreeMap::new(), opts);
            let mut standing: BTreeMap<_, _> = problem.movables.iter().enumerate()
                .filter(|(_, mv)| intents.stays.contains(&mv.comp)).map(|(m, _)| (m, packed[m])).collect();
            for (copies, group) in held {
                if !group.mates[0].is_empty() {
                    let start = Instant::now();
                    let trial = search.try_seats(copies, &group.frames, &group.mates, &packed, &packed_report.unplaced, &intents.sides, &mut standing);
                    seat_rows.insert(copies[0].block, ConformRow::from_seat_trial(copies, trial, start.elapsed()));
                }
                held_groups.insert(copies[0].block, group);
            }
            standing.retain(|&m, _| !intents.stays.contains(&problem.movables[m].comp));
            seats = standing;
            seat_search = Some(search);
        }
        bodies = pass.packed_bodies(&packed, layout);
    }

    let Some(search) = seat_search.filter(|_| !seats.is_empty()) else {
        let (report, trial, _) = pass.pipeline(&bodies, &plain, &seat_rows, watch);
        poses.copy_from_slice(&trial);
        return report
    };
    let shared = watch.comparison_progress();
    let pool_helpers = pool.filter(|_| shared.is_some());
    let progress = shared.unwrap_or_default();
    progress.set_phase(JobPhase::Compare { seated: CompareStep::Waiting, plain: CompareStep::Waiting, of: group_count });
    let rows = seat_rows.clone();
    let record = move |pass: &SeedPass, seated: bool, progress: &Progress, cancelled: &dyn Fn() -> bool| {
        if cancelled() { return None }
        progress.comparing(seated, CompareStep::Round(0));
        if cancelled() { return None }
        let mut recording = Recording { progress, cancelled, seated, matching: false, events: Vec::new() };
        let result = if seated {
            pass.pipeline(&bodies, &seats, &rows, &mut recording)
        } else {
            // The ordinary packed pass supplies the plain run's IC/capacitor offsets.
            let mut bodies = Bodies::new();
            if !pass.found.decoupling.is_empty() {
                let (_, packed) = pass.run(None, &BTreeMap::new(), &none, &[], &plain);
                bodies = pass.packed_bodies(&packed, layout.as_ref().expect("seats have a group layout"));
            }
            if cancelled() { return None }
            pass.pipeline(&bodies, &plain, &BTreeMap::new(), &mut recording)
        };
        if cancelled() { return None }
        progress.comparing(seated, CompareStep::Done);
        let (report, poses, over) = result;
        Some(Recorded { report, poses, over, events: recording.events })
    };
    let search = Arc::new(search);
    let (seated_run, plain_run) = if let Some(pool) = pool_helpers {
        // A helper can claim one run only once it starts. The caller takes every remaining
        // index (seated first), including the nested exemplar trials on a saturated pool.
        // The search already owns the board and problem snapshots needed by a queued helper.
        let inputs = search.clone();
        let (circuit, opts, helpers) = (circuit.clone(), opts.clone(), Arc::downgrade(pool));
        let progress = progress.clone();
        let mut runs = pool_trials(2, Some(pool), move |i| {
            let helpers = helpers.upgrade().expect("the caller holds the pool until both runs finish");
            let pass = SeedPass { board: &inputs.board, problem: &inputs.problem, circuit: &circuit, opts: &opts,
                found: &found, intents: &intents, start: &start_poses, pool: Some(&helpers) };
            record(&pass, i == 0, &progress, &|| progress.cancelled())
        }).into_iter().map(|r| r.expect("seat comparison worker finished"));
        (runs.next().unwrap(), runs.next().unwrap())
    } else {
        let cancelled = || progress.cancelled() || watch.cancelled();
        (record(&pass, true, &progress, &cancelled), record(&pass, false, &progress, &cancelled))
    };
    let (Some(seated), Some(plain)) = (seated_run, plain_run) else { return SeedReport::default() };
    let score = |run: &Recorded| RoundScore {
        passes: run.report.passes, unplaced: run.report.unplaced.clone(), broken: search.broken(&run.poses), over: run.over,
        alike: held_groups.values().filter(|g| g.alike(&run.poses, &run.report.unplaced)).count(),
    };
    let guard = SeatGuard { seated: score(&seated), plain: score(&plain) };
    let decision = guard.decision();
    let delta_wire = search.wirelength(&seated.poses) - search.wirelength(&plain.poses);
    let (mut report, events) = if decision == ConformStatus::Kept { (seated.report, seated.events) } else { (plain.report, plain.events) };
    for row in &mut report.conformed {
        let Some(mut seat_row) = seat_rows.remove(&row.blocks[0]) else { continue };
        if seat_row.status == ConformStatus::Kept {
            seat_row.status = decision;
            seat_row.delta_over = Some(guard.seated.over - guard.plain.over);
            seat_row.delta_wirelength = Some(delta_wire);
            for &m in held_groups[&row.blocks[0]].mates.iter().flatten() { seat_row.measure_move(plain.poses[m], seated.poses[m]); }
        }
        *row = seat_row;
    }
    report.seat_guard = Some(guard);
    if progress.cancelled() || watch.cancelled() { return SeedReport::default() }
    replay(&events, &mut report, poses, watch);
    report
}

/// The settled map is shared by both attempts. Only growth needs the previous poses.
#[derive(Clone, Copy)]
struct Demand<'a> {
    map: &'a CongestionMap,
    grow_from: Option<&'a [Pose]>,
}

struct SeedPass<'a> {
    board: &'a Board,
    circuit: &'a Circuit,
    problem: &'a PlaceProblem,
    opts: &'a SeedOpts,
    found: &'a Clusters,
    intents: &'a Intents,
    start: &'a [Pose],
    pool: Option<&'a Arc<dyn Spawn>>,
}

impl SeedPass<'_> {
    // Parts retried for room precede even the held groups' mates in the packed pass.
    fn run(&self, demand: Option<Demand<'_>>, halos: &BTreeMap<ItemId, Nm>, bodies: &Bodies, priority: &[usize], seats: &BTreeMap<usize, Pose>) -> (SeedReport, Vec<Pose>) {
        let mut retry = Vec::new();
        let mut best: Option<(SeedReport, Vec<Pose>)> = None;
        loop {
            let first: Vec<_> = retry.iter().chain(priority).copied().collect();
            let mut trial = self.start.to_vec();
            let report = pass(self.board, self.problem, self.found, self.intents, seats, bodies, &first, &mut trial, self.opts.grid, halos, demand, &self.opts.rooms);
            let new: Vec<usize> = report.unplaced.iter().copied().filter(|u| !retry.contains(u)).collect();
            let done = report.unplaced.is_empty() || new.is_empty();
            if best.as_ref().is_none_or(|(b, _)| report.unplaced.len() < b.unplaced.len()) {
                best = Some((report, trial));
            }
            if done { break }
            retry.extend(new);
        }
        best.expect("one pass ran")
    }

    // A decoupling capacitor keeps the packed offset from its IC, on each run.
    fn packed_bodies(&self, packed: &[Pose], layout: &GroupLayout) -> Bodies {
        let mut bodies = Bodies::new();
        for &(cap, ic) in &self.found.decoupling {
            let (Some(&c), Some(&i)) = (layout.movable.get(&cap), layout.movable.get(&ic)) else { continue };
            let (pc, pi) = (packed[c], packed[i]);
            if pc.side != pi.side || self.intents.stays.contains(&ic) { continue }
            bodies.entry(i).or_default().push((c, pc.at.sub(pi.at).rotated(pi.rot.neg()), pc.rot.sub(pi.rot)));
        }
        bodies
    }

    fn pipeline(&self, bodies: &Bodies, seats: &BTreeMap<usize, Pose>, rows: &BTreeMap<BlockId, ConformRow>, watch: &mut dyn SeedWatch) -> (SeedReport, Vec<Pose>, i64) {
        let (board, circuit, problem, opts) = (self.board, self.circuit, self.problem, self.opts);
        let none = Bodies::new();
        let run = |demand: Option<Demand<'_>>, halos: &BTreeMap<ItemId, Nm>| {
            self.run(demand, halos, if halos.is_empty() { &none } else { bodies }, &[], seats)
        };
        let (mut report, mut trial) = run(None, &opts.halos);
        if !opts.demand {
            watch.kept(&SeedRound { round: 0, poses: &trial, unplaced: &report.unplaced, overflow: None });
            return (report, trial, 0)
        }
        let (cell, pitch) = (default_cell(board), pitch(board));
        let mut map = congestion(board, problem, &trial, cell);
        let mut halos = opts.halos.clone();
        // Each round reads its predecessor's map and is kept while it lowers overflow
        // and leaves no more parts without room.
        let over = loop {
            if !watch.kept(&SeedRound { round: report.rounds, poses: &trial, unplaced: &report.unplaced, overflow: Some(map.over()) }) {
                break map.over()
            }
            let mut next_halos = halos.clone();
            raise(board, problem, &trial, &map, pitch, &mut next_halos);
            // Keep the plain chain intact. Both attempts start with exactly the same map,
            // poses and raised halos. A grown success starts another plain-first round.
            let mut accepted = None;
            for grow in [false, true] {
                #[cfg(test)]
                if grow && opts.plain_only { break }
                let demand = Demand { map: &map, grow_from: grow.then_some(trial.as_slice()) };
                let (next_report, next) = run(Some(demand), &next_halos);
                let next_map = congestion(board, problem, &next, cell);
                if next_map.over() < map.over() && next_report.unplaced.len() <= report.unplaced.len() {
                    if grow { report.passes.grown += 1 } else { report.passes.plain += 1 }
                    accepted = Some((next_report, next, next_map));
                    break
                }
                report.passes.failed += 1;
            }
            let Some((next_report, next, next_map)) = accepted else {
                break conform(board, circuit, problem, self.intents, &halos, opts, &mut trial, &mut report, map.over(), rows, watch, self.pool)
            };
            (report, trial, map) = (SeedReport { rounds: report.rounds + 1, passes: report.passes, ..next_report }, next, next_map);
            halos = next_halos;
        };
        (report, trial, over)
    }
}

/// Each IC movable's decoupling capacitors that go on with it, as (movable, offset from the IC's
/// anchor at rotation zero, rotation relative to the IC's).
type Bodies = BTreeMap<usize, Vec<(usize, P, Angle)>>;

/// A claim always delivers a slot, even if its helper unwinds inside the trial.
struct Claim<'a, T> {
    index: usize,
    result: Option<T>,
    finished: &'a mpsc::Sender<(usize, Option<T>)>,
}

impl<T> Drop for Claim<'_, T> {
    fn drop(&mut self) {
        let _ = self.finished.send((self.index, self.result.take()));
    }
}

fn pool_trials<T: Send + 'static>(
    count: usize, pool: Option<&Arc<dyn Spawn>>, trial: impl Fn(usize) -> T + Send + Sync + 'static,
) -> Vec<Option<T>> {
    let Some(pool) = pool else { return (0..count).map(|i| Some(trial(i))).collect() };
    let (finished, results) = mpsc::channel();
    let next = AtomicUsize::new(0);
    let work = Arc::new(move || loop {
        let index = next.fetch_add(1, Ordering::Relaxed);
        if index >= count { break }
        let mut claim = Claim { index, result: None, finished: &finished };
        claim.result = Some(trial(index));
    });
    for _ in 1..count {
        let work = Arc::downgrade(&work);
        pool.spawn(Box::new(move || { if let Some(work) = work.upgrade() { work() } }));
    }
    work();
    let mut slots: Vec<_> = (0..count).map(|_| None).collect();
    // Every index is now claimed. Queued helpers can start after the entire job finishes.
    for _ in 0..count {
        let (index, value) = results.recv().expect("each claim returns its slot");
        slots[index] = value;
    }
    slots
}

fn net_length(problem: &PlaceProblem, net: &PNet, pose: impl Fn(usize) -> Pose) -> i64 {
    net.wirelength(BBox::of_points(net.pins.iter().map(|&p| match p {
        PinRef::Fixed(at) => at,
        PinRef::Mov(m, pad) => problem.movables[m].pad_at(pad, pose(m)),
    })))
}

/// Geometry of a near end. Fixed footprints keep their full courtyard.
#[derive(Clone, Copy)]
enum NearPart {
    Fixed(BBox),
    Mov(usize, Option<PadIx>),
}

struct TrialNear {
    complete: bool,
    a: Vec<NearPart>,
    b: Vec<NearPart>,
    max: Nm,
}

impl TrialNear {
    fn holds(&self, bounds: impl Fn(NearPart) -> BBox) -> bool {
        self.max >= 0 && self.a.iter().any(|&a| {
            let a = bounds(a);
            self.b.iter().any(|&b| {
                let b = bounds(b);
                let dx = (i64::from(a.min.x) - i64::from(b.max.x)).max(i64::from(b.min.x) - i64::from(a.max.x)).max(0);
                let dy = (i64::from(a.min.y) - i64::from(b.max.y)).max(i64::from(b.min.y) - i64::from(a.max.y)).max(0);
                i128::from(dx) * i128::from(dx) + i128::from(dy) * i128::from(dy) <= i128::from(self.max).pow(2)
            })
        })
    }
}

struct ConformSearch {
    rooms: Vec<PortRoom>,
    board: Board,
    problem: PlaceProblem,
    halos: BTreeMap<ItemId, Nm>,
    of_ic: BTreeMap<ItemId, ItemId>,
    nears: Vec<TrialNear>,
    grid: Nm,
}

impl ConformSearch {
    fn new(board: &Board, circuit: &Circuit, problem: &PlaceProblem, report: &SeedReport, halos: &BTreeMap<ItemId, Nm>, opts: &SeedOpts) -> Self {
        let of_ic = report.decoupling.iter().filter_map(|&(cap, ic)| Some((board.placed_of(cap)?.id, board.placed_of(ic)?.id))).collect();
        let mov: BTreeMap<_, _> = problem.movables.iter().enumerate().map(|(m, p)| (p.comp, m)).collect();
        let part = |c: CompId, pad: Option<PadIx>| {
            let p = board.placed_of(c)?;
            if let Some(&m) = mov.get(&c) { return Some(NearPart::Mov(m, pad)) }
            let bounds = pad.and_then(|i| p.pad_at(i)).map(BBox::point)
                .unwrap_or_else(|| body_at(p, p.at, p.rot, p.side, true).bbox());
            Some(NearPart::Fixed(if bounds.is_empty() { BBox::point(p.at) } else { bounds }))
        };
        let end = |target: &Target| -> (Vec<_>, usize) {
            match *target {
                Target::Block(b) => {
                    let comps = circuit.block_comps(b);
                    let len = comps.len();
                    (comps.into_iter().filter_map(|c| part(c, None)).collect(), len)
                }
                Target::Comp(c) => (part(c, None).into_iter().collect(), 1),
                Target::Pin(c, pin) => {
                    let pad = circuit.comps[c.ix()].part.pins[pin.ix()].pads.first().and_then(|no| {
                        board.placed_of(c)?.footprint.pads.iter().position(|p| &p.no == no)
                    });
                    (pad.and_then(|i| part(c, Some(PadIx(i as u16)))).into_iter().collect(), 1)
                }
            }
        };
        let nears = circuit.intents.iter().filter_map(|intent| {
            let Intent::Near { a, b, max } = intent else { return None };
            let ((a, na), (b, nb)) = (end(a), end(b));
            let complete = a.len() == na && b.len() == nb;
            (!a.is_empty() && !b.is_empty()).then_some(TrialNear { a, b, complete, max: *max })
        }).collect();
        Self { board: board.clone(), problem: problem.clone(), halos: halos.clone(), of_ic, nears, grid: opts.grid, rooms: opts.rooms.clone() }
    }

    fn bounds(&self, end: NearPart, pose_of: impl Fn(usize) -> Pose) -> BBox {
        let (m, pad) = match end { NearPart::Fixed(b) => return b, NearPart::Mov(m, pad) => (m, pad) };
        let (mv, pose) = (&self.problem.movables[m], pose_of(m));
        if let Some(pad) = pad { return BBox::point(mv.pad_at(pad, pose)) }
        let p = self.board.placed_item(mv.item).expect("a movable has a board item");
        let b = body_at(p, pose.at, pose.rot, pose.side, true).bbox();
        if b.is_empty() { BBox::point(pose.at) } else { b }
    }

    fn broken(&self, poses: &[Pose]) -> usize {
        self.nears.iter().filter(|n| !n.holds(|end| self.bounds(end, |m| poses[m]))).count()
    }

    fn wirelength(&self, poses: &[Pose]) -> i64 {
        self.problem.nets.iter().map(|net| net_length(&self.problem, net, |m| poses[m])).sum()
    }

    /// Try a distinct packed pattern on the bare panel. The scratch standing set includes
    /// earlier groups' winners. A passing winner adds its mates for round zero.
    fn try_seats(
        &self, copies: &[Instance], frames: &[Pose], mates: &[Vec<usize>], packed: &[Pose],
        unplaced: &[usize], sides: &BTreeMap<CompId, Side>, standing: &mut BTreeMap<usize, Pose>,
    ) -> SeatTrial {
        let origin = Pose { at: P::ZERO, rot: Angle::ZERO, side: Side::Top };
        let moving = self.problem.movables.iter().map(|mv| mv.item).collect();
        let ids: BTreeSet<_> = copies.iter().flat_map(|c| &c.members).filter_map(|&c| self.board.placed_of(c))
            .flat_map(|p| p.pad_nets.iter().flatten().copied()).collect();
        let nets: Vec<_> = self.problem.nets.iter().filter(|n| n.weight != 0 && ids.contains(&n.net)).collect();
        let mut row = SeatTrial { candidates: Vec::new(), best: None };
        let mut winner = None;
        for (ex, source) in mates.iter().enumerate() {
            if source.iter().any(|m| unplaced.binary_search(m).is_ok()) { continue }
            let offsets: Vec<_> = source.iter().map(|&m| in_copy(packed[m], frames[ex], origin)).collect();
            if row.candidates.iter().any(|c| c.offsets == offsets) { continue }
            let mut candidate = SeatCandidate { exemplar: ex, offsets, copies_fit: 0, blocker: None, wirelength: None };
            let mut trial = standing.clone();
            let free_at = |trial: &BTreeMap<usize, Pose>| {
                let mut free = Free::with_rooms(&self.board, &moving, BTreeSet::new(), &self.rooms);
                for (&m, &pose) in trial { free.put(self.problem.movables[m].item, pose) }
                free
            };
            let mut free = free_at(&trial);
            for (copy, &frame) in mates.iter().zip(frames) {
                let mut fit = true;
                for (k, (&m, &src)) in copy.iter().zip(source).enumerate() {
                    let pose = in_copy(packed[src], frames[ex], frame);
                    let mv = &self.problem.movables[m];
                    let item = mv.item;
                    let side = sides.get(&mv.comp).copied().unwrap_or(packed[m].side);
                    if pose.side != side || !free.fits(item, pose) {
                        candidate.blocker.get_or_insert_with(|| {
                            if pose.side != side { SeatBlocker::Side(item) }
                            else { free.blocker(item, pose).map_or(SeatBlocker::Edge(item), |other| SeatBlocker::Item(item, other)) }
                        });
                        // A partial copy must not consume room from the following copies.
                        if k > 0 {
                            for placed in &copy[..k] { trial.remove(placed); }
                            free = free_at(&trial);
                        }
                        fit = false;
                        break
                    }
                    free.put(item, pose);
                    trial.insert(m, pose);
                }
                candidate.copies_fit += usize::from(fit);
            }
            if candidate.copies_fit == copies.len() {
                let broken = self.nears.iter().any(|n| n.complete && n.a.iter().chain(&n.b).all(|p| match p {
                    NearPart::Fixed(_) => true,
                    NearPart::Mov(m, _) => trial.contains_key(m),
                }) && !n.holds(|e| self.bounds(e, |m| trial[&m])));
                if broken {
                    candidate.blocker = Some(SeatBlocker::Near);
                } else {
                    candidate.wirelength = Some(nets.iter().map(|net| {
                        let pins = net.pins.iter().filter_map(|p| match *p {
                            PinRef::Fixed(at) => Some(at),
                            PinRef::Mov(m, pad) => trial.get(&m).map(|&pose| self.problem.movables[m].pad_at(pad, pose)),
                        });
                        net.wirelength(BBox::of_points(pins))
                    }).sum());
                }
            }
            let rank = |c: &SeatCandidate| (c.wirelength.is_none(), std::cmp::Reverse(c.copies_fit), c.wirelength, c.exemplar);
            if row.best.is_none_or(|i| rank(&candidate) < rank(&row.candidates[i])) {
                row.best = Some(row.candidates.len());
                winner = candidate.wirelength.map(|_| trial);
            }
            row.candidates.push(candidate);
        }
        if let Some(poses) = winner { *standing = poses }
        row
    }

    /// Each rotation is scored on the union of the body's nets, counting each net once.
    /// Earlier copies have their trial poses and later copies still have their original nets.
    fn trial(&self, poses: &[Pose], copies: &[Vec<usize>], anchor: usize, exemplar: usize) -> Option<Vec<Pose>> {
        let problem = &self.problem;
        let moving = problem.movables.iter().map(|mv| mv.item).collect();
        let mut free = Free::with_rooms(&self.board, &moving, BTreeSet::new(), &self.rooms);
        if !self.halos.is_empty() {
            let owner = |p: &Placed| self.of_ic.get(&p.id).copied().unwrap_or(p.id);
            free = free.with_halo(move |p| self.halos.get(&owner(p)).copied().unwrap_or(0), owner);
        }
        let group: BTreeSet<_> = copies.iter().flatten().copied().collect();
        for (m, mv) in problem.movables.iter().enumerate() {
            if !group.contains(&m) { free.put(mv.item, poses[m]) }
        }
        let reference = poses[copies[exemplar][anchor]];
        let offsets: Vec<_> = copies[exemplar].iter().map(|&m| {
            let p = poses[m];
            (p.at.sub(reference.at).rotated(reference.rot.neg()), p.rot.sub(reference.rot))
        }).collect();
        let mut trial = poses.to_vec();
        for copy in copies {
            let i = copy[anchor];
            let old = poses[i];
            let slots: BTreeMap<_, _> = copy.iter().copied().zip(&offsets).collect();
            let nets: Vec<_> = problem.nets.iter().filter(|net| net.weight != 0 && net.pins.iter().any(|p| {
                matches!(p, PinRef::Mov(m, _) if slots.contains_key(m))
            })).collect();
            let held: Vec<_> = self.nears.iter().filter(|n| {
                n.a.iter().chain(&n.b).any(|p| matches!(p, NearPart::Mov(m, _) if slots.contains_key(m)))
                    && n.holds(|end| self.bounds(end, |m| trial[m]))
            }).collect();
            let member = |at: P, rot: Angle, (rel, drot): (P, Angle)| Pose {
                at: at.add(rel.rotated(rot)), rot: rot.add(drot), side: old.side,
            };
            let mut best: Option<((i64, i128, bool), P, Angle)> = None;
            for rot in [Angle::ZERO, Angle::QUARTER, Angle::HALF, Angle::QUARTER.neg()] {
                let pose_at = |m: usize, at| slots.get(&m).map_or(trial[m], |&&offset| member(at, rot, offset));
                let fits = |at| copy.iter().all(|&m| m == i || free.fits(problem.movables[m].item, pose_at(m, at)))
                    && held.iter().all(|n| n.holds(|end| self.bounds(end, |m| pose_at(m, at))));
                let Some(at) = free.nearest_spaced_with(problem.movables[i].item, Pose { rot, ..old }, self.grid, &fits) else { continue };
                let length: i64 = nets.iter().map(|net| net_length(problem, net, |m| pose_at(m, at))).sum();
                let key = (length, at.dist2(old.at), rot != old.rot);
                if best.as_ref().is_none_or(|&(k, ..)| key < k) { best = Some((key, at, rot)) }
            }
            let (_, at, rot) = best?;
            for (&m, &offset) in copy.iter().zip(&offsets) {
                let pose = member(at, rot, offset);
                // Also check the members against one another on the destination side.
                if !free.fits(problem.movables[m].item, pose) { return None }
                free.put(problem.movables[m].item, pose);
                trial[m] = pose;
            }
        }
        Some(trial)
    }
}

/// The same final-pose test scores both runs. A copy can be the exemplar even if
/// transporting from a different, non-cardinal frame would round by a nanometre.
struct HeldMates {
    frames: Vec<Pose>,
    mates: Vec<Vec<usize>>,
}

impl HeldMates {
    fn alike(&self, poses: &[Pose], unplaced: &[usize]) -> bool {
        !self.mates.iter().flatten().any(|m| unplaced.binary_search(m).is_ok())
            && self.mates.iter().zip(&self.frames).any(|(source, &from)| {
                self.mates.iter().zip(&self.frames).all(|(copy, &to)| {
                    source.iter().zip(copy).all(|(&a, &b)| poses[b] == in_copy(poses[a], from, to))
                })
            })
    }
}

/// All exemplars of a group read this same board state, including prior groups' kept poses.
struct GroupTrials {
    search: Arc<ConformSearch>,
    copies: Vec<Vec<usize>>,
    poses: Vec<Pose>,
    anchor: usize,
    cell: Nm,
}

struct Candidate {
    members: Vec<Pose>,
    over: i64,
    wirelength: i64,
    broken: usize,
}

impl GroupTrials {
    fn evaluate(&self, index: usize) -> Option<Candidate> {
        let search = &self.search;
        let trial = search.trial(&self.poses, &self.copies, self.anchor, index)?;
        Some(Candidate {
            members: self.copies.iter().flatten().map(|&m| trial[m]).collect(),
            over: congestion(&search.board, &search.problem, &trial, self.cell).over(),
            wirelength: search.wirelength(&trial),
            broken: search.broken(&trial),
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn conform(
    board: &Board, circuit: &Circuit, problem: &PlaceProblem, intents: &Intents, halos: &BTreeMap<ItemId, Nm>, opts: &SeedOpts,
    poses: &mut [Pose], report: &mut SeedReport, mut over: i64, seat_rows: &BTreeMap<BlockId, ConformRow>, watch: &mut dyn SeedWatch,
    pool: Option<&Arc<dyn Spawn>>,
) -> i64 {
    let groups = identical_instances(board, circuit);
    if groups.is_empty() { return over }
    let search = Arc::new(ConformSearch::new(board, circuit, problem, report, halos, opts));
    let cell = default_cell(board);
    let mut wirelength = search.wirelength(poses);
    let mut broken = search.broken(poses);
    let layout = GroupLayout::new(board, circuit, problem, intents, poses);
    let tell = |row: ConformRow, report: &mut SeedReport, poses: &[Pose], over, watch: &mut dyn SeedWatch| {
        let kept = row.status == ConformStatus::Kept;
        report.conformed.push(row);
        !kept || watch.kept(&SeedRound { round: report.rounds, poses, unplaced: &report.unplaced, overflow: Some(over) })
    };
    let of = groups.len();
    for (index, copies) in groups.into_iter().enumerate() {
        let block = copies[0].block;
        if !watch.matching(index, of, block) { break }
        if let Some(row) = seat_rows.get(&block) {
            // These poses already stood (or were refused) at round zero.
            if !tell(row.clone(), report, poses, over, watch) { break }
            continue
        }
        let start = Instant::now();
        let mut row = ConformRow::new(&copies);
        let members = copies.iter().flat_map(|copy| &copy.members);
        let setup = if members.clone().filter_map(|c| layout.movable.get(c)).any(|m| report.unplaced.binary_search(m).is_ok()) {
            Err(ConformStatus::SkippedUnplaced)
        } else {
            layout.anchor(&copies, problem, poses)
        };
        match setup {
            Err(status) => row.status = status,
            Ok((GroupAnchor::Held(_), mates)) if !mates[0].is_empty() => {
                // The unseated control leaves these mates to the ordinary pass.
                row.status = ConformStatus::SkippedFixed;
            }
            Ok((GroupAnchor::Held(_), _)) => {
                // Every part already stands; each copy was checked against the anchor frame.
                row.status = ConformStatus::Kept;
                row.exemplar = Some(0);
                row.passed = row.copies;
                row.delta_over = Some(0);
                row.delta_wirelength = Some(0);
            }
            Ok((GroupAnchor::Free(anchor), copies)) => {
                let group = Arc::new(GroupTrials { search: search.clone(), poses: poses.to_vec(), copies, anchor, cell });
                let work = group.clone();
                let trials = pool_trials(row.copies, pool, move |index| {
                    let start = Instant::now();
                    (work.evaluate(index), start.elapsed())
                });
                // A complete trial places every member; the unplaced count is invariant. Rank passing
                // trials first, or report the lowest-overflow refusal when none passes.
                let best = trials.into_iter().enumerate().filter_map(|(index, slot)| {
                    let (candidate, time) = slot?;
                    row.trial_time += time;
                    let candidate = candidate?;
                    let status = if candidate.broken > broken { ConformStatus::RefusedNear }
                        else if candidate.over > over { ConformStatus::RefusedOver }
                        else { ConformStatus::Kept };
                    row.passed += usize::from(status == ConformStatus::Kept);
                    Some((index, status, candidate))
                }).min_by_key(|(index, status, c)| (*status != ConformStatus::Kept, c.over, c.wirelength, *index));

                if let Some((index, status, best)) = best {
                    row.exemplar = Some(index);
                    row.status = status;
                    row.delta_over = Some(best.over - over);
                    row.delta_wirelength = Some(best.wirelength - wirelength);
                    for (&m, pose) in group.copies.iter().flatten().zip(best.members) {
                        row.measure_move(poses[m], pose);
                        if status == ConformStatus::Kept { poses[m] = pose }
                    }
                    if status == ConformStatus::Kept { (over, wirelength, broken) = (best.over, best.wirelength, best.broken) }
                }
            }
        }
        row.wall_time = start.elapsed();
        if !tell(row, report, poses, over, watch) { break }
    }
    over
}

struct GroupLayout {
    movable: BTreeMap<CompId, usize>,
    standing: BTreeMap<CompId, Pose>,
    edges: BTreeSet<CompId>,
}

impl GroupLayout {
    fn new(board: &Board, circuit: &Circuit, problem: &PlaceProblem, intents: &Intents, poses: &[Pose]) -> Self {
        let movable: BTreeMap<_, _> = problem.movables.iter().enumerate().map(|(m, p)| (p.comp, m)).collect();
        let standing = board.placed.iter().filter_map(|p| {
            let pose = match movable.get(&p.comp) {
                None => Pose::from(p),
                Some(&m) if intents.stays.contains(&p.comp) => Pose {
                    side: intents.sides.get(&p.comp).copied().unwrap_or(poses[m].side), ..poses[m]
                },
                _ => return None,
            };
            Some((p.comp, pose))
        }).collect();
        let edges = edge_slots(board, circuit).into_iter().map(|s| s.0).filter(|c| !intents.stays.contains(c)).collect();
        Self { movable, standing, edges }
    }

    fn anchor(&self, copies: &[Instance], problem: &PlaceProblem, poses: &[Pose]) -> Result<(GroupAnchor, Vec<Vec<usize>>), ConformStatus> {
        if copies.iter().flat_map(|c| &c.members).any(|c| self.edges.contains(c)) { return Err(ConformStatus::SkippedFixed) }
        group_anchor(copies, problem, poses, &self.movable, &self.standing)
    }
}

/// The frame for a copy: a searched member, or the standing pose of each copy's first held member.
enum GroupAnchor {
    Free(usize),
    Held(Vec<Pose>),
}

/// Transport a pose between anchor frames. Mirroring in local X reverses the relative turn.
/// Compose the two turns before rotating so identical frames preserve even non-cardinal poses.
fn in_copy(pose: Pose, from: Pose, to: Pose) -> Pose {
    let offset = pose.at.sub(from.at);
    if from.side == to.side {
        let turn = to.rot.sub(from.rot);
        Pose { at: to.at.add(offset.rotated(turn)), rot: pose.rot.add(turn), side: pose.side }
    } else {
        let turn = to.rot.add(from.rot);
        Pose { at: to.at.add(offset.mirrored_x(0).rotated(turn)), rot: turn.sub(pose.rot), side: pose.side.flipped() }
    }
}

/// Resolve each copy's free movables and the frame they share with the exemplar.
fn group_anchor(
    copies: &[Instance], problem: &PlaceProblem, poses: &[Pose], movable: &BTreeMap<CompId, usize>,
    standing: &BTreeMap<CompId, Pose>,
) -> Result<(GroupAnchor, Vec<Vec<usize>>), ConformStatus> {
    let first = &copies[0].members;
    let anchor = first.iter().position(|c| standing.contains_key(c));
    let mut frames = Vec::new();
    let mut free = Vec::new();
    for copy in copies {
        let mut members = Vec::new();
        for (&c, &reference) in copy.members.iter().zip(first) {
            if standing.contains_key(&c) != standing.contains_key(&reference) {
                return Err(ConformStatus::SkippedFixed)
            }
            if !standing.contains_key(&c) { members.push(movable[&c]) }
        }
        if let Some(a) = anchor {
            frames.push(standing[&copy.members[a]]);
        } else if members.iter().any(|&m| poses[m].side != poses[members[0]].side) {
            return Err(ConformStatus::SkippedMixedSides)
        }
        free.push(members);
    }
    if anchor.is_some() {
        for (copy, &frame) in copies.iter().zip(&frames) {
            for (&c, reference) in copy.members.iter().zip(first) {
                if let Some(&pose) = standing.get(reference) {
                    if standing[&c] != in_copy(pose, frames[0], frame) { return Err(ConformStatus::HeldApart) }
                }
            }
        }
        Ok((GroupAnchor::Held(frames), free))
    } else {
        let a = (0..first.len()).min_by_key(|&k| std::cmp::Reverse(problem.movables[free[0][k]].pads.len()))
            .expect("a nonempty copy");
        Ok((GroupAnchor::Free(a), free))
    }
}

/// What the circuit says of where its parts go.
struct Intents {
    /// Parts that keep their pose: fixed (to a spot, they stand on it).
    stays: BTreeSet<CompId>,
    /// Parts a `side` intent puts on a side.
    sides: BTreeMap<CompId, Side>,
    /// Parts fixed to an edge, with the rotation that faces them out across it.
    edges: BTreeMap<CompId, (Edge, Angle)>,
}

/// The parts the circuit's edge intents put on an edge, with the edge and the rotation the intent
/// names: the part an intent names, or of a block its parts with a plug port (its connectors),
/// else its part with the most pads. An intent that also names a spot fixes the part there.
pub(crate) fn edge_slots(board: &Board, circuit: &Circuit) -> Vec<(CompId, Edge, Option<Angle>)> {
    let plug = |c: CompId| circuit.comps[c.ix()].part.body.as_ref().is_some_and(|b| b.ports.iter().any(|p| p.kind == BodyPortKind::Plug));
    let mut slots = Vec::new();
    for intent in &circuit.intents {
        let Intent::Fixed { t, edge: Some(edge), at: None, rot } = intent else { continue };
        let parts = match *t {
            Target::Comp(c) | Target::Pin(c, _) => vec![c],
            Target::Block(b) => {
                let all = circuit.block_comps(b);
                let plugs: Vec<CompId> = all.iter().copied().filter(|&c| plug(c)).collect();
                if plugs.is_empty() {
                    all.into_iter().max_by_key(|&c| (board.placed_of(c).map_or(0, |p| p.footprint.pads.len()), std::cmp::Reverse(c))).into_iter().collect()
                } else {
                    plugs
                }
            }
        };
        slots.extend(parts.into_iter().map(|c| (c, *edge, *rot)));
    }
    slots
}

/// The rotation that faces a part out across `edge` on `side`: its plug port's direction out of
/// the board (within about 45 degrees), else its pads' box inward of its courtyard's; then the
/// courtyard shallowest across the edge (a header lies along it), then `start`, then the lower angle.
pub(crate) fn edge_rotation(part: &Placed, body: Option<&Body3d>, side: Side, edge: Edge, start: Angle) -> Angle {
    let (nx, ny): (i64, i64) = match edge { Edge::Left => (-1, 0), Edge::Right => (1, 0), Edge::Bottom => (0, -1), Edge::Top => (0, 1) };
    let out = |rot: Angle| body.map_or(f64::MIN, |b| b.ports.iter().filter(|p| p.kind == BodyPortKind::Plug).map(|p| {
        let d = b.offset.rotate(p.dir);
        let x = if side == Side::Bottom { -d[0] } else { d[0] };
        let (sin, cos) = rot.radians().sin_cos();
        (x * cos - d[1] * sin) * nx as f64 + (x * sin + d[1] * cos) * ny as f64
    }).fold(f64::MIN, f64::max));
    let rots = [Angle::ZERO, Angle::QUARTER, Angle::HALF, Angle::deg(270.0)];
    rots.into_iter().enumerate().max_by_key(|&(k, rot)| {
        let court = body_at(part, P::ZERO, rot, side, true).bbox();
        let pads = pads_box(part, rot, side);
        let (pc, cc) = (pads.center(), court.center());
        let inward = -(i64::from(pc.x - cc.x) * nx + i64::from(pc.y - cc.y) * ny);
        let depth = if nx != 0 { court.width() } else { court.height() };
        (out(rot) > 0.7, inward, -i64::from(depth), rot == start, std::cmp::Reverse(k))
    }).map_or(start, |(_, rot)| rot)
}

/// One constructive pass: the parts of `first` in that order, then the rest by pull.
#[allow(clippy::too_many_arguments)]
fn pass(
    board: &Board, problem: &PlaceProblem, found: &Clusters, intents: &Intents, seats: &BTreeMap<usize, Pose>, bodies: &Bodies, first: &[usize], poses: &mut [Pose], grid: Nm,
    halos: &BTreeMap<ItemId, Nm>, demand: Option<Demand<'_>>, rooms: &[PortRoom],
) -> SeedReport {
    let n = poses.len();
    let mut anchor: Vec<_> = problem.movables.iter().map(|mv| intents.stays.contains(&mv.comp)).collect();
    for (&m, &pose) in seats {
        poses[m] = pose;
        anchor[m] = true;
    }
    // Anchors stand where `poses` has them (on their intent's side), not where the board does.
    let moving: BTreeSet<_> = problem.movables.iter().map(|m| m.item).collect();
    let mut free = Free::with_rooms(board, &moving, BTreeSet::new(), rooms);
    let decap: BTreeSet<CompId> = found.decoupling.iter().map(|&(cap, _)| cap).collect();
    if !halos.is_empty() {
        let of_ic: BTreeMap<ItemId, ItemId> = found.decoupling.iter()
            .filter_map(|&(cap, ic)| Some((board.placed_of(cap)?.id, board.placed_of(ic)?.id))).collect();
        let halos = halos.clone();
        let body = of_ic.clone();
        free = free.with_halo(
            move |p: &Placed| halos.get(of_ic.get(&p.id).unwrap_or(&p.id)).copied().unwrap_or(0),
            move |p: &Placed| body.get(&p.id).copied().unwrap_or(p.id),
        );
    }
    for (m, mv) in problem.movables.iter().enumerate().filter(|&(m, _)| anchor[m]) {
        poses[m].side = intents.sides.get(&mv.comp).copied().unwrap_or(poses[m].side);
        free.put(mv.item, poses[m]);
    }
    let mut placed = anchor.clone();

    // Each movable's nets, with its own pads on each.
    let mut incident: Vec<Vec<(usize, Vec<PadIx>)>> = vec![Vec::new(); n];
    for (i, net) in problem.nets.iter().enumerate() {
        for pin in &net.pins {
            let PinRef::Mov(m, pad) = *pin else { continue };
            match incident[m].last_mut() {
                Some((j, pads)) if *j == i => pads.push(pad),
                _ => incident[m].push((i, vec![pad])),
            }
        }
    }
    let middle = if board.outline.is_empty() {
        board.placed.iter().fold(BBox::EMPTY, |b, p| b.union(p.bbox())).center()
    } else {
        board.outline_bbox().center()
    };

    // Each net's standing pins, as their box: it only grows, as parts go on and never move.
    let mut standing: Vec<BBox> = problem.nets.iter().map(|net| BBox::of_points(net.pins.iter().filter_map(|pin| match *pin {
        PinRef::Fixed(p) => Some(p),
        PinRef::Mov(j, pad) => placed[j].then(|| problem.movables[j].pad_at(pad, poses[j])),
    }))).collect();

    let mut report = SeedReport { decoupling: found.decoupling.clone(), ..SeedReport::default() };
    // Parts fixed to an edge go first and slide along it to where what stands pulls them (the
    // edge's middle when nothing does): they are anchors the rest gathers round.
    for m in 0..n {
        let Some(&(edge, rot)) = intents.edges.get(&problem.movables[m].comp) else { continue };
        if placed[m] { continue }
        let side = intents.sides.get(&problem.movables[m].comp).copied().unwrap_or(poses[m].side);
        let (want, own) = want(problem, &found.pulls[m], &incident[m], &standing, &placed, poses, &found.of, m, middle);
        let want = own.map_or(want, |pad| want.sub(problem.movables[m].pad_at(pad, Pose { at: P::ZERO, rot, side })));
        let pose = Pose { at: want, rot, side };
        let item = problem.movables[m].item;
        match free.nearest_on_edge(item, pose, grid, edge).or_else(|| free.nearest(item, pose, grid)) {
            Some(at) => {
                placed[m] = true;
                stand(problem, &incident[m], &mut standing, &mut free, poses, m, Pose { at, ..pose });
            }
            None => report.unplaced.push(m),
        }
    }
    let mut current = None;
    loop {
        // How strongly what stands holds movable m.
        let pull = |m: usize| -> i64 {
            incident[m].iter().filter(|(i, _)| !standing[*i].is_empty()).map(|(i, _)| problem.nets[*i].weight).sum()
        };
        let pads = |m: usize| incident[m].iter().map(|(_, p)| p.len()).sum::<usize>();
        let best = |pick: &mut dyn Iterator<Item = usize>| pick
            .map(|m| (pull(m), pads(m), std::cmp::Reverse(problem.movables[m].comp), m))
            .max().map(|t| t.3);
        // A capacitor of an IC's body waits for its IC (it goes on with it, or after it when it
        // does not fit there).
        let held = |m: usize| bodies.iter().any(|(&i, members)| !placed[i] && !report.unplaced.contains(&i) && members.iter().any(|&(c, ..)| c == m));
        let open = |m: &usize| !placed[*m] && !report.unplaced.contains(m) && !held(*m);
        // A part whose pull stands (a decoupling capacitor once its IC does) goes next, before
        // anything else takes the room by its pin.
        let tied = |m: &usize| found.pulls[*m].iter().any(|(p, ..)| match *p {
            Pull::Pad(PinRef::Fixed(_)) => true,
            Pull::Pad(PinRef::Mov(j, _)) | Pull::Part(j) => placed[j],
        });
        let next = first.iter().copied().find(open)
            .or_else(|| best(&mut (0..n).filter(open).filter(tied)))
            .or_else(|| current.and_then(|c| best(&mut (0..n).filter(open).filter(|&m| found.of[m] == c)))
            .or_else(|| best(&mut (0..n).filter(open))));
        let Some(m) = next else { break };
        current = Some(found.of[m]);

        let (want, own) = want(problem, &found.pulls[m], &incident[m], &standing, &placed, poses, &found.of, m, middle);
        // A part no pull holds goes where the round before left the least traffic.
        let want = match demand {
            Some(d) if !tied(&m) => quiet(board, d.map, problem, &incident[m], &standing, m, poses[m], d.grow_from.map(|last| last[m]), want),
            _ => want,
        };
        // The `near` intents whose other end stands: each spot is judged against their max.
        let nears: Vec<(Pull, Option<PadIx>, Nm)> = found.pulls[m].iter().filter_map(|&(to, own, max)| {
            let stands = match to { Pull::Pad(PinRef::Fixed(_)) => true, Pull::Pad(PinRef::Mov(j, _)) | Pull::Part(j) => placed[j] };
            Some((to, own, max?)).filter(|_| stands)
        }).collect();
        let side = intents.sides.get(&problem.movables[m].comp).copied().unwrap_or(poses[m].side);
        let mut choice: Option<((i64, i128, usize), Pose)> = None;
        for (r, rot) in [Angle::ZERO, Angle::QUARTER, Angle::HALF, Angle::deg(270.0)].into_iter().enumerate() {
            // A pull on one of the part's pads wants that pad, not the anchor, at the point.
            let want = own.map_or(want, |pad| want.sub(problem.movables[m].pad_at(pad, Pose { at: P::ZERO, rot, side })));
            let (item, wanted) = (problem.movables[m].item, Pose { at: want, rot, side });
            let within = |at: P| nears.iter().all(|&(to, own, max)| near_gap(board, problem, poses, m, own, Pose { at, rot, side }, to) <= max);
            // The members of its body at the IC's anchor `at`.
            let members = |at: P| bodies.get(&m).into_iter().flatten().filter(|&&(c, ..)| !placed[c])
                .map(move |&(c, rel, drot)| (c, Pose { at: at.add(rel.rotated(rot)), rot: rot.add(drot), side }));
            // A decoupling capacitor sits by its pin whatever the spacing. A `near` end takes the
            // nearest spot within the intent's max, spaced if one is, else packed; an IC with a
            // body, the nearest where its capacitors fit too. Only where no spot is, the nearest
            // after all (the intent then shows as broken; spacing never leaves a part out).
            let at = if decap.contains(&problem.movables[m].comp) {
                (!nears.is_empty()).then(|| free.nearest_with(item, wanted, grid, &within)).flatten()
                    .or_else(|| free.nearest(item, wanted, grid))
            } else if bodies.contains_key(&m) || !nears.is_empty() {
                let fit = |at: P| within(at) && members(at).all(|(c, pose)| free.fits(problem.movables[c].item, pose));
                free.nearest_spaced_with(item, wanted, grid, &fit).or_else(|| free.nearest_spaced(item, wanted, grid))
            } else {
                free.nearest_spaced(item, wanted, grid)
            };
            let Some(at) = at else { continue };
            let pose = Pose { at, rot, side };
            let length: i64 = incident[m].iter().map(|(i, own)| {
                let mine = BBox::of_points(own.iter().map(|&pad| problem.movables[m].pad_at(pad, pose)));
                problem.nets[*i].wirelength(standing[*i].union(mine))
            }).sum();
            let key = (length, at.dist2(want), r);
            if choice.as_ref().is_none_or(|(k, _)| key < *k) {
                choice = Some((key, pose));
            }
        }
        match choice {
            Some((_, pose)) => {
                placed[m] = true;
                stand(problem, &incident[m], &mut standing, &mut free, poses, m, pose);
                // Its body's capacitors go on with it where they fit.
                let members: Vec<(usize, P, Angle)> = bodies.get(&m).into_iter().flatten().copied().filter(|&(c, ..)| !placed[c]).collect();
                for (c, rel, drot) in members {
                    let at = Pose { at: pose.at.add(rel.rotated(pose.rot)), rot: pose.rot.add(drot), side: pose.side };
                    if free.fits(problem.movables[c].item, at) {
                        placed[c] = true;
                        stand(problem, &incident[c], &mut standing, &mut free, poses, c, at);
                    }
                }
            }
            None => report.unplaced.push(m),
        }
    }
    report.unplaced.sort_unstable();
    report
}

/// Movable `m` goes on at `pose`: its pins stand, and it is an obstacle.
fn stand(problem: &PlaceProblem, incident: &[(usize, Vec<PadIx>)], standing: &mut [BBox], free: &mut Free<'_>, poses: &mut [Pose], m: usize, pose: Pose) {
    poses[m] = pose;
    for (i, own) in incident {
        standing[*i] = standing[*i].union(BBox::of_points(own.iter().map(|&pad| problem.movables[m].pad_at(pad, pose))));
    }
    free.put(problem.movables[m].item, pose);
}

/// How far apart a `near` intent's ends are with movable `m` at `pose`: from its own pad (its
/// courtyard's box when the intent names no pin) to the other end's pad or courtyard box.
fn near_gap(board: &Board, problem: &PlaceProblem, poses: &[Pose], m: usize, own: Option<PadIx>, pose: Pose, to: Pull) -> Nm {
    let court = |j: usize, pose: Pose| {
        let b = board.placed_item(problem.movables[j].item).map_or(BBox::EMPTY, |p| body_at(p, pose.at, pose.rot, pose.side, true).bbox());
        if b.is_empty() { BBox::point(pose.at) } else { b }
    };
    let a = own.map_or_else(|| court(m, pose), |pad| BBox::point(problem.movables[m].pad_at(pad, pose)));
    let b = match to {
        Pull::Pad(PinRef::Fixed(p)) => BBox::point(p),
        Pull::Pad(PinRef::Mov(j, pad)) => BBox::point(problem.movables[j].pad_at(pad, poses[j])),
        Pull::Part(j) => court(j, poses[j]),
    };
    let dx = (a.min.x - b.max.x).max(b.min.x - a.max.x).max(0);
    let dy = (a.min.y - b.max.y).max(b.min.y - a.max.y).max(0);
    f64::from(dx).hypot(f64::from(dy)).ceil() as Nm
}

/// The point movable `m` is pulled to by what stands, and its own pad that point is for, if any.
#[allow(clippy::too_many_arguments)]
fn want(
    problem: &PlaceProblem, pulls: &[(Pull, Option<PadIx>, Option<Nm>)], incident: &[(usize, Vec<PadIx>)], standing: &[BBox], placed: &[bool], poses: &[Pose],
    cluster: &[usize], m: usize, middle: P,
) -> (P, Option<PadIx>) {
    for &(pull, own, _) in pulls {
        match pull {
            Pull::Pad(PinRef::Fixed(p)) => return (p, own),
            Pull::Pad(PinRef::Mov(j, pad)) if placed[j] => return (problem.movables[j].pad_at(pad, poses[j]), own),
            Pull::Part(j) if placed[j] => return (poses[j].at, own),
            _ => {}
        }
    }
    // The weighted centre of each net's standing pins.
    let (mut sx, mut sy, mut sw) = (0i128, 0i128, 0i128);
    for (i, _) in incident {
        let net = &problem.nets[*i];
        if net.weight == 0 || standing[*i].is_empty() { continue }
        let c = standing[*i].center();
        let w = i128::from(net.weight / (PNet::UNIT_WEIGHT >> 16)).max(1);
        sx += i128::from(c.x) * w;
        sy += i128::from(c.y) * w;
        sw += w;
    }
    if sw > 0 {
        return (P::new((sx / sw) as Nm, (sy / sw) as Nm), None);
    }
    let mates: Vec<P> = (0..placed.len()).filter(|&j| j != m && placed[j] && cluster[j] == cluster[m]).map(|j| poses[j].at).collect();
    if !mates.is_empty() {
        return (BBox::of_points(mates.into_iter()).center(), None);
    }
    (middle, None)
}

/// The old weighted median interval, grown on each side by the wire the previous
/// courtyard's overflow can pay for. Each standing net contributes two endpoints.
pub(super) fn quiet_interval(ends: &mut [(Nm, i64)], over: i64, min: Nm, max: Nm) -> (Nm, Nm) {
    ends.sort_unstable();
    // Keep the median's original weight precision. The growth is charged at full net weight.
    let median_weight = |w: i64| i128::from((w / (PNet::UNIT_WEIGHT >> 16)).max(1));
    let total = ends.iter().map(|&(_, w)| median_weight(w)).sum::<i128>();
    let median = |sum: &mut i128, w| { *sum += median_weight(w); 2 * *sum >= total };
    let (mut left, mut right) = (0, 0);
    let lo = ends.iter().find(|&&(_, w)| median(&mut left, w)).unwrap().0;
    let hi = ends.iter().rev().find(|&&(_, w)| median(&mut right, w)).unwrap().0.max(lo);
    if over == 0 { return (lo, hi) }

    let total = ends.iter().map(|&(_, w)| i128::from(w)).sum::<i128>() / 2;
    let budget = i128::from(over) * i128::from(PNet::UNIT_WEIGHT);
    // Beyond the last endpoint every net stretches. Working back to the median edge,
    // each endpoint subtracts one net weight from that slope. Crossing it outward adds it back.
    fn reach<'a>(edge: Nm, limit: Nm, dir: i128, total: i128, mut budget: i128,
        ahead: impl Iterator<Item = &'a (Nm, i64)> + Clone) -> Nm {
        let mut slope = total - ahead.clone().map(|&(_, w)| i128::from(w)).sum::<i128>();
        let mut at = i128::from(edge);
        for (next, w) in ahead.copied().take_while(|&(p, _)| dir * (i128::from(p) - i128::from(limit)) < 0)
            .chain(std::iter::once((limit, 0))) {
            let distance = dir * (i128::from(next) - at);
            let cost = distance * slope;
            if cost > budget { return (at + dir * (budget / slope)) as Nm }
            budget -= cost;
            at = i128::from(next);
            slope += i128::from(w);
        }
        limit
    }
    let left = ends.partition_point(|&(p, _)| p < lo);
    let right = ends.partition_point(|&(p, _)| p <= hi);
    (reach(lo, min.min(lo), -1, total, budget, ends[..left].iter().rev()).clamp(min, max),
     reach(hi, max.max(hi), 1, total, budget, ends[right..].iter()).clamp(min, max))
}

/// The quietest point in the median box, including any growth paid for by overflow.
/// Of each cell the region touches, its point nearest `old` is judged by the demand in
/// the cells under the courtyard there (by area), its own nets' wire left out. The least
/// wins, ties by distance to `old`, then row and column. A region within one cell keeps `old`.
#[allow(clippy::too_many_arguments)]
fn quiet(board: &Board, map: &CongestionMap, problem: &PlaceProblem, incident: &[(usize, Vec<PadIx>)], standing: &[BBox],
    m: usize, pose: Pose, last: Option<Pose>, old: P) -> P {
    let (mut xs, mut ys, mut own) = (Vec::new(), Vec::new(), BTreeSet::new());
    for (i, _) in incident {
        let (net, b) = (&problem.nets[*i], standing[*i]);
        if net.weight == 0 || b.is_empty() { continue }
        xs.extend([(b.min.x, net.weight), (b.max.x, net.weight)]);
        ys.extend([(b.min.y, net.weight), (b.max.y, net.weight)]);
        own.insert(net.net);
    }
    if xs.is_empty() { return old }
    let part = board.placed_item(problem.movables[m].item);
    let over = part.zip(last).map_or(0, |(p, last)| {
        let court = body_at(p, last.at, last.rot, last.side, true).bbox();
        map.cells_under(court).map(|i| map.overflow(i)).max().unwrap_or(0)
    });
    let board_box = board.outline_bbox();
    let (x0, x1) = quiet_interval(&mut xs, over, board_box.min.x, board_box.max.x);
    let (y0, y1) = quiet_interval(&mut ys, over, board_box.min.y, board_box.max.y);
    let region = BBox::new(P::new(x0, y0), P::new(x1, y1));
    let cells: Vec<usize> = map.cells_under(region).collect();
    if cells.len() < 2 {
        return old;
    }
    let court = part.map_or(BBox::EMPTY, |p| body_at(p, P::ZERO, pose.rot, pose.side, true).bbox());
    let court = if court.is_empty() { BBox::point(P::ZERO) } else { court };
    let load = |at: P| -> i128 {
        let b = BBox::new(court.min.add(at), court.max.add(at));
        map.cells_under(b).map(|i| {
            let c = map.cell_box(i);
            let dx = (i64::from(b.max.x.min(c.max.x)) - i64::from(b.min.x.max(c.min.x))).max(1);
            let dy = (i64::from(b.max.y.min(c.max.y)) - i64::from(b.min.y.max(c.min.y))).max(1);
            let mine: i64 = map.wire_in(i).iter().filter(|(n, _)| own.contains(n)).map(|e| e.1).sum();
            i128::from(map.demand[i] - mine) * i128::from(dx) * i128::from(dy)
        }).sum()
    };
    let pick = |v: Nm, lo: Nm, hi: Nm| v.clamp(lo, hi.max(lo));
    cells.into_iter().map(|i| {
        let c = map.cell_box(i);
        let at = P::new(pick(old.x, x0.max(c.min.x), x1.min(c.max.x)), pick(old.y, y0.max(c.min.y), y1.min(c.max.y)));
        ((load(at), at.dist2(old), i), at)
    }).min().map_or(old, |(_, at)| at)
}

/// Raise each part's halo for the tracks that overflow where it stands in `poses`: the pitch times
/// half the overflow, in tracks, of the worst cell of `map` its courtyard touches (half, as two
/// neighbours' halos add up: each overflowing track gets one pitch of room). A part fixed in place
/// keeps its spot, and its halo still keeps others off. A halo never shrinks.
fn raise(board: &Board, problem: &PlaceProblem, poses: &[Pose], map: &CongestionMap, pitch: Nm, halos: &mut BTreeMap<ItemId, Nm>) {
    let moved: BTreeMap<ItemId, Pose> = problem.movables.iter().zip(poses).map(|(m, &pose)| (m.item, pose)).collect();
    for p in &board.placed {
        let pose = moved.get(&p.id).copied().unwrap_or(Pose { at: p.at, rot: p.rot, side: p.side });
        let court = body_at(p, pose.at, pose.rot, pose.side, true).bbox();
        let worst = map.cells_under(court).map(|i| map.overflow(i)).max().unwrap_or(0);
        let halo = (i128::from(pitch) * i128::from(worst) / (2 * i128::from(map.cell))) as Nm;
        if halo > 0 {
            let h = halos.entry(p.id).or_insert(0);
            *h = (*h).max(halo);
        }
    }
}
