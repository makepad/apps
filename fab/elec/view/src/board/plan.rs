//! Pure key-action plans over a committed design.

use fab_elec_board::text::{arrange_references, LabelBlocker};
use fab_elec_board::{Area, Board, BoardOp, Field, Flash, Item, Origin, Tent, Via, ViaKind, Zone, ZoneSettings};
use fab_elec_circuit::NetKind;
use fab_elec_core::{BBox, BoardId, ItemId, LayerId, LayerSet, NetId, Nm, PolySet, P};
use fab_elec_design::{Design, DesignOp, PartRef, Place};

/// A key action's result: the ops to commit as one undo step and its label.
#[derive(Clone, PartialEq, Debug)]
pub struct Plan {
    pub ops: Vec<DesignOp>,
    pub label: String,
}

/// Delete the item `item` of board `unit`: one `BoardOp::Remove`. A placed part goes back to the tray
/// (label "Unplace R3"); its tracks keep their copper and their pad ends become dangling (the op frees
/// the anchors). Copper the router laid is deleted like drawn copper, as Clear routes takes it. `Err`
/// is the chip text: why nothing happens, naming an action only where the person can take it (a
/// block's item names its block and no action: nothing detaches it yet).
pub fn delete(design: &Design, unit: BoardId, item: ItemId) -> Result<Plan, String> {
    let board_unit = design.unit(unit).ok_or_else(|| "nothing to delete here".to_owned())?;
    let board = board_unit.board.as_ref().ok_or_else(|| "nothing to delete here".to_owned())?;
    let item = board.item(item).ok_or_else(|| "nothing to delete here".to_owned())?;
    let name = item_name(board, &item);
    let sentence_name = capitalized(&name);
    let locked = match &item {
        Item::Placed(x) => x.locked,
        Item::Track(x) => x.locked,
        Item::Via(x) => x.locked,
        Item::Zone(x) => x.locked,
        Item::Graphic(x) => x.locked,
        Item::Text(x) => x.locked,
        Item::Keepout(_) | Item::Hole(_) | Item::Cutout(_) | Item::Tuning(_) => false,
    };
    if locked {
        return Err(format!("{sentence_name} is locked: unlock it to delete it"));
    }
    match item.origin() {
        Origin::Block { block, detached: false } => {
            let path = board_unit.circuit.tree.get(block.0 as usize).map(|block| block.path.as_str()).unwrap_or("its block");
            return Err(format!("{sentence_name} is part of block {path}"));
        }
        Origin::Derived { .. } => {
            return Err(format!("{sentence_name} follows another board: delete it at its source"));
        }
        Origin::Authored | Origin::Solved { .. } | Origin::Block { detached: true, .. } => {}
    }
    let label = if matches!(&item, Item::Placed(_)) {
        format!("Unplace {name}")
    } else {
        format!("Delete {name}")
    };
    Ok(Plan { ops: vec![DesignOp::Board { unit, op: BoardOp::Remove(item.id()) }], label })
}

/// Clear routes: every track and via of board `unit` that is neither locked nor a block's or another
/// board's, with the tunings laid on those tracks, as one undo step. `nets` limits it to those nets.
/// Tunings go first, then tracks, then the vias they ended on, so no op names a removed anchor. `Err`
/// is the chip text when there is nothing to clear. O(tracks + vias + tunings x their tracks).
pub fn clear_routes(design: &Design, unit: BoardId, nets: Option<&[NetId]>) -> Result<Plan, String> {
    let board = design.unit(unit).and_then(|unit| unit.board.as_ref()).ok_or_else(|| "no board here".to_owned())?;
    let of = |net: NetId| nets.is_none_or(|nets| nets.contains(&net));
    let tracks: Vec<ItemId> = board.tracks.iter().filter(|t| of(t.net) && clearable(&t.origin, t.locked)).map(|t| t.id).collect();
    let vias: Vec<ItemId> = board.vias.iter().filter(|v| of(v.net) && clearable(&v.origin, v.locked)).map(|v| v.id).collect();
    if tracks.is_empty() && vias.is_empty() {
        return Err(match nets {
            Some(_) => "no routes to clear on these nets".to_owned(),
            None => "no routes to clear".to_owned(),
        });
    }
    let gone = |id: &ItemId| tracks.contains(id);
    let tunings = board.tunings.iter()
        .filter(|tuning| clearable(&tuning.origin, false) && (tuning.host.iter().any(gone) || tuning.copper.iter().any(gone)))
        .map(|tuning| tuning.id);
    let ops = tunings.chain(tracks.iter().copied()).chain(vias.iter().copied())
        .map(|id| DesignOp::Board { unit, op: BoardOp::Remove(id) }).collect();
    let label = match nets {
        Some([net]) => format!("Clear the {} routes", net_name(board, *net)),
        Some(nets) => format!("Clear the routes of {} nets", nets.len()),
        None => "Clear routes".to_owned(),
    };
    Ok(Plan { ops, label })
}

/// The board has copper Clear routes would take. O(tracks + vias), stopping at the first.
pub fn has_clearable_routes(board: &Board) -> bool {
    board.tracks.iter().any(|t| clearable(&t.origin, t.locked)) || board.vias.iter().any(|v| clearable(&v.origin, v.locked))
}

/// Copper Clear routes takes: not locked, and the board's own (a block's undetached copper and
/// another board's projection are refused by the op).
fn clearable(origin: &Origin, locked: bool) -> bool {
    !locked && matches!(origin, Origin::Authored | Origin::Solved { .. } | Origin::Block { detached: true, .. })
}

/// Pour `net` (or, when `None`, the circuit's ground net, `ground_net`) on copper layer `layer` over the
/// whole board: one `BoardOp::Add(Item::Zone)` with `Area::Board` at the lowest priority, so every pour of
/// another net on the layer fills first and is cut out of it. Zones fill over each other at equal
/// priority (`zone.rs`), so when one of those sits at 0 too, every zone of the board goes up one step
/// first (one `SetField` each, their order kept). A layer another net already pours over the whole board
/// is refused: two board-wide pours of different nets would leave one of them nothing. `Err` is the
/// chip text.
pub fn pour(design: &Design, unit: BoardId, net: Option<NetId>, layer: LayerId) -> Result<Plan, String> {
    let board = design.unit(unit).and_then(|unit| unit.board.as_ref()).ok_or_else(|| "no board here".to_owned())?;
    let layer_name = layer.name();
    if !board.copper_set().has(layer) {
        return Err(format!("{layer_name} is not a copper layer: pour on copper"));
    }
    let net = net.or_else(|| ground_net(design, unit)).ok_or_else(|| "no net to pour: point at copper, or give the circuit a ground net".to_owned())?;
    let name = net_name(board, net);
    let whole = |zone: &Zone, layer: LayerId| zone.area == Area::Board && zone.layers.has(layer);
    if board.zones.iter().any(|zone| zone.net == Some(net) && whole(zone, layer)) {
        return Err(format!("{name} is already poured on {layer_name}"));
    }
    if let Some(other) = board.zones.iter().find(|zone| zone.net != Some(net) && whole(zone, layer)) {
        let other = other.net.map_or("a pour", |n| net_name(board, n));
        let free = board.copper_set().iter().find(|l| !board.zones.iter().any(|zone| whole(zone, *l)));
        return Err(match free {
            Some(free) => format!("{layer_name} is poured with {other}: make {} active to pour {name} there", free.name()),
            None => format!("{layer_name} is poured with {other}: delete that pour first"),
        });
    }
    let mut ops = Vec::new();
    if board.zones.iter().any(|zone| zone.net != Some(net) && zone.layers.has(layer) && zone.priority == 0) {
        for zone in &board.zones {
            let field = Field::Priority(zone.priority.saturating_add(1));
            ops.push(DesignOp::Board { unit, op: BoardOp::SetField { item: zone.id, field } });
        }
    }
    let zone = Zone {
        id: ItemId(board.next_item),
        net: Some(net),
        name: name.to_owned(),
        layers: LayerSet::one(layer),
        area: Area::Board,
        priority: 0,
        settings: ZoneSettings::default(),
        locked: false,
        origin: Origin::Authored,
    };
    ops.push(DesignOp::Board { unit, op: BoardOp::Add(Item::Zone(zone)) });
    Ok(Plan { ops, label: format!("Pour {name} on {layer_name}") })
}

/// The Via tool's click: a through via at `at` on `net` (the copper under the click), `pad` across
/// with a `drill` hole, spanning the board's copper top to bottom. One `BoardOp::Add`. `Err` is
/// the chip text: no copper under the click, or a pad that leaves no ring around its drill.
pub fn via(design: &Design, unit: BoardId, at: P, net: Option<NetId>, pad: Nm, drill: Nm) -> Result<Plan, String> {
    let board = design.unit(unit).and_then(|unit| unit.board.as_ref()).ok_or_else(|| "no board here".to_owned())?;
    let net = net.ok_or_else(|| "click on copper: a via takes the net of the copper under it".to_owned())?;
    if pad <= drill {
        return Err(format!("a {} via leaves no ring around its {} drill: pick a larger Via", mm(pad), mm(drill)));
    }
    let copper = board.stackup.copper();
    let (Some(&top), Some(&bottom)) = (copper.first(), copper.last()) else { return Err("the board has no copper layers".to_owned()) };
    let via = Via {
        id: ItemId(board.next_item),
        net,
        at,
        drill,
        pad,
        span: (top, bottom),
        kind: ViaKind::Through,
        tent: Tent::default(),
        backdrill: None,
        locked: false,
        origin: Origin::Authored,
        flash: Flash::All,
    };
    let label = format!("Via on {}", net_name(board, net));
    Ok(Plan { ops: vec![DesignOp::Board { unit, op: BoardOp::Add(Item::Via(via)) }], label })
}

/// The Zone tool's drag: a copper zone over the rectangle `area` on `layer`, of `net` (the copper
/// where the drag started) or else the circuit's ground. One `BoardOp::Add`, below every zone it
/// overlaps (priority 0). `Err` is the chip text.
pub fn zone(design: &Design, unit: BoardId, area: BBox, net: Option<NetId>, layer: LayerId) -> Result<Plan, String> {
    let board = design.unit(unit).and_then(|unit| unit.board.as_ref()).ok_or_else(|| "no board here".to_owned())?;
    let layer_name = layer.name();
    if !board.copper_set().has(layer) {
        return Err(format!("{layer_name} is not a copper layer: draw zones on copper"));
    }
    let shape = PolySet::rect(area);
    if shape.is_empty() {
        return Err("drag a rectangle: a zone needs some area".to_owned());
    }
    let net = net.or_else(|| ground_net(design, unit)).ok_or_else(|| "no net for the zone: start the drag on copper, or give the circuit a ground net".to_owned())?;
    let name = net_name(board, net);
    let zone = Zone {
        id: ItemId(board.next_item),
        net: Some(net),
        name: name.to_owned(),
        layers: LayerSet::one(layer),
        area: Area::Poly(shape),
        priority: 0,
        settings: ZoneSettings::default(),
        locked: false,
        origin: Origin::Authored,
    };
    Ok(Plan { ops: vec![DesignOp::Board { unit, op: BoardOp::Add(Item::Zone(zone)) }], label: format!("Zone {name} on {layer_name}") })
}

/// The gap between parts placed side by side, and below the board.
const PLACE_GAP: Nm = 2_000_000;

/// Update from the circuit: every part of the circuit the board does not place yet
/// (`tray::unplaced`), placed in rows under the board's outline, left to right from its left edge,
/// a new row once a row reaches the outline's width (at least 40 mm). One undo step. `Err` is the
/// chip text when every part is on the board already.
/// Cost: O(parts) once per press.
pub fn place_unplaced(design: &Design, unit: BoardId) -> Result<(Vec<Place>, String), String> {
    let board_unit = design.unit(unit).ok_or_else(|| "no board here".to_owned())?;
    let board = board_unit.board.as_ref().ok_or_else(|| "no board here".to_owned())?;
    let parts = super::tray::unplaced(design, unit);
    if parts.is_empty() {
        return Err("the board has every part of the circuit".to_owned());
    }
    let outline = board.outline_bbox();
    let gap = i64::from(PLACE_GAP);
    let (left, top) = if outline.is_empty() { (0, 0) } else { (i64::from(outline.min.x), i64::from(outline.max.y) + gap) };
    let width = if outline.is_empty() { 0 } else { outline.width() }.max(40_000_000);
    let (mut x, mut y, mut row) = (left, top, 0i64);
    let mut places = Vec::with_capacity(parts.len());
    for part in &parts {
        let Some(comp) = board_unit.circuit.comps.iter().find(|c| c.id == part.comp) else { continue };
        let (w, h) = (part.bbox.width().max(1), part.bbox.height().max(1));
        if x > left && x + w > left + width {
            (x, y, row) = (left, y + row + gap, 0);
        }
        // the footprint's own origin sits inside its box: shift so the box starts at (x, y)
        let at = P::new((x - i64::from(part.bbox.min.x)) as Nm, (y - i64::from(part.bbox.min.y)) as Nm);
        places.push(Place { unit, part: PartRef::Path(comp.path.clone()), at, rot: None, side: None });
        x += w + gap;
        row = row.max(h);
    }
    let label = match places.len() {
        1 => "Place 1 part from the circuit".to_owned(),
        n => format!("Place {n} parts from the circuit"),
    };
    Ok((places, label))
}

fn mm(nm: Nm) -> String {
    format!("{} mm", nm as f64 / 1e6)
}

/// How far from an edited item a part's reference label is looked at again: a few label lengths.
const LABEL_NEAR: Nm = 3_000_000;

/// The reference labels an edit disturbs, re-placed: board `unit` after this unit's board ops in
/// `ops`, then `arrange_references` over the parts within `LABEL_NEAR` of the items `items` and of
/// every item the ops move, where they are now (the items themselves included), and so over the
/// rest of their rows. A via a drag's answer pushes or carries lands on a label as a moved part
/// does. A part the ops move keeps its label where it rode to when that is clear (the person held
/// it there), and the others make room round it. The ops join the edit's own undo step; a row whose labels are still clear and alike
/// stays where it is. Empty when the edit does not apply to `board`.
/// Cost: one board clone and the edit's board ops as one batch, then one pass over the board that keeps only what lies
/// within reach of the labels being arranged; it runs once per drop or placement, never per drag sample.
pub fn label_ops(unit: BoardId, board: &Board, ops: &[DesignOp], items: &[ItemId]) -> Vec<DesignOp> {
    let mut after = board.clone();
    let mine = ops.iter().filter_map(|op| match op {
        DesignOp::Board { unit: u, op } if *u == unit => Some(op),
        _ => None,
    });
    if after.apply_batch_seen(mine, false, |_, _| {}).is_err() {
        return Vec::new();
    }
    let riding: Vec<ItemId> = ops.iter().filter_map(|op| match op {
        DesignOp::Board { unit: u, op: BoardOp::Move { item, .. } } if *u == unit => Some(*item),
        _ => None,
    }).collect();
    let near: Vec<_> = items.iter().chain(&riding).filter_map(|id| after.item(*id)).map(|it| it.bbox(&after).inflate(LABEL_NEAR)).collect();
    let parts: Vec<ItemId> = after.placed.iter().filter(|p| near.iter().any(|b| b.intersects(&p.bbox()))).map(|p| p.id).collect();
    arranged_ops(unit, &after, &parts, &riding)
}

/// The label ops a placement brings: on each board the `ops` add parts to, `label_ops` over those
/// parts, from that board as it is in `before`. A placement so lands with its labels, and its
/// neighbours' rows, beside their parts in its own undo step: the host's `place` resolution and the
/// tools that resolve a tray drop themselves share it.
/// Cost: per board one clone and one label pass near the placed parts (`label_ops`).
pub fn placement_labels(before: &Design, ops: &[DesignOp]) -> Vec<DesignOp> {
    let placed = |op: &DesignOp| match op {
        DesignOp::Board { unit, op: BoardOp::Add(Item::Placed(placed)) } => Some((*unit, placed.id)),
        _ => None,
    };
    let mut units: Vec<BoardId> = ops.iter().filter_map(placed).map(|(unit, _)| unit).collect();
    units.sort_by_key(|unit| unit.0);
    units.dedup();
    units.into_iter().flat_map(|unit| {
        let Some(board) = before.unit(unit).and_then(|of| of.board.as_ref()) else { return Vec::new() };
        let parts: Vec<ItemId> = ops.iter().filter_map(placed).filter(|(of, _)| *of == unit).map(|(_, id)| id).collect();
        label_ops(unit, board, ops, &parts)
    }).collect()
}

/// `arrange_references` as design ops, logging each label that could not take its row's side.
fn arranged_ops(unit: BoardId, board: &Board, parts: &[ItemId], riding: &[ItemId]) -> Vec<DesignOp> {
    let arranged = arrange_references(board, parts, riding);
    let name = |id: ItemId| board.placed_item(id).map_or_else(|| format!("{id:?}"), |p| p.refdes.clone());
    for off in &arranged.off_row {
        let by = match off.blocked_by {
            LabelBlocker::Courtyard(id) => format!("{}'s courtyard", name(id)),
            LabelBlocker::Pad(id) => format!("a pad of {}", name(id)),
            LabelBlocker::Label(id) => format!("{}'s label", name(id)),
            LabelBlocker::Via(_) => "a via".to_owned(),
            LabelBlocker::Hole(_) => "a hole".to_owned(),
            LabelBlocker::Text(_) => "board text".to_owned(),
            LabelBlocker::Outline => "the board edge".to_owned(),
        };
        makepad_widgets_core::log!("fab.board: {}'s label leaves the row of {}: its spot there is held by {by}", name(off.item), name(off.row));
    }
    arranged.ops.into_iter().map(|op| DesignOp::Board { unit, op }).collect()
}

/// Re-place every reference label on board `unit` beside its part (key A), as one undo step. A label
/// that is clear stays; pinned ones never move. `Err` is the chip text.
pub fn arrange_labels(design: &Design, unit: BoardId) -> Result<Plan, String> {
    let board = design.unit(unit).and_then(|u| u.board.as_ref()).ok_or_else(|| "no board here".to_owned())?;
    let parts: Vec<ItemId> = board.placed.iter().map(|p| p.id).collect();
    let ops = arranged_ops(unit, board, &parts, &[]);
    if ops.is_empty() {
        return Err("every label is already clear of pads, parts and other labels".to_owned());
    }
    let label = match ops.len() {
        1 => "Arrange 1 label".to_owned(),
        n => format!("Arrange {n} labels"),
    };
    Ok(Plan { ops, label })
}

/// The circuit's ground net: the `NetKind::Ground` net named "GND" when there is one, else the first
/// `NetKind::Ground` net in order. Board net ids are circuit net ids (`Board::nets[i]` is `circuit.nets[i]`).
pub fn ground_net(design: &Design, unit: BoardId) -> Option<NetId> {
    let nets = &design.unit(unit)?.circuit.nets;
    nets.iter().find(|net| net.kind == NetKind::Ground && net.name == "GND")
        .or_else(|| nets.iter().find(|net| net.kind == NetKind::Ground))
        .map(|net| net.id)
}

fn net_name(board: &Board, net: NetId) -> &str {
    board.nets.get(net.0 as usize).map(|net| net.name.as_str()).unwrap_or("unnamed")
}

fn item_name(board: &Board, item: &Item) -> String {
    match item {
        Item::Placed(x) => x.refdes.clone(),
        Item::Track(x) => format!("the {} track", net_name(board, x.net)),
        Item::Via(x) => format!("the {} via", net_name(board, x.net)),
        Item::Zone(x) => match x.net {
            Some(net) => format!("the {} pour", net_name(board, net)),
            None => "the zone".to_owned(),
        },
        Item::Text(_) => "the text".to_owned(),
        Item::Graphic(_) => "the drawing".to_owned(),
        Item::Keepout(_) => "the keepout".to_owned(),
        Item::Hole(_) => "the hole".to_owned(),
        Item::Cutout(_) => "the cutout".to_owned(),
        Item::Tuning(_) => "the tuning".to_owned(),
    }
}

fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::{BBox, BlockId, FeatureRef, PolySet, SolverId, P};
    use std::sync::Arc;

    /// What an edit moves disturbs the labels round where it lands, as the items it names do (E6
    /// S7): a via moved onto another part's reference label, as a drag's answer pushes or carries
    /// one, moves that label, though the edit names only a part far from it.
    #[test]
    fn a_via_an_edit_moves_onto_a_label_moves_that_label() {
        use fab_elec_board::text::text_pose;
        use fab_elec_parts::FpTextKind;
        let design = fab_elec_fixtures::ledctl();
        let unit = design.units.iter().find(|unit| unit.board.is_some()).expect("ledctl has a board");
        let board = unit.board.as_deref().expect("ledctl has a board");
        let via = board.vias.first().expect("fixture setup: ledctl has a via");
        let label_at = |placed: &fab_elec_board::Placed| placed.footprint.texts.iter()
            .find(|fp| fp.visible && fp.kind == FpTextKind::Reference).map(|fp| placed.to_board(text_pose(placed, fp).0));
        let settled = |placed: &fab_elec_board::Placed| label_ops(unit.id, board, &[], &[placed.id]).is_empty();
        let (part, at) = board.placed.iter().filter(|p| !p.locked && settled(p)).find_map(|p| Some((p, label_at(p)?)))
            .expect("fixture setup: a part with a reference label, its labels round it settled");
        let mut by_distance: Vec<_> = board.placed.iter().collect();
        by_distance.sort_by_key(|p| std::cmp::Reverse(p.at.dist2(at)));
        let far = by_distance.into_iter().find(|p| settled(p)).expect("fixture setup: a part far off, its labels settled");
        assert!(!far.bbox().inflate(LABEL_NEAR).intersects(&part.bbox()), "fixture setup: {} is far from {}", far.refdes, part.refdes);
        let ops = [DesignOp::Board { unit: unit.id, op: BoardOp::Move { item: via.id, by: at.sub(via.at) } }];
        let labels = label_ops(unit.id, board, &ops, &[far.id]);
        assert!(labels.iter().any(|op| matches!(op,
            DesignOp::Board { op: BoardOp::SetField { item, field: Field::Label(FpTextKind::Reference, Some(_)) }, .. } if *item == part.id)),
            "the via moved onto {}'s label moves the label: {labels:?}", part.refdes);
    }

    #[test]
    fn clear_routes_takes_the_boards_own_unlocked_copper_in_one_step() {
        let mut design = fab_elec_fixtures::ledctl();
        let unit_index = design.units.iter().position(|unit| unit.board.is_some()).unwrap();
        let unit = design.units[unit_index].id;
        let (locked, block, kept_net) = {
            let board = Arc::make_mut(design.units[unit_index].board.as_mut().unwrap());
            assert!(board.tracks.len() >= 3, "fixture setup: ledctl has routed copper");
            board.tracks[0].locked = true;
            board.tracks[1].origin = Origin::Block { block: BlockId(1), detached: false };
            board.tracks[2].origin = Origin::Solved { by: SolverId(0x0101) };
            (board.tracks[0].id, board.tracks[1].id, board.tracks[2].net)
        };
        let board = design.units[unit_index].board.clone().unwrap();
        assert!(has_clearable_routes(&board), "authored and solved copper is there to clear");

        let plan = clear_routes(&design, unit, None).expect("a routed board has routes to clear");
        assert_eq!(plan.label, "Clear routes", "the whole board's clear says so");
        let removed: Vec<ItemId> = plan.ops.iter().map(|op| match op {
            DesignOp::Board { unit: target, op: BoardOp::Remove(id) } if *target == unit => *id,
            other => panic!("clearing only removes items of its board: {other:?}"),
        }).collect();
        let tracks = board.tracks.len() - 2;
        assert_eq!(removed.len(), tracks + board.vias.len(), "every track but the locked and the block's, and every via, once");
        assert!(!removed.contains(&locked) && !removed.contains(&block), "locked copper and a block's copper stay");
        let first_via = removed.iter().position(|id| board.vias.iter().any(|via| via.id == *id)).unwrap_or(removed.len());
        assert!(removed[..first_via].iter().all(|id| board.tracks.iter().any(|t| t.id == *id)), "tracks go before the vias they end on");

        let mut cleared = design.clone();
        cleared.apply_all(&plan.ops, false).expect("clearing applies through the non-host path");
        let after = cleared.unit(unit).unwrap().board.as_ref().unwrap();
        assert_eq!(after.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), vec![locked, block], "only the kept copper is left");
        assert!(after.vias.is_empty() && after.placed.len() == board.placed.len() && after.zones.len() == board.zones.len(),
            "vias go; parts and pours stay");
        assert!(!has_clearable_routes(after), "nothing is left to clear");
        assert_eq!(clear_routes(&cleared, unit, None).unwrap_err(), "no routes to clear", "a cleared board says there is nothing to clear");

        let one = clear_routes(&design, unit, Some(&[kept_net])).expect("the net has copper of its own");
        assert_eq!(one.label, format!("Clear the {} routes", net_name(&board, kept_net)), "a one-net clear names its net");
        assert!(one.ops.iter().all(|op| match op {
            DesignOp::Board { op: BoardOp::Remove(id), .. } => board.tracks.iter().find(|t| t.id == *id).map(|t| t.net)
                .or_else(|| board.vias.iter().find(|v| v.id == *id).map(|v| v.net)) == Some(kept_net),
            _ => false,
        }), "a clear of named nets touches only their copper");
    }

    #[test]
    fn delete_and_pour_plans() {
        let design = fab_elec_fixtures::ledctl();
        let unit_index = design.units.iter().position(|unit| unit.board.is_some()).unwrap();
        let unit = design.units[unit_index].id;
        let board = design.units[unit_index].board.as_ref().unwrap();
        let track = &board.tracks[0];
        let placed = &board.placed[0];

        let plan = delete(&design, unit, track.id).expect("an authored track has a delete plan");
        assert_eq!(plan.ops, vec![DesignOp::Board { unit, op: BoardOp::Remove(track.id) }], "track deletion is exactly one remove of that item");
        assert_eq!(plan.label, format!("Delete the {} track", board.nets[track.net.0 as usize].name), "track deletion names its net");
        let mut edited = design.clone();
        edited.apply_all(&plan.ops, false).expect("track deletion applies through the non-host path");
        assert!(edited.unit(unit).unwrap().board.as_ref().unwrap().item(track.id).is_none(), "track deletion removes the item");

        let plan = delete(&design, unit, placed.id).expect("a placed part has an unplace plan");
        assert_eq!(plan.label, format!("Unplace {}", placed.refdes), "unplacing names the part's refdes");
        edited.apply_all(&plan.ops, false).expect("unplacing applies through the non-host path");
        assert!(crate::board::tray::unplaced(&edited, unit).iter().any(|part| part.comp == placed.comp), "unplacing returns the component to the tray");

        let mut guarded = design.clone();
        {
            let board = Arc::make_mut(guarded.units[unit_index].board.as_mut().unwrap());
            board.tracks[0].locked = true;
            board.tracks[0].origin = Origin::Block { block: BlockId(1), detached: false };
        }
        assert!(delete(&guarded, unit, track.id).unwrap_err().contains("is locked: unlock it"), "a locked track requires unlocking before origin checks");
        for (origin, why) in [
            (Origin::Block { block: BlockId(1), detached: false }, "is part of block"),
            (Origin::Derived { from: FeatureRef::Outline(unit) }, "at its source"),
        ] {
            let board = Arc::make_mut(guarded.units[unit_index].board.as_mut().unwrap());
            board.tracks[0].locked = false;
            board.tracks[0].origin = origin;
            assert!(delete(&guarded, unit, track.id).unwrap_err().contains(why), "the track's origin says '{why}'");
        }
        // Nothing reaches a block item's Detach yet, so its refusal names no action.
        Arc::make_mut(guarded.units[unit_index].board.as_mut().unwrap()).tracks[0].origin = Origin::Block { block: BlockId(1), detached: false };
        assert!(!delete(&guarded, unit, track.id).unwrap_err().contains("detach"), "a block item's refusal points at no unreachable Detach");
        // Copper the router laid is deleted directly, through the person's (non-host) path.
        Arc::make_mut(guarded.units[unit_index].board.as_mut().unwrap()).tracks[0].origin = Origin::Solved { by: SolverId(0x0101) };
        let plan = delete(&guarded, unit, track.id).expect("the router's copper has a delete plan");
        assert_eq!(plan.ops, vec![DesignOp::Board { unit, op: BoardOp::Remove(track.id) }], "the router's track is deleted by one remove");
        let mut edited = guarded.clone();
        edited.apply_all(&plan.ops, false).expect("deleting the router's copper applies through the non-host path");
        assert!(edited.unit(unit).unwrap().board.as_ref().unwrap().item(track.id).is_none(), "the router's track is gone");
        assert_eq!(delete(&design, unit, ItemId(board.next_item)).unwrap_err(), "nothing to delete here", "a missing item has nothing to delete");

        let gnd = design.units[unit_index].circuit.nets.iter().find(|net| net.name == "GND").unwrap().id;
        assert_eq!(ground_net(&design, unit), Some(gnd), "ledctl's ground net is GND");
        let mut poured = design.clone();
        {
            let board = Arc::make_mut(poured.units[unit_index].board.as_mut().unwrap());
            // The fixture already pours GND on both copper layers; make room for the new pour.
            board.zones.retain(|zone| !(zone.net == Some(gnd) && zone.area == Area::Board && zone.layers.has(LayerId::B_CU)));
            board.reindex();
        }
        let plan = pour(&poured, unit, None, LayerId::B_CU).expect("back copper accepts a new ground pour");
        let [DesignOp::Board { unit: target, op: BoardOp::Add(Item::Zone(zone)) }] = plan.ops.as_slice() else {
            panic!("pouring produces exactly one zone add");
        };
        assert_eq!(*target, unit, "the pour targets the requested board");
        assert_eq!(zone.id, ItemId(board.next_item), "the pour uses the next item id");
        assert_eq!(zone.area, Area::Board, "the pour covers the whole board");
        assert_eq!(zone.layers, LayerSet::one(LayerId::B_CU), "the pour uses only back copper");
        assert_eq!(zone.net, Some(gnd), "an implicit net pours GND");
        assert_eq!(zone.name, "GND", "the zone takes its net's name");
        assert_eq!(zone.priority, 0, "the board-wide pour has lowest priority");
        assert_eq!(zone.settings, ZoneSettings::default(), "the pour uses default settings");
        assert!(!zone.locked, "the new pour is unlocked");
        assert_eq!(zone.origin, Origin::Authored, "the new pour is authored");
        assert_eq!(plan.label, format!("Pour GND on {}", LayerId::B_CU.name()), "the pour label names its net and layer");
        poured.apply_all(&plan.ops, false).expect("pouring applies through the non-host path");
        assert!(pour(&poured, unit, None, LayerId::B_CU).unwrap_err().contains("is already poured on"), "a second whole-board pour on the same net and layer is rejected");
        assert_eq!(pour(&design, unit, None, LayerId::F_SILK).unwrap_err(), format!("{} is not a copper layer: pour on copper", LayerId::F_SILK.name()), "silk requires choosing a copper layer");
        let signal = board.tracks.iter().map(|t| t.net).find(|n| *n != gnd).expect("ledctl has a signal track");
        assert!(pour(&design, unit, Some(signal), LayerId::F_CU).unwrap_err().ends_with("is poured with GND: delete that pour first"), "a layer another net pours whole is refused, with no free layer");
        // `poured` now pours GND over the whole board on B.Cu alone (the pour applied above).
        assert!(pour(&poured, unit, Some(signal), LayerId::B_CU).unwrap_err().contains(&format!("make {} active", LayerId::F_CU.name())), "the refusal names the free copper layer");
        let plan = pour(&poured, unit, Some(signal), LayerId::F_CU).expect("a free layer takes a signal pour");
        assert_eq!(plan.ops.len(), 1, "with no other zone on the layer, the pour is the only op");
        {
            // GND over a patch of B.Cu instead of the whole board, still at priority 0.
            let board = Arc::make_mut(poured.units[unit_index].board.as_mut().unwrap());
            let zone = board.zones.iter_mut().find(|zone| zone.net == Some(gnd)).unwrap();
            zone.area = Area::Poly(PolySet::rect(BBox::new(P::mm(1.0, 1.0), P::mm(5.0, 5.0))));
            board.reindex();
        }
        let plan = pour(&poured, unit, Some(signal), LayerId::B_CU).expect("a patch of another net leaves the layer free to pour");
        let zones = poured.unit(unit).unwrap().board.as_ref().unwrap().zones.len();
        let raised: Vec<u16> = plan.ops.iter().filter_map(|op| match op {
            DesignOp::Board { op: BoardOp::SetField { field: Field::Priority(p), .. }, .. } => Some(*p),
            _ => None,
        }).collect();
        assert_eq!(raised, vec![1; zones], "a zone of another net at priority 0 on the layer raises every zone from 0 to 1, their order kept");
        assert!(matches!(plan.ops.last(), Some(DesignOp::Board { op: BoardOp::Add(Item::Zone(zone)), .. }) if zone.priority == 0), "the new pour comes last, at priority 0, below the patch");

        let circuit = Arc::make_mut(&mut guarded.units[unit_index].circuit);
        for net in &mut circuit.nets {
            net.kind = NetKind::Signal;
        }
        assert_eq!(pour(&guarded, unit, None, LayerId::B_CU).unwrap_err(), "no net to pour: point at copper, or give the circuit a ground net", "a circuit without ground needs an explicit pour net");
    }
}
