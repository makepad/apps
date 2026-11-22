//! Design rule check (C4, E1 4.3). First version by ELEC at M0 with the core checks: copper
//! clearance by net pair through the rule matrix, edge clearance, hole clearance, hole to hole,
//! pair gaps between the tracks of a differential pair, track width, via drill and annular ring,
//! keepout breaches, courtyard overlaps, silk over mask openings, dangling (`Free`) track ends,
//! dangling vias, vias in pads, shorts and unconnected items.
//! EROUTE and EDRC own it from here (the rest of E1 4.3's table, the reference's parity from the specs).
//!
//! Live form (E4): `drc(.., Some(delta))` checks only the region the delta can affect and says
//! so (`DrcResult::region`); `merge_violations` folds it into the previous full list. The rule
//! that makes the merge exact: every geometric violation has an `at` point inside the extent of
//! each item it names grown by the required clearance, so a violation can only appear, change or
//! vanish with `at` inside `delta.bbox` grown by the largest clearance. Region and scope each grow
//! by the largest copper, hole-to-hole, silk, pair or via-drill-to-pad clearance. Inside the region the update
//! reports everything, outside it the previous list stands. Silk's `at` is inside the opening's box and
//! the silk box grown by its clearance. A pair gap's `at` is inside one track's box grown by the gap
//! and the other's box. Shorts and unconnected items come from connectivity and
//! are always reported in full, and so are pour necks, which read it (`pour_neck_checks`).
//! Dangling vias also come from connectivity in full. A via whose
//! class allows vias in pads and whose bound factory, if any, fills and caps them raises no
//! `ViaInPad` finding. Its location when reported is inside the pad box and the drill box grown by
//! that pad's required gap. Finding ids are content ids: the kind,
//! rule and sorted item paths or geometry, never the item ids reissued when a document is read.
//! `Waiver::violation` holds that content id, so waivers and merged lists survive rereads.
//! Waived violations are left out.
//!
//! One definition of legal (E6 0.8): `required`, `check_pair`, `check_to`, `check_drills`,
//! `check_via_in_pad` and `check_item` are DRC's own inner checks, public so the router's verifier
//! calls exactly them.
//! A router-legal result that DRC rejects is a bug in one of the two, found by a differential test.
//!
//! Cost: O(n log n) in copper pieces per full run (an R-tree per layer, exact distances on
//! candidates only, rules looked up in O(1)); an update is O(k log n) in the pieces near the
//! delta, plus a scan of the pads for the via-drill search bound. No predicate runs in the inner
//! loop (custom rules are resolved in the matrix).
//! Silk scans owner bounds, indexes openings per side, and lays out text only for owners in scope.
//! Content ids encode each named item's path or geometry, sharing a component lookup per run.

use crate::connect::{pieces, ConnItem, Connectivity};
use crate::geom::Copper;
use crate::index::{piece_clearance, PKey, Piece, PieceIndex, PieceKind};
use crate::ops::BoardDelta;
use crate::rules::{RuleMatrix, RuleRef};
use crate::text::{footprint_text, text_bound, text_strokes};
use crate::zone::{class_of, TileKey, ZoneFill};
use crate::{Anchor, Board, ItemKind, Placed, Track, Zone, ARC_ERR};
use fab_elec_circuit::{CheckKind, Measure, Violation};
use fab_elec_core::{
    clearance, BBox, Canon, CompId, CompRef, FeatureRef, Index, ItemId, LayerId, LayerSet, NetId, Nm, PadIx, PolySet,
    Seg, Sel, Severity, Shape, Side, Writer, P,
};
use fab_elec_core::{dist::core_distance, shape::Core};
use fab_elec_parts::{Forbid, FpTextKind};
use std::collections::{BTreeSet, HashMap};
use std::ops::ControlFlow;

/// What two pieces of copper need between them on a layer, and the rule that says so.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Required {
    pub gap: Nm,
    pub rule: RuleRef,
}

/// The clearance `a` and `b` need on `layer`: 0 for one item or one net, else the matrix's.
/// Two tracks of one pair need at most the pair gap: a pair may couple tighter than its class's
/// clearance. (Edge and hole clearances are `RuleMatrix::edge` and `hole_clearance`.)
pub fn required(m: &RuleMatrix, a: &Copper, b: &Copper, layer: LayerId) -> Required {
    if a.same_item(b) || (a.net.is_some() && a.net == b.net) {
        return Required { gap: 0, rule: RuleRef::CLEARANCE };
    }
    let (gap, rule) = m.clearance_ref(a.key(), b.key(), layer);
    if let Some(pair_gap) = partner_tracks(m, a, b).filter(|&g| g < gap) {
        return Required { gap: pair_gap, rule: m.pair_gap_ref() };
    }
    Required { gap, rule }
}

/// The pair gap between `a` and `b` when they are tracks of a pair's two nets on one layer.
fn partner_tracks(m: &RuleMatrix, a: &Copper, b: &Copper) -> Option<Nm> {
    if a.layer != b.layer
        || !matches!(a.item, ConnItem::Track { .. })
        || !matches!(b.item, ConnItem::Track { .. })
    {
        return None;
    }
    let (partner, gap) = m.partner(a.net?)?;
    (b.net == Some(partner) && gap > 0).then_some(gap)
}

/// How far a measured gap may fall short of a required one before it is a fault: rounding in
/// coordinates and polygonised curves, as the reference allows (0.5 µm). A requirement no larger
/// than this (a keepout's "must not touch") gets no allowance.
pub const DRC_EPSILON: Nm = 500;

/// The smallest gap a clearance requirement allows. Every clearance verdict (`short_of`: copper,
/// hole and hole-to-hole gaps) uses this threshold; the via-in-pad mask web (`via_pad_fault`) is
/// judged without it.
pub fn allowed_need(need: Nm) -> Nm {
    if need > DRC_EPSILON { need - DRC_EPSILON } else { need }
}

/// Whether `gap` falls short of `need` by more than the allowance.
fn short_of(gap: Nm, need: Nm) -> bool {
    gap < allowed_need(need)
}

/// A clearance fault between two pieces of copper.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct PairFault {
    /// The exact gap (0 when they touch or overlap).
    pub gap: Nm,
    pub need: Required,
    /// A point inside both boxes grown by the need (the merge rule above).
    pub at: P,
}

/// `Some` when `a` and `b` are closer on `layer` than they need to be.
pub fn check_pair(m: &RuleMatrix, a: &Copper, b: &Copper, layer: LayerId) -> Option<PairFault> {
    check_pair_with(m, a, b, layer, || clearance(&a.shape, &b.shape))
}

fn check_piece_pair(
    m: &RuleMatrix,
    pa: &Piece,
    a: &Copper,
    pb: &Piece,
    b: &Copper,
    layer: LayerId,
) -> Option<PairFault> {
    check_pair_with(m, a, b, layer, || piece_clearance(pa, a, pb, b))
}

fn check_pair_with(
    m: &RuleMatrix,
    a: &Copper,
    b: &Copper,
    layer: LayerId,
    gap: impl FnOnce() -> Nm,
) -> Option<PairFault> {
    let need = required(m, a, b, layer);
    if need.gap == 0 {
        return None;
    }
    let (ab, bb) = (a.shape.bbox(), b.shape.bbox());
    if !ab.inflate(need.gap).intersects(&bb) {
        return None;
    }
    let gap = gap();
    short_of(gap, need.gap).then(|| PairFault { gap, need, at: overlap_centre(ab.inflate(need.gap), bb) })
}

/// A pair-gap fault between track pieces on one layer, using the caller's exact distance.
fn check_pair_gap_with(
    rules: &RuleMatrix,
    a: &Copper,
    b: &Copper,
    gap: impl FnOnce() -> Nm,
) -> Option<PairFault> {
    let g = partner_tracks(rules, a, b)?;
    let (ab, bb) = (a.shape.bbox(), b.shape.bbox());
    if !ab.inflate(g).intersects(&bb) {
        return None;
    }
    let gap = gap();
    let need = Required { gap: g, rule: rules.pair_gap_ref() };
    short_of(gap, g).then(|| PairFault { gap, need, at: overlap_centre(ab.inflate(g), bb) })
}

/// What copper keeps its distance from besides other copper (E6 1.2). DRC's edge, hole and
/// keepout checks and the router's verifier both test through `check_to`.
#[derive(Copy, Clone, Debug)]
pub enum Obstacle<'a> {
    Copper(&'a Copper),
    /// A piece of the board edge (an outline or cutout step, width 0).
    Edge(&'a Shape),
    /// A drilled hole as its drill circle, with the item that owns it and that item's net.
    Drill { item: ConnItem, net: Option<NetId>, shape: &'a Shape },
    /// A keepout area forbidding `forbid` on the layer tested.
    Keepout { area: &'a Shape, forbid: Forbid },
}

impl Obstacle<'_> {
    pub fn shape(&self) -> &Shape {
        match self {
            Obstacle::Copper(c) => &c.shape,
            Obstacle::Edge(s) | Obstacle::Drill { shape: s, .. } | Obstacle::Keepout { area: s, .. } => s,
        }
    }
}

/// What a keepout must forbid to exclude this piece of copper.
fn forbidden_by(c: &Copper) -> Forbid {
    match c.item {
        ConnItem::Track { .. } => Forbid::TRACKS,
        ConnItem::Via(_) => Forbid::VIAS,
        ConnItem::Pad { .. } | ConnItem::Hole(_) => Forbid::PADS,
        ConnItem::Zone { .. } => Forbid::ZONES,
    }
}

/// The gap copper `a` needs from `b` on `layer`: copper pairs by the matrix (`required`), the
/// board edge by `edge`, another item's drilled hole by `hole clearance` (none from its own
/// item's or its own net's hole), and a keepout that forbids it by 1 nm (it must not touch).
pub fn required_to(m: &RuleMatrix, a: &Copper, b: &Obstacle, layer: LayerId) -> Required {
    let none = Required { gap: 0, rule: RuleRef::CLEARANCE };
    match *b {
        Obstacle::Copper(c) => required(m, a, c, layer),
        Obstacle::Edge(_) => Required { gap: m.edge(), rule: RuleRef::EDGE },
        Obstacle::Drill { item, net, .. } => {
            if same_piece_owner(a.item, item) || (a.net.is_some() && a.net == net) {
                none
            } else {
                Required { gap: m.hole_clearance(), rule: RuleRef::HOLE_CLEARANCE }
            }
        }
        Obstacle::Keepout { forbid, .. } => {
            if forbid.has(forbidden_by(a)) {
                Required { gap: 1, rule: RuleRef::KEEPOUT }
            } else {
                none
            }
        }
    }
}

/// A pad's own drill, a via's own drill, a plated hole's own hole.
fn same_piece_owner(a: ConnItem, b: ConnItem) -> bool {
    match (a, b) {
        (ConnItem::Pad { placed, pad }, ConnItem::Pad { placed: p2, pad: d2 }) => placed == p2 && pad == d2,
        (ConnItem::Via(x), ConnItem::Via(y)) | (ConnItem::Hole(x), ConnItem::Hole(y)) => x == y,
        _ => false,
    }
}

/// `Some` when copper `a` is closer to `b` on `layer` than `required_to` allows.
pub fn check_to(m: &RuleMatrix, a: &Copper, b: &Obstacle, layer: LayerId) -> Option<PairFault> {
    check_to_with(m, a, b, layer, || clearance(&a.shape, b.shape()))
}

fn check_piece_to(m: &RuleMatrix, pa: &Piece, a: &Copper, b: &Obstacle, layer: LayerId) -> Option<PairFault> {
    check_to_with(m, a, b, layer, || {
        if let Some(grid) = &pa.grid {
            // Equals clearance(&Shape::Poly(set), other), with this piece's polygon first.
            grid.clearance(b.shape())
        } else {
            clearance(&a.shape, b.shape())
        }
    })
}

fn check_to_with(
    m: &RuleMatrix,
    a: &Copper,
    b: &Obstacle,
    layer: LayerId,
    gap: impl FnOnce() -> Nm,
) -> Option<PairFault> {
    let need = required_to(m, a, b, layer);
    if need.gap == 0 {
        return None;
    }
    let (ab, bb) = (a.shape.bbox(), b.shape().bbox());
    if !ab.inflate(need.gap).intersects(&bb) {
        return None;
    }
    let gap = gap();
    short_of(gap, need.gap).then(|| PairFault { gap, need, at: overlap_centre(ab.inflate(need.gap), bb) })
}

/// `Some` when two drilled holes of different items are closer than `hole to hole`. `a` and
/// `b` are drill circles.
pub fn check_drills(m: &RuleMatrix, a: (ConnItem, &Shape), b: (ConnItem, &Shape)) -> Option<PairFault> {
    if same_piece_owner(a.0, b.0) {
        return None;
    }
    let need = Required { gap: m.hole_to_hole(), rule: RuleRef::HOLE_TO_HOLE };
    let (ab, bb) = (a.1.bbox(), b.1.bbox());
    if need.gap == 0 || !ab.inflate(need.gap).intersects(&bb) {
        return None;
    }
    let gap = clearance(a.1, b.1);
    short_of(gap, need.gap).then(|| PairFault { gap, need, at: overlap_centre(ab.inflate(need.gap), bb) })
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ItemFaultKind {
    TrackWidth,
    Drill,
    AnnularRing,
    /// A track end attached to nothing (0 the start, 1 the end).
    Dangling(u8),
    /// A track on a copper layer the stackup does not have.
    MissingLayer(LayerId),
}

/// A fault of one item on its own (no other item involved).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct ItemFault {
    pub kind: ItemFaultKind,
    pub have: Nm,
    pub need: Nm,
    pub rule: RuleRef,
    pub at: P,
}

const DANGLING_RULES: [&str; 2] = ["dangling start", "dangling end"];

/// The ends of `t` that DRC reports Dangling: anchored to nothing, and their finding not
/// waived. DRC and the router read this one definition. O(1), plus one content id per free
/// end when the board has waivers.
pub fn dangling_ends(board: &Board, t: &Track) -> [bool; 2] {
    [0, 1].map(|e| {
        if t.ends[e] != Anchor::Free { return false; }
        if board.rules.waivers.is_empty() { return true; }
        let id = content_id(board, &CheckKind::Dangling, &[Sel::Item(board.unit, t.id)], DANGLING_RULES[e]);
        !board.rules.waivers.iter().any(|w| w.violation == id)
    })
}

/// The single-item checks of `item`: track width, missing layers and dangling ends, via drill and annular ring.
pub fn check_item(m: &RuleMatrix, board: &Board, item: ItemId) -> Vec<ItemFault> {
    let mut out = Vec::new();
    match board.locate(item) {
        Some((ItemKind::Track, i)) => {
            let t = &board.tracks[i];
            let class = class_of(board, Some(t.net));
            let need = m.track_min(class);
            let have = t.widths.as_ref().and_then(|w| w.iter().min().copied()).unwrap_or(t.w).min(t.w);
            if have < need {
                out.push(ItemFault { kind: ItemFaultKind::TrackWidth, have, need, rule: m.track_ref(class), at: t.path.start });
            }
            if !board.stackup.copper_set().has(t.layer) {
                out.push(ItemFault { kind: ItemFaultKind::MissingLayer(t.layer), have: 0, need: 0, rule: m.track_ref(class), at: t.path.start });
            }
            for (e, dangling) in dangling_ends(board, t).into_iter().enumerate() {
                if dangling {
                    let p = if e == 0 { t.path.start } else { t.path.end() };
                    out.push(ItemFault { kind: ItemFaultKind::Dangling(e as u8), have: 0, need: 0, rule: RuleRef::DANGLING, at: p });
                }
            }
        }
        Some((ItemKind::Via, i)) => {
            let v = &board.vias[i];
            if v.drill < m.drill_min() {
                out.push(ItemFault { kind: ItemFaultKind::Drill, have: v.drill, need: m.drill_min(), rule: m.drill_ref(), at: v.at });
            }
            let ring = (v.pad - v.drill) / 2;
            if ring < m.annular_min() {
                out.push(ItemFault { kind: ItemFaultKind::AnnularRing, have: ring, need: m.annular_min(), rule: m.annular_ref(), at: v.at });
            }
        }
        _ => {}
    }
    out
}

/// A DRC run's findings. `region: None` is a full run; `Some(r)` replaces, in the previous list,
/// the geometric violations whose `at` lies in `r` and every connectivity violation.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DrcResult {
    pub violations: Vec<Violation>,
    pub region: Option<BBox>,
}

/// Connectivity findings are replaced in full: changed joins can affect items outside the delta's
/// region, including a via whose joined-layer count changes through a zone fill.
fn is_connectivity(k: &CheckKind) -> bool {
    matches!(k, CheckKind::Short | CheckKind::Unconnected | CheckKind::DanglingVia | CheckKind::PourNeck)
}

/// The previous list with an update folded in (matched and sorted by content id).
pub fn merge_violations(prev: &[Violation], update: &DrcResult) -> Vec<Violation> {
    let mut out: Vec<Violation> = match update.region {
        None => Vec::new(),
        Some(r) => prev
            .iter()
            .filter(|v| !is_connectivity(&v.kind) && !v.at.is_some_and(|p| !r.is_empty() && r.contains(p)))
            .cloned()
            .collect(),
    };
    out.extend(update.violations.iter().cloned());
    out.sort_by_key(|v| v.id);
    out.dedup_by_key(|v| v.id);
    out
}

/// The selection a piece of copper is reported as.
pub fn sel_of(board: &Board, c: ConnItem) -> Sel {
    match c {
        ConnItem::Pad { placed, pad } => match board.placed_item(placed) {
            Some(p) => Sel::Feature(FeatureRef::Pad(CompRef { board: board.unit, comp: p.comp }, pad)),
            None => Sel::Item(board.unit, placed),
        },
        ConnItem::Track { item, .. } | ConnItem::Zone { item, .. } | ConnItem::Via(item) | ConnItem::Hole(item) => Sel::Item(board.unit, item),
    }
}

/// The content id of a DRC finding: a hash of its kind, its rule and, for each item it names, what that item
/// is by path or geometry (never its `ItemId`), sorted. Equal on every read of the same board document and
/// for the full and the delta form of `drc`. `Waiver::violation` holds this id.
pub fn content_id(board: &Board, kind: &CheckKind, items: &[Sel], rule: &str) -> u64 {
    content_id_with_placed(board, kind, items, rule, |comp| board.placed_of(comp))
}

fn content_id_with_placed<'a>(
    board: &'a Board,
    kind: &CheckKind,
    items: &[Sel],
    rule: &str,
    placed_of: impl Fn(CompId) -> Option<&'a Placed>,
) -> u64 {
    let mut keys: Vec<Vec<u8>> = items.iter().map(|sel| {
        let mut w = Writer::bytes();
        match sel {
            Sel::Item(_, id) => match board.locate(*id) {
                Some((ItemKind::Placed, i)) => {
                    let p = &board.placed[i];
                    w.tag(0);
                    p.path.canon(&mut w);
                }
                Some((ItemKind::Track, i)) => {
                    let t = &board.tracks[i];
                    w.tag(1);
                    board.nets[t.net.ix()].name.canon(&mut w);
                    t.layer.canon(&mut w);
                    t.path.canon(&mut w);
                }
                Some((ItemKind::Via, i)) => {
                    let v = &board.vias[i];
                    w.tag(2);
                    board.nets[v.net.ix()].name.canon(&mut w);
                    v.at.canon(&mut w);
                    v.span.canon(&mut w);
                }
                Some((ItemKind::Zone, i)) => {
                    let z = &board.zones[i];
                    w.tag(3);
                    z.net.map_or("", |n| board.nets[n.ix()].name.as_str()).canon(&mut w);
                    z.layers.canon(&mut w);
                    z.area.canon(&mut w);
                }
                Some((ItemKind::Hole, i)) => {
                    let h = &board.holes[i];
                    w.tag(4);
                    h.at.canon(&mut w);
                    h.drill.canon(&mut w);
                    h.plated.canon(&mut w);
                }
                Some((ItemKind::Keepout, i)) => {
                    let k = &board.keepouts[i];
                    w.tag(5);
                    k.area.canon(&mut w);
                    k.layers.canon(&mut w);
                }
                Some((ItemKind::Cutout, i)) => {
                    let c = &board.cutouts[i];
                    w.tag(6);
                    c.contour.canon(&mut w);
                }
                Some((ItemKind::Text, i)) => {
                    let t = &board.texts[i];
                    w.tag(11);
                    t.layer.canon(&mut w);
                    t.text.canon(&mut w);
                    t.at.canon(&mut w);
                }
                Some((ItemKind::Graphic, i)) => {
                    let g = &board.graphics[i];
                    w.tag(12);
                    g.layer.canon(&mut w);
                    g.gfx.canon(&mut w);
                }
                Some(_) => {
                    w.tag(254);
                    canon_without_unit(sel, &mut w);
                }
                None => {
                    // DRC never emits a selection for an item absent from the board.
                    w.tag(255);
                    sel.canon(&mut w);
                }
            },
            Sel::Feature(FeatureRef::Pad(CompRef { comp, .. }, pad)) => {
                match placed_of(*comp).and_then(|p| p.footprint.pads.get(pad.ix()).map(|pad| (p, pad))) {
                    Some((p, pad)) => {
                        w.tag(7);
                        p.path.canon(&mut w);
                        pad.no.canon(&mut w);
                    }
                    None => {
                        w.tag(254);
                        canon_without_unit(sel, &mut w);
                    }
                }
            }
            Sel::Feature(FeatureRef::Outline(_)) => w.tag(8),
            // A part's body or one of its ports (the case checks): by the part's path.
            Sel::Feature(FeatureRef::Body(CompRef { comp, .. })) => match placed_of(*comp) {
                Some(p) => {
                    w.tag(9);
                    p.path.canon(&mut w);
                }
                None => {
                    w.tag(254);
                    canon_without_unit(sel, &mut w);
                }
            },
            Sel::Feature(FeatureRef::Port(CompRef { comp, .. }, port)) => match placed_of(*comp) {
                Some(p) => {
                    w.tag(10);
                    p.path.canon(&mut w);
                    port.canon(&mut w);
                }
                None => {
                    w.tag(254);
                    canon_without_unit(sel, &mut w);
                }
            },
            // A hole named as a feature keys as the hole item does: by its geometry.
            Sel::Feature(FeatureRef::Hole(_, id)) => match board.locate(*id) {
                Some((ItemKind::Hole, i)) => {
                    let h = &board.holes[i];
                    w.tag(4);
                    h.at.canon(&mut w);
                    h.drill.canon(&mut w);
                    h.plated.canon(&mut w);
                }
                _ => {
                    w.tag(254);
                    canon_without_unit(sel, &mut w);
                }
            },
            _ => {
                w.tag(254);
                canon_without_unit(sel, &mut w);
            }
        }
        w.into_bytes()
    }).collect();
    keys.sort();
    let mut w = Writer::hasher();
    kind.canon(&mut w);
    w.count(keys.len());
    for key in keys {
        w.raw(&key);
    }
    rule.canon(&mut w);
    let h = w.finish();
    u64::from_le_bytes(h.0[..8].try_into().unwrap())
}

/// The selection's canonical tags and fields, omitting the implied board unit.
fn canon_without_unit(sel: &Sel, w: &mut Writer) {
    match sel {
        Sel::Unit(_) => w.tag(0),
        Sel::Comp(c) => {
            w.tag(1);
            c.comp.canon(w);
        }
        Sel::Pin(p) => {
            w.tag(2);
            p.comp.canon(w);
            p.pin.canon(w);
        }
        Sel::Net(n) => {
            w.tag(3);
            n.net.canon(w);
        }
        Sel::Block(_, b) => {
            w.tag(4);
            b.canon(w);
        }
        Sel::Item(_, id) => {
            w.tag(5);
            id.canon(w);
        }
        Sel::Feature(f) => {
            w.tag(7);
            match f {
                FeatureRef::Part(c) => {
                    w.tag(0);
                    c.comp.canon(w);
                }
                FeatureRef::Pad(c, pad) => {
                    w.tag(1);
                    c.comp.canon(w);
                    pad.canon(w);
                }
                FeatureRef::Body(c) => {
                    w.tag(2);
                    c.comp.canon(w);
                }
                FeatureRef::Port(c, port) => {
                    w.tag(3);
                    c.comp.canon(w);
                    port.canon(w);
                }
                FeatureRef::Hole(_, id) => {
                    w.tag(4);
                    id.canon(w);
                }
                FeatureRef::HoleSet(_, name) => {
                    w.tag(5);
                    name.canon(w);
                }
                FeatureRef::Outline(_) => w.tag(6),
                // These features carry no unit, so their full canon is already unit-free.
                FeatureRef::Sketch(_) | FeatureRef::Face { .. } | FeatureRef::Panel(_) | FeatureRef::Generated(_) => f.canon(w),
            }
        }
        // These selections carry no unit, so their full canon is already unit-free.
        Sel::Link(_) | Sel::Joint { .. } | Sel::Solid(_) | Sel::Source { .. } => sel.canon(w),
    }
}

fn overlap_centre(a: BBox, b: BBox) -> P {
    let m = BBox { min: P::new(a.min.x.max(b.min.x), a.min.y.max(b.min.y)), max: P::new(a.max.x.min(b.max.x), a.max.y.min(b.max.y)) };
    if m.min.x <= m.max.x && m.min.y <= m.max.y { m.center() } else { b.center() }
}

fn drc_region(rules: &RuleMatrix, drill_pad_max: Nm, delta: Option<&BoardDelta>) -> (Option<BBox>, Option<BBox>) {
    let max_c = rules.max_clearance().max(rules.hole_to_hole())
        .max(rules.silk_clearance()).max(rules.max_pair_gap()).max(drill_pad_max);
    let region = match delta {
        Some(d) if !d.global => Some(if d.bbox.is_empty() { BBox::EMPTY } else { d.bbox.inflate(max_c) }),
        _ => None,
    };
    let scope = region.map(|r| if r.is_empty() { r } else { r.inflate(max_c) });
    (region, scope)
}

fn in_region(region: Option<BBox>, at: P) -> bool {
    region.map_or(true, |r| !r.is_empty() && r.contains(at))
}

fn foreign(a: &Copper, b: &Copper, sa: &Sel, sb: &Sel) -> bool {
    sa != sb && !(a.net.is_some() && a.net == b.net)
}

fn clearance_order(f: PairFault) -> (i64, P, Nm, RuleRef) {
    // Coincident faults on different layers can still differ in their measure or rule.
    (f.gap as i64 - f.need.gap as i64, f.at, f.gap, f.need.rule)
}

// The overlap point depends on which box is inflated, so use a stable drill direction too.
fn ordered_drills(m: &RuleMatrix, a: (ConnItem, &Shape), b: (ConnItem, &Shape)) -> Option<PairFault> {
    if a.0 <= b.0 { check_drills(m, a, b) } else { check_drills(m, b, a) }
}

fn clearance_violation(board: &Board, rules: &RuleMatrix, sx: &Sel, sy: &Sel, f: PairFault) -> Violation {
    Violation::new(
        CheckKind::Clearance,
        Severity::Error,
        Some(board.unit),
        vec![sx.clone(), sy.clone()],
        rules.rule_name(f.need.rule),
        format!("clearance {} mm, needs {} mm", mm3(f.gap), mm3(f.need.gap)),
    )
    .at(f.at)
    .measures(Measure::Len(f.gap), Measure::Len(f.need.gap))
    .hint(limit_hint(rules, f.need.rule, "move one of them apart, or reroute"))
}

/// The worst gap fault of two tracks belonging to a differential pair.
fn pair_gap_violation(board: &Board, rules: &RuleMatrix, sx: &Sel, sy: &Sel, f: PairFault) -> Violation {
    Violation::new(
        CheckKind::PairGap,
        Severity::Warning,
        Some(board.unit),
        vec![sx.clone(), sy.clone()],
        rules.rule_name(f.need.rule),
        format!("pair tracks {} mm apart, the pair gap is {} mm", mm3(f.gap), mm3(f.need.gap)),
    )
    .at(f.at)
    .measures(Measure::Len(f.gap), Measure::Len(f.need.gap))
    .hint("space the pair's tracks at the pair gap")
}

fn edge_violation(board: &Board, rules: &RuleMatrix, sel: &Sel, gap: Nm, at: P) -> Violation {
    Violation::new(
        CheckKind::EdgeClearance,
        Severity::Error,
        Some(board.unit),
        vec![sel.clone()],
        rules.rule_name(rules.edge_ref()),
        format!("{} mm from the board edge, needs {} mm", mm3(gap), mm3(rules.edge())),
    )
    .at(at)
    .measures(Measure::Len(gap), Measure::Len(rules.edge()))
    .hint(limit_hint(rules, rules.edge_ref(), ""))
}

fn hole_clearance_violation(board: &Board, rules: &RuleMatrix, sx: &Sel, sy: &Sel, f: PairFault) -> Violation {
    Violation::new(
        CheckKind::HoleClearance,
        Severity::Error,
        Some(board.unit),
        vec![sx.clone(), sy.clone()],
        rules.rule_name(f.need.rule),
        format!("{} mm from a drilled hole, needs {} mm", mm3(f.gap), mm3(f.need.gap)),
    )
    .at(f.at)
    .measures(Measure::Len(f.gap), Measure::Len(f.need.gap))
    .hint("move the copper away from the hole")
}

fn hole_to_hole_violation(board: &Board, rules: &RuleMatrix, sx: &Sel, sy: &Sel, f: PairFault) -> Violation {
    Violation::new(
        CheckKind::HoleToHole,
        Severity::Error,
        Some(board.unit),
        vec![sx.clone(), sy.clone()],
        rules.rule_name(f.need.rule),
        format!("holes {} mm apart, needs {} mm", mm3(f.gap), mm3(f.need.gap)),
    )
    .at(f.at)
    .measures(Measure::Len(f.gap), Measure::Len(f.need.gap))
    .hint("move one hole")
}

/// The measured gap, required gap and location when a via drill is too close to this pad.
fn via_pad_fault(
    board: &Board, rules: &RuleMatrix, drill: &Shape, drill_box: BBox, copper: &Copper, pad_box: BBox,
) -> Option<(Nm, Nm, P)> {
    let ConnItem::Pad { placed, pad } = copper.item else { return None };
    let need = rules.via_drill_to_board_pad(board, placed, pad);
    let reach = drill_box.inflate(need);
    if !reach.intersects(&pad_box) {
        return None;
    }
    let gap = clearance(drill, &copper.shape);
    // Raw on purpose: the mask web is a manufacturing limit, not a clearance with `allowed_need`'s allowance.
    (gap < need).then(|| (gap, need, overlap_centre(reach, pad_box)))
}

/// `Some` when the drill of a via of `net` is closer to `pad` than the pad's mask opening plus a
/// mask web (`RuleMatrix::via_drill_to_board_pad`) and the via's class may not sit in pads (the
/// `ViaInPad` finding). `None` for copper that is not a pad. O(1) but the exact distance.
pub fn check_via_in_pad(board: &Board, rules: &RuleMatrix, net: Option<NetId>, drill: &Shape, pad: &Copper) -> Option<PairFault> {
    if !matches!(pad.item, ConnItem::Pad { .. }) || rules.via_in_pad(class_of(board, net)) {
        return None;
    }
    let (gap, need, at) = via_pad_fault(board, rules, drill, drill.bbox(), pad, pad.shape.bbox())?;
    Some(PairFault { gap, need: Required { gap: need, rule: RuleRef::VIA_IN_PAD }, at })
}

/// A via drill inside the pad's required clearance for its mask opening and minimum mask web.
fn via_in_pad_violation(board: &Board, via: &Sel, pad: &Sel, gap: Nm, need: Nm, at: P) -> Violation {
    // Show sub-micron differences when a drill is just inside the limit.
    let mm = |nm| if nm % 1_000 == 0 { mm3(nm) } else { format!("{:.6}", f64::from(nm) / 1e6) };
    Violation::new(
        CheckKind::ViaInPad,
        Severity::Warning,
        Some(board.unit),
        vec![via.clone(), pad.clone()],
        "via in pad",
        format!("via drill {} mm from a pad, needs {} mm (the pad's mask opening plus a mask web): \
            solder wicks into the hole unless the via is filled and capped", mm(gap), mm(need)),
    )
    .at(at)
    .measures(Measure::Len(gap), Measure::Len(need))
    .hint("move the via off the pad, or fill and cap it")
}

/// The via's own class and the factory capability decide; a via the board cannot locate is checked.
fn via_in_pad_allowed(board: &Board, rules: &RuleMatrix, via: ItemId) -> bool {
    let Some((ItemKind::Via, v)) = board.locate(via) else { return false };
    rules.via_in_pad(class_of(board, Some(board.vias[v].net)))
}

fn keepout_violation(board: &Board, rules: &RuleMatrix, sx: &Sel, sy: &Sel, at: P, message: String) -> Violation {
    Violation::new(
        CheckKind::Keepout,
        Severity::Error,
        Some(board.unit),
        vec![sx.clone(), sy.clone()],
        rules.rule_name(RuleRef::KEEPOUT),
        message,
    )
    .at(at)
    .hint("move it out of the keepout")
}

fn courtyard_violation(board: &Board, a: &crate::Placed, b: &crate::Placed, at: P) -> Violation {
    Violation::new(
        CheckKind::CourtyardOverlap,
        Severity::Error,
        Some(board.unit),
        vec![Sel::Item(board.unit, a.id), Sel::Item(board.unit, b.id)],
        "courtyard",
        format!("courtyards of {} and {} overlap", a.refdes, b.refdes),
    )
    .at(at)
    .hint("move one part")
}

struct SilkFault {
    gap: Nm,
    at: P,
    what: String,
}

fn silk_violation(
    board: &Board,
    rules: &RuleMatrix,
    silk: ItemId,
    opening: ConnItem,
    f: SilkFault,
) -> Violation {
    let name = if let ConnItem::Pad { placed, pad } = opening {
        let p = board.placed_item(placed).expect("a mask opening belongs to a placed pad");
        format!("pad {} of {}", p.footprint.pads[pad.ix()].no, p.refdes)
    } else {
        "a via".into()
    };
    let c = rules.silk_clearance();
    let message = if f.gap == 0 {
        format!("{} over {name}", f.what)
    } else {
        format!("{} {} mm from {name}, needs {} mm", f.what, mm3(f.gap), mm3(c))
    };
    Violation::new(
        CheckKind::SilkOverPad,
        Severity::Warning,
        Some(board.unit),
        vec![Sel::Item(board.unit, silk), sel_of(board, opening)],
        rules.rule_name(rules.silk_clearance_ref()),
        message,
    )
    .at(f.at)
    .measures(Measure::Len(f.gap), Measure::Len(c))
    .hint("move the silk off the opening; the fab clips silk there")
}

fn item_violation(board: &Board, rules: &RuleMatrix, sel: &Sel, f: ItemFault) -> Violation {
    let (kind, message) = match f.kind {
        ItemFaultKind::TrackWidth => (
            CheckKind::TrackWidth,
            format!("track {} mm wide, needs {} mm", mm3(f.have), mm3(f.need)),
        ),
        ItemFaultKind::Drill => (
            CheckKind::Drill,
            format!("drill {} mm, needs {} mm", mm3(f.have), mm3(f.need)),
        ),
        ItemFaultKind::AnnularRing => (
            CheckKind::AnnularRing,
            format!("annular ring {} mm, needs {} mm", mm3(f.have), mm3(f.need)),
        ),
        ItemFaultKind::Dangling(e) => {
            let rule = DANGLING_RULES[usize::from(e)];
            return Violation::new(
                CheckKind::Dangling,
                Severity::Warning,
                Some(board.unit),
                vec![sel.clone()],
                rule,
                "track end attached to nothing".into(),
            )
            .at(f.at)
            .hint("end it on a pad, via or track");
        }
        ItemFaultKind::MissingLayer(layer) => {
            return Violation::new(
                CheckKind::MissingLayer,
                Severity::Error,
                Some(board.unit),
                vec![sel.clone()],
                "copper layer",
                format!("track on {}, a copper layer the stackup does not have", layer.name()),
            )
            .at(f.at)
            .hint("move it to a layer of the stackup, or add the layer back");
        }
    };
    Violation::new(kind, Severity::Error, Some(board.unit), vec![sel.clone()], rules.rule_name(f.rule), message)
        .at(f.at)
        .measures(Measure::Len(f.have), Measure::Len(f.need))
        .hint(limit_hint(rules, f.rule, ""))
}

/// Run DRC: in full (`delta: None`, or a global delta) or for the region a delta affects.
pub fn drc(
    board: &Board,
    conn: &Connectivity,
    rules: &RuleMatrix,
    fills: &[ZoneFill],
    delta: Option<&BoardDelta>,
) -> DrcResult {
    let max_c = rules.max_clearance().max(rules.hole_to_hole()).max(rules.max_pair_gap());
    let drill_pad_max = rules.max_via_drill_to_pad(board);
    let (region, scope) = drc_region(rules, drill_pad_max, delta);
    let in_scope = |b: &BBox| scope.is_none_or_box(b);
    let at_ok = |p: P| in_region(region, p);
    let pcs = pieces(board, fills);
    // Fill polygons carry many vertices: every box is taken once.
    let boxes: Vec<BBox> = pcs.iter().map(|c| c.shape.bbox()).collect();
    let sels: Vec<Sel> = pcs.iter().map(|c| sel_of(board, c.item)).collect();
    let mut by_sel: HashMap<&Sel, Vec<usize>> = HashMap::new();
    for (i, s) in sels.iter().enumerate() {
        by_sel.entry(s).or_default().push(i);
    }
    let mut out: Vec<Violation> = Vec::new();

    // copper clearance between items of different nets
    let mut per_layer: HashMap<fab_elec_core::LayerId, Vec<(BBox, u32)>> = HashMap::new();
    for (i, c) in pcs.iter().enumerate() {
        per_layer.entry(c.layer).or_default().push((boxes[i], i as u32));
    }
    let layer_index: HashMap<LayerId, Index<u32>> =
        per_layer.into_iter().map(|(l, v)| (l, Index::build(v))).collect();
    let mut pairs: Vec<(&Sel, &Sel)> = Vec::new();
    let mut pair_gap_pairs: Vec<(&Sel, &Sel)> = Vec::new();
    for (i, a) in pcs.iter().enumerate() {
        let ab = boxes[i];
        if !in_scope(&ab) {
            continue;
        }
        layer_index[&a.layer].query(ab.inflate(max_c), |bb, j| {
            let j = j as usize;
            let b = &pcs[j];
            if j > i
                && in_scope(&bb)
                && foreign(a, b, &sels[i], &sels[j])
                && check_pair(rules, a, b, a.layer).is_some()
            {
                let (x, y) = if sels[i] <= sels[j] { (&sels[i], &sels[j]) } else { (&sels[j], &sels[i]) };
                pairs.push((x, y));
            }
            if j > i && in_scope(&bb)
                && check_pair_gap_with(rules, a, b, || clearance(&a.shape, &b.shape)).is_some()
            {
                let (x, y) = if sels[i] <= sels[j] { (&sels[i], &sels[j]) } else { (&sels[j], &sels[i]) };
                pair_gap_pairs.push((x, y));
            }
            ControlFlow::Continue(())
        });
    }
    pairs.sort();
    pairs.dedup();
    for (sx, sy) in pairs {
        // the worst piece pair of the two items decides the finding; pieces further apart than
        // any clearance cannot be closer than theirs
        let mut worst: Option<PairFault> = None;
        for &i in &by_sel[sx] {
            let reach = boxes[i].inflate(max_c);
            for &j in &by_sel[sy] {
                let (a, b) = (&pcs[i], &pcs[j]);
                if a.layer != b.layer || !reach.intersects(&boxes[j]) || !foreign(a, b, &sels[i], &sels[j]) {
                    continue;
                }
                if let Some(f) = check_pair(rules, a, b, a.layer) {
                    if worst.map_or(true, |w| clearance_order(f) < clearance_order(w)) {
                        worst = Some(f);
                    }
                }
            }
        }
        if let Some(f) = worst {
            if at_ok(f.at) {
                out.push(clearance_violation(board, rules, sx, sy, f));
            }
        }
    }

    pair_gap_pairs.sort();
    pair_gap_pairs.dedup();
    for (sx, sy) in pair_gap_pairs {
        // All segments choose the same worst fault in full and regional runs.
        let mut worst: Option<PairFault> = None;
        for &i in &by_sel[sx] {
            for &j in &by_sel[sy] {
                let (a, b) = (&pcs[i], &pcs[j]);
                if let Some(f) = check_pair_gap_with(rules, a, b, || clearance(&a.shape, &b.shape)) {
                    if worst.map_or(true, |w| clearance_order(f) < clearance_order(w)) {
                        worst = Some(f);
                    }
                }
            }
        }
        if let Some(f) = worst {
            if at_ok(f.at) {
                out.push(pair_gap_violation(board, rules, sx, sy, f));
            }
        }
    }

    // edge clearance against the outline and cutouts
    let mut edge_shapes: Vec<Shape> = Vec::new();
    for c in board.outline.iter().map(|c| &c.path).chain(board.cutouts.iter().map(|c| &c.contour.path)) {
        for (a, seg) in c.steps() {
            edge_shapes.push(match seg {
                Seg::Line(b) => Shape::Segment { a, b, w: 0 },
                Seg::Arc { mid, end } => Shape::Arc { a, mid, b: end, w: 0 },
            });
        }
    }
    let edge_index = Index::build(edge_shapes.iter().enumerate().map(|(i, s)| (s.bbox(), i as u32)).collect());
    let edge = rules.edge();
    let piece_edge = |c: &Copper, cb: BBox| -> Option<Nm> {
        let mut min: Option<Nm> = None;
        edge_index.query(cb.inflate(edge), |_, k| {
            if let Some(f) = check_to(rules, c, &Obstacle::Edge(&edge_shapes[k as usize]), c.layer) {
                if min.map_or(true, |m| f.gap < m) {
                    min = Some(f.gap);
                }
            }
            ControlFlow::Continue(())
        });
        min
    };
    let mut edge_items: Vec<&Sel> = pcs
        .iter()
        .enumerate()
        .filter(|(i, c)| in_scope(&boxes[*i]) && piece_edge(c, boxes[*i]).is_some())
        .map(|(i, _)| &sels[i])
        .collect();
    edge_items.sort();
    edge_items.dedup();
    for s in edge_items {
        let mut worst: Option<(Nm, P)> = None;
        for &i in &by_sel[s] {
            if let Some(g) = piece_edge(&pcs[i], boxes[i]) {
                let at = boxes[i].center();
                if worst.map_or(true, |w| (g, at) < w) {
                    worst = Some((g, at));
                }
            }
        }
        if let Some((gap, at)) = worst {
            if at_ok(at) {
                out.push(edge_violation(board, rules, s, gap, at));
            }
        }
    }

    // drilled holes, shared by the copper-to-hole and hole-to-hole checks
    let drills: Vec<(ConnItem, Shape, Option<NetId>)> =
        board.drills().into_iter().map(|h| (h.item, h.shape(), h.net)).collect();
    let drill_sels: Vec<Sel> = drills.iter().map(|(item, _, _)| sel_of(board, *item)).collect();
    let mut drills_by_sel: HashMap<&Sel, Vec<usize>> = HashMap::new();
    for (i, s) in drill_sels.iter().enumerate() {
        drills_by_sel.entry(s).or_default().push(i);
    }
    let drill_boxes: Vec<BBox> = drills.iter().map(|(_, s, _)| s.bbox()).collect();
    let drill_index: Index<u32> = Index::build(drill_boxes.iter().enumerate().map(|(i, b)| (*b, i as u32)).collect());

    // Via drills inside a pad's required gap, keeping the smallest gap across spanned layers.
    let copper = board.copper_set();
    let mut via_in_pad_pairs = Vec::new();
    for (i, (item, shape, _)) in drills.iter().enumerate() {
        let ConnItem::Via(id) = item else { continue };
        let bb = drill_boxes[i];
        if !in_scope(&bb) {
            continue;
        }
        let Some((ItemKind::Via, v)) = board.locate(*id) else { continue };
        if via_in_pad_allowed(board, rules, *id) {
            continue;
        }
        let reach = bb.inflate(drill_pad_max);
        for layer in board.vias[v].span_layers(copper).iter() {
            let Some(index) = layer_index.get(&layer) else { continue };
            index.query(reach, |pad_box, j| {
                let j = j as usize;
                if let Some(fault) = via_pad_fault(board, rules, shape, bb, &pcs[j], pad_box) {
                    via_in_pad_pairs.push((&drill_sels[i], &sels[j], fault));
                }
                ControlFlow::Continue(())
            });
        }
    }
    via_in_pad_pairs.sort();
    via_in_pad_pairs.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    for (via, pad, (gap, need, at)) in via_in_pad_pairs {
        if at_ok(at) {
            out.push(via_in_pad_violation(board, via, pad, gap, need, at));
        }
    }

    // copper clearance from another item's drilled hole
    let mut pairs: Vec<(&Sel, &Sel)> = Vec::new();
    for (i, c) in pcs.iter().enumerate() {
        let cb = boxes[i];
        if !in_scope(&cb) {
            continue;
        }
        drill_index.query(cb.inflate(rules.hole_clearance()), |_, j| {
            let j = j as usize;
            let (item, shape, net) = &drills[j];
            if check_to(rules, c, &Obstacle::Drill { item: *item, net: *net, shape }, c.layer).is_some() {
                let (x, y) = if sels[i] <= drill_sels[j] {
                    (&sels[i], &drill_sels[j])
                } else {
                    (&drill_sels[j], &sels[i])
                };
                pairs.push((x, y));
            }
            ControlFlow::Continue(())
        });
    }
    pairs.sort();
    pairs.dedup();
    for (sx, sy) in pairs {
        // Check every piece in both directions so a delta chooses the same worst as a full run.
        let mut worst: Option<PairFault> = None;
        for (copper_sel, drill_sel) in [(sx, sy), (sy, sx)] {
            let (Some(copper), Some(holes)) = (by_sel.get(copper_sel), drills_by_sel.get(drill_sel)) else { continue };
            for &i in copper {
                let reach = boxes[i].inflate(rules.hole_clearance());
                for &j in holes {
                    if !reach.intersects(&drill_boxes[j]) {
                        continue;
                    }
                    let c = &pcs[i];
                    let (item, shape, net) = &drills[j];
                    if let Some(f) = check_to(rules, c, &Obstacle::Drill { item: *item, net: *net, shape }, c.layer) {
                        if worst.map_or(true, |w| (f.gap, f.at) < (w.gap, w.at)) {
                            worst = Some(f);
                        }
                    }
                }
            }
        }
        if let Some(f) = worst {
            if at_ok(f.at) {
                out.push(hole_clearance_violation(board, rules, sx, sy, f));
            }
        }
    }

    // spacing between drilled holes
    let mut pairs: Vec<(&Sel, &Sel, PairFault)> = Vec::new();
    for (i, (item, circle, _)) in drills.iter().enumerate() {
        let ab = circle.bbox();
        if !in_scope(&ab) {
            continue;
        }
        drill_index.query(ab.inflate(rules.hole_to_hole()), |bb, j| {
            let j = j as usize;
            if j > i && in_scope(&bb) {
                let (other, shape, _) = &drills[j];
                if let Some(f) = ordered_drills(rules, (*item, circle), (*other, shape)) {
                    let (x, y) = if drill_sels[i] <= drill_sels[j] {
                        (&drill_sels[i], &drill_sels[j])
                    } else {
                        (&drill_sels[j], &drill_sels[i])
                    };
                    pairs.push((x, y, f));
                }
            }
            ControlFlow::Continue(())
        });
    }
    pairs.sort_by_key(|(sx, sy, f)| (*sx, *sy, f.gap, f.at));
    pairs.dedup_by(|a, b| (a.0, a.1) == (b.0, b.1));
    for (sx, sy, f) in pairs {
        if at_ok(f.at) {
            out.push(hole_to_hole_violation(board, rules, sx, sy, f));
        }
    }

    let keepouts = board_keepouts(board);
    let keepout_sels: Vec<Sel> = keepouts.iter().map(|(id, ..)| Sel::Item(board.unit, *id)).collect();
    let mut keepouts_by_sel: HashMap<&Sel, Vec<usize>> = HashMap::new();
    for (i, s) in keepout_sels.iter().enumerate() {
        keepouts_by_sel.entry(s).or_default().push(i);
    }
    let keepout_boxes: Vec<BBox> = keepouts.iter().map(|(_, area, ..)| area.bbox()).collect();
    let keepout_index = Index::build(keepout_boxes.iter().enumerate().map(|(i, b)| (*b, i as u32)).collect());
    let mut pairs: Vec<(&Sel, &Sel)> = Vec::new();
    for (i, c) in pcs.iter().enumerate() {
        let cb = boxes[i];
        if !in_scope(&cb) {
            continue;
        }
        keepout_index.query(cb.inflate(1), |_, j| {
            let j = j as usize;
            let (owner, area, layers, forbid, _, z) = &keepouts[j];
            if c.item.item() != *owner
                && layers.has(c.layer)
                && reaches_copper(*z)
                && check_to(rules, c, &Obstacle::Keepout { area, forbid: *forbid }, c.layer).is_some()
            {
                pairs.push((&sels[i], &keepout_sels[j]));
            }
            ControlFlow::Continue(())
        });
    }
    pairs.sort();
    pairs.dedup();
    for (sx, sy) in pairs {
        // All pieces and owned keepouts choose the same fault in a full or delta run.
        let mut worst: Option<PairFault> = None;
        for &i in &by_sel[sx] {
            let c = &pcs[i];
            let reach = boxes[i].inflate(1);
            for &j in &keepouts_by_sel[sy] {
                let (owner, area, layers, forbid, _, z) = &keepouts[j];
                if c.item.item() == *owner
                    || !layers.has(c.layer)
                    || !reaches_copper(*z)
                    || !reach.intersects(&keepout_boxes[j])
                {
                    continue;
                }
                if let Some(f) = check_to(rules, c, &Obstacle::Keepout { area, forbid: *forbid }, c.layer) {
                    if worst.map_or(true, |w| (f.gap, f.at) < (w.gap, w.at)) {
                        worst = Some(f);
                    }
                }
            }
        }
        if let Some(f) = worst {
            if at_ok(f.at) {
                out.push(keepout_violation(board, rules, sx, sy, f.at, KEEPOUT_MESSAGE.into()));
            }
        }
    }

    board_checks(board, rules, &keepouts, scope, region, &mut out);
    connectivity_checks(board, conn, &mut out);
    let zone_polys = |zone: ItemId| {
        pcs.iter().filter_map(move |c| match (c.item, &c.shape) {
            (ConnItem::Zone { item, .. }, Shape::Poly(poly)) if item == zone => Some((c.item, poly)),
            _ => None,
        })
    };
    pour_neck_checks(board, conn, zone_polys, &mut out);
    finish_violations(board, &mut out);
    DrcResult { violations: out, region }
}

/// DRC after `delta` over the piece index (already updated to `board` and the current fills),
/// folded into `prev` (the previous findings): the merged list. Geometric checks query the index
/// around the delta's region instead of rebuilding pieces; connectivity findings come from `conn`
/// in full. While a zone is `stale`, its fill islands take part in no geometric check and every
/// previous geometric finding that names one of its islands (or the zone) is carried over unchanged.
/// With no stale zone it equals `merge_violations(prev, &drc(board, conn, rules, fills, Some(delta)))`,
/// which equals the full `drc`.
///
/// Geometry costs O(k log n) for the k pieces in scope and the pieces of their selections, plus
/// exact candidate checks; a regional update never builds a full piece list. A piece in scope (a fill
/// tile's polygon can reach far beyond it) checks only the drills and keepouts that meet the scope: their
/// faults lie in their boxes, so the others have no finding in the region. Board-only checks,
/// connectivity findings and content-id lookups have the same costs as in `drc`.
pub fn drc_indexed(
    prev: &[Violation],
    index: &PieceIndex,
    board: &Board,
    conn: &Connectivity,
    rules: &RuleMatrix,
    delta: &BoardDelta,
    stale: &[ItemId],
) -> Vec<Violation> {
    let stale: BTreeSet<ItemId> = stale.iter().copied().collect();
    let drill_pad_max = rules.max_via_drill_to_pad(board);
    let (region, scope) = drc_region(rules, drill_pad_max, Some(delta));
    let max_c = rules.max_clearance().max(rules.hole_to_hole()).max(rules.max_pair_gap());
    let mut clearance_pairs = BTreeSet::new();
    let mut pair_gap_pairs = BTreeSet::new();
    let mut edge_items = BTreeSet::new();
    let mut hole_pairs = BTreeSet::new();
    let mut drill_pairs = Vec::new();
    let mut via_in_pad_pairs = Vec::new();
    let mut keepout_pairs = BTreeSet::new();

    visit_index(index, scope, |piece| {
        if let Some(a) = active_copper(piece, &stale) {
            let sa = PieceSel::new(board, a.item);
            index.query(a.layer, piece.bbox.inflate(max_c.max(1)), |other| {
                match &other.kind {
                    PieceKind::Copper(_) | PieceKind::Fill(_) => {
                        if piece.key() < other.key() && scope.is_none_or_box(&other.bbox) {
                            if let Some(b) = active_copper(other, &stale) {
                                let sb = PieceSel::new(board, b.item);
                                if foreign(a, b, &sa.sel, &sb.sel)
                                    && check_piece_pair(rules, piece, a, other, b, a.layer).is_some()
                                {
                                    clearance_pairs.insert(ordered_sels(sa.clone(), sb.clone()));
                                }
                                if check_pair_gap_with(rules, a, b, || piece_clearance(piece, a, other, b))
                                    .is_some()
                                {
                                    pair_gap_pairs.insert(ordered_sels(sa.clone(), sb));
                                }
                            }
                        }
                    }
                    // An offered ring is not copper until a fill reaches it (then it is `Copper`).
                    PieceKind::Offer(_) => {}
                    PieceKind::Edge(_) => {
                        if check_piece_to(rules, piece, a, &other.obstacle(), a.layer).is_some() {
                            edge_items.insert(sa.clone());
                        }
                    }
                    // A drill's or keepout's fault lies in its box: one that misses the scope has
                    // no finding in the region.
                    PieceKind::Drill { item, .. } => {
                        if scope.is_none_or_box(&other.bbox)
                            && check_piece_to(rules, piece, a, &other.obstacle(), a.layer).is_some()
                        {
                            hole_pairs.insert(ordered_sels(sa.clone(), PieceSel::new(board, *item)));
                        }
                    }
                    PieceKind::Keepout { .. } => {
                        if let Some(owner) = other.item {
                            if a.item.item() != owner
                                && other.layers.has(a.layer)
                                && scope.is_none_or_box(&other.bbox)
                                && check_piece_to(rules, piece, a, &other.obstacle(), a.layer).is_some()
                            {
                                let sb = PieceSel { sel: Sel::Item(board.unit, owner), item: owner };
                                keepout_pairs.insert((sa.clone(), sb));
                            }
                        }
                    }
                }
                ControlFlow::Continue(())
            });
        } else if let PieceKind::Drill { item, shape, .. } = &piece.kind {
            if matches!(item, ConnItem::Via(id) if !via_in_pad_allowed(board, rules, *id)) {
                let reach = piece.bbox.inflate(drill_pad_max);
                for layer in piece.layers.iter() {
                    index.query(layer, reach, |other| {
                        if let Some(pad) = active_copper(other, &stale) {
                            if let Some(fault) = via_pad_fault(board, rules, shape, piece.bbox, pad, other.bbox) {
                                via_in_pad_pairs.push((sel_of(board, *item), sel_of(board, pad.item), fault));
                            }
                        }
                        ControlFlow::Continue(())
                    });
                }
            }
            visit_index(index, Some(piece.bbox.inflate(rules.hole_to_hole())), |other| {
                if piece.key() >= other.key() || !scope.is_none_or_box(&other.bbox) {
                    return;
                }
                if let PieceKind::Drill { item: other_item, shape: other_shape, .. } = &other.kind {
                    if let Some(f) = ordered_drills(rules, (*item, shape), (*other_item, other_shape)) {
                        let (sx, sy) = ordered_sels(
                            PieceSel::new(board, *item),
                            PieceSel::new(board, *other_item),
                        );
                        drill_pairs.push((sx, sy, f));
                    }
                }
            });
        }
    });

    let mut out = Vec::new();
    for (sx, sy) in clearance_pairs {
        let mut worst: Option<PairFault> = None;
        for (pa, a) in sx.copper(index, board, &stale) {
            let reach = pa.bbox.inflate(max_c);
            for (pb, b) in sy.copper(index, board, &stale) {
                if a.layer != b.layer || !reach.intersects(&pb.bbox) || !foreign(a, b, &sx.sel, &sy.sel) {
                    continue;
                }
                if let Some(f) = check_piece_pair(rules, pa, a, pb, b, a.layer) {
                    if worst.map_or(true, |w| clearance_order(f) < clearance_order(w)) {
                        worst = Some(f);
                    }
                }
            }
        }
        if let Some(f) = worst {
            if in_region(region, f.at) {
                out.push(clearance_violation(board, rules, &sx.sel, &sy.sel, f));
            }
        }
    }

    for (sx, sy) in pair_gap_pairs {
        let mut worst: Option<PairFault> = None;
        for (pa, a) in sx.copper(index, board, &stale) {
            for (pb, b) in sy.copper(index, board, &stale) {
                if let Some(f) = check_pair_gap_with(rules, a, b, || piece_clearance(pa, a, pb, b)) {
                    if worst.map_or(true, |w| clearance_order(f) < clearance_order(w)) {
                        worst = Some(f);
                    }
                }
            }
        }
        if let Some(f) = worst {
            if in_region(region, f.at) {
                out.push(pair_gap_violation(board, rules, &sx.sel, &sy.sel, f));
            }
        }
    }

    for sel in edge_items {
        let mut worst: Option<(Nm, P)> = None;
        for (piece, c) in sel.copper(index, board, &stale) {
            let cb = piece.bbox;
            index.query(c.layer, cb.inflate(rules.edge()), |edge| {
                if matches!(&edge.kind, PieceKind::Edge(_)) {
                    if let Some(f) = check_piece_to(rules, piece, c, &edge.obstacle(), c.layer) {
                        let finding = (f.gap, cb.center());
                        if worst.map_or(true, |w| finding < w) {
                            worst = Some(finding);
                        }
                    }
                }
                ControlFlow::Continue(())
            });
        }
        if let Some((gap, at)) = worst {
            if in_region(region, at) {
                out.push(edge_violation(board, rules, &sel.sel, gap, at));
            }
        }
    }

    for (sx, sy) in hole_pairs {
        let mut worst: Option<PairFault> = None;
        for (copper_sel, drill_sel) in [(&sx, &sy), (&sy, &sx)] {
            for (copper_piece, c) in copper_sel.copper(index, board, &stale) {
                let cb = copper_piece.bbox;
                let reach = cb.inflate(rules.hole_clearance());
                for piece in index.pieces(drill_sel.item) {
                    let PieceKind::Drill { item, .. } = &piece.kind else { continue };
                    if !reach.intersects(&piece.bbox) || sel_of(board, *item) != drill_sel.sel {
                        continue;
                    }
                    if let Some(f) = check_piece_to(rules, copper_piece, c, &piece.obstacle(), c.layer) {
                        if worst.map_or(true, |w| (f.gap, f.at) < (w.gap, w.at)) {
                            worst = Some(f);
                        }
                    }
                }
            }
        }
        if let Some(f) = worst {
            if in_region(region, f.at) {
                out.push(hole_clearance_violation(board, rules, &sx.sel, &sy.sel, f));
            }
        }
    }

    drill_pairs.sort_by(|a, b| (&a.0, &a.1, a.2.gap, a.2.at).cmp(&(&b.0, &b.1, b.2.gap, b.2.at)));
    drill_pairs.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    for (sx, sy, f) in drill_pairs {
        if in_region(region, f.at) {
            out.push(hole_to_hole_violation(board, rules, &sx.sel, &sy.sel, f));
        }
    }

    via_in_pad_pairs.sort();
    via_in_pad_pairs.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    for (via, pad, (gap, need, at)) in via_in_pad_pairs {
        if in_region(region, at) {
            out.push(via_in_pad_violation(board, &via, &pad, gap, need, at));
        }
    }

    for (sx, sy) in keepout_pairs {
        let mut worst: Option<PairFault> = None;
        for (piece, c) in sx.copper(index, board, &stale) {
            let cb = piece.bbox;
            let reach = cb.inflate(1);
            for keepout in index.pieces(sy.item) {
                if !matches!(&keepout.kind, PieceKind::Keepout { .. })
                    || c.item.item() == sy.item
                    || !keepout.layers.has(c.layer)
                    || !reach.intersects(&keepout.bbox)
                {
                    continue;
                }
                if let Some(f) = check_piece_to(rules, piece, c, &keepout.obstacle(), c.layer) {
                    if worst.map_or(true, |w| (f.gap, f.at) < (w.gap, w.at)) {
                        worst = Some(f);
                    }
                }
            }
        }
        if let Some(f) = worst {
            if in_region(region, f.at) {
                out.push(keepout_violation(board, rules, &sx.sel, &sy.sel, f.at, KEEPOUT_MESSAGE.into()));
            }
        }
    }

    board_checks(board, rules, &board_keepouts(board), scope, region, &mut out);
    connectivity_checks(board, conn, &mut out);
    let zone_polys = |zone: ItemId| {
        index.pieces(zone).filter_map(|piece| piece.copper()).filter_map(|c| match &c.shape {
            Shape::Poly(poly) => Some((c.item, poly)),
            _ => None,
        })
    };
    pour_neck_checks(board, conn, zone_polys, &mut out);
    finish_violations(board, &mut out);
    let mut merged = merge_violations(prev, &DrcResult { violations: out, region });
    if !stale.is_empty() {
        // A fill polygon's selection is its zone's (`sel_of`).
        let stale_sels: BTreeSet<Sel> = stale.iter().map(|&zone| Sel::Item(board.unit, zone)).collect();
        let held = |v: &Violation| {
            !is_connectivity(&v.kind) && v.items.iter().any(|sel| stale_sels.contains(sel))
        };
        merged.retain(|v| !held(v));
        merged.extend(prev.iter().filter(|v| held(v)).cloned());
        merged.sort_by_key(|v| v.id);
        merged.dedup_by_key(|v| v.id);
    }
    merged
}

// Queries cannot lend their pieces to the caller; retain just the keys, then borrow by rank.
// A region visits only its hits, deduplicating drills and edges present on several layers.
fn visit_index(index: &PieceIndex, scope: Option<BBox>, mut visit: impl FnMut(&Piece)) {
    let Some(area) = scope else {
        for piece in index.iter() {
            visit(piece);
        }
        return;
    };
    if area.is_empty() {
        return;
    }
    let mut keys: BTreeSet<PKey> = BTreeSet::new();
    for &layer in index.layers() {
        index.query(layer, area, |piece| {
            keys.insert(piece.key());
            ControlFlow::Continue(())
        });
    }
    for key in keys {
        if let Some(piece) = index.get(key) {
            visit(piece);
        }
    }
}

fn active_copper<'a>(piece: &'a Piece, stale: &BTreeSet<ItemId>) -> Option<&'a Copper> {
    match &piece.kind {
        PieceKind::Fill(c) if stale.contains(&c.item.item()) => None,
        _ => piece.copper(),
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PieceSel {
    sel: Sel,
    item: ItemId,
}

impl PieceSel {
    fn new(board: &Board, item: ConnItem) -> Self {
        Self { sel: sel_of(board, item), item: item.item() }
    }

    // The active copper of the selection, with its indexed pieces.
    fn copper<'a>(
        &'a self,
        index: &'a PieceIndex,
        board: &'a Board,
        stale: &'a BTreeSet<ItemId>,
    ) -> impl Iterator<Item = (&'a Piece, &'a Copper)> {
        index
            .pieces(self.item)
            .filter_map(move |piece| Some((piece, active_copper(piece, stale)?)))
            .filter(move |(_, c)| sel_of(board, c.item) == self.sel)
    }
}

fn ordered_sels(a: PieceSel, b: PieceSel) -> (PieceSel, PieceSel) {
    if a <= b { (a, b) } else { (b, a) }
}

// Owner, area, copper layers, what it forbids, the side of a footprint's keepout, the height band.
type DrcKeepout = (ItemId, Shape, LayerSet, Forbid, Option<Side>, Option<(Nm, Nm)>);

const KEEPOUT_MESSAGE: &str = "inside a keepout that forbids it";

/// Does a keepout with height band `z` catch copper? Copper lies on the board surface, so a band
/// that starts above it (a light pipe's) forbids bodies only. `None` is every height.
pub fn reaches_copper(z: Option<(Nm, Nm)>) -> bool {
    z.map_or(true, |(lo, _)| lo <= 0)
}

/// Do two height bands overlap with positive length? A missing band is every height, so an
/// unknown body or an unbounded keepout never hides a breach.
pub fn bands_overlap(a: Option<(Nm, Nm)>, b: Option<(Nm, Nm)>) -> bool {
    match (a, b) {
        (Some((a0, a1)), Some((b0, b1))) => a0.max(b0) < a1.min(b1),
        _ => true,
    }
}

fn board_keepouts(board: &Board) -> Vec<DrcKeepout> {
    let mut keepouts: Vec<_> = board
        .keepouts
        .iter()
        .map(|k| (k.id, Shape::Poly(k.area.clone()), k.layers, k.forbid, None, k.z))
        .collect();
    let copper = board.copper_set();
    let copper_count = board.copper_count();
    for p in &board.placed {
        for k in &p.footprint.keepouts {
            let layers = copper
                .iter()
                .filter(|l| k.layers.has(p.fp_layer(*l, copper_count)))
                .fold(LayerSet::EMPTY, |s, l| s.with(l));
            keepouts.push((p.id, p.shape_to_board(&Shape::Poly(k.area.clone())), layers, k.forbid, Some(p.side), None));
        }
    }
    keepouts
}

fn board_checks(
    board: &Board,
    rules: &RuleMatrix,
    keepouts: &[DrcKeepout],
    scope: Option<BBox>,
    region: Option<BBox>,
    out: &mut Vec<Violation>,
) {
    silk_checks(board, rules, scope, region, out);
    let in_scope = |b: &BBox| scope.is_none_or_box(b);
    let at_ok = |p: P| in_region(region, p);
    // Nonempty courtyards in board coordinates, indexed for both placement checks.
    let courtyards: Vec<_> = board
        .placed
        .iter()
        .filter_map(|p| {
            // The placement box contains the courtyard, so rejecting it cannot discard an in-scope courtyard.
            if p.footprint.courtyard.is_empty() || !in_scope(&p.bbox()) {
                return None;
            }
            let Shape::Poly(area) = p.shape_to_board(&Shape::Poly(p.footprint.courtyard.clone())) else { return None };
            let bb = area.bbox();
            (!area.is_empty() && in_scope(&bb)).then_some((p, area, bb))
        })
        .collect();
    let courtyard_index = Index::build(courtyards.iter().enumerate().map(|(i, (_, _, bb))| (*bb, i as u32)).collect());

    // Keepouts that forbid other footprints on their side use positive courtyard area, and a
    // positive overlap of the body's height band with the keepout's.
    let mut pairs: Vec<(Sel, Sel, P, String)> = Vec::new();
    for (owner, area, layers, forbid, side, z) in keepouts {
        if !forbid.has(Forbid::FOOTPRINTS) {
            continue;
        }
        let Shape::Poly(area) = area else { continue };
        courtyard_index.query(area.bbox(), |_, j| {
            let (p, courtyard, _) = &courtyards[j as usize];
            let same_side = side.map_or_else(
                || layers.has(if p.side == Side::Top { LayerId::F_CU } else { LayerId::B_CU }),
                |s| s == p.side,
            );
            if p.id != *owner && same_side && bands_overlap(*z, p.body_z) {
                let overlap = courtyard.intersection(area);
                if !overlap.is_empty() && overlap.area() > 0.0 {
                    let message = match (p.body_z, *z) {
                        (Some((b0, b1)), Some((k0, k1))) => format!(
                            "body {}-{} mm high reaches into the keepout's {}-{} mm band",
                            mm3(b0),
                            mm3(b1),
                            mm3(k0),
                            mm3(k1)
                        ),
                        _ => KEEPOUT_MESSAGE.into(),
                    };
                    pairs.push((Sel::Item(board.unit, p.id), Sel::Item(board.unit, *owner), overlap.bbox().center(), message));
                }
            }
            ControlFlow::Continue(())
        });
    }
    pairs.sort();
    pairs.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    for (sx, sy, at, message) in pairs {
        if at_ok(at) {
            out.push(keepout_violation(board, rules, &sx, &sy, at, message));
        }
    }

    // Same-side courtyard overlaps; touching edges alone are legal.
    let mut pairs = Vec::new();
    for (i, (a, area, bb)) in courtyards.iter().enumerate() {
        courtyard_index.query(*bb, |_, j| {
            let j = j as usize;
            let (b, other, _) = &courtyards[j];
            if j > i && a.side == b.side {
                let overlap = area.intersection(other);
                if !overlap.is_empty() && overlap.area() > 0.0 {
                    let (a, b) = if a.id <= b.id { (*a, *b) } else { (*b, *a) };
                    pairs.push((a, b, overlap.bbox().center()));
                }
            }
            ControlFlow::Continue(())
        });
    }
    pairs.sort_by_key(|(a, b, at)| (a.id, b.id, *at));
    pairs.dedup_by_key(|(a, b, _)| (a.id, b.id));
    for (a, b, at) in pairs {
        if at_ok(at) {
            out.push(courtyard_violation(board, a, b, at));
        }
    }

    // single-item checks: track widths, missing layers and dangling ends, via drill and annular ring
    let items = board
        .tracks
        .iter()
        .filter(|t| in_scope(&t.bbox()))
        .map(|t| t.id)
        .chain(board.vias.iter().map(|v| v.id));
    for id in items {
        let sel = Sel::Item(board.unit, id);
        for f in check_item(rules, board, id) {
            if !at_ok(f.at) {
                continue;
            }
            out.push(item_violation(board, rules, &sel, f));
        }
    }
}

fn silk_checks(
    board: &Board,
    rules: &RuleMatrix,
    scope: Option<BBox>,
    region: Option<BBox>,
    out: &mut Vec<Violation>,
) {
    let in_scope = |b: &BBox| scope.is_none_or_box(b);
    let sides = [Side::Top, Side::Bottom];
    let mut openings: [Vec<(ConnItem, Shape)>; 2] = [Vec::new(), Vec::new()];
    for p in &board.placed {
        if !in_scope(&p.bbox().inflate(p.mask_growth(board))) {
            continue;
        }
        for i in 0..p.footprint.pads.len() {
            let pad = PadIx(i as u16);
            for (s, side) in sides.into_iter().enumerate() {
                if let Some(shape) = board.mask_opening(p.id, pad, side) {
                    if in_scope(&shape.bbox()) {
                        openings[s].push((ConnItem::Pad { placed: p.id, pad }, shape));
                    }
                }
            }
        }
    }
    let margin = board.mask_margin().max(0);
    for v in &board.vias {
        if !in_scope(&BBox::point(v.at).inflate(v.pad.max(v.drill) / 2 + 1 + margin)) {
            continue;
        }
        for (s, side) in sides.into_iter().enumerate() {
            if let Some(shape) = board.via_mask_opening(v.id, side) {
                if in_scope(&shape.bbox()) {
                    openings[s].push((ConnItem::Via(v.id), shape));
                }
            }
        }
    }
    let indices = openings.each_ref().map(|side| {
        Index::build(side.iter().enumerate().map(|(i, (_, shape))| (shape.bbox(), i as u32)).collect())
    });
    let c = rules.silk_clearance();
    let mut worst: HashMap<(ItemId, ConnItem), SilkFault> = HashMap::new();
    let mut check = |owner: ItemId, layer: LayerId, what: &str, shape: Shape| {
        let s = match layer {
            LayerId::F_SILK => 0,
            LayerId::B_SILK => 1,
            _ => return,
        };
        let bb = shape.bbox();
        if !in_scope(&bb) {
            return;
        }
        indices[s].query(bb.inflate(c.max(1)), |ob, i| {
            let (opening, other) = &openings[s][i as usize];
            let gap = clearance(&shape, other);
            if gap == 0 || gap < c {
                let at = overlap_centre(bb.inflate(c), ob);
                let key = (owner, *opening);
                if worst.get(&key).map_or(true, |w| (gap, at) < (w.gap, w.at)) {
                    worst.insert(key, SilkFault { gap, at, what: what.into() });
                }
            }
            ControlFlow::Continue(())
        });
    };
    let is_silk = |l| matches!(l, LayerId::F_SILK | LayerId::B_SILK);
    let n = board.copper_count();
    for p in &board.placed {
        if !in_scope(&p.text_extent()) {
            continue;
        }
        for (layer, gfx) in &p.footprint.graphics {
            let layer = p.fp_layer(*layer, n);
            if is_silk(layer) {
                let what = format!("silk of {}", p.refdes);
                for shape in gfx.shapes() {
                    check(p.id, layer, &what, p.shape_to_board(&shape));
                }
            }
        }
        for t in p.texts() {
            if !t.visible || !is_silk(p.fp_layer(t.layer, n)) {
                continue;
            }
            let what = match t.kind {
                FpTextKind::Reference => format!("reference {}", p.refdes),
                FpTextKind::Value => format!("value {}", crate::text::shown_text(p, t)),
                FpTextKind::User => format!("text {}", t.text),
            };
            let (layer, strokes) = footprint_text(p, t, n);
            for shape in strokes {
                check(p.id, layer, &what, shape);
            }
        }
    }
    for g in &board.graphics {
        if is_silk(g.layer) && in_scope(&g.gfx.bbox()) {
            for shape in g.gfx.shapes() {
                check(g.id, g.layer, "silk graphic", shape);
            }
        }
    }
    for t in &board.texts {
        if is_silk(t.layer) && in_scope(&text_bound(t.at, &t.text, t.size, t.thick)) {
            let what = format!("text {}", t.text);
            for shape in text_strokes(&t.text, t.at, t.rot, t.size, t.thick, t.mirrored) {
                check(t.id, t.layer, &what, shape);
            }
        }
    }
    let mut pairs: Vec<_> = worst.into_iter().collect();
    pairs.sort_by_key(|(pair, _)| *pair);
    for ((silk, opening), f) in pairs {
        if in_region(region, f.at) {
            out.push(silk_violation(board, rules, silk, opening, f));
        }
    }
}

fn connectivity_checks(board: &Board, conn: &Connectivity, out: &mut Vec<Violation>) {
    let unit = Some(board.unit);
    // connectivity, always in full
    let mut joined_layers: HashMap<ItemId, LayerSet> = HashMap::new();
    for join in &conn.joins {
        for item in [join.a, join.b] {
            if let ConnItem::Via(id) = item {
                let layers = joined_layers.entry(id).or_default();
                *layers = layers.with(join.layer);
            }
        }
    }
    for via in &board.vias {
        let message = match joined_layers.get(&via.id).map_or(0, |layers| layers.len()) {
            0 => "via connected on no copper layer",
            1 => "via connected on one copper layer only",
            _ => continue,
        };
        out.push(
            Violation::new(
                CheckKind::DanglingVia,
                Severity::Warning,
                unit,
                vec![sel_of(board, ConnItem::Via(via.id))],
                "dangling via",
                message.into(),
            )
            .at(via.at)
            .hint("connect it on a second layer or remove it"),
        );
    }
    for s in &conn.shorts {
        let names: Vec<&str> = s.nets.iter().filter_map(|n| board.nets.get(n.ix())).map(|n| n.name.as_str()).collect();
        let names = names.join(", ");
        out.push(match s.witness {
            Some(w) => Violation::new(
                CheckKind::Short,
                Severity::Error,
                unit,
                vec![sel_of(board, w.items.0), sel_of(board, w.items.1)],
                "short",
                format!("short between {names}"),
            )
            .at(w.at),
            None => Violation::new(
                CheckKind::Short,
                Severity::Error,
                unit,
                Vec::new(),
                "short",
                format!("short between {names}; no touching pieces were found to show"),
            ),
        });
    }
    for r in &conn.ratsnest {
        let items = vec![sel_of(board, r.a), sel_of(board, r.b)];
        let name = board.nets.get(r.net.ix()).map(|n| n.name.as_str()).unwrap_or("?");
        let at = P::new(((r.pa.x as i64 + r.pb.x as i64) / 2) as Nm, ((r.pa.y as i64 + r.pb.y as i64) / 2) as Nm);
        out.push(
            Violation::new(
                CheckKind::Unconnected,
                Severity::Error,
                unit,
                items,
                "unconnected",
                format!("{name} not connected here"),
            )
            .at(at)
            .hint("route it"),
        );
    }
}

/// One fill polygon of a zone whose net is unrouted: its item, copper, box and cluster.
type NeckPoly<'a> = (ConnItem, &'a PolySet, BBox, usize);

/// The maximum error of the erosions that measure a neck, finer than the fill's `ARC_ERR`.
const NECK_ERR: Nm = 1_000;

/// Pour necks (L2 of the pour-connection definition). Copper of one fill that conducts only through
/// a passage narrower than the zone's minimum width is two conductors (the fill's width labels), so
/// connectivity holds an open there. Where two fill polygons of one zone on one layer touch but lie
/// in different clusters, one error finding stands at the contact. It names the neck's width and the
/// copper the pour does not connect: a pad, else a via, else a track of the smaller cluster.
/// Contacts of one cluster pair within the minimum width of each other are one neck. Only zones
/// whose net is unrouted are read, so a board whose pour nets are routed pays one pass over its zones.
///
/// Cost: per unrouted zone O(P) for its polygons and their clusters, plus one exact touch test per
/// pair of polygons in one tile or in side-adjacent tiles whose boxes meet and whose clusters differ.
/// Per neck: about a dozen offsets and splits of the fill within twice the minimum width of it.
fn pour_neck_checks<'a, I>(
    board: &Board,
    conn: &Connectivity,
    zone_polys: impl Fn(ItemId) -> I,
    out: &mut Vec<Violation>,
) where
    I: Iterator<Item = (ConnItem, &'a PolySet)>,
{
    let zones: Vec<&Zone> = board
        .zones
        .iter()
        .filter(|z| z.settings.min_width > 1 && z.net.is_some_and(|net| conn.unrouted.binary_search(&net).is_ok()))
        .collect();
    if zones.is_empty() {
        return;
    }
    let ids: BTreeSet<ItemId> = zones.iter().map(|z| z.id).collect();
    let mut cluster_of: HashMap<ConnItem, usize> = HashMap::new();
    for (i, cluster) in conn.clusters.iter().enumerate() {
        for &item in &cluster.items {
            if matches!(item, ConnItem::Zone { item: zone, .. } if ids.contains(&zone)) {
                cluster_of.insert(item, i);
            }
        }
    }
    for zone in zones {
        let mut polys: Vec<NeckPoly<'a>> = zone_polys(zone.id)
            .filter_map(|(item, poly)| Some((item, poly, poly.bbox(), *cluster_of.get(&item)?)))
            .collect();
        // Both entry points read the same polygons, in their own orders.
        polys.sort_unstable_by_key(|p| p.0);
        let mut tiles: HashMap<(LayerId, TileKey), Vec<usize>> = HashMap::new();
        for (i, p) in polys.iter().enumerate() {
            if let ConnItem::Zone { layer, tile, .. } = p.0 {
                tiles.entry((layer, tile)).or_default().push(i);
            }
        }
        // Touching polygons of different clusters, each pair once: a polygon lies in its tile, so
        // it can touch only polygons of its own tile and of the tiles beside it.
        let mut contacts: Vec<(LayerId, (usize, usize), PolySet)> = Vec::new();
        for (i, &(item, a, box_a, cluster_a)) in polys.iter().enumerate() {
            let ConnItem::Zone { layer, tile, .. } = item else { continue };
            for key in [tile, TileKey { x: tile.x + 1, ..tile }, TileKey { y: tile.y + 1, ..tile }] {
                for &j in tiles.get(&(layer, key)).into_iter().flatten() {
                    let (_, b, box_b, cluster_b) = polys[j];
                    if (key == tile && j <= i) || cluster_a == cluster_b || !box_a.intersects(&box_b) {
                        continue;
                    }
                    if core_distance(&Core::Set(a), &Core::Set(b)) >= 1.0 {
                        continue;
                    }
                    let strip = a.offset(ARC_ERR, ARC_ERR).intersection(b);
                    if !strip.is_empty() {
                        contacts.push((layer, (cluster_a.min(cluster_b), cluster_a.max(cluster_b)), strip));
                    }
                }
            }
        }
        let m = zone.settings.min_width;
        let mut necks: Vec<(LayerId, (usize, usize), Vec<PolySet>, BBox)> = Vec::new();
        for (layer, pair, strip) in contacts {
            let b = strip.bbox();
            match necks.iter_mut().find(|n| n.0 == layer && n.1 == pair && n.3.inflate(m).intersects(&b)) {
                Some(neck) => {
                    neck.3 = neck.3.union(b);
                    neck.2.push(strip);
                }
                None => necks.push((layer, pair, vec![strip], b)),
            }
        }
        for (layer, (ca, cb), strips, b) in necks {
            let Some(w) = neck_width(&polys, layer, &PolySet::union_all(&strips), m / 2) else { continue };
            let side = if (conn.clusters[ca].items.len(), ca) <= (conn.clusters[cb].items.len(), cb) { ca } else { cb };
            let items = &conn.clusters[side].items;
            let rank = |c: &ConnItem| match c {
                ConnItem::Pad { .. } => Some(0),
                ConnItem::Via(_) => Some(1),
                ConnItem::Track { .. } => Some(2),
                _ => None,
            };
            let named = items.iter().filter_map(|c| Some((rank(c)?, *c))).min().map(|(_, c)| c);
            let what = match named {
                Some(ConnItem::Pad { placed, pad }) => match board.placed_item(placed) {
                    Some(p) => format!("pad {} of {}", p.footprint.pads[pad.ix()].no, p.refdes),
                    None => "a pad".into(),
                },
                Some(ConnItem::Via(_)) => "a via".into(),
                Some(_) => "a track".into(),
                None => "the copper beyond it".into(),
            };
            let polygon = items.iter().copied().find(|c| matches!(c, ConnItem::Zone { .. }));
            let polygon = polygon.expect("a neck's cluster holds fill");
            let mut sels = vec![sel_of(board, polygon)];
            sels.extend(named.map(|c| sel_of(board, c)));
            let net = zone.net.and_then(|n| board.nets.get(n.ix())).map_or("?", |n| n.name.as_str());
            out.push(
                Violation::new(
                    CheckKind::PourNeck,
                    Severity::Error,
                    Some(board.unit),
                    sels,
                    "pour neck",
                    format!(
                        "Pour neck narrower than the minimum width ({} mm < {} mm): the {net} pour does not connect {what} here.",
                        mm3(w),
                        mm3(m)
                    ),
                )
                .at(b.center())
                .measures(Measure::Len(w), Measure::Len(m))
                .hint("widen the passage, or connect it with a track"),
            );
        }
    }
}

/// The width of the widest disc that passes a neck: twice the largest `r` at which the fill around
/// the contact `strips`, eroded by `r`, still joins the fill's cores beside it (`h = min_width / 2`).
/// The cores are the parts of the fill eroded by just under `h` that come within `h` of the contact,
/// one per side; with more than two it is the widest disc that joins them all. The fill is read
/// `h` beyond the window, so its erosion is exact there; bisection stops at 0.5 um. None: the
/// polygons touch at a point, with no passage. A neck whose sides are one core even at `h`
/// measures the minimum width (the fill's rounding).
fn neck_width(polys: &[NeckPoly<'_>], layer: LayerId, strips: &PolySet, h: Nm) -> Option<Nm> {
    let window = strips.bbox().inflate(2 * h);
    let read = window.inflate(h + ARC_ERR);
    let clip = PolySet::rect(read);
    let parts: Vec<PolySet> = polys
        .iter()
        .filter(|p| matches!(p.0, ConnItem::Zone { layer: l, .. } if l == layer) && p.2.intersects(&read))
        .map(|p| p.1.intersection(&clip))
        .collect();
    let fill = PolySet::union_all(&parts);
    let window = PolySet::rect(window);
    let eroded = |r: Nm| fill.offset(-r, NECK_ERR).intersection(&window).split();
    let reach = (h + 2 * ARC_ERR) as f64;
    let cores = |r: Nm| -> Vec<PolySet> {
        eroded(r).into_iter().filter(|c| core_distance(&Core::Set(c), &Core::Set(strips)) <= reach).collect()
    };
    let mut seeds = cores(h - ARC_ERR - NECK_ERR);
    if seeds.len() < 2 {
        seeds = cores(h);
    }
    if seeds.len() < 2 {
        return Some(2 * h);
    }
    let joined = |r: Nm| {
        let parts = eroded(r);
        let owner = |seed: &PolySet| parts.iter().position(|p| !p.intersection(seed).is_empty());
        let first = owner(&seeds[0]);
        first.is_some() && seeds[1..].iter().all(|seed| owner(seed) == first)
    };
    if joined(h) {
        return Some(2 * h);
    }
    if !joined(0) {
        return None;
    }
    let (mut lo, mut hi) = (0, h);
    while hi - lo > NECK_ERR / 2 {
        let mid = (lo + hi) / 2;
        if joined(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(2 * lo)
}

fn finish_violations(board: &Board, out: &mut Vec<Violation>) {
    let mut placements: HashMap<CompId, usize> = HashMap::with_capacity(board.placed.len());
    for (i, p) in board.placed.iter().enumerate() {
        // Match placed_of: the first placement of a component wins.
        placements.entry(p.comp).or_insert(i);
    }
    for v in out.iter_mut() {
        v.id = content_id_with_placed(board, &v.kind, &v.items, &v.rule, |comp| {
            placements.get(&comp).map(|&i| &board.placed[i])
        });
    }
    let waived: Vec<u64> = board.rules.waivers.iter().map(|w| w.violation).collect();
    out.retain(|v| !waived.contains(&v.id));
    out.sort_by_key(|v| v.id);
    out.dedup_by_key(|v| v.id);
}

fn limit_hint(rules: &RuleMatrix, rule: RuleRef, hint: &str) -> String {
    match rules.origin(rule) {
        Some(p) if hint.is_empty() => format!("below {p}'s minimum"),
        Some(p) => format!("below {p}'s minimum; {hint}"),
        None => hint.into(),
    }
}

fn mm3(v: Nm) -> String {
    format!("{:.3}", v as f64 / 1e6)
}

trait ScopeBox {
    fn is_none_or_box(&self, b: &BBox) -> bool;
}

impl ScopeBox for Option<BBox> {
    /// No scope (a full run), or `b` meets the scope box.
    fn is_none_or_box(&self, b: &BBox) -> bool {
        match self {
            None => true,
            Some(s) => !s.is_empty() && b.intersects(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zone::fill_all;
    use crate::{Area, Item, Zone, ZoneSettings};
    use crate::{
        connect, resolve_rules, BoardKeepout, BoardNet, BoardOp, Flash, Hole, Origin, Placed, Tent, Text, Track, Via,
        ViaKind, Waiver,
    };
    use fab_elec_core::{mm, Angle, BoardId, ClassId, CompId, Contour, Path, PolySet};
    use fab_elec_parts::{Footprint, FpText, Gfx, Keepout, Pad, PadKind, PadProp, PadShape, PadStack, PasteRule};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    /// Pair gaps are strict, track-only and layer-local, with indexed parity and explicit precedence.
    #[test]
    fn pair_gap() {
        use crate::NetPair;
        use fab_elec_circuit::{ClassRules, NetClass, PairRule};

        let mut board = Board::new(BoardId(0), "pair gap");
        board.outline.push(Contour::rect(P::ZERO, P::mm(20.0, 20.0), 0));
        board.nets = ["p", "n", "q"].into_iter()
            .map(|name| BoardNet { name: name.into(), class: ClassId(0) }).collect();
        board.classes = vec![NetClass { id: ClassId(0), name: "default".into(), rules: ClassRules::default() }];
        board.pairs = vec![NetPair { p: NetId(0), n: NetId(1), gap: None }];
        let rules = resolve_rules(&board, None);
        assert_eq!(rules.partner(NetId(0)), Some((NetId(1), 200_000)), "p resolves n and the standard gap");
        assert_eq!(rules.partner(NetId(1)), Some((NetId(0), 200_000)), "n resolves p and the same gap");
        assert_eq!(rules.partner(NetId(2)), None, "q has no pair partner");
        let run = |board: &Board, delta: &BoardDelta, prev: &[Violation], property: &str| {
            let conn = connect(board, &[]);
            let full = drc(board, &conn, &rules, &[], None);
            let index = PieceIndex::build(board, &[]);
            assert_eq!(
                drc_indexed(prev, &index, board, &conn, &rules, delta, &[]), full.violations,
                "{property}: indexed findings equal full DRC"
            );
            full
        };
        let gaps = |result: &DrcResult| {
            result.violations.iter().filter(|v| v.kind == CheckKind::PairGap).cloned().collect::<Vec<_>>()
        };
        let track = |id, net, layer, y| Track {
            id,
            net,
            layer,
            w: mm(0.2),
            widths: None,
            path: Path::new(P::new(mm(5.0), y)).line(P::new(mm(10.0), y)),
            ends: [Anchor::Free; 2],
            locked: false,
            origin: Origin::Authored,
        };
        let mut full = drc(&board, &connect(&board, &[]), &rules, &[], None);
        let p = board.alloc_id();
        let n = board.alloc_id();
        for (id, net, y, property) in [
            (p, NetId(0), mm(5.0), "adding the p track"),
            (n, NetId(1), mm(5.4), "adding n at exactly the standard gap"),
        ] {
            let applied = board.apply(&BoardOp::Add(Item::Track(track(id, net, LayerId::F_CU, y))))
                .expect("adding a pair track is valid");
            full = run(&board, &applied.delta, &full.violations, property);
            assert!(gaps(&full).is_empty(), "{property}: the standard pair gap is legal");
        }

        let applied = board.apply(&BoardOp::Move { item: n, by: P::mm(0.0, -0.05) })
            .expect("moving n towards p is valid");
        full = run(&board, &applied.delta, &full.violations, "moving n below the pair gap");
        let found = gaps(&full);
        assert_eq!(found.len(), 1, "a 0.15 mm pair gap produces exactly one finding");
        let items = vec![Sel::Item(board.unit, p), Sel::Item(board.unit, n)];
        assert_eq!(found[0].items, items, "the pair-gap finding names both tracks");
        assert_eq!(found[0].actual, Some(Measure::Len(mm(0.15))), "the actual edge gap is 0.15 mm");
        assert_eq!(found[0].required, Some(Measure::Len(mm(0.2))), "the standard pair gap is 0.2 mm");
        assert_eq!(found[0].rule, "pair gap", "the finding uses the pair-gap rule name");
        assert_eq!(found[0].severity, Severity::Warning, "a pair-gap finding is a warning");
        assert_eq!(
            found[0].message, "pair tracks 0.150 mm apart, the pair gap is 0.200 mm",
            "the pair-gap message gives actual and required distances"
        );
        assert_eq!(
            found[0].fix_hint, "space the pair's tracks at the pair gap",
            "the pair-gap hint describes the spacing remedy"
        );
        let at = found[0].at.expect("a pair-gap finding has a point");
        assert!(board.tracks[0].bbox().inflate(mm(0.2)).contains(at), "the point is inside p's grown box");
        assert!(board.tracks[1].bbox().inflate(mm(0.2)).contains(at), "the point is inside n's grown box");
        assert!(
            full.violations.iter().all(|v| v.kind != CheckKind::Clearance),
            "0.15 mm meets standard clearance even when it violates the pair gap"
        );

        let q = board.alloc_id();
        let applied = board.apply(&BoardOp::Add(Item::Track(track(q, NetId(2), LayerId::F_CU, mm(4.65)))))
            .expect("adding q on the other side of p is valid");
        full = run(&board, &applied.delta, &full.violations, "adding an unpaired neighbour");
        let found = gaps(&full);
        assert_eq!(found.len(), 1, "q at a 0.15 mm gap adds no pair-gap finding");
        assert_eq!(found[0].items, items, "only p and n take part in the pair-gap finding");

        let back = board.alloc_id();
        let applied = board.apply(&BoardOp::Add(Item::Track(track(back, NetId(1), LayerId::B_CU, mm(5.0)))))
            .expect("adding n directly below p on the back layer is valid");
        full = run(&board, &applied.delta, &full.violations, "adding n on the other layer");
        let found = gaps(&full);
        assert_eq!(found.len(), 1, "tracks on different layers add no pair-gap finding");
        assert_eq!(found[0].items, items, "the pair-gap finding still names only the front tracks");

        let fresh = |board: &Board| {
            drc(board, &connect(board, &[]), &resolve_rules(board, None), &[], None)
        };
        board.rules.base.pair_gap = Some(mm(0.3));
        let found = gaps(&fresh(&board));
        assert_eq!(found.len(), 1, "the board's larger pair gap keeps one finding");
        assert_eq!(found[0].required, Some(Measure::Len(mm(0.3))), "the board gap overrides the standard gap");
        board.classes[0].rules.pair = Some(PairRule { width: mm(0.2), gap: mm(0.25) });
        let found = gaps(&fresh(&board));
        assert_eq!(found.len(), 1, "the class's pair gap keeps one finding");
        assert_eq!(found[0].required, Some(Measure::Len(mm(0.25))), "the class gap overrides the board gap");
        board.pairs[0].gap = Some(mm(0.1));
        assert!(gaps(&fresh(&board)).is_empty(), "the pair's own gap overrides both board and class gaps");
    }

    /// A pair gap below the class clearance is the partners' clearance; other nets keep the class's.
    #[test]
    fn pair_gap_tighter_than_clearance() {
        use crate::NetPair;
        use fab_elec_circuit::{ClassRules, NetClass, PairRule};

        let mut base = Board::new(BoardId(0), "tight pair");
        base.outline.push(Contour::rect(P::ZERO, P::mm(20.0, 20.0), 0));
        base.nets = ["p", "n", "q"].into_iter()
            .map(|name| BoardNet { name: name.into(), class: ClassId(0) }).collect();
        base.classes = vec![NetClass {
            id: ClassId(0),
            name: "pair".into(),
            rules: ClassRules {
                track: Some(mm(0.2)),
                clearance: Some(mm(0.2)),
                pair: Some(PairRule { width: mm(0.2), gap: mm(0.15) }),
                ..Default::default()
            },
        }];
        base.pairs = vec![NetPair { p: NetId(0), n: NetId(1), gap: None }];
        let rules = resolve_rules(&base, None);
        assert_eq!(rules.partner(NetId(0)), Some((NetId(1), mm(0.15))), "p's partner keeps the class's pair gap");

        let copper = |item, net| Copper {
            item, net: Some(net), class: ClassId(0), layer: LayerId::F_CU,
            shape: Shape::Segment { a: P::ZERO, b: P::mm(1.0, 0.0), w: mm(0.2) },
        };
        let (p_id, other_id) = (ItemId(1000), ItemId(1001));
        let p_track = copper(ConnItem::Track { item: p_id, seg: 0 }, NetId(0));
        assert_eq!(
            required(&rules, &p_track, &copper(ConnItem::Track { item: other_id, seg: 0 }, NetId(1)), LayerId::F_CU),
            Required { gap: mm(0.15), rule: RuleRef::PAIR_GAP },
            "partner tracks need the pair gap, not the larger class clearance"
        );
        assert_eq!(
            required(&rules, &p_track, &copper(ConnItem::Track { item: other_id, seg: 0 }, NetId(2)), LayerId::F_CU).gap,
            mm(0.2),
            "a foreign net's track keeps the class clearance"
        );
        assert_eq!(
            required(&rules, &p_track, &copper(ConnItem::Via(other_id), NetId(1)), LayerId::F_CU).gap,
            mm(0.2),
            "a partner's via keeps the class clearance"
        );

        let track = |id, net, y| Track {
            id,
            net,
            layer: LayerId::F_CU,
            w: mm(0.2),
            widths: None,
            path: Path::new(P::new(mm(5.0), y)).line(P::new(mm(10.0), y)),
            ends: [Anchor::Free; 2],
            locked: false,
            origin: Origin::Authored,
        };
        let p = base.alloc_id();
        base.apply(&BoardOp::Add(Item::Track(track(p, NetId(0), mm(5.0))))).expect("adding p is valid");
        let before = drc(&base, &connect(&base, &[]), &rules, &[], None).violations;
        // The other track's centre is 0.2 mm (its width) plus the edge gap above p's. Below the pair
        // gap a partner is both a clearance error (at the pair gap) and a pair-gap warning.
        for (net, edge_gap, clearance_need, pair_gap_need, property) in [
            (NetId(1), mm(0.15), None, None, "partner n at the pair gap"),
            (NetId(1), mm(0.14), Some(mm(0.15)), Some(mm(0.15)), "partner n below the pair gap"),
            (NetId(2), mm(0.15), Some(mm(0.2)), None, "foreign q below the clearance"),
        ] {
            let mut board = base.clone();
            let id = board.alloc_id();
            let applied = board.apply(&BoardOp::Add(Item::Track(track(id, net, mm(5.2) + edge_gap))))
                .expect("adding the second track is valid");
            let conn = connect(&board, &[]);
            let full = drc(&board, &conn, &rules, &[], None);
            let index = PieceIndex::build(&board, &[]);
            assert_eq!(
                drc_indexed(&before, &index, &board, &conn, &rules, &applied.delta, &[]), full.violations,
                "{property}: indexed findings equal full DRC"
            );
            let need = |kind| {
                let found: Vec<_> = full.violations.iter().filter(|v| v.kind == kind).collect();
                assert!(found.len() <= 1, "{property}: at most one {kind:?} finding");
                found.first().map(|v| {
                    assert_eq!(v.actual, Some(Measure::Len(edge_gap)), "{property}: {kind:?} measures the edge gap");
                    (v.required, v.rule.clone())
                })
            };
            assert_eq!(
                need(CheckKind::Clearance).map(|(required, _)| required),
                clearance_need.map(|n| Some(Measure::Len(n))),
                "{property}: the clearance finding and its distance"
            );
            assert_eq!(
                need(CheckKind::PairGap).map(|(required, _)| required),
                pair_gap_need.map(|n| Some(Measure::Len(n))),
                "{property}: the pair-gap finding and its distance"
            );
            if net == NetId(1) {
                if let Some((_, rule)) = need(CheckKind::Clearance) {
                    assert_eq!(rule, "pair gap", "{property}: the partners' clearance is the pair-gap rule");
                }
            }
        }
    }

    /// Joined copper layers include zone fills, and every edit replaces connectivity findings in full.
    #[test]
    fn dangling_via() {
        let mut board = Board::new(BoardId(0), "dangling via");
        board.outline.push(Contour::rect(P::ZERO, P::mm(20.0, 20.0), 0));
        board.nets = vec![BoardNet { name: "a".into(), class: ClassId(0) }];
        assert_eq!(board.copper_count(), 2, "the fixture has two copper layers");
        let rules = resolve_rules(&board, None);
        let run = |board: &Board, fills: &[ZoneFill], delta: &BoardDelta, prev: &[Violation], property: &str| {
            let conn = connect(board, fills);
            let full = drc(board, &conn, &rules, fills, None);
            let index = PieceIndex::build(board, fills);
            assert_eq!(
                drc_indexed(prev, &index, board, &conn, &rules, delta, &[]), full.violations,
                "{property}: indexed findings equal full DRC"
            );
            (conn, full)
        };
        let dangling = |result: &DrcResult| {
            result.violations.iter().filter(|v| v.kind == CheckKind::DanglingVia).cloned().collect::<Vec<_>>()
        };
        let mut full = drc(&board, &connect(&board, &[]), &rules, &[], None);
        let via_id = board.alloc_id();
        let at = P::mm(5.0, 5.0);
        let applied = board.apply(&BoardOp::Add(Item::Via(Via {
            id: via_id,
            net: NetId(0),
            at,
            drill: mm(0.3),
            pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through,
            tent: Tent::Both,
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        }))).expect("adding the lone via is valid");
        let (_, next) = run(&board, &[], &applied.delta, &full.violations, "adding a lone via");
        full = next;
        let found = dangling(&full);
        assert_eq!(found.len(), 1, "a lone via has one dangling-via finding");
        assert_eq!(found[0].message, "via connected on no copper layer", "a lone via has no joined layer");
        assert_eq!(found[0].items, vec![sel_of(&board, ConnItem::Via(via_id))], "the finding names the via");
        assert_eq!(found[0].at, Some(at), "the dangling-via finding is at the via centre");
        assert_eq!(found[0].severity, Severity::Warning, "a dangling via is a warning");
        assert_eq!(found[0].rule, "dangling via", "a dangling via uses its own rule name");
        assert_eq!(
            found[0].fix_hint, "connect it on a second layer or remove it",
            "the dangling-via hint explains how to resolve it"
        );

        let track = |id, layer, start| Track {
            id,
            net: NetId(0),
            layer,
            w: mm(0.2),
            widths: None,
            path: Path::new(start).line(at),
            ends: [Anchor::Free, Anchor::Via(via_id)],
            locked: false,
            origin: Origin::Authored,
        };
        let front_id = board.alloc_id();
        let applied = board.apply(&BoardOp::Add(Item::Track(track(
            front_id, LayerId::F_CU, P::mm(3.0, 5.0),
        )))).expect("adding the front track is valid");
        let (_, next) = run(&board, &[], &applied.delta, &full.violations, "adding the front track");
        full = next;
        let found = dangling(&full);
        assert_eq!(found.len(), 1, "one front track leaves one dangling-via finding");
        assert_eq!(
            found[0].message, "via connected on one copper layer only",
            "the front track joins only one copper layer"
        );

        let zone_id = board.alloc_id();
        let applied = board.apply(&BoardOp::Add(Item::Zone(Zone {
            id: zone_id,
            net: Some(NetId(0)),
            name: "back pour".into(),
            layers: LayerSet::one(LayerId::B_CU),
            area: Area::Board,
            priority: 0,
            settings: ZoneSettings::default(),
            locked: false,
            origin: Origin::Authored,
        }))).expect("adding the back zone is valid");
        let fills = fill_all(&board, &rules);
        let (conn, next) = run(&board, &fills, &applied.delta, &full.violations, "adding the back fill");
        full = next;
        assert!(
            conn.joins.iter().any(|join| {
                join.layer == LayerId::B_CU
                    && ((join.a == ConnItem::Via(via_id)
                        && matches!(join.b, ConnItem::Zone { item, .. } if item == zone_id))
                        || (join.b == ConnItem::Via(via_id)
                            && matches!(join.a, ConnItem::Zone { item, .. } if item == zone_id)))
            }),
            "the back fill really joins the via through a zone polygon"
        );
        assert!(dangling(&full).is_empty(), "front track and back fill connect the via on two layers");

        let applied = board.apply(&BoardOp::Remove(zone_id)).expect("removing the back zone is valid");
        let (_, next) = run(&board, &[], &applied.delta, &full.violations, "removing the back fill");
        full = next;
        let found = dangling(&full);
        assert_eq!(found.len(), 1, "removing the back fill restores the dangling-via finding");
        assert_eq!(
            found[0].message, "via connected on one copper layer only",
            "removing the back fill leaves only the front connection"
        );

        let back_id = board.alloc_id();
        let applied = board.apply(&BoardOp::Add(Item::Track(track(
            back_id, LayerId::B_CU, P::mm(7.0, 5.0),
        )))).expect("adding the back track is valid");
        let (_, next) = run(&board, &[], &applied.delta, &full.violations, "adding the back track");
        assert!(dangling(&next).is_empty(), "front and back tracks connect the via on two layers");
    }

    /// Only the via drill triggers a pad warning, and moving either owner preserves indexed parity.
    #[test]
    fn via_in_pad() {
        use crate::{resolve_rules_with, Effective};
        use fab_elec_circuit::{ClassRules, NetClass};

        let mut board = Board::new(BoardId(0), "via in pad");
        board.outline.push(Contour::rect(P::ZERO, P::mm(20.0, 20.0), 0));
        board.nets = vec![BoardNet { name: "a".into(), class: ClassId(0) }];
        let pad_id = board.alloc_id();
        board.placed.push(Placed {
            id: pad_id,
            comp: CompId(0),
            path: "U1".into(),
            refdes: "U1".into(),
            footprint: Arc::new(Footprint {
                pads: vec![Pad {
                    no: "1".into(),
                    kind: PadKind::Smd,
                    stack: PadStack::smd(PadShape::Rect { size: P::mm(1.0, 1.0) }),
                    at: P::ZERO,
                    rot: Angle::ZERO,
                    drill: None,
                    mask_margin: None,
                    paste: PasteRule::default(),
                    thermal: None,
                    zone_connect: None,
                    die_len: 0,
                    prop: PadProp::None,
                }],
                ..Footprint::default()
            }),
            pad_nets: vec![Some(NetId(0))],
            at: P::mm(5.0, 5.0),
            rot: Angle::ZERO,
            side: Side::Top,
            locked: false,
            origin: Origin::Authored,
            body_z: None,
            ref_label: None, value_label: None, legend: None,
            value: None,
        });
        let rules = resolve_rules(&board, None);
        let run = |board: &Board, rules: &RuleMatrix, delta: &BoardDelta, prev: &[Violation], property: &str| {
            let conn = connect(board, &[]);
            let full = drc(board, &conn, rules, &[], None);
            let index = PieceIndex::build(board, &[]);
            assert_eq!(
                drc_indexed(prev, &index, board, &conn, rules, delta, &[]), full.violations,
                "{property}: indexed findings equal full DRC"
            );
            full
        };
        let in_pad = |result: &DrcResult| {
            result.violations.iter().filter(|v| v.kind == CheckKind::ViaInPad).cloned().collect::<Vec<_>>()
        };
        let mut full = drc(&board, &connect(&board, &[]), &rules, &[], None);
        let via_id = board.alloc_id();
        let applied = board.apply(&BoardOp::Add(Item::Via(Via {
            id: via_id,
            net: NetId(0),
            at: board.placed[0].at,
            drill: mm(0.3),
            pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through,
            tent: Tent::Both,
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        }))).expect("adding the via at the pad centre is valid");
        full = run(&board, &rules, &applied.delta, &full.violations, "adding the via at the pad centre");
        let found = in_pad(&full);
        assert_eq!(found.len(), 1, "a via at the pad centre has one via-in-pad finding");
        let mut items = vec![
            sel_of(&board, ConnItem::Via(via_id)),
            sel_of(&board, ConnItem::Pad { placed: pad_id, pad: PadIx(0) }),
        ];
        items.sort();
        assert_eq!(found[0].items, items, "the finding names the via and the placed pad");
        assert_eq!(found[0].severity, Severity::Warning, "a via in a pad is a warning");
        assert_eq!(found[0].rule, "via in pad", "the finding uses the via-in-pad rule name");
        assert_eq!(
            found[0].message,
            "via drill 0.000 mm from a pad, needs 0.150 mm (the pad's mask opening plus a mask web): \
                solder wicks into the hole unless the via is filled and capped",
            "the warning gives the measured and required gaps and explains the solder-wicking risk"
        );
        assert_eq!(
            found[0].fix_hint, "move the via off the pad, or fill and cap it",
            "the hint describes both remedies"
        );
        let standard_need = 150_000;
        assert_eq!((found[0].actual, found[0].required), (Some(Measure::Len(0)), Some(Measure::Len(standard_need))),
            "the centred drill has zero clearance and needs the standard mask expansion plus web");
        let at = found[0].at.expect("the via-in-pad finding has a point");
        let pad = board.placed[0].pad_shape(PadIx(0), LayerId::F_CU, board.copper_count())
            .expect("the SMD pad has front copper");
        assert!(pad.bbox().contains(at), "the centred finding lies inside the pad");
        assert!(board.vias[0].drill_hole().shape().bbox().contains(at), "the finding lies inside the drill box");

        let original = board.placed[0].clone();
        let mut wider = original.clone();
        Arc::make_mut(&mut wider.footprint).pads[0].mask_margin = Some(mm(0.5));
        let wide_need = 600_000;
        let move_via = |dx| BoardOp::Move { item: via_id, by: P::new(dx, 0) };
        // Pad half-width 0.5 mm plus drill radius 0.15 mm: the wall touches at a 0.65 mm offset.
        for (property, op, gap, need) in [
            ("drill overlaps the pad edge", move_via(600_000), 0, standard_need),
            ("drill 1 nm off its own pad", move_via(50_001), 1, standard_need),
            ("drill 1 nm short of the standard need", move_via(149_998), 149_999, standard_need),
            ("at the standard need only the annular ring touches", move_via(1), 150_000, standard_need),
            ("drill beyond the standard search bound", move_via(400_000), 550_000, standard_need),
            ("pad edit raises the need on the live board", BoardOp::Replace(Item::Placed(wider)), 550_000, wide_need),
            ("drill exactly at the pad override's need", move_via(50_000), 600_000, wide_need),
            ("drill back inside the wider opening's web", move_via(-50_000), 550_000, wide_need),
            ("removing the override restores the board need", BoardOp::Replace(Item::Placed(original)), 550_000, standard_need),
            ("moving the pad back over the via", BoardOp::Move { item: pad_id, by: P::mm(1.2, 0.0) }, 0, standard_need),
        ] {
            let applied = board.apply(&op).unwrap_or_else(|e| panic!("{property}: fixture edit applies: {e:?}"));
            assert!(!applied.delta.global, "{property}: editing the via or pad produces a local board delta");
            full = run(&board, &rules, &applied.delta, &full.violations, property);
            let found = in_pad(&full);
            assert_eq!(found.len(), usize::from(gap < need), "{property}: one finding exactly below the pad's need");
            let pad = board.placed[0].pad_shape(PadIx(0), LayerId::F_CU, board.copper_count())
                .expect("the moved SMD pad has front copper");
            let drill = board.vias[0].drill_hole().shape();
            assert_eq!(clearance(&drill, &pad), gap, "{property}: the fixture has the specified drill-wall gap");
            if gap == standard_need {
                assert_eq!(clearance(&board.vias[0].shape(), &pad), 0, "{property}: the annular ring touches");
            }
            if let Some(v) = found.first() {
                assert_eq!(v.items, items, "{property}: the same via and pad are named");
                assert_eq!((v.actual, v.required), (Some(Measure::Len(gap)), Some(Measure::Len(need))),
                    "{property}: the finding measures the drill-wall gap and the pad's need in nm");
                let at = v.at.expect("the via-in-pad finding has a point");
                assert!(pad.bbox().contains(at), "{property}: the finding lies inside the pad box");
                assert_eq!(at, overlap_centre(drill.bbox().inflate(need), pad.bbox()),
                    "{property}: the location centres the pad's overlap with the drill's required clearance");
            }
        }

        let class = |name: &str, via_in_pad| NetClass {
            id: ClassId(0),
            name: name.into(),
            rules: ClassRules { via_in_pad, ..ClassRules::default() },
        };
        for (property, classes, factory, count) in [
            ("the via's class allows vias in pads without a factory", vec![class("default", Some(true))], None, 0),
            ("the via's class forbids them explicitly", vec![class("default", Some(false))], None, 1),
            (
                "only another class allows them",
                vec![class("default", None), NetClass { id: ClassId(1), ..class("open", Some(true)) }],
                None,
                1,
            ),
            ("the factory cannot fill and cap vias", vec![class("default", Some(true))], Some(false), 1),
            ("the class and factory both allow them", vec![class("default", Some(true))], Some(true), 0),
        ] {
            let mut board = board.clone();
            board.classes = classes;
            let effective = factory.map(|via_in_pad| Effective {
                rules: board.rules.clone(), origin: "factory".into(), via_in_pad,
            });
            let rules = resolve_rules_with(&board, effective.as_ref(), None);
            let prev = drc(&board, &connect(&board, &[]), &rules, &[], None).violations;
            let applied = board.apply(&BoardOp::Move { item: via_id, by: P::mm(0.0, 0.1) })
                .expect("moving the via within the pad is valid");
            let full = run(&board, &rules, &applied.delta, &prev, property);
            assert_eq!(in_pad(&full).len(), count, "{property}: via-in-pad findings follow the resolved permission");
        }
    }

    #[test]
    fn silk_over_mask_openings() {
        let mut board = Board::new(BoardId(0), "silk");
        board.outline.push(Contour::rect(P::ZERO, P::mm(50.0, 40.0), 0));
        board.nets = vec![BoardNet { name: "a".into(), class: ClassId(0) }];
        let footprint = Arc::new(Footprint {
            name: "silk test".into(),
            pads: vec![Pad {
                no: "1".into(),
                kind: PadKind::Smd,
                stack: PadStack::smd(PadShape::Rect { size: P::mm(2.0, 2.0) }),
                at: P::ZERO,
                rot: Angle::ZERO,
                drill: None,
                mask_margin: None,
                paste: PasteRule::default(),
                thermal: None,
                zone_connect: None,
                die_len: 0,
                prop: PadProp::None,
            }],
            ..Footprint::default()
        });
        for (comp, refdes, x) in [(CompId(0), "R1", 10.0), (CompId(1), "R2", 20.0)] {
            let id = board.alloc_id();
            board.placed.push(Placed {
                id,
                comp,
                path: refdes.into(),
                refdes: refdes.into(),
                footprint: footprint.clone(),
                pad_nets: vec![Some(NetId(0))],
                at: P::mm(x, 10.0),
                rot: Angle::ZERO,
                side: Side::Top,
                locked: false,
                origin: Origin::Authored,
                body_z: None,
                ref_label: None, value_label: None, legend: None,
                value: None,
            });
        }
        let fp = Arc::make_mut(&mut board.placed[0].footprint);
        fp.texts.push(FpText {
            kind: FpTextKind::Reference,
            text: "REF**".into(),
            layer: LayerId::F_SILK,
            at: P::mm(10.0, 0.0),
            rot: Angle::ZERO,
            size: mm(1.0),
            thick: mm(0.15),
            visible: true,
        });
        fp.graphics.push((LayerId::F_SILK, Gfx::Stroke {
            path: Path::new(P::mm(-0.5, 1.2)).line(P::mm(0.5, 1.2)),
            w: mm(0.1),
        }));
        let via = board.alloc_id();
        board.vias.push(Via {
            id: via,
            net: NetId(0),
            at: P::mm(30.0, 10.0),
            drill: mm(0.3),
            pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through,
            tent: Tent::None,
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        });
        let back_text = board.alloc_id();
        let via_text = board.alloc_id();
        for (id, layer, at) in [
            (back_text, LayerId::B_SILK, board.placed[1].at),
            (via_text, LayerId::F_SILK, board.vias[0].at),
        ] {
            board.texts.push(Text {
                id,
                layer,
                text: "X".into(),
                at,
                rot: Angle::ZERO,
                size: mm(1.0),
                thick: mm(0.15),
                mirrored: layer == LayerId::B_SILK,
                locked: false,
                origin: Origin::Authored,
            });
        }
        let run = |board: &Board, delta: Option<&BoardDelta>| {
            drc(board, &connect(board, &[]), &resolve_rules(board, None), &[], delta)
        };
        let silk = |result: &DrcResult| {
            result.violations.iter().filter(|v| v.kind == CheckKind::SilkOverPad).cloned().collect::<Vec<_>>()
        };
        let first = board.placed[0].id;
        let second = board.placed[1].id;
        let full = run(&board, None);
        let found = silk(&full);
        assert_eq!(found.len(), 2, "default clearance reports only the reference and untented via overlaps");
        for (owner, opening, message) in [
            (first, ConnItem::Pad { placed: second, pad: PadIx(0) }, "reference R1 over pad 1 of R2"),
            (via_text, ConnItem::Via(via), "text X over a via"),
        ] {
            let v = found.iter().find(|v| v.message == message).expect("overlap message names silk and opening");
            let mut items = vec![Sel::Item(board.unit, owner), sel_of(&board, opening)];
            items.sort();
            assert_eq!(v.items, items, "overlap selections name the silk owner and the opening");
            assert_eq!(v.severity, Severity::Warning, "silk clipping is a warning");
            assert_eq!(v.actual, Some(Measure::Len(0)), "overlap has zero actual gap");
            assert_eq!(v.required, Some(Measure::Len(0)), "standard silk clearance is zero");
            assert_eq!(v.rule, "silk clearance", "silk findings use the resolved rule name");
            assert_eq!(
                v.fix_hint, "move the silk off the opening; the fab clips silk there",
                "silk finding explains the fabrication consequence"
            );
        }
        assert!(
            found.iter().all(|v| !v.items.contains(&Sel::Item(board.unit, back_text))),
            "bottom silk does not conflict with a top-only pad opening"
        );
        board.vias[0].tent = Tent::Both;
        let tented = silk(&run(&board, None));
        assert_eq!(tented.len(), 1, "tenting the via leaves only the reference overlap");
        assert_eq!(tented[0].message, "reference R1 over pad 1 of R2", "tented vias have no silk finding");
        board.vias[0].tent = Tent::None;

        board.rules.base.silk_clearance = Some(mm(0.2));
        let full = run(&board, None);
        let found = silk(&full);
        assert_eq!(found.len(), 3, "0.2 mm clearance also reports the nearby footprint graphic");
        let graphic = found.iter().find(|v| v.message.starts_with("silk of R1 "))
            .expect("positive clearance catches the footprint graphic");
        assert!(
            matches!(graphic.actual, Some(Measure::Len(gap)) if (gap - mm(0.1)).abs() <= 1),
            "graphic is about 0.1 mm from its own pad's mask opening"
        );
        assert_eq!(graphic.required, Some(Measure::Len(mm(0.2))), "graphic requires the configured 0.2 mm");
        assert_eq!(graphic.severity, Severity::Warning, "positive silk clearance is also a warning");
        assert_eq!(
            graphic.message, "silk of R1 0.100 mm from pad 1 of R1, needs 0.200 mm",
            "positive clearance message names both owners and both measures"
        );
        let mut own_items = vec![
            Sel::Item(board.unit, first),
            sel_of(&board, ConnItem::Pad { placed: first, pad: PadIx(0) }),
        ];
        own_items.sort();
        assert_eq!(graphic.items, own_items, "a footprint's own pad is not exempt");

        for (property, op, removed_message) in [
            (
                "moving the text's owner",
                BoardOp::Move { item: first, by: P::mm(0.0, 10.0) },
                "reference R1 over pad 1 of R2",
            ),
            (
                "moving the pad's owner",
                BoardOp::Move { item: second, by: P::mm(10.0, 10.0) },
                "reference R1 over pad 1 of R2",
            ),
            ("removing the via", BoardOp::Remove(via), "text X over a via"),
        ] {
            let mut changed = board.clone();
            let applied = changed.apply(&op).expect("fixture edit is valid");
            let after = run(&changed, None);
            assert!(
                after.violations.iter().all(|v| v.message != removed_message),
                "{property} removes its overlap finding"
            );
            let update = run(&changed, Some(&applied.delta));
            assert_eq!(
                merge_violations(&full.violations, &update), after.violations,
                "{property}: merging the apply delta equals a full run"
            );
            let delta = BoardDelta::between(&board, &changed);
            let update = run(&changed, Some(&delta));
            assert_eq!(
                merge_violations(&full.violations, &update), after.violations,
                "{property}: merging the between delta equals a full run"
            );
        }
    }

    #[test]
    fn content_ids_survive_renumbering() {
        let make_board = |via_first| {
            let mut board = Board::new(BoardId(0), "t");
            board.outline.push(Contour::rect(P::ZERO, P::new(20_000_000, 20_000_000), 0));
            board.nets = vec![
                BoardNet { name: "a".into(), class: ClassId(0) },
                BoardNet { name: "b".into(), class: ClassId(0) },
            ];
            let ids = if via_first {
                let via = board.alloc_id();
                [board.alloc_id(), board.alloc_id(), via]
            } else {
                [board.alloc_id(), board.alloc_id(), board.alloc_id()]
            };
            for (i, y) in [5_000_000, 5_300_000].into_iter().enumerate() {
                board.tracks.push(Track {
                    id: ids[i],
                    net: NetId(i as u32),
                    layer: LayerId::F_CU,
                    w: 200_000,
                    widths: None,
                    path: Path::new(P::new(3_000_000, y)).line(P::new(7_000_000, y)),
                    ends: [Anchor::Free; 2],
                    locked: false,
                    origin: Origin::Authored,
                });
            }
            board.vias.push(Via {
                id: ids[2],
                net: NetId(0),
                at: P::new(12_000_000, 12_000_000),
                drill: 200_000,
                pad: 450_000,
                span: (LayerId::F_CU, LayerId::B_CU),
                kind: ViaKind::Through,
                tent: Tent::Both,
                backdrill: None,
                locked: false,
                origin: Origin::Authored,
                flash: Flash::All,
            });
            board
        };
        let run = |board: &Board, delta: Option<&BoardDelta>| {
            let conn = connect(board, &[]);
            let rules = resolve_rules(board, None);
            drc(board, &conn, &rules, &[], delta)
        };
        let ids = |result: &DrcResult| result.violations.iter().map(|v| v.id).collect::<BTreeSet<_>>();
        let board = make_board(false);
        let first = run(&board, None);
        let mut reread = make_board(true);
        let second = run(&reread, None);
        assert_eq!(ids(&first), ids(&second), "ids are content, not item ids");
        let clearance = first.violations.iter().find(|v| v.kind == CheckKind::Clearance)
            .expect("the fixture has a clearance finding");
        let reread_clearance = second.violations.iter().find(|v| v.kind == CheckKind::Clearance)
            .expect("the renumbered fixture has a clearance finding");
        assert_ne!(clearance.items, reread_clearance.items, "the fixture really renumbers");
        let mut reversed = clearance.items.clone();
        reversed.reverse();
        assert_eq!(
            content_id(&board, &clearance.kind, &reversed, &clearance.rule), clearance.id,
            "item order does not change content ids"
        );
        let delta = BoardDelta {
            changed: vec![reread.tracks[0].id],
            bbox: reread.tracks[0].bbox(),
            ..BoardDelta::default()
        };
        let update = run(&reread, Some(&delta));
        assert!(
            update.violations.iter().any(|v| v.id == clearance.id),
            "full and delta runs share content ids"
        );
        reread.rules.waivers.push(Waiver { violation: clearance.id, reason: "accepted clearance".into() });
        let waived = run(&reread, None);
        assert!(
            !waived.violations.iter().any(|v| v.kind == CheckKind::Clearance),
            "a waiver written against one read matches the next"
        );
        let track = &mut reread.tracks[1];
        let (start, end) = (track.path.start, track.path.end());
        track.path = Path::new(P::new(start.x, start.y + 1)).line(P::new(end.x, end.y + 1));
        let moved = run(&reread, None);
        let moved_clearance = moved.violations.iter().find(|v| v.kind == CheckKind::Clearance)
            .expect("the moved track still violates clearance and is no longer waived");
        assert_ne!(moved_clearance.id, clearance.id, "geometry is content");
    }

    #[test]
    fn keepout_and_courtyard_checks() {
        let mut board = Board::new(BoardId(0), "t");
        board.outline.push(Contour::rect(P::ZERO, P::new(20_000_000, 20_000_000), 0));
        board.nets = vec![BoardNet { name: "a".into(), class: ClassId(0) }];
        let m = resolve_rules(&board, None);
        let run = |board: &Board, delta: Option<&BoardDelta>| {
            let conn = connect(board, &[]);
            drc(board, &conn, &m, &[], delta)
        };
        let keepout_id = board.alloc_id();
        board.keepouts.push(BoardKeepout {
            id: keepout_id,
            area: PolySet::rect(BBox::new(P::new(4_000_000, 4_000_000), P::new(6_000_000, 6_000_000))),
            layers: LayerSet::one(LayerId::F_CU),
            forbid: Forbid::TRACKS,
            reason: String::new(),
            origin: Origin::Authored,
            z: None,
        });
        let track_id = board.alloc_id();
        board.tracks.push(Track {
            id: track_id,
            net: NetId(0),
            layer: LayerId::F_CU,
            w: 200_000,
            widths: None,
            path: Path::new(P::new(3_000_000, 5_000_000)).line(P::new(5_000_000, 5_000_000)).line(P::new(7_000_000, 5_000_000)),
            ends: [Anchor::Free; 2],
            locked: false,
            origin: Origin::Authored,
        });
        let result = run(&board, None);
        let keepouts: Vec<_> = result.violations.iter().filter(|v| v.kind == CheckKind::Keepout).collect();
        assert_eq!(keepouts.len(), 1, "one keepout finding across crossing track pieces");
        assert_eq!(keepouts[0].items, vec![Sel::Item(board.unit, keepout_id), Sel::Item(board.unit, track_id)], "keepout selections are sorted");
        assert_eq!(keepouts[0].rule, "keepout", "keepout uses the shared rule name");
        assert_eq!((keepouts[0].actual, keepouts[0].required), (None, None), "keepout findings have no measures");
        let delta = BoardDelta { changed: vec![track_id], bbox: board.tracks[0].bbox(), ..BoardDelta::default() };
        let update = run(&board, Some(&delta));
        let local: Vec<_> = update.violations.iter().filter(|v| v.kind == CheckKind::Keepout).collect();
        assert_eq!(local, keepouts, "delta preserves the full-run keepout finding");

        board.tracks[0].path = Path::new(P::new(3_000_000, 6_600_000)).line(P::new(7_000_000, 6_600_000));
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::Keepout), "track copper 0.5 mm clear of the keepout is legal");
        let via_id = board.alloc_id();
        board.vias.push(Via {
            id: via_id,
            net: NetId(0),
            at: P::new(5_000_000, 5_000_000),
            drill: 300_000,
            pad: 600_000,
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through,
            tent: Tent::Both,
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        });
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::Keepout), "a tracks-only keepout permits a via inside it");

        let footprint = Arc::new(Footprint {
            name: "test".into(),
            pads: vec![Pad {
                no: "1".into(),
                kind: PadKind::Smd,
                stack: PadStack::smd(PadShape::Rect { size: P::new(500_000, 500_000) }),
                at: P::ZERO,
                rot: Angle::ZERO,
                drill: None,
                mask_margin: None,
                paste: PasteRule::default(),
                thermal: None,
                zone_connect: None,
                die_len: 0,
                prop: PadProp::None,
            }],
            courtyard: PolySet::rect(BBox::new(P::new(-1_000_000, -1_000_000), P::new(1_000_000, 1_000_000))),
            ..Footprint::default()
        });
        for (comp, refdes, x) in [(CompId(0), "U1", 12_000_000), (CompId(1), "U2", 13_500_000)] {
            let id = board.alloc_id();
            board.placed.push(Placed {
                id,
                comp,
                path: refdes.into(),
                refdes: refdes.into(),
                footprint: footprint.clone(),
                pad_nets: vec![Some(NetId(0))],
                at: P::new(x, 12_000_000),
                rot: Angle::ZERO,
                side: Side::Top,
                locked: false,
                origin: Origin::Authored,
                body_z: None,
                ref_label: None, value_label: None, legend: None,
                value: None,
            });
        }
        let result = run(&board, None);
        let overlaps: Vec<_> = result.violations.iter().filter(|v| v.kind == CheckKind::CourtyardOverlap).collect();
        assert_eq!(overlaps.len(), 1, "same-side courtyards overlapping by 0.5 mm produce one finding");
        assert_eq!(overlaps[0].items, vec![Sel::Item(board.unit, board.placed[0].id), Sel::Item(board.unit, board.placed[1].id)], "courtyard selections are sorted");
        assert_eq!(overlaps[0].at, Some(P::new(12_750_000, 12_000_000)), "courtyard location is the intersection box centre");
        assert_eq!(overlaps[0].message, "courtyards of U1 and U2 overlap", "courtyard message names both parts");
        let delta = BoardDelta { changed: vec![board.placed[1].id], bbox: board.placed[1].bbox(), ..BoardDelta::default() };
        let update = run(&board, Some(&delta));
        let local: Vec<_> = update.violations.iter().filter(|v| v.kind == CheckKind::CourtyardOverlap).collect();
        assert_eq!(local, overlaps, "delta preserves the full-run courtyard finding");
        let delta = BoardDelta { bbox: BBox::point(P::new(2_000_000, 2_000_000)), ..BoardDelta::default() };
        let update = run(&board, Some(&delta));
        assert!(!update.violations.iter().any(|v| v.kind == CheckKind::CourtyardOverlap), "delta excludes courtyards outside its scope");
        board.placed.swap(0, 1);
        let reversed = run(&board, None);
        let reverse_overlaps: Vec<_> = reversed.violations.iter().filter(|v| v.kind == CheckKind::CourtyardOverlap).collect();
        assert_eq!(reverse_overlaps, overlaps, "courtyard findings are independent of placement order");
        board.placed.swap(0, 1);

        board.placed[1].side = Side::Bottom;
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::CourtyardOverlap), "opposite-side courtyards do not overlap");
        board.placed[1].side = Side::Top;
        board.placed[1].at.x = 14_000_000;
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::CourtyardOverlap), "abutting courtyards have zero intersection area");
        board.placed[1].at.x = 13_500_000;
        Arc::make_mut(&mut board.placed[1].footprint).courtyard = PolySet::new();
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::CourtyardOverlap), "an empty courtyard never overlaps");

        board.placed[1].footprint = footprint.clone();
        Arc::make_mut(&mut board.placed[0].footprint).keepouts.push(Keepout {
            area: footprint.courtyard.clone(),
            layers: LayerSet::one(LayerId::F_CU),
            forbid: Forbid::FOOTPRINTS.with(Forbid::PADS),
            reason: String::new(),
        });
        let result = run(&board, None);
        let keepouts: Vec<_> = result.violations.iter().filter(|v| v.kind == CheckKind::Keepout).collect();
        assert_eq!(keepouts.len(), 1, "a footprint keepout flags the other courtyard and exempts its own pad");
        assert_eq!(keepouts[0].items, vec![Sel::Item(board.unit, board.placed[0].id), Sel::Item(board.unit, board.placed[1].id)], "footprint keepout findings name the part and owner");
        board.placed[1].side = Side::Bottom;
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::Keepout), "a footprint keepout permits a courtyard on the other side");
        board.placed[1].side = Side::Top;
        board.placed[1].at.x = 14_000_000;
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::Keepout), "a courtyard may abut a footprint keepout");

        Arc::make_mut(&mut board.placed[0].footprint).keepouts[0].forbid = Forbid::PADS;
        board.placed[1].at.x = 12_500_000;
        let result = run(&board, None);
        assert_eq!(result.violations.iter().filter(|v| v.kind == CheckKind::Keepout).count(), 1, "a footprint keepout forbids another part's pad");
        board.placed[0].side = Side::Bottom;
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::Keepout), "bottom placement flips the keepout off front copper");
        board.placed[1].side = Side::Bottom;
        let result = run(&board, None);
        assert_eq!(result.violations.iter().filter(|v| v.kind == CheckKind::Keepout).count(), 1, "the flipped keepout forbids another bottom pad and exempts its own");

        // Height bands: a raised band catches a body only where the bands overlap, and never copper.
        Arc::make_mut(&mut board.placed[0].footprint).keepouts.clear();
        board.placed[0].side = Side::Top;
        board.placed[1].side = Side::Top;
        board.placed[1].at.x = 16_000_000;
        let pipe = board.alloc_id();
        board.keepouts.push(BoardKeepout {
            id: pipe,
            area: PolySet::rect(BBox::new(P::new(15_000_000, 11_000_000), P::new(17_000_000, 13_000_000))),
            layers: LayerSet::one(LayerId::F_CU),
            forbid: Forbid::FOOTPRINTS.with(Forbid::PADS),
            reason: String::new(),
            origin: Origin::Authored,
            z: Some((2_000_000, 9_000_000)),
        });
        let breaches = |board: &Board| -> Vec<Violation> {
            let names_pipe = |v: &Violation| v.kind == CheckKind::Keepout && v.items.contains(&Sel::Item(board.unit, pipe));
            run(board, None).violations.into_iter().filter(names_pipe).collect()
        };
        board.placed[1].body_z = Some((0, 600_000));
        assert!(breaches(&board).is_empty(), "a short body under a raised band is no breach, and its pad is not caught");
        board.placed[1].body_z = Some((0, 5_000_000));
        let found = breaches(&board);
        assert_eq!(found.len(), 1, "a tall body reaching into the band is a breach (its pad still is not)");
        assert_eq!(found[0].message, "body 0.000-5.000 mm high reaches into the keepout's 2.000-9.000 mm band", "the finding names both bands");
        board.placed[1].body_z = None;
        assert_eq!(breaches(&board).len(), 1, "a part without a body band counts as full height");
        board.placed[1].body_z = Some((0, 600_000));
        board.keepouts.last_mut().expect("the band keepout").z = None;
        assert_eq!(breaches(&board).len(), 2, "a keepout without a band catches the short body and its pad");
    }

    #[test]
    fn drill_checks() {
        let mut board = Board::new(BoardId(0), "t");
        board.outline.push(Contour::rect(P::ZERO, P::new(20_000_000, 20_000_000), 0));
        board.nets = vec![BoardNet { name: "a".into(), class: ClassId(0) }, BoardNet { name: "b".into(), class: ClassId(0) }];
        let m = resolve_rules(&board, None);
        let run = |board: &Board, delta: Option<&BoardDelta>| {
            let conn = connect(board, &[]);
            drc(board, &conn, &m, &[], delta)
        };
        for (net, at) in [(NetId(0), P::new(5_000_000, 5_000_000)), (NetId(1), P::new(5_500_000, 5_000_000))] {
            let id = board.alloc_id();
            board.vias.push(Via {
                id,
                net,
                at,
                drill: 300_000,
                pad: 450_000,
                span: (LayerId::F_CU, LayerId::B_CU),
                kind: ViaKind::Through,
                tent: Tent::Both,
                backdrill: None,
                locked: false,
                origin: Origin::Authored,
                flash: Flash::All,
            });
        }
        let result = run(&board, None);
        let holes: Vec<_> = result.violations.iter().filter(|v| v.kind == CheckKind::HoleToHole).collect();
        assert_eq!(holes.len(), 1, "one hole-to-hole finding per via pair");
        assert_eq!(holes[0].actual, Some(Measure::Len(200_000)), "hole-to-hole actual gap");
        assert_eq!(holes[0].required, Some(Measure::Len(250_000)), "standard hole-to-hole requirement");
        let copper: Vec<_> = result.violations.iter().filter(|v| v.kind == CheckKind::HoleClearance).collect();
        assert_eq!(copper.len(), 1, "copper-to-hole findings merge across layers and directions");
        assert_eq!(copper[0].actual, Some(Measure::Len(125_000)), "via copper-to-hole gap");

        board.vias[1].at.x = 5_600_000;
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::HoleToHole), "300000 nm hole spacing is legal");

        board.vias[1].at.x = 7_000_000;
        let id = board.alloc_id();
        board.tracks.push(Track {
            id,
            net: NetId(0),
            layer: LayerId::F_CU,
            w: 200_000,
            widths: None,
            path: Path::new(P::new(3_000_000, 5_000_000)).line(board.vias[0].at),
            ends: [Anchor::Free, Anchor::Via(board.vias[0].id)],
            locked: false,
            origin: Origin::Authored,
        });
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::HoleClearance), "own drill and same-net track ending on a via are exempt");

        let hole_id = board.alloc_id();
        board.holes.push(Hole {
            id: hole_id,
            name: "h".into(),
            at: P::new(12_000_000, 12_000_000),
            drill: 1_000_000,
            pad: None,
            net: None,
            plated: false,
            origin: Origin::Authored,
        });
        let track_id = board.alloc_id();
        board.tracks.push(Track {
            id: track_id,
            net: NetId(1),
            layer: LayerId::F_CU,
            w: 200_000,
            widths: None,
            path: Path::new(P::new(11_000_000, 12_800_000)).line(P::new(12_000_000, 12_800_000)).line(P::new(13_000_000, 12_800_000)),
            ends: [Anchor::Free; 2],
            locked: false,
            origin: Origin::Authored,
        });
        let full = run(&board, None);
        let copper: Vec<_> = full.violations.iter().filter(|v| v.kind == CheckKind::HoleClearance).collect();
        assert_eq!(copper.len(), 1, "one unplated-hole clearance finding across track pieces");
        assert_eq!(copper[0].actual, Some(Measure::Len(200_000)), "copper-to-hole actual gap");
        assert_eq!(copper[0].required, Some(Measure::Len(250_000)), "standard copper-to-hole requirement");
        board.tracks[1].path = Path::new(P::new(13_000_000, 12_800_000)).line(P::new(12_000_000, 12_800_000)).line(P::new(11_000_000, 12_800_000));
        let reversed = run(&board, None);
        let reverse_copper: Vec<_> = reversed.violations.iter().filter(|v| v.kind == CheckKind::HoleClearance).collect();
        let mut expected = copper[0].clone();
        expected.id = content_id(&board, &expected.kind, &expected.items, &expected.rule);
        assert_eq!(reverse_copper, vec![&expected], "equal gaps choose the same point regardless of piece order");
        let delta = BoardDelta {
            changed: vec![hole_id, track_id],
            bbox: BBox::point(board.holes[0].at).inflate(board.holes[0].drill / 2).union(board.tracks[1].bbox()),
            ..BoardDelta::default()
        };
        let update = run(&board, Some(&delta));
        let local: Vec<_> = update.violations.iter().filter(|v| v.kind == CheckKind::HoleClearance).collect();
        assert_eq!(local.len(), 1, "delta reports the local hole clearance");
        assert_eq!(local[0].id, reverse_copper[0].id, "delta preserves the full-run hole-clearance id");
        assert_eq!(local[0], reverse_copper[0], "delta preserves the full-run worst fault");

        board.tracks[1].path = Path::new(P::new(11_000_000, 12_860_000)).line(P::new(12_000_000, 12_860_000)).line(P::new(13_000_000, 12_860_000));
        let result = run(&board, None);
        assert!(!result.violations.iter().any(|v| v.kind == CheckKind::HoleClearance), "260000 nm copper-to-hole spacing is legal");
    }

    /// A pour neck under the minimum width between two pads of the pour's net is one error at the
    /// neck, from the full and the indexed run alike: it names the passage's width (the widest disc
    /// through it) and the pad of the smaller part. A passage wider than the minimum raises nothing.
    #[test]
    fn pour_neck_under_the_minimum_width_is_one_finding_at_the_neck() {
        let (h, wall) = (100_000.0f64, 10_000.0f64);
        for passage in [150_000, 260_000] {
            let (mut board, _, centre) = crate::zone::tests::walled_board(
                &[(0, passage)],
                &[(-500_000, fab_elec_parts::ZoneConnect::Solid), (800_000, fab_elec_parts::ZoneConnect::Solid)],
            );
            board.zones[0].settings.min_width = 200_000;
            board.placed[0].refdes = "J1".into();
            board.placed[1].refdes = "U7".into();
            let rules = resolve_rules(&board, None);
            let fills = fill_all(&board, &rules);
            let conn = connect(&board, &fills);
            let full = drc(&board, &conn, &rules, &fills, None).violations;
            let index = PieceIndex::build(&board, &fills);
            let indexed = drc_indexed(&[], &index, &board, &conn, &rules, &BoardDelta::default(), &[]);
            let necks = |list: &[Violation]| list.iter().filter(|v| v.kind == CheckKind::PourNeck).cloned().collect::<Vec<_>>();
            assert_eq!(necks(&indexed), necks(&full), "passage {passage}: the indexed run reports the same necks");
            let found = necks(&full);
            if passage > 200_000 {
                assert!(found.is_empty() && conn.unrouted.is_empty(),
                    "passage {passage}: a passage wider than the minimum joins the pads");
                continue;
            }
            assert_eq!(found.len(), 1, "passage {passage}: one neck, one finding");
            let v = &found[0];
            // Each side's fill grows from the tip of its eroded copper, where two circles of radius h
            // around the wall ends meet; the discs from both tips leave a waist at the centre line.
            let tip = wall + (h * h - (passage as f64 / 2.0).powi(2)).sqrt();
            let expected = 2.0 * (h * h - tip * tip).sqrt();
            let Some(Measure::Len(w)) = v.actual else { panic!("the finding measures the neck: {v:?}") };
            assert!((w as f64 - expected).abs() <= (2 * ARC_ERR) as f64, "passage {passage}: width {w} near {expected:.0}");
            assert_eq!(v.required, Some(Measure::Len(200_000)), "the minimum width is the zone's");
            assert_eq!(v.severity, Severity::Error, "a neck is an error");
            assert_eq!(
                v.message,
                format!("Pour neck narrower than the minimum width ({} mm < 0.200 mm): the own pour does not connect pad 1 of J1 here.", mm3(w)),
                "the smaller part's pad is named"
            );
            let pad = sel_of(&board, ConnItem::Pad { placed: board.placed[0].id, pad: PadIx(0) });
            let zone = Sel::Item(board.unit, board.zones[0].id);
            assert!(v.items.contains(&pad) && v.items.contains(&zone), "it names the zone and the pad");
            let at = v.at.expect("the finding has a place");
            let near = (at.x - centre.x).abs() <= 100_000 && (at.y - centre.y).abs() <= 100_000;
            assert!(near, "passage {passage}: at the neck, {at:?}");
        }
    }
}
