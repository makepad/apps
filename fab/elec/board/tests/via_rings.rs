//! Guards per-layer via rings (`Via::flash`, `Board::rings`): which span layers carry a via's ring
//! in each mode, that the op path keeps the ring table equal to a rebuild and names every via whose
//! rings changed in the delta, and that an offered ring is copper exactly where a fill reaches it
//! (fills, the piece index, connectivity, DRC) while the fill itself never depends on the reach.

use fab_elec_board::{
    connect, drc, fill, fill_update, resolve_rules, Anchor, Area, Board, BoardDelta, BoardKeepout, BoardNet, BoardOp, Copper,
    FillPlan, Flash, Islands, Item, LayerRole, Origin, PieceIndex, PieceKind, Rings, StackKind, StackLayer, Tent, Track,
    Via, ViaKind, Zone, ZoneFill, ZoneSettings,
};
use fab_elec_circuit::CheckKind;
use fab_elec_core::{
    mm, point_clearance, BBox, BoardId, Canon, ClassId, Contour, ItemId, LayerId, LayerSet, NetId, Path, PolySet, Shape, P,
};

const A: NetId = NetId(0);
const B: NetId = NetId(1);

fn in1() -> LayerId {
    LayerId::inner(1)
}
fn in2() -> LayerId {
    LayerId::inner(2)
}

/// A 4-layer board with nets A and B.
fn board() -> Board {
    let mut b = Board::new(BoardId(0), "rings");
    let cu = |l| StackLayer {
        kind: StackKind::Copper(l), thickness: 35_000, material: "copper".into(), er: None, loss_tan: None,
        role: LayerRole::Signal, velocity: None,
    };
    let at = b.stackup.layers.iter().position(|l| l.kind == StackKind::Copper(LayerId::B_CU)).unwrap();
    b.stackup.layers.insert(at, cu(in2()));
    b.stackup.layers.insert(at, cu(in1()));
    b.nets = vec![BoardNet { name: "A".into(), class: ClassId(0) }, BoardNet { name: "B".into(), class: ClassId(0) }];
    b
}

/// A through via of `net` at `x` mm, 0.6 mm ring.
fn via(b: &mut Board, net: NetId, x: f64, flash: Flash) -> Via {
    Via {
        id: b.alloc_id(), net, at: P::mm(x, 0.0), drill: mm(0.3), pad: mm(0.6), span: (LayerId::F_CU, LayerId::B_CU),
        kind: ViaKind::Through, tent: Tent::Both, backdrill: None, locked: false, origin: Origin::Authored, flash,
    }
}

/// An unanchored track of `net` on `layer` straight across the origin.
fn track(b: &mut Board, net: NetId, layer: LayerId) -> Track {
    Track {
        id: b.alloc_id(), net, layer, w: mm(0.2), widths: None, path: Path::new(P::mm(-1.0, 0.0)).line(P::mm(1.0, 0.0)),
        ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
    }
}

fn set(layers: &[LayerId]) -> LayerSet {
    LayerSet::of(layers)
}

/// The ring table equals a rebuild's: every via's rings and every zone's offers.
fn same_as_rebuild(b: &Board, when: &str) {
    let mut fresh = b.clone();
    fresh.reindex();
    for v in &b.vias {
        assert_eq!(b.rings(v), fresh.rings(v), "{when}: via {:?} rings equal a rebuild's", v.id);
    }
    for z in &b.zones {
        assert_eq!(b.rings.offers(z.id), fresh.rings.offers(z.id), "{when}: zone {:?} offers equal a rebuild's", z.id);
    }
}

#[test]
fn a_via_rings_where_its_mode_and_the_same_net_copper_say() {
    let mut b = board();
    let v = via(&mut b, A, 0.0, Flash::All);
    let id = v.id;
    b.vias.push(v);
    let own = track(&mut b, A, in1());
    let foreign = track(&mut b, B, in2());
    b.tracks.extend([own, foreign]);
    let zone = b.alloc_id();
    b.zones.push(Zone {
        id: zone, net: Some(A), name: "pour".into(), layers: set(&[in2()]), area: Area::Board, priority: 0,
        settings: ZoneSettings::default(), locked: false, origin: Origin::Authored,
    });

    let v = &b.vias[0];
    let old = [v.id.canon_bytes(), v.net.canon_bytes(), v.at.canon_bytes(), v.drill.canon_bytes(), v.pad.canon_bytes(),
        v.span.canon_bytes(), v.kind.canon_bytes(), v.tent.canon_bytes(), v.backdrill.canon_bytes(), v.locked.canon_bytes(),
        v.origin.canon_bytes()].concat();
    assert_eq!(v.canon_bytes(), old, "a via that rings every layer keeps its old canonical form");
    let connected = Via { flash: Flash::Connected, ..v.clone() };
    assert_ne!(connected.content_hash(), v.content_hash(), "a via's flashing is content");

    let all = set(&[LayerId::F_CU, in1(), in2(), LayerId::B_CU]);
    let cases = [
        (Flash::All, all, LayerSet::EMPTY),
        (Flash::Connected, set(&[in1()]), set(&[in2()])),
        (Flash::ConnectedEnds, set(&[LayerId::F_CU, in1(), LayerId::B_CU]), set(&[in2()])),
        (Flash::Ends, set(&[LayerId::F_CU, LayerId::B_CU]), LayerSet::EMPTY),
    ];
    for (flash, fixed, offer) in cases {
        b.vias[0].flash = flash;
        b.reindex();
        assert_eq!(
            b.rings(&b.vias[0]), Rings { fixed, offer },
            "{flash:?}: fixed on the mode's layers and where the unanchored own-net track crosses the disc, \
             never for the foreign track; the own-net zone offers its layer"
        );
        let mut copper: Vec<Copper> = Vec::new();
        b.item_copper(id, &mut copper);
        let layers = copper.iter().fold(LayerSet::EMPTY, |s, c| s.with(c.layer));
        assert_eq!(layers, fixed, "{flash:?}: the via's fixed copper is its fixed rings");
        let table = flash.follows_copper();
        assert_eq!(b.rings.len(), table as usize, "{flash:?}: only a via that follows the copper has a table entry");
        let offers: &[(ItemId, LayerId)] = if table { &[(id, LayerId::inner(2))] } else { &[] };
        assert_eq!(b.rings.offers(zone), offers, "{flash:?}: the zone lists what it offers");
    }
}

#[test]
fn ring_edits_follow_the_op_path_and_equal_a_rebuild() {
    let mut b = board();
    let v = via(&mut b, A, 0.0, Flash::Connected);
    let id = v.id;
    b.vias.push(v);
    b.reindex();
    let rings = |b: &Board| b.rings(&b.vias[0]).fixed;
    assert_eq!(rings(&b), LayerSet::EMPTY, "a connected via with no copper around it has no ring");

    let before = b.clone();
    let own = track(&mut b, A, in1());
    let own_id = own.id;
    let added = b.apply(&BoardOp::Add(Item::Track(own))).unwrap();
    assert_eq!(rings(&b), set(&[in1()]), "an own-net track added by op rings the via on its layer");
    assert!(added.delta.changed.contains(&id), "the via whose rings changed is in the op's delta");
    assert!(BoardDelta::between(&before, &b).changed.contains(&id), "and in the delta between the two boards");
    same_as_rebuild(&b, "after the add");

    let foreign = track(&mut b, B, in2());
    let other = b.apply(&BoardOp::Add(Item::Track(foreign))).unwrap();
    assert!(!other.delta.touches(id), "a foreign track changes no ring, so the via stays out of the delta");
    assert_eq!(rings(&b), set(&[in1()]), "a foreign track rings nothing");

    let removed = b.apply(&BoardOp::Remove(own_id)).unwrap();
    assert_eq!(rings(&b), LayerSet::EMPTY, "removing the track drops its ring");
    assert!(removed.delta.changed.contains(&id), "the remove names the via");
    same_as_rebuild(&b, "after the remove");

    let undone = b.undo(&removed.inverse).unwrap();
    assert_eq!(rings(&b), set(&[in1()]), "undo puts the ring back");
    assert!(undone.delta.changed.contains(&id), "the undo names the via");
    same_as_rebuild(&b, "after the undo");
    b.undo(&undone.inverse).unwrap();
    assert_eq!(rings(&b), LayerSet::EMPTY, "redo drops it again");
    same_as_rebuild(&b, "after the redo");

    let own = track(&mut b, A, in1());
    let stacked = via(&mut b, A, 0.2, Flash::Connected);
    let stacked_id = stacked.id;
    let both = b.apply_all(&[BoardOp::Add(Item::Track(own)), BoardOp::Add(Item::Via(stacked))]).unwrap();
    let all = set(&[LayerId::F_CU, in1(), in2(), LayerId::B_CU]);
    assert_eq!(rings(&b), all, "a touching via of the net rings the via on their common span");
    assert_eq!(b.rings(&b.vias[1]).fixed, all, "and the other way round");
    assert!(both.delta.changed.contains(&id) && both.delta.added.contains(&stacked_id), "one sequence names both vias");
    same_as_rebuild(&b, "after the sequence");
}


/// `board()` with a 10 mm square outline and a zone of `net` on `layer`.
fn outlined(zone_net: NetId, layer: LayerId, area: Area, priority: u16) -> (Board, ItemId) {
    let mut b = board();
    b.outline.push(Contour::rect(P::mm(0.0, -5.0), P::mm(10.0, 5.0), 0));
    let zone = b.alloc_id();
    b.zones.push(Zone {
        id: zone, net: Some(zone_net), name: "pour".into(), layers: set(&[layer]), area, priority,
        settings: ZoneSettings::default(), locked: false, origin: Origin::Authored,
    });
    (b, zone)
}

fn fill_zones(b: &Board) -> Vec<ZoneFill> {
    let rules = resolve_rules(b, None);
    b.zones.iter().map(|z| fill(b, &rules, z.id)).collect()
}

/// The index's piece of `via`'s ring on `layer`: `Copper` or `Offer`.
fn ring_piece(index: &PieceIndex, via: ItemId, layer: LayerId) -> Option<&'static str> {
    index.pieces(via).find(|p| p.layers.has(layer) && matches!(p.kind, PieceKind::Copper(_) | PieceKind::Offer(_))).map(|p| {
        if matches!(p.kind, PieceKind::Copper(_)) { "Copper" } else { "Offer" }
    })
}

#[test]
fn drc_sees_a_ring_only_where_the_via_has_one() {
    // A via of A with a 0.4 mm ring and a 0.15 mm drill; a 0.1 mm B track on In2, `dy` mm off its centre.
    let findings = |flash: Flash, dy: f64| {
        let mut b = board();
        let mut v = via(&mut b, A, 0.0, flash);
        v.pad = mm(0.8);
        b.vias.push(v);
        let mut t = track(&mut b, B, in2());
        t.w = mm(0.1);
        t.path = Path::new(P::mm(-1.0, dy)).line(P::mm(1.0, dy));
        b.tracks.push(t);
        b.reindex();
        let rules = resolve_rules(&b, None);
        let found = drc(&b, &connect(&b, &[]), &rules, &[], None);
        let has = |kind| found.violations.iter().any(|v| v.kind == kind);
        (has(CheckKind::Clearance), has(CheckKind::HoleClearance))
    };
    // The track's edge 0.45 mm from the centre: 0.05 mm from the ring, 0.3 mm from the drill.
    assert_eq!(findings(Flash::All, 0.5), (true, false), "a ring on In2 is too close to the foreign track");
    assert_eq!(findings(Flash::Connected, 0.5), (false, false), "with no ring on In2 only the drill counts, and it is clear");
    // The edge 0.25 mm from the centre: 0.1 mm from the drill.
    assert_eq!(findings(Flash::Connected, 0.3).1, true, "with no ring the drill's hole clearance still holds");
}

#[test]
fn connectivity_is_the_same_in_every_mode() {
    let mut runs = Vec::new();
    for flash in [Flash::All, Flash::Connected] {
        let (mut b, _) = outlined(A, in1(), Area::Board, 0);
        let left = via(&mut b, A, 3.0, flash);
        let right = via(&mut b, A, 7.0, flash);
        let mut top = track(&mut b, A, LayerId::F_CU);
        top.path = Path::new(P::mm(1.0, 0.0)).line(P::mm(3.0, 0.0));
        let mut bottom = track(&mut b, A, LayerId::B_CU);
        bottom.path = Path::new(P::mm(7.0, 0.0)).line(P::mm(9.0, 0.0));
        let ids = [left.id, right.id];
        b.vias.extend([left, right]);
        b.tracks.extend([top, bottom]);
        b.reindex();
        let fills = fill_zones(&b);
        let c = connect(&b, &fills);
        assert!(c.unrouted.is_empty() && c.shorts.is_empty(), "{flash:?}: the zone joins the two vias' tracks: {:?}", c.unrouted);
        if flash == Flash::Connected {
            assert_eq!(b.reached_rings(&fills), vec![(ids[0], in1()), (ids[1], in1())], "the fill reaches both offered rings");
        }
        runs.push((c.clusters.len(), c.unrouted, c.shorts.len()));
    }
    assert_eq!(runs[0], runs[1], "the clusters are the same with every ring and with connected rings");
}

#[test]
fn an_offered_ring_is_copper_where_the_fill_reaches_it() {
    let (mut b, zone) = outlined(A, in1(), Area::Board, 0);
    let v = via(&mut b, A, 5.0, Flash::Connected);
    let id = v.id;
    b.vias.push(v);
    let keepout = b.alloc_id();
    b.keepouts.push(BoardKeepout {
        id: keepout, area: PolySet::rect(BBox::new(P::mm(4.0, -1.0), P::mm(6.0, 1.0))), layers: set(&[in1()]),
        forbid: fab_elec_parts::Forbid::ZONES, reason: "no pour".into(), origin: Origin::Authored, z: None,
    });
    b.reindex();
    let rules = resolve_rules(&b, None);

    let before = fill(&b, &rules, zone);
    assert_eq!(b.rings(&b.vias[0]).offer, set(&[in1()]), "the zone's outline offers the ring");
    assert!(before.layer(in1()).unwrap().reached.is_empty(), "the keepout keeps the fill off the ring: not reached");
    let mut index = PieceIndex::build(&b, std::slice::from_ref(&before));
    assert_eq!(ring_piece(&index, id, in1()), Some("Offer"), "an unreached ring is an offer in the index");

    let removed = b.apply(&BoardOp::Remove(keepout)).unwrap();
    let after = fill_update(&b, &rules, &before, &removed.delta);
    assert_eq!(after.layer(in1()).unwrap().reached, vec![id], "without the keepout the fill reaches the ring");
    assert_eq!(b.reached_rings(std::slice::from_ref(&after)), vec![(id, in1())], "and the board takes the reach");
    let change = index.update(&b, std::slice::from_ref(&after), &removed.delta, &[zone]);
    assert!(change.items.contains(&id), "the flip names the via in the index change");
    assert_eq!(ring_piece(&index, id, in1()), Some("Copper"), "a reached ring is copper in the index");
    let fresh = PieceIndex::build(&b, std::slice::from_ref(&after));
    assert_eq!(index.iter().collect::<Vec<_>>(), fresh.iter().collect::<Vec<_>>(), "edits equal a fresh build after the flip");

    // The fill is a function of the board: refilled over the index that holds the ring as copper, it
    // equals the fill over a fresh index (which holds it as an offer), tile hashes included.
    let plan = FillPlan::full(&b, &rules, zone, &index);
    let tiles = (0..plan.len()).map(|i| plan.tile(&b, &rules, &index, i)).collect();
    assert!(plan.finish(tiles).same(&fill(&b, &rules, zone)), "a reach flip changes no tile, so it never refills");
}

#[test]
fn a_foreign_zone_keeps_clear_of_an_offered_ring() {
    let mut gaps = Vec::new();
    for offered in [true, false] {
        let (mut b, _) = outlined(B, in2(), Area::Board, 0);
        b.zones[0].settings.islands = Islands::Keep;
        let mut v = via(&mut b, A, 5.0, Flash::Connected);
        v.pad = mm(1.2);
        let at = v.at;
        b.vias.push(v);
        if offered {
            // A tiny own-net zone over the ring's edge: it offers the ring and fills nothing.
            let zone = b.alloc_id();
            b.zones.push(Zone {
                id: zone, net: Some(A), name: "spot".into(), layers: set(&[in2()]),
                area: Area::Poly(PolySet::rect(BBox::new(P::mm(5.55, -0.05), P::mm(5.65, 0.05)))), priority: 1,
                settings: ZoneSettings::default(), locked: false, origin: Origin::Authored,
            });
        }
        b.reindex();
        assert_eq!(b.rings(&b.vias[0]).offer.has(in2()), offered, "offered only with the own-net zone");
        let rules = resolve_rules(&b, None);
        let foreign = fill(&b, &rules, b.zones[0].id);
        let gap = foreign.layer(in2()).unwrap().kept().map(|(_, poly)| point_clearance(at, &Shape::Poly(poly.clone()))).min();
        gaps.push(gap.expect("the foreign zone fills"));
    }
    assert!(gaps[0] > mm(0.6), "the foreign fill keeps clear of the offered ring's disc: {} nm from the via", gaps[0]);
    assert!(gaps[1] < mm(0.6), "without the offer it comes up to the drill's clearance, inside the disc: {} nm", gaps[1]);
}

#[test]
#[should_panic(expected = "ring table stale")]
fn a_following_via_pushed_without_reindex_is_refused() {
    let mut b = board();
    let v = via(&mut b, A, 0.0, Flash::Connected);
    b.vias.push(v);
    b.rings(&b.vias[0]);
}
