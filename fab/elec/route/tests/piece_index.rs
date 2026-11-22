//! A persistent piece snapshot agrees with fresh derivation through board and fill edits.

use fab_elec_board::zone::fill_all;
use fab_elec_board::{
    fill_update, resolve_rules, Anchor, BoardOp, ConnItem, Field, FillLattice, Item, Origin, PieceIndex, PieceKind,
    Track,
};
use fab_elec_core::{mm, BBox, LayerId, Path, P};
use fab_elec_fixtures::ledctl;
use fab_elec_parts::ThermalRule;
use std::ops::ControlFlow;
use std::sync::Arc;

#[test]
fn index_update_equals_build() {
    let design = ledctl();
    let mut board = design.units[0].board.as_deref().expect("piece index fixture has a board").clone();
    let rules = resolve_rules(&board, None);
    let gnd = board.net("GND").expect("zone clearance: the fixture has GND");
    let signal = board.net("LED0").expect("add track: the fixture has LED0");
    let zone = board.zones.iter().find(|zone| zone.net == Some(gnd)).expect("zone clearance: GND has a zone");
    let zone_id = zone.id;
    let thermal = ThermalRule { gap: FillLattice::of(&board, &rules).side(), ..zone.settings.thermal.clone() };
    let mut settings = zone.settings.clone();
    settings.clearance = Some(mm(0.3));
    let placed = board
        .placed
        .iter_mut()
        .find(|part| part.path == "mcu.dec[0]")
        .expect("move part: the fixture has the decoupling part");
    let pad = placed.pad_nets.iter().position(|net| *net == Some(gnd))
        .expect("relief entries: the part has a GND pad");
    Arc::make_mut(&mut placed.footprint).pads[pad].thermal = Some(thermal);
    let saved = placed.clone();
    let part = saved.id;
    let mut fills = fill_all(&board, &rules);
    let mut idx = PieceIndex::build(&board, &fills);
    assert!(
        idx.pieces(part).any(|piece| piece.relief.is_some()),
        "relief entries: the big-relief pad is indexed"
    );

    let mut islands: Vec<_> = idx
        .iter()
        .filter_map(|piece| match &piece.kind {
            PieceKind::Fill(copper) => Some(copper),
            _ => None,
        })
        .collect();
    let connected = fab_elec_board::connect::pieces(&board, &fills);
    let mut expected: Vec<_> = connected
        .iter()
        .filter(|copper| matches!(copper.item, ConnItem::Zone { .. }))
        .collect();
    islands.sort_by_key(|copper| (copper.item, copper.net, copper.layer));
    expected.sort_by_key(|copper| (copper.item, copper.net, copper.layer));
    assert!(!islands.is_empty(), "islands match connectivity: the fixture exercises fill islands");
    assert_eq!(islands, expected, "islands match connectivity: identity, net, layer, class and shape");

    let via = board.vias[0].id;
    let track_id = board.alloc_id();
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
    let ops = [
        ("add track", BoardOp::Add(Item::Track(track))),
        ("move part", BoardOp::Move { item: part, by: P::mm(0.4, 0.0) }),
        ("remove part", BoardOp::Remove(part)),
        ("remove via", BoardOp::Remove(via)),
        ("add part", BoardOp::Add(Item::Placed(saved))),
        ("zone clearance", BoardOp::SetField { item: zone_id, field: Field::ZoneSettings(settings) }),
        ("remove track", BoardOp::Remove(track_id)),
    ];
    let mut snapshot = None;
    for (step, (stage, op)) in ops.iter().enumerate() {
        if step == 2 {
            let snap = idx.clone();
            snapshot = Some((snap, board.clone(), fills.clone()));
        }
        let applied = board.apply(op).unwrap_or_else(|error| panic!("edit application: {stage}: {error}"));
        let mut refilled = Vec::new();
        for fill in &mut fills {
            let next = fill_update(&board, &rules, fill, &applied.delta);
            if !next.same(fill) {
                refilled.push(fill.zone);
            }
            *fill = next;
        }
        idx.update(&board, &fills, &applied.delta, &refilled);
        let fresh = PieceIndex::build(&board, &fills);
        assert_eq!(idx, fresh, "update equals build: {stage}");
        assert!(idx.iter().eq(fresh.iter()), "update equals build: iteration order after {stage}");
        assert_eq!(idx.len(), idx.iter().count(), "piece count includes the outline: {stage}");
        if board.placed_item(part).is_some() {
            assert!(
                idx.pieces(part).any(|piece| piece.relief.is_some()),
                "relief entries: the big-relief pad is indexed after {stage}"
            );
        }
    }
    let (snap, saved_board, saved_fills) = snapshot.expect("a snapshot never changes: taken before the third op");
    assert_eq!(snap, PieceIndex::build(&saved_board, &saved_fills), "a snapshot never changes");

    let bounds = board.outline_bbox();
    assert!(
        bounds.width() >= i64::from(mm(10.0)) && bounds.height() >= i64::from(mm(10.0)),
        "query equals brute force: the outline can contain the largest query"
    );
    let big = idx.pieces(part).find(|piece| piece.relief.is_some()).expect("relief entries: the pad is back");
    let relief_box = big.bbox.inflate(big.relief.expect("relief entries: the pad has its own thermal"));
    // A window touching the relief's far edge cannot find the pad through its ordinary box.
    let relief_area = BBox::new(
        P::new(relief_box.max.x, big.bbox.min.y),
        P::new(relief_box.max.x + mm(0.1), big.bbox.max.y),
    );
    let mut seed = 0x6a09_e667_f3bc_c909u64;
    let mut random = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for &layer in idx.layers() {
        for case in 0..=30 {
            let area = if case == 30 {
                relief_area
            } else {
                let width = mm(0.1) + (random() % (mm(9.9) as u64 + 1)) as i32;
                let height = mm(0.1) + (random() % (mm(9.9) as u64 + 1)) as i32;
                let x = bounds.min.x + (random() % (bounds.width() - i64::from(width) + 1) as u64) as i32;
                let y = bounds.min.y + (random() % (bounds.height() - i64::from(height) + 1) as u64) as i32;
                BBox::new(P::new(x, y), P::new(x + width, y + height))
            };
            let mut keys = Vec::new();
            idx.query(layer, area, |piece| {
                keys.push(piece.key());
                ControlFlow::Continue(())
            });
            let expected: Vec<_> = idx
                .iter()
                .filter(|piece| piece.layers.has(layer) && !piece.bbox.is_empty() && piece.bbox.intersects(&area))
                .map(|piece| piece.key())
                .collect();
            assert!(
                keys.windows(2).all(|pair| pair[0] < pair[1]),
                "query equals brute force: keys strictly increase on {layer:?}, area {case}"
            );
            assert_eq!(keys, expected, "query equals brute force: completeness on {layer:?}, area {case}");
            for r in [0, mm(0.5), mm(2.0)] {
                let mut keys = Vec::new();
                idx.query_fill(layer, area, r, |piece| {
                    keys.push(piece.key());
                    ControlFlow::Continue(())
                });
                let expected: Vec<_> = idx
                    .iter()
                    .filter(|piece| {
                        piece.layers.has(layer)
                            && !matches!(&piece.kind, PieceKind::Fill(_) | PieceKind::Edge(_))
                            && !piece.bbox.is_empty()
                            && (piece.bbox.intersects(&area.inflate(r))
                                || piece.relief.is_some_and(|relief| piece.bbox.inflate(relief).intersects(&area)))
                    })
                    .map(|piece| piece.key())
                    .collect();
                assert!(
                    keys.windows(2).all(|pair| pair[0] < pair[1]),
                    "fill query keys strictly increase: {layer:?}, area {case}, r {r}"
                );
                assert_eq!(
                    keys, expected,
                    "fill query equals brute force: completeness on {layer:?}, area {case}, r {r}"
                );
                if case == 30 && big.layers.has(layer) {
                    assert!(
                        !big.bbox.inflate(r).intersects(&area),
                        "relief query coverage: the window misses the pad's rule-inflated box for r {r}"
                    );
                    assert!(relief_box.intersects(&area), "relief query coverage: closed relief boxes touch");
                    assert!(keys.contains(&big.key()), "relief query coverage: the relief quad supplies the pad");
                }
            }
        }
    }
}
