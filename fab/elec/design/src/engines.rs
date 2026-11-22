//! The design's engine pipeline, published by the host as one result.
//!
//! `update` and pass A update the piece index for the delta and run indexed connectivity
//! (geometry only for the fresh pieces) and indexed DRC (the delta's region); full connectivity
//! runs only in `full` and for a unit without results. ERC and system nets are O(pins).
//! Runs on the host's worker, never on the UI thread.
//! Effective rules are derived on every rules pass and never stored.

use crate::ops::DesignApplied;
use crate::{sysnets, system_erc, Design, SysOp, SystemNet, Unit};
use fab_elec_board::drc::drc_indexed;
use fab_elec_board::{
    apply_capabilities, connect, connect_indexed, drc, resolve_rules_with, zone,
    Board, BoardDelta, Connectivity, Effective, Factories, IndexChange, PieceIndex, RuleMatrix, ZoneFill,
};
use fab_elec_circuit::{erc::erc_supplied, Violation};
use fab_elec_core::{BBox, BoardId, ItemId, Sel};
use std::sync::Arc;

/// One board's engine results, as the host publishes them.
#[derive(Clone, PartialEq, Debug)]
pub struct BoardResults {
    pub unit: BoardId,
    /// The board generation they were computed at.
    pub generation: u64,
    pub rules: Arc<RuleMatrix>,
    /// Every zone's fill, sorted by zone id (as `zone::fill_all`).
    pub fills: Arc<Vec<ZoneFill>>,
    /// The piece index over `fills`.
    pub index: Pieces,
    pub conn: Arc<Connectivity>,
    /// DRC findings, sorted by id.
    pub drc: Arc<Vec<Violation>>,
    pub pending: Pending,
}

/// The board's piece index over the results' `fills`, which the next update starts from. A function of
/// the board at `generation` and of `fills`, both of which equality compares, so equality leaves it
/// out. Cloning shares it.
#[derive(Clone)]
pub struct Pieces(pub Arc<PieceIndex>);

impl PartialEq for Pieces {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for Pieces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pieces(generation {})", self.0.generation())
    }
}

/// What a unit's results still owe (empty when exact: `full` always gives the default).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Pending {
    /// Zones whose fill is stale: (zone, the board generation of the latest delta that reached
    /// it), sorted by zone.
    pub zones: Vec<(ItemId, u64)>,
    /// Every delta since the last exact results, merged (`merge_delta`), with each merged fill's
    /// change box (`zone::changed_box` of old to new) in `bbox`. Default when `zones` is empty.
    pub debt: BoardDelta,
    /// What the piece index re-derived since the last exact results (their updates' changes,
    /// absorbed): the copper a pending zone's polygons test again when its fill merges, since they
    /// joined it by recorded joins while held. Default when `zones` is empty.
    pub changes: IndexChange,
}

/// Every engine result of a design.
#[derive(Clone, PartialEq, Debug)]
pub struct DesignResults {
    pub generation: u64,
    /// One per unit with a board, sorted by unit.
    pub boards: Vec<BoardResults>,
    /// ERC of every unit's circuit, concatenated in unit order (each list as `erc` returns it).
    pub erc: Arc<Vec<Violation>>,
    pub sysnets: Arc<Vec<SystemNet>>,
    pub system: Arc<Vec<Violation>>,
}

/// Every result from scratch.
pub fn full(d: &Design) -> DesignResults {
    full_with(d, &Factories::new())
}

/// Every result from scratch, each unit's rules raised by its factory's capabilities.
pub fn full_with(d: &Design, factories: &Factories) -> DesignResults {
    design_results(d, |unit, board| full_board(unit, board, factories))
}

/// A unit's effective rules for this pass: its factory's capabilities on the board's own rules
/// (`None` when it names no factory, or one `factories` does not hold).
pub fn effective(unit: &Unit, board: &Board, factories: &Factories) -> Option<Effective> {
    let name = unit.factory.as_ref()?;
    let caps = factories.get(name)?;
    Some(Effective { rules: apply_capabilities(&board.rules, caps), origin: name.clone(), via_in_pad: caps.via_in_pad })
}

/// The results after the boards changed by `deltas` (one merged delta per unit; a unit absent
/// from `deltas` is unchanged). Must equal `full(d)` (the test property).
pub fn update(prev: &DesignResults, d: &Design, deltas: &[(BoardId, BoardDelta)]) -> DesignResults {
    update_with(prev, d, deltas, &Factories::new())
}

/// The results after `deltas`, each unit's rules raised by its factory's capabilities. A change
/// of a unit's factory, or of a profile it names, is a global delta for that unit.
pub fn update_with(prev: &DesignResults, d: &Design, deltas: &[(BoardId, BoardDelta)], factories: &Factories) -> DesignResults {
    design_results(d, |unit, board| {
        let Some(prev) = prev.boards.iter().find(|b| b.unit == unit.id) else { return full_board(unit, board, factories) };
        match deltas.iter().find(|(id, _)| *id == unit.id) {
            None => prev.clone(),
            Some((_, delta)) if delta.global => full_board(unit, board, factories),
            Some((_, delta)) => update_board(prev, board, delta),
        }
    })
}

/// Pass A for one unit: the results right after `delta` without filling any zone.
/// Cost: sort/scan zones and clone tile references and island labels; `reaches` does a linear
/// zone lookup per zone. Removed fill boxes cost O(tiles), no geometry. Index update costs
/// O(changed pieces x log n); connectivity O(n log n) list passes without geometry plus indexed
/// geometry for the fresh pieces; DRC O(k log n) for the k pieces in the region. Rules resolve
/// only for a global delta or missing results. Runs on the engine strand.
pub fn update_copper(prev: Option<&BoardResults>, unit: &Unit, board: &Board, delta: &BoardDelta, factories: &Factories) -> BoardResults {
    let rules = match prev {
        Some(prev) if !delta.global => prev.rules.clone(),
        _ => Arc::new(resolve_rules_with(board, effective(unit, board, factories).as_ref(), None)),
    };
    let mut zones: Vec<_> = board.zones.iter().collect();
    zones.sort_by_key(|z| z.id);
    let mut pending = Pending::default();
    let mut fills = Vec::with_capacity(zones.len());
    for z in zones {
        if prev.is_none() || zone::reaches(board, &rules, z.id, delta) {
            pending.zones.push((z.id, board.generation));
        } else if let Some(prev) = prev {
            if let Ok(i) = prev.pending.zones.binary_search_by_key(&z.id, |(id, _)| *id) {
                pending.zones.push(prev.pending.zones[i]);
            }
        }
        let before = prev.and_then(|prev| prev.fills.binary_search_by_key(&z.id, |f| f.zone).ok().map(|i| &prev.fills[i]));
        fills.push(before.cloned().unwrap_or_else(|| ZoneFill {
            zone: z.id,
            lattice: zone::FillLattice::of(board, &rules),
            layers: Vec::new(),
        }));
    }
    let mut ext = delta.clone();
    if let Some(prev) = prev {
        for old in prev.fills.iter() {
            if fills.binary_search_by_key(&old.zone, |f| f.zone).is_err() {
                ext.changed.push(old.zone);
                ext.bbox = ext.bbox.union(fill_box(old));
            }
        }
    }
    ext.changed.sort();
    ext.changed.dedup();
    let mut debt = prev.map_or_else(BoardDelta::default, |prev| prev.pending.debt.clone());
    merge_delta(&mut debt, &ext);
    let stale: Vec<_> = pending.zones.iter().map(|(id, _)| *id).collect();
    let fills = Arc::new(fills);
    // Results that become exact without a fill (the last stale zone removed) check the whole debt,
    // as `merge_fill` does; otherwise the delta's own region (the debt is the delta when none was owed).
    let region = if stale.is_empty() { &debt } else { &ext };
    let (index, conn, drc, changes) = match prev {
        Some(prev) => {
            let mut index = (*prev.index.0).clone();
            let change = index.update(board, &fills, &ext, &refilled(&prev.fills, &fills));
            let conn = connect_indexed(&prev.conn, &index, board, &ext, &change, &stale, &prev.pending.changes);
            let drc = drc_indexed(&prev.drc, &index, board, &conn, &rules, region, &stale);
            let mut changes = prev.pending.changes.clone();
            changes.absorb(&change);
            (index, conn, drc, changes)
        }
        None => {
            let index = PieceIndex::build(board, &fills);
            let conn = connect(board, &fills);
            let drc = drc_indexed(&[], &index, board, &conn, &rules, &BoardDelta { global: true, ..Default::default() }, &stale);
            (index, conn, drc, IndexChange::all())
        }
    };
    let conn = Arc::new(conn);
    let drc = Arc::new(drc);
    if !stale.is_empty() {
        pending.debt = debt;
        pending.changes = changes;
    }
    BoardResults { unit: unit.id, generation: board.generation, rules, fills, index: Pieces(Arc::new(index)), conn, drc, pending }
}

/// Pass A over the design: changed units and units without results get `update_copper`; others
/// keep their results. ERC, system nets and system ERC are recomputed as in `design_results`.
/// Cost: `update_copper` per changed/new board, plus O(pins) for the design-wide engines.
pub fn update_held(prev: &DesignResults, d: &Design, deltas: &[(BoardId, BoardDelta)], factories: &Factories) -> DesignResults {
    design_results(d, |unit, board| {
        let prev = prev.boards.iter().find(|b| b.unit == unit.id);
        match deltas.iter().find(|(id, _)| *id == unit.id) {
            Some((_, delta)) => update_copper(prev, unit, board, delta, factories),
            None => match prev {
                Some(prev) => prev.clone(),
                None => update_copper(None, unit, board, &BoardDelta::default(), factories),
            },
        }
    })
}

/// One fill a unit's results owe, for a pool task.
#[derive(Clone, PartialEq, Debug)]
pub struct FillTask {
    pub unit: BoardId,
    pub zone: ItemId,
    pub generation: u64,
}

/// A finding that names a zone whose fill is still the held, pre-edit one: the refill decides it,
/// so views show it as pending, not as a violation. False once the zone's fill is exact.
/// Cost: O(the finding's items x log pending zones).
pub fn awaits_refill(r: &BoardResults, v: &Violation) -> bool {
    v.items.iter().any(|sel| matches!(sel, Sel::Item(unit, item) if *unit == r.unit && r.pending.zones.binary_search_by_key(item, |(zone, _)| *zone).is_ok()))
}

/// Every fill the results owe (unit order, then zone order); `generation` is the board generation
/// of the results (the snapshot the task fills over). Cost: O(units + pending zones).
pub fn owed(r: &DesignResults) -> Vec<FillTask> {
    r.boards.iter().flat_map(|board| board.pending.zones.iter().map(move |(zone, _)| FillTask {
        unit: board.unit, zone: *zone, generation: board.generation,
    })).collect()
}

/// Pass B: merge zone `zone`'s `fill`, computed over the unit's board at generation `at`
/// with `rules` (the results' rules the task was given). `None` when the zone is absent, is not
/// pending, a delta at a generation later than `at` reached it, or the unit's rules are no longer
/// that `Arc` (a factory or rules change re-resolves them; a factory change moves no board
/// generation). Other boards and design-wide results are kept.
/// Cost: clone tile references and island labels, compute the changed box in O(tiles) without
/// geometry, and merge the debt; index update for the zone's changed tiles in O(changed pieces x log n),
/// indexed connectivity with O(n log n) list passes and geometry for fresh pieces, and indexed DRC
/// in the debt's region in O(k log n) for the k pieces there.
/// No zone is filled and no rules or design-wide engine is recomputed.
pub fn merge_fill(
    prev: &DesignResults,
    d: &Design,
    unit: BoardId,
    zone: ItemId,
    at: u64,
    rules: &Arc<RuleMatrix>,
    fill: ZoneFill,
) -> Option<DesignResults> {
    let i = prev.boards.iter().position(|b| b.unit == unit)?;
    let prev_unit = &prev.boards[i];
    let pending_i = prev_unit.pending.zones.binary_search_by_key(&zone, |(id, _)| *id).ok()?;
    if at < prev_unit.pending.zones[pending_i].1 || !Arc::ptr_eq(rules, &prev_unit.rules) {
        return None;
    }
    let board = d.unit(unit)?.board.as_ref()?;
    if !board.zones.iter().any(|z| z.id == zone) {
        return None;
    }
    let fill_i = prev_unit.fills.binary_search_by_key(&zone, |f| f.zone).ok()?;
    let delta = BoardDelta {
        changed: vec![zone],
        bbox: zone::changed_box(&prev_unit.fills[fill_i], &fill),
        ..Default::default()
    };
    let mut out = prev.clone();
    let next = &mut out.boards[i];
    Arc::make_mut(&mut next.fills)[fill_i] = fill;
    next.pending.zones.remove(pending_i);
    merge_delta(&mut next.pending.debt, &BoardDelta { bbox: delta.bbox, ..Default::default() });
    let remaining: Vec<_> = next.pending.zones.iter().map(|(id, _)| *id).collect();
    let mut index = (*prev_unit.index.0).clone();
    let change = index.update(board, &next.fills, &delta, &[zone]);
    next.conn = Arc::new(connect_indexed(&prev_unit.conn, &index, board, &delta, &change, &remaining, &prev_unit.pending.changes));
    next.pending.changes.absorb(&change);
    next.drc = Arc::new(drc_indexed(&prev_unit.drc, &index, board, &next.conn, &next.rules, &next.pending.debt, &remaining));
    next.index = Pieces(Arc::new(index));
    if remaining.is_empty() {
        next.pending = Pending::default();
    }
    Some(out)
}

/// The board deltas an applied op causes: a board op's own; a factory change is a global delta
/// of that unit's board (its effective rules change); other system ops change no board.
pub fn deltas_of(d: &Design, applied: &DesignApplied) -> Vec<(BoardId, BoardDelta)> {
    match applied {
        DesignApplied::Board { unit, applied } => vec![(*unit, applied.delta.clone())],
        DesignApplied::Sys { inverse: SysOp::SetFactory { unit, .. } } => d.unit(*unit)
            .and_then(|u| u.board.as_ref())
            .map(|board| vec![(*unit, BoardDelta { global: true, bbox: board.outline_bbox(), ..Default::default() })])
            .unwrap_or_default(),
        DesignApplied::Sys { .. } => Vec::new(),
    }
}

/// Fold `next` into `into` so the result covers both (ids united and deduplicated, sorted;
/// boxes united; item extents concatenated in order; `global` or-ed; nets united, sorted, deduplicated).
pub fn merge_delta(into: &mut BoardDelta, next: &BoardDelta) {
    for (ids, more) in [(&mut into.added, &next.added), (&mut into.removed, &next.removed), (&mut into.changed, &next.changed)] {
        ids.extend_from_slice(more);
        ids.sort();
        ids.dedup();
    }
    into.bbox = into.bbox.union(next.bbox);
    into.extents.extend_from_slice(&next.extents);
    into.global |= next.global;
    into.nets.extend_from_slice(&next.nets);
    into.nets.sort();
    into.nets.dedup();
}

/// Every finding of the results (DRC of each board, ERC, system ERC), sorted by id, duplicates
/// by id removed: what the Checks list shows.
pub fn violations(r: &DesignResults) -> Vec<Violation> {
    let mut out: Vec<_> = r.boards.iter().flat_map(|b| b.drc.iter()).chain(r.erc.iter()).chain(r.system.iter()).cloned().collect();
    out.sort_by_key(|v| v.id);
    out.dedup_by_key(|v| v.id);
    out
}

fn design_results(d: &Design, mut board_results: impl FnMut(&Unit, &Board) -> BoardResults) -> DesignResults {
    let mut units: Vec<_> = d.units.iter().collect();
    units.sort_by_key(|u| u.id);
    let mut boards = Vec::new();
    let mut circuit_erc = Vec::new();
    for u in units {
        if let Some(board) = &u.board {
            boards.push(board_results(u, board));
        }
        circuit_erc.extend(erc_supplied(&u.circuit, &u.supplied));
    }
    DesignResults {
        generation: d.generation,
        boards,
        erc: Arc::new(circuit_erc),
        sysnets: Arc::new(sysnets(d)),
        system: Arc::new(system_erc(d)),
    }
}

fn full_board(unit: &Unit, board: &Board, factories: &Factories) -> BoardResults {
    let rules = Arc::new(resolve_rules_with(board, effective(unit, board, factories).as_ref(), None));
    let fills = Arc::new(zone::fill_all(board, &rules));
    let index = PieceIndex::build(board, &fills);
    let conn = Arc::new(connect(board, &fills));
    let drc = Arc::new(drc(board, &conn, &rules, &fills, None).violations);
    BoardResults { unit: unit.id, generation: board.generation, rules, fills, index: Pieces(Arc::new(index)), conn, drc, pending: Pending::default() }
}

/// Update fixed pieces before filling and fill pieces afterward, sharing one index across zones.
/// Index work costs O(changed pieces x log n), avoiding a fresh O(n log n) index build per zone.
fn update_board(prev: &BoardResults, board: &Board, delta: &BoardDelta) -> BoardResults {
    // Rules, net classes and the unit's factory change only with a global delta.
    let rules = prev.rules.clone();
    let mut index = (*prev.index.0).clone();
    let mut change = index.update(board, &prev.fills, delta, &[]);
    let mut zones: Vec<_> = board.zones.iter().collect();
    zones.sort_by_key(|z| z.id);
    let mut fills = Vec::with_capacity(zones.len());
    let mut ext = delta.clone();
    for z in zones {
        let before = prev.fills.binary_search_by_key(&z.id, |f| f.zone).ok().map(|i| &prev.fills[i]);
        let fill = match before {
            Some(before) => zone::fill_update_indexed(board, &rules, before, delta, &index),
            None => zone::fill(board, &rules, z.id),
        };
        if before.map_or(true, |before| !fill.same(before)) {
            ext.changed.push(z.id);
            let bbox = before.map_or_else(|| fill_box(&fill), |before| zone::changed_box(before, &fill));
            ext.bbox = ext.bbox.union(bbox);
        }
        fills.push(fill);
    }
    for old in prev.fills.iter() {
        if fills.binary_search_by_key(&old.zone, |f| f.zone).is_err() {
            ext.changed.push(old.zone);
            ext.bbox = ext.bbox.union(fill_box(old));
        }
    }
    ext.changed.sort();
    ext.changed.dedup();
    let fills = Arc::new(fills);
    change.absorb(&index.update(board, &fills, &BoardDelta::default(), &refilled(&prev.fills, &fills)));
    let conn = Arc::new(connect_indexed(&prev.conn, &index, board, &ext, &change, &[], &prev.pending.changes));
    let drc = Arc::new(drc_indexed(&prev.drc, &index, board, &conn, &rules, &ext, &[]));
    BoardResults { unit: prev.unit, generation: board.generation, rules, fills, index: Pieces(Arc::new(index)), conn, drc, pending: Pending::default() }
}

/// The zones whose fill differs between two fill lists (both sorted by zone), or that only one lists; sorted.
/// O(zones), no geometry.
fn refilled(old: &[ZoneFill], new: &[ZoneFill]) -> Vec<ItemId> {
    let mut changed = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if old[i].zone < new[j].zone {
            changed.push(old[i].zone);
            i += 1;
        } else if old[i].zone > new[j].zone {
            changed.push(new[j].zone);
            j += 1;
        } else {
            if !old[i].same(&new[j]) {
                changed.push(old[i].zone);
            }
            i += 1;
            j += 1;
        }
    }
    changed.extend(old[i..].iter().map(|fill| fill.zone));
    changed.extend(new[j..].iter().map(|fill| fill.zone));
    changed
}

fn fill_box(fill: &ZoneFill) -> BBox {
    let mut bbox = BBox::EMPTY;
    for layer in &fill.layers {
        for (tile, labels) in layer.tiles.iter().zip(&layer.labels) {
            if labels.kept.contains(&true) {
                bbox = bbox.union(fill.lattice.tile_box(tile.key));
            }
        }
    }
    bbox
}
