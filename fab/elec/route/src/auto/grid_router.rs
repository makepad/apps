//! The grid router (E6 5.2, R1): nets in plan order, each grown as a tree from its first
//! terminal (Prim), or from the one with the most free ends of fixed copper: search from every
//! access cell of the tree (its terminals and the copper routed so far, so T-junctions arise) to
//! the access cells of the other terminals, lift the
//! path to tracks and vias, verify them, add them to the world's overlay and drop the grid
//! tiles they reach. A transaction routes one planned net, settles the fills and connectivity,
//! and repairs nets it cut off their pours. A cut the repairs cannot close re-routes the planned
//! net once with keepouts on the corridors the cut nets need, found on the pre-transaction board.
//! If a cut remains, the whole transaction rolls back.
//! Only the settled, legal result is offered as step `k`.
//! A free end of fixed copper is a route somebody started, continued from its tip: each join
//! first searches from the tree's unused ends to the other terminals' ends (their access if they
//! have none), then from the whole tree to those, then today's query. An end no search reaches
//! stays free, and DRC reports it; the net still connects.
//!
//! At `Effort::Fast` the job ends after the first pass. At `Effort::Full`, after the first pass,
//! each pass tries every net left `Blocked`, in plan order: a plain
//! route first (the board may have changed), then rip its blockers, route it, and reroute the
//! ripped nets. A net that finds itself blocked only by nets neither attempted nor ripped yet rips
//! those too and routes on: the attempted net when it meets one blockade after another, and a
//! ripped net whose way back is held by a third net (a displacement that has to pass along a
//! corridor). The nets it rips join the end of the reroute queue. Every such rip adds at least
//! one net to the attempt's ripped set, so an attempt rips each problem net at most once.
//! A repair net outside the problem is ripped by its repairs: its job copper falls into groups
//! joined end to end (never through a pad or a pour), and its first rip in an attempt takes only
//! the groups near a contested cell of the blockers path, unless that is none or all of them.
//! Named again, it loses the rest and is barred like any ripped net, so an attempt rips it at
//! most twice. It comes back as a repair: its access is looked for in the box of its ripped copper
//! and of the copper the attempted net laid, where the cut is.
//! An attempt is kept only when it lowers the open-net count and every ripped net outside the
//! problem is joined again; otherwise it rolls back and the contested cells of its blockers path
//! gain history cost, so the next search prefers another corridor. Only owned copper is ripped:
//! the job's own copper, so a repair the job laid for a pour it cut can move like any route (it
//! reroutes after the problem's nets), and in a re-route (`RouteProblem::reroute`) the board's
//! solved, unlocked copper of any net: a routed net outside the problem ripped so routes again
//! after the problem's nets, from its terminals anywhere, and must be joined again like a repair.
//! Otherwise a net's tree on the board is kept and new pins join it.
//!
//! What a rip costs depends on who asks. Inside an attempt, a net's blockers search prices the
//! attempted net and the nets ripped so far (but itself) at [`BARRED`], and the attempted net's
//! search also prices the nets its failed attempts left open (the phase's failed pairs). So
//! `Blocked` names such a net only when every way crosses one. The search runs as before first
//! and again priced only when its answer names such a net, so nothing changes where it does
//! not. A failed attempt records its pairs: the attempted net with each net open at its end
//! that was not open at its start or, when there is none, with each net of its first blocker
//! set. A net's attempts in a pass end with a failed attempt that records no new pair; the
//! phase ends after a pass that routes no more nets. Kept attempts strictly lower the open
//! count, and a net records each other net at most once in the phase, so this ends.
//!
//! Each step of an attempt is offered (ripped, rerouted, kept, rolled back). A kept attempt records
//! each net's job copper before and after it, the attempted net's apart from the ripped nets' (the
//! loss) and the pours it cut (`RouteMetrics::kept`). Cost per attempt: a snapshot (one board
//! clone), the plain route, then per rip a world rebuild (O(pieces added)) and one settle, and one
//! transaction for the blocked net and for each ripped net, plus one more for each net that rips in
//! turn (at most the problem's nets, each once, and each repair net twice). A repair net's rip adds
//! O(its items x log) for its groups and O(contested cells x its pieces). A priced search runs only
//! after one that named a barred net; it explores every cheaper way first, so it can visit the
//! whole reachable grid.
//!
//! Legality: the grid passes only steps its exact step rule allows ([`Grid::step`]); access stubs
//! are verified when they are made; the verifier then checks everything a net adds with the
//! functions DRC calls. A verifier rejection is an engine fault: the net stays open with the
//! collision as its reason, nothing illegal is added. No net the board had connected is cut.
//!
//! Open nets say why: the cheapest-blockers search names the rippable nets in the way
//! (`Reason::Blocked`); a net no search can reach is `Reason::NoRoom`; a terminal with no
//! legal access is `Reason::Terminal`.
//!
//! Determinism: the plan order, sorted access lists, the search's total tie order, ids issued
//! in order from the board's `next_item`. Cost per net: the searches (O(V log V) in visited
//! states) plus access (O(candidates x (log n + k)) per terminal piece) and the verifier.
//! Each transaction snapshots the settled state with one board clone. Each settle round applies
//! only that round's ops, updates fills of the zones its delta reaches, and updates the index and
//! connectivity in proportion to the change. Each cut net is tried at most once per attempt.
//! Only transactions whose repairs failed pay for one corridor search per unreachable pocket
//! plus one more route of the planned net.
//!
//! Guard (`Costs::guard`, 0 turns it off): every pad that connects only through a pour keeps an
//! escape, a shortest path on its layer to a cell with room for a via of its net, and other nets'
//! routes pay a soft cost on its cells (never a keepout; see `grid::guard`). The table is built
//! before the first transaction and refreshed after every settle (a rip's too) and every restore
//! (a transaction's or a rip-up attempt's): only the escapes of pads that became or stopped being
//! pour-only, or whose watch box meets an area the grid invalidated since the last refresh, are
//! found again. So the kept table always equals a rebuild. Its cost per refresh: one pass over the
//! connectivity's joins, the stale test (entries x changed areas) and one escape flood per stale
//! pad; nothing on a board without pour-only pads.

use crate::auto::access::{access, end_access, zone_access, Access, AccessNet};
use crate::grid::guard::{escape, pour_only_pads, Escape, EscapeIn, GuardTable, PadKey};
use crate::grid::lift::{lift, Lifted};
use crate::grid::search::{search_blockers, search_widening, Costs, Query, Rip};
use crate::grid::{Cell, Grid, GridSpec, ObstacleSource, PlaneId, Probe};
use crate::problem::{anchor_point, class_track_width, rnet, Autorouter, RNet, RouteProblem, Terminal};
use crate::request::Effort;
use crate::progress::{Progress, RetryWhy};
use crate::solution::{Kept, NetCopper, Reason, RouteSolution, SearchExpansions};
use crate::solvers;
use crate::source::WorldSource;
use crate::verify::{new_pieces, verify};
use crate::world::{Fixity, Overlay, RItem, RPiece, World};
use fab_elec_board::drc::sel_of;
use fab_elec_board::{
    connect_indexed, fill, fill_update, Anchor, Board, BoardOp, ConnItem, Connectivity, Flash, IndexChange, Item, Origin,
    PieceIndex, RuleItem, RuleKey, RuleMatrix, Tent, Track, Via, ViaKind, ZoneFill,
};
use fab_elec_circuit::Measure;
use fab_elec_core::{point_clearance, BBox, ItemId, LayerId, LayerSet, NetId, Nm, OnTrack, PolySet, Sel, Shape, P};
use fab_elec_parts::Forbid;
use std::collections::{BTreeMap, BTreeSet};

/// Cost of ripping one net's copper for one cell, in step units (the blockers search).
const RIP_CELL: u32 = 10;
/// Cost of entering a cell of a net the searching net may not rip: above any path's other cost,
/// so a blockers path crosses such a net only where no other way exists. A price, not a wall.
const BARRED: u32 = u32::MAX;
/// History cost a contested cell gains per failed rip-up attempt, in step units: one more rip
/// of that cell, so a corridor whose rip-up failed costs more for every later search.
/// A cost, never a limit.
const HISTORY: u16 = RIP_CELL as u16;
/// The first search window: the terminals' box grown by this many cells (it widens to the board).
const WINDOW_MARGIN: Nm = 16;

/// The R1 grid autorouter.
#[derive(Clone, Debug, Default)]
pub struct GridRouter {
    pub costs: Costs,
}

impl Autorouter for GridRouter {
    fn name(&self) -> &str {
        "grid"
    }

    fn route(&mut self, p: &RouteProblem, progress: &Progress) -> RouteSolution {
        let mut costs = self.costs.clone();
        costs.prefs = p.stack.prefs.clone();
        let Some(mut r) = Router::new(p, costs) else {
            let mut sol = RouteSolution::default();
            for n in &p.nets {
                let at = n.terminals.first().map_or(P::ZERO, |t| t.bbox.center());
                sol.open.push((n.net, Reason::Terminal { at, with: None }));
            }
            sol.metrics.nets_open = sol.open.len() as u32;
            sol.metrics.work.open_after_first_pass = sol.metrics.nets_open;
            return sol;
        };
        progress.set_total(p.nets.len() as u32);
        let mut before = r.work;
        r.refresh_guard();
        for (step, planned) in p.nets.iter().enumerate() {
            if progress.cancelled() {
                r.sol.open.push((planned.net, Reason::NotTried));
                r.sol.metrics.nets_open += 1;
                continue;
            }
            progress.begin_net(planned.net);
            // The net's terminals as the board stands now: a route before it may have cut its pour
            // into islands, each a terminal to join (its cuts are left to it, as it comes later).
            match rnet(&r.settled.board, &p.rules, &r.settled.conn, &p.stack, planned.net) {
                Some(now) => r.transaction(&RNet { terminals: now.terminals, ..planned.clone() }, &p.nets[step + 1..], BBox::EMPTY, progress),
                None => r.sol.metrics.nets_routed += 1,
            }
            r.publish(progress, &mut before);
        }
        // Cancellation can leave the last planned nets open without an offer.
        r.sol.metrics.work.open_after_first_pass = r.sol.metrics.nets_open;
        if p.effort == Effort::Full && !progress.cancelled() {
            r.rip_up(progress, &mut before);
        }
        r.sol
    }
}

#[derive(Clone)]
struct Settled {
    board: Board,
    fills: Vec<ZoneFill>,
    conn: Connectivity,
    index: PieceIndex,
}

struct Snapshot {
    settled: Settled,
    overlay: Overlay,
    sol: RouteSolution,
    next_id: u32,
}

/// What an attempt has ripped so far.
#[derive(Default)]
struct Taken {
    /// Every ripped net, in the order of its first rip.
    order: Vec<NetId>,
    /// The nets ripped whole: barred for the rest of the attempt. A net in `order` and not here is
    /// a repair net that lost only the groups in the way.
    whole: Vec<NetId>,
    /// Per ripped net, the box of all the job copper it lost.
    boxes: BTreeMap<NetId, BBox>,
}

enum Outcome {
    Accepted,
    /// Rolled back; `learned` when it recorded a failed pair the phase had not seen.
    Failed { learned: bool },
    Stop,
}

struct Stuck {
    net: NetId,
    track: PlaneId,
    via: Option<PlaneId>,
    layers: Vec<usize>,
    sources: Vec<(usize, Cell)>,
    /// One entry per terminal the search could not reach: its access cells, sorted, deduplicated.
    pockets: Vec<Vec<(usize, Cell)>>,
    /// The repair's track width and via pad.
    w: Nm,
    via_pad: Nm,
}

struct Router<'p> {
    p: &'p RouteProblem,
    world: World<'p>,
    grid: Grid,
    /// The grid on which any net's copper but a pad may be crossed, for naming the kept copper
    /// in a net's way (`Router::in_the_way`): made on first use.
    what_if: Option<Grid>,
    costs: Costs,
    next_id: u32,
    sol: RouteSolution,
    work: u64,
    // Publish the search breakdown at the same boundary as the existing expansion total.
    by_search: SearchExpansions,
    settled: Settled,
    stuck: Vec<Stuck>,
    contested: BTreeMap<NetId, Vec<(usize, Cell)>>,
    /// The pour-only pads' escapes, current after every settle and restore.
    guard: GuardTable,
    /// Areas the grid invalidated since the guard's last refresh.
    touched: Vec<BBox>,
    /// The rip-up attempt under way: its net and the nets it ripped so far.
    attempt: Option<(NetId, Vec<NetId>)>,
    /// The phase's failed pairs: the nets each net's failed attempts left open.
    failed: BTreeMap<NetId, BTreeSet<NetId>>,
}

impl<'p> Router<'p> {
    /// `None` when the board has no outline or nothing to route.
    ///
    /// The pitch is the finest any net of the board routes at, not only the problem's nets: a
    /// coarse grid inflates obstacles further and keeps its cells further apart, so a job routing
    /// only wide nets could find no access to a fine-pitch pad beside another net's copper that a
    /// job routing every net reaches. One board, one grid, whichever nets a job routes. Cost:
    /// O(board nets) here; a job of wide nets only searches the finer grid's cells.
    fn new(p: &'p RouteProblem, costs: Costs) -> Option<Router<'p>> {
        let area = p.board.outline_bbox();
        if area.is_empty() || p.stack.layers.is_empty() {
            return None;
        }
        let first = p.stack.layers[0];
        let pitch_of = |class, w: Nm| {
            let key = RuleKey { class, item: RuleItem::Track };
            GridSpec::pitch_for(w + p.rules.clearance(key, key, first))
        };
        let routed = p.nets.iter().map(|n| pitch_of(n.class, width(n))).min()?;
        let pitch = p.board.nets.iter()
            .map(|net| pitch_of(net.class, class_track_width(&p.rules, net.class)))
            .fold(routed, Nm::min);
        let spec = GridSpec::covering(area, pitch, p.stack.layers.clone());
        let mut fills = (*p.fills).clone();
        fills.sort_by_key(|fill| fill.zone);
        Some(Router {
            p,
            world: World::new(&p.base),
            grid: Grid::new(spec),
            what_if: None,
            costs,
            next_id: p.board.next_item,
            sol: RouteSolution::default(),
            work: 0,
            by_search: SearchExpansions::default(),
            settled: Settled {
                board: (*p.board).clone(),
                fills,
                conn: (*p.conn).clone(),
                index: PieceIndex::build(&p.board, &p.fills),
            },
            stuck: Vec::new(),
            contested: BTreeMap::new(),
            guard: GuardTable::default(),
            touched: Vec::new(),
            attempt: None,
            failed: BTreeMap::new(),
        })
    }

    fn alloc(&mut self) -> ItemId {
        let id = ItemId(self.next_id);
        self.next_id += 1;
        id
    }

    fn publish(&mut self, progress: &Progress, before: &mut u64) {
        self.sol.metrics.work.transactions += 1;
        self.sol.metrics.work.expansions = self.work;
        self.sol.metrics.work.by_search = self.by_search;
        let s = &self.by_search;
        debug_assert_eq!(s.widening + s.blockers + s.priced_blockers + s.what_if + s.escape, self.work);
        progress.add_work(self.work - *before);
        *before = self.work;
        progress.offer(self.sol.metrics.work.transactions, &self.sol);
    }

    /// Negotiate congestion until a whole pass cannot reduce the number of open nets.
    fn rip_up(&mut self, progress: &Progress, before: &mut u64) {
        let first_pass_work = self.work;
        loop {
            let open_at_start = self.sol.open.len();
            let candidates: Vec<_> = self.p.nets.iter()
                .filter(|n| self.sol.open.iter().any(|(net, reason)| {
                    *net == n.net && matches!(reason, Reason::Blocked { .. })
                }))
                .map(|n| n.net)
                .collect();
            for net in candidates {
                while self.sol.open.iter().any(|(n, reason)| {
                    *n == net && matches!(reason, Reason::Blocked { .. })
                }) {
                    if progress.cancelled() {
                        return;
                    }
                    let outcome = self.attempt(net, progress, before);
                    let counts = &self.sol.metrics.work.by_attempt;
                    debug_assert_eq!(counts.kept + counts.rolled_back, self.work - first_pass_work,
                        "every expansion since the first pass belongs to one completed rip-up attempt");
                    match outcome {
                        Outcome::Failed { learned: true } => continue,
                        Outcome::Accepted | Outcome::Stop | Outcome::Failed { learned: false } => break,
                    }
                }
            }
            if self.sol.open.len() >= open_at_start {
                break;
            }
        }
    }

    /// Keep a rip and its reroutes only when they reduce the snapshot's open-net count. While it
    /// runs, the searches price what each net may not rip (see the module doc).
    fn attempt(&mut self, n: NetId, progress: &Progress, before: &mut u64) -> Outcome {
        self.attempt = Some((n, Vec::new()));
        let outcome = self.try_attempt(n, progress, before);
        self.attempt = None;
        outcome
    }

    fn try_attempt(&mut self, n: NetId, progress: &Progress, before: &mut u64) -> Outcome {
        let p = self.p;
        let started = self.work;
        let mut unkeepable_at = None;
        let saved = Snapshot {
            settled: self.settled.clone(),
            overlay: self.world.clone().into_overlay(),
            sol: self.sol.clone(),
            next_id: self.next_id,
        };
        let open_before = self.sol.open.len();
        let open_at_start: BTreeSet<NetId> = self.sol.open.iter().map(|(net, _)| *net).collect();
        let mut area = BBox::EMPTY;
        let Some(rn) = rnet(&self.settled.board, &p.rules, &self.settled.conn, &p.stack, n) else {
            self.rollback_attempt(&saved, area, started, unkeepable_at);
            return Outcome::Stop;
        };
        self.sol.open.retain(|(net, _)| *net != n);
        self.sol.metrics.nets_open -= 1;
        progress.begin_net(n);
        self.contested.clear();
        self.transaction(&rn, &[], BBox::EMPTY, progress);
        if self.sol.open.len() < open_before {
            self.sol.metrics.work.by_attempt.kept += self.work - started;
            self.keep(n, &[], &saved);
            self.publish(progress, before);
            return Outcome::Accepted;
        }
        let reason = self.sol.open.iter().find(|(net, _)| *net == n)
            .expect("a failed plain route leaves its net open").1.clone();
        let (at, by, cells) = match (&reason, self.contested.get(&n)) {
            (Reason::Blocked { at, by }, Some(cells)) if !by.is_empty() && !cells.is_empty() => {
                (*at, by.clone(), cells.clone())
            }
            _ => {
                self.rollback_attempt(&saved, area, started, unkeepable_at);
                self.sol.open.iter_mut().find(|(net, _)| *net == n)
                    .expect("the snapshot has the attempted net open").1 = reason;
                return Outcome::Stop;
            }
        };
        let first = by.clone();
        // The plain try was speculative: drop its partial copper without offering it.
        self.restore_attempt(&saved, area);

        // Rip the blockers and route the net; while it is still blocked, by owned copper of nets
        // not ripped yet (a net cut off in several places meets one blockade after another), rip
        // those too and route it on from what it has. Each round rips at least one more net.
        let (mut at, mut by, mut history) = (at, by, cells);
        let mut contested = history.clone();
        let mut taken = Taken::default();
        loop {
            self.rip(n, at, &by, &contested, &mut taken, &mut area, progress, before);
            self.attempt = Some((n, taken.whole.clone()));
            if progress.cancelled() {
                self.rollback_attempt(&saved, area, started, unkeepable_at);
                self.publish(progress, before);
                return Outcome::Stop;
            }
            let rn = rnet(&self.settled.board, &p.rules, &self.settled.conn, &p.stack, n);
            self.sol.open.retain(|(net, _)| *net != n);
            self.sol.metrics.nets_open -= 1;
            self.contested.remove(&n);
            if let Some(rn) = rn {
                self.transaction(&rn, &[], BBox::EMPTY, progress);
            } else {
                self.sol.metrics.nets_routed += 1;
            }
            self.publish(progress, before);
            let Some((next_at, next_by, cells)) = self.fresh_blockers(n, n, &taken.whole) else {
                if self.sol.open.iter().any(|(open, _)| *open == n) {
                    unkeepable_at = Some(self.work);
                }
                break;
            };
            (at, by) = (next_at, next_by);
            contested = cells.clone();
            history.extend(cells);
        }

        // The ripped nets route again, in plan order, then the ripped repairs of nets outside the
        // problem (pours, as zone-served nets come last). One blocked only by nets neither `n` nor
        // ripped whole yet rips those and routes on, and they join the end of the queue unless
        // they are still waiting in it.
        let mut queue = reroute_order(p, &taken.order);
        let mut next = 0;
        while let Some(&net) = queue.get(next) {
            next += 1;
            if progress.cancelled() {
                self.rollback_attempt(&saved, area, started, unkeepable_at);
                self.publish(progress, before);
                return Outcome::Stop;
            }
            progress.begin_net(net);
            progress.retry_net(net, RetryWhy::Ripped);
            loop {
                self.sol.open.retain(|(open, _)| *open != net);
                self.sol.metrics.nets_open -= 1;
                self.contested.remove(&net);
                if let Some(rm) = rnet(&self.settled.board, &p.rules, &self.settled.conn, &p.stack, net) {
                    let focus = self.return_focus(net, n, &taken, saved.next_id);
                    self.transaction(&rm, &[], focus, progress);
                } else {
                    self.sol.metrics.nets_routed += 1;
                }
                self.publish(progress, before);
                let Some((at, by, cells)) = self.fresh_blockers(net, n, &taken.whole) else {
                    if unkeepable_at.is_none() && self.sol.open.iter().any(|(open, _)| *open == net) {
                        unkeepable_at = Some(self.work);
                    }
                    break;
                };
                self.rip(net, at, &by, &cells, &mut taken, &mut area, progress, before);
                if progress.cancelled() {
                    self.rollback_attempt(&saved, area, started, unkeepable_at);
                    self.publish(progress, before);
                    return Outcome::Stop;
                }
                self.attempt = Some((n, taken.whole.clone()));
                let again: Vec<NetId> = reroute_order(p, &by).into_iter().filter(|net| !queue[next..].contains(net)).collect();
                queue.extend(again);
                history.extend(cells);
            }
        }
        // A ripped net outside the problem is a pour the job had repaired, or a routed net a
        // re-route names: the attempt holds only when it is joined again, so a net left cut never
        // passes for success.
        let rejoined = taken.order.iter().all(|net| {
            p.nets.iter().any(|r| r.net == *net)
                || !self.settled.conn.unrouted.contains(net)
                || saved.settled.conn.unrouted.contains(net)
        });
        if rejoined && self.sol.open.len() < open_before {
            // Offered as its own step, so the stream ends on the solution with its record.
            self.sol.metrics.work.by_attempt.kept += self.work - started;
            self.keep(n, &taken.order, &saved);
            self.publish(progress, before);
            return Outcome::Accepted;
        }
        // The nets this attempt left open; with none, the rips of its first blocker set failed.
        let mut culprits: BTreeSet<NetId> = self.sol.open.iter().map(|(net, _)| *net)
            .filter(|net| !open_at_start.contains(net)).collect();
        if culprits.is_empty() {
            culprits.extend(first);
        }
        let pairs = self.failed.entry(n).or_default();
        let learned = culprits.into_iter().fold(false, |new, net| pairs.insert(net) | new);
        self.rollback_attempt(&saved, area, started, unkeepable_at);
        for (li, cell) in history {
            self.grid.add_extra(li, cell, HISTORY);
        }
        self.sol.open.iter_mut().find(|(net, _)| *net == n)
            .expect("the snapshot has the attempted net open").1 = reason;
        self.publish(progress, before);
        Outcome::Failed { learned }
    }

    /// Record kept attempt `n` that ripped `ripped`: each net's job copper in the snapshot and now.
    /// Cost: O(job items + changed nets x log), once per kept attempt.
    fn keep(&mut self, n: NetId, ripped: &[NetId], saved: &Snapshot) {
        let before = copper_by_net(&saved.sol.add);
        let after = copper_by_net(&self.sol.add);
        let of = |net: NetId| {
            let (length_before, vias_before) = before.get(&net).copied().unwrap_or_default();
            let (length_after, vias_after) = after.get(&net).copied().unwrap_or_default();
            NetCopper { net, length_before, vias_before, length_after, vias_after }
        };
        let others = before.keys().chain(after.keys()).copied().collect::<BTreeSet<_>>().into_iter()
            .filter(|net| *net != n && !ripped.contains(net) && before.get(net) != after.get(net))
            .map(of)
            .collect();
        let kept = Kept { own: of(n), ripped: ripped.iter().map(|net| of(*net)).collect(), others };
        self.sol.metrics.kept.push(kept);
    }

    /// The nets `net`'s blockers search may not rip: inside an attempt, the attempted net and the
    /// nets ripped so far (but `net` itself), and for the attempted net its failed pairs.
    fn barred(&self, net: NetId) -> BTreeSet<NetId> {
        let Some((n, ripped)) = &self.attempt else { return BTreeSet::new() };
        let mut barred: BTreeSet<NetId> = std::iter::once(*n).chain(ripped.iter().copied())
            .filter(|other| *other != net).collect();
        if net == *n {
            barred.extend(self.failed.get(&net).into_iter().flatten().copied());
        }
        barred
    }

    /// Where `net`'s last route stopped and the cells it contests, when it is blocked only by
    /// nets other than the attempted `n` and not in `ripped`: the nets it may rip next.
    fn fresh_blockers(&self, net: NetId, n: NetId, ripped: &[NetId]) -> Option<(P, Vec<NetId>, Vec<(usize, Cell)>)> {
        match (self.sol.open.iter().find(|(open, _)| *open == net), self.contested.get(&net)) {
            (Some((_, Reason::Blocked { at, by })), Some(cells))
                if !by.is_empty() && !cells.is_empty() && by.iter().all(|b| *b != n && !ripped.contains(b)) =>
            {
                Some((*at, by.clone(), cells.clone()))
            }
            _ => None,
        }
    }

    /// Remove the owned copper of nets `by` for net `n` blocked at `at` with `contested` cells:
    /// they open, `Blocked` by `n`, and the world, the settled board and the guard follow; `area`
    /// grows by what went and `taken` records it. A repair net outside the problem ripped for the
    /// first time in this attempt loses only its groups in the way ([`Router::groups_in_way`]);
    /// any other net goes whole and is barred from then on.
    fn rip(
        &mut self,
        n: NetId,
        at: P,
        by: &[NetId],
        contested: &[(usize, Cell)],
        taken: &mut Taken,
        area: &mut BBox,
        progress: &Progress,
        before: &mut u64,
    ) {
        let p = self.p;
        let mut take: BTreeSet<ItemId> = BTreeSet::new();
        let mut whole: Vec<NetId> = Vec::new();
        for &net in by {
            let first = !taken.order.contains(&net);
            let part = if first && !p.nets.iter().any(|r| r.net == net) {
                self.groups_in_way(n, net, contested)
            } else {
                None
            };
            match part {
                Some(ids) => take.extend(ids),
                None => {
                    take.extend(self.sol.add.iter().filter(|item| copper_net(item) == Some(net)).map(Item::id));
                    whole.push(net);
                    taken.whole.push(net);
                }
            }
            if first {
                taken.order.push(net);
            }
        }
        let mut ripped = BTreeSet::new();
        let board = &self.settled.board;
        self.sol.add.retain(|item| {
            let Some(net) = copper_net(item).filter(|_| take.contains(&item.id())) else { return true };
            let bbox = item.bbox(board);
            *area = area.union(bbox);
            let lost = taken.boxes.entry(net).or_insert(BBox::EMPTY);
            *lost = lost.union(bbox);
            ripped.insert(item.id());
            false
        });
        for it in p.base.sorted() {
            if !p.owned(it) || !it.net().is_some_and(|net| whole.contains(&net)) {
                continue;
            }
            if let Some(id) = it.item {
                if let Err(index) = self.sol.remove.binary_search(&id) {
                    let item = board.item(id).expect("owned base copper is on the settled board");
                    *area = area.union(item.bbox(board));
                    self.sol.remove.insert(index, id);
                    ripped.insert(id);
                }
            }
        }
        self.world = World::new(&p.base);
        for &id in &self.sol.remove {
            self.world.remove_item(id);
        }
        for item in &self.sol.add {
            for piece in new_pieces(&p.board, item) {
                self.world.add(piece);
            }
        }
        let ops: Vec<_> = ripped.into_iter().map(BoardOp::Remove).collect();
        settle(&mut self.settled, &p.rules, &ops);
        self.invalidate(*area);
        self.refresh_guard();
        for &net in by {
            let reason = Reason::Blocked { at, by: vec![n] };
            if let Some((_, previous)) = self.sol.open.iter_mut().find(|(open, _)| *open == net) {
                *previous = reason;
            } else {
                // A routed net outside the problem that a re-route rips is the job's to join again:
                // counted as one it reconnects, as a repaired pour is.
                if !p.nets.iter().any(|r| r.net == net) {
                    if let Err(index) = self.sol.repaired.binary_search(&net) {
                        self.sol.repaired.insert(index, net);
                        self.sol.metrics.nets_routed += 1;
                    }
                }
                self.sol.open.push((net, reason));
                self.sol.metrics.nets_routed -= 1;
                self.sol.metrics.nets_open += 1;
            }
        }
        self.sol.metrics.work.rips += by.len() as u32;
        self.recount();
        progress.retry_net(n, RetryWhy::RipUp);
        self.publish(progress, before);
    }

    /// The job copper of repair net `net` in `n`'s way. Its tracks and vias fall into groups
    /// joined end to end (a track's end anchored on another of them; a pad or a pour joins nothing
    /// here, so two repairs meeting at one pad stay apart). A group is in the way when one of its
    /// copper pieces lies within the reach of `n`'s track or via probe (largest need, half width
    /// and grid padding) of a `contested` cell's centre on that cell's layer. `None` when no group
    /// or every group is: then the net goes whole, as any other. Cost: O(the net's items x log)
    /// for the groups and O(contested cells x the net's pieces).
    fn groups_in_way(&self, n: NetId, net: NetId, contested: &[(usize, Cell)]) -> Option<BTreeSet<ItemId>> {
        let p = self.p;
        let items: Vec<&Item> = self.sol.add.iter().filter(|item| copper_net(item) == Some(net)).collect();
        let index: BTreeMap<ItemId, usize> = items.iter().enumerate().map(|(i, item)| (item.id(), i)).collect();
        fn root(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let mut parent: Vec<usize> = (0..items.len()).collect();
        for (a, item) in items.iter().enumerate() {
            let Item::Track(track) = item else { continue };
            for end in track.ends {
                let other = match end {
                    Anchor::Via(id) | Anchor::Track { id, .. } => index.get(&id).copied(),
                    _ => None,
                };
                if let Some(b) = other {
                    let (a, b) = (root(&mut parent, a), root(&mut parent, b));
                    parent[a.max(b)] = a.min(b);
                }
            }
        }
        let class = self.settled.board.nets.get(n.ix()).map(|net| net.class).unwrap_or_default();
        let via = p.rules.class(class).via.unwrap_or_else(|| p.rules.via(class));
        let probes = [
            Probe { class, w: class_track_width(&p.rules, class), drill: None },
            Probe { class, w: via.pad, drill: Some(via.drill) },
        ];
        let rip = |it: &RItem| p.owned(it);
        let src = WorldSource::new(&self.world, &p.rules, &p.board, &rip);
        let spec = &self.grid.spec;
        let reach = probes.iter()
            .map(|probe| i64::from(src.max_need(probe)) + i64::from(probe.w) / 2 + spec.padding(probe))
            .max()
            .unwrap_or(0);
        let mut in_way = vec![false; items.len()];
        for (i, item) in items.iter().enumerate() {
            let group = root(&mut parent, i);
            if in_way[group] {
                continue;
            }
            let pieces = new_pieces(&p.board, item);
            in_way[group] = contested.iter().any(|&(li, cell)| {
                let c = spec.centre(cell);
                pieces.iter().any(|piece| {
                    matches!(&piece.piece, RPiece::Copper(copper) if copper.layer == spec.layers[li])
                        && i64::from(point_clearance(c, piece.shape())) < reach
                })
            });
        }
        let groups: BTreeSet<usize> = (0..items.len()).map(|i| root(&mut parent, i)).collect();
        let hit = groups.iter().filter(|group| in_way[**group]).count();
        if hit == 0 || hit == groups.len() {
            return None;
        }
        Some((0..items.len()).filter(|&i| in_way[root(&mut parent, i)]).map(|i| items[i].id()).collect())
    }

    /// Where ripped `net` comes back. A repair net outside the problem looks for access in the box
    /// of the job copper it lost and of the copper the attempted net `n` laid in this attempt (ids
    /// from `first_id`), where the cut is; a net of the problem as in the first pass, and so does a
    /// routed net a re-route ripped, which lost only the board's copper.
    fn return_focus(&self, net: NetId, n: NetId, taken: &Taken, first_id: u32) -> BBox {
        let Some(lost) = taken.boxes.get(&net).copied().filter(|_| !self.p.nets.iter().any(|r| r.net == net)) else {
            return BBox::EMPTY;
        };
        let board = &self.settled.board;
        self.sol.add.iter()
            .filter(|item| copper_net(item) == Some(n) && item.id().0 >= first_id)
            .fold(lost, |area, item| area.union(item.bbox(board)))
    }

    /// Charge a finished rollback, including escape floods while restoring the guard.
    fn rollback_attempt(&mut self, saved: &Snapshot, area: BBox, started: u64, unkeepable_at: Option<u64>) {
        self.restore_attempt(saved, area);
        let counts = &mut self.sol.metrics.work.by_attempt;
        counts.rolled_back += self.work - started;
        if let Some(mark) = unkeepable_at {
            counts.after_unkeepable += self.work - mark;
        }
    }

    /// Restore by item identity: ripping can shrink or reorder the snapshot's added items. The
    /// guard table follows the restored board.
    fn restore_attempt(&mut self, saved: &Snapshot, mut area: BBox) {
        let ids: BTreeSet<_> = saved.sol.add.iter().map(Item::id).collect();
        for item in &self.sol.add {
            if !ids.contains(&item.id()) {
                area = area.union(item.bbox(&self.settled.board));
            }
        }
        let work = self.sol.metrics.work;
        self.settled = saved.settled.clone();
        self.world = World::with(&self.p.base, saved.overlay.clone());
        self.sol = saved.sol.clone();
        self.sol.metrics.work = work;
        self.next_id = saved.next_id;
        self.invalidate(area);
        self.refresh_guard();
    }

    fn recount(&mut self) {
        self.sol.metrics.length = 0;
        self.sol.metrics.bends = 0;
        self.sol.metrics.vias = 0;
        for item in &self.sol.add {
            match item {
                Item::Track(track) => {
                    self.sol.metrics.length += track.path.length().round() as i64;
                    self.sol.metrics.bends += track.path.segs.len().saturating_sub(1) as u32;
                }
                Item::Via(_) => self.sol.metrics.vias += 1,
                _ => {}
            }
        }
    }

    /// Route, settle and repair before publishing a transaction. A cut repairs cannot close
    /// re-routes the planned net once with keepouts on the cut nets' corridors, found on the
    /// pre-transaction board, then rolls back if a cut remains. Only failed repairs pay for
    /// one corridor search per unreachable pocket plus one more route of the planned net.
    /// `focus` is where the planned net's access is looked for (empty: near its other terminals).
    fn transaction(&mut self, planned: &RNet, later: &[RNet], focus: BBox, progress: &Progress) {
        let p = self.p;
        let (settled, overlay, saved, next_id) = (
            self.settled.clone(),
            self.world.clone().into_overlay(),
            self.sol.clone(),
            self.next_id,
        );
        let mut keepouts: Option<(Overlay, BBox)> = None;
        for attempt in 0..2 {
            let attempt_work = self.work;
            self.stuck.clear();
            let fills = self.settled.fills.clone();
            match self.route_net(planned, &fills, focus) {
                None => self.sol.metrics.nets_routed += 1,
                Some(reason) => {
                    self.sol.open.push((planned.net, reason));
                    self.sol.metrics.nets_open += 1;
                }
            }
            // Corridors are for the cut repairs' pockets: a focused planned route leaves its own.
            self.stuck.clear();
            if let Some((overlay, mut area)) = keepouts.take() {
                // Keepouts guide only the planned route; fills and repairs see real copper.
                self.world = World::with(&p.base, overlay);
                for item in &self.sol.add[saved.add.len()..] {
                    area = area.union(item.bbox(&self.settled.board));
                    for piece in new_pieces(&p.board, item) {
                        self.world.add(piece);
                    }
                }
                self.invalidate(area);
            }
            let ops: Vec<_> = self.sol.add[saved.add.len()..].iter().cloned().map(BoardOp::Add).collect();
            settle(&mut self.settled, &p.rules, &ops);
            self.refresh_guard();
            let mut attempted = BTreeSet::new();
            // A pour this transaction cut is cut along its new copper: repairs look there first.
            let cut_at = self.sol.add[saved.add.len()..]
                .iter()
                .fold(BBox::EMPTY, |area, item| area.union(item.bbox(&self.settled.board)));
            loop {
                let cut = self.cut_nets(later, &settled.conn.unrouted);
                let pending: Vec<_> = cut.iter().copied().filter(|net| !attempted.contains(net)).collect();
                if pending.is_empty() {
                    break;
                }
                let first = self.sol.add.len();
                let fills = self.settled.fills.clone();
                for net in pending {
                    attempted.insert(net);
                    if let Some(repair) = rnet(&self.settled.board, &p.rules, &self.settled.conn, &p.stack, net) {
                        let _ = self.route_net(&repair, &fills, cut_at);
                    }
                }
                let ops: Vec<_> = self.sol.add[first..].iter().cloned().map(BoardOp::Add).collect();
                settle(&mut self.settled, &p.rules, &ops);
                self.refresh_guard();
                if !cut.iter().any(|net| !self.settled.conn.unrouted.contains(net)) {
                    break;
                }
            }
            if let Some(net) = self.cut_nets(later, &settled.conn.unrouted).first().copied() {
                let at = rnet(&self.settled.board, &p.rules, &self.settled.conn, &p.stack, net)
                    .and_then(|net| net.terminals.first().map(|t| t.bbox.center()))
                    .unwrap_or(P::ZERO);
                let spent = self.work - attempt_work;
                self.restore(&settled, &overlay, &saved, next_id);
                self.sol.metrics.work.discarded += spent;
                if attempt == 0 && !self.stuck.is_empty() {
                    if let Some(pieces) = self.corridors() {
                        let overlay = self.world.clone().into_overlay();
                        let mut area = BBox::EMPTY;
                        for piece in pieces {
                            area = area.union(piece.bbox);
                            self.world.add(piece);
                        }
                        self.invalidate(area);
                        keepouts = Some((overlay, area));
                        self.sol.metrics.work.retries += 1;
                        progress.retry_net(planned.net, RetryWhy::PourNeck);
                        continue;
                    }
                }
                // Routing the planned net would cut `net` off its pour.
                progress.block_net(planned.net, net);
                self.sol.open.push((planned.net, Reason::Blocked { at, by: vec![net] }));
                self.sol.metrics.nets_open += 1;
                return;
            }
            // A refill can split a planned net's original terminals too. Claims use settled
            // connectivity even when the tree search joined all of those original terminals.
            if self.settled.conn.unrouted.contains(&planned.net)
                && !self.sol.open.iter().any(|(net, _)| *net == planned.net)
            {
                // Named by its smallest piece left apart: a pad, mostly.
                let apart = rnet(&self.settled.board, &p.rules, &self.settled.conn, &p.stack, planned.net)
                    .and_then(|net| net.terminals.into_iter().min_by_key(|t| t.items.len()));
                let reason = apart.map_or(Reason::Terminal { at: P::ZERO, with: None }, |t| self.terminal_reason(&t));
                self.sol.open.push((planned.net, reason));
                self.sol.metrics.nets_routed -= 1;
                self.sol.metrics.nets_open += 1;
            }
            for net in attempted {
                if !p.nets.iter().any(|n| n.net == net) {
                    if let Err(index) = self.sol.repaired.binary_search(&net) {
                        self.sol.repaired.insert(index, net);
                        self.sol.metrics.nets_routed += 1;
                    }
                }
            }
            return;
        }
    }

    /// Restore the transaction snapshot while retaining all search work spent so far, and bring
    /// the guard table back in line with the restored board.
    fn restore(&mut self, settled: &Settled, overlay: &Overlay, saved: &RouteSolution, next_id: u32) {
        let touched = self.sol.add[saved.add.len()..]
            .iter()
            .fold(BBox::EMPTY, |area, item| area.union(item.bbox(&self.settled.board)));
        let work = self.sol.metrics.work;
        self.settled = settled.clone();
        self.world = World::with(&self.p.base, overlay.clone());
        self.sol = saved.clone();
        self.sol.metrics.work = work;
        self.next_id = next_id;
        self.invalidate(touched);
        self.refresh_guard();
    }

    fn invalidate(&mut self, area: BBox) {
        if !area.is_empty() {
            self.touched.push(area);
            let p = self.p;
            let rip = |it: &RItem| p.owned(it);
            let src = WorldSource::new(&self.world, &p.rules, &p.board, &rip);
            let layers: Vec<_> = (0..self.grid.spec.layers.len()).collect();
            self.grid.invalidate(&src, area, &layers);
            if let Some(grid) = &mut self.what_if {
                let any = |_: &RItem| true;
                grid.invalidate(&WorldSource::new(&self.world, &p.rules, &p.board, &any), area, &layers);
            }
        }
    }

    /// Find every failed repair's corridors on the restored world before adding any keepouts.
    fn corridors(&mut self) -> Option<Vec<RItem>> {
        let p = self.p;
        let rip = |it: &RItem| p.owned(it);
        let src = WorldSource::new(&self.world, &p.rules, &p.board, &rip);
        // Like the outline these derived obstacles have no board item (and verifier diagnostics
        // select the outline). Keep their order unique by ranking them after its edge pieces.
        let first_rank = p.board.outline.iter().map(|c| c.path.steps().count()).sum::<usize>();
        let mut pieces = Vec::new();
        for s in &self.stuck {
            for pocket in &s.pockets {
                let spec = &self.grid.spec;
                let area = pocket.iter().fold(BBox::EMPTY, |b, (_, c)| b.union(BBox::point(spec.centre(*c))));
                let window = spec.cells_in(area.inflate(WINDOW_MARGIN * spec.pitch)).unwrap_or_else(|| spec.all());
                let q = Query {
                    track: s.track,
                    via: s.via,
                    net: s.net,
                    layers: &s.layers,
                    sources: &s.sources,
                    targets: pocket,
                    window,
                    guard: None,
                };
                let before = self.work;
                let path = search_widening(&mut self.grid, &src, &q, &self.costs, &mut self.work);
                self.by_search.widening += self.work - before;
                let path = path?;
                let spec = &self.grid.spec;
                let clearance = p.rules.max_clearance() + spec.slack();
                let w = s.w + 2 * clearance;
                let all = LayerSet::of(&spec.layers);
                let mut push = |area: Shape, layers| {
                    let bbox = area.bbox();
                    let rank = (first_rank + pieces.len()) as u32;
                    pieces.push(RItem {
                        piece: RPiece::Keepout { area, forbid: Forbid::TRACKS.with(Forbid::VIAS) },
                        item: None,
                        rank,
                        layers,
                        bbox,
                        fixity: Fixity::Derived,
                        locked: true,
                        kept: None,
                    });
                };
                for pair in path.cells.windows(2) {
                    let [(li, a), (next, b)] = [pair[0], pair[1]];
                    if li == next {
                        push(
                            Shape::Segment { a: spec.centre(a), b: spec.centre(b), w },
                            LayerSet::EMPTY.with(spec.layers[li]),
                        );
                    } else {
                        push(Shape::Circle { c: spec.centre(a), r: s.via_pad / 2 + clearance }, all);
                    }
                }
                for (i, &(li, cell)) in path.cells.iter().enumerate() {
                    if i == 0 || i + 1 == path.cells.len() {
                        push(Shape::Circle { c: spec.centre(cell), r: w / 2 }, LayerSet::EMPTY.with(spec.layers[li]));
                    }
                }
            }
        }
        Some(pieces)
    }

    /// Nets this transaction disconnected, excluding reported opens and the remaining plan.
    fn cut_nets(&self, later: &[RNet], unrouted: &[NetId]) -> Vec<NetId> {
        self.settled.conn.unrouted
            .iter()
            .copied()
            .filter(|net| {
                !unrouted.contains(net)
                    && !self.sol.open.iter().any(|(open, _)| open == net)
                    && !later.iter().any(|next| next.net == *net)
            })
            .collect()
    }

    /// Grow one net's tree, retaining legal partial copper and returning why it remains open.
    /// Zone terminals look for drop vias from `focus` outward when it is not empty (a repair:
    /// where the cut is), else from the other terminals. Its searches pay the guard; the table
    /// stays as it is while the net grows and is refreshed after the settle.
    fn route_net(&mut self, rnet: &RNet, fills: &[ZoneFill], focus: BBox) -> Option<Reason> {
        let table = std::mem::take(&mut self.guard);
        let guard = (self.costs.guard > 0).then_some(&table);
        let open = self.grow(rnet, fills, focus, guard);
        self.guard = table;
        open
    }

    fn grow(&mut self, rnet: &RNet, fills: &[ZoneFill], focus: BBox, guard: Option<&GuardTable>) -> Option<Reason> {
        let p = self.p;
        let w = width(rnet);
        let track_probe = Probe { class: rnet.class, w, drill: None };
        let via_probe = Probe { class: rnet.class, w: rnet.via.pad, drill: Some(rnet.via.drill) };
        let track = self.grid.plane(track_probe);
        let via = self.grid.plane(via_probe);
        let net = AccessNet { net: rnet.net, class: rnet.class, w };
        let layers: Vec<usize> = self.grid.spec.layers.iter().enumerate()
            .filter(|(_, l)| rnet.layers.has(**l))
            .map(|(i, _)| i)
            .collect();
        let n = rnet.terminals.len();
        let mut acc: Vec<Vec<Access>> = rnet.terminals.iter().enumerate().map(|(i, t)| {
            let near = if focus.is_empty() {
                rnet.terminals.iter().enumerate()
                    .filter(|(j, _)| *j != i)
                    .fold(BBox::EMPTY, |bbox, (_, other)| bbox.union(other.bbox))
            } else {
                focus
            };
            self.terminal_access(t, track, via, &net, rnet.via.pad, near, fills)
        }).collect();
        // A free end of fixed copper is a route somebody started: the tree starts at the terminal
        // with the most ends (ties: the lowest index), and each join tries the ends first. Without
        // ends every join is today's one search. A tree that reaches none of the terminals left
        // (or has no access) keeps what it joined, and the terminals left grow a tree of their own
        // from the next root, so one unreachable terminal never fails the rest of the net: the net
        // stays open with the first reason found.
        let mut ends: Vec<Vec<(usize, Access)>> = rnet.terminals.iter().map(|t| self.end_access(t, track, &net)).collect();
        let next_root = |in_tree: &[bool], with_access: bool, acc: &[Vec<Access>]| (0..n)
            .filter(|&t| !in_tree[t] && (!with_access || !acc[t].is_empty()))
            .max_by_key(|&t| (rnet.terminals[t].ends.len(), std::cmp::Reverse(t)));
        let mut in_tree = vec![false; n];
        let mut root = next_root(&in_tree, false, &acc).unwrap_or(0);
        in_tree[root] = true;
        let mut sources = std::mem::take(&mut acc[root]);
        // The unused ends of terminals in the tree, keyed (terminal, end index).
        let mut pending: Vec<((usize, usize), Access)> = std::mem::take(&mut ends[root]).into_iter().map(|(e, a)| ((root, e), a)).collect();
        let mut added: Vec<Item> = Vec::new();
        let mut open: Option<Reason> = None;
        'join: while in_tree.iter().any(|t| !t) {
            let targets: Vec<(usize, Access)> = (0..n)
                .filter(|&t| !in_tree[t])
                .flat_map(|t| acc[t].iter().map(move |a| (t, *a)))
                .collect();
            if targets.is_empty() {
                // none of the terminals left has a legal access
                let t = (0..n).find(|&t| !in_tree[t]).unwrap_or(0);
                open.get_or_insert_with(|| self.terminal_reason(&rnet.terminals[t]));
                break;
            }
            if sources.is_empty() {
                // the tree's root has no legal access
                open.get_or_insert_with(|| self.terminal_reason(&rnet.terminals[root]));
                root = next_root(&in_tree, true, &acc).expect("a target's terminal has access");
                in_tree[root] = true;
                sources = std::mem::take(&mut acc[root]);
                pending = std::mem::take(&mut ends[root]).into_iter().map(|(e, a)| ((root, e), a)).collect();
                continue;
            }
            // T': per terminal outside the tree its end access if it has ends, else its access.
            let mut prime: Vec<(usize, Option<usize>, Access)> = Vec::new();
            for t in (0..n).filter(|&t| !in_tree[t]) {
                if ends[t].is_empty() {
                    prime.extend(acc[t].iter().map(|a| (t, None, *a)));
                } else {
                    prime.extend(ends[t].iter().map(|(e, a)| (t, Some(*e), *a)));
                }
            }
            let prime_differs = (0..n).any(|t| !in_tree[t] && !ends[t].is_empty());
            // Stage 0: the pending ends to T'; 1: the whole tree to T'; 2: today's query, S -> T.
            let rip = |it: &RItem| p.owned(it);
            let mut found = None;
            for stage in 0..3 {
                if (stage == 0 && pending.is_empty()) || (stage == 1 && !prime_differs) {
                    continue;
                }
                let stage_sources: Vec<Access> = if stage == 0 { pending.iter().map(|(_, a)| *a).collect() } else { sources.clone() };
                let stage_targets: Vec<(usize, Option<usize>, Access)> = if stage < 2 { prime.clone() } else { targets.iter().map(|(t, a)| (*t, None, *a)).collect() };
            let mut src_cells: Vec<(usize, Cell)> = stage_sources.iter().map(|a| (a.li, a.cell)).collect();
            src_cells.sort_unstable();
            src_cells.dedup();
            let mut tgt_cells: Vec<(usize, Cell)> = stage_targets.iter().map(|(_, _, a)| (a.li, a.cell)).collect();
            tgt_cells.sort_unstable();
            tgt_cells.dedup();
            let spec = &self.grid.spec;
            let pts = src_cells.iter().chain(&tgt_cells)
                .fold(BBox::EMPTY, |b, (_, c)| b.union(BBox::point(spec.centre(*c))));
            let window = spec.cells_in(pts.inflate(WINDOW_MARGIN * spec.pitch)).unwrap_or_else(|| spec.all());
            let q = Query {
                track,
                via: (layers.len() > 1).then_some(via),
                net: rnet.net,
                layers: &layers,
                sources: &src_cells,
                targets: &tgt_cells,
                window,
                guard,
            };
            let src = WorldSource::new(&self.world, &p.rules, &p.board, &rip);
            let before = self.work;
            let path = search_widening(&mut self.grid, &src, &q, &self.costs, &mut self.work);
            self.by_search.widening += self.work - before;
            if let Some(path) = path {
                found = Some((path, stage, stage_sources, stage_targets));
                break;
            }
            if stage == 0 {
                // the pending ends are given up for this grow
                pending.clear();
            }
            if stage < 2 {
                continue;
            }
                if !focus.is_empty() {
                    let pockets = (0..n).filter(|&t| !in_tree[t]).filter_map(|t| {
                        let mut cells: Vec<_> = acc[t].iter().map(|a| (a.li, a.cell)).collect();
                        cells.sort_unstable();
                        cells.dedup();
                        (!cells.is_empty()).then_some(cells)
                    }).collect();
                    self.stuck.push(Stuck {
                        net: rnet.net,
                        track: q.track,
                        via: q.via,
                        layers: q.layers.to_vec(),
                        sources: src_cells.clone(),
                        pockets,
                        w,
                        via_pad: rnet.via.pad,
                    });
                }
                if open.is_none() {
                    let (reason, cells) = self.explain(rnet, &q, track_probe, via_probe, &targets);
                    if matches!(reason, Reason::Blocked { .. }) {
                        self.contested.insert(rnet.net, cells);
                    }
                    open = Some(reason);
                }
            }
            let Some((path, stage, stage_sources, stage_targets)) = found else {
                // the tree reaches none of the terminals left: they grow a tree of their own
                root = next_root(&in_tree, true, &acc).expect("a target's terminal has access");
                in_tree[root] = true;
                sources = std::mem::take(&mut acc[root]);
                pending = std::mem::take(&mut ends[root]).into_iter().map(|(e, a)| ((root, e), a)).collect();
                continue 'join;
            };
            let first = path.cells[0];
            let last = *path.cells.last().expect("a path has cells");
            let s = *stage_sources.iter().find(|a| (a.li, a.cell) == first).expect("a path starts at a source");
            let (t, used, e) = *stage_targets.iter().find(|(_, _, a)| (a.li, a.cell) == last).expect("a path ends at a target");
            if stage == 0 {
                let key = pending.iter().find(|(_, a)| (a.li, a.cell) == first).expect("a stage 0 path starts at a pending end").0;
                pending.retain(|(k, _)| *k != key);
            }
            pending.extend(std::mem::take(&mut ends[t]).into_iter().filter(|(ei, _)| Some(*ei) != used).map(|(ei, a)| ((t, ei), a)));
            let start: Vec<P> = std::iter::once(s.point).chain(s.knee).collect();
            let end: Vec<P> = std::iter::once(e.point).chain(e.knee).collect();
            let lifted = lift(&self.grid.spec, &path, &start, &end);
            let items = self.build(rnet, w, &lifted, s, e);
            let pieces: Vec<RItem> = items.iter().flat_map(|it| new_pieces(&p.board, it)).collect();
            let collisions = verify(&self.world, &p.rules, &p.board, &pieces);
            if let Some(c) = collisions.first() {
                // an engine fault: the grid and access promised legal steps
                open = Some(Reason::NoRoom {
                    at: c.at,
                    layer: c.layer,
                    need: Measure::Len(c.need),
                    have: Measure::Len(c.gap),
                    between: [c.a.clone(), c.b.clone()],
                    by: Vec::new(),
                });
                break;
            }
            let mut area = BBox::EMPTY;
            for pc in &pieces {
                area = area.union(pc.bbox);
            }
            let new_copper: Vec<(usize, fab_elec_board::Copper, Anchor)> = pieces
                .iter()
                .filter_map(|pc| match &pc.piece {
                    RPiece::Copper(c) => {
                        let li = self.grid.spec.layers.iter().position(|l| *l == c.layer)?;
                        let anchor = match c.item {
                            // Access sets the segment and fraction from each piece's attach point.
                            ConnItem::Track { item, .. } => Anchor::Track { id: item, at: OnTrack::default() },
                            ConnItem::Via(v) => Anchor::Via(v),
                            _ => return None,
                        };
                        Some((li, c.clone(), anchor))
                    }
                    _ => None,
                })
                .collect();
            for pc in pieces {
                self.world.add(pc);
            }
            let all: Vec<usize> = (0..self.grid.spec.layers.len()).collect();
            let src = WorldSource::new(&self.world, &p.rules, &p.board, &rip);
            self.grid.invalidate(&src, area, &all);
            self.touched.push(area);
            for (li, c, anchor) in &new_copper {
                access(&mut self.grid, &src, track, *li, &net, c, *anchor, &mut sources);
            }
            sources.append(&mut acc[t]);
            // Once a drop is built, later branches use that via rather than drilling it again.
            for entry in sources.iter_mut().chain(acc.iter_mut().flatten()) {
                if entry.drop {
                    if let Some(via) = items.iter().find_map(|item| match item {
                        Item::Via(via) if via.at == entry.point => Some(via),
                        _ => None,
                    }) {
                        entry.anchor = Anchor::Via(via.id);
                        entry.drop = false;
                    }
                }
            }
            sort_access(&self.grid.spec, &mut sources);
            in_tree[t] = true;
            added.extend(items);
        }
        for it in &added {
            match it {
                Item::Track(t) => {
                    self.sol.metrics.length += t.path.length().round() as i64;
                    self.sol.metrics.bends += t.path.segs.len().saturating_sub(1) as u32;
                }
                Item::Via(_) => self.sol.metrics.vias += 1,
                _ => {}
            }
        }
        // a partly connected net keeps what it routed: every connection added is legal
        self.sol.add.extend(added);
        open
    }

    /// Bring the guard table up to date with the settled board and every area the grid
    /// invalidated since the last refresh: find again the escapes of pads that became or
    /// stopped being pour-only, or whose watch box meets a changed area. With the guard off it
    /// only forgets the areas.
    fn refresh_guard(&mut self) {
        if self.costs.guard == 0 {
            self.touched.clear();
            return;
        }
        let pour_only = pour_only_pads(&self.settled.board, &self.settled.conn);
        let stale = self.guard.stale(&pour_only, &self.touched);
        self.touched.clear();
        for key in stale {
            let found = if pour_only.contains(&key) { self.escape_of(key) } else { None };
            self.guard.set(key, found);
        }
    }

    /// The escape of pour-only pad `key` on the current world. Its watch box also covers what
    /// the pad's access cells depend on: access looks up to its zone around the pad, and each
    /// cell and stub sees obstacles a probe's reach and padding further. `None` when the pad's
    /// layer is not in the grid, its net is unknown or it has no copper on that layer.
    fn escape_of(&mut self, key: PadKey) -> Option<Escape> {
        let p = self.p;
        let (placed, pad, layer) = key;
        let board = &self.settled.board;
        let net = board.placed_item(placed)?.pad_nets.get(pad.ix()).copied().flatten()?;
        let li = self.grid.spec.layers.iter().position(|l| *l == layer)?;
        let class = board.nets.get(net.ix()).map(|n| n.class).unwrap_or_default();
        let w = class_track_width(&p.rules, class);
        let via = p.rules.class(class).via.unwrap_or_else(|| p.rules.via(class));
        let probe = Probe { class, w, drill: None };
        let track = self.grid.plane(probe);
        let via_plane = self.grid.plane(Probe { class, w: via.pad, drill: Some(via.drill) });
        let rip = |it: &RItem| p.owned(it);
        let src = WorldSource::new(&self.world, &p.rules, &p.board, &rip);
        let item = ConnItem::Pad { placed, pad };
        let anet = AccessNet { net, class, w };
        let mut out = Vec::new();
        let mut copper = BBox::EMPTY;
        for piece in p.base.pieces(placed).iter().filter_map(|rid| p.base.get(*rid)) {
            let RPiece::Copper(c) = &piece.piece else { continue };
            if c.item != item || c.layer != layer {
                continue;
            }
            copper = copper.union(piece.bbox);
            access(&mut self.grid, &src, track, li, &anet, c, Anchor::Pad { placed, pad }, &mut out);
        }
        if copper.is_empty() {
            return None;
        }
        let mut cells: Vec<Cell> = out.iter().filter(|a| a.li == li).map(|a| a.cell).collect();
        cells.sort_unstable_by_key(|c| (c.y, c.x));
        cells.dedup();
        let input = EscapeIn { net, li, track, via: via_plane, access: cells, w, via_pad: via.pad };
        let clearance = p.rules.max_clearance() + self.grid.spec.slack();
        let before = self.work;
        let mut found = escape(&mut self.grid, &src, &input, clearance, &mut self.work);
        self.by_search.escape += self.work - before;
        let spec = &self.grid.spec;
        let reach = i64::from(src.max_need(&probe)) + spec.padding(&probe);
        let pitch = i64::from(spec.pitch);
        let zone = copper.width().max(copper.height()) + 2 * reach + 4 * pitch;
        // A grow past any board's coordinates covers the whole board; clamp it so it fits `Nm`.
        let grow = Nm::try_from(zone + reach + pitch).unwrap_or(Nm::MAX / 4).min(Nm::MAX / 4);
        found.watch = found.watch.union(copper.inflate(grow));
        Some(found)
    }

    /// The access cells of one terminal, sorted.
    fn terminal_access(
        &mut self,
        t: &Terminal,
        plane: PlaneId,
        via: PlaneId,
        net: &AccessNet,
        pad: Nm,
        near: BBox,
        fills: &[ZoneFill],
    ) -> Vec<Access> {
        let p = self.p;
        let rip = |it: &RItem| p.owned(it);
        let src = WorldSource::new(&self.world, &p.rules, &p.board, &rip);
        let mut out = Vec::new();
        let mut zones: BTreeMap<(ItemId, LayerId), Vec<&PolySet>> = BTreeMap::new();
        for ci in &t.items {
            if let ConnItem::Zone { item, layer, tile, ord } = *ci {
                let polygon = fills.iter().find(|f| f.zone == item)
                    .and_then(|f| f.layer(layer))
                    .and_then(|layer| {
                        let i = layer.tiles.binary_search_by_key(&tile, |tile| tile.key).ok()?;
                        layer.labels[i].kept.get(ord as usize).copied().filter(|kept| *kept)?;
                        layer.tiles[i].polys.get(ord as usize)
                    });
                if let Some(polygon) = polygon {
                    zones.entry((item, layer)).or_default().push(polygon);
                }
                continue;
            }
            let anchor = match *ci {
                ConnItem::Pad { placed, pad } => Anchor::Pad { placed, pad },
                ConnItem::Via(v) => Anchor::Via(v),
                // Access sets the segment and fraction from each piece's attach point.
                ConnItem::Track { item, .. } => Anchor::Track { id: item, at: OnTrack::default() },
                ConnItem::Hole(_) | ConnItem::Zone { .. } => continue,
            };
            let added = self.sol.add.iter().find(|it| it.id() == ci.item()).map(|it| new_pieces(&p.board, it));
            let pieces: Vec<_> = match &added {
                Some(pieces) => pieces.iter().collect(),
                None => p.base.pieces(ci.item()).iter().filter_map(|rid| p.base.get(*rid)).collect(),
            };
            for piece in pieces {
                let RPiece::Copper(c) = &piece.piece else { continue };
                if c.item != *ci {
                    continue;
                }
                let Some(li) = self.grid.spec.layers.iter().position(|l| *l == c.layer) else { continue };
                access(&mut self.grid, &src, plane, li, net, c, anchor, &mut out);
            }
        }
        for polys in zones.values() {
            let li_count = self.grid.spec.layers.len();
            zone_access(&mut self.grid, &src, via, li_count, net, polys, pad, near, &mut out);
        }
        sort_access(&self.grid.spec, &mut out);
        out
    }

    /// Access to each free end of `t`, tagged with the end's index in `t.ends`. An end's copper is
    /// fixed, so it is in the base world.
    fn end_access(&mut self, t: &Terminal, plane: PlaneId, net: &AccessNet) -> Vec<(usize, Access)> {
        let p = self.p;
        let rip = |it: &RItem| p.owned(it);
        let src = WorldSource::new(&self.world, &p.rules, &p.board, &rip);
        let mut out = Vec::new();
        for (i, end) in t.ends.iter().enumerate() {
            let Some(li) = self.grid.spec.layers.iter().position(|l| *l == end.layer) else { continue };
            let segs = p.base.pieces(end.track).iter().filter_map(|rid| p.base.get(*rid)).filter_map(|piece| match &piece.piece {
                RPiece::Copper(c) => match c.item {
                    ConnItem::Track { item, seg } if item == end.track => Some((seg, c)),
                    _ => None,
                },
                _ => None,
            });
            let piece = if end.end == 0 { segs.min_by_key(|(seg, _)| *seg) } else { segs.max_by_key(|(seg, _)| *seg) };
            let Some((seg, c)) = piece else { continue };
            let at = if end.end == 0 { OnTrack::default() } else { OnTrack { seg, frac: OnTrack::ONE } };
            let mut found = Vec::new();
            end_access(&mut self.grid, &src, plane, li, net, c, end.at, Anchor::Track { id: end.track, at }, &mut found);
            out.extend(found.into_iter().map(|a| (i, a)));
        }
        out
    }

    /// Why a terminal cannot be reached: it has no legal access cell.
    fn terminal_reason(&self, t: &Terminal) -> Reason {
        let board = &self.p.board;
        let item = t.items.iter().copied()
            .find(|c| anchor_point(board, *c).is_some())
            .or_else(|| t.items.first().copied());
        let at = item.and_then(|c| anchor_point(board, c)).unwrap_or_else(|| t.bbox.center());
        Reason::Terminal { at, with: item.map(|c| sel_of(board, c)) }
    }

    /// Why no path exists, with the cells where the blockers path paid rip costs, in path order
    /// (via cells and landings included).
    fn explain(
        &mut self,
        rnet: &RNet,
        q: &Query<'_>,
        track_probe: Probe,
        via_probe: Probe,
        targets: &[(usize, Access)],
    ) -> (Reason, Vec<(usize, Cell)>) {
        let p = self.p;
        let spec = self.grid.spec.clone();
        let found = {
            let rip = |it: &RItem| p.owned(it);
            let src = WorldSource::new(&self.world, &p.rules, &p.board, &rip);
            let all = Query { window: spec.all(), ..*q };
            let barred = self.barred(rnet.net);
            let cost = |_: NetId| RIP_CELL;
            let mut owners = |plane: PlaneId, li: usize, a: Cell, b: Cell| {
                let probe = if plane == q.track { track_probe } else { via_probe };
                src.owners_on(&probe, spec.layers[li], spec.centre(a), spec.centre(b), rnet.net)
            };
            let mut r = Rip { cost: &cost, owners: &mut owners };
            let before = self.work;
            let mut found = search_blockers(&mut self.grid, &src, &all, &self.costs, &mut r, &mut self.work);
            self.by_search.blockers += self.work - before;
            // Only an answer that names a barred net is asked again, with the barred nets priced.
            if found.as_ref().is_some_and(|b| b.nets.iter().any(|net| barred.contains(net))) {
                let priced = |net: NetId| if barred.contains(&net) { BARRED } else { RIP_CELL };
                let mut r = Rip { cost: &priced, owners: &mut owners };
                let before = self.work;
                found = search_blockers(&mut self.grid, &src, &all, &self.costs, &mut r, &mut self.work);
                self.by_search.priced_blockers += self.work - before;
            }
            found
        };
        match found {
            Some(b) if !b.nets.is_empty() => {
                let at = spec.centre(b.contested[0].1);
                (Reason::Blocked { at, by: b.nets }, b.contested)
            }
            _ => {
                let board = &p.board;
                let from = rnet.terminals[0].items.first().map(|c| sel_of(board, *c));
                let to = targets.first()
                    .and_then(|(t, _)| rnet.terminals[*t].items.first())
                    .map(|c| sel_of(board, *c));
                let at = targets.first().map_or(rnet.terminals[0].bbox.center(), |(_, a)| a.point);
                let reason = match (from, to) {
                    (Some(a), Some(b)) => {
                        // Where the kept copper in the way is, and which, when only it walls the net in.
                        let (at, layer, by) = self.in_the_way(rnet, q, track_probe, via_probe)
                            .unwrap_or((at, spec.layers[q.layers.first().copied().unwrap_or(0)], Vec::new()));
                        Reason::NoRoom { at, layer, need: Measure::Len(width(rnet)), have: Measure::Len(0), between: [a, b], by }
                    }
                    _ => Reason::Terminal { at, with: None },
                };
                (reason, Vec::new())
            }
        }
    }

    /// The kept copper in the way of `rnet` when no route of it can rip its way through: a
    /// blockers search on the what-if grid, where any net's copper but a pad may be crossed at a
    /// rip's price, and the items at the cells it paid for, in path order, each once, with where
    /// the first is and its layer. `None` when even that finds no way: pads, keepouts or the
    /// board's edge wall the net in. One search over the board, on a grid of its own (the main
    /// grid's tiles know only what this job may rip), made on first use and invalidated with the
    /// main one.
    fn in_the_way(&mut self, rnet: &RNet, q: &Query<'_>, track_probe: Probe, via_probe: Probe) -> Option<(P, LayerId, Vec<Sel>)> {
        let p = self.p;
        let spec = self.grid.spec.clone();
        let any = |_: &RItem| true;
        let src = WorldSource::new(&self.world, &p.rules, &p.board, &any);
        let grid = self.what_if.get_or_insert_with(|| Grid::new(spec.clone()));
        let track = grid.plane(track_probe);
        let via = q.via.map(|_| grid.plane(via_probe));
        let all = Query { track, via, window: spec.all(), guard: None, ..*q };
        let cost = |_: NetId| RIP_CELL;
        let mut owners = |plane: PlaneId, li: usize, a: Cell, b: Cell| {
            let probe = if plane == track { track_probe } else { via_probe };
            src.owners_on(&probe, spec.layers[li], spec.centre(a), spec.centre(b), rnet.net)
        };
        let mut r = Rip { cost: &cost, owners: &mut owners };
        let before = self.work;
        let found = search_blockers(grid, &src, &all, &self.costs, &mut r, &mut self.work);
        self.by_search.what_if += self.work - before;
        let found = found?;
        let &(first_li, first) = found.contested.first()?;
        let mut by: Vec<Sel> = Vec::new();
        for &(li, cell) in &found.contested {
            let (layer, c) = (spec.layers[li], spec.centre(cell));
            let mut items = src.in_way_on(&track_probe, layer, c, c, rnet.net);
            if items.is_empty() {
                items = src.in_way_on(&via_probe, layer, c, c, rnet.net);
            }
            for sel in items.into_iter().map(|item| sel_of(&p.board, item)) {
                if !by.contains(&sel) {
                    by.push(sel);
                }
            }
        }
        Some((spec.centre(first), spec.layers[first_li], by))
    }

    /// The items of a lifted connection: vias first, then the tracks anchored to them and to the
    /// two terminals.
    fn build(&mut self, rnet: &RNet, w: Nm, lifted: &Lifted, s: Access, e: Access) -> Vec<Item> {
        let layers = self.grid.spec.layers.clone();
        let (lo, hi) = (layers[0], *layers.last().expect("a grid has layers"));
        let mut items = Vec::new();
        let mut vias: Vec<(P, ItemId)> = Vec::new();
        let points = lifted.vias.iter().map(|(at, _, _)| *at)
            .chain([s, e].into_iter().filter(|a| a.drop).map(|a| a.point));
        for at in points {
            if vias.iter().any(|(point, _)| *point == at) {
                continue;
            }
            let id = self.alloc();
            vias.push((at, id));
            items.push(Item::Via(Via {
                id,
                net: rnet.net,
                at,
                drill: rnet.via.drill,
                pad: rnet.via.pad,
                span: (lo, hi),
                kind: ViaKind::Through,
                tent: Tent::Both,
                backdrill: None,
                locked: false,
                origin: Origin::Solved { by: solvers::GRID },
                flash: Flash::All,
            }));
        }
        let anchor = |pt: P| {
            if let Some((_, id)) = vias.iter().find(|(v, _)| *v == pt) {
                Anchor::Via(*id)
            } else if pt == s.point {
                s.anchor
            } else if pt == e.point {
                e.anchor
            } else {
                Anchor::Free
            }
        };
        let mut tracks = Vec::new();
        for run in &lifted.runs {
            tracks.push((run.li, run.path.clone(), [anchor(run.path.start), anchor(run.path.end())]));
        }
        for (li, path, ends) in tracks {
            let id = self.alloc();
            items.push(Item::Track(Track {
                id,
                net: rnet.net,
                layer: layers[li],
                w,
                widths: None,
                path,
                ends,
                locked: false,
                origin: Origin::Solved { by: solvers::GRID },
            }));
        }
        items
    }
}

/// Apply only the next round's ops and update the settled fills, index and connectivity.
fn settle(settled: &mut Settled, rules: &RuleMatrix, ops: &[BoardOp]) {
    let board = &mut settled.board;
    let applied = board.apply_all(ops).expect("verified routing operations apply");
    let mut zones: Vec<_> = board.zones.iter().map(|zone| zone.id).collect();
    zones.sort_unstable();
    let mut fills = Vec::with_capacity(zones.len());
    let mut refilled = Vec::new();
    for zone in zones {
        let previous = settled.fills.binary_search_by_key(&zone, |fill| fill.zone)
            .ok().map(|i| &settled.fills[i]);
        let next = match previous {
            Some(previous) => fill_update(board, rules, previous, &applied.delta),
            None => fill(board, rules, zone),
        };
        if previous.map_or(true, |previous| !next.same(previous)) {
            refilled.push(zone);
        }
        fills.push(next);
    }
    for previous in &settled.fills {
        if fills.binary_search_by_key(&previous.zone, |fill| fill.zone).is_err() {
            refilled.push(previous.zone);
        }
    }
    refilled.sort_unstable();
    let change = settled.index.update(board, &fills, &applied.delta, &refilled);
    // The request's connectivity may hold zones over updates the router never saw.
    let unknown = IndexChange::all();
    settled.conn = connect_indexed(&settled.conn, &settled.index, board, &applied.delta, &change, &[], &unknown);
    settled.fills = fills;
}

/// Per net, the length (nm, each track rounded as the metrics count it) and vias of `items`.
fn copper_by_net(items: &[Item]) -> BTreeMap<NetId, (i64, u32)> {
    let mut out: BTreeMap<NetId, (i64, u32)> = BTreeMap::new();
    for item in items {
        match item {
            Item::Track(track) => out.entry(track.net).or_default().0 += track.path.length().round() as i64,
            Item::Via(via) => out.entry(via.net).or_default().1 += 1,
            _ => {}
        }
    }
    out
}

/// The net of a track or via.
fn copper_net(item: &Item) -> Option<NetId> {
    match item {
        Item::Track(track) => Some(track.net),
        Item::Via(via) => Some(via.net),
        _ => None,
    }
}

/// The reroute order of ripped `nets`: the problem's in plan order, then the repaired nets outside
/// it in the order they were ripped.
fn reroute_order(p: &RouteProblem, nets: &[NetId]) -> Vec<NetId> {
    let planned = p.nets.iter().map(|net| net.net).filter(|net| nets.contains(net));
    planned.chain(nets.iter().copied().filter(|net| !p.nets.iter().any(|r| r.net == *net))).collect()
}

/// The track width a net routes with: its widest per-layer width, so one plane serves all layers.
fn width(n: &RNet) -> Nm {
    n.w.iter().copied().max().unwrap_or(0)
}

/// The documented access order: squared distance from the attach point to the cell centre,
/// layer, row, column (a stable sort: equal keys keep their deterministic input order).
fn sort_access(spec: &GridSpec, v: &mut Vec<Access>) {
    v.sort_by_key(|a| {
        let d = spec.centre(a.cell).sub(a.point);
        (i64::from(d.x) * i64::from(d.x) + i64::from(d.y) * i64::from(d.y), a.li, a.cell.y, a.cell.x)
    });
    v.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{problem, scope_nets, Base, Effort, RBase, RouteScope, Style};
    use fab_elec_board::{connect, fill_all, resolve_rules};
    use fab_elec_core::PadIx;
    use std::sync::Arc;

    /// A table rebuilt from scratch on `r`'s state; `r` keeps its own table, areas and work.
    fn rebuilt(r: &mut Router<'_>) -> GuardTable {
        let touched = std::mem::take(&mut r.touched);
        let kept = std::mem::take(&mut r.guard);
        let work = r.work;
        let by_search = r.by_search;
        r.refresh_guard();
        let rebuilt = std::mem::replace(&mut r.guard, kept);
        r.touched = touched;
        r.work = work;
        r.by_search = by_search;
        rebuilt
    }

    #[test]
    fn pour_neck_guard_matches_rebuild_after_transactions() {
        let (board, gnd, signal, _) = fab_elec_fixtures::pour_neck();
        let pad = board.placed.iter().find(|placed| placed.refdes == "P")
            .expect("the pour-neck fixture has pad P").id;
        let key = (pad, PadIx(0), LayerId::F_CU);
        let board = Arc::new(board);
        let rules = Arc::new(resolve_rules(&board, None));
        let fills = Arc::new(fill_all(&board, &rules));
        let conn = Arc::new(connect(&board, &fills));
        let base = Base::new(board, rules, conn, fills);
        assert!(!base.conn.unrouted.contains(&gnd),
            "P's exit is wider than the pour's minimum width, so P reaches G through the pour");
        let nets = scope_nets(&base.board, &base.conn, &RouteScope::Nets(vec![signal]));
        let world = Arc::new(RBase::build(&base.board));
        let p = problem(&base, world, &nets, Style::FortyFive, Effort::default());
        assert_eq!(p.nets.len(), 1, "the fixture plans exactly the signal transaction");
        assert!(pour_only_pads(&p.board, &p.conn).contains(&key),
            "pad P starts connected only through the pour");

        let costs = Costs { prefs: p.stack.prefs.clone(), ..Costs::default() };
        let mut r = Router::new(&p, costs).expect("the pour-neck fixture is routable");
        r.refresh_guard();
        assert!(r.guard.len() > 0, "P's initial guard table must not be empty");

        // A keepout on P's escape target changes that escape; taking it away brings it back.
        let initial = r.guard.clone();
        let target = r.escape_of(key).and_then(|found| found.target).expect("P starts with an escape");
        let overlay = r.world.clone().into_overlay();
        let area = Shape::Circle { c: r.grid.spec.centre(target), r: r.grid.spec.pitch };
        let bbox = area.bbox();
        r.world.add(RItem {
            piece: RPiece::Keepout { area, forbid: Forbid::TRACKS.with(Forbid::VIAS) },
            item: None,
            rank: p.board.outline.iter().map(|c| c.path.steps().count()).sum::<usize>() as u32,
            layers: LayerSet::of(&r.grid.spec.layers),
            bbox,
            fixity: Fixity::Derived,
            locked: true,
            kept: None,
        });
        r.invalidate(bbox);
        r.refresh_guard();
        assert_ne!(r.guard, initial, "a keepout on P's escape target moves its escape");
        let fresh = rebuilt(&mut r);
        assert_eq!(r.guard, fresh, "a refresh after the keepout equals a rebuild");
        r.world = World::with(&p.base, overlay);
        r.invalidate(bbox);
        r.refresh_guard();
        assert_eq!(r.guard, initial, "without the keepout P's escape is the initial one again");

        for (step, planned) in p.nets.iter().enumerate() {
            let before = r.guard.clone();
            let snapshot = (r.settled.clone(), r.world.clone().into_overlay(), r.sol.clone(), r.next_id);
            r.transaction(planned, &p.nets[step + 1..], BBox::EMPTY, &Progress::new());
            assert!(r.sol.open.is_empty() && r.settled.conn.unrouted.is_empty(),
                "step {step}: the guarded transaction routes {:?} and keeps P on its pour; opens={:?}, unrouted={:?}",
                planned.net, r.sol.open, r.settled.conn.unrouted);
            assert_eq!((r.sol.metrics.work.retries, r.sol.metrics.work.discarded), (0, 0),
                "step {step}: the guard steers the first route, so nothing is retried or discarded");
            assert!(pour_only_pads(&r.settled.board, &r.settled.conn).contains(&key),
                "step {step}: P remains pour-only after the transaction");
            assert!(r.touched.is_empty(), "step {step}: a transaction ends with every changed area consumed");
            let kept = r.guard.clone();
            assert_eq!(kept, rebuilt(&mut r), "step {step}: after a settle the kept table equals a rebuild");

            // A rollback: the restore a retry or a block uses brings back the table it started with.
            let (settled, overlay, saved, next_id) = snapshot;
            r.restore(&settled, &overlay, &saved, next_id);
            assert!(r.touched.is_empty(), "step {step}: a restore consumes the areas it changed");
            assert_eq!(r.guard, before, "step {step}: after a restore the table equals the one before the transaction");
            let fresh = rebuilt(&mut r);
            assert_eq!(r.guard, fresh, "step {step}: after a restore the kept table equals a rebuild");
            r.transaction(planned, &p.nets[step + 1..], BBox::EMPTY, &Progress::new());
            assert_eq!(r.guard, kept, "step {step}: routing again after the rollback keeps the same table");
        }
    }
}
