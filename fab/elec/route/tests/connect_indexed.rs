use fab_elec_board::index::PieceIndex;
use fab_elec_board::zone::fill_all;
use fab_elec_board::{
    connect, connect_held, connect_indexed, fill, fill_update, resolve_rules, Anchor, Area, BoardDelta,
    BoardOp, ConnItem, IndexChange, Item, Origin, Track, Witness, Zone, ZoneSettings,
};
use fab_elec_core::{mm, BBox, LayerId, LayerSet, Nm, PadIx, Path, PolySet, P};
use fab_elec_fixtures::ledctl;

#[test]
fn indexed_equals_held() {
    enum Refill {
        Update,
        Hold,
        All,
    }

    let design = ledctl();
    let mut board = design.units[0].board.as_deref().expect("indexed fixture has a board").clone();
    let gnd = board.net("GND").expect("indexed fixture has GND");
    let signal = board.net("LED0").expect("step 1: indexed fixture has LED0");
    let gnd_zone = board
        .zones
        .iter()
        .find(|zone| zone.net == Some(gnd))
        .expect("steps 3 and 4: GND has a zone to hold")
        .id;
    let part = board
        .placed
        .iter()
        .find(|part| part.path == "mcu.dec[0]")
        .expect("step 3: the decoupling part is placed")
        .id;
    let resistor = board
        .placed
        .iter()
        .find(|part| part.path == "lamps[0].r")
        .expect("step 2: the resistor is placed");
    let a = resistor.pad_at(PadIx(0)).expect("step 2: resistor pad 1 has a position");
    let b = resistor.pad_at(PadIx(1)).expect("step 2: resistor pad 2 has a position");
    let short_net = resistor.pad_nets[0].expect("step 2: resistor pad 1 has a net");
    assert_ne!(resistor.pad_nets[0], resistor.pad_nets[1], "step 2: the short bridges two distinct nets");
    let back_id = board.alloc_id();
    let short_id = board.alloc_id();
    let back_track = Track {
        id: back_id,
        net: signal,
        layer: LayerId::B_CU,
        w: mm(0.25),
        widths: None,
        path: Path::new(P::mm(25.0, 12.0)).line(P::mm(35.0, 12.0)),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Authored,
    };
    let short_track = Track {
        id: short_id,
        net: short_net,
        layer: LayerId::F_CU,
        w: mm(0.25),
        widths: None,
        path: Path::new(a).line(b),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Authored,
    };
    // A back-copper track from edge to edge cuts the GND plane in two: islands split across tiles the
    // edit never reaches, and merge again when it goes.
    let outline = board.outline_bbox();
    let split_id = board.alloc_id();
    let split_track = Track {
        id: split_id,
        net: signal,
        layer: LayerId::B_CU,
        w: mm(0.25),
        widths: None,
        path: Path::new(P::new(outline.min.x, outline.center().y)).line(P::new(outline.max.x, outline.center().y)),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Authored,
    };
    // While the GND plane is held, a GND track and a second GND zone come over it where its refill
    // changes no tile: only the copper that changed while it was held tells the release to test them.
    let gnd_track_id = board.alloc_id();
    let gnd_track = Track {
        id: gnd_track_id,
        net: gnd,
        layer: LayerId::F_CU,
        w: mm(0.25),
        widths: None,
        path: Path::new(P::mm(31.0, 3.5)).line(P::mm(36.0, 3.5)),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Authored,
    };
    let patch_id = board.alloc_id();
    let patch = Zone {
        id: patch_id,
        net: Some(gnd),
        name: "GND patch".into(),
        layers: LayerSet::of(&[LayerId::F_CU]),
        area: Area::Poly(PolySet::rect(BBox::new(P::mm(30.0, 1.5), P::mm(42.0, 6.0)))),
        priority: 0,
        settings: ZoneSettings::default(),
        locked: false,
        origin: Origin::Authored,
    };
    // Steps 11 to 13: a second short track across the resistor, then a third net's track across it
    // between the pads, which the short gains.
    let short2_id = board.alloc_id();
    let cross_id = board.alloc_id();
    let short2 = Track { id: short2_id, ..short_track.clone() };
    let cross_net = board.net("LED1").expect("step 12: indexed fixture has LED1");
    let resistor_nets = &board.placed.iter().find(|part| part.path == "lamps[0].r").expect("step 12: the resistor is placed").pad_nets;
    assert!(!resistor_nets.contains(&Some(cross_net)), "step 12: the crossing track's net is a third net");
    let (mid, run) = (P::new((a.x + b.x) / 2, (a.y + b.y) / 2), b.sub(a));
    let run_len = (run.dist2(P::ZERO) as f64).sqrt();
    let across = |t: Nm| P::new(mid.x - (run.y as f64 * t as f64 / run_len) as Nm, mid.y + (run.x as f64 * t as f64 / run_len) as Nm);
    let cross = Track {
        id: cross_id,
        net: cross_net,
        layer: LayerId::F_CU,
        w: mm(0.1),
        widths: None,
        path: Path::new(across(mm(-0.6))).line(across(mm(0.6))),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Authored,
    };
    // Steps 14 and 15: two nets' tracks end on an MCU pin with no net (pin 15, between two others with
    // none), one from outside, one from under the body, apart from each other.
    let outer_id = board.alloc_id();
    let inner_id = board.alloc_id();
    let (outer_net, inner_net) = (
        board.net("LED2").expect("step 14: indexed fixture has LED2"),
        board.net("LED3").expect("step 15: indexed fixture has LED3"),
    );
    let mcu = board.placed.iter().find(|part| part.path == "mcu.u").expect("step 14: the MCU is placed");
    let nc = PadIx(14);
    assert!((13..=15).all(|k| mcu.pad_nets[k].is_none()), "step 14: MCU pins 14 to 16 have no net");
    let pin = mcu.pad_at(nc).expect("step 14: MCU pin 15 has a position");
    // Outward: across the row of pins, away from the part's centre.
    let row = pin.sub(mcu.pad_at(PadIx(13)).expect("step 14: MCU pin 14 has a position"));
    let row_len = (row.dist2(P::ZERO) as f64).sqrt();
    assert!((row_len - mm(0.65) as f64).abs() < mm(0.05) as f64, "step 14: MCU pins 14 and 15 are neighbours in a row");
    let from_centre = pin.sub(mcu.at);
    let side = if (-row.y as i64) * from_centre.x as i64 + row.x as i64 * from_centre.y as i64 > 0 { 1 } else { -1 };
    let out = P::new(-row.y * side, row.x * side);
    let out_len = row_len;
    let mcu_id = mcu.id;
    let along = |t: Nm| P::new(pin.x + (out.x as f64 * t as f64 / out_len) as Nm, pin.y + (out.y as f64 * t as f64 / out_len) as Nm);
    let to_pin = |id, net, from: Nm, to: Nm| Track {
        id,
        net,
        layer: LayerId::F_CU,
        w: mm(0.1),
        widths: None,
        path: Path::new(along(from)).line(along(to)),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Authored,
    };
    let outer = to_pin(outer_id, outer_net, mm(1.5), mm(0.2));
    let inner = to_pin(inner_id, inner_net, mm(-1.5), mm(-0.2));
    let rules = resolve_rules(&board, None);
    let mut fills = fill_all(&board, &rules);
    let mut idx = PieceIndex::build(&board, &fills);
    let mut prev = connect(&board, &fills);
    let steps = [
        ("step 1: add B_CU track", Some(BoardOp::Add(Item::Track(back_track))), Refill::Update),
        ("step 2: add short track", Some(BoardOp::Add(Item::Track(short_track))), Refill::Update),
        (
            "step 3: move decoupling part with held GND fill",
            Some(BoardOp::Move { item: part, by: P::mm(0.4, 0.0) }),
            Refill::Hold,
        ),
        ("step 4: remove short track with held GND fill", Some(BoardOp::Remove(short_id)), Refill::Hold),
        ("step 5: add a GND track over the held GND fill", Some(BoardOp::Add(Item::Track(gnd_track))), Refill::Hold),
        ("step 6: add a GND zone over the held GND fill", Some(BoardOp::Add(Item::Zone(patch))), Refill::Hold),
        ("step 7: refill all zones", None, Refill::All),
        ("step 8: remove B_CU track", Some(BoardOp::Remove(back_id)), Refill::Update),
        ("step 9: split the GND plane", Some(BoardOp::Add(Item::Track(split_track))), Refill::Update),
        ("step 10: join the GND plane again", Some(BoardOp::Remove(split_id)), Refill::Update),
        ("step 11: add a second short track", Some(BoardOp::Add(Item::Track(short2))), Refill::Update),
        // Guards: a short that gains a net is shown where the net joined, not at its first touch.
        ("step 12: cross the short with a third net's track", Some(BoardOp::Add(Item::Track(cross))), Refill::Update),
        ("step 13: remove the crossing track", Some(BoardOp::Remove(cross_id)), Refill::Update),
        ("step 14: run a track onto a pin with no net", Some(BoardOp::Add(Item::Track(outer))), Refill::Update),
        // Guards: nets that meet only through copper with no net are shown at that copper.
        ("step 15: run a second net's track onto that pin", Some(BoardOp::Add(Item::Track(inner))), Refill::Update),
    ];
    // What the index re-derived while the GND plane was held.
    let mut since = IndexChange::default();
    for (step_index, (step, op, refill)) in steps.into_iter().enumerate() {
        let delta = match op {
            Some(op) => board.apply(&op).unwrap_or_else(|error| panic!("edit application at {step}: {error}")).delta,
            // Releasing held fills changes the fill input without a board edit.
            None => BoardDelta::default(),
        };
        let mut refilled = Vec::new();
        for zone in &board.zones {
            if let Err(at) = fills.binary_search_by_key(&zone.id, |f| f.zone) {
                fills.insert(at, fill(&board, &rules, zone.id));
                refilled.push(zone.id);
            }
        }
        let held = matches!(refill, Refill::Hold);
        let added = refilled.clone();
        for previous in fills.iter_mut().filter(|f| !(held && f.zone == gnd_zone) && !added.contains(&f.zone)) {
            let next = match refill {
                Refill::All => fill(&board, &rules, previous.zone),
                _ => fill_update(&board, &rules, previous, &delta),
            };
            if matches!(refill, Refill::All) || !next.same(previous) {
                refilled.push(previous.zone);
            }
            *previous = next;
        }
        refilled.sort_unstable();
        let stale = if held { vec![gnd_zone] } else { Vec::new() };
        let change = idx.update(&board, &fills, &delta, &refilled);
        let next = connect_indexed(&prev, &idx, &board, &delta, &change, &stale, &since);
        assert_eq!(
            next,
            connect_held(&board, &fills, &prev, &stale),
            "indexed equals held after {step}"
        );
        if stale.is_empty() {
            since = IndexChange::default();
        } else {
            since.absorb(&change);
        }
        // The tiles of the GND plane's polygons joined to pieces `c` picks.
        let plane_joins = |c: &dyn Fn(ConnItem) -> bool| -> Vec<_> {
            next.joins.iter().filter_map(|j| match (j.a, j.b) {
                (ConnItem::Zone { item, layer, tile, .. }, other) | (other, ConnItem::Zone { item, layer, tile, .. })
                    if item == gnd_zone && c(other) => Some((item, layer, tile)),
                _ => None,
            }).collect()
        };
        let gnd_track_piece = |c: ConnItem| matches!(c, ConnItem::Track { item, .. } if item == gnd_track_id);
        let patch_piece = |c: ConnItem| matches!(c, ConnItem::Zone { item, .. } if item == patch_id);
        if step_index == 1 {
            assert!(!next.shorts.is_empty(), "a short is found after {step}");
            let names_short = |w: Witness| w.items.0.item() == short_id || w.items.1.item() == short_id;
            assert!(
                next.shorts.iter().all(|s| s.witness.is_some_and(names_short)),
                "the short is shown where the added track touches after {step}: {:?}", next.shorts
            );
        }
        if step_index == 3 {
            assert!(next.shorts.is_empty(), "no short remains after {step}");
        }
        let names = |w: Witness, c: ConnItem| w.items.0 == c || w.items.1 == c;
        let short_with = |net| next.shorts.iter().find(|s| s.nets.contains(&net));
        if step_index == 11 {
            let grown = short_with(cross_net).unwrap_or_else(|| panic!("the short gains the crossing net after {step}"));
            assert!(
                grown.witness.is_some_and(|w| names(w, ConnItem::Track { item: cross_id, seg: 0 })),
                "the short is shown where the third net joined after {step}: {grown:?}"
            );
        }
        if step_index == 14 {
            let bridged = short_with(outer_net).unwrap_or_else(|| panic!("the pin with no net joins two nets after {step}"));
            assert_eq!(bridged.nets.len(), 2, "only the two tracks' nets meet at the pin after {step}");
            let pad = ConnItem::Pad { placed: mcu_id, pad: nc };
            let at_pad = |w: Witness| names(w, pad) && w.at.dist2(pin) < (mm(1.0) as i128).pow(2);
            assert!(bridged.witness.is_some_and(at_pad), "the short is shown at the pin with no net after {step}: {bridged:?}");
        }
        if step_index == 5 {
            assert!(plane_joins(&gnd_track_piece).is_empty(), "the GND track joins no held polygon after {step}");
            assert!(plane_joins(&patch_piece).is_empty(), "the GND patch joins no held polygon after {step}");
        }
        if step_index == 6 {
            let joined = [("GND track", plane_joins(&gnd_track_piece)), ("GND patch", plane_joins(&patch_piece))];
            for (what, joined) in joined {
                assert!(!joined.is_empty(), "the released plane joins the {what} after {step}");
                assert!(
                    joined.iter().all(|tile| change.tiles.binary_search(tile).is_err()),
                    "the plane joins the {what} only through tiles its refill left as they were after {step}"
                );
            }
        }
        if (8..=9).contains(&step_index) {
            assert!(!change.relabeled.is_empty(), "far tiles take new island labels without new polygons after {step}");
        }
        prev = next;
    }
    assert_eq!(
        prev,
        connect(&board, &fills),
        "held with no stale zone is full connectivity after step 8"
    );
}
