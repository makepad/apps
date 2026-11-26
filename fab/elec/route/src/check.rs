//! The check of a change against a checked board state, S29.R74's gate: apply the ops, refill the
//! pours they reach, then bring the piece index, the connectivity and the findings up to date
//! incrementally. The worker checks every proposal with it (against its unit's committed base,
//! which a job receives as its `Basis`), and the organic job's connectivity check settles its
//! solution from that basis with the same steps (`settle`: everything but the findings). One
//! check, one definition, and one for which findings a change adds or makes worse
//! (`new_or_worse`).
//!
//! Each step is the board crate's incremental form, documented equal to its full form:
//! `fill_update_indexed` to a full fill, `PieceIndex::update` to a fresh build, `connect_indexed`
//! to `connect_held`, `drc_indexed` to the full `drc`. The fill reads only the fixed pieces of
//! its index, so the index is brought to the changed board first (its zone pieces still the old
//! fills') and the refilled zones are indexed after.
//!
//! Cost: the board apply (O(ops)), one index clone (persistent: only touched paths copy), the
//! index update for the changed items (O(k log n)), the refill of the tiles the delta meets for
//! each zone it reaches, the refilled zones' tiles, `connect_indexed` (geometry for the fresh
//! pieces only, plus O(n log n) passes over the previous lists) and `drc_indexed` (geometry
//! around the delta, connectivity findings in full). No step builds a full piece list.
//! Measured (native, a Linux node, the grid engine): one net inside the pour checks in
//! 34 ms on ledctl (100 items) and 760 ms on ledctl4x4 (2236 items, `fill_all` 857 ms there); the
//! zone refill is nearly all of it, as the refill reaches most of the pour.

use crate::problem::RouteProblem;
use fab_elec_board::drc::{drc, drc_indexed, sel_of};
use fab_elec_board::{
    connect_indexed, fill, fill_update_indexed, Applied, Board, BoardDelta, BoardOp, ConnItem, Connectivity, IndexChange, OpError,
    PieceIndex, RuleMatrix, ZoneFill,
};
use fab_elec_circuit::{CheckKind, Measure, Violation};
use fab_elec_core::{ItemId, NetId, Nm, Sel};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A checked board state: the fills, piece index, connectivity and findings of one board.
pub struct State<'a> {
    pub rules: &'a RuleMatrix,
    pub fills: &'a [ZoneFill],
    /// A piece index of the board and `fills`.
    pub index: &'a PieceIndex,
    pub conn: &'a Connectivity,
    pub violations: &'a [Violation],
    /// What the index re-derived since the zones `conn` holds were current: `IndexChange::all()`
    /// when unknown (a committed base may hold zones over updates the caller never saw), the
    /// default when `conn` was computed over this index (nothing since).
    pub since: &'a IndexChange,
}

/// The checked state of a job's problem board that the problem does not carry: the unit's piece
/// index and findings (the worker's, for its committed base).
#[derive(Clone)]
pub struct Basis {
    pub index: PieceIndex,
    pub violations: Arc<Vec<Violation>>,
}

impl Basis {
    /// The basis of `problem` built in full (for a caller without a worker unit).
    pub fn of(problem: &RouteProblem) -> Basis {
        let index = PieceIndex::build(&problem.board, &problem.fills);
        let violations = drc(&problem.board, &problem.conn, &problem.rules, &problem.fills, None).violations;
        Basis { index, violations: Arc::new(violations) }
    }

    /// The state of the problem's board. Its connectivity may hold zones over updates the job
    /// never saw, so a check from here passes `IndexChange::all()` as `since`.
    pub fn state<'a>(&'a self, problem: &'a RouteProblem, since: &'a IndexChange) -> State<'a> {
        State { rules: &problem.rules, fills: &problem.fills, index: &self.index, conn: &problem.conn, violations: &self.violations, since }
    }
}

/// A settled change: the board after it, its fills, piece index and connectivity brought up
/// to date (no findings: what a connectivity question needs).
pub struct Settled {
    pub applied: Applied,
    pub fills: Vec<ZoneFill>,
    pub index: PieceIndex,
    /// What the index update re-derived (both of its passes).
    pub change: IndexChange,
    pub conn: Connectivity,
}

/// A checked change: the board after it and everything `State` holds, brought up to date.
pub struct Outcome {
    pub applied: Applied,
    pub fills: Vec<ZoneFill>,
    pub index: PieceIndex,
    /// What the index update re-derived (both of its passes).
    pub change: IndexChange,
    pub conn: Connectivity,
    pub violations: Vec<Violation>,
    /// The indices in `violations` of the findings new or worse than the state's
    /// (`new_or_worse`), in order.
    pub new: Vec<usize>,
}

/// Apply `ops` to `board` (the board of `state`) and check the result. On an error nothing was
/// applied (`Board::apply_all` is all or nothing) and `board` is unchanged.
pub fn check(board: &mut Board, ops: &[BoardOp], state: &State) -> Result<Outcome, OpError> {
    let was = FindingNets::of(board, state.conn);
    let Settled { applied, fills, index, change, conn } = settle(board, ops, state)?;
    let violations = drc_indexed(state.violations, &index, board, &conn, state.rules, &applied.delta, &[]);
    let new = new_or_worse(state.violations, &was, &violations, &FindingNets::of(board, &conn));
    Ok(Outcome { applied, fills, index, change, conn, violations, new })
}

/// The nets of a board's connectivity findings, by the kind and the items `drc` names them with:
/// a short's cluster nets, sorted (`Connectivity::shorts`), a missing connection's net (`ratsnest`). Their
/// items are witnesses, a short's touching pair (`Short::witness`) and a line's nearest anchors, which
/// change with copper of the cluster or the net, so `new_or_worse` matches these findings by their
/// nets. O((shorts + lines) log) to build.
#[derive(Default)]
pub struct FindingNets(BTreeMap<(CheckKind, Vec<Sel>), Vec<NetId>>);

impl FindingNets {
    /// The nets of the connectivity findings of `board` with `conn`.
    pub fn of(board: &Board, conn: &Connectivity) -> FindingNets {
        let named = |kind: CheckKind, a: ConnItem, b: ConnItem| {
            let mut items = vec![sel_of(board, a), sel_of(board, b)];
            items.sort();
            (kind, items)
        };
        let mut nets = BTreeMap::new();
        for s in &conn.shorts {
            if let Some(w) = s.witness {
                nets.insert(named(CheckKind::Short, w.items.0, w.items.1), s.nets.clone());
            }
        }
        for r in &conn.ratsnest {
            nets.insert(named(CheckKind::Unconnected, r.a, r.b), vec![r.net]);
        }
        FindingNets(nets)
    }

    fn of_finding(&self, v: &Violation) -> Option<&[NetId]> {
        if !matches!(v.kind, CheckKind::Short | CheckKind::Unconnected) {
            return None;
        }
        self.0.get(&(v.kind.clone(), v.items.clone())).map(Vec::as_slice)
    }
}

/// The indices of the findings of `now` that are new or worse against `base`, what a change adds
/// to a board's findings or makes worse; `base_nets` and `now_nets` are the nets of each side's
/// connectivity findings (`FindingNets::of` its board and connectivity). Not by `Violation::id`:
/// that is a content id, which hashes each item's geometry, so a refilled pour or a track laid
/// again would make every finding of it look new, and none could read as worse.
/// - A short is new unless a short of the base (same rule) holds all of its nets: one that joins
///   nets the base kept apart is new, one that lost nets or kept them is not.
/// - A missing connection matches by its rule and its net, and is counted: the findings of a net
///   beyond the base's count of it are new (the later ones in `now`).
/// - Any other finding matches by its kind, its rule and the items it names (in any order). Among
///   the base's findings of one key the smallest gap counts, and a finding is worse below it; one
///   without a gap (`actual` not a length) matches any finding of its key. A key that repeats is
///   matched, not counted: the judge's per-kind count tells repeats apart.
///
/// A connectivity finding whose nets a side does not know matches by its items.
/// O((base + now) log base) and one small allocation per finding, plus O(net count) per pair of a
/// short of `now` and a short of the base.
pub fn new_or_worse(base: &[Violation], base_nets: &FindingNets, now: &[Violation], now_nets: &FindingNets) -> Vec<usize> {
    fn items(v: &Violation) -> (&CheckKind, &str, Vec<&Sel>) {
        let mut items: Vec<&Sel> = v.items.iter().collect();
        items.sort_unstable();
        (&v.kind, &v.rule, items)
    }
    fn gap(v: &Violation) -> Option<Nm> {
        match v.actual {
            Some(Measure::Len(gap)) => Some(gap),
            _ => None,
        }
    }
    // both sorted
    fn within(nets: &[NetId], of: &[NetId]) -> bool {
        let mut of = of.iter();
        nets.iter().all(|n| of.by_ref().any(|o| o == n))
    }
    let mut shorts: Vec<(&str, &[NetId])> = Vec::new();
    let mut counted: BTreeMap<(&str, &[NetId]), usize> = BTreeMap::new();
    let mut was: BTreeMap<_, Option<Nm>> = BTreeMap::new();
    for v in base {
        if let Some(nets) = base_nets.of_finding(v) {
            match v.kind {
                CheckKind::Short => shorts.push((&v.rule, nets)),
                _ => *counted.entry((&v.rule, nets)).or_default() += 1,
            }
            continue;
        }
        let at = was.entry(items(v)).or_insert(gap(v));
        if let (Some(a), Some(b)) = (*at, gap(v)) {
            *at = Some(a.min(b));
        }
    }
    let mut new = Vec::new();
    for (i, v) in now.iter().enumerate() {
        let added = match now_nets.of_finding(v) {
            Some(nets) if v.kind == CheckKind::Short => !shorts.iter().any(|&(rule, of)| rule == v.rule && within(nets, of)),
            Some(nets) => match counted.get_mut(&(v.rule.as_str(), nets)) {
                Some(left) if *left > 0 => {
                    *left -= 1;
                    false
                }
                _ => true,
            },
            None => match was.get(&items(v)) {
                None => true,
                Some(&at) => matches!((gap(v), at), (Some(g), Some(at)) if g < at),
            },
        };
        if added {
            new.push(i);
        }
    }
    new
}

/// `check` without the findings: apply `ops`, refill, update the index and the connectivity
/// (`state.violations` is not read).
pub fn settle(board: &mut Board, ops: &[BoardOp], state: &State) -> Result<Settled, OpError> {
    let applied = board.apply_all(ops)?;
    let delta = &applied.delta;
    let mut index = state.index.clone();
    // the changed items first: the fill below reads this index's fixed pieces
    let mut change = index.update(board, state.fills, delta, &[]);
    let fills: Vec<ZoneFill> = board
        .zones
        .iter()
        .map(|zone| match state.fills.iter().find(|f| f.zone == zone.id) {
            Some(previous) => fill_update_indexed(board, state.rules, previous, delta, &index),
            None => fill(board, state.rules, zone.id),
        })
        .collect();
    let refilled = refilled(state.fills, &fills);
    if !refilled.is_empty() {
        change.absorb(&index.update(board, &fills, &BoardDelta::default(), &refilled));
    }
    let conn = connect_indexed(state.conn, &index, board, delta, &change, &[], state.since);
    Ok(Settled { applied, fills, index, change, conn })
}

/// The zones whose fill differs between `old` and `new` (by content), and the zones only one of
/// them has, sorted.
pub fn refilled(old: &[ZoneFill], new: &[ZoneFill]) -> Vec<ItemId> {
    let mut previous: BTreeMap<_, _> = old.iter().map(|fill| (fill.zone, fill)).collect();
    let mut changed = Vec::new();
    for fill in new {
        if !previous.remove(&fill.zone).is_some_and(|old| fill.same(old)) {
            changed.push(fill.zone);
        }
    }
    changed.extend(previous.into_keys());
    changed.sort_unstable();
    changed
}

/// Each net's count of missing connections (ratsnest lines).
pub fn ratsnest_lines(conn: &Connectivity) -> BTreeMap<NetId, usize> {
    let mut out = BTreeMap::new();
    for r in &conn.ratsnest {
        *out.entry(r.net).or_insert(0) += 1;
    }
    out
}
