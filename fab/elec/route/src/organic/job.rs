//! Fab's side of the `@organic` method (not a port): the grid router routes, the ported relaxer
//! relaxes every net it routed, and our verifier judges the result against the real rules before
//! it leaves the job.
//!
//! What the job relaxes, each a ledger row:
//! - the copper the grid router made in this job: its tracks move, its vias move
//!   (`enable_dynamic_vias`) and the net simplifier rebuilds its nets
//!   (`enable_loop_simplification`; a net that also has other copper keeps its topology, the
//!   relaxer's K4 rule). Every other track is locked and every other via is fixed, and so is the
//!   copper of a net with a pour: the relaxer does not see pours, that copper ties the net's pads
//!   to its pour, and putting such a net (often board-wide) back in grid form would collide with
//!   every relaxed neighbour that moved into the room it left. Such a net never lifts: every
//!   partial and the result carry its grid copper whole;
//! - a junction (`Anchor::Track`) a moving track ends in becomes the relaxer's branch link: the
//!   host gets a node at the junction and the branch end follows that node. A junction at a
//!   host's end is two tracks meeting end to end: on a via the branch end rides the via with the
//!   host's end, otherwise it stays where it is;
//! - before the relax, the pre stage runs the finish passes `Finish` switches on (Cleanup,
//!   ViaHops and LayerReassign by default, as in the reference) on the relaxer's board, in the
//!   reference's order, each judged by one judge (`judge`) where the reference runs its DRC. The
//!   judge's accepted state starts as the grid form, its lift is the `points` form (the node
//!   polylines), and a unit it refuses the pass restores itself. When the judge accepted a
//!   change, the lift is rebased on the accepted state: that state is the form a net falls back
//!   to, its ends keep that form's exact end points and its copper that form's ids (the rest
//!   below reads "grid form" as that state). A job cancelled in the pre stage returns the
//!   accepted state;
//! - the relaxer runs with `OrganicSettings::base()` plus those two (and, with `Finish::pairs`, diff-pair
//!   coupling and skew tuning) in steps of `STEP_UNITS`
//!   evaluation units until its own test says settled (a step with no node or via awake) or the
//!   job is cancelled (a newer request cancels it: latest wins). There is no step count: every
//!   state is legal, the partials keep coming while it settles (the worker labels them
//!   "route (settling)"), and a cancelled job's last partial stands;
//! - a net lifts as a whole: its vias at their new places, each of its traces a polyline track
//!   on the nm grid along the exported curve. A pad end or a fixed end keeps the exact end point
//!   of its own grid track (else of the net's nearest grid end), a via end sits on its via, a
//!   branch end on its host's new centreline. A grid via an end has left (moved, or pruned by the
//!   simplifier) is no anchor of it. A track keeps the Fab id of the grid track whose port id it
//!   carries when that track was on its net (the simplifier reuses the ids of deleted traces of
//!   any net). Any other track gets `fresh + 2 * port_id`, where `fresh` is the applied grid
//!   board's `next_item`, or the judge's next id after the rebase. A via keeps its Fab id, its
//!   exact place when it did not move, and its layers as the port leaves them. The rebase tags
//!   pass-added vias with their accepted Fab ids. Other added vias get stable tags in port
//!   order and ids `fresh + 2 * (tag - via_table_len) + 1`, the net's via size and the grid
//!   router's tenting. Trace and via offsets have different parity; an id that does not fit
//!   below `u32::MAX` prevents its net from lifting;
//! - a net keeps its grid form when it does not lift (a curve without a segment, a junction the
//!   relaxer let go of while its host moved, a cycle of junctions, no fresh id left), when it
//!   connects fewer of its pads than its grid form (the port's `net_pad_groups`), or when the
//!   verifier finds a collision on it. Verification repeats until no relaxed net collides: a net
//!   in grid form was legal against grid-form neighbours, and every collision with a relaxed net
//!   is found from the relaxed side;
//! - the final solution is then applied, its pours refilled and its connectivity compared with
//!   the grid form's: a net with more missing connections (ratsnest lines) than in grid form
//!   reverts when it is relaxed; when it is not (a relaxed track cut its pour), the relaxed nets
//!   nearest to each of its missing connections revert. A missing connection joins the nearest
//!   points of two pieces the cut separates, so the copper that separates them lies across it;
//!   the nearest are the nets whose copper comes closest to the connection's segment (a track by
//!   its centreline less half its width, a via by its centre less half its pad; every net at
//!   that distance, to the nm). Both checks repeat until nothing reverts, each round reverting at
//!   least one net, so a cut the nearest nets do not make reverts the next nearest; every other
//!   relaxed net keeps its relaxed form. A solution that does not apply keeps the grid form
//!   whole. So the grid solution's `open` and `repaired` claims hold for the result.
//! - once the relax has settled, the nets that went back to grid form first are held: a net that
//!   does not lift or connects fewer pads, a net the first verification pass rejects, and the
//!   nets nearest a cut in the first connectivity check. Every later revert follows from them: a
//!   net that collides with the grid form of a net that went back before it. A hold round starts
//!   the relax again from the start form (the relaxer's board as it stood before the pour paths),
//!   with every held net locked there and a fresh relaxer: a held net's start form mixed into
//!   copper that relaxed without it could cross that copper, and the guard cannot undo a
//!   crossing, so a round starts from the one state without crossings the job owns. A net that
//!   went back only by such a collision is not held, so it relaxes again around the held nets.
//!   A held net stays in grid form in the result. No partial is offered once a net is held: the
//!   round's relax starts from the grid form, so it would show the job going back and then
//!   relaxed again. While nets still go back first, the checks stop
//!   once that first wave is known (one connectivity check, not the whole cascade of reverts it
//!   starts); a settled relax with no first wave is checked to the end. Each round holds at least
//!   one more net, so the job ends; a cancelled job stops holding and checks its last state to
//!   the end.
//! - a pour's connections are laid as pour paths before the relax (`pour`): a spanning forest of
//!   each pour net's hard groups through the start form's fill, as traces of the pour's net at the
//!   zone's minimum width plus a margin, on a clearance row of their own (the zone's gap plus the
//!   adapter's margin against every row). The margin is owed only to copper that moves: a stretch
//!   where a path of that width fits against the fixed-only fill (the start form without the
//!   job's moving copper; in a hold round the held nets count as fixed too) moves; one that fixed
//!   copper or a mask bounds is locked. The routes are laid once; a hold round classifies them
//!   again and lays them on its fresh board. Either way the
//!   relaxer's guard pushes every moving net that far off it, also a neighbour that starts inside
//!   the margin, so the refilled fill holds each path and the pour never splits where a path
//!   runs; the fill's static masks are keepouts only the pour's net feels, a path slides over its
//!   own net's copper, and the loop simplifier leaves its net alone. The lift skips them: the fill
//!   takes their room. A start-form join the forest cannot lay (a pinch no core passes) is counted
//!   in `pour_unprotected`, a path segment that leaves the refilled fill's core at the end in
//!   `pour_escaped`, and a path with moving copper still under its clearance beyond the margin in
//!   `pour_tight`; the connectivity check and the hold stay their judge.
//!
//! Partial solutions are offered at every relax step and accepted unit when the consumer's cadence
//! says one is due, and built only then. Relax partials are lifted and verified the same way but
//! without the connectivity check (the worker refills and checks every proposal, so a cut in a
//! partial shows as its violation). A partial is a function of the problem and its step.
//!
//! Cost per offer and at each round's end: one full non-pour curve rebuild (an Engine, its static
//! and dynamic index builds over all obstacles including pour paths, clearance_pass over every
//! non-pour chord, and build_curve per non-pour trace), then the lift (O(port copper + ends log
//! ends + branch ends x host segments)), one pad-group count per relaxed net, per round of reverts one world overlay of the
//! solution (O(pieces log n)) and one verification of the relaxed nets' pieces
//! (O(pieces x (log n + k))). At the end only: the grid form settled once, and per round of the
//! connectivity check the solution settled (`check::settle` from the job's `Basis`: the board
//! apply, the index update and refill of what the delta meets, and `connect_indexed`; no full
//! fill or connectivity). Per hold round: one copy of the start board (O(port copper)) and a full
//! relax from it until it settles, with a pour one settle of the round's fixed-only form, the
//! paths' classification (O(path length / piece x test)) and their rows and masks laid again;
//! then the curve rebuild, lift and checks again. A cancelled or refused end that needs a second
//! lift pays for another rebuild. Per step: the relaxer's sweeps, bounded by `STEP_UNITS`, its
//! fixed topology work and curve work only for the step-time readers' traces (paired nets when
//! coupling and skew tuning are on, target-length nets when matching is on, including their pour
//! rows; no curve Engine when none qualify), and one
//! O(vias) tag walk, a status publication and a cadence check. Per judged unit of the pre stage:
//! a status publication and the judge's lift and check of the unit's nets (see `judge`), with a
//! cadence check after acceptance. The rebase runs once (the lift's tables again, no check). The
//! pour paths cost two settles (the start form and its fixed-only form), the forest (see `pour`)
//! and the static masks per zone once per job, and the paths' nodes per step; at the end one test
//! per path segment against the refilled fill of the final lift's last connectivity check (a settle
//! of its own only when the lift ended without one that found no cut), and the tight count
//! (O(path segments x moving segments of their layers)).
//! Nothing runs on the UI thread.

use super::adapt::{adapt, add_pour_rows, from_ref, mm, nm, pour_avoid_areas, to_ref, Adapted, PadSrc, MARGIN_NM};
use super::copper::net_pad_groups;
use super::geom::{Box2, Segment2, Vec2};
use super::judge::{Accepted, Judge};
use super::model::{OBoard, Trace, TracePoint, Via as PortVia, ViaType};
use super::opt::cleanup::{self, CleanupOptions, CleanupStats};
use super::opt::layer_reassign::{self, LayerReassignOptions, LayerReassignStats};
use super::opt::viahops::{self, ViaHopOptions, ViaHopStats};
use super::pour::{forest, pour_test, PourForest, PourParams};
use super::relaxer::OrganicRelaxer;
use super::settings::OrganicSettings;
use crate::auto::GridRouter;
use crate::check::{ratsnest_lines, settle, Basis, Settled};
use crate::problem::{Autorouter, RouteProblem};
use crate::progress::{JobPhase, Progress};
use crate::solution::{RouteMetrics, RouteSolution};
use crate::solvers;
use crate::verify::{new_pieces, verify};
use crate::world::World;
use fab_elec_board::{Anchor, Board, BoardOp, Flash, IndexChange, Item, ItemKind, Origin, RuleMatrix, Tent, Track, Via, ViaKind};
use fab_elec_circuit::ViaSize;
use fab_elec_core::{BBox, ItemId, LayerId, NetId, Nm, OnTrack, Path, Seg, P};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub use super::judge::JudgeStats;

/// Evaluation units per relax step (one trace node = 1, one via = 3; the reference's
/// interactive rate is 250 units per ms).
pub const STEP_UNITS: i64 = 4_000;
/// How far (nm) a fixed end may lie from the grid end it keeps: the f32 frame's rounding is a few
/// nm at board scale.
const END_SNAP_NM: Nm = 1_000;
/// How close (mm) a junction lies to a host node to be that node.
const NODE_EPS_MM: f32 = 1e-4;
/// The contact tolerance (mm) of the pad-group count, as the net simplifier counts.
const GROUP_TOL_MM: f64 = 1e-3;

/// The finish passes of the pre stage (on the relaxer's board before the relax, each judged at
/// the reference's `Drc::run` points by one judge from the grid form), each switched on alone.
/// The default is the reference's for the passes switched here (main.cpp PostOptions and
/// AutorouterCore.h:441): Cleanup and ViaHops on, LayerReassign at depth 2. The reference's
/// ViaMerge (on, AutorouterCore.h:444) is ported but held out: on every board measured the judge
/// kept none of its merges; it goes on when a board shows its effect. With every pass off the job
/// is the relax alone. `pairs` is the relax's own diff-pair term, not a pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finish {
    /// optimize/Cleanup with its default options.
    pub cleanup: bool,
    /// optimize/ViaHops with its default options (spikeMerge off), after Cleanup (main.cpp:1675).
    pub via_hops: bool,
    /// optimize/LayerReassign with its default options (depth 2), after ViaHops (main.cpp:1676).
    pub layer_reassign: bool,
    /// The relax couples diff pairs and tunes their skew (OrganicSettings enableDiffPairCoupling and
    /// enableDiffPairSkewTuning, on in the reference). Off by default until Fab's DRC has been checked
    /// on pair gaps below the clearance.
    pub pairs: bool,
}

impl Default for Finish {
    fn default() -> Finish {
        Finish { cleanup: true, via_hops: true, layer_reassign: true, pairs: false }
    }
}

impl Finish {
    /// Every pass off: the relax alone.
    pub fn none() -> Finish {
        Finish { cleanup: false, via_hops: false, layer_reassign: false, pairs: false }
    }

    fn pre(&self) -> bool {
        self.cleanup || self.via_hops || self.layer_reassign
    }
}

/// What a relax run did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OrganicReport {
    /// Relax steps run.
    pub steps: u32,
    /// The relaxer settled (its last step was idle).
    pub idle: bool,
    /// Nets in their relaxed form.
    pub nets: u32,
    /// Tracks of those nets (`Origin::Solved { by: solvers::ORGANIC }`).
    pub relaxed: u32,
    /// Grid tracks kept as routed: their net keeps its grid form.
    pub kept: u32,
    /// Nets that keep their grid form (see the module notes for why a net does). A net with a
    /// pour is not counted: it never lifts.
    pub reverted: u32,
    /// Nets held in their grid form while the relax went on around them (see the module notes).
    pub held: u32,
    /// Pour paths laid before the relax (see the module notes).
    pub pour_paths: u32,
    /// Their pinned length in the last round: stretches no path of full width fits against
    /// fixed copper.
    pub pour_pinned_mm: f64,
    /// Start-form pour joins no pour path protects.
    pub pour_unprotected: u32,
    /// Pour-path traces with moving copper of another net still under their clearance, beyond
    /// the adapter's margin, at the end: where to look when `pour_escaped` is not 0.
    pub pour_tight: u32,
    /// Pour-path segments that at the end leave their refilled fill's core (the search test,
    /// free and pinned alike); 0 when every path held its room.
    pub pour_escaped: u32,
    /// Nodes and vias still awake after the last step (0 when settled).
    pub awake: i64,
    /// The pre stage's judge (all zero when no pass ran).
    pub judge: JudgeStats,
    /// Cleanup's report when it ran.
    pub cleanup: Option<CleanupStats>,
    /// ViaHops' report when it ran.
    pub via_hops: Option<ViaHopStats>,
    /// LayerReassign's report when it ran.
    pub layer_reassign: Option<LayerReassignStats>,
}

/// The `@organic` method: route with the grid engine, then relax its copper. Returns the step
/// of the final solution and the solution.
pub fn route(problem: &RouteProblem, basis: &Basis, progress: &Progress) -> (u32, RouteSolution) {
    let grid = GridRouter::default().route(problem, progress);
    let first = grid.metrics.work.transactions;
    if progress.cancelled() {
        return (first, grid);
    }
    let (step, solution, _) = relax(problem, basis, &grid, progress, first);
    (step, solution)
}

/// Relax the grid router's solution `grid` of `problem`, checked from `basis` (the problem
/// board's piece index and findings), after the default finish passes (`Finish::default()`).
/// Partial solutions are offered to `progress` at steps after `first_step`; returns the last
/// step, the solution and the report.
pub fn relax(
    problem: &RouteProblem,
    basis: &Basis,
    grid: &RouteSolution,
    progress: &Progress,
    first_step: u32,
) -> (u32, RouteSolution, OrganicReport) {
    relax_with(problem, basis, grid, progress, first_step, &Finish::default())
}

/// `relax` with the finish passes `finish` (`relax` runs the default; see the module notes: the
/// pre stage runs on the relaxer's board before the relax, and the lift falls back to its accepted
/// form).
pub fn relax_with(
    problem: &RouteProblem,
    basis: &Basis,
    grid: &RouteSolution,
    progress: &Progress,
    first_step: u32,
    finish: &Finish,
) -> (u32, RouteSolution, OrganicReport) {
    let mut report = OrganicReport::default();
    let grid_tracks = grid.add.iter().filter(|it| matches!(it, Item::Track(_))).count() as u32;
    let mut board = (*problem.board).clone();
    if grid_tracks == 0 || board.apply_all(&grid.ops()).is_err() {
        report.kept = grid_tracks;
        return (first_step, grid.clone(), report);
    }
    let mut a = adapt(&board, &problem.rules, &problem.stack);
    let grid_ids: BTreeSet<ItemId> = grid.add.iter().map(Item::id).collect();
    // a net with a pour keeps its grid copper (see the module notes)
    let poured: BTreeSet<i32> = board.zones.iter().filter_map(|z| z.net).map(|net| net.0 as i32).collect();
    for (ti, id) in a.traces.iter().enumerate() {
        if !grid_ids.contains(id) || poured.contains(&a.board.traces[ti].net) {
            a.board.traces[ti].locked = true;
        }
    }
    for (vi, id) in a.vias.iter().enumerate() {
        if !grid_ids.contains(id) || poured.contains(&a.board.vias[vi].net) {
            a.board.vias[vi].is_fixed = true;
        }
    }
    link_junctions(&board, &a.traces, &mut a.board);
    let lift = Lift::new(problem, basis, grid, &board, &a);

    let mut step = first_step;
    let accepted = if finish.pre() && !progress.cancelled() {
        progress.set_phase(JobPhase::Cleanup);
        pre_stage(problem, basis, grid, &lift, &mut a.board, progress, step, finish, &mut report)
    } else {
        None
    };
    let rebased;
    let lift = match &accepted {
        Some(acc) => {
            step = acc.step;
            report.judge = acc.stats.clone();
            if progress.cancelled() {
                // the accepted state after the last judged unit stands
                return (step, acc.solution.clone(), report);
            }
            if acc.units == 0 {
                // nothing changed: the grid form stands, and the job is the relax alone
                &lift
            } else {
                rebased = lift.rebase(acc, &mut a.board);
                &rebased
            }
        }
        None => &lift,
    };
    let mut next_src = i32::try_from(lift.vias.len()).unwrap_or(i32::MAX);
    for via in &a.board.vias {
        next_src = next_src.max(via.src.saturating_add(1));
    }
    lift.tag_vias(&mut a.board, &mut next_src);
    let form = accepted.as_ref().filter(|acc| acc.units > 0).map_or(grid, |acc| &acc.solution);
    // the start form: every hold round starts the relax again from it (see the module notes)
    let start = a.board.clone();
    let mut pour = (!poured.is_empty()).then(|| {
        let (start_board, settled) = settled_form(problem, basis, form)?;
        let (fixed_board, fixed) = settled_form(problem, basis, &fixed_only(form, &poured))?;
        let forest = forest(&start_board, &problem.rules, &settled.fills, (&fixed_board, &fixed.fills), &settled.conn, &PourParams::default());
        lay_pour_paths(&mut a, &start_board, &problem.rules, &forest);
        report.pour_paths = forest.paths.len() as u32;
        report.pour_pinned_mm = forest.pinned_nm as f64 / 1e6;
        report.pour_unprotected = forest.unprotected;
        Some((start_board, forest))
    }).flatten();

    let settings = OrganicSettings {
        enable_dynamic_vias: true,
        enable_loop_simplification: true,
        enable_diff_pair_coupling: finish.pairs,
        enable_diff_pair_skew_tuning: finish.pairs,
        ..OrganicSettings::base()
    };
    let mut relaxer = OrganicRelaxer::new(settings.clone());
    let mut held: BTreeSet<i32> = BTreeSet::new();
    let (solution, counts, last_settle) = loop {
        let mut refused = false;
        while !progress.cancelled() {
            if relaxer.step(&mut a.board, STEP_UNITS).is_err() {
                // these settings turn on nothing the step refuses; a refusal ends the relaxing
                refused = true;
                break;
            }
            lift.tag_vias(&mut a.board, &mut next_src);
            report.steps += 1;
            step += 1;
            progress.set_phase(JobPhase::Relax { step });
            let info = relaxer.last_step_info();
            if info.idle {
                report.idle = true;
                break;
            }
            report.awake = info.awake;
            // no partial while a hold round settles: it starts from the grid form, so the job would
            // show going back and then relaxed again
            if held.is_empty() && progress.due(step) {
                relaxer.update_splines(&mut a.board);
                let (partial, _, _, _) = lift.solution(&a.board, false, &held, true);
                progress.offer(step, &partial);
            }
        }
        relaxer.update_splines(&mut a.board);
        let (solution, counts, first, last_settle) = lift.solution(&a.board, true, &held, false);
        if first.is_empty() {
            break (solution, counts, last_settle);
        }
        if refused || progress.cancelled() {
            relaxer.update_splines(&mut a.board);
            let (solution, counts, _, last_settle) = lift.solution(&a.board, true, &held, true);
            break (solution, counts, last_settle);
        }
        report.held += first.len() as u32;
        report.idle = false;
        held.extend(first);
        a.board = start.clone();
        for t in a.board.traces.iter_mut().filter(|t| held.contains(&t.net)) {
            t.locked = true;
        }
        for v in a.board.vias.iter_mut().filter(|v| held.contains(&v.net)) {
            v.is_fixed = true;
        }
        if let Some((start_board, forest)) = &mut pour {
            let fixed_nets: BTreeSet<i32> = poured.union(&held).copied().collect();
            let fixed = settled_form(problem, basis, &fixed_only(form, &fixed_nets));
            forest.classify(&problem.rules, fixed.as_ref().map(|(b, s)| (b, &s.fills[..])), &PourParams::default());
            lay_pour_paths(&mut a, start_board, &problem.rules, forest);
            report.pour_pinned_mm = forest.pinned_nm as f64 / 1e6;
        }
        relaxer = OrganicRelaxer::new(settings.clone());
    };
    if pour.as_ref().is_some_and(|(_, forest)| !forest.paths.is_empty()) {
        report.pour_tight = tight(&a.board);
        // the final lift's last connectivity check settled this very solution when it found no cut;
        // a lift that ended otherwise (no relaxed net left, or its settle failed) settles it here
        report.pour_escaped = last_settle.or_else(|| settled_form(problem, basis, &solution))
            .map_or(0, |(after, settled)| escaped(&a.board, &a, &after, &problem.rules, &settled));
    }
    report.nets = counts.nets;
    report.relaxed = counts.relaxed;
    report.kept = counts.kept;
    report.reverted = counts.reverted;
    (step, solution, report)
}

/// A board with a solution applied, and its settled fills and connectivity.
type Settle = (Board, Settled);

/// `sol` applied to `problem`'s board and settled from `basis`: the board, its fills and connectivity.
fn settled_form(problem: &RouteProblem, basis: &Basis, sol: &RouteSolution) -> Option<Settle> {
    let all = IndexChange::all();
    let state = basis.state(problem, &all);
    let mut board = (*problem.board).clone();
    let settled = settle(&mut board, &sol.ops(), &state).ok()?;
    Some((board, settled))
}

/// `form` without the copper that moves in the relax: every track and via of a net not in `fixed`
/// (the poured nets, and in a hold round the held ones) goes, what it removes stays removed.
fn fixed_only(form: &RouteSolution, fixed: &BTreeSet<i32>) -> RouteSolution {
    let mut out = form.clone();
    out.add.retain(|it| match it {
        Item::Track(t) => fixed.contains(&(t.net.0 as i32)),
        Item::Via(v) => fixed.contains(&(v.net.0 as i32)),
        _ => true,
    });
    out
}

/// Lay `forest`'s pour paths on the relaxer's board: one clearance row per zone with paths, the
/// zones' static masks as keepouts only their net feels, and each path as traces of its net on
/// its zone's row, one per stretch: a free stretch moves, a pinned one is locked. Every trace's
/// ends are fixed and linked to nothing.
fn lay_pour_paths(a: &mut Adapted, board: &Board, rules: &RuleMatrix, forest: &PourForest) {
    if forest.paths.is_empty() {
        return;
    }
    let zones: Vec<ItemId> = forest.paths.iter().map(|p| p.zone).collect::<BTreeSet<_>>().into_iter().collect();
    let mut ob = std::mem::take(&mut a.board);
    let rows = add_pour_rows(&mut ob, a, board, rules, &zones, MARGIN_NM);
    let mut next_ko = ob.keepouts.iter().map(|k| k.id).max().unwrap_or(-1).saturating_add(1);
    for &zone in &zones {
        let areas = pour_avoid_areas(board, rules, zone, a, next_ko);
        next_ko = areas.iter().map(|k| k.id).max().map_or(next_ko, |id| id.saturating_add(1));
        ob.keepouts.extend(areas);
    }
    let mut next_trace = ob.traces.iter().map(|t| t.id).max().unwrap_or(-1).saturating_add(1);
    for path in &forest.paths {
        let row = rows[zones.binary_search(&path.zone).expect("every path's zone has a row")];
        let width = mm(path.width);
        let mut from = 0;
        while from < path.pinned.len() {
            let pinned = path.pinned[from];
            let to = (from..path.pinned.len()).find(|&i| path.pinned[i] != pinned).unwrap_or(path.pinned.len());
            let run = &path.points[from..=to];
            let points = run.iter().enumerate()
                .map(|(i, &q)| TracePoint { pos: to_ref(q), width, is_fixed: i == 0 || i + 1 == run.len(), ..TracePoint::default() })
                .collect();
            ob.traces.push(Trace {
                id: next_trace, net: path.net.0 as i32, layer: a.layer_of(path.layer), width, points, rule: row,
                locked: pinned, ..Trace::default()
            });
            next_trace = next_trace.saturating_add(1);
            from = to;
        }
    }
    a.board = ob;
}

/// Pour-path traces on the relaxer's board `ob` with moving copper of another net under their
/// clearance: a free stretch's neighbour the relax did not push out, or a pinned stretch's.
fn tight(ob: &OBoard) -> u32 {
    let pour = |rule: u16| ob.pour_rules.binary_search(&rule).is_ok();
    let half = |t: &Trace| t.points.iter().map(|p| p.width).fold(t.width, f32::max) / 2.0;
    let bounds = |pts: &mut dyn Iterator<Item = Vec2>, pad: f32| {
        let mut b = Box2::default();
        pts.for_each(|p| b.expand(p));
        b.pad(pad);
        b
    };
    // a pair is under clearance only within the table's largest value and both half widths
    let reach_max = |t: &Trace| ob.clr.max + t.width / 2.0;
    let mut out = 0;
    for t in ob.traces.iter().filter(|t| pour(t.rule)) {
        let segs: Vec<Segment2> = t.points.windows(2).map(|w| Segment2::new(w[0].pos, w[1].pos)).collect();
        let near = |q: &Segment2, reach: f32| segs.iter().any(|s| s.distance_to_seg(q) < reach);
        let around = bounds(&mut t.points.iter().map(|p| p.pos), reach_max(t));
        let traces = ob.traces.iter()
            .filter(|u| u.net != t.net && u.layer == t.layer && !u.locked && !pour(u.rule))
            .filter(|u| around.overlaps(&bounds(&mut u.points.iter().map(|p| p.pos), half(u))))
            .any(|u| {
                let reach = ob.clr.get(t.rule, u.rule, t.layer) + t.width / 2.0 + half(u) - TIGHT_TOL_MM;
                u.points.windows(2).any(|w| near(&Segment2::new(w[0].pos, w[1].pos), reach))
            });
        let vias = ob.vias.iter()
            .filter(|v| v.net != t.net && !v.is_fixed && v.layer_start <= t.layer && t.layer <= v.layer_end)
            .any(|v| {
                let reach = ob.clr.get(t.rule, v.rule, t.layer) + t.width / 2.0 + v.diameter / 2.0 - TIGHT_TOL_MM;
                segs.iter().any(|s| s.distance_to_point(v.pos) < reach)
            });
        out += u32::from(traces || vias);
    }
    out
}

/// What `tight` forgives a pair (mm): the margin the pour row adds to the zone gap, the float and
/// chord allowance (`MARGIN_NM`). A pair within it still stands at the zone gap or more and spends
/// none of the path's own width.
const TIGHT_TOL_MM: f32 = MARGIN_NM as f32 / 1e6;

/// Pour-path segments on the relaxer's board `ob` that leave their refilled fill on the settled
/// final form (`after`, `settled`): every segment, free or pinned, must pass the search test (the
/// fill's core). The full-width test belongs to the start classification only: the guard holds a
/// free stretch at its row's clearance, and the refill's edge may round that by up to three arc
/// errors, so a free stretch that moved cannot pass it by construction.
fn escaped(ob: &OBoard, a: &Adapted, after: &Board, rules: &RuleMatrix, settled: &Settled) -> u32 {
    let mut tests = BTreeMap::new();
    let mut out = 0;
    for t in ob.traces.iter().filter(|t| ob.pour_rules.binary_search(&t.rule).is_ok()) {
        let Some(&layer) = usize::try_from(t.layer).ok().and_then(|l| a.layers.get(l)) else { continue };
        let net = NetId(t.net as u32);
        let Some(zone) = after.zones.iter().find(|z| z.net == Some(net) && z.layers.has(layer)) else { continue };
        let test = tests.entry((zone.id, layer))
            .or_insert_with(|| pour_test(after, rules, &settled.fills, zone.id, layer, &PourParams::default()));
        let Some(test) = test.as_ref() else { continue };
        out += t.points.windows(2).filter(|w| !test.reach(layer, from_ref(w[0].pos), from_ref(w[1].pos))).count() as u32;
    }
    out
}

/// A trace the lift takes: unlocked and not a pour path (the fill takes a pour path's room).
fn moving(ob: &OBoard, t: &Trace) -> bool {
    !t.locked && ob.pour_rules.binary_search(&t.rule).is_err()
}

/// How near `it` comes to the segment `a`-`b` in nm (0 when it touches): a track by its
/// centreline (an arc by its two chords through its mid point) less half its width, a via by its
/// centre less half its pad, anything else by its box.
fn reach(it: &Item, board: &Board, a: P, b: P) -> i64 {
    let d = match it {
        Item::Track(t) => {
            let mut d = f64::INFINITY;
            let mut from = t.path.start;
            for seg in &t.path.segs {
                match seg {
                    Seg::Line(to) => {
                        d = d.min(seg_seg(from, *to, a, b));
                        from = *to;
                    }
                    Seg::Arc { mid, end } => {
                        d = d.min(seg_seg(from, *mid, a, b)).min(seg_seg(*mid, *end, a, b));
                        from = *end;
                    }
                }
            }
            d - f64::from(t.w) / 2.0
        }
        Item::Via(v) => point_seg(v.at, a, b) - f64::from(v.pad) / 2.0,
        _ => box_gap(&it.bbox(board), &BBox::new(a, b)) as f64,
    };
    d.max(0.0).round() as i64
}

/// The distance from `p` to the segment `a`-`b`.
fn point_seg(p: P, a: P, b: P) -> f64 {
    let (ax, ay, bx, by, px, py) = (f64::from(a.x), f64::from(a.y), f64::from(b.x), f64::from(b.y), f64::from(p.x), f64::from(p.y));
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (px - (ax + t * dx)).hypot(py - (ay + t * dy))
}

/// The distance between the segments `p`-`q` and `a`-`b`: 0 when they cross.
fn seg_seg(p: P, q: P, a: P, b: P) -> f64 {
    let cross = |o: P, u: P, v: P| {
        (i64::from(u.x) - i64::from(o.x)) as i128 * (i64::from(v.y) - i64::from(o.y)) as i128
            - (i64::from(u.y) - i64::from(o.y)) as i128 * (i64::from(v.x) - i64::from(o.x)) as i128
    };
    let (d1, d2, d3, d4) = (cross(a, b, p), cross(a, b, q), cross(p, q, a), cross(p, q, b));
    if ((d1 > 0 && d2 < 0) || (d1 < 0 && d2 > 0)) && ((d3 > 0 && d4 < 0) || (d3 < 0 && d4 > 0)) {
        return 0.0;
    }
    point_seg(p, a, b).min(point_seg(q, a, b)).min(point_seg(a, p, q)).min(point_seg(b, p, q))
}

/// The gap between two boxes, 0 when they meet: the larger of the x and y gaps.
fn box_gap(a: &BBox, b: &BBox) -> i64 {
    let dx = (i64::from(b.min.x) - i64::from(a.max.x)).max(i64::from(a.min.x) - i64::from(b.max.x));
    let dy = (i64::from(b.min.y) - i64::from(a.max.y)).max(i64::from(a.min.y) - i64::from(b.max.y));
    dx.max(dy).max(0)
}

/// The pre stage on the relaxer's board `ob` (one OBoard from the adapter to the lift, so what
/// a pass leaves that Fab's items cannot carry reaches the relax): each switched-on pass in the
/// reference's order (tests/route_board_main.cpp 1469-1552), all judged by one judge whose
/// accepted state starts as the grid form. Each pass ends by its own logic; the accepted solution
/// is offered at every accepted unit and each pass end when the consumer's cadence says one is
/// due, and built only then, one step per accepted unit. `None` when the grid form does not check
/// (nothing ran).
fn pre_stage(
    problem: &RouteProblem,
    basis: &Basis,
    grid: &RouteSolution,
    lift: &Lift,
    ob: &mut OBoard,
    progress: &Progress,
    step: u32,
    finish: &Finish,
    report: &mut OrganicReport,
) -> Option<Accepted> {
    let mut judge = Judge::new(problem, basis, grid, lift, progress, step)?;
    if finish.cleanup {
        report.cleanup = Some(cleanup::run(ob, &CleanupOptions::default(), &mut judge));
        judge.offer();
    }
    if finish.via_hops {
        let mut stats = ViaHopStats::default();
        viahops::run(ob, &ViaHopOptions::default(), &mut stats, &mut judge);
        report.via_hops = Some(stats);
        judge.offer();
    }
    if finish.layer_reassign {
        let mut stats = LayerReassignStats::default();
        layer_reassign::run(ob, &LayerReassignOptions::default(), &mut stats, &mut judge);
        report.layer_reassign = Some(stats);
        judge.offer();
    }
    Some(judge.finish())
}

/// Turn every junction a moving track ends in into the relaxer's branch link: the host gets a
/// node at the junction (unless one is there) and the branch end names that node. A junction at
/// a host's end on a via attaches the branch end to that via.
fn link_junctions(board: &Board, traces: &[ItemId], ob: &mut OBoard) {
    let index: BTreeMap<ItemId, usize> = traces.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    // (branch trace, end, host trace, the junction in the port's frame)
    let mut links: Vec<(usize, usize, usize, Vec2)> = Vec::new();
    for (ti, id) in traces.iter().enumerate() {
        if ob.traces[ti].locked {
            continue;
        }
        let Some((ItemKind::Track, fi)) = board.locate(*id) else { continue };
        for (e, a) in board.tracks[fi].ends.iter().enumerate() {
            let Anchor::Track { id: host, .. } = *a else { continue };
            let (Some(&hi), Some(j)) = (index.get(&host), board.anchor_pos(*a)) else { continue };
            if hi != ti {
                links.push((ti, e, hi, to_ref(j)));
            }
        }
    }
    for &(_, _, hi, j) in &links {
        let pts = &mut ob.traces[hi].points;
        if pts.len() < 2 || pts.iter().any(|p| p.pos.distance_to(j) <= NODE_EPS_MM) {
            continue;
        }
        let mut best = (f32::INFINITY, 0);
        for k in 0..pts.len() - 1 {
            let d = Segment2::new(pts[k].pos, pts[k + 1].pos).distance_to_point(j);
            if d < best.0 {
                best = (d, k);
            }
        }
        let k = best.1;
        let width = pts[k].width;
        pts.insert(k + 1, TracePoint { pos: j, width, ..TracePoint::default() });
    }
    for &(ti, e, hi, j) in &links {
        let host = &ob.traces[hi];
        let Some(k) = (0..host.points.len()).min_by(|&x, &y| host.points[x].pos.distance_to(j).total_cmp(&host.points[y].pos.distance_to(j))) else {
            continue;
        };
        let (pos, via, id, at_end) = (host.points[k].pos, host.points[k].attached_via, host.id, k == 0 || k + 1 == host.points.len());
        let pts = &mut ob.traces[ti].points;
        let end = if e == 0 { 0 } else { pts.len() - 1 };
        let p = &mut pts[end];
        if at_end {
            if via >= 0 {
                p.pos = pos;
                p.is_fixed = false;
                p.attached_via = via;
            }
            continue;
        }
        p.pos = pos;
        p.is_fixed = false;
        p.branch_trace = id;
        p.branch_point = k as i32;
    }
}

#[derive(Copy, Clone, Default)]
struct LiftCounts {
    nets: u32,
    relaxed: u32,
    kept: u32,
    reverted: u32,
}

/// A grid end: its anchor and exact end point.
type End = (Anchor, P);
/// One net's copper in the relaxer's board: port via indices and moving trace indices.
pub(super) type NetCopper = (Vec<usize>, Vec<usize>);

/// Fab ids for copper without a table id: per port trace id and per port via index. The relax
/// derives them from trace ids and via tags; the judge allocates or keeps its accepted ids.
#[derive(Default)]
pub(super) struct Fresh {
    pub traces: BTreeMap<i32, ItemId>,
    pub vias: BTreeMap<usize, ItemId>,
}

/// What a lift reads besides the relaxer's board, built once per job (and once more by the
/// pre stage's rebase). Its "grid form" is the solution it falls back to: the grid router's, or
/// after a pre stage the judge's accepted one.
pub(super) struct Lift<'a> {
    problem: &'a RouteProblem,
    basis: &'a Basis,
    grid: &'a RouteSolution,
    /// The problem's board with the grid solution applied.
    board: &'a Board,
    /// The Fab item of each port trace id (the adapter's traces, then the traces a rebase
    /// takes from the accepted state), and the net (port index) that trace was on.
    traces: BTreeMap<i32, ItemId>,
    trace_net: BTreeMap<i32, i32>,
    /// The port trace id of each Fab track in `traces`.
    trace_ix: BTreeMap<ItemId, i32>,
    /// Fab via ids by tag: adapter vias, then pass-added vias carried by the rebase.
    vias: Vec<ItemId>,
    /// The net of each via table entry still present on the applied board, for the tag walk.
    via_net: Vec<Option<i32>>,
    /// The adapter's pads by index.
    pads: Vec<PadSrc>,
    pub(super) layers: Vec<LayerId>,
    /// `grid.add` index of each grid item.
    grid_ix: BTreeMap<ItemId, usize>,
    /// The nets with grid copper (the port's net index), each with its pad-group count before
    /// the relaxing.
    nets: BTreeMap<i32, i32>,
    /// The nets with a pour (port net index): their copper is locked in the relax and they never
    /// lift, so every partial and the result keep their grid copper.
    poured: BTreeSet<i32>,
    /// Each grid track's own two ends.
    own: BTreeMap<ItemId, [End; 2]>,
    /// Each net's grid track ends, by x then y (ties in grid order).
    ends: BTreeMap<i32, Vec<End>>,
    /// Each net's grid track widths: the port's width bits and the width.
    widths: BTreeMap<i32, Vec<(u32, Nm)>>,
    /// Each net's missing connections (ratsnest lines) with the grid solution applied, made by the
    /// first connectivity check.
    lines: OnceCell<BTreeMap<NetId, usize>>,
    /// The base of derived ids: even offsets for traces, odd offsets for fresh-tagged vias.
    fresh: u32,
    /// Each problem net's via size (port net index), for the vias a pass adds.
    net_via: BTreeMap<i32, ViaSize>,
}

impl<'a> Lift<'a> {
    fn new(problem: &'a RouteProblem, basis: &'a Basis, grid: &'a RouteSolution, board: &'a Board, a: &Adapted) -> Lift<'a> {
        let traces = a.traces.iter().enumerate().map(|(i, id)| (i as i32, *id)).collect();
        let trace_net = a.board.traces.iter().enumerate().map(|(i, t)| (i as i32, t.net)).collect();
        let ids = Ids { traces, trace_net, vias: a.vias.clone(), pads: a.pads.clone(), layers: a.layers.clone() };
        Lift::with(problem, basis, grid, board, &a.board, ids, OnceCell::new(), board.next_item)
    }

    /// The lift whose grid form is the pre stage's accepted state `acc` (the rebase): a net that
    /// does not lift after the relax keeps its accepted form, its ends keep that form's exact end
    /// points, its copper that form's ids, and it may connect no fewer pads than it does in `ob`
    /// (the relaxer's board as the pre stage left it). Accepted traces join the trace table;
    /// accepted fresh vias join the via table in each net's port order and receive its tags.
    /// The base lines are the judge's (the same check). Result claims stay the grid's.
    fn rebase<'b>(&'b self, acc: &'b Accepted, ob: &mut OBoard) -> Lift<'b> {
        let mut traces = self.traces.clone();
        let mut trace_net = self.trace_net.clone();
        for &(port, id, net) in &acc.ports {
            traces.insert(port, id);
            trace_net.insert(port, net);
        }
        let mut vias = self.vias.clone();
        for (n, c) in self.copper(ob) {
            let accepted = acc.vias.get(&n).map_or(&[][..], Vec::as_slice);
            for (k, vi) in self.fresh_vias(ob, n, &c.0).into_iter().enumerate() {
                ob.vias[vi].src = -1;
                if let (Some(&id), Ok(src)) = (accepted.get(k), i32::try_from(vias.len())) {
                    vias.push(id);
                    ob.vias[vi].src = src;
                }
            }
        }
        let ids = Ids { traces, trace_net, vias, pads: self.pads.clone(), layers: self.layers.clone() };
        Lift::with(self.problem, self.basis, &acc.solution, &acc.board, ob, ids, OnceCell::from(acc.lines.clone()), acc.next)
    }

    fn with(
        problem: &'a RouteProblem,
        basis: &'a Basis,
        grid: &'a RouteSolution,
        board: &'a Board,
        ob: &OBoard,
        ids: Ids,
        lines: OnceCell<BTreeMap<NetId, usize>>,
        fresh: u32,
    ) -> Lift<'a> {
        let mut nets = BTreeMap::new();
        let mut own = BTreeMap::new();
        let mut ends: BTreeMap<i32, Vec<End>> = BTreeMap::new();
        let mut widths: BTreeMap<i32, Vec<(u32, Nm)>> = BTreeMap::new();
        for it in &grid.add {
            match it {
                Item::Track(t) => {
                    let n = t.net.0 as i32;
                    nets.insert(n, 0);
                    let two = [(t.ends[0], t.path.start), (t.ends[1], t.path.end())];
                    own.insert(t.id, two);
                    ends.entry(n).or_default().extend(two);
                    widths.entry(n).or_default().push((mm(t.w).to_bits(), t.w));
                }
                Item::Via(v) => {
                    nets.insert(v.net.0 as i32, 0);
                }
                _ => {}
            }
        }
        for list in ends.values_mut() {
            list.sort_by_key(|(_, p)| (p.x, p.y));
        }
        for (n, groups) in nets.iter_mut() {
            *groups = net_pad_groups(ob, *n, None, GROUP_TOL_MM);
        }
        let Ids { traces, trace_net, vias, pads, layers } = ids;
        let via_nets: BTreeMap<ItemId, i32> = board.vias.iter().map(|v| (v.id, v.net.0 as i32)).collect();
        let via_net = vias.iter().map(|id| via_nets.get(id).copied()).collect();
        Lift {
            problem,
            basis,
            grid,
            board,
            trace_ix: traces.iter().map(|(port, id)| (*id, *port)).collect(),
            traces,
            trace_net,
            vias,
            via_net,
            pads,
            layers,
            grid_ix: grid.add.iter().enumerate().map(|(i, it)| (it.id(), i)).collect(),
            nets,
            poured: board.zones.iter().filter_map(|z| z.net).map(|net| net.0 as i32).collect(),
            own,
            ends,
            widths,
            lines,
            fresh,
            net_via: problem.nets.iter().map(|r| (r.net.0 as i32, r.via)).collect(),
        }
    }

    /// The solution with every net that lifts and passes the checks in its relaxed form (the
    /// connectivity check only when `connected`), and the nets that went back to grid form first
    /// (see the module notes; empty when none went back). The nets in `held` (the nets the relax
    /// holds) stay in grid form and are not counted among them. Unless `whole`, the checks stop
    /// once that first wave is known and not empty (after the first connectivity check), and the
    /// solution is then not checked to the end: for a caller that holds the wave and relaxes on.
    /// Last, when the final connectivity check found no cut, its settle of exactly this solution
    /// (the problem's board with the solution applied, and its fills and connectivity), as
    /// `settled_form` would make it; `None` when the lift ended without such a check.
    fn solution(&self, ob: &OBoard, connected: bool, held: &BTreeSet<i32>, whole: bool) -> (RouteSolution, LiftCounts, BTreeSet<i32>, Option<Settle>) {
        let copper = self.copper(ob);
        let fresh = self.fresh_ids(ob, &copper);
        let mut forms: BTreeMap<i32, Vec<Item>> = BTreeMap::new();
        for (&n, &groups) in &self.nets {
            if self.poured.contains(&n) || held.contains(&n) {
                continue;
            }
            let Some(c) = copper.get(&n) else { continue };
            if let Some(items) = self.form(ob, n, c, &fresh, true) {
                if net_pad_groups(ob, n, None, GROUP_TOL_MM) <= groups {
                    forms.insert(n, items);
                }
            }
        }
        let mut first: BTreeSet<i32> =
            self.nets.keys().filter(|n| !forms.contains_key(n) && !self.poured.contains(n) && !held.contains(n)).copied().collect();
        let mut round = 0;
        let mut settled = None;
        let add = loop {
            let add = self.verified(&mut forms, (round == 0).then_some(&mut first));
            if forms.is_empty() || !connected {
                break add;
            }
            match self.cuts(&add, &forms) {
                Some((cut, after)) if cut.is_empty() => {
                    settled = Some(after);
                    break add;
                }
                Some((cut, _)) => {
                    if round == 0 {
                        first.extend(cut.iter().copied());
                    }
                    for n in cut {
                        forms.remove(&n);
                    }
                }
                None => {
                    if round == 0 {
                        first.extend(forms.keys().copied());
                    }
                    forms.clear();
                }
            }
            round += 1;
            if !whole && !first.is_empty() {
                break add;
            }
        };
        let reverted = self.nets.keys().filter(|n| !forms.contains_key(n) && !self.poured.contains(n)).count() as u32;
        let mut counts = LiftCounts { nets: forms.len() as u32, reverted, ..LiftCounts::default() };
        for it in &add {
            if let Item::Track(t) = it {
                if forms.contains_key(&(t.net.0 as i32)) {
                    counts.relaxed += 1;
                } else {
                    counts.kept += 1;
                }
            }
        }
        (self.with_add(add), counts, first, settled)
    }

    /// The solution of the grid form with every net in `forms` in that form (unverified: what
    /// the judge accepted).
    pub(super) fn accepted(&self, forms: &BTreeMap<i32, Vec<Item>>) -> RouteSolution {
        self.with_add(self.assemble(forms))
    }

    /// The grid form's solution with the items `add`: its removals and claims, its metrics with
    /// the copper measures (length, corners, vias) of `add`.
    fn with_add(&self, add: Vec<Item>) -> RouteSolution {
        let mut metrics: RouteMetrics = self.grid.metrics.clone();
        metrics.length = 0;
        metrics.bends = 0;
        metrics.turning = 0;
        metrics.acute = 0;
        metrics.vias = 0;
        for it in &add {
            match it {
                Item::Track(t) => {
                    metrics.length += t.path.length().round() as i64;
                    let (bends, turning, acute) = corners(&t.path);
                    metrics.bends += bends;
                    metrics.turning += turning;
                    metrics.acute += acute;
                }
                Item::Via(_) => metrics.vias += 1,
                _ => {}
            }
        }
        RouteSolution { add, remove: self.grid.remove.clone(), open: self.grid.open.clone(), repaired: self.grid.repaired.clone(), metrics }
    }

    /// The solution's items with every net in `forms` that passes verification relaxed; drops
    /// the others from `forms`. `first` gains the nets the first pass rejects (a later pass
    /// rejects nets that collide with the grid forms of nets rejected before).
    fn verified(&self, forms: &mut BTreeMap<i32, Vec<Item>>, mut first: Option<&mut BTreeSet<i32>>) -> Vec<Item> {
        loop {
            let add = self.assemble(forms);
            let mut world = World::new(&self.problem.base);
            for id in &self.grid.remove {
                world.remove_item(*id);
            }
            for it in &add {
                for piece in new_pieces(self.board, it) {
                    world.add(piece);
                }
            }
            let rejected: Vec<i32> = forms
                .iter()
                .filter(|(_, items)| {
                    let pieces: Vec<_> = items.iter().flat_map(|it| new_pieces(self.board, it)).collect();
                    !verify(&world, &self.problem.rules, self.board, &pieces).is_empty()
                })
                .map(|(n, _)| *n)
                .collect();
            if rejected.is_empty() {
                return add;
            }
            if let Some(first) = first.take() {
                first.extend(rejected.iter().copied());
            }
            for n in rejected {
                forms.remove(&n);
            }
        }
    }

    /// The relaxed nets to put back in grid form because the solution `add` cuts a connection
    /// the grid form has (see the module notes), with the settle it read (the problem's board with
    /// `add` applied, and its fills and connectivity); `None` when `add` does not apply.
    fn cuts(&self, add: &[Item], forms: &BTreeMap<i32, Vec<Item>>) -> Option<(Vec<i32>, Settle)> {
        let ops: Vec<BoardOp> = self.grid.remove.iter().map(|id| BoardOp::Remove(*id)).chain(add.iter().cloned().map(BoardOp::Add)).collect();
        // each settled from the basis: the problem's connectivity may hold zones over updates this
        // job never saw
        let since = IndexChange::all();
        let state = self.basis.state(self.problem, &since);
        let mut board = (*self.problem.board).clone();
        let settled = settle(&mut board, &ops, &state).ok()?;
        let base = self.lines.get_or_init(|| {
            let mut grid_board = (*self.problem.board).clone();
            settle(&mut grid_board, &self.grid.ops(), &state).map_or_else(|_| BTreeMap::new(), |s| ratsnest_lines(&s.conn))
        });
        let mut out = BTreeSet::new();
        for (net, k) in ratsnest_lines(&settled.conn) {
            if k <= base.get(&net).copied().unwrap_or(0) {
                continue;
            }
            let n = net.0 as i32;
            if forms.contains_key(&n) {
                out.insert(n);
                continue;
            }
            // a relaxed track cut its pour: per missing connection, the relaxed nets nearest to it
            for line in settled.conn.ratsnest.iter().filter(|r| r.net == net) {
                let near: Vec<(i32, i64)> = forms
                    .iter()
                    .map(|(m, items)| (*m, items.iter().map(|it| reach(it, &board, line.pa, line.pb)).min().unwrap_or(i64::MAX)))
                    .collect();
                if let Some(nearest) = near.iter().map(|(_, d)| *d).min() {
                    out.extend(near.iter().filter(|(_, d)| *d == nearest).map(|(m, _)| *m));
                }
            }
        }
        Some((out.into_iter().collect(), (board, settled)))
    }

    /// Each lifted net's port vias and moving traces, in port order.
    pub(super) fn copper(&self, ob: &OBoard) -> BTreeMap<i32, NetCopper> {
        let mut out: BTreeMap<i32, NetCopper> = BTreeMap::new();
        for (vi, v) in ob.vias.iter().enumerate() {
            if self.nets.contains_key(&v.net) {
                out.entry(v.net).or_default().0.push(vi);
            }
        }
        for (ti, t) in ob.traces.iter().enumerate() {
            if moving(ob, t) && self.nets.contains_key(&t.net) {
                out.entry(t.net).or_default().1.push(ti);
            }
        }
        out
    }

    /// The Fab id a moving trace keeps: that of its port id when that was a grid track of its
    /// own net.
    fn reuse(&self, tr: &Trace) -> Option<ItemId> {
        let id = *self.traces.get(&tr.id)?;
        (self.trace_net.get(&tr.id) == Some(&tr.net) && self.grid_ix.contains_key(&id)).then_some(id)
    }

    /// Derived ids for moving traces without table ids and fresh-tagged vias. Missing or
    /// overflowing ids stay absent, so their nets do not lift. No offer changes these ids.
    fn fresh_ids(&self, ob: &OBoard, copper: &BTreeMap<i32, NetCopper>) -> Fresh {
        let ids: Vec<usize> = (0..ob.traces.len()).filter(|&ti| moving(ob, &ob.traces[ti]) && self.nets.contains_key(&ob.traces[ti].net)).collect();
        let traces = self.fresh_of(ob, &ids).into_iter().filter_map(|id| {
            Some((id, self.derived_id(u32::try_from(id).ok()?, 0)?))
        }).collect();
        let vias = copper.iter().flat_map(|(&n, c)| self.fresh_vias(ob, n, &c.0)).filter_map(|vi| {
            let tag = usize::try_from(ob.vias[vi].src).ok()?.checked_sub(self.vias.len())?;
            Some((vi, self.derived_id(u32::try_from(tag).ok()?, 1)?))
        }).collect();
        Fresh { traces, vias }
    }

    /// A derived id below `u32::MAX`, leaving room for the applied board's `next_item`.
    fn derived_id(&self, key: u32, parity: u32) -> Option<ItemId> {
        let id = self.fresh.checked_add(key.checked_mul(2)?.checked_add(parity)?)?;
        (id < u32::MAX).then_some(ItemId(id))
    }

    /// Keep each valid table tag on its first same-net via and each fresh tag on its first
    /// via. Assign other vias tags in port order, independently of offers. The counter starts
    /// beyond the table and every existing tag; exhausted tags stay absent and cannot lift.
    fn tag_vias(&self, ob: &mut OBoard, next_src: &mut i32) {
        let mut seen = HashSet::new();
        for via in &mut ob.vias {
            let keep = usize::try_from(via.src).ok().is_some_and(|src| {
                (src >= self.vias.len() || self.via_net[src] == Some(via.net)) && seen.insert(via.src)
            });
            if !keep {
                via.src = -1;
                if let Some(next) = next_src.checked_add(1) {
                    via.src = *next_src;
                    *next_src = next;
                    seen.insert(via.src);
                }
            }
        }
    }

    /// The Fab via each of net `n`'s port vias `vias` (indices in `ob`, ascending) stands for:
    /// the one its tag names, when that via is of net `n` and no earlier port via of the net
    /// carries the same tag; `None` for a fresh tag, a missing tag or a duplicate/foreign tag.
    fn via_ids(&self, ob: &OBoard, n: i32, vias: &[usize]) -> Vec<Option<ItemId>> {
        let mut seen = BTreeSet::new();
        vias.iter()
            .map(|&vi| {
                let src = usize::try_from(ob.vias[vi].src).ok()?;
                let id = *self.vias.get(src)?;
                (self.via_net[src] == Some(n) && seen.insert(src)).then_some(id)
            })
            .collect()
    }

    /// The port vias of `vias` (net `n`'s, indices in `ob`, ascending) that a pass added: they
    /// have no table id. In the relax their fresh tags derive their Fab ids.
    pub(super) fn fresh_vias(&self, ob: &OBoard, n: i32, vias: &[usize]) -> Vec<usize> {
        vias.iter().zip(self.via_ids(ob, n, vias)).filter(|(_, id)| id.is_none()).map(|(&vi, _)| vi).collect()
    }

    /// The layers of port via `v` in Fab's terms (first and last copper layer in stack order)
    /// and the kind they make it; `None` when a layer is outside the stack.
    fn span_of(&self, v: &PortVia) -> Option<((LayerId, LayerId), ViaKind)> {
        let (a, b) = (v.layer_start.min(v.layer_end), v.layer_start.max(v.layer_end));
        let first = *self.layers.get(usize::try_from(a).ok()?)?;
        let last = *self.layers.get(usize::try_from(b).ok()?)?;
        let outer = (a == 0, b as usize + 1 == self.layers.len());
        let kind = match outer {
            _ if v.kind == ViaType::Micro => ViaKind::Micro,
            (true, true) => ViaKind::Through,
            (false, false) => ViaKind::Buried,
            _ => ViaKind::Blind,
        };
        Some(((first, last), kind))
    }

    /// The via a pass added to net `n` (port via `v`) with Fab id `id`: the net's via size when
    /// the port's is that size to its float rounding, else the port's; tented on both sides and
    /// unlocked, as the grid router makes its vias.
    fn new_via(&self, id: ItemId, n: i32, v: &PortVia) -> Option<Via> {
        let (span, kind) = self.span_of(v)?;
        let (mut pad, mut drill) = (nm(v.diameter), nm(v.drill));
        if let Some(size) = self.net_via.get(&n) {
            if (pad - size.pad).abs() <= 1 && (drill - size.drill).abs() <= 1 {
                (pad, drill) = (size.pad, size.drill);
            }
        }
        let origin = Origin::Solved { by: solvers::ORGANIC };
        Some(Via { id, net: NetId(n as u32), at: from_ref(v.pos), drill, pad, span, kind, tent: Tent::Both, backdrill: None, locked: false, origin, flash: Flash::All })
    }

    /// The port ids of the traces `traces` (indices in `ob`) that keep no Fab id, sorted, once
    /// each.
    pub(super) fn fresh_of(&self, ob: &OBoard, traces: &[usize]) -> Vec<i32> {
        let mut ids: Vec<i32> = traces.iter().map(|&ti| &ob.traces[ti]).filter(|t| self.reuse(t).is_none()).map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Each trace of `c` (net copper in `ob`) with the Fab id its lift gives it: kept, or from
    /// `fresh`; port id, Fab id and net.
    pub(super) fn ports(&self, ob: &OBoard, c: &NetCopper, fresh: &BTreeMap<i32, ItemId>) -> Vec<(i32, ItemId, i32)> {
        c.1.iter()
            .filter_map(|&ti| {
                let tr = &ob.traces[ti];
                let id = self.reuse(tr).or_else(|| fresh.get(&tr.id).copied())?;
                Some((tr.id, id, tr.net))
            })
            .collect()
    }

    /// The solution's items: every net in its relaxed form or its grid form; vias first, then
    /// tracks (each relaxed net's with every junction host before its branches), then the rest,
    /// each in grid order.
    fn assemble(&self, forms: &BTreeMap<i32, Vec<Item>>) -> Vec<Item> {
        let (mut vias, mut tracks, mut rest) = (Vec::new(), Vec::new(), Vec::new());
        let mut done = BTreeSet::new();
        for it in &self.grid.add {
            let net = match it {
                Item::Track(t) => Some(t.net.0 as i32),
                Item::Via(v) => Some(v.net.0 as i32),
                _ => None,
            };
            if let Some((n, form)) = net.and_then(|n| forms.get(&n).map(|f| (n, f))) {
                if done.insert(n) {
                    for x in form {
                        if matches!(x, Item::Via(_)) { vias.push(x.clone()) } else { tracks.push(x.clone()) }
                    }
                }
                continue;
            }
            match it {
                Item::Via(_) => vias.push(it.clone()),
                Item::Track(_) => tracks.push(it.clone()),
                _ => rest.push(it.clone()),
            }
        }
        vias.extend(tracks);
        vias.extend(rest);
        vias
    }

    /// Net `n`'s relaxed form from its copper `c`: its grid vias at their new places (a via the
    /// port left in place at its exact grid position) and on their new layers, the vias a pass
    /// added (with ids from `fresh`), then its tracks with every junction host before its
    /// branches; `None` when it does not lift. Each track runs along its trace's exported curve
    /// (`smoothed`) or, before the relax, its node polyline (the `points` form).
    pub(super) fn form(&self, ob: &OBoard, n: i32, c: &NetCopper, fresh: &Fresh, smoothed: bool) -> Option<Vec<Item>> {
        let lim = END_SNAP_NM as i128 * END_SNAP_NM as i128;
        let mut items = Vec::new();
        // the Fab via and its position of each of the net's port vias, by port index and Fab id
        let mut via_end: BTreeMap<i32, (ItemId, P)> = BTreeMap::new();
        let mut via_at: BTreeMap<ItemId, P> = BTreeMap::new();
        for (&vi, kept) in c.0.iter().zip(self.via_ids(ob, n, &c.0)) {
            let v = &ob.vias[vi];
            let (id, at) = match kept {
                Some(id) => match self.grid_ix.get(&id).map(|&g| &self.grid.add[g]) {
                    Some(Item::Via(g)) => {
                        // the port's f32 millimetres miss most nm positions by a nm or two, and a
                        // finding names a via by its position (`drc::content_id`): an unmoved via
                        // in a pad would read as a new finding
                        let r = to_ref(g.at);
                        let left = v.pos.x.to_bits() == r.x.to_bits() && v.pos.y.to_bits() == r.y.to_bits();
                        let at = if left { g.at } else { from_ref(v.pos) };
                        let mut via = Via { at, ..g.clone() };
                        // a pass may move a via to other layers; its kind follows them
                        if let Some((span, kind)) = self.span_of(v).filter(|(span, _)| *span != g.span) {
                            (via.span, via.kind) = (span, kind);
                        }
                        items.push(Item::Via(via));
                        (id, at)
                    }
                    _ => match self.board.locate(id) {
                        Some((ItemKind::Via, i)) => (id, self.board.vias[i].at),
                        _ => return None,
                    },
                },
                None => {
                    let via = self.new_via(*fresh.vias.get(&vi)?, n, v)?;
                    let end = (via.id, via.at);
                    items.push(Item::Via(via));
                    end
                }
            };
            via_end.insert(vi as i32, (id, at));
            via_at.insert(id, at);
        }

        let mut tracks: Vec<Track> = Vec::with_capacity(c.1.len());
        let mut port_ids: Vec<i32> = Vec::with_capacity(c.1.len());
        let mut hosts: Vec<[Option<i32>; 2]> = Vec::with_capacity(c.1.len());
        for &ti in &c.1 {
            let tr = &ob.traces[ti];
            let (id, own) = match self.reuse(tr) {
                Some(id) => (id, self.own.get(&id)),
                None => (*fresh.traces.get(&tr.id)?, None),
            };
            let mut ends = [(Anchor::Free, P::ZERO); 2];
            let mut host = [None; 2];
            for e in 0..2 {
                let p = if e == 0 { tr.points.first()? } else { tr.points.last()? };
                let q = from_ref(p.pos);
                ends[e] = if p.attached_via >= 0 {
                    let &(via, at) = via_end.get(&p.attached_via)?;
                    (Anchor::Via(via), at)
                } else if p.terminal_pad >= 0 {
                    let anchor = match *self.pads.get(p.terminal_pad as usize)? {
                        PadSrc::Pad { placed, pad } => Anchor::Pad { placed, pad },
                        PadSrc::Hole(h) => Anchor::Hole(h),
                    };
                    (anchor, self.exact(n, q, Some(anchor), own).map_or(q, |(_, at)| at))
                } else if p.branch_trace >= 0 {
                    host[e] = Some(p.branch_trace);
                    (Anchor::Free, q)
                } else {
                    match self.exact(n, q, None, own) {
                        // a junction on a moving host without a link (two tracks meeting end to
                        // end): placed on the host's new centreline like a branch end
                        Some((Anchor::Track { id: h, .. }, _)) if self.grid_ix.contains_key(&h) => {
                            host[e] = Some(*self.trace_ix.get(&h)?);
                            (Anchor::Free, q)
                        }
                        // a grid via of the net: an anchor only where the via now is
                        Some((Anchor::Via(v), _)) if self.grid_ix.contains_key(&v) => match via_at.get(&v) {
                            Some(&at) if at.dist2(q) <= lim => (Anchor::Via(v), at),
                            _ => (Anchor::Free, q),
                        },
                        Some(end) => end,
                        None => (Anchor::Free, q),
                    }
                };
            }
            let nodes: Vec<Vec2>;
            let curve = if smoothed {
                &tr.smoothed[..]
            } else {
                nodes = tr.points.iter().map(|p| p.pos).collect();
                &nodes[..]
            };
            let path = polyline(ends[0].1, curve, ends[1].1)?;
            let layer = *self.layers.get(usize::try_from(tr.layer).ok()?)?;
            let origin = Origin::Solved { by: solvers::ORGANIC };
            let w = self.width(n, tr.width);
            tracks.push(Track { id, net: NetId(n as u32), layer, w, widths: None, path, ends: [ends[0].0, ends[1].0], locked: false, origin });
            port_ids.push(tr.id);
            hosts.push(host);
        }
        let by_port: BTreeMap<i32, usize> = port_ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        if by_port.len() != port_ids.len() {
            return None;
        }

        // hosts before branches; a cycle of junctions does not lift. In that order each branch
        // end goes onto its host's final centreline, which runs through the node the end follows
        // (a host that moved away from the end does not lift). On a host of this form the end
        // goes onto one of the host's vertices (`junction`), so the two share one coordinate and a
        // written board reads the junction back; its anchor is taken once every host has all its
        // vertices, since an inserted vertex renumbers the segments after it.
        let order = host_order(&hosts, &by_port)?;
        let mut on_host: Vec<(usize, usize, usize)> = Vec::new();
        for &i in &order {
            for e in 0..2 {
                let Some(h) = hosts[i][e] else { continue };
                let p = if e == 0 { tracks[i].path.start } else { tracks[i].path.end() };
                let q = match by_port.get(&h) {
                    Some(&hi) if hi != i => {
                        on_host.push((i, e, hi));
                        junction(&mut tracks[hi].path, p, lim)?
                    }
                    Some(_) => return None,
                    None => {
                        let id = *self.traces.get(&h)?;
                        // a moving host that is gone; otherwise a locked track of the board
                        if self.grid_ix.contains_key(&id) {
                            return None;
                        }
                        let Some((ItemKind::Track, k)) = self.board.locate(id) else { return None };
                        let (at, q) = self.board.tracks[k].path.locate(p);
                        if p.dist2(q) > lim {
                            return None;
                        }
                        tracks[i].ends[e] = Anchor::Track { id, at };
                        q
                    }
                };
                set_end(&mut tracks[i].path, e, q);
            }
            if tracks[i].path.segs.is_empty() || tracks[i].path.start == tracks[i].path.end() {
                return None;
            }
        }
        for (i, e, hi) in on_host {
            let q = if e == 0 { tracks[i].path.start } else { tracks[i].path.end() };
            tracks[i].ends[e] = Anchor::Track { id: tracks[hi].id, at: vertex_at(&tracks[hi].path, q)? };
        }
        items.extend(order.into_iter().map(|i| Item::Track(tracks[i].clone())));
        Some(items)
    }

    /// The grid end nearest `q` within `END_SNAP_NM` (with `anchor`, when given): one of the
    /// track's own grid ends `own` when one is, else any end of net `n`.
    fn exact(&self, n: i32, q: P, anchor: Option<Anchor>, own: Option<&[End; 2]>) -> Option<End> {
        let lim = END_SNAP_NM as i128 * END_SNAP_NM as i128;
        let fits = |(a, p): &&End| anchor.map_or(true, |x| x == *a) && q.dist2(*p) <= lim;
        if let Some(&end) = own.into_iter().flatten().filter(fits).min_by_key(|(_, p)| q.dist2(*p)) {
            return Some(end);
        }
        let ends = self.ends.get(&n)?;
        let lo = ends.partition_point(|(_, p)| (p.x as i64) < q.x as i64 - END_SNAP_NM as i64);
        ends[lo..]
            .iter()
            .take_while(|(_, p)| p.x as i64 <= q.x as i64 + END_SNAP_NM as i64)
            .filter(fits)
            .min_by_key(|(_, p)| q.dist2(*p))
            .copied()
    }

    /// The Fab width of a port width on net `n`: a grid track's own width when it is that one.
    fn width(&self, n: i32, w: f32) -> Nm {
        self.widths.get(&n).and_then(|ws| ws.iter().find(|(b, _)| *b == w.to_bits())).map_or_else(|| nm(w), |(_, w)| *w)
    }
}

/// The id tables of a lift (see `Lift`).
struct Ids {
    traces: BTreeMap<i32, ItemId>,
    trace_net: BTreeMap<i32, i32>,
    vias: Vec<ItemId>,
    pads: Vec<PadSrc>,
    layers: Vec<LayerId>,
}

/// The tracks in an order that puts every junction host among them (`by_port`: port id to
/// track) before its branches, depth first in track order; `None` on a cycle.
fn host_order(hosts: &[[Option<i32>; 2]], by_port: &BTreeMap<i32, usize>) -> Option<Vec<usize>> {
    // 0 not seen, 1 on the current path, 2 placed
    let mut state = vec![0u8; hosts.len()];
    let mut order = Vec::with_capacity(hosts.len());
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for root in 0..hosts.len() {
        if state[root] != 0 {
            continue;
        }
        state[root] = 1;
        stack.push((root, 0));
        while let Some(&(i, e)) = stack.last() {
            if e == 2 {
                stack.pop();
                state[i] = 2;
                order.push(i);
                continue;
            }
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            let Some(&h) = hosts[i][e].as_ref().and_then(|h| by_port.get(h)) else { continue };
            match state[h] {
                0 => {
                    state[h] = 1;
                    stack.push((h, 0));
                }
                1 => return None,
                _ => {}
            }
        }
    }
    Some(order)
}

/// A path's bends (vertices that turn), total turning (micro-degrees) and corners sharper than
/// 90 degrees, over its vertices (an arc by its mid point).
fn corners(path: &Path) -> (u32, i64, u32) {
    let mut pts = vec![path.start];
    for s in &path.segs {
        match *s {
            Seg::Line(p) => pts.push(p),
            Seg::Arc { mid, end } => pts.extend([mid, end]),
        }
    }
    let (mut bends, mut turning, mut acute) = (0, 0, 0);
    for w in pts.windows(3) {
        let (ax, ay) = (w[1].x as f64 - w[0].x as f64, w[1].y as f64 - w[0].y as f64);
        let (bx, by) = (w[2].x as f64 - w[1].x as f64, w[2].y as f64 - w[1].y as f64);
        let (cross, dot) = (ax * by - ay * bx, ax * bx + ay * by);
        if cross == 0.0 && dot >= 0.0 {
            continue;
        }
        bends += 1;
        turning += (cross.atan2(dot).abs().to_degrees() * 1e6).round() as i64;
        if dot < 0.0 {
            acute += 1;
        }
    }
    (bends, turning, acute)
}

/// A vertex closer than this to the segment between its neighbours is not a bend a board can
/// make: the lift drops it, so a straight run of the relaxer's resampled points (about 0.06 mm
/// apart, with nm rounding jitter) is written as one segment, and a gentle curve as chords that
/// stay within about this distance of it. The lift verifies the result like any other form.
pub const STRAIGHT_NM: Nm = 1_000;

/// The output grid of a lifted track's interior vertices: each lies on a multiple of this in x and
/// y, so a written board reads in whole micrometres. Its ends are not rounded: each sits exactly on
/// its anchor (a pad, via, hole or host track).
pub const OUTPUT_GRID_NM: Nm = 1_000;

/// `p` rounded to the nearest point of the output grid (halves upwards).
fn on_output_grid(p: P) -> P {
    let g = i64::from(OUTPUT_GRID_NM);
    let round = |v: Nm| Nm::try_from((i64::from(v) + g / 2).div_euclid(g) * g).unwrap_or(v);
    P { x: round(p.x), y: round(p.y) }
}

/// A track path from `start` along the exported curve's interior to `end`: interior vertices on
/// the output grid (`OUTPUT_GRID_NM`), without vertices that lie on the way between their
/// neighbours (`STRAIGHT_NM`); `None` when it has no segment.
fn polyline(start: P, pts: &[Vec2], end: P) -> Option<Path> {
    if pts.len() < 2 {
        return None;
    }
    let mut v = vec![start];
    for q in &pts[1..pts.len() - 1] {
        let p = on_output_grid(from_ref(*q));
        if v.last() != Some(&p) {
            v.push(p);
        }
    }
    if v.last() != Some(&end) {
        v.push(end);
    }
    drop_straight(&mut v);
    let mut path = Path::new(v[0]);
    path.segs.extend(v[1..].iter().map(|&p| Seg::Line(p)));
    (!path.segs.is_empty()).then_some(path)
}

/// Drop each interior vertex within `STRAIGHT_NM` of the segment from the last kept vertex to the
/// next one, scanning again until a scan drops nothing; then every kept interior vertex is
/// farther than `STRAIGHT_NM` from the segment between its neighbours (so also from each of them:
/// no segment shorter than that ends at an interior vertex). Each scan only removes, so this
/// ends: O(n) per scan, a few scans in practice.
fn drop_straight(v: &mut Vec<P>) {
    loop {
        let mut kept = Vec::with_capacity(v.len());
        kept.push(v[0]);
        for i in 1..v.len().saturating_sub(1) {
            let a = *kept.last().expect("the start is kept");
            if point_segment_distance(v[i], a, v[i + 1]) > STRAIGHT_NM as f64 {
                kept.push(v[i]);
            }
        }
        if v.len() > 1 {
            kept.push(v[v.len() - 1]);
        }
        if kept.len() == v.len() {
            return;
        }
        *v = kept;
    }
}

/// Distance in nm from `p` to the segment `a`-`b`.
fn point_segment_distance(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.x as f64 - a.x as f64, b.y as f64 - a.y as f64);
    let (px, py) = (p.x as f64 - a.x as f64, p.y as f64 - a.y as f64);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { ((px * dx + py * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    ((px - t * dx).powi(2) + (py - t * dy).powi(2)).sqrt()
}

/// The vertex of the polyline `host` that an end at `p` joins (`lim`: the squared reach): the
/// host's vertex nearest `p` within reach (the junction node the end follows, on the output grid),
/// else a vertex inserted on the output grid at `p`'s nearest point on the host, when that point is
/// within reach (the host's vertex there was dropped as straight). A reader finds a junction from
/// coordinates alone, and on its host's vertex it needs no rounding of a point along a segment.
fn junction(host: &mut Path, p: P, lim: i128) -> Option<P> {
    let vertices = std::iter::once(host.start).chain(host.segs.iter().map(Seg::end));
    if let Some(v) = vertices.filter(|v| v.dist2(p) <= lim).min_by_key(|v| v.dist2(p)) {
        return Some(v);
    }
    let (at, q) = host.locate(p);
    if p.dist2(q) > lim {
        return None;
    }
    let g = on_output_grid(q);
    let seg = at.seg as usize;
    let a = if seg == 0 { host.start } else { host.segs[seg - 1].end() };
    // the rounding may land on the segment's own vertex: the junction is that vertex
    if g != a && g != host.segs[seg].end() {
        host.segs.insert(seg, Seg::Line(g));
    }
    Some(g)
}

/// Where on `path` its vertex `v` lies: the start, or the end of the segment that ends there.
fn vertex_at(path: &Path, v: P) -> Option<OnTrack> {
    if path.start == v {
        return Some(OnTrack::default());
    }
    let seg = path.segs.iter().position(|s| s.end() == v)?;
    Some(OnTrack { seg: u32::try_from(seg).ok()?, frac: OnTrack::ONE })
}

/// Move end `e` of a polyline path to `q`, dropping the segment it empties.
fn set_end(path: &mut Path, e: usize, q: P) {
    if e == 0 {
        path.start = q;
        if path.segs.len() > 1 && path.segs[0].end() == q {
            path.segs.remove(0);
        }
    } else if let Some(last) = path.segs.last_mut() {
        *last = Seg::Line(q);
        let n = path.segs.len();
        if n > 1 && path.segs[n - 2].end() == q {
            path.segs.pop();
        }
    }
}
