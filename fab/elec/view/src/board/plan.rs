//! Pure key-action plans over a committed design.

use fab_elec_board::text::{arrange_references, LabelBlocker, LABEL_NEAR};
use fab_elec_board::{Anchor, Area, Board, BoardOp, Field, Flash, Item, Origin, Placed, Tent, Via, ViaKind, Zone, ZoneSettings};
use fab_elec_circuit::NetKind;
use fab_elec_board::placement::PlacementIndex;
use fab_elec_core::{Angle, BBox, BoardId, CompId, ItemId, LayerId, LayerSet, NetId, Nm, PolySet, Side, P};
use fab_elec_design::{Design, DesignOp, PartRef, Place, Resolved, Unit};
use std::collections::{HashMap, HashSet};

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

/// Stitch the kept copper on two or more layers. A selected zone supplies one side of the
/// overlap. Fills, the spatial index and the effective rules all come from one committed result.
pub fn stitch(
    design: &Design, unit: BoardId, fills: &fab_elec_design::engines::BoardResults,
    net: NetId, zone: Option<ItemId>, pitch: Nm, pad: Option<Nm>, drill: Option<Nm>,
) -> Result<Plan, String> {
    use fab_elec_core::PQuad;
    use std::collections::{BTreeMap, BTreeSet};
    use std::ops::ControlFlow;

    let board = design.unit(unit).and_then(|u| u.board.as_deref()).ok_or("no board here")?;
    let network = board.nets.get(net.ix()).ok_or("choose a net on this board")?;
    let name = &network.name;
    if fills.unit != unit || fills.generation != board.generation || !fills.pending.zones.is_empty()
        || fills.index.0.generation() != board.generation {
        return Err(format!("the {name} pour is refilling: stitch again in a moment"));
    }
    if pitch <= 0 { return Err("stitch pitch must be greater than zero".into()); }
    if let Some(id) = zone {
        if !board.zones.iter().any(|z| z.id == id && z.net == Some(net)) {
            return Err(format!("zone {} is not a {name} pour", id.0));
        }
    }
    let rules = &fills.rules;
    let size = rules.via(network.class);
    let (pad, drill) = (pad.unwrap_or(size.pad), drill.unwrap_or(size.drill));
    if drill <= 0 || drill < rules.drill_min() {
        return Err(format!("choose a positive via drill of at least {}", mm(rules.drill_min())));
    }
    if pad <= drill || (pad - drill) / 2 < rules.annular_min() {
        return Err(format!("choose a via pad with an annular ring of at least {}", mm(rules.annular_min())));
    }

    let copper = board.stackup.copper();
    let mut by_layer = BTreeMap::<LayerId, Vec<&PolySet>>::new();
    let mut scoped = BTreeMap::new();
    for z in board.zones.iter().filter(|z| z.net == Some(net)) {
        let i = fills.fills.binary_search_by_key(&z.id, |f| f.zone)
            .map_err(|_| format!("the {name} pour is refilling: stitch again in a moment"))?;
        for layer in &fills.fills[i].layers {
            by_layer.entry(layer.layer).or_default().extend(layer.kept().map(|(_, p)| p));
            if zone == Some(z.id) { scoped.insert(layer.layer, layer.set()); }
        }
    }
    let layers: BTreeMap<_, _> = by_layer.into_iter()
        .map(|(id, pieces)| (id, PolySet::union_all(pieces))).collect();
    let mut sites = BTreeSet::new();
    let mut overlap = false;
    for (a, first) in if zone.is_some() { &scoped } else { &layers } {
        for (b, second) in &layers {
            if a == b || (zone.is_none() && a > b) { continue; }
            let common = first.intersection(second);
            if common.is_empty() { continue; }
            overlap = true;
            stitch_rows(&common, pitch, (pad / 2).max(1), &mut sites);
        }
    }
    if !overlap { return Err(format!("no overlap of {name} pour on two copper layers")); }
    let (Some(&top), Some(&bottom)) = (copper.first(), copper.last()) else { return Err("no copper layers".into()) };
    let reach = rules.max_clearance().max(rules.edge()).max(rules.hole_clearance())
        .max(rules.hole_to_hole()).max(rules.max_via_drill_to_pad(board)).max(1);
    let separation = i64::from(2 * (drill / 2)) + i64::from(fab_elec_board::drc::allowed_need(rules.hole_to_hole()));
    // The obstacle query only needs an unused owner id to compare to the indexed pieces.
    let via_at = |at, id| Via { id: ItemId(id), at, net, pad, drill, span: (top, bottom), kind: ViaKind::Through,
        tent: Tent::default(), backdrill: None, locked: false, origin: Origin::Authored, flash: Flash::All };
    sites.retain(|&at| stitch_clear(board, fills, &via_at(at, board.next_item), network.class, &copper, reach, pitch));
    let mut added = PQuad::<P>::new();
    let mut ops = Vec::new();
    for at in sites {
        // A fine pitch must also respect the drills already proposed by this same press.
        if i64::from(pitch) < separation {
            let mut blocked = false;
            added.query(BBox::point(at).inflate(separation.try_into().unwrap_or(Nm::MAX)), |_, other| {
                blocked = at.dist2(*other) < i128::from(separation).pow(2);
                if blocked { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
            });
            if blocked { continue; }
            added.insert(BBox::point(at), at);
        }
        let id = board.next_item.checked_add(u32::try_from(ops.len()).map_err(|_| "no board item ids left")?)
            .filter(|id| *id < u32::MAX).ok_or("no board item ids left")?;
        let via = via_at(at, id);
        ops.push(DesignOp::Board { unit, op: BoardOp::Add(Item::Via(via)) });
    }
    if ops.is_empty() { return Err(format!("every {name} stitch site is blocked or already stitched")); }
    let label = format!("Stitch {name}: {} {}", ops.len(), if ops.len() == 1 { "via" } else { "vias" });
    Ok(Plan { label, ops })
}

/// Scan the overlap after the fill tiles have been unioned on each layer. Crossings toggle the
/// polygon interior; each edge also excludes one interval of pad centres on the row.
fn stitch_rows(common: &PolySet, pitch: Nm, radius: Nm, sites: &mut std::collections::BTreeSet<P>) {
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum Event { Crossing, BlockStart, BlockEnd }

    let pitch = i64::from(pitch);
    let radius64 = i64::from(radius);
    let ceil_col = |x: i64| -(-x).div_euclid(pitch);
    for polygon in common.polygons() {
        let bounds = BBox::of_points(polygon.outer.iter().copied()).inflate(-radius);
        if bounds.is_empty() { continue; }
        let left = ceil_col(i64::from(bounds.min.x));
        let right = i64::from(bounds.max.x).div_euclid(pitch);
        if left > right { continue; }
        let mut edges = Vec::new();
        for ring in std::iter::once(&polygon.outer).chain(&polygon.holes) {
            for (&a, &b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
                edges.push(if a.y <= b.y { (a, b) } else { (b, a) });
            }
        }
        edges.sort_unstable_by_key(|(a, _)| a.y);
        let mut next = edges.as_slice();
        let mut active = Vec::<(P, P)>::new();
        let mut events = Vec::new();
        for row in ceil_col(i64::from(bounds.min.y))..=i64::from(bounds.max.y).div_euclid(pitch) {
            let y = row * pitch;
            let count = next.partition_point(|(a, _)| i64::from(a.y) <= y + radius64);
            active.extend_from_slice(&next[..count]);
            next = &next[count..];
            active.retain(|(_, b)| i64::from(b.y) >= y - radius64);
            events.clear();
            for &(a, b) in &active {
                // The column closest to this segment lies at its crossing with this row, or
                // at the nearer endpoint if the row misses it. Keep the crossing rational.
                let dy = i128::from(b.y) - i128::from(a.y);
                let rise = i128::from(y) - i128::from(a.y);
                let denominator = dy.max(1) * i128::from(pitch);
                let numerator = i128::from(a.x) * dy.max(1)
                    + (i128::from(b.x) - i128::from(a.x)) * rise.clamp(0, dy);
                if 0 <= rise && rise < dy {
                    let crossing = -(-numerator).div_euclid(denominator);
                    events.push(((crossing as i64).clamp(left, right + 1), Event::Crossing));
                }
                let lo = left.max(ceil_col(i64::from(a.x.min(b.x)) - radius64));
                let hi = right.min((i64::from(a.x.max(b.x)) + radius64).div_euclid(pitch));
                if lo > hi { continue; }
                let closest = numerator.div_euclid(denominator) as i64;
                let edge = fab_elec_core::Shape::Segment { a, b, w: 0 };
                let hits = |col: i64| {
                    let p = P::new((col * pitch) as Nm, y as Nm);
                    fab_elec_core::point_clearance(p, &edge) < radius
                };
                // A capsule cuts a row in one interval. Either neighbour of the closest
                // column can witness it. Find its ends with the same rounded gap as collides.
                let Some(hit) = [closest, closest + 1].into_iter()
                    .map(|col| col.clamp(lo, hi)).find(|&col| hits(col)) else { continue };
                let start = first_column(lo, hit, hits);
                let end = first_column(hit + 1, hi + 1, |col| !hits(col));
                events.push((start, Event::BlockStart));
                events.push((end, Event::BlockEnd));
            }
            events.sort_unstable();
            let mut inside = false;
            let mut blocked = 0isize;
            let mut from = left;
            for &(col, event) in &events {
                if inside && blocked == 0 {
                    sites.extend((from..col).map(|x| P::new((x * pitch) as Nm, y as Nm)));
                }
                match event {
                    Event::Crossing => inside = !inside,
                    Event::BlockStart => blocked += 1,
                    Event::BlockEnd => blocked -= 1,
                }
                from = col;
            }
        }
    }
}

/// First column in [lo, hi) that passes a monotone test, or hi if none does.
fn first_column(mut lo: i64, mut hi: i64, test: impl Fn(i64) -> bool) -> i64 {
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if test(mid) { hi = mid; } else { lo = mid + 1; }
    }
    lo
}

fn stitch_clear(board: &Board, fills: &fab_elec_design::engines::BoardResults, via: &Via,
    class: fab_elec_core::ClassId, layers: &[LayerId], reach: Nm, pitch: Nm) -> bool {
    use fab_elec_board::{drc::{check_drills, check_to, check_via_in_pad}, ConnItem, Copper, Obstacle, PieceKind};
    use fab_elec_core::Shape;
    use std::ops::ControlFlow;

    let rules = &fills.rules;
    let item = ConnItem::Via(via.id);
    let net = Some(via.net);
    let drill = Shape::Circle { c: via.at, r: via.drill / 2 };
    let hole = Obstacle::Drill { item, net, shape: &drill };
    let mut ring = Copper { item, net, class, layer: via.span.0, shape: via.shape() };
    let query = ring.shape.bbox().inflate(reach).union(BBox::point(via.at).inflate(pitch / 2));
    for &layer in layers {
        ring.layer = layer;
        let mut blocked = false;
        fills.index.0.query(layer, query, |piece| {
            blocked = check_to(rules, &ring, &piece.obstacle(), layer).is_some() || match &piece.kind {
                PieceKind::Copper(c) | PieceKind::Fill(c) | PieceKind::Offer(c) =>
                    check_to(rules, c, &hole, layer).is_some() || check_via_in_pad(board, rules, net, &drill, c).is_some()
                    || (c.net != net && fab_elec_core::collides(&ring.shape, &c.shape, 1)),
                PieceKind::Drill { item: other, shape, net: other_net, .. } =>
                    check_drills(rules, (item, &drill), (*other, shape)).is_some()
                    || (matches!(other, ConnItem::Via(_)) && net == *other_net
                        && via.at.dist2(shape.bbox().center()) * 4 < i128::from(pitch).pow(2)),
                _ => false,
            };
            if blocked { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        });
        if blocked { return false; }
    }
    true
}

/// Corners of a drag rectangle, in contour order. Both drag directions have the same result.
pub fn rect_points(bounds: BBox) -> [P; 4] {
    [bounds.min, P::new(bounds.max.x, bounds.min.y), bounds.max, P::new(bounds.min.x, bounds.max.y)]
}

/// Validate the drawn ring before turning it into a PolySet, whose normalisation would
/// otherwise remove crossings. An explicit closing point and stationary clicks are harmless.
fn drawn_ring(points: &[P]) -> Result<Vec<P>, String> {
    use fab_elec_core::geom::orient;
    let mut ring = points.to_vec();
    ring.dedup();
    if ring.first() == ring.last() { ring.pop(); }
    if ring.iter().collect::<HashSet<_>>().len() < 3 {
        return Err("a contour needs at least three distinct points".into());
    }
    if ring.iter().all(|&p| orient(ring[0], ring[1], p) == 0) {
        return Err("the contour has zero area".into());
    }
    let edges = || ring.iter().copied().zip(ring.iter().copied().cycle().skip(1));
    for (i, (a, b)) in edges().enumerate() {
        let next = ring[(i + 2) % ring.len()];
        if orient(a, b, next) == 0 && !BBox::new(a, next).contains(b) {
            return Err("the contour crosses itself".into());
        }
        for (j, (c, d)) in edges().enumerate().skip(i + 2) {
            if i == 0 && j + 1 == ring.len() { continue; }
            if BBox::new(a, b).intersects(&BBox::new(c, d))
                && orient(a, b, c).signum() * orient(a, b, d).signum() <= 0
                && orient(c, d, a).signum() * orient(c, d, b).signum() <= 0 {
                return Err("the contour crosses itself".into());
            }
        }
    }
    Ok(ring)
}

fn drawn_contour(points: &[P]) -> Result<fab_elec_core::Contour, String> {
    let ring = drawn_ring(points)?;
    let path = ring[1..].iter().copied().fold(fab_elec_core::Path::new(ring[0]), fab_elec_core::Path::line);
    Ok(fab_elec_core::Contour::new(path))
}

/// Replace the outer (first) contour. Every subsequent contour keeps its path and position.
pub fn outline(design: &Design, unit: BoardId, contour: &[P]) -> Result<Plan, String> {
    let board = design.unit(unit).and_then(|u| u.board.as_deref()).ok_or("no board here")?;
    let mut contours = vec![drawn_contour(contour)?];
    contours.extend(board.outline.iter().skip(1).cloned());
    Ok(Plan { ops: vec![DesignOp::Board { unit, op: BoardOp::SetOutline(contours) }], label: "Set board outline".into() })
}

/// Add one authored keepout. Copper and parts already in the area are checked by DRC.
pub fn keepout(design: &Design, unit: BoardId, area: &[P], layers: LayerSet, forbid: fab_elec_parts::Forbid,
    reason: &str) -> Result<Plan, String> {
    let board = design.unit(unit).and_then(|u| u.board.as_deref()).ok_or("no board here")?;
    let item = fab_elec_board::BoardKeepout { id: ItemId(board.next_item), area: PolySet::from_ring_nonzero(drawn_ring(area)?),
        layers, forbid, reason: reason.to_owned(), origin: Origin::Authored, z: None };
    Ok(Plan { ops: vec![DesignOp::Board { unit, op: BoardOp::Add(Item::Keepout(item)) }], label: "Add keepout".into() })
}

/// Add one named, authored board cutout.
pub fn cutout(design: &Design, unit: BoardId, contour: &[P], name: &str) -> Result<Plan, String> {
    let board = design.unit(unit).and_then(|u| u.board.as_deref()).ok_or("no board here")?;
    let item = fab_elec_board::Cutout { id: ItemId(board.next_item), contour: drawn_contour(contour)?,
        name: name.to_owned(), origin: Origin::Authored };
    let label = if name.is_empty() || name == "Cutout" { "Add cutout".to_owned() } else { format!("Add cutout {name}") };
    Ok(Plan { ops: vec![DesignOp::Board { unit, op: BoardOp::Add(Item::Cutout(item)) }], label })
}

/// The Zone tool's rectangle or polygon on `layer`, of `net` (the copper at its first press)
/// or else the circuit's ground. One `BoardOp::Add`, below every zone it
/// overlaps (priority 0). `Err` is the chip text.
pub fn zone(design: &Design, unit: BoardId, points: &[P], net: Option<NetId>, layer: LayerId) -> Result<Plan, String> {
    let board = design.unit(unit).and_then(|unit| unit.board.as_ref()).ok_or_else(|| "no board here".to_owned())?;
    let layer_name = layer.name();
    if !board.copper_set().has(layer) {
        return Err(format!("{layer_name} is not a copper layer: draw zones on copper"));
    }
    let shape = PolySet::from_ring_nonzero(drawn_ring(points)?);
    let net = net.or_else(|| ground_net(design, unit)).ok_or_else(|| "no net for the zone: start on copper, or give the circuit a ground net".to_owned())?;
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

/// The courtyard silhouette in board coordinates, including bottom-side mirroring. Imported
/// footprints can lack a courtyard; use their placement extent in that case.
pub(super) fn courtyard_box(part: &Placed) -> BBox {
    let bounds = BBox::of_points(part.footprint.courtyard.rings().flatten().map(|&p| part.to_board(p)));
    if bounds.is_empty() { part.bbox() } else { bounds }
}

pub(super) fn part_refusal(part: &Placed) -> Option<String> {
    if part.locked { return Some(format!("{} is locked", part.refdes)); }
    match part.origin {
        Origin::Block { detached: false, .. } => Some(format!("{} belongs to a block", part.refdes)),
        Origin::Derived { .. } => Some(format!("{} follows another board", part.refdes)),
        _ => None,
    }
}

/// The unique parts in selection order. Resolve the whole selection before planning any move.
struct Parts<'a> {
    board: &'a Board,
    selected: Vec<&'a Placed>,
}

impl<'a> Parts<'a> {
    fn new(design: &'a Design, unit: BoardId, ids: &[CompId]) -> Result<Self, String> {
        let unit = design.unit(unit).ok_or("no board here")?;
        let board = unit.board.as_deref().ok_or("no board here")?;
        let placed: HashMap<_, _> = board.placed.iter().map(|part| (part.comp, part)).collect();
        let mut seen = HashSet::new();
        let mut selected = Vec::new();
        for id in ids.iter().filter(|id| seen.insert(**id)) {
            let part = placed.get(id).copied().ok_or_else(|| {
                let name = unit.circuit.comps.iter().find(|c| c.id == *id).map_or("part", |c| c.refdes.as_str());
                format!("{name} is not placed")
            })?;
            if let Some(why) = part_refusal(part) { return Err(why); }
            selected.push(part);
        }
        Ok(Self { board, selected })
    }

    // Twice the pivot coordinates retain half-nanometre centres until the final grid rounding.
    fn pivot(&self) -> Result<(i64, i64), String> {
        match self.selected.as_slice() {
            [] => Err("select a placed part".into()),
            [part] => Ok((2 * i64::from(part.at.x), 2 * i64::from(part.at.y))),
            parts => {
                let b = parts.iter().fold(BBox::EMPTY, |b, p| b.union(courtyard_box(p)));
                Ok((i64::from(b.min.x) + i64::from(b.max.x), i64::from(b.min.y) + i64::from(b.max.y)))
            }
        }
    }

    fn named(&self, verb: &str) -> String {
        let names: Vec<_> = self.selected.iter().take(2).map(|p| p.refdes.as_str()).collect();
        let mut label = format!("{verb} {}", names.join(", "));
        if self.selected.len() > 2 {
            label.push_str(&format!(" (+{})", self.selected.len() - 2));
        }
        label
    }

    fn finish(&self, changes: PartChanges, label: String) -> Result<Resolved, String> {
        if changes.ops.is_empty() {
            return Err("nothing would change".into());
        }
        let unit = self.board.unit;
        let mut ops: Vec<_> = changes.ops.into_iter().map(|op| DesignOp::Board { unit, op }).collect();
        // The batch snaps retained ends to their pads. Only a change of side can take a
        // pad off the track's layer; through-hole pads can keep their anchor across a flip.
        if !changes.flipped.is_empty() {
            let (copper, layers) = (self.board.copper_set(), self.board.copper_count());
            for track in &self.board.tracks {
                let mut ends = track.ends;
                for end in &mut ends {
                    let Anchor::Pad { placed, pad } = *end else { continue };
                    if changes.flipped.get(&placed).is_some_and(|p| !p.pad_layers(pad, copper, layers).has(track.layer)) {
                        *end = Anchor::Free;
                    }
                }
                if ends != track.ends {
                    ops.push(DesignOp::Board { unit, op: BoardOp::SetField { item: track.id, field: Field::Ends(ends) } });
                }
            }
        }
        let ids: Vec<_> = self.selected.iter().map(|p| p.id).collect();
        ops.extend(label_ops(unit, self.board, &ops, &ids));
        Ok(Resolved { ops, label, place: vec![] })
    }
}

#[derive(Default)]
struct PartChanges {
    ops: Vec<BoardOp>,
    flipped: HashMap<ItemId, Placed>,
}

impl PartChanges {
    fn pose(&mut self, part: &Placed, dx: i64, dy: i64, rot: Angle, side: Side) -> Result<(), String> {
        if (dx, dy, rot, side) == (0, 0, part.rot, part.side) { return Ok(()); }
        let nm = |v| Nm::try_from(v).map_err(|_| format!("{} would move outside the board coordinate range", part.refdes));
        let at = P::new(nm(i64::from(part.at.x) + dx)?, nm(i64::from(part.at.y) + dy)?);
        let by = P::new(nm(dx)?, nm(dy)?);
        if by != P::ZERO {
            self.ops.push(BoardOp::Move { item: part.id, by });
        }
        if rot != part.rot {
            self.ops.push(BoardOp::SetField { item: part.id, field: Field::Rot(rot) });
        }
        if side != part.side {
            self.ops.push(BoardOp::SetField { item: part.id, field: Field::Side(side) });
            self.flipped.insert(part.id, Placed { at, rot, side, ..part.clone() });
        }
        Ok(())
    }
}

/// Rotate about the anchor of one part or the combined courtyard centre of several.
pub fn rotate(design: &Design, unit: BoardId, ids: &[CompId], by: Angle) -> Result<Resolved, String> {
    let parts = Parts::new(design, unit, ids)?;
    let (cx, cy) = parts.pivot()?;
    let (sin, cos) = by.sin_cos();
    let mut changes = PartChanges::default();
    if by != Angle::ZERO {
        for part in &parts.selected {
            let x = f64::from(part.at.x) - cx as f64 / 2.0;
            let y = f64::from(part.at.y) - cy as f64 / 2.0;
            let dx = ((cos - 1.0) * x - sin * y).round() as i64;
            let dy = (sin * x + (cos - 1.0) * y).round() as i64;
            changes.pose(part, dx, dy, part.rot.add(by), part.side)?;
        }
    }
    parts.finish(changes, parts.named("Rotate"))
}

/// Reflect across the vertical pivot line. In to_board the local mirror precedes rotation:
/// reflecting the result swaps the side and negates that rotation (M R(a) = R(-a) M).
pub fn flip(design: &Design, unit: BoardId, ids: &[CompId]) -> Result<Resolved, String> {
    let parts = Parts::new(design, unit, ids)?;
    let (cx, _) = parts.pivot()?;
    let mut changes = PartChanges::default();
    for part in &parts.selected {
        changes.pose(part, cx - 2 * i64::from(part.at.x), 0, part.rot.neg(), part.side.flipped())?;
    }
    parts.finish(changes, parts.named("Flip"))
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Edge { Left, Right, Top, Bottom, CentreX, CentreY }

impl Edge {
    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "left", Self::Right => "right", Self::Top => "top", Self::Bottom => "bottom",
            Self::CentreX => "centre_x", Self::CentreY => "centre_y",
        }
    }

    fn coordinate(self, b: BBox) -> Nm {
        match self {
            Self::Left => b.min.x, Self::Right => b.max.x, Self::Top => b.max.y, Self::Bottom => b.min.y,
            Self::CentreX => b.center().x, Self::CentreY => b.center().y,
        }
    }
}

/// Bring each selected courtyard edge or centre line to the matching line of their union.
pub fn align(design: &Design, unit: BoardId, ids: &[CompId], edge: Edge) -> Result<Resolved, String> {
    let parts = Parts::new(design, unit, ids)?;
    let changes = align_changes(&parts.selected, edge)?;
    parts.finish(changes, format!("Align {} parts {}", parts.selected.len(), edge.name().replace('_', " ")))
}

fn align_changes(selected: &[&Placed], edge: Edge) -> Result<PartChanges, String> {
    let n = selected.len();
    if n < 2 { return Err("select at least two parts to align".into()); }
    let boxes: Vec<_> = selected.iter().map(|p| courtyard_box(p)).collect();
    let target = edge.coordinate(boxes.iter().copied().fold(BBox::EMPTY, BBox::union));
    let mut changes = PartChanges::default();
    for (part, b) in selected.iter().zip(boxes) {
        let delta = i64::from(target) - i64::from(edge.coordinate(b));
        let (dx, dy) = match edge {
            Edge::Left | Edge::Right | Edge::CentreX => (delta, 0),
            Edge::Top | Edge::Bottom | Edge::CentreY => (0, delta),
        };
        changes.pose(part, dx, dy, part.rot, part.side)?;
    }
    Ok(changes)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Axis { X, Y }

impl Axis {
    pub fn name(self) -> &'static str { match self { Self::X => "x", Self::Y => "y" } }
}

/// Keep the two outer parts (ordered by courtyard centre) and equalise the signed gaps.
/// Round cumulative gap distances so an indivisible nanometre never accumulates across a row.
pub fn distribute(design: &Design, unit: BoardId, ids: &[CompId], axis: Axis) -> Result<Resolved, String> {
    let parts = Parts::new(design, unit, ids)?;
    let changes = distribute_changes(&parts.selected, axis)?;
    parts.finish(changes, format!("Distribute {} parts along {}", parts.selected.len(), axis.name()))
}

fn distribute_changes(selected: &[&Placed], axis: Axis) -> Result<PartChanges, String> {
    let n = selected.len();
    if n < 3 { return Err("select at least three parts to distribute".into()); }
    let mut spans: Vec<_> = selected.iter().map(|&part| {
        let b = courtyard_box(part);
        let (lo, hi) = match axis { Axis::X => (b.min.x, b.max.x), Axis::Y => (b.min.y, b.max.y) };
        (part, i64::from(lo), i64::from(hi))
    }).collect();
    spans.sort_by_key(|(part, lo, hi)| (lo + hi, *lo, part.id));
    let inner = &spans[1..n - 1];
    let widths: i64 = inner.iter().map(|(_, lo, hi)| hi - lo).sum();
    let gaps = spans[n - 1].1 - spans[0].2 - widths;
    let mut cursor = spans[0].2;
    let mut changes = PartChanges::default();
    for (i, &(part, lo, hi)) in inner.iter().enumerate() {
        let offset = (gaps as f64 * (i + 1) as f64 / (n - 1) as f64).round() as i64;
        let delta = cursor + offset - lo;
        let (dx, dy) = match axis { Axis::X => (delta, 0), Axis::Y => (0, delta) };
        changes.pose(part, dx, dy, part.rot, part.side)?;
        cursor += hi - lo;
    }
    Ok(changes)
}

/// The strip tests movement only when it opens the flyout, without cloning the board or arranging labels.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Arrangement { Align(Edge), Distribute(Axis) }

impl Arrangement {
    pub(super) fn available(self, parts: &[&Placed]) -> bool {
        let changes = match self {
            Self::Align(edge) => align_changes(parts, edge),
            Self::Distribute(axis) => distribute_changes(parts, axis),
        };
        changes.is_ok_and(|c| !c.ops.is_empty())
    }
}

/// What the strip's Place asks the placer for (`ViewAction::Place`): the `selected` parts, placed
/// again; with none selected an empty list, the parts the board does not place yet and those
/// outside its outline (the job's own scope rule). `Err` is the chip text when nothing is selected
/// and every part stands on the board inside its outline.
/// Cost: O(parts), plus one placement index of the board when nothing is selected.
pub fn place_scope(design: &Design, unit: BoardId, selected: &[CompId]) -> Result<Vec<CompId>, String> {
    if !selected.is_empty() {
        return Ok(selected.to_vec());
    }
    let board = design.unit(unit).and_then(|u| u.board.as_deref()).ok_or_else(|| "no board here".to_owned())?;
    if !super::tray::unplaced(design, unit).is_empty() || !PlacementIndex::new(board, None).outside_outline().is_empty() {
        return Ok(Vec::new());
    }
    Err("every part is on the board: select parts to place them again".to_owned())
}

/// The parts among `comps` that `elec.place @place` holds unless told `panel: @free`: panel parts
/// (a body with a port: keys, pots, jacks, lights, plugs) standing on the board inside its
/// outline, since the case is built round them. Parts the board does not place yet and parts
/// outside the outline are never held. In `comps` order. Cost: O(comps), plus one placement
/// index of the board when one of them is a standing panel part.
pub fn held_panel(unit: &Unit, comps: &[CompId]) -> Vec<CompId> {
    let Some(board) = unit.board.as_deref() else { return Vec::new() };
    let panel: Vec<CompId> = comps.iter().copied().filter(|&comp| {
        board.placed_of(comp).is_some()
            && unit.circuit.comps.get(comp.ix()).and_then(|c| c.part.body.as_ref()).is_some_and(|body| !body.ports.is_empty())
    }).collect();
    if panel.is_empty() {
        return panel;
    }
    let outside: HashSet<CompId> = PlacementIndex::new(board, None).outside_outline().into_iter().map(|(placed, _)| placed.comp).collect();
    panel.into_iter().filter(|comp| !outside.contains(comp)).collect()
}

/// What the strip's Place does with a scope (`place_scope`): `send` the place job, holding the
/// panel parts as the job does, unless every part in it is held (the job would refuse); and
/// offer "Place them anyway" for the `held` ones, which places the whole scope with the panel
/// free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacePress {
    pub parts: Vec<CompId>,
    pub held: Vec<CompId>,
}

impl PlacePress {
    /// The strip's Place on board `unit` of `design` with `selected`. O(selected), plus one
    /// placement index as `place_scope` and `held_panel` need.
    pub fn of(design: &Design, unit: BoardId, selected: &[CompId]) -> Result<PlacePress, String> {
        let parts = place_scope(design, unit, selected)?;
        let held = design.unit(unit).map_or_else(Vec::new, |found| held_panel(found, &parts));
        Ok(PlacePress { parts, held })
    }

    /// Whether the job goes now: something in scope is not held (an empty scope is the parts
    /// off the board, never held).
    pub fn send(&self) -> bool {
        self.parts.is_empty() || self.held.len() < self.parts.len()
    }
}

/// The gap between parts placed side by side, and below the board.
const PLACE_GAP: Nm = 2_000_000;

/// Update from the circuit: every part of the circuit the board does not place yet
/// (`tray::unplaced`), placed in rows under the board's outline, left to right from its left edge,
/// a new row once a row reaches the outline's width (at least 40 mm); a part the circuit fixes to a
/// spot goes on its spot (`spots_of`). One undo step. `Err` is the chip text when every part is
/// on the board already.
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
    let spots = fab_elec_board::spots_of(&board_unit.circuit);
    for part in &parts {
        let Some(comp) = board_unit.circuit.comps.iter().find(|c| c.id == part.comp) else { continue };
        // A part fixed to a spot goes on it, turned as the spot says.
        if let Some(spot) = spots.iter().find(|s| s.comp == comp.id) {
            places.push(Place { unit, part: PartRef::Path(comp.path.clone()), at: spot.at, rot: spot.rot, side: None });
            continue;
        }
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

    #[test]
    fn drawn_shapes_commit_once_and_undo_with_inner_contours_intact() {
        use fab_elec_core::{Canon, Contour};
        use fab_elec_parts::Forbid;
        let mut design = fab_elec_fixtures::ledctl();
        let unit = design.units[0].id;
        let board = Arc::make_mut(design.units[0].board.as_mut().unwrap());
        let inner = Contour::circle(P::mm(4.0, 6.0), 500_000);
        board.outline.push(inner.clone());
        let before = board.content_hash();
        let p = [P::new(0, 0), P::new(0, 60), P::new(30, 30), P::new(60, 60), P::new(60, 0)];
        let contour = Contour::new(p[1..].iter().copied().fold(fab_elec_core::Path::new(p[0]), fab_elec_core::Path::line));
        let layers = LayerSet::one(LayerId::B_CU);
        let plans = [outline(&design, unit, &p).unwrap(), keepout(&design, unit, &p, layers, Forbid::VIAS, "fixture").unwrap(),
            cutout(&design, unit, &p, "slot").unwrap()];
        for plan in plans {
            let [DesignOp::Board { op, .. }] = plan.ops.as_slice() else { panic!("one board op for {}", plan.label) };
            match op {
                BoardOp::SetOutline(rings) => assert_eq!(rings, &[contour.clone(), inner.clone()]),
                BoardOp::Add(Item::Keepout(k)) => {
                    assert_eq!(k.area, PolySet::from_ring_nonzero(p.to_vec()));
                    assert_eq!((k.layers, k.forbid, k.reason.as_str(), &k.origin), (layers, Forbid::VIAS, "fixture", &Origin::Authored));
                }
                BoardOp::Add(Item::Cutout(c)) => assert_eq!((&c.contour, c.name.as_str(), &c.origin), (&contour, "slot", &Origin::Authored)),
                _ => panic!("unexpected op for {}", plan.label),
            }
            let steps = design.apply_all(&plan.ops, false).unwrap();
            let [step] = steps.as_slice() else { panic!("one undo step for {}", plan.label) };
            design.apply_all(&step.inverse(), true).unwrap();
            assert_eq!(design.units[0].board.as_ref().unwrap().content_hash(), before);
        }
    }

    #[test]
    fn drawn_shapes_validate_the_input_ring_before_normalisation() {
        let design = fab_elec_fixtures::ledctl();
        let unit = design.units[0].id;
        let p = P::new;
        for (points, why) in [
            (vec![p(1, 2), p(3, 4), p(1, 2)], "three distinct"),
            (vec![p(0, 0), p(2, 1), p(4, 2)], "zero area"),
            (vec![p(0, 0), p(5, 5), p(0, 4), p(4, 0)], "crosses itself"),
            (vec![p(0, 0), p(4, 0), p(2, 0), p(2, 4)], "crosses itself"),
        ] {
            for result in [outline(&design, unit, &points), cutout(&design, unit, &points, "slot"),
                keepout(&design, unit, &points, LayerSet::one(LayerId::F_CU), fab_elec_parts::Forbid::ALL, "")] {
                assert!(result.unwrap_err().contains(why));
            }
        }
        let closed = [p(0, 0), p(2, 0), p(2, 0), p(4, 0), p(4, 4), p(0, 0)];
        assert!(outline(&design, unit, &closed).is_ok(), "redundant vertices still describe a triangle");
    }

    #[test]
    fn stitch_rows_clear_holes_and_accept_tangent_disks_across_tile_seams() {
        // Two kept tile pieces meet at x = 0. Their notches join to make a rectangular hole.
        let rect = |x0, y0, x1, y1| PolySet::rect(BBox::new(P::new(x0, y0), P::new(x1, y1)));
        let hole = rect(-3, -2, 3, 2);
        let left = rect(-20, -15, 0, 15).difference(&hole);
        let right = rect(0, -15, 20, 15).difference(&hole);
        let mut sites = std::collections::BTreeSet::new();
        stitch_rows(&PolySet::union_all([&left, &right]), 1, 5, &mut sites);

        // The outer rectangle shrinks by the pad radius. At the hole, the exclusion has
        // round corners: a 3-4-5 tangent is legal, as is a disk centred across the tile seam.
        for x in -20_i32..=20 {
            for y in -15_i32..=15 {
                let dx = (x.abs() - 3).max(0);
                let dy = (y.abs() - 2).max(0);
                let fits = x.abs() <= 15 && y.abs() <= 10 && dx * dx + dy * dy >= 25;
                assert_eq!(sites.contains(&P::new(x, y)), fits, "disk centred at ({x}, {y})");
            }
        }
    }

    #[test]
    fn stitch_uses_kept_copper_and_current_rules_in_one_undo() {
        use std::collections::BTreeSet;
        use fab_elec_board::{BoardNet, Islands, Track};
        use fab_elec_core::{clearance, mm, Canon, ClassId, Contour, Path, Shape};
        use fab_elec_design::{engines, UnitKind};
        let unit = BoardId(0);
        let (ground, signal) = (NetId(0), NetId(1));
        let mut board = Board::new(unit, "stitch");
        board.nets = ["GND", "SIG"].map(|name| BoardNet { name: name.into(), class: ClassId(0) }).into();
        board.outline.push(Contour::rect(P::mm(-11.0, -11.0), P::mm(11.0, 11.0), 0));
        for layer in [LayerId::F_CU, LayerId::B_CU] {
            let id = board.alloc_id();
            board.zones.push(Zone { id, name: "ground".into(), net: Some(ground), layers: LayerSet::one(layer),
                area: Area::Board, priority: 0, settings: ZoneSettings { islands: Islands::Keep, ..Default::default() },
                locked: false, origin: Origin::Authored });
        }
        let segment = Shape::Segment { a: P::mm(-10.0, 0.0), b: P::mm(10.0, 0.0), w: mm(0.3) };
        let id = board.alloc_id();
        board.tracks.push(Track { id, net: signal, layer: LayerId::F_CU, w: mm(0.3), widths: None,
            path: Path::new(P::mm(-10.0, 0.0)).line(P::mm(10.0, 0.0)), ends: [Anchor::Free; 2],
            locked: false, origin: Origin::Authored });
        let id = board.alloc_id();
        board.vias.push(Via { id, net: ground, pad: mm(0.45), drill: mm(0.3), at: P::mm(4.0, 5.0),
            span: (LayerId::F_CU, LayerId::B_CU), kind: ViaKind::Through, tent: Tent::Both,
            backdrill: None, flash: Flash::All, locked: false, origin: Origin::Authored });
        board.reindex();
        let before = board.content_hash();
        let mut design = Design { units: vec![Unit { id: unit, name: "stitch".into(), kind: UnitKind::Board,
            circuit: Arc::default(), board: Some(Arc::new(board)), supplied: vec![], factory: None }], ..Default::default() };
        let results = engines::full(&design).boards.remove(0);
        let plan = |design: &Design, results: &engines::BoardResults| stitch(design, unit, results, ground,
            Some(results.fills[0].zone), mm(5.0), None, None);
        let edit = plan(&design, &results).unwrap();
        let expected: BTreeSet<_> = [-10, -5, 0, 5, 10].into_iter().flat_map(|x|
            [-10, -5, 5, 10].into_iter().map(move |y| P::mm(f64::from(x), f64::from(y))))
            .filter(|p| *p != P::mm(5.0, 5.0)).collect();
        let mut sites = BTreeSet::new();
        for op in &edit.ops {
            let DesignOp::Board { op: BoardOp::Add(Item::Via(v)), .. } = op else { panic!("expected a via") };
            assert!(sites.insert(v.at));
            assert_eq!(v.origin, Origin::Authored);
            assert!(clearance(&segment, &v.shape()) >= mm(0.15));
            for fill in results.fills.iter() {
                let copper = fill.layers[0].set();
                assert!(copper.contains(v.at));
                for ring in copper.rings() {
                    for (&a, &b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
                        assert!(fab_elec_core::point_clearance(v.at, &Shape::Segment { a, b, w: 0 }) >= v.pad / 2);
                    }
                }
            }
        }
        assert_eq!(sites, expected);
        assert_eq!(edit.label, format!("Stitch GND: {} vias", sites.len()));
        let applied = design.apply_all(&edit.ops, false).unwrap();
        assert!(plan(&design, &results).unwrap_err().contains("refilling"));
        let current = engines::full(&design).boards.remove(0);
        assert!(current.drc.iter().all(|v| results.drc.iter().any(|old| old.id == v.id)), "new DRC: {:?}", current.drc);
        assert!(plan(&design, &current).unwrap_err().contains("already stitched"));
        let mut held = current.clone();
        held.pending.zones.push((held.fills[0].zone, held.generation));
        assert!(plan(&design, &held).unwrap_err().contains("refilling"));
        let [step] = applied.as_slice() else { panic!("one board edit") };
        design.apply_all(&step.inverse(), true).unwrap();
        assert_eq!(design.units[0].board.as_ref().unwrap().content_hash(), before);

        // A signal on an inner layer does not cut either outside pour, but a through via
        // would still hit it. Only the spatial clearance check can reject the centre site.
        let inner = LayerId::inner(1);
        let board = Arc::make_mut(design.units[0].board.as_mut().unwrap());
        let mut layer = board.stackup.layers.iter().find(|l| l.kind == fab_elec_board::StackKind::Copper(LayerId::F_CU)).unwrap().clone();
        layer.kind = fab_elec_board::StackKind::Copper(inner);
        let bottom = board.stackup.layers.iter().position(|l| l.kind == fab_elec_board::StackKind::Copper(LayerId::B_CU)).unwrap();
        board.stackup.layers.insert(bottom, layer);
        board.tracks[0].layer = inner;
        board.reindex();
        let results = engines::full(&design).boards.remove(0);
        assert!(results.fills.iter().all(|f| f.layers[0].set().contains(P::new(0, 0))));
        let edit = plan(&design, &results).unwrap();
        assert_eq!(edit.ops.len(), expected.len());
        for op in edit.ops {
            let DesignOp::Board { op: BoardOp::Add(Item::Via(v)), .. } = op else { panic!("expected a via") };
            assert!(expected.contains(&v.at));
        }
        let board = Arc::make_mut(design.units[0].board.as_mut().unwrap());
        board.zones.pop();
        board.reindex();
        let one_layer = engines::full(&design).boards.remove(0);
        assert!(plan(&design, &one_layer).unwrap_err().contains("no overlap"));
    }

    /// Why: the strip's Place places the selection again; with nothing selected it places what is
    /// not on the board (the tray, a part outside the outline), and with every part on the board
    /// it says so instead of starting a job that has nothing to do.
    #[test]
    fn place_scope_names_the_selection_or_what_is_off_the_board() {
        let mut design = fab_elec_fixtures::ledctl();
        let unit = design.units[0].id;
        // A resistor: an edge part (a connector) may overhang the outline.
        let at = design.units[0].board.as_ref().expect("fixture setup: ledctl has a board").placed.iter()
            .position(|p| p.path == "lamps[0].r").expect("fixture setup: a lamp resistor is placed");
        let first = design.units[0].board.as_ref().expect("the board").placed[at].clone();
        assert_eq!(place_scope(&design, unit, &[first.comp]), Ok(vec![first.comp]), "a selection is placed again by name");
        assert!(place_scope(&design, unit, &[]).is_err(), "with every part on the board inside its outline, Place says so");
        // The panel parts that stand (an LED, a body with a light port) stay, and the strip
        // offers to place them anyway: alone, nothing goes; beside a resistor, the job goes too.
        let comp = |path: &str| design.units[0].circuit.comps.iter().find(|c| c.path == path).map(|c| c.id).expect(path);
        let (led, other) = (comp("lamps[0].d"), comp("lamps[1].d"));
        let press = |design: &Design, selected: &[CompId]| PlacePress::of(design, unit, selected).expect("a selection is a scope");
        let alone = press(&design, &[led, other]);
        assert_eq!((alone.held.as_slice(), alone.send()), (&[led, other][..], false), "only standing panel parts: nothing goes, both are offered");
        let beside = press(&design, &[first.comp, led]);
        assert_eq!((beside.held.as_slice(), beside.send()), (&[led][..], true), "a resistor beside an LED: the job goes, the LED is offered");
        assert!(press(&design, &[first.comp]).held.is_empty(), "a part with no port is never held");
        let board = Arc::make_mut(design.units[0].board.as_mut().expect("fixture setup: ledctl has a board"));
        let outline = board.outline_bbox();
        board.placed[at].at = P::new(outline.max.x + 20_000_000, outline.max.y + 20_000_000);
        board.reindex();
        assert_eq!(place_scope(&design, unit, &[]), Ok(vec![]), "a part outside the outline gives Place work with nothing selected");
        let board = Arc::make_mut(design.units[0].board.as_mut().expect("the board"));
        board.placed.remove(at);
        board.reindex();
        assert_eq!(place_scope(&design, unit, &[]), Ok(vec![]), "a part in the tray gives Place work with nothing selected");
    }

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
    fn placed_part_transforms_and_undo() {
        let mut design = fab_elec_fixtures::ledctl();
        let index = design.units.iter().position(|u| u.board.is_some()).unwrap();
        let unit = design.units[index].id;
        // Keep the three footprints under test and both ends of the LED's track. Other
        // rows of LEDs would only repeat the label-arrangement work on every assertion.
        let paths = ["usb.j", "mcu.dbg", "lamps[0].d"];
        let fixture = Arc::make_mut(design.units[index].board.as_mut().unwrap());
        fixture.placed.retain(|p| paths.contains(&p.path.as_str()) || p.path == "lamps[0].r");
        let led = fixture.placed.iter().find(|p| p.path == paths[2]).unwrap().id;
        fixture.tracks.retain(|t| t.ends.iter().any(|a| a.item() == Some(led)));
        fixture.reindex();
        let board = design.units[index].board.clone().unwrap();
        // Different footprints in increasing X order, a bottom-side header and a routed LED.
        let parts = paths.map(|path|
            board.placed.iter().find(|p| p.path == path).expect("ledctl places the selected part"));
        let ids = parts.map(|p| p.comp);
        let bounds = |part: &Placed| BBox::of_points(
            part.footprint.courtyard.rings().flatten().map(|&point| part.to_board(point)));
        let whole = parts.iter().map(|p| bounds(p)).fold(BBox::EMPTY, BBox::union);
        assert!(!whole.is_empty(), "the fixture supplies physical courtyard edges");
        let apply = |edit: Resolved| {
            let mut changed = design.clone();
            let applied = changed.apply_all(&edit.ops, false).expect("the edit applies through the user's path");
            let after = changed.unit(unit).unwrap().board.clone().unwrap();
            let [step] = applied.as_slice() else { panic!("{} must be one board batch", edit.label) };
            changed.apply_all(&step.inverse(), true).expect("the single inverse applies");
            let restored = Arc::make_mut(changed.units[index].board.as_mut().unwrap());
            restored.generation = board.generation; // undo advances this counter
            assert_eq!(&*restored, &*board, "one undo of {} restores the board, including all reference labels", edit.label);
            after
        };

        let solo = apply(rotate(&design, unit, &ids[..1], Angle::deg(-37.0)).unwrap());
        let p = solo.placed_item(parts[0].id).unwrap();
        assert_eq!(p.at, parts[0].at, "a single rotation keeps the part's own anchor");
        assert_eq!(p.rot, parts[0].rot.add(Angle::deg(-37.0)), "a negative rotation turns clockwise");

        let edit = rotate(&design, unit, &ids, Angle::QUARTER).unwrap();
        assert_eq!(edit.label, format!("Rotate {}, {} (+1)", parts[0].refdes, parts[1].refdes), "the group label names and counts the parts");
        let after = apply(edit);
        let old_track = &board.tracks[0];
        let end = old_track.ends.iter().position(|a| a.item() == Some(parts[2].id)).expect("the LED has an anchored track");
        let Anchor::Pad { placed, pad } = old_track.ends[end] else { panic!("the LED track ends on its pad") };
        let rotated_track = &after.tracks[0];
        let endpoint = if end == 0 { rotated_track.path.start } else { rotated_track.path.end() };
        assert_eq!(rotated_track.ends[end], old_track.ends[end], "rotation retains the LED pad anchor");
        assert_eq!(Some(endpoint), after.placed_item(placed).unwrap().pad_at(pad), "the anchored copper follows the rotated pad");
        let cx = (f64::from(whole.min.x) + f64::from(whole.max.x)) / 2.0;
        let cy = (f64::from(whole.min.y) + f64::from(whole.max.y)) / 2.0;
        for old in parts {
            let new = after.placed_item(old.id).unwrap();
            let expected = P::from_f(cx + cy - f64::from(old.at.y), cy - cx + f64::from(old.at.x));
            assert_eq!(new.at, expected, "{} orbits the centre of the combined courtyards counter-clockwise", old.refdes);
            assert_eq!(new.rot, old.rot.add(Angle::QUARTER), "{} also turns a quarter turn", old.refdes);
        }
        for group in [&ids[1..2], &ids[..]] {
            let flipped = apply(flip(&design, unit, group).unwrap());
            if group.contains(&parts[2].comp) {
                let track = &flipped.tracks[0];
                assert_eq!(track.ends[end], Anchor::Free, "flipping the SMD LED frees its old-layer track end");
                assert!(fab_elec_board::dangling_ends(&flipped, track)[end], "the released SMD track end is Dangling");
            }
            let twice_x = if group.len() == 1 { 2 * parts[1].at.x } else { whole.min.x + whole.max.x };
            for &id in group {
                let old = board.placed_of(id).unwrap();
                let new = flipped.placed_of(id).unwrap();
                assert_eq!(new.side, old.side.flipped(), "{} switches board side", old.refdes);
                for pad in &old.footprint.pads {
                    let was = old.to_board(pad.at);
                    assert_eq!(new.to_board(pad.at), P::new(twice_x - was.x, was.y),
                        "flipping {} mirrors each pad about the selection's vertical axis", old.refdes);
                }
            }
        }
        for edge in [Edge::Left, Edge::CentreY] {
            let after = apply(align(&design, unit, &ids, edge).unwrap());
            for old in parts {
                let new = after.placed_item(old.id).unwrap();
                let b = bounds(new);
                match edge {
                    Edge::Left => assert_eq!((b.min.x, new.at.y), (whole.min.x, old.at.y), "left edges align with only horizontal moves"),
                    Edge::CentreY => assert_eq!((b.center().y, new.at.x), (whole.center().y, old.at.x), "centre lines align with only vertical moves"),
                    _ => unreachable!(),
                }
                assert_eq!((new.rot, new.side), (old.rot, old.side), "alignment preserves each pose's orientation");
            }
        }
        let after = apply(distribute(&design, unit, &[ids[2], ids[0], ids[1]], Axis::X).unwrap());
        let [first, middle, last] = parts.map(|p| after.placed_item(p.id).unwrap());
        assert_eq!((first.at, last.at), (parts[0].at, parts[2].at), "distribution keeps both outer anchors fixed");
        let (a, b, c) = (bounds(first), bounds(middle), bounds(last));
        assert!(((b.min.x - a.max.x) - (c.min.x - b.max.x)).abs() <= 1, "courtyard gaps agree to the nanometre grid");
        assert_eq!((middle.at.y, middle.rot, middle.side), (parts[1].at.y, parts[1].rot, parts[1].side), "distribute x moves only along x");

        assert!(align(&design, unit, &[ids[0], ids[0]], Edge::Top).unwrap_err().contains("two"), "two mentions of one part cannot align");
        assert!(distribute(&design, unit, &ids[..2], Axis::Y).unwrap_err().contains("three"), "distribution needs three distinct parts");
        assert!(rotate(&design, unit, &ids, Angle::deg(360.0)).unwrap_err().contains("nothing"), "a whole turn creates no undo step");
        let mutable = Arc::make_mut(design.units[index].board.as_mut().unwrap());
        mutable.placed.iter_mut().find(|p| p.comp == ids[1]).unwrap().locked = true;
        assert_eq!(flip(&design, unit, &ids).unwrap_err(), format!("{} is locked", parts[1].refdes), "one locked part refuses the whole selection");
        design.apply(&DesignOp::Board { unit, op: BoardOp::Remove(parts[0].id) }, false).unwrap();
        assert_eq!(rotate(&design, unit, &ids[..1], Angle::QUARTER).unwrap_err(), format!("{} is not placed", parts[0].refdes), "an unplaced part is named");

        // A non-cardinal placement on the nm grid: the distant pad distinguishes adjacent
        // micro-degree rotations, and the near pad lies on a half-nanometre rounding boundary.
        let board = Arc::make_mut(design.units[index].board.as_mut().unwrap());
        let part = board.placed.iter_mut().find(|p| p.comp == ids[2]).unwrap();
        part.at = P::ZERO;
        part.rot = Angle::deg(30.0);
        part.side = Side::Top;
        let pads = &mut Arc::make_mut(&mut part.footprint).pads;
        pads[0].at = P::new(0, 1_000_001);
        pads[1].at = P::new(100_000_000, 0);
        let pad_centres = |p: &Placed| [0, 1].map(|i| p.pad_at(fab_elec_core::PadIx(i)).unwrap());
        let mirrored = pad_centres(part).map(|p| p.mirrored_x(0));
        let edit = flip(&design, unit, &ids[2..]).expect("a 30-degree placement can flip about its anchor");
        design.apply_all(&edit.ops, false).unwrap();
        let flipped = design.unit(unit).unwrap().board.as_ref().unwrap().placed_of(ids[2]).unwrap();
        for (actual, expected) in pad_centres(flipped).into_iter().zip(mirrored) {
            assert!(actual.x.abs_diff(expected.x) <= 1 && actual.y.abs_diff(expected.y) <= 1,
                "30-degree pad positions round independently to the nm grid: {actual:?} must be within 1 nm per axis of {expected:?}");
        }
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
