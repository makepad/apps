//! The verifier (E6 0.8, design 4): what every engine runs on what it adds, through the board's
//! own `check_to`, `check_drills` and `check_via_in_pad`, the functions DRC calls. There is no
//! rule logic here.
//!
//! New copper is tested against every world item on its layer within its box grown by the
//! largest need; a new drill against the world's drills and copper. Items added to the world's
//! overlay earlier in a transaction are part of the world, so a solution's own nets are tested
//! against each other.
//!
//! The fault rule: copper an answer lays new must be clean. Copper that keeps the shape of a base
//! piece (`RItem::kept`: the base itself, what moves with a moved item by one translation, a track
//! segment kept where it was) may keep a fault it had: the same pair of selections, layer and rule,
//! at a gap no smaller than the pair's in the base. Two such pieces moved by the same translation
//! are not checked at all (their gap is the base's). A pair the base already shorts (a base gap of
//! 0 or less) may overlap deeper; a pair the base only had too close may not become a short unless
//! it was one.
//!
//! Cost: O(pieces x (log n + k)) for k neighbours each; a meeting of two kept pieces moved apart
//! adds the base pieces of their two selections (`base_gap`), unless the base pieces of either
//! item fault nothing (`base_clean`: asked once per item and base generation).

use crate::proposal::Collision;
use crate::world::{RBase, RId, RItem, RPiece, WRef, World};
use fab_elec_board::drc::sel_of;
use fab_elec_board::{check_drills, check_to, check_via_in_pad, Board, ConnItem, Copper, Item, Obstacle, PairFault, RuleMatrix, RuleRef, Track};
use fab_elec_core::{BBox, ClassId, FeatureRef, ItemId, LayerId, LayerSet, Nm, PadIx, Seg, Sel, P};
use std::ops::ControlFlow;
use std::sync::atomic::Ordering::Relaxed;

/// The world pieces of a track or via that is not on the board yet (other items: none).
pub fn new_pieces(board: &Board, item: &Item) -> Vec<RItem> {
    let class = |net: fab_elec_core::NetId| board.nets.get(net.ix()).map(|n| n.class).unwrap_or_default();
    let mut out = Vec::new();
    let push = |piece: RPiece, layers: LayerSet, id: ItemId, out: &mut Vec<RItem>| {
        let bbox = match &piece {
            RPiece::Copper(c) => c.shape.bbox(),
            RPiece::Edge(s) | RPiece::Drill { shape: s, .. } | RPiece::Keepout { area: s, .. } => s.bbox(),
        };
        let rank = out.len() as u32;
        out.push(RItem { piece, item: Some(id), rank, layers, bbox, fixity: crate::world::Fixity::Solved, locked: false, kept: None });
    };
    match item {
        Item::Track(t) => return track_pieces(board, t, |_| true),
        Item::Via(v) => {
            let layers = v.span_layers(board.copper_set());
            for l in layers.iter() {
                let c = Copper { item: ConnItem::Via(v.id), net: Some(v.net), class: class(v.net), layer: l, shape: v.shape() };
                push(RPiece::Copper(c), LayerSet::EMPTY.with(l), v.id, &mut out);
            }
            let h = v.drill_hole();
            push(RPiece::Drill { item: h.item, net: h.net, shape: h.shape() }, layers, v.id, &mut out);
        }
        _ => {}
    }
    out
}

/// The world pieces of the segments of track `t` that are `kept`, as `new_pieces` makes them
/// (each ranked by its segment). O(segments).
pub fn track_pieces(board: &Board, t: &Track, kept: impl Fn(usize) -> bool) -> Vec<RItem> {
    let class = board.nets.get(t.net.ix()).map(|n| n.class).unwrap_or_default();
    t.path.steps().enumerate().filter(|(si, _)| kept(*si)).map(|(si, (a, seg))| piece(t, class, si, a, &seg)).collect()
}

/// The world piece segment `si` of track `t` would be, from `a` along `seg`, as `new_pieces`
/// makes it: a track's own segment, or a probe of one. O(1).
pub fn track_piece(board: &Board, t: &Track, si: usize, a: P, seg: &Seg) -> RItem {
    piece(t, board.nets.get(t.net.ix()).map(|n| n.class).unwrap_or_default(), si, a, seg)
}

fn piece(t: &Track, class: ClassId, si: usize, a: P, seg: &Seg) -> RItem {
    let shape = t.seg_shape(si, a, seg);
    let bbox = shape.bbox();
    let c = Copper { item: ConnItem::Track { item: t.id, seg: si as u32 }, net: Some(t.net), class, layer: t.layer, shape };
    RItem { piece: RPiece::Copper(c), item: Some(t.id), rank: si as u32, layers: LayerSet::EMPTY.with(t.layer), bbox, fixity: crate::world::Fixity::Solved, locked: false, kept: None }
}

/// The selection a world item is reported as.
fn sel(board: &Board, it: &RItem) -> Sel {
    match &it.piece {
        RPiece::Copper(c) => sel_of(board, c.item),
        RPiece::Drill { item, .. } => sel_of(board, *item),
        RPiece::Edge(_) => match it.item {
            Some(id) => Sel::Item(board.unit, id),
            None => Sel::Feature(FeatureRef::Outline(board.unit)),
        },
        RPiece::Keepout { .. } => match it.item {
            Some(id) => Sel::Item(board.unit, id),
            None => Sel::Feature(FeatureRef::Outline(board.unit)),
        },
    }
}

/// Every collision of `new` (pieces of items not yet in `world`) with `world`, in a stable order.
pub fn verify(world: &World, rules: &RuleMatrix, board: &Board, new: &[RItem]) -> Vec<Collision> {
    sorted(hits(world, rules, board, new).into_iter().map(|(collision, _)| collision).collect())
}

/// `collisions` in `verify`'s stable order, one per pair of selections and layer.
pub fn sorted(mut collisions: Vec<Collision>) -> Vec<Collision> {
    collisions.sort_by(|x, y| (&x.a, &x.b, x.layer, x.at, x.gap).cmp(&(&y.a, &y.b, y.layer, y.at, y.gap)));
    collisions.dedup_by(|x, y| x.a == y.a && x.b == y.b && x.layer == y.layer);
    collisions
}

/// Every collision of `new` with `world`, each with the world piece it is with: one per piece
/// pair and layer, in query order.
pub fn hits(world: &World, rules: &RuleMatrix, board: &Board, new: &[RItem]) -> Vec<(Collision, WRef)> {
    hits_ruled(world, rules, board, new, |_| true)
}

/// `hits`, under the fault rule only against the world pieces `ruled` names: against the others
/// `new` is new copper, and a fault it had in the base counts too.
pub fn hits_ruled(world: &World, rules: &RuleMatrix, board: &Board, new: &[RItem], ruled: impl Fn(&RItem) -> bool) -> Vec<(Collision, WRef)> {
    let mut out = Vec::new();
    for n in new {
        let first = n.layers.iter().next();
        let reach = reach_of(rules, board, world.base, n);
        for layer in n.layers.iter() {
            let area = n.bbox.inflate(reach);
            for (reference, it) in world.query_sorted(layer, area) {
                let f = if ruled(it) { fault_now(world.base, rules, board, n, it, layer, first) } else { fault(rules, board, n, it, layer, first) };
                if let Some(f) = f {
                    out.push((Collision { at: f.at, layer, a: sel(board, n), b: sel(board, it), gap: f.gap, need: f.need.gap }, reference));
                }
            }
        }
    }
    out
}

/// Whether `new` meets anything in `world`: `hits` not empty, stopping at the first. One query
/// per layer over all of `new` on it. O(layers × (log n + k × pieces of `new`)).
pub fn meets(world: &World, rules: &RuleMatrix, board: &Board, new: &[RItem]) -> bool {
    meets_in(world, rules, board, new, false)
}

/// Whether `new` meets any added piece of `world`. For pieces keeping a base shape at
/// translation zero this equals `meets`, since every base piece has that translation too
/// and `fault_now` accepts two kept pieces with the same translation.
pub fn meets_added(world: &World, rules: &RuleMatrix, board: &Board, new: &[RItem]) -> bool {
    meets_in(world, rules, board, new, true)
}

fn meets_in(world: &World, rules: &RuleMatrix, board: &Board, new: &[RItem], added_only: bool) -> bool {
    let reach: Vec<Nm> = new.iter().map(|n| reach_of(rules, board, world.base, n)).collect();
    let base = world.base;
    let layers = new.iter().fold(LayerSet::EMPTY, |a, n| a.union(n.layers));
    layers.iter().any(|layer| {
        let area = new.iter().zip(&reach).filter(|(n, _)| n.layers.has(layer)).fold(BBox::EMPTY, |a, (n, &r)| a.union(n.bbox.inflate(r)));
        let mut met = false;
        let visit = |_, it: &RItem| {
            met = meets_one(base, rules, board, &reach, new, it, layer);
            if met { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        };
        if added_only {
            world.query_added(layer, area, visit);
        } else {
            world.query(layer, area, visit);
        }
        met
    })
}

/// Whether `new` meets any of `among` (pieces of a world over `base`), as `meets` would find
/// them there. Like `meets`, it skips layers the base does not index. O(pieces of `new` × `among`).
pub fn meets_among(base: &RBase, rules: &RuleMatrix, board: &Board, new: &[RItem], among: &[&RItem]) -> bool {
    let reach: Vec<Nm> = new.iter().map(|n| reach_of(rules, board, base, n)).collect();
    among.iter().any(|it| {
        it.layers.iter().any(|layer| base.layer_index(layer).is_some() && meets_one(base, rules, board, &reach, new, it, layer))
    })
}

/// Whether one of `new` (each with its `reach_of`) breaks a rule with world piece `it` on `layer`.
fn meets_one(base: &RBase, rules: &RuleMatrix, board: &Board, reach: &[Nm], new: &[RItem], it: &RItem, layer: LayerId) -> bool {
    new.iter().zip(reach).any(|(n, &r)| n.layers.has(layer) && n.bbox.inflate(r).intersects(&it.bbox) && fault_now(base, rules, board, n, it, layer, n.layers.iter().next()).is_some())
}

/// How far from a new piece's box a world piece can collide with it, but a via drill's gap to a
/// pad (`reach_of`): the largest clearance, hole and edge need. O(1).
pub fn reach(rules: &RuleMatrix) -> Nm {
    rules.max_clearance().max(rules.hole_to_hole()).max(rules.edge()).max(1)
}

/// How far from new piece `n`'s box a world piece over `base` can collide with it: `reach`, and
/// for a via drill the gap a drill keeps from a pad, for a pad the gap a via drill keeps from
/// it. Only these grow by a pad's mask margin, which may be far wider than the clearances; a
/// via drill grows past the board-wide margin's gap only near a pad that sets a wider margin of
/// its own. O(pads that set their own margin) for a via drill, a pad's lookup for a pad.
pub fn reach_of(rules: &RuleMatrix, board: &Board, base: &RBase, n: &RItem) -> Nm {
    match &n.piece {
        RPiece::Drill { item: ConnItem::Via(_), .. } => {
            let mut out = reach(rules).max(rules.via_drill_to_pad(None));
            for (pad, margin) in base.pad_margins() {
                let gap = rules.via_drill_to_pad(Some(margin));
                if gap > out && n.bbox.inflate(gap).intersects(&pad) {
                    out = gap;
                }
            }
            out
        }
        _ => copper_reach(rules, board, n),
    }
}

/// `reach_of` a piece that is not a via drill.
fn copper_reach(rules: &RuleMatrix, board: &Board, n: &RItem) -> Nm {
    match &n.piece {
        RPiece::Copper(Copper { item: ConnItem::Pad { placed, pad }, .. }) => reach(rules).max(rules.via_drill_to_board_pad(board, *placed, *pad)),
        _ => reach(rules),
    }
}

/// The fault of answer piece `n` with world piece `it` on `layer` under the fault rule (see the
/// module docs): none when both keep a base piece's shape and are moved by one translation; when
/// they are moved apart, none when the base has that fault (the same selections, layer and rule)
/// at a gap no larger, or as a short (any gap then). Else `fault`. O(1), plus `base_gap` for a
/// fault of two kept pieces. Every caller finds `it` on an indexed `layer`, which `base_clean`
/// and `base_gap` rely on here.
fn fault_now(base: &RBase, rules: &RuleMatrix, board: &Board, n: &RItem, it: &RItem, layer: LayerId, first: Option<LayerId>) -> Option<PairFault> {
    if let (Some((_, a)), Some((_, b))) = (n.kept, it.kept) {
        if a == b {
            return None;
        }
    }
    let f = fault(rules, board, n, it, layer, first)?;
    match (n.kept, it.kept) {
        (Some((a, _)), Some((b, _))) if !base_clean(base, rules, board, n.item) && !base_clean(base, rules, board, it.item) && base_gap(base, rules, board, (n.item, a), (it.item, b), layer, f.need.rule).is_some_and(|base| base <= 0 || f.gap >= base) => None,
        _ => Some(f),
    }
}

/// Whether the base pieces of `item` (`None` for the outline) are free of faults on all indexed
/// layers, so `base_gap` finds none there. Returns `false` when the base cannot say.
/// The first ask per base generation scans each piece's neighbours, stopping at the first fault.
/// Later asks read the cached result in O(1). Big items skip the scan and return `false`.
fn base_clean(base: &RBase, rules: &RuleMatrix, board: &Board, item: Option<ItemId>) -> bool {
    /// A big item's first scan can take thousands of fault checks. `base_gap` decides its pairs exactly.
    const MAX_SCAN_PIECES: usize = 64;

    let Some(bit) = base.clean_bit(item) else { return false };
    match bit.load(Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    let rids = match item {
        Some(id) => base.pieces(id),
        None => base.outline(),
    };
    if rids.len() > MAX_SCAN_PIECES {
        return false;
    }
    let mut clean = true;
    for x in rids.iter().filter_map(|&r| base.get(r)) {
        let area = x.bbox.inflate(reach_of(rules, board, base, x));
        for layer in x.layers.iter() {
            base.query(layer, area, |_, y| {
                if fault(rules, board, x, y, layer, x.layers.iter().next()).is_some() || fault(rules, board, y, x, layer, y.layers.iter().next()).is_some() {
                    clean = false;
                }
                if clean { ControlFlow::Continue(()) } else { ControlFlow::Break(()) }
            });
        }
        if !clean {
            break;
        }
    }
    bit.store(if clean { 1 } else { 2 }, Relaxed);
    clean
}

/// The selection a piece is part of, as `sel` names it, without the board: its item, and the pad
/// for a part's pad.
fn selection(it: &RItem) -> (Option<ItemId>, Option<PadIx>) {
    let conn = |c: ConnItem| match c {
        ConnItem::Pad { placed, pad } => (Some(placed), Some(pad)),
        ConnItem::Track { item, .. } | ConnItem::Via(item) | ConnItem::Hole(item) | ConnItem::Zone { item, .. } => (Some(item), None),
    };
    match &it.piece {
        RPiece::Copper(c) => conn(c.item),
        RPiece::Drill { item, .. } => conn(*item),
        RPiece::Edge(_) | RPiece::Keepout { .. } => (it.item, None),
    }
}

/// The smallest gap at which the base pieces of the selections of base pieces `a` and `b` (each
/// its item, `None` for the outline, and its rank) break `rule` on `layer`; `None` when they do
/// not. For each member of `a`, scan `b`'s slice or, for a large item or pad on an indexed
/// layer, query the neighbours within reach. The outline always uses its slice. O(pieces of
/// `a`'s selection × (pieces of `b`'s, or log n + the pieces near each)).
fn base_gap(base: &RBase, rules: &RuleMatrix, board: &Board, a: (Option<ItemId>, u32), b: (Option<ItemId>, u32), layer: LayerId, rule: RuleRef) -> Option<Nm> {
    const QUERY_THRESHOLD: usize = 32;

    let side = |(item, rank): (Option<ItemId>, u32)| {
        let rids = match item {
            Some(item) => base.pieces(item),
            None => base.outline(),
        };
        // An item's pieces are in rank order.
        let at = |r: &RId| base.get(*r).filter(|it| it.rank == rank);
        let piece = rids.get(rank as usize).and_then(at).or_else(|| rids.iter().find_map(at))?;
        let of = selection(piece);
        let rids = match of {
            (Some(placed), Some(pad)) => base.pad_pieces(placed, pad),
            _ => rids,
        };
        Some((rids, of))
    };
    let (xs, of_a) = side(a)?;
    let (ys, of_b) = side(b)?;
    let query_b = of_b.0.is_some() && ys.len() > QUERY_THRESHOLD && base.layer_index(layer).is_some();
    let mut gap: Option<Nm> = None;
    for x in xs.iter().filter_map(|&r| base.get(r)).filter(|it| it.layers.has(layer) && selection(it) == of_a) {
        let area = x.bbox.inflate(reach_of(rules, board, base, x));
        let first = x.layers.iter().next();
        let mut check = |y: &RItem| {
            if selection(y) != of_b || !y.layers.has(layer) || !area.intersects(&y.bbox) {
                return;
            }
            if let Some(f) = fault(rules, board, x, y, layer, first).filter(|f| f.need.rule == rule) {
                gap = Some(gap.map_or(f.gap, |g| g.min(f.gap)));
            }
        };
        if query_b {
            base.query(layer, area, |_, y| {
                check(y);
                ControlFlow::Continue(())
            });
        } else {
            for y in ys.iter().filter_map(|&r| base.get(r)) {
                check(y);
            }
        }
    }
    gap
}

/// The fault of new piece `n` with world piece `it` on `layer` (`first`: `n`'s first layer): a
/// clearance, hole or edge fault, hole to hole, or a via drill in a pad either way round.
fn fault(rules: &RuleMatrix, board: &Board, n: &RItem, it: &RItem, layer: LayerId, first: Option<LayerId>) -> Option<PairFault> {
    if it.item.is_some() && it.item == n.item {
        return None;
    }
    match (&n.piece, &it.piece) {
        (RPiece::Copper(c), RPiece::Drill { item: ConnItem::Via(_), net, shape }) => {
            check_to(rules, c, &it.obstacle(), layer).or_else(|| check_via_in_pad(board, rules, *net, shape, c))
        }
        (RPiece::Copper(c), _) => check_to(rules, c, &it.obstacle(), layer),
        (RPiece::Drill { item, net, shape }, RPiece::Copper(wc)) => check_to(rules, wc, &Obstacle::Drill { item: *item, net: *net, shape }, layer)
            .or_else(|| if matches!(item, ConnItem::Via(_)) { check_via_in_pad(board, rules, *net, shape, wc) } else { None }),
        // hole to hole is not a layer matter: tested once, on the drill's first layer
        (RPiece::Drill { item, shape, .. }, RPiece::Drill { item: wi, shape: ws, .. }) if Some(layer) == first => check_drills(rules, (*item, shape), (*wi, ws)),
        _ => None,
    }
}

/// The world's pieces on one layer that new copper within an area can collide with, queried once,
/// to test many such pieces (`clear`). O(log n + k).
pub struct Near<'w> {
    base: &'w RBase,
    layer: LayerId,
    area: BBox,
    pieces: Vec<&'w RItem>,
}

impl<'w> Near<'w> {
    /// What new copper on `layer` within `within` can collide with in `world`.
    pub fn gather(world: &'w World, rules: &RuleMatrix, layer: LayerId, within: BBox) -> Near<'w> {
        let area = within.inflate(reach(rules));
        Near { base: world.base, layer, area, pieces: world.query_sorted(layer, area).into_iter().map(|(_, it)| it).collect() }
    }

    /// Whether new piece `n` collides with nothing (`verify` of it alone is empty), when it is
    /// copper on this layer only and within the area; `None` when it is not. O(k).
    pub fn clear(&self, rules: &RuleMatrix, board: &Board, n: &RItem) -> Option<bool> {
        let one_layer = n.layers.has(self.layer) && n.layers.iter().count() == 1;
        if !matches!(n.piece, RPiece::Copper(_)) || !one_layer {
            return None;
        }
        let area = n.bbox.inflate(copper_reach(rules, board, n));
        if !self.area.contains_box(&area) {
            return None;
        }
        Some(!self.pieces.iter().any(|it| it.bbox.intersects(&area) && fault_now(self.base, rules, board, n, it, self.layer, Some(self.layer)).is_some()))
    }
}
