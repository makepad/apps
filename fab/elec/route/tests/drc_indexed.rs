//! Indexed DRC agrees with full checks through refills and held stale zone geometry.

use fab_elec_board::connect::{connect_held, pieces};
use fab_elec_board::drc::{drc_indexed, sel_of};
use fab_elec_board::index::PieceIndex;
use fab_elec_board::zone::fill_all;
use fab_elec_board::{
    connect, drc, fill, fill_update, resolve_rules, Anchor, BoardOp, ConnItem, Item, Origin, Track, Via,
};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{mm, LayerId, PadIx, Path, Sel, P};
use fab_elec_fixtures::ledctl;

#[test]
fn indexed_equals_full() {
    let design = ledctl();
    let mut board = design.units[0].board.as_deref().expect("edit fixture has a board").clone();
    let rules = resolve_rules(&board, None);
    let mut fills = fill_all(&board, &rules);
    let mut idx = PieceIndex::build(&board, &fills);
    let mut conn = connect(&board, &fills);
    let mut prev = drc(&board, &conn, &rules, &fills, None).violations;
    let gnd = board.net("GND").expect("fixture has GND");
    let signal = board.net("LED0").expect("fixture has LED0");
    let zone = board.zones.iter().find(|zone| zone.net == Some(gnd)).expect("GND has a zone").id;
    let part = board
        .placed
        .iter()
        .find(|part| part.path == "mcu.dec[0]")
        .expect("fixture has the decoupling part")
        .id;
    let back = Track {
        id: board.alloc_id(),
        net: signal,
        layer: LayerId::B_CU,
        w: mm(0.25),
        widths: None,
        path: Path::new(P::mm(25.0, 12.0)).line(P::mm(35.0, 12.0)),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Authored,
    };
    let front_id = board.alloc_id();
    let placed = board.placed.iter().find(|part| part.path == "lamps[0].r").expect("fixture has the resistor");
    let front = Track {
        id: front_id,
        net: placed.pad_nets[0].expect("resistor pad 1 has a net"),
        layer: LayerId::F_CU,
        w: mm(0.25),
        widths: None,
        path: Path::new(placed.pad_at(PadIx(0)).expect("resistor pad 1 has a position"))
            .line(placed.pad_at(PadIx(1)).expect("resistor pad 2 has a position")),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Authored,
    };
    // Reapplying the same outline supplies a global delta for the final all-zone refresh.
    let ops = [
        ("1: add back track", BoardOp::Add(Item::Track(back))),
        ("2: short resistor pads", BoardOp::Add(Item::Track(front))),
        ("3: move GND part while held", BoardOp::Move { item: part, by: P::mm(0.4, 0.0) }),
        ("4: remove short while held", BoardOp::Remove(front_id)),
        ("5: refill every zone", BoardOp::SetOutline(board.outline.clone())),
    ];
    for (step, (stage, op)) in ops.iter().enumerate() {
        let applied = board.apply(op).unwrap_or_else(|error| panic!("edit application after {stage}: {error}"));
        let delta = &applied.delta;
        let stale = if step == 2 || step == 3 { vec![zone] } else { Vec::new() };
        let mut refilled = Vec::new();
        if stale.is_empty() {
            for previous in &mut fills {
                let next = if step == 4 {
                    fill(&board, &rules, previous.zone)
                } else {
                    fill_update(&board, &rules, previous, delta)
                };
                if step == 4 || !next.same(previous) {
                    refilled.push(previous.zone);
                }
                *previous = next;
            }
        }
        idx.update(&board, &fills, delta, &refilled);
        conn = connect_held(&board, &fills, &conn, &stale);
        let got = drc_indexed(&prev, &idx, &board, &conn, &rules, delta, &stale);
        let full = drc(&board, &conn, &rules, &fills, None).violations;
        if stale.is_empty() {
            assert_eq!(got, full, "indexed equals full after {stage}");
        } else {
            let mut stale_sels: Vec<_> = stale.iter().map(|zone| Sel::Item(board.unit, *zone)).collect();
            for copper in pieces(&board, &fills) {
                if matches!(copper.item, ConnItem::Zone { item, .. } if stale.contains(&item)) {
                    stale_sels.push(sel_of(&board, copper.item));
                }
            }
            let held = |v: &Violation| {
                !matches!(v.kind, CheckKind::Short | CheckKind::Unconnected)
                    && v.items.iter().any(|sel| stale_sels.contains(sel))
            };
            let mut expected: Vec<_> = full.into_iter().filter(|v| !held(v)).collect();
            expected.extend(prev.iter().filter(|v| held(v)).cloned());
            expected.sort_by_key(|v| v.id);
            expected.dedup_by_key(|v| v.id);
            assert_eq!(got, expected, "held findings carry over after {stage}");
        }
        if step == 1 {
            assert!(
                got.iter().any(|v| v.kind == CheckKind::Short)
                    && got.iter().any(|v| v.kind == CheckKind::Clearance),
                "the edit's findings appear"
            );
        }
        prev = got;
    }
}

/// Random via and part moves over ledctl's GND pour with the fills held as they were (fills are an input
/// of both forms): indexed DRC equals full DRC after every move. A foreign via moved into the pour brings
/// its drill against fill tiles that reach far beyond the scope, with drills inside and outside it.
#[test]
fn random_moves_over_a_pour_equal_full() {
    let design = ledctl();
    let mut board = design.units[0].board.as_deref().expect("edit fixture has a board").clone();
    let template = board.vias.first().expect("fixture has a via").clone();
    let foreign = Via {
        id: board.alloc_id(),
        net: board.net("LED0").expect("fixture has LED0"),
        at: template.at.add(P::mm(1.0, 1.0)),
        ..template
    };
    let foreign_id = foreign.id;
    board.apply_host(&BoardOp::Add(Item::Via(foreign))).expect("the foreign via is added");
    let rules = resolve_rules(&board, None);
    let fills = fill_all(&board, &rules);
    let mut idx = PieceIndex::build(&board, &fills);
    let mut prev = drc(&board, &connect(&board, &fills), &rules, &fills, None).violations;
    let mut movers: Vec<_> = board.vias.iter().map(|via| via.id).collect();
    movers.extend(board.placed.iter().take(3).map(|part| part.id));
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut random = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut hole_findings = 0;
    for step in 0..24 {
        let pick = random();
        let item = if pick % 2 == 0 { foreign_id } else { movers[(pick / 2) as usize % movers.len()] };
        let mut offset = || ((random() % 81) as f64 - 40.0) * 0.05;
        let by = P::mm(offset(), offset());
        let applied = board
            .apply_host(&BoardOp::Move { item, by })
            .unwrap_or_else(|error| panic!("move {step} applies: {error}"));
        idx.update(&board, &fills, &applied.delta, &[]);
        let conn = connect(&board, &fills);
        let got = drc_indexed(&prev, &idx, &board, &conn, &rules, &applied.delta, &[]);
        let full = drc(&board, &conn, &rules, &fills, None).violations;
        assert_eq!(got, full, "indexed equals full after move {step}");
        hole_findings += got.iter().filter(|v| v.kind == CheckKind::HoleClearance).count();
        prev = got;
    }
    assert!(hole_findings > 0, "moved drills meet the pour (hole clearance findings appear)");
}
