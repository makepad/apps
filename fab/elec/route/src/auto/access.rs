//! Pin access connects terminal copper to the cells where a grid route can start or end.
//! A route starts or ends at a cell whose centre is free for a track end (the grid's exact
//! centre test, [`Grid::step_pass`] from a cell to itself); neighbouring pads can still block a
//! fine-pitch pad's own centre. Access searches nearby free centres and checks the connecting
//! stubs with the same `check_to` that DRC uses. On a pad or via a stub starts at its centre,
//! where the board puts an anchored end, straight or through one knee. On a track it
//! attaches on the centreline, where the board puts the anchored end, with a checked stub to a
//! centre inside the track's copper. At a free end of fixed track copper ([`end_access`]) it
//! attaches exactly at the end point, so the end is continued from its tip.
//!
//! Per piece, candidate checks cost O(candidates x (log n + k)) for k nearby world items,
//! with candidates bounded by the terminal's zone box, plus lazy grid rasterisation.
//! Ordering the accepted cells costs O(accepted log accepted).

use crate::grid::{Cell, CellBox, Grid, ObstacleSource, Pass, PlaneId, Probe};
use crate::source::WorldSource;
use fab_elec_board::{check_to, Anchor, ConnItem, Copper, ARC_ERR};
use fab_elec_core::{point_clearance, Angle, BBox, ClassId, ItemId, LayerId, NetId, Nm, OnTrack, Path, PolySet, Seg, Shape, P};
use std::ops::ControlFlow;

/// One way onto a terminal: grid cell `cell` on layer index `li`, attached at `point` by
/// `anchor`. The connection runs from `point` through `knee`, when present, to the cell
/// centre. Each straight segment is legal for the net's track width. `point` is where the
/// board puts an end with that anchor, so applying the route moves none of its copper.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Access {
    pub li: usize,
    pub cell: Cell,
    pub point: P,
    pub knee: Option<P>,
    pub anchor: Anchor,
    /// The connection ends in a new via at `point`.
    pub drop: bool,
}

/// What access needs to know about the routed net.
pub struct AccessNet {
    pub net: NetId,
    pub class: ClassId,
    pub w: Nm,
}

/// Append access cells for terminal copper `piece` on its own layer, indexed by `li` in
/// `grid`. The track `plane` must use `net`'s class and width. The appended entries have no
/// duplicates and are sorted by (squared attach-point-to-centre distance, li, cell.y, cell.x).
/// Existing entries in `out` are preserved.
pub fn access(
    grid: &mut Grid,
    src: &WorldSource<'_>,
    plane: PlaneId,
    li: usize,
    net: &AccessNet,
    piece: &Copper,
    anchor: Anchor,
    out: &mut Vec<Access>,
) {
    let terminal = match piece.item {
        ConnItem::Pad { .. } | ConnItem::Via(_) => true,
        ConnItem::Track { .. } => false,
        ConnItem::Hole(_) | ConnItem::Zone { .. } => return,
    };
    let bbox = piece.shape.bbox();
    if bbox.is_empty() {
        return;
    }
    let point = match &piece.shape {
        Shape::Circle { c, .. } | Shape::Rect { c, .. } | Shape::Oval { c, .. } => *c,
        Shape::Segment { a, b, .. } => BBox::new(*a, *b).center(),
        Shape::Arc { mid, .. } => *mid,
        Shape::Poly(_) => bbox.center(),
    };
    let probe = Probe { class: net.class, w: net.w, drill: None };
    let reach = src.max_need(&probe);
    let pitch = i64::from(grid.spec.pitch);
    // Beyond this zone, obstacles around the pad no longer shape the nearby grid. A longer
    // stub would be a route the grid did not check, not an access. No legal access in the
    // zone leaves the router to report Reason::Terminal.
    let zone = if terminal {
        bbox.width().max(bbox.height()) + 2 * (i64::from(reach) + grid.spec.padding(&probe)) + 4 * pitch
    } else {
        0
    };
    let rotation = match &piece.shape {
        Shape::Rect { rot, .. } | Shape::Oval { rot, .. } => Some(*rot),
        Shape::Circle { .. } => Some(Angle::ZERO),
        _ => None,
    };
    let axes = rotation.map(|rot| {
        [rot, rot.add(Angle::QUARTER), rot.add(Angle::HALF), rot.add(Angle::deg(270.0))]
            .map(|angle| {
                let (sin, cos) = angle.sin_cos();
                (cos, sin)
            })
    });
    let legal = |a, b| legal_segment(src, net, piece.layer, reach, a, b);
    let first = out.len();
    let mut radius = if terminal { 2 * pitch } else { 0 };
    let mut previous: Option<CellBox> = None;
    loop {
        let cells = grid.spec.cells_in(inflate(bbox, radius));
        if let Some(cells) = cells {
            for y in cells.min.y..=cells.max.y {
                for x in cells.min.x..=cells.max.x {
                    let cell = Cell { x, y };
                    if previous.is_some_and(|bounds| bounds.contains(cell)) {
                        continue;
                    }
                    if grid.step_pass(src, plane, li, cell, cell, net.net) != Pass::Free {
                        continue;
                    }
                    let centre = grid.spec.centre(cell);
                    let mut junction = None;
                    let (attach, knee) = if terminal {
                        if legal(point, centre) {
                            (point, None)
                        } else {
                            let dx = f64::from(centre.x) - f64::from(point.x);
                            let dy = f64::from(centre.y) - f64::from(point.y);
                            let knee = axes.iter().flatten().find_map(|&(ux, uy)| {
                                let t = dx * ux + dy * uy;
                                let e = (dx * uy - dy * ux).abs();
                                if t <= 0.0 || e > f64::from(grid.spec.pitch) {
                                    return None;
                                }
                                let knee = P::from_f(f64::from(point.x) + ux * t, f64::from(point.y) + uy * t);
                                if knee == centre || !legal(point, knee) || !legal(knee, centre) {
                                    return None;
                                }
                                Some(knee)
                            });
                            // The board puts an end anchored on a pad or via at its centre, so
                            // the stub from that centre is the copper: an attach elsewhere inside
                            // the copper would be moved back onto the stub just refused.
                            let Some(knee) = knee else { continue };
                            (point, Some(knee))
                        }
                    } else if point_clearance(centre, &piece.shape) == 0 {
                        // The board puts an end anchored on a track onto its centreline, so a
                        // junction attaches there and the stub to the cell centre is checked.
                        let Some((at, on)) = on_track(piece, centre) else { continue };
                        if on != centre && !legal(on, centre) {
                            continue;
                        }
                        junction = Some(at);
                        (on, (on != centre).then_some(centre))
                    } else {
                        continue;
                    };
                    let anchor = match (piece.item, junction) {
                        (ConnItem::Track { item, .. }, Some(at)) => Anchor::Track { id: item, at },
                        _ => anchor,
                    };
                    out.push(Access { li, cell, point: attach, knee, anchor, drop: false });
                }
            }
        }
        if out.len() > first || radius == zone || cells == Some(grid.spec.all()) || !terminal {
            break;
        }
        previous = cells;
        radius = (radius * 2).min(zone);
    }
    // Each cell is checked once; knees do not change the attach-point-to-centre order.
    out[first..].sort_unstable_by_key(|a| {
        (a.point.dist2(grid.spec.centre(a.cell)), a.li, a.cell.y, a.cell.x)
    });
}

/// Append access to the free end at `at` of fixed track copper `piece` (the track's end segment,
/// on layer index `li`), anchored by `anchor` on that end, so the board puts the new copper's end
/// exactly there. Candidates are free cell centres in a box about the point that doubles as the
/// pad search's does; the stub from the point is legal straight, or with a knee along the end
/// segment's outward direction or one of its normals. A centre behind the end (the stub would run
/// back over the track) is refused, and nothing attaches anywhere but the point. When the point
/// is a cell centre a route may start with a via there; the board attaches the end to it.
/// The appended entries are sorted like [`access`]'s; existing entries in `out` are preserved.
pub fn end_access(
    grid: &mut Grid,
    src: &WorldSource<'_>,
    plane: PlaneId,
    li: usize,
    net: &AccessNet,
    piece: &Copper,
    at: P,
    anchor: Anchor,
    out: &mut Vec<Access>,
) {
    let (a, b) = match &piece.shape {
        Shape::Segment { a, b, .. } => (*a, *b),
        Shape::Arc { a, mid, b, .. } => if *a == at { (*a, *mid) } else { (*mid, *b) },
        _ => return,
    };
    // The end segment's outward direction: away from the rest of the track.
    let (from, to) = if a == at { (b, a) } else { (a, b) };
    let (dx, dy) = (f64::from(to.x) - f64::from(from.x), f64::from(to.y) - f64::from(from.y));
    let len = dx.hypot(dy);
    let axes: Vec<(f64, f64)> = if len > 0.0 {
        let (ux, uy) = (dx / len, dy / len);
        vec![(ux, uy), (-uy, ux), (uy, -ux)]
    } else {
        Vec::new()
    };
    let probe = Probe { class: net.class, w: net.w, drill: None };
    let reach = src.max_need(&probe);
    let pitch = i64::from(grid.spec.pitch);
    let zone = 2 * (i64::from(reach) + grid.spec.padding(&probe)) + 4 * pitch;
    let legal = |p: P, q: P| legal_segment(src, net, piece.layer, reach, p, q);
    let first = out.len();
    let mut radius = 2 * pitch;
    let mut previous: Option<CellBox> = None;
    loop {
        let cells = grid.spec.cells_in(inflate(BBox::point(at), radius));
        if let Some(cells) = cells {
            for y in cells.min.y..=cells.max.y {
                for x in cells.min.x..=cells.max.x {
                    let cell = Cell { x, y };
                    if previous.is_some_and(|bounds| bounds.contains(cell)) {
                        continue;
                    }
                    if grid.step_pass(src, plane, li, cell, cell, net.net) != Pass::Free {
                        continue;
                    }
                    let centre = grid.spec.centre(cell);
                    let (cx, cy) = (f64::from(centre.x) - f64::from(at.x), f64::from(centre.y) - f64::from(at.y));
                    if cx * dx + cy * dy < 0.0 {
                        continue;
                    }
                    let knee = if legal(at, centre) {
                        None
                    } else {
                        let knee = axes.iter().find_map(|&(ux, uy)| {
                            let t = cx * ux + cy * uy;
                            let e = (cx * uy - cy * ux).abs();
                            if t <= 0.0 || e > f64::from(grid.spec.pitch) {
                                return None;
                            }
                            let knee = P::from_f(f64::from(at.x) + ux * t, f64::from(at.y) + uy * t);
                            if knee == centre || !legal(at, knee) || !legal(knee, centre) {
                                return None;
                            }
                            Some(knee)
                        });
                        let Some(knee) = knee else { continue };
                        Some(knee)
                    };
                    out.push(Access { li, cell, point: at, knee, anchor, drop: false });
                }
            }
        }
        if out.len() > first || radius >= zone || cells == Some(grid.spec.all()) {
            break;
        }
        previous = cells;
        radius = (radius * 2).min(zone);
    }
    out[first..].sort_unstable_by_key(|a| {
        (a.point.dist2(grid.spec.centre(a.cell)), a.li, a.cell.y, a.cell.x)
    });
}

/// Append access to a zone island by dropping a through via. A track end inside a pour would
/// be a dangling end; a via of the net inside the island joins it through the fill.
/// Candidates lie on a lattice anchored at cell (0, 0), free on every layer for the via plane.
/// The search doubles its reach from `near` until it finds access or covers the whole island.
pub fn zone_access(
    grid: &mut Grid,
    src: &WorldSource<'_>,
    via_plane: PlaneId,
    li_count: usize,
    net: &AccessNet,
    polys: &[&PolySet],
    pad: Nm,
    near: BBox,
    out: &mut Vec<Access>,
) {
    let bbox = polys.iter().fold(BBox::EMPTY, |bbox, poly| bbox.union(poly.bbox()));
    if bbox.is_empty() || li_count == 0 {
        return;
    }
    let near = if near.is_empty() { bbox } else { near };
    let step = (pad / grid.spec.pitch).max(1) as u64;
    let align = |v: u32| u64::from(v).div_ceil(step) * step;
    let first = out.len();
    let mut radius = 8 * i64::from(grid.spec.pitch);
    let mut previous: Option<CellBox> = None;
    loop {
        let grown = inflate(near, radius);
        let area = BBox {
            min: P::new(grown.min.x.max(bbox.min.x), grown.min.y.max(bbox.min.y)),
            max: P::new(grown.max.x.min(bbox.max.x), grown.max.y.min(bbox.max.y)),
        };
        let cells = grid.spec.cells_in(area);
        if let Some(cells) = cells {
            let reach = area.inflate(pad);
            // A cell in `area` stays a drop site iff its half-pad disc lies in the copper, and that
            // disc lies inside `reach`: clipping to `reach` before eroding changes no answer here.
            let inner = PolySet::union_all(polys.iter().copied().filter(|poly| poly.bbox().intersects(&reach)))
                .intersection(&PolySet::rect(reach))
                .offset(-((i64::from(pad) + 1) / 2) as Nm, ARC_ERR);
            for y in (align(cells.min.y)..=u64::from(cells.max.y)).step_by(step as usize) {
                // One row's edges, collected once: each cell's test visits only those.
                let row = inner.row(grid.spec.centre(Cell { x: cells.min.x, y: y as u32 }).y);
                for x in (align(cells.min.x)..=u64::from(cells.max.x)).step_by(step as usize) {
                    let cell = Cell { x: x as u32, y: y as u32 };
                    if previous.is_some_and(|bounds| bounds.contains(cell)) {
                        continue;
                    }
                    let point = grid.spec.centre(cell);
                    if !row.contains(point.x)
                        || !(0..li_count).all(|li| grid.state(src, via_plane, li, cell).free_for(net.net))
                    {
                        continue;
                    }
                    for li in 0..li_count {
                        out.push(Access { li, cell, point, knee: None, anchor: Anchor::Free, drop: true });
                    }
                }
            }
        }
        if out.len() > first || grown.contains_box(&bbox) {
            break;
        }
        previous = cells;
        radius *= 2;
    }
}

/// The position on track piece `piece`'s centreline nearest to `p`, as an anchor and as the point
/// the board computes for it (`Path::point_on`). `None` for a piece that is not a track's.
fn on_track(piece: &Copper, p: P) -> Option<(OnTrack, P)> {
    let (seg, a, mid, b) = match (piece.item, &piece.shape) {
        (ConnItem::Track { seg, .. }, Shape::Segment { a, b, .. }) => (seg, *a, None, *b),
        (ConnItem::Track { seg, .. }, Shape::Arc { a, mid, b, .. }) => (seg, *a, Some(*mid), *b),
        _ => return None,
    };
    let at = OnTrack::along(seg, a, mid, b, p);
    let host = Path { start: a, segs: vec![mid.map_or(Seg::Line(b), |mid| Seg::Arc { mid, end: b })] };
    Some((at, host.point_on(OnTrack { seg: 0, ..at })))
}

fn legal_segment(src: &WorldSource<'_>, net: &AccessNet, layer: LayerId, reach: Nm, a: P, b: P) -> bool {
    let stub = Copper {
        item: ConnItem::Track { item: ItemId(u32::MAX - 1), seg: 0 },
        net: Some(net.net),
        class: net.class,
        layer,
        shape: Shape::Segment { a, b, w: net.w },
    };
    let mut legal = true;
    src.world.query(layer, stub.shape.bbox().inflate(reach), |_, it| {
        if check_to(src.rules, &stub, &it.obstacle(), layer).is_some() {
            legal = false;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    legal
}

// A terminal's zone radius may exceed Nm::MAX.
// Widen before growing, then saturate coordinates just as BBox::inflate does.
fn inflate(bbox: BBox, radius: i64) -> BBox {
    let clamp = |v: i64| v.clamp(i64::from(Nm::MIN), i64::from(Nm::MAX)) as Nm;
    BBox {
        min: P::new(clamp(i64::from(bbox.min.x) - radius), clamp(i64::from(bbox.min.y) - radius)),
        max: P::new(clamp(i64::from(bbox.max.x) + radius), clamp(i64::from(bbox.max.y) + radius)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::GridSpec;
    use crate::{verify, Fixity, RBase, RItem, RPiece, World};
    use fab_elec_board::{resolve_rules, RuleItem, RuleKey};
    use fab_elec_circuit::NetKind;
    use fab_elec_core::{LayerId, LayerSet};

    #[test]
    fn fixture_pads_are_reachable() {
        let design = fab_elec_fixtures::ledctl();
        let unit = &design.units[0];
        let board = unit.board.as_deref().expect("access fixture has a board");
        let rules = resolve_rules(board, None);
        let base = RBase::build(board);
        let world = World::new(&base);
        let src = WorldSource::new(&world, &rules, board, &|_| false);
        let class = ClassId::default();
        let w = rules.class(class).track.unwrap_or_else(|| rules.track_min(class));
        let key = RuleKey { class, item: RuleItem::Track };
        let clearance = rules.clearance(key, key, LayerId::F_CU);
        let mut grid = Grid::new(GridSpec::covering(
            board.outline_bbox(),
            GridSpec::pitch_for(w + clearance),
            vec![LayerId::F_CU, LayerId::B_CU],
        ));
        let mut copper = Vec::new();
        let mut entries = Vec::new();
        let mut repeated = Vec::new();
        let mut vdd_checked = false;
        for placed in &board.placed {
            copper.clear();
            board.item_copper(placed.id, &mut copper);
            for piece in &copper {
                let ConnItem::Pad { placed: placed_id, pad: pad_ix } = piece.item else {
                    continue;
                };
                let Some(net_id) = piece.net else { continue };
                let signal = &unit.circuit.nets[net_id.ix()];
                let refdes = &placed.refdes;
                let number = &placed.footprint.pads[pad_ix.ix()].no;
                let vdd = placed.path == "mcu.u" && number == "4";
                if !vdd && (signal.kind != NetKind::Signal || signal.name == "GND") {
                    continue;
                }
                let class = if vdd { signal.class } else { class };
                let w = if vdd { rules.class(class).track.expect("VDD has a power track width") } else { w };
                let probe = Probe { class, w, drill: None };
                let plane = grid.plane(probe);
                let li = grid.spec.layers.iter().position(|&l| l == piece.layer)
                    .expect("fixture pad copper is on an outer layer");
                let net = AccessNet { net: net_id, class, w };
                let anchor = Anchor::Pad { placed: placed_id, pad: pad_ix };
                entries.clear();
                access(&mut grid, &src, plane, li, &net, piece, anchor, &mut entries);
                assert!(!entries.is_empty(), "every pad is reachable: {refdes} pad {number:?}");
                if vdd {
                    vdd_checked = true;
                    assert_eq!(unit.circuit.classes[class.ix()].name, "power", "VDD uses its power class");
                    assert_eq!(w, 500_000, "VDD access uses the full 0.5 mm power track width");
                    assert!(
                        entries.iter().any(|entry| entry.knee.is_some()),
                        "fine-pitch pads escape along their axis",
                    );
                    let bbox = piece.shape.bbox();
                    let zone = bbox.width().max(bbox.height())
                        + 2 * (i64::from(src.max_need(&probe)) + grid.spec.padding(&probe))
                        + 4 * i64::from(grid.spec.pitch);
                    let cells = grid.spec.cells_in(inflate(bbox, zone)).expect("VDD zone overlaps the grid");
                    assert!(
                        entries.iter().all(|entry| cells.contains(entry.cell)),
                        "VDD access stays inside its zone box",
                    );
                }
                for entry in &entries {
                    assert!(
                        grid.step_pass(&src, plane, entry.li, entry.cell, entry.cell, net_id) == Pass::Free,
                        "access cell centres are free: {refdes} pad {number:?}, {entry:?}",
                    );
                    assert_eq!(entry.anchor, anchor, "access preserves the anchor: {refdes} pad {number:?}");
                    let centre = grid.spec.centre(entry.cell);
                    if entry.point == centre {
                        assert_eq!(
                            point_clearance(centre, &piece.shape),
                            0,
                            "access without a stub overlaps copper: {refdes} pad {number:?}",
                        );
                        continue;
                    }
                    let mut from = entry.point;
                    for to in entry.knee.into_iter().chain(std::iter::once(centre)) {
                        if from == to {
                            continue;
                        }
                        let shape = Shape::Segment { a: from, b: to, w };
                        let stub = RItem {
                            bbox: shape.bbox(),
                            piece: RPiece::Copper(Copper {
                                item: ConnItem::Track { item: ItemId(u32::MAX - 1), seg: 0 },
                                net: Some(net_id),
                                class,
                                layer: piece.layer,
                                shape,
                            }),
                            item: Some(ItemId(u32::MAX - 1)),
                            rank: 0,
                            layers: LayerSet::EMPTY.with(piece.layer),
                            fixity: Fixity::Solved,
                            locked: false,
                            kept: None,
                        };
                        let collisions = verify(&world, &rules, board, &[stub]);
                        let property = if vdd {
                            "fine-pitch pads escape along their axis"
                        } else {
                            "access stubs are legal"
                        };
                        assert!(
                            collisions.is_empty(),
                            "{property}: {refdes} pad {number:?}, {entry:?}: {collisions:?}",
                        );
                        from = to;
                    }
                }
                let key = |a: &Access| {
                    (a.point.dist2(grid.spec.centre(a.cell)), a.li, a.cell.y, a.cell.x)
                };
                assert!(
                    entries.windows(2).all(|pair| key(&pair[0]) < key(&pair[1])),
                    "access is ordered without duplicates: {refdes} pad {number:?}",
                );
                repeated.clear();
                access(&mut grid, &src, plane, li, &net, piece, anchor, &mut repeated);
                assert_eq!(entries, repeated, "access is deterministic: {refdes} pad {number:?}");
            }
        }
        assert!(vdd_checked, "the fixture checks mcu.u pad 4 (VDD) with its power class");
    }
}
