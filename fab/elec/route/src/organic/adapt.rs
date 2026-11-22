//! Fab's side of the seam (not a port): the relaxer's `OBoard` from a `Board`, its `RuleMatrix`
//! and its `RStack`, and the conversions back.
//!
//! Frames and units: the port works in the reference's frame, mm with Y down. `to_ref` flips Y
//! and converts nm to mm once (f64, then one rounding to f32); `from_ref` converts back and rounds
//! to the nm grid once. Rotations keep their sign across the flip: the reference measures them
//! counter-clockwise as seen on its Y-down screen, which is Fab's counter-clockwise on its Y-up
//! board.
//!
//! What the adapter decides (each a ledger row):
//! - one `Pad` per distinct copper shape of a Fab pad, on the layers that carry it (`piece_of`
//!   links the pieces); a plated hole with a ring is a pad of no footprint. An unplated hole (an
//!   unplated pad or board hole, which has no copper) is a pad the shape of its hole on every
//!   copper layer, with its drill: the relaxer keeps the hole clearance from its wall, as DRC
//!   does, and its copper clearance besides (the safe side). A pad with a drill keeps it, plated
//!   or not;
//! - shapes: circle, rectangle (rounded or not) and stadium map exactly; a polygon pad with one
//!   ring and no holes is a `Polygon` pad; any other shape becomes the rectangle of its box
//!   (larger, so the relaxer stays on the safe side);
//! - clearances: `ClearanceTable` holds `RuleMatrix::clearance` for every pair of the board's rule
//!   keys (including a via key for every net's class) on every copper layer, plus `MARGIN_NM`;
//!   its `max` covers every row, while `DesignRules::min_clearance` (the floor) is the smallest
//!   over the rows of copper the board has; `DesignRules` holds the edge and hole rules plus the
//!   same margin. It replaces the reference's net clearances and pad-clearance policy;
//! - each net's through-via diameter/drill comes from `RuleMatrix::via(class)`, in mm; a via
//!   keeps Fab's kind (`ViaType`), so the lift can give it back;
//! - copper the router made (`Origin::Solved`) is the relaxer's to move; every other origin is
//!   `imported` (kept), and locked items are `locked`;
//! - arcs (outline, cutouts, tracks) become chords within `CHORD_ERR_NM`; that error is part of
//!   the margin;
//! - zones become one `Zone` per outer ring of their area (holes left out: a via in a hole counts
//!   as inside, so the net simplifier keeps it); a board-wide zone takes the outline;
//! - keepouts that forbid tracks or vias (the board's that reach copper, the footprints') become
//!   one `Keepout` per outer ring (holes left out: the larger area, the safe side), with Fab's 1 nm
//!   keepout gap plus the margin;
//! - a pad's number is its footprint pad's `no`, interned in pad order (equal numbers, equal
//!   index; the finish passes compare them within one `component`); a hole has none;
//! - a net's diff pair partner and gap are Fab's (`RuleMatrix::partner`: the pair's own gap, else
//!   the larger of its two classes' gaps; the reference's Board::diffPairGap takes the class gap
//!   only when both nets share a class, else the board's pair spacing), and the pair spacing is
//!   the default class's pair gap;
//! - the routing intents Fab does not have are written as absent: no pad fan-out style, no net
//!   sized for current.
//!
//! Cost: O(items + rule keys^2 x layers), once per relax job; nothing per frame.

use super::geom::Vec2;
use super::model::{ClearanceTable, DesignRules, Keepout, LayerInfo, LayerIntent, Net, OBoard, Pad, PadShape, Trace, TracePoint, Via, ViaType, Zone, NONE};
use crate::problem::RStack;
use fab_elec_board::{Anchor, Area, Board, ConnItem, Origin, RuleItem, RuleKey, RuleMatrix, ViaKind};
use fab_elec_board::zone::{static_masks, zone_gap};
use fab_elec_core::geom::ArcGeom;
use fab_elec_core::{Angle, ClassId, ItemId, LayerId, NetId, Nm, PadIx, Path, Seg, Shape, P};
use fab_elec_parts::Forbid;
use std::collections::BTreeMap;

/// Chord tolerance of arcs the port sees as polylines, nm.
pub const CHORD_ERR_NM: Nm = 1_000;
/// What every clearance the port guards holds on top of the rule: float rounding of the mm model
/// (Fable 2: 20 nm) plus the chord tolerance.
pub const MARGIN_NM: Nm = 20 + CHORD_ERR_NM;

const NM_PER_MM: f64 = 1_000_000.0;

/// A Fab point in the port's frame.
#[inline]
pub fn to_ref(p: P) -> Vec2 {
    Vec2::new((p.x as f64 / NM_PER_MM) as f32, (-(p.y as f64) / NM_PER_MM) as f32)
}

/// A port point on Fab's nm grid.
#[inline]
pub fn from_ref(v: Vec2) -> P {
    P::new((v.x as f64 * NM_PER_MM).round() as Nm, (-(v.y as f64) * NM_PER_MM).round() as Nm)
}

#[inline]
pub fn mm(v: Nm) -> f32 {
    (v as f64 / NM_PER_MM) as f32
}

/// A port length (mm) on Fab's nm grid.
#[inline]
pub fn nm(v: f32) -> Nm {
    (v as f64 * NM_PER_MM).round() as Nm
}

/// Where an `OBoard` pad came from.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum PadSrc {
    Pad { placed: ItemId, pad: PadIx },
    Hole(ItemId),
}

/// An `OBoard` and the Fab identity of each of its objects (same indices).
#[derive(Clone, Debug)]
pub struct Adapted {
    pub board: OBoard,
    pub pads: Vec<PadSrc>,
    pub traces: Vec<ItemId>,
    pub vias: Vec<ItemId>,
    pub nets: Vec<NetId>,
    pub layers: Vec<LayerId>,
    pub rules: Vec<RuleKey>,
}

impl Adapted {
    pub fn layer_of(&self, l: LayerId) -> i32 {
        self.layers.iter().position(|x| *x == l).map_or(NONE, |i| i as i32)
    }
    pub fn rule_of(&self, k: RuleKey) -> u16 {
        self.rules.binary_search(&k).map_or(0, |i| i as u16)
    }
}

/// Append every zone's pour-path clearance row at once, returning rows in `zones` order.
/// Each new pair is the zone gap against the other row's key plus `margin`, in mm. Between two
/// new rows, the later zone supplies the gap, as with successive single-row appends. A single-layer
/// table over several adapter layers uses their maximum. Old rows and the clearance floor are unchanged.
/// `ob` must use `a`'s original rows and layer numbering; pass all pour zones in this one call.
/// Empty `zones` is a no-op. Panics if a zone is absent or has no net, the table dimensions differ,
/// or the appended rows exceed u16 indices.
pub fn add_pour_rows(
    ob: &mut OBoard,
    a: &Adapted,
    board: &Board,
    rules: &RuleMatrix,
    zones: &[ItemId],
    margin: Nm,
) -> Vec<u16> {
    if zones.is_empty() { return Vec::new(); }
    let pours: Vec<_> = zones.iter().map(|&zone| {
        let z = board.zones.iter().find(|z| z.id == zone).expect("pour row requires an existing zone");
        let net = z.net.expect("pour row requires a zone net");
        let key = RuleKey {
            class: board.nets.get(net.ix()).map_or(ClassId::default(), |net| net.class),
            item: RuleItem::Zone,
        };
        (z, key)
    }).collect();
    let old = &ob.clr;
    assert_eq!(old.rows, a.rules.len().max(1), "pour rows require the adapter's original clearance rows");
    let layers = old.layers;
    assert!(layers > 0 && (layers == 1 || layers == a.layers.len()), "pour row requires adapter layers");
    assert_eq!(old.mm.len(), old.rows * old.rows * layers, "pour row requires a complete clearance table");
    let rows = old.rows.checked_add(pours.len()).expect("pour row count overflow");
    assert!(rows <= u16::MAX as usize + 1, "pour rows exceed u16 rule indices");
    let added: Vec<u16> = (old.rows..rows).map(|row| row as u16).collect();
    let mut keys = a.rules.clone();
    if keys.is_empty() {
        // adapt reserves row zero even when the input has no copper or nets.
        keys.push(RuleKey { class: ClassId::default(), item: RuleItem::Track });
    }
    keys.extend(pours.iter().map(|(_, key)| *key));
    let mut values = vec![0.0; rows * rows * layers];
    for i in 0..old.rows {
        let from = i * old.rows * layers;
        let to = i * rows * layers;
        values[to..to + old.rows * layers].copy_from_slice(&old.mm[from..from + old.rows * layers]);
    }
    let mut max = old.max;
    for (i, &(z, zkey)) in pours.iter().enumerate() {
        let row = old.rows + i;
        for (k, &key) in keys[..=row].iter().enumerate() {
            for li in 0..layers {
                let rule = if layers == 1 {
                    a.layers.iter().map(|&l| rules.clearance(zkey, key, l)).max().unwrap_or(0)
                } else {
                    rules.clearance(zkey, key, a.layers[li])
                };
                let value = mm(zone_gap(z, rule)) + mm(margin);
                values[(row * rows + k) * layers + li] = value;
                values[(k * rows + row) * layers + li] = value;
                max = max.max(value);
            }
        }
    }
    ob.clr = ClearanceTable { rows, layers, mm: values, max };
    for &row in &added {
        if let Err(i) = ob.pour_rules.binary_search(&row) {
            ob.pour_rules.insert(i, row);
        }
    }
    added
}

/// Static pour masks as net-specific keepout edges in adapter units and layer numbering.
/// Both outer and hole rings are retained: the engine measures edges, not polygon containment.
/// The caller passes one past the board's largest keepout id as `first_id` and threads it across
/// zones by adding the number of returned keepouts. Skipped layers outside 0..64 consume no id.
/// Missing zones and zones without a net in the adapter produce no avoid areas.
pub fn pour_avoid_areas(
    board: &Board, rules: &RuleMatrix, zone: ItemId, a: &Adapted, first_id: i32,
) -> Vec<Keepout> {
    let Some(z) = board.zones.iter().find(|z| z.id == zone) else { return Vec::new() };
    let Some(net) = z.net.and_then(|net| a.nets.iter().position(|&n| n == net)) else { return Vec::new() };
    let mut next = i64::from(first_id);
    let mut out = Vec::new();
    for (layer, masks) in static_masks(board, rules, zone) {
        let layer = a.layer_of(layer);
        if !(0..64).contains(&layer) { continue; }
        for ring in masks.rings() {
            let id = i32::try_from(next).expect("pour keepout id exceeds i32");
            next += 1;
            out.push(Keepout {
                id, poly: ring_ref(ring), layer_mask: 1u64 << layer, clearance: 0.0,
                no_tracks: true, no_vias: false, only_net: net as i32,
            });
        }
    }
    out
}

fn mask_of(layers: &[i32]) -> u64 {
    layers.iter().filter(|l| (0..64).contains(*l)).fold(0, |m, l| m | 1u64 << l)
}

fn imported(o: &Origin) -> bool {
    !matches!(o, Origin::Solved { .. })
}

/// Where a polyline point of a track came from.
#[derive(Copy, Clone)]
enum At {
    /// Path vertex `k`.
    Vertex(usize),
    /// A chord vertex inside the arc that ends at path vertex `k`.
    InArc(usize),
}

/// The points of a path, arcs as chords (vertices on the arc).
fn path_points(path: &Path) -> Vec<(P, At)> {
    let mut out = vec![(path.start, At::Vertex(0))];
    for (k, (from, s)) in path.steps().enumerate() {
        match s {
            Seg::Line(b) => out.push((b, At::Vertex(k + 1))),
            Seg::Arc { mid, end } => match ArcGeom::new(from, mid, end) {
                Some(g) => {
                    let mut pts = Vec::new();
                    g.chord_points(CHORD_ERR_NM, &mut pts);
                    let n = pts.len();
                    for (i, q) in pts.into_iter().enumerate() {
                        out.push((q, if i + 1 == n { At::Vertex(k + 1) } else { At::InArc(k + 1) }));
                    }
                }
                None => out.push((end, At::Vertex(k + 1))),
            },
        }
    }
    out
}

fn ring_ref(ring: &[P]) -> Vec<Vec2> {
    let mut v: Vec<Vec2> = ring.iter().map(|p| to_ref(*p)).collect();
    if v.len() >= 2 {
        let (a, b) = (v[0], v[v.len() - 1]);
        if a.x == b.x && a.y == b.y {
            v.pop();
        }
    }
    v
}

/// The board's copper shape of one pad piece as a reference pad (geometry fields only).
fn pad_geometry(shape: &Shape) -> Pad {
    let bb = shape.bbox();
    let size = Vec2::new(mm(bb.width() as Nm), mm(bb.height() as Nm));
    let rect = |c: P, half: P, rot: Angle, kind: PadShape, ratio: f32| Pad {
        center: to_ref(c),
        size,
        local_size: Vec2::new(mm(2 * half.x), mm(2 * half.y)),
        rotation: rot.degrees() as f32,
        shape: kind,
        round_rect_ratio: ratio,
        ..Pad::default()
    };
    match shape {
        Shape::Circle { c, r } => rect(*c, P::new(*r, *r), Angle::ZERO, PadShape::Circle, 0.0),
        Shape::Rect { c, half, rot, corner } => {
            let corner = (*corner).max(0).min(half.x.min(half.y));
            if corner == 0 {
                rect(*c, *half, *rot, PadShape::Rect, 0.0)
            } else {
                let ratio = (corner as f64 / (2.0 * half.x.min(half.y) as f64)) as f32;
                rect(*c, *half, *rot, PadShape::RoundRect, ratio)
            }
        }
        Shape::Oval { c, half, rot } => rect(*c, *half, *rot, PadShape::Oval, 0.0),
        Shape::Poly(set) if set.polygons().len() == 1 && set.polygons()[0].holes.is_empty() => {
            let c = bb.center();
            let o = to_ref(c);
            let polygon = ring_ref(&set.polygons()[0].outer).into_iter().map(|v| v - o).collect();
            Pad { center: o, size, local_size: size, rotation: 0.0, shape: PadShape::Polygon, polygon, ..Pad::default() }
        }
        other => {
            let bb = other.bbox();
            rect(bb.center(), P::new((bb.width() / 2) as Nm, (bb.height() / 2) as Nm), Angle::ZERO, PadShape::Rect, 0.0)
        }
    }
}

/// The drill of a pad piece: diameter, extent along the pad's axes, and the rotation the pad
/// frame must have (a circle takes its slot's angle).
fn drill_fields(p: &mut Pad, size: P, rot: Angle) {
    let (sx, sy) = (mm(size.x), mm(size.y));
    p.drill_diameter = sx.min(sy);
    if size.x == size.y {
        p.drill_size = Vec2::new(0.0, 0.0);
        return;
    }
    let diff = rot.sub(Angle::deg(p.rotation as f64));
    if p.shape == PadShape::Circle {
        p.rotation = rot.degrees() as f32;
        p.drill_size = Vec2::new(sx, sy);
    } else {
        match diff.quarter_turns() {
            Some(0) | Some(2) => p.drill_size = Vec2::new(sx, sy),
            Some(1) | Some(3) => p.drill_size = Vec2::new(sy, sx),
            // a slot turned against its pad: a round hole as wide as the slot is long (larger)
            _ => {
                p.drill_diameter = sx.max(sy);
                p.drill_size = Vec2::new(0.0, 0.0);
            }
        }
    }
}

struct PadPiece {
    shape: Shape,
    layers: Vec<i32>,
    net: Option<NetId>,
    key: RuleKey,
}

/// The relaxer's view of `board`: every pad, via and track, with nets, layers and rules.
pub fn adapt(board: &Board, rules: &RuleMatrix, stack: &RStack) -> Adapted {
    let layers = stack.layers.clone();
    let layer_ix = |l: LayerId| layers.iter().position(|x| *x == l).map_or(NONE, |i| i as i32);
    let n_layers = layers.len().max(1);

    // copper pieces of every item, grouped per owner
    let mut pad_pieces: BTreeMap<(u8, ItemId, u16), Vec<PadPiece>> = BTreeMap::new();
    let mut item_key: BTreeMap<ItemId, RuleKey> = BTreeMap::new();
    let mut keys: Vec<RuleKey> = Vec::new();
    for c in board.all_copper() {
        let key = c.key();
        keys.push(key);
        let owner = match c.item {
            ConnItem::Pad { placed, pad } => Some((0u8, placed, pad.0)),
            ConnItem::Hole(h) => Some((1u8, h, 0)),
            ConnItem::Track { item, .. } | ConnItem::Via(item) => {
                item_key.entry(item).or_insert(key);
                None
            }
            ConnItem::Zone { .. } => None,
        };
        if let Some(o) = owner {
            let l = layer_ix(c.layer);
            let list = pad_pieces.entry(o).or_default();
            match list.iter_mut().find(|q| q.shape == c.shape) {
                Some(q) => q.layers.push(l),
                None => list.push(PadPiece { shape: c.shape, layers: vec![l], net: c.net, key }),
            }
        }
    }
    // unplated holes (see the module notes): pads with no copper and unplated board holes
    let all_layers: Vec<i32> = (0..layers.len() as i32).collect();
    let mut unplated = Vec::new();
    for p in &board.placed {
        for pad in 0..p.footprint.pads.len() as u16 {
            if let Some(hole) = p.drill_hole(PadIx(pad)).filter(|h| !h.plated) {
                unplated.push(((0u8, p.id, pad), hole));
            }
        }
    }
    for h in &board.holes {
        let hole = h.drill_hole();
        if !hole.plated {
            unplated.push(((1u8, h.id, 0), hole));
        }
    }
    let mut hole_keys = Vec::new();
    for (o, hole) in unplated {
        if pad_pieces.contains_key(&o) {
            continue;
        }
        let class = hole.net.and_then(|n| board.nets.get(n.ix())).map_or(ClassId::default(), |n| n.class);
        let key = RuleKey { class, item: RuleItem::Hole };
        hole_keys.push(key);
        pad_pieces.insert(o, vec![PadPiece { shape: hole.shape(), layers: all_layers.clone(), net: hole.net, key }]);
    }
    // The board's own copper keys: the clearance floor is theirs alone, so the via row of a class
    // with no via on the board (below) and the rows of unplated holes never lower it.
    let mut present = keys.clone();
    present.sort();
    present.dedup();
    // New vias need the same key as ConnItem::Via copper (zone::rule_item / class_of),
    // even when this class has no via on the input board. Keep all rows in sorted order.
    for net in &board.nets { keys.push(RuleKey { class: net.class, item: RuleItem::Via }); }
    keys.extend(hole_keys);
    keys.sort();
    keys.dedup();
    let on_board: Vec<bool> = keys.iter().map(|k| present.binary_search(k).is_ok()).collect();
    let rule_of = |k: RuleKey| keys.binary_search(&k).map_or(0, |i| i as u16);

    // rules
    let margin = mm(MARGIN_NM);
    let rows = keys.len().max(1);
    let mut clr = ClearanceTable { rows, layers: n_layers, mm: vec![0.0; rows * rows * n_layers], max: 0.0 };
    let mut floor = f32::MAX;
    for (a, ka) in keys.iter().enumerate() {
        for (b, kb) in keys.iter().enumerate() {
            for (li, l) in layers.iter().enumerate() {
                let v = mm(rules.clearance(*ka, *kb, *l)) + margin;
                clr.mm[(a * rows + b) * n_layers + li] = v;
                if on_board[a] && on_board[b] {
                    floor = floor.min(v);
                }
                // every row: the reach of a via a pass adds, as the reference's maxClearance
                // covers every net's class
                clr.max = clr.max.max(v);
            }
        }
    }
    if floor == f32::MAX {
        floor = margin;
    }
    let def = ClassId(0);
    let via = rules.via(def);
    let design = DesignRules {
        default_trace_width: mm(rules.class(def).track.unwrap_or(rules.track_min(def))),
        min_trace_width: mm(rules.track_min(def)),
        min_clearance: floor,
        via_diameter: mm(via.pad),
        via_drill: mm(via.drill),
        board_edge_clearance: mm(rules.edge()) + margin,
        hole_clearance: mm(rules.hole_clearance()) + margin,
        hole_to_hole: mm(rules.hole_to_hole()) + margin,
        diff_pair_spacing: mm(rules.pair_gap(def)),
        ..DesignRules::default()
    };

    // nets: one per board net, in NetId order
    let nets_src: Vec<NetId> = (0..board.nets.len() as u32).map(NetId).collect();
    let net_ix = |n: Option<NetId>| n.map_or(NONE, |n| n.0 as i32);

    let mut ob = OBoard {
        layers: (0..layers.len()).map(|i| LayerInfo { index: i as i32, intent: LayerIntent::Signal, plane_net: NONE }).collect(),
        nets: board.nets.iter().enumerate().map(|(i, net)| {
            let via = rules.via(net.class);
            let pair = rules.partner(NetId(i as u32));
            Net {
                id: i as i32, via_diameter: mm(via.pad), via_drill: mm(via.drill),
                via_rule: rule_of(RuleKey { class: net.class, item: RuleItem::Via }),
                via_in_pad: rules.via_in_pad(net.class),
                is_diff_pair: pair.is_some(),
                diff_pair_partner: pair.map_or(NONE, |(q, _)| q.0 as i32),
                pair_gap: pair.map_or(0.0, |(_, gap)| mm(gap)),
                ..Net::default()
            }
        }).collect(),
        rules: design,
        clr,
        ..OBoard::default()
    };

    // outline and cutouts
    for (k, c) in board.outline.iter().enumerate() {
        let ring = ring_ref(&c.to_ring(CHORD_ERR_NM));
        if k == 0 {
            ob.outline = ring;
        } else {
            ob.cutouts.push(ring);
        }
    }
    for c in &board.cutouts {
        ob.cutouts.push(ring_ref(&c.contour.to_ring(CHORD_ERR_NM)));
    }

    // zones (copper pours): one per outer ring of their area
    for z in &board.zones {
        let net = net_ix(z.net);
        let layer_mask = mask_of(&z.layers.iter().map(layer_ix).collect::<Vec<_>>());
        let rings: Vec<Vec<Vec2>> = match &z.area {
            Area::Board => vec![ob.outline.clone()],
            Area::Poly(ps) => ps.polygons().iter().map(|p| ring_ref(&p.outer)).collect(),
        };
        for outline in rings {
            ob.zones.push(Zone { net, layer_mask, outline, is_rule_area: false });
        }
    }

    // keepouts that forbid tracks or vias: the board's that reach copper, then the footprints'
    // in placed order; one per outer ring of their area (holes left out: the larger area)
    let keepout = |ob: &mut OBoard, area: &Shape, layer_mask: u64, forbid: Forbid| {
        let (no_tracks, no_vias) = (forbid.has(Forbid::TRACKS), forbid.has(Forbid::VIAS));
        let Shape::Poly(ps) = area else { return };
        if layer_mask == 0 || !(no_tracks || no_vias) {
            return;
        }
        for p in ps.polygons() {
            let id = ob.keepouts.len() as i32;
            ob.keepouts.push(Keepout { id, poly: ring_ref(&p.outer), layer_mask, clearance: mm(1) + margin,
                no_tracks, no_vias, only_net: NONE });
        }
    };
    for k in board.keepouts.iter().filter(|k| fab_elec_board::drc::reaches_copper(k.z)) {
        let layer_mask = mask_of(&k.layers.iter().map(layer_ix).collect::<Vec<_>>());
        keepout(&mut ob, &Shape::Poly(k.area.clone()), layer_mask, k.forbid);
    }
    let copper_count = board.copper_count();
    for p in &board.placed {
        for k in &p.footprint.keepouts {
            let on: Vec<i32> = layers.iter().enumerate().filter(|(_, l)| k.layers.has(p.fp_layer(**l, copper_count))).map(|(i, _)| i as i32).collect();
            keepout(&mut ob, &p.shape_to_board(&Shape::Poly(k.area.clone())), mask_of(&on), k.forbid);
        }
    }

    // pads (footprint pads in placed order, then board holes)
    let placed_ix: BTreeMap<ItemId, usize> = board.placed.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let mut pads_src = Vec::new();
    let mut pad_index: BTreeMap<(u8, ItemId, u16), Vec<usize>> = BTreeMap::new();
    // pad numbers, interned in pad order (ADAPTED: `Pad::name` within `componentRef`)
    let mut numbers: BTreeMap<String, i32> = BTreeMap::new();
    let mut order: Vec<(u8, ItemId, u16)> = pad_pieces.keys().copied().collect();
    order.sort_by_key(|(kind, id, pad)| (*kind, if *kind == 0 { placed_ix.get(id).copied().unwrap_or(usize::MAX) } else { id.0 as usize }, *pad));
    for o in order {
        let (kind, id, pad) = o;
        let drill = if kind == 0 {
            board.placed_item(id).and_then(|p| p.drill_hole(PadIx(pad)))
        } else {
            board.holes.iter().find(|h| h.id == id).map(|h| h.drill_hole())
        };
        let first = ob.pads.len();
        // a hole (a board hole, an unplated pad) has no number
        let number = if kind == 0 && drill.as_ref().map_or(true, |d| d.plated) {
            board.placed_item(id).and_then(|p| p.footprint.pads.get(pad as usize)).map_or(NONE, |fp| {
                let next = numbers.len() as i32;
                *numbers.entry(fp.no.clone()).or_insert(next)
            })
        } else {
            NONE
        };
        for piece in &pad_pieces[&o] {
            let mut p = pad_geometry(&piece.shape);
            p.id = ob.pads.len() as i32;
            p.layer_mask = mask_of(&piece.layers);
            p.net = net_ix(piece.net);
            p.rule = rule_of(piece.key);
            p.component = if kind == 0 { placed_ix.get(&id).map_or(NONE, |i| *i as i32) } else { NONE };
            p.piece_of = if ob.pads.len() == first { NONE } else { first as i32 };
            p.number = number;
            // ADAPTED: the hard level of a same-net via on this pad (holes are netless)
            p.via_drill_gap = if kind == 0 { mm(rules.via_drill_to_board_pad(board, id, PadIx(pad))) + margin } else { 0.0 };
            match &drill {
                Some(d) => {
                    p.is_smd = false;
                    drill_fields(&mut p, d.size, d.rot);
                }
                _ => {
                    p.is_smd = true;
                    p.drill_diameter = 0.0;
                }
            }
            pad_index.entry(o).or_default().push(ob.pads.len());
            ob.pads.push(p);
            pads_src.push(if kind == 0 { PadSrc::Pad { placed: id, pad: PadIx(pad) } } else { PadSrc::Hole(id) });
        }
    }

    // vias
    let mut via_ix: BTreeMap<ItemId, i32> = BTreeMap::new();
    let mut vias_src = Vec::new();
    for v in &board.vias {
        let i = ob.vias.len() as i32;
        via_ix.insert(v.id, i);
        let key = item_key.get(&v.id).copied().unwrap_or(RuleKey { class: def, item: RuleItem::Via });
        ob.vias.push(Via {
            id: i,
            net: v.net.0 as i32,
            pos: to_ref(v.at),
            diameter: mm(v.pad),
            drill: mm(v.drill),
            layer_start: layer_ix(v.span.0),
            layer_end: layer_ix(v.span.1),
            is_fixed: v.locked || imported(&v.origin),
            locked: v.locked,
            imported: imported(&v.origin),
            rule: rule_of(key),
            kind: match v.kind {
                ViaKind::Through => ViaType::Through,
                ViaKind::Blind => ViaType::Blind,
                ViaKind::Buried => ViaType::Buried,
                ViaKind::Micro => ViaType::Micro,
            },
            src: i,
            ..Via::default()
        });
        vias_src.push(v.id);
    }

    // tracks
    let mut traces_src = Vec::new();
    for t in &board.tracks {
        let layer = layer_ix(t.layer);
        let pts = path_points(&t.path);
        let width_at = |at: At| -> f32 {
            let w = |i: usize| t.widths.as_ref().and_then(|ws| ws.get(i).copied()).unwrap_or(t.w);
            match at {
                At::Vertex(k) => mm(w(k)),
                // inside an arc: the wider of the arc's ends
                At::InArc(e) => mm(w(e - 1).max(w(e))),
            }
        };
        let end_anchor = |a: Anchor| -> (i32, i32) {
            // (terminal pad, attached via)
            let pad_on = |o: (u8, ItemId, u16)| {
                pad_index.get(&o).and_then(|v| v.iter().copied().find(|i| ob.pads[*i].layer_mask & (1u64 << layer.max(0)) != 0)).map_or(NONE, |i| i as i32)
            };
            match a {
                Anchor::Pad { placed, pad } => (pad_on((0, placed, pad.0)), NONE),
                Anchor::Hole(h) => (pad_on((1, h, 0)), NONE),
                Anchor::Via(v) => (NONE, via_ix.get(&v).copied().unwrap_or(NONE)),
                Anchor::Free | Anchor::Track { .. } => (NONE, NONE),
            }
        };
        let n = pts.len();
        let points: Vec<TracePoint> = pts
            .iter()
            .enumerate()
            .map(|(i, (q, at))| {
                let mut tp = TracePoint { pos: to_ref(*q), width: width_at(*at), ..TracePoint::default() };
                if i == 0 || i + 1 == n {
                    let (pad, via) = end_anchor(t.ends[if i == 0 { 0 } else { 1 }]);
                    tp.terminal_pad = pad;
                    tp.attached_via = via;
                    tp.is_fixed = via == NONE;
                }
                tp
            })
            .collect();
        let key = item_key.get(&t.id).copied().unwrap_or(RuleKey { class: def, item: RuleItem::Track });
        let i = ob.traces.len() as i32;
        ob.traces.push(Trace {
            id: i,
            net: t.net.0 as i32,
            layer,
            width: mm(t.w),
            points,
            is_routed: true,
            locked: t.locked,
            imported: imported(&t.origin),
            rule: rule_of(key),
            ..Trace::default()
        });
        traces_src.push(t.id);
    }

    Adapted { board: ob, pads: pads_src, traces: traces_src, vias: vias_src, nets: nets_src, layers, rules: keys }
}
