//! Fab's side of the finish passes (L6, not a port): the judge, what a pass asks where the
//! reference runs `Drc::run` and compares the reports (`opt::Guard`).
//!
//! The judge holds the last accepted state of the job's board: the board, its fills, piece
//! index, connectivity and findings, and per lifted net its form (items) and the Fab id of each
//! of its traces and fresh vias. A re-lift keeps trace ids by port id and net, and fresh via ids
//! by their order within the net. Its first state is the grid form, checked from the job's
//! `Basis`. A call lifts the asked nets' copper from the relaxer's board (the lift's `points`
//! form: the pre stage runs before the relax), replaces those nets' items with it and checks
//! the change with the worker's check (`crate::check`): one check, one definition. The change is
//! not worse when it brings
//! (a) no finding the accepted findings lack or have at a larger gap (`check::new_or_worse`, as
//! the worker counts `new_violations`), (b) no `CheckKind` with more findings and (c) no net with
//! more missing connections (per net, as the final connectivity check counts). Accepting keeps
//! the new state; refusing undoes the board (`Board::undo`) and drops the rest.
//!
//! Each change of the accepted state (an accepted call, a `sync`) is one unit of the job's step
//! count, so a partial stays a function of (problem, step): the accepted solution is offered at
//! every accepted unit and each pass end (`offer`) when the consumer's cadence says one is due,
//! and built only then. Until the first unit the accepted solution is the grid solution itself.
//! Once the job is cancelled every call refuses unchecked, so the pass restores its own copy and
//! ends, and the job returns the last accepted state. A `sync` whose copper does not lift or apply
//! keeps the accepted state (legal, as every accepted state):
//! the pass's board and the judge then differ on those nets until a later call lifts them, and
//! the count says so (`sync_failed`).
//!
//! Cost per call: the lift of the asked nets (and one pass over the relaxer's board for their
//! copper), the board apply and the check's incremental steps for their change (see `check`, which
//! matches the findings against the accepted ones) and one pass over the findings and ratsnest
//! lines for the report (O(findings + lines)). Each judged unit publishes status for its first
//! net, and each accepted unit checks the cadence (`offer`).
//! Per offer: the accepted solution's assembly (O(items)).

use super::adapt::{from_ref, nm, to_ref};
use super::geom::Vec2;
use super::job::{Fresh, Lift};
use super::model::OBoard;
use super::opt::{Finding, Guard};
use crate::check::{check, ratsnest_lines, Basis, Outcome, State};
use crate::problem::RouteProblem;
use crate::progress::Progress;
use crate::solution::RouteSolution;
use fab_elec_board::{Board, BoardOp, ConnItem, Connectivity, IndexChange, Item, ItemKind, PieceIndex, ZoneFill};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::dist::clearance;
use fab_elec_core::{ItemId, NetId, Sel, Shape};
use std::collections::BTreeMap;
use std::ops::ControlFlow;

/// What the judge did, for the job's report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JudgeStats {
    /// `judge` calls: accepted + refused.
    pub calls: u32,
    pub accepted: u32,
    pub refused: u32,
    /// Refused because a net did not lift (no Fab form) or the ops did not apply.
    pub unlifted: u32,
    /// Refused unchecked because the job was cancelled.
    pub cancelled: u32,
    /// `sync` calls, and those whose copper did not lift or apply (the state then stays).
    pub synced: u32,
    pub sync_failed: u32,
}

/// The judge's accepted state at the end of the pre stage, for the rebase.
pub(super) struct Accepted {
    /// The accepted state's changes (accepted calls and syncs); 0: `solution` is the grid's.
    pub units: u32,
    /// The accepted solution: every lifted net in its accepted form, the others in grid form.
    pub solution: RouteSolution,
    /// The problem's board with the items of `solution` (in the judge's apply order).
    pub board: Board,
    /// Its missing connections per net.
    pub lines: BTreeMap<NetId, usize>,
    /// Each trace of a lifted net: port id, Fab id and net.
    pub ports: Vec<(i32, ItemId, i32)>,
    /// Each lifted net's fresh via ids, in port via order, for the rebase's via table.
    pub vias: BTreeMap<i32, Vec<ItemId>>,
    /// The next fresh Fab item id.
    pub next: u32,
    /// The job's step after the last accepted unit.
    pub step: u32,
    pub stats: JudgeStats,
}

/// A lifted change: the board ops, and per net its new form, trace ids and fresh via ids.
struct Lifted {
    ops: Vec<BoardOp>,
    nets: BTreeMap<i32, (Vec<Item>, Vec<(i32, ItemId, i32)>)>,
    vias: BTreeMap<i32, Vec<ItemId>>,
}

/// The judge of one job (see the module notes).
pub(super) struct Judge<'a> {
    lift: &'a Lift<'a>,
    problem: &'a RouteProblem,
    grid: &'a RouteSolution,
    progress: &'a Progress,
    board: Board,
    fills: Vec<ZoneFill>,
    index: PieceIndex,
    conn: Connectivity,
    violations: Vec<Violation>,
    kinds: BTreeMap<CheckKind, usize>,
    lines: BTreeMap<NetId, usize>,
    /// Each net's items on `board` (port net index): the grid's, or the last accepted lift's.
    held: BTreeMap<i32, Vec<ItemId>>,
    /// Each lifted net's accepted form, trace ids and fresh via ids in port order.
    forms: BTreeMap<i32, Vec<Item>>,
    ports: BTreeMap<i32, Vec<(i32, ItemId, i32)>>,
    vias: BTreeMap<i32, Vec<ItemId>>,
    /// The next fresh item id for copper with no grid or accepted id.
    next: u32,
    /// Changes of the accepted state, the job's step (one per change) and the change count at
    /// the last offer.
    units: u32,
    step: u32,
    offered: u32,
    refusal: Vec<Finding>,
    stats: JudgeStats,
}

impl<'a> Judge<'a> {
    /// The judge with the grid solution `grid` of `problem` as its accepted state, checked from
    /// `basis`, offering to `progress` from step `step` on; `None` when the grid solution does
    /// not apply.
    pub fn new(problem: &'a RouteProblem, basis: &Basis, grid: &'a RouteSolution, lift: &'a Lift<'a>, progress: &'a Progress, step: u32) -> Option<Judge<'a>> {
        let mut board = (*problem.board).clone();
        let all = IndexChange::all();
        let out = check(&mut board, &grid.ops(), &basis.state(problem, &all)).ok()?;
        let mut held: BTreeMap<i32, Vec<ItemId>> = BTreeMap::new();
        for it in &grid.add {
            let net = match it {
                Item::Track(t) => t.net,
                Item::Via(v) => v.net,
                _ => continue,
            };
            held.entry(net.0 as i32).or_default().push(it.id());
        }
        let next = board.next_item;
        Some(Judge {
            lift,
            problem,
            grid,
            progress,
            board,
            kinds: kinds(&out.violations),
            lines: ratsnest_lines(&out.conn),
            fills: out.fills,
            index: out.index,
            conn: out.conn,
            violations: out.violations,
            held,
            forms: BTreeMap::new(),
            ports: BTreeMap::new(),
            vias: BTreeMap::new(),
            next,
            units: 0,
            step,
            offered: 0,
            refusal: Vec::new(),
            stats: JudgeStats::default(),
        })
    }

    /// Build and offer the accepted solution at the current step when due, unless it was offered
    /// already (a pass end).
    pub fn offer(&mut self) {
        if self.units > self.offered && self.progress.due(self.step) {
            self.offered = self.units;
            self.progress.offer(self.step, &self.lift.accepted(&self.forms));
        }
    }

    /// The accepted state, for the rebase.
    pub fn finish(self) -> Accepted {
        Accepted {
            units: self.units,
            solution: if self.units == 0 { self.grid.clone() } else { self.lift.accepted(&self.forms) },
            board: self.board,
            lines: self.lines,
            ports: self.ports.into_values().flatten().collect(),
            vias: self.vias,
            next: self.next,
            step: self.step,
            stats: self.stats,
        }
    }

    /// Keep `out` and the lifted forms as the accepted state: one more unit.
    fn take(&mut self, out: Outcome, lifted: Lifted) {
        self.units += 1;
        self.step += 1;
        for (n, (items, ports)) in lifted.nets {
            self.held.insert(n, items.iter().map(Item::id).collect());
            self.forms.insert(n, items);
            self.ports.insert(n, ports);
        }
        self.vias.extend(lifted.vias);
        self.kinds = kinds(&out.violations);
        self.lines = ratsnest_lines(&out.conn);
        self.fills = out.fills;
        self.index = out.index;
        self.conn = out.conn;
        self.violations = out.violations;
    }

    /// The ops that replace the items of `nets` with their copper in `ob`, and each net's new
    /// form, keeping accepted trace ids by port id and net and fresh via ids by port order;
    /// `None` when one does not lift.
    fn lifted(&mut self, ob: &OBoard, nets: &[i32]) -> Option<Lifted> {
        let mut nets = nets.to_vec();
        nets.sort_unstable();
        nets.dedup();
        let copper = self.lift.copper(ob);
        let mut removes = Vec::new();
        let mut adds = Vec::new();
        let mut out = BTreeMap::new();
        let mut vias = BTreeMap::new();
        for n in nets {
            removes.extend(self.held.get(&n).into_iter().flatten().map(|id| BoardOp::Remove(*id)));
            let (mut items, mut ports) = (Vec::new(), Vec::new());
            let mut via_ids = Vec::new();
            if let Some(c) = copper.get(&n) {
                // Keep the accepted ids; allocate only for copper without one, traces first.
                let mut fresh = Fresh::default();
                let accepted: BTreeMap<i32, ItemId> = self.ports.get(&n).into_iter().flatten()
                    .filter(|(_, _, net)| *net == n).map(|&(port, id, _)| (port, id)).collect();
                for id in self.lift.fresh_of(ob, &c.1) {
                    let fab = match accepted.get(&id) {
                        Some(&fab) => fab,
                        None => self.fresh_id()?,
                    };
                    fresh.traces.insert(id, fab);
                }
                for (k, vi) in self.lift.fresh_vias(ob, n, &c.0).into_iter().enumerate() {
                    let id = match self.vias.get(&n).and_then(|ids| ids.get(k)) {
                        Some(&id) => id,
                        None => self.fresh_id()?,
                    };
                    fresh.vias.insert(vi, id);
                    via_ids.push(id);
                }
                items = self.lift.form(ob, n, c, &fresh, false)?;
                ports = self.lift.ports(ob, c, &fresh.traces);
                adds.extend(items.iter().cloned().map(BoardOp::Add));
            }
            out.insert(n, (items, ports));
            vias.insert(n, via_ids);
        }
        removes.extend(adds);
        Some(Lifted { ops: removes, nets: out, vias })
    }

    /// The next fresh item id. The id space ends below u32::MAX, as `Lift::fresh_ids`: past it
    /// there is none, and the net that asks does not lift.
    fn fresh_id(&mut self) -> Option<ItemId> {
        let f = (self.next < u32::MAX).then(|| ItemId(self.next));
        self.next = self.next.saturating_add(1);
        f
    }

    /// Check the change to `nets` in `ob`; the outcome when it applied.
    fn change(&mut self, ob: &OBoard, nets: &[i32]) -> Option<(Outcome, Lifted)> {
        let lifted = self.lifted(ob, nets)?;
        let since = IndexChange::default();
        let state = State {
            rules: &self.problem.rules,
            fills: &self.fills,
            index: &self.index,
            conn: &self.conn,
            violations: &self.violations,
            since: &since,
        };
        let out = check(&mut self.board, &lifted.ops, &state).ok()?;
        Some((out, lifted))
    }

    /// Why `out` is worse than the accepted state (empty when it is not).
    fn worse(&self, out: &Outcome) -> Vec<Finding> {
        let mut found = Vec::new();
        for v in out.new.iter().map(|&i| &out.violations[i]) {
            found.push(Finding { pos: v.at.map(to_ref), nets: self.nets_of(v) });
        }
        if found.is_empty() {
            // (a) matches a repeated item key once; (b) counts the repeats
            let k = kinds(&out.violations);
            if k.iter().any(|(kind, n)| *n > self.kinds.get(kind).copied().unwrap_or(0)) {
                found.push(Finding::default());
            }
        }
        for (net, n) in ratsnest_lines(&out.conn) {
            if n > self.lines.get(&net).copied().unwrap_or(0) {
                found.push(Finding { pos: None, nets: vec![net.0 as i32] });
            }
        }
        found
    }

    /// The nets (port index) of the board items a finding names.
    fn nets_of(&self, v: &Violation) -> Vec<i32> {
        let mut nets = Vec::new();
        for s in &v.items {
            let net = match s {
                Sel::Item(_, id) => match self.board.locate(*id) {
                    Some((ItemKind::Track, i)) => Some(self.board.tracks[i].net),
                    Some((ItemKind::Via, i)) => Some(self.board.vias[i].net),
                    _ => None,
                },
                Sel::Net(r) => Some(r.net),
                _ => None,
            };
            if let Some(n) = net {
                nets.push(n.0 as i32);
            }
        }
        nets.sort_unstable();
        nets.dedup();
        nets
    }
}

impl Guard for Judge<'_> {
    fn judge(&mut self, ob: &OBoard, nets: &[i32]) -> bool {
        self.stats.calls += 1;
        self.refusal.clear();
        if self.progress.cancelled() {
            self.stats.refused += 1;
            self.stats.cancelled += 1;
            return false;
        }
        if let Some(&n) = nets.first() {
            if n >= 0 {
                self.progress.begin_net(NetId(n as u32));
            }
        }
        let Some((out, lifted)) = self.change(ob, nets) else {
            self.stats.refused += 1;
            self.stats.unlifted += 1;
            self.refusal = nets.iter().map(|&n| Finding { pos: None, nets: vec![n] }).collect();
            return false;
        };
        let worse = self.worse(&out);
        if worse.is_empty() {
            self.stats.accepted += 1;
            self.take(out, lifted);
            self.offer();
            true
        } else {
            self.stats.refused += 1;
            self.board.undo(&out.applied.inverse).expect("judge: the inverse of an applied change applies");
            self.refusal = worse;
            false
        }
    }

    fn sync(&mut self, ob: &OBoard, nets: &[i32]) {
        self.stats.synced += 1;
        self.refusal.clear();
        match self.change(ob, nets) {
            Some((out, lifted)) => self.take(out, lifted),
            None => self.stats.sync_failed += 1,
        }
    }

    fn refusal(&self) -> &[Finding] {
        &self.refusal
    }

    fn tap_connected(&self, net: i32, layer: i32, pos: Vec2, radius: f32) -> bool {
        let Some(&lid) = usize::try_from(layer).ok().and_then(|l| self.lift.layers.get(l)) else { return false };
        let disc = Shape::Circle { c: from_ref(pos), r: nm(radius) };
        let want = NetId(net as u32);
        let mut hit = false;
        self.index.query(lid, disc.bbox(), |piece| {
            if let Some(c) = piece.copper() {
                if matches!(c.item, ConnItem::Zone { .. }) && c.net == Some(want) && clearance(&c.shape, &disc) == 0 {
                    hit = true;
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        });
        hit
    }
}

fn kinds(violations: &[Violation]) -> BTreeMap<CheckKind, usize> {
    let mut out = BTreeMap::new();
    for v in violations {
        *out.entry(v.kind.clone()).or_insert(0) += 1;
    }
    out
}
