//! Guards manufacturable GND pours and agreement of incremental fill, connectivity and DRC
//! with full results through board edits, including exact restoration after undo.

use fab_elec_board::zone::{fill_all, FillLayer, PolyRef};
use fab_elec_board::{
    check_pair, check_to, connect, connect_update, drc, fill, fill_update, merge_violations,
    resolve_rules, Anchor, Board, BoardOp, ConnItem, Connectivity, Copper, Field, Item, Obstacle,
    Origin, RuleMatrix, Track, ZoneFill, ARC_ERR,
};
use fab_elec_circuit::Violation;
use fab_elec_core::{clearance, mm, Canon, ContentHash, ItemId, LayerId, Path, PolySet, Seg, Shape, P};
use fab_elec_fixtures::ledctl;
use fab_elec_parts::{PadKind, ZoneConnect};
use std::collections::BTreeMap;

fn full(board: &Board) -> (RuleMatrix, Vec<ZoneFill>, Connectivity, Vec<Violation>) {
    let rules = resolve_rules(board, None);
    let fills = fill_all(board, &rules);
    let conn = connect(board, &fills);
    let violations = drc(board, &conn, &rules, &fills, None).violations;
    (rules, fills, conn, violations)
}

fn layer_fill(fills: &[ZoneFill], zone: ItemId, layer: LayerId) -> &FillLayer {
    fills
        .iter()
        .find(|fill| fill.zone == zone)
        .expect("fill coverage: the zone has a fill")
        .layer(layer)
        .expect("fill coverage: the zone has its requested layer")
}

#[test]
fn gnd_fill_is_manufacturable() {
    let design = ledctl();
    let board = design.units[0].board.as_deref().expect("manufacturability fixture has a board");
    let gnd = board.net("GND").expect("manufacturability fixture has GND");
    let zone = board.zones.iter().find(|zone| zone.net == Some(gnd)).expect("GND has a zone");
    let rules = resolve_rules(board, None);
    let fills = fill_all(board, &rules);
    let copper = board.all_copper();
    let edges: Vec<_> = board
        .outline
        .iter()
        .flat_map(|contour| contour.path.steps())
        .map(|(a, seg)| match seg {
            Seg::Line(b) => Shape::Segment { a, b, w: 0 },
            Seg::Arc { mid, end } => Shape::Arc { a, mid, b: end, w: 0 },
        })
        .collect();
    let drills: Vec<_> = board
        .drills()
        .into_iter()
        .map(|h| (h.item, h.net, h.shape()))
        .filter(|(_, net, _)| *net != Some(gnd))
        .collect();
    let conn = connect(board, &fills);
    let cluster = conn
        .clusters
        .iter()
        .find(|cluster| {
            cluster.items.iter().any(|item| matches!(item, ConnItem::Zone { item, .. } if *item == zone.id))
        })
        .expect("pour connectivity: GND fill belongs to a cluster");

    for layer in zone.layers.iter() {
        let filled = layer_fill(&fills, zone.id, layer);
        let set = filled.set();
        assert!(!set.is_empty(), "fill coverage: GND fills {}", layer.name());
        let mut islands: BTreeMap<PolyRef, Vec<&PolySet>> = BTreeMap::new();
        for (r, poly) in filled.kept() {
            let i = filled.tiles.binary_search_by_key(&r.tile, |tile| tile.key).expect("kept tile exists");
            islands.entry(filled.labels[i].island[r.ord as usize]).or_default().push(poly);
            let poured = Copper {
                item: ConnItem::Zone { item: zone.id, layer, tile: r.tile, ord: r.ord },
                net: zone.net,
                class: board.nets[gnd.ix()].class,
                layer,
                shape: Shape::Poly(poly.clone()),
            };
            for other in copper.iter().filter(|piece| piece.layer == layer && piece.net != Some(gnd)) {
                assert_eq!(
                    check_pair(&rules, &poured, other, layer),
                    None,
                    "copper clearance: {:?} keeps clear of {:?}",
                    poured.item,
                    other.item
                );
            }
            for (edge, step) in edges.iter().enumerate() {
                assert_eq!(
                    check_to(&rules, &poured, &Obstacle::Edge(step), layer),
                    None,
                    "edge clearance: {:?} keeps clear of outline step {edge}",
                    poured.item
                );
            }
            for (item, net, shape) in &drills {
                let drill = Obstacle::Drill { item: *item, net: *net, shape };
                assert_eq!(
                    check_to(&rules, &poured, &drill, layer),
                    None,
                    "drill clearance: {:?} keeps clear of {item:?}",
                    poured.item
                );
            }
        }
        for (r, polys) in islands {
            let island = PolySet::union_all(polys);
            let min_width = f64::from(zone.settings.min_width);
            assert!(
                island.area() >= min_width * min_width,
                "minimum island area: GND island {r:?} on {} survives without a sliver",
                layer.name()
            );
            let shape = Shape::Poly(island);
            assert!(
                copper.iter().any(|piece| {
                    piece.layer == layer && piece.net == Some(gnd) && clearance(&shape, &piece.shape) == 0
                }),
                "island removal: GND island {r:?} on {} touches GND copper",
                layer.name()
            );
        }

        // Ignore inner clearance holes when deciding which pad centres the pour surrounds.
        let outer: Vec<_> = set
            .polygons()
            .iter()
            .map(|polygon| PolySet::from_ring_nonzero(polygon.outer.clone()))
            .collect();
        let mut connected_pads = 0;
        let mut thermal_pads = 0;
        for piece in copper.iter().filter(|piece| piece.layer == layer && piece.net == Some(gnd)) {
            let ConnItem::Pad { placed, pad } = piece.item else { continue };
            let placed = board.placed_item(placed).expect("GND pad has a placed owner");
            let fp_pad = &placed.footprint.pads[pad.ix()];
            let centre = placed.pad_at(pad).expect("GND pad has a board position");
            if outer.iter().any(|extent| extent.contains(centre)) {
                connected_pads += 1;
                assert!(
                    cluster.items.contains(&piece.item),
                    "pour connectivity: {} pad {} ({:?}) on {} joins the common GND fill cluster",
                    placed.refdes,
                    fp_pad.no,
                    piece.item,
                    layer.name()
                );
            }
            let thermal = match fp_pad.zone_connect.unwrap_or(zone.settings.connect) {
                ZoneConnect::Thermal => true,
                ZoneConnect::ThruThermal => fp_pad.kind == PadKind::Thru,
                ZoneConnect::Solid | ZoneConnect::Off => false,
            };
            if thermal {
                thermal_pads += 1;
                let rule = fp_pad.thermal.as_ref().unwrap_or(&zone.settings.thermal);
                let pad_set = piece.shape.to_polyset(ARC_ERR);
                let ring = pad_set.offset(rule.gap + ARC_ERR, ARC_ERR).difference(&pad_set);
                let covered = ring.intersection(&set).area();
                assert!(
                    covered > 0.0,
                    "thermal spokes: {} pad {} ({:?}) on {} meets fill across its gap ring",
                    placed.refdes,
                    fp_pad.no,
                    piece.item,
                    layer.name()
                );
                assert!(
                    covered < 0.6 * ring.area(),
                    "thermal gap: {} pad {} ({:?}) on {} covers less than 60% of its gap ring",
                    placed.refdes,
                    fp_pad.no,
                    piece.item,
                    layer.name()
                );
            }
        }
        assert!(connected_pads > 0, "pour connectivity coverage: GND pads lie within {}", layer.name());
        assert!(thermal_pads > 0, "thermal coverage: GND pads use reliefs on {}", layer.name());
    }
}

#[test]
fn zone_edits_update_equals_full() {
    let design = ledctl();
    let mut board = design.units[0].board.as_deref().expect("edit fixture has a board").clone();
    let start_hash: ContentHash = board.content_hash();
    let gnd = board.net("GND").expect("edit fixture has GND");
    let signal = board.net("LED0").expect("edit fixture has a signal net");
    let zone = board.zones.iter().find(|zone| zone.net == Some(gnd)).expect("GND has a zone");
    let zone_id = zone.id;
    let mut settings = zone.settings.clone();
    settings.clearance = Some(mm(0.3));
    let part = board
        .placed
        .iter()
        .find(|part| part.path == "mcu.dec[0]" && part.pad_nets.contains(&Some(gnd)))
        .expect("part movement: the decoupling part has a GND pad")
        .id;
    let (rules, mut fills, mut conn, mut violations) = full(&board);
    let start_fills = fills.clone();
    let start_back = layer_fill(&start_fills, zone_id, LayerId::B_CU).set();
    let track_id = board.alloc_id();
    // The bottom layer is open here, away from the through pads, vias and mounting holes.
    let track = Track {
        id: track_id,
        net: signal,
        layer: LayerId::B_CU,
        w: mm(0.25),
        widths: None,
        path: Path::new(P::mm(25.0, 12.0)).line(P::mm(35.0, 12.0)),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Authored,
    };
    let track_pieces: Vec<_> = track
        .path
        .steps()
        .enumerate()
        .map(|(seg, (a, step))| Copper {
            item: ConnItem::Track { item: track_id, seg: seg as u32 },
            net: Some(signal),
            class: board.nets[signal.ix()].class,
            layer: track.layer,
            shape: track.seg_shape(seg, a, &step),
        })
        .collect();
    let copper = board.all_copper();
    for piece in &track_pieces {
        for other in copper.iter().filter(|other| other.layer == piece.layer) {
            assert_eq!(
                check_pair(&rules, piece, other, piece.layer),
                None,
                "track placement clearance: {:?} is legal against {:?} before adding",
                piece.item,
                other.item
            );
        }
        assert!(
            piece.shape.to_polyset(ARC_ERR).intersection(&start_back).area() > 0.0,
            "track placement coverage: {:?} crosses the existing B_CU pour",
            piece.item
        );
    }
    // One edit per kind: an addition, a rule change, and a move (which also removes the old
    // geometry, so it proves refilling freed area); undo then removes the track.
    let ops = [
        ("add track", BoardOp::Add(Item::Track(track))),
        ("zone clearance", BoardOp::SetField { item: zone_id, field: Field::ZoneSettings(settings) }),
        ("move GND part", BoardOp::Move { item: part, by: P::mm(0.4, 0.0) }),
    ];
    let mut inverses = Vec::new();
    for (index, (stage, op)) in ops.iter().enumerate() {
        let applied = board.apply(op).unwrap_or_else(|error| panic!("edit application: {stage}: {error}"));
        let delta = &applied.delta;
        assert!(!delta.is_empty(), "edit delta: {stage} reports its change");
        let updated: Vec<_> = fills
            .iter()
            .map(|previous| fill_update(&board, &rules, previous, delta))
            .collect();
        for updated_zone in &updated {
            assert_eq!(
                *updated_zone,
                fill(&board, &rules, updated_zone.zone),
                "incremental fill: zone {:?} equals full fill after {stage}",
                updated_zone.zone
            );
        }
        fills = updated;
        let updated_conn = connect_update(&conn, &board, &fills, delta);
        assert_eq!(updated_conn, connect(&board, &fills), "incremental connectivity: equals full after {stage}");
        conn = updated_conn;
        let merged = merge_violations(&violations, &drc(&board, &conn, &rules, &fills, Some(delta)));
        assert_eq!(
            merged,
            drc(&board, &conn, &rules, &fills, None).violations,
            "incremental DRC: merged violations equal full DRC after {stage}"
        );
        violations = merged;
        if index == 0 {
            let back = layer_fill(&fills, zone_id, LayerId::B_CU);
            assert!(back.set().area() < start_back.area(), "track knockout area: adding the track reduces B_CU fill");
            for (r, poly) in back.kept() {
                let poured = Copper {
                    item: ConnItem::Zone { item: zone_id, layer: LayerId::B_CU, tile: r.tile, ord: r.ord },
                    net: Some(gnd),
                    class: board.nets[gnd.ix()].class,
                    layer: LayerId::B_CU,
                    shape: Shape::Poly(poly.clone()),
                };
                for piece in &track_pieces {
                    assert_eq!(
                        check_pair(&rules, &poured, piece, LayerId::B_CU),
                        None,
                        "track knockout clearance: B_CU polygon {r:?} keeps clear of {:?}",
                        piece.item
                    );
                }
            }
        }
        inverses.push(applied.inverse);
    }
    for inverse in inverses.iter().rev() {
        board.undo(inverse).expect("undo restoration: every inverse applies");
    }
    assert_eq!(board.content_hash(), start_hash, "undo restoration: canonical board hash equals the start");
    assert_eq!(fill_all(&board, &rules), start_fills, "undo restoration: every fill equals the start");
}
