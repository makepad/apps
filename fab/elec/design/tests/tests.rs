//! The model's tests on the `ledctl` fixtures: one setup each, several properties, each assert
//! naming the property it guards. The live-feedback property (E4) is here: every update form
//! (`connect_update`, `fill_update`, `drc` with a delta folded by `merge_violations`) equals its
//! full form after every op of a mixed sequence, and undoing the sequence restores the board.

use fab_elec_fixtures::{ledctl, ledctl_pair};
use fab_elec_design::trace::{drivers, neighbourhood, pin_named, power_tree, Source};
use fab_elec_design::{project, sysnets, system_erc, Design, Feature, ProjectInto};
use fab_elec_board::ops::{BoardOp, Field, Item};
use fab_elec_board::zone::fill_all;
use fab_elec_board::{connect, connect_update, drc, fill, fill_update, merge_violations, resolve_rules, Anchor, Board, Flash, Origin, Tent, Via, ViaKind, ZoneFill};
use fab_elec_circuit::{erc, CheckKind};
use fab_elec_core::canon::Canon;
use fab_elec_core::{mm, BoardId, CompRef, FeatureRef, ItemId, LayerId, NetRef, P};

/// The height bands (`Placed::body_z`, `BoardKeepout::z`) are additive content: placement takes
/// a part's band from its body, a band changes the hash, and without one the canonical bytes
/// are the fields written before the bands existed (old hashes stand).
#[test]
fn height_bands_are_additive_content() {
    use fab_elec_board::{BoardKeepout, Placed};
    use fab_elec_core::{LayerSet, PolySet};
    let d = ledctl();
    let b = board_of(&d, 0);
    let circuit = &d.unit(BoardId(0)).unwrap().circuit;
    for p in &b.placed {
        let part = &circuit.comps.iter().find(|c| c.id == p.comp).expect("every placement has its component").part;
        assert_eq!(p.body_z, Placed::band_of(part.body.as_ref()), "{}: placement takes the band of its part's body", p.refdes);
        if let Some((lo, hi)) = p.body_z { assert!(0 <= lo && lo <= hi, "{}: a band runs up from the surface", p.refdes); }
    }
    let banded = b.placed.iter().find(|p| p.body_z.is_some()).expect("the fixture has a part with a body").clone();
    let bare = Placed { ref_label: None, value_label: None, legend: None, body_z: None, ..banded.clone() };
    assert_ne!(banded.content_hash(), bare.content_hash(), "a placement's band is content");
    let old = [bare.id.canon_bytes(), bare.comp.canon_bytes(), bare.path.canon_bytes(), bare.refdes.canon_bytes(), bare.footprint.canon_bytes(),
        bare.pad_nets.canon_bytes(), bare.at.canon_bytes(), bare.rot.canon_bytes(), bare.side.canon_bytes(), bare.locked.canon_bytes(), bare.origin.canon_bytes()].concat();
    assert_eq!(bare.canon_bytes(), old, "a placement without a band keeps its old canonical form");

    let keepout = BoardKeepout {
        id: ItemId(5), area: PolySet::rect(fab_elec_core::BBox::new(P::mm(1.0, 1.0), P::mm(3.0, 3.0))), layers: LayerSet::one(LayerId::F_CU),
        forbid: fab_elec_parts::Forbid::FOOTPRINTS, reason: "light pipe".into(), origin: Origin::Authored, z: None,
    };
    let piped = BoardKeepout { z: Some((mm(1.0), mm(9.0))), ..keepout.clone() };
    assert_ne!(piped.content_hash(), keepout.content_hash(), "a keepout's band is content");
    let old = [keepout.id.canon_bytes(), keepout.area.canon_bytes(), keepout.layers.canon_bytes(), keepout.forbid.canon_bytes(),
        keepout.reason.canon_bytes(), keepout.origin.canon_bytes()].concat();
    assert_eq!(keepout.canon_bytes(), old, "a keepout without a band keeps its old canonical form");
}

#[test]
fn board_ops_have_content_only_canon_bytes() {
    use fab_elec_board::{Area, BoardKeepout, Cutout, Graphic, Placed, Text, TuneSide, TuneStyle, TuneTarget, Tuning, ZoneSettings};
    use fab_elec_core::{Angle, BBox, LayerSet, NetId, Path, PolySet, Side, SolverId};
    use std::sync::Arc;

    let d = ledctl();
    let mut b = board_of(&d, 0);
    let hash_before = b.content_hash();
    let via = b.vias[0].clone();
    let id = via.id;
    let p = b.placed[0].clone();
    let other_id = p.id;
    let rebuilt = Via {
        id: via.id,
        net: via.net,
        at: via.at,
        drill: via.drill,
        pad: via.pad,
        span: via.span,
        kind: via.kind,
        tent: via.tent,
        backdrill: via.backdrill,
        locked: via.locked,
        origin: via.origin.clone(),
        flash: Flash::All,
    };
    let fresh = Placed { ref_label: None, value_label: None, legend: None, footprint: Arc::new((*p.footprint).clone()), ..p.clone() };
    assert!(!Arc::ptr_eq(&p.footprint, &fresh.footprint), "equal placements use distinct footprint allocations");
    let item = Item::Via(via.clone());
    let item_ops = |item: Item| [BoardOp::Add(item.clone()), BoardOp::Replace(item.clone()), BoardOp::Insert { item, pos: 0 }];
    for (property, left, right) in [
        ("a cloned via and a field-by-field rebuilt via", item.clone(), Item::Via(rebuilt)),
        ("equal footprints in different Arcs", Item::Placed(p.clone()), Item::Placed(fresh)),
        ("setting an item's own id and origin", item.clone(), item.clone().with_id(item.id()).with_origin(item.origin().clone())),
    ] {
        for ((left, right), op) in item_ops(left).into_iter().zip(item_ops(right)).zip(["Add", "Replace", "Insert"]) {
            assert_eq!(left.canon_bytes(), right.canon_bytes(), "{op}: {property} preserves canonical bytes");
        }
    }

    let area = PolySet::rect(BBox::new(P::ZERO, P::mm(2.0, 3.0)));
    let settings = b.zones[0].settings.clone();
    let changed_settings = ZoneSettings { min_width: settings.min_width + 1, ..settings.clone() };
    for (name, left, right) in [
        ("At", Field::At(P::ZERO), Field::At(P::new(1, 0))),
        ("Rot", Field::Rot(Angle::ZERO), Field::Rot(Angle::QUARTER)),
        ("Side", Field::Side(Side::Top), Field::Side(Side::Bottom)),
        ("Locked", Field::Locked(false), Field::Locked(true)),
        ("Width", Field::Width(mm(0.2)), Field::Width(mm(0.3))),
        ("Widths", Field::Widths(Some(vec![mm(0.2)])), Field::Widths(Some(vec![mm(0.3)]))),
        ("Layer", Field::Layer(LayerId::F_CU), Field::Layer(LayerId::B_CU)),
        ("Layers", Field::Layers(LayerSet::one(LayerId::F_CU)), Field::Layers(LayerSet::of(&[LayerId::F_CU, LayerId::B_CU]))),
        ("Net", Field::Net(Some(via.net)), Field::Net(Some(NetId(via.net.0 + 1)))),
        ("Path", Field::Path(Path::new(P::ZERO).line(P::new(1, 0))), Field::Path(Path::new(P::ZERO).line(P::new(2, 0)))),
        ("Ends", Field::Ends([Anchor::Free, Anchor::Free]), Field::Ends([Anchor::Free, Anchor::Via(id)])),
        ("Drill", Field::Drill(via.drill), Field::Drill(via.drill + 1)),
        ("Pad", Field::Pad(via.pad), Field::Pad(via.pad + 1)),
        ("Span first layer", Field::Span(LayerId::F_CU, LayerId::B_CU), Field::Span(LayerId::inner(1), LayerId::B_CU)),
        ("Span last layer", Field::Span(LayerId::F_CU, LayerId::B_CU), Field::Span(LayerId::F_CU, LayerId::inner(1))),
        ("Priority", Field::Priority(0), Field::Priority(1)),
        ("ZoneSettings", Field::ZoneSettings(settings), Field::ZoneSettings(changed_settings)),
        ("Area", Field::Area(Area::Board), Field::Area(Area::Poly(area.clone()))),
        ("Text", Field::Text("a".into()), Field::Text("b".into())),
        ("Origin", Field::Origin(Origin::Authored), Field::Origin(Origin::Solved { by: SolverId(0) })),
    ] {
        let same_field_other_item = BoardOp::SetField { item: other_id, field: left.clone() };
        let left = BoardOp::SetField { item: id, field: left };
        let right = BoardOp::SetField { item: id, field: right };
        assert_ne!(left.canon_bytes(), right.canon_bytes(), "SetField {name}: changing its payload changes canonical bytes");
        assert_ne!(left.canon_bytes(), same_field_other_item.canon_bytes(), "SetField {name}: the target item id is content");
    }
    assert_eq!(Field::Span(LayerId::F_CU, LayerId::B_CU).canon_bytes(), [vec![13], LayerId::F_CU.canon_bytes(), LayerId::B_CU.canon_bytes()].concat(), "Span writes both layers in order after its tag");

    let changed_item = Item::Via(Via { pad: via.pad + 1, ..via.clone() });
    let mut stackup = b.stackup.clone();
    stackup.thickness += 1;
    let mut outline = b.outline.clone();
    outline[0].path.start.x += 1;
    let mut rules = b.rules.clone();
    rules.base.clearance = Some(rules.base.clearance.unwrap_or(0) + 1);
    let mut nets = b.nets.clone();
    nets[0].name.push_str(" renamed");
    let mut classes = b.classes.clone();
    classes[0].name.push_str(" renamed");
    for (property, left, right) in [
        ("Add item", BoardOp::Add(item.clone()), BoardOp::Add(changed_item.clone())),
        ("Insert position", BoardOp::Insert { item: item.clone(), pos: 0 }, BoardOp::Insert { item: item.clone(), pos: 1 }),
        ("Insert item", BoardOp::Insert { item: item.clone(), pos: 0 }, BoardOp::Insert { item: changed_item.clone(), pos: 0 }),
        ("Remove id", BoardOp::Remove(id), BoardOp::Remove(other_id)),
        ("Replace item", BoardOp::Replace(item.clone()), BoardOp::Replace(changed_item)),
        ("Move displacement", BoardOp::Move { item: id, by: P::ZERO }, BoardOp::Move { item: id, by: P::new(1, 0) }),
        ("Move id", BoardOp::Move { item: id, by: P::new(1, 0) }, BoardOp::Move { item: other_id, by: P::new(1, 0) }),
        ("SetField field", BoardOp::SetField { item: id, field: Field::Locked(false) }, BoardOp::SetField { item: id, field: Field::Locked(true) }),
        ("Detach id", BoardOp::Detach(id), BoardOp::Detach(other_id)),
        ("SetStackup thickness", BoardOp::SetStackup(b.stackup.clone()), BoardOp::SetStackup(stackup)),
        ("SetOutline point", BoardOp::SetOutline(b.outline.clone()), BoardOp::SetOutline(outline)),
        ("SetRules clearance", BoardOp::SetRules(b.rules.clone()), BoardOp::SetRules(rules)),
        ("SetNets net name", BoardOp::SetNets { nets: b.nets.clone(), classes: b.classes.clone(), pairs: b.pairs.clone() }, BoardOp::SetNets { nets, classes: b.classes.clone(), pairs: b.pairs.clone() }),
        ("SetNets class name", BoardOp::SetNets { nets: b.nets.clone(), classes: b.classes.clone(), pairs: b.pairs.clone() }, BoardOp::SetNets { nets: b.nets.clone(), classes, pairs: b.pairs.clone() }),
    ] {
        assert_ne!(left.canon_bytes(), right.canon_bytes(), "{property}: changing one payload field changes canonical bytes");
    }
    let [add, replace, insert] = item_ops(item.clone()).map(|op| op.canon_bytes());
    assert_ne!(add[0], replace[0], "Add and Replace with the same item have distinct tags");
    assert_ne!(add[0], insert[0], "Add and Insert with the same item have distinct tags");
    assert_ne!(replace[0], insert[0], "Replace and Insert with the same item have distinct tags");

    // Include item kinds absent from the fixture, and both forms of the additive height bands.
    let keepout = BoardKeepout {
        id, area, layers: LayerSet::one(LayerId::F_CU), forbid: fab_elec_parts::Forbid::FOOTPRINTS,
        reason: "clearance".into(), origin: Origin::Authored, z: None,
    };
    for (tag, item) in [
        (0, Item::Placed(p.clone())),
        (0, Item::Placed(Placed { ref_label: None, value_label: None, legend: None, body_z: None, ..p.clone() })),
        (0, Item::Placed(Placed { ref_label: None, value_label: None, legend: None, body_z: Some((0, mm(1.0))), ..p.clone() })),
        (1, Item::Track(b.tracks[0].clone())),
        (2, item.clone()),
        (3, Item::Zone(b.zones[0].clone())),
        (4, Item::Keepout(keepout.clone())),
        (4, Item::Keepout(BoardKeepout { z: Some((0, mm(1.0))), ..keepout })),
        (5, Item::Hole(b.holes[0].clone())),
        (6, Item::Cutout(Cutout { id, name: "slot".into(), contour: b.outline[0].clone(), origin: Origin::Authored })),
        (7, Item::Graphic(Graphic { id, layer: LayerId::F_SILK, gfx: p.footprint.graphics[0].1.clone(), locked: false, origin: Origin::Authored })),
        (8, Item::Text(Text {
            id, layer: LayerId::F_SILK, text: "label".into(), at: P::ZERO, rot: Angle::ZERO,
            size: mm(1.0), thick: mm(0.1), mirrored: false, locked: false, origin: Origin::Authored,
        })),
        (9, Item::Tuning(Tuning {
            id, net: via.net, pair: None, host: vec![b.tracks[0].id], target: TuneTarget::Len(mm(10.0)),
            side: TuneSide::Both, amp: (mm(0.5), mm(1.0)), pitch: mm(1.0), style: TuneStyle::Organic,
            copper: vec![], origin: Origin::Authored,
        })),
    ] {
        let inner = match &item {
            Item::Placed(x) => x.canon_bytes(),
            Item::Track(x) => x.canon_bytes(),
            Item::Via(x) => x.canon_bytes(),
            Item::Zone(x) => x.canon_bytes(),
            Item::Keepout(x) => x.canon_bytes(),
            Item::Hole(x) => x.canon_bytes(),
            Item::Cutout(x) => x.canon_bytes(),
            Item::Graphic(x) => x.canon_bytes(),
            Item::Text(x) => x.canon_bytes(),
            Item::Tuning(x) => x.canon_bytes(),
        };
        let bytes = item.canon_bytes();
        assert_eq!(bytes, [vec![tag], inner].concat(), "{:?}: Item writes only its tag and the inner item's existing canonical bytes", item.kind());
        assert_eq!(BoardOp::Add(item.clone()).canon_bytes(), [vec![0], bytes.clone()].concat(), "{:?}: Add writes its tag then the item without a record length", item.kind());
        assert_eq!(BoardOp::Replace(item.clone()).canon_bytes(), [vec![3], bytes.clone()].concat(), "{:?}: Replace writes its tag then the item without a record length", item.kind());
        assert_eq!(BoardOp::Insert { item: item.clone(), pos: 7 }.canon_bytes(), [vec![1], 7u32.canon_bytes(), bytes].concat(), "{:?}: Insert writes its tag and position before the item without a record length", item.kind());
    }

    let before = BoardOp::Add(b.item(id).expect("the fixture via exists")).canon_bytes();
    let generation = b.generation;
    let next_item = b.next_item;
    let added_id = b.alloc_id();
    for op in [BoardOp::Add(item.with_id(added_id).with_origin(Origin::Authored)), BoardOp::Remove(added_id)] {
        b.apply(&op).expect("adding and removing an unrelated authored via applies");
        b.reindex();
        let rebuilt = BoardOp::Add(b.item(id).expect("the original fixture via remains"));
        assert_eq!(rebuilt.canon_bytes(), before, "board list and index changes, generation and id allocation do not enter an op's bytes");
    }
    assert!(b.generation > generation && b.next_item > next_item, "the derived-data check advances generation and the id counter");
    assert_eq!(b.content_hash(), hash_before, "building and encoding ops does not mutate board content or its existing hash");
}

fn board_of(d: &Design, unit: u16) -> Board {
    (**d.unit(BoardId(unit)).unwrap().board.as_ref().unwrap()).clone()
}

fn comp(d: &Design, unit: u16, path: &str) -> CompRef {
    let c = d.unit(BoardId(unit)).unwrap().circuit.comp(path).unwrap_or_else(|| panic!("no component {path}"));
    CompRef { board: BoardId(unit), comp: c.id }
}

#[test]
fn ledctl_is_consistent_and_clean() {
    use fab_elec_parts::PartKind;
    let d = ledctl();
    assert_eq!(d.check_consistent(), Vec::new(), "the fixture is internally consistent");
    let u = &d.units[0];
    assert_eq!((u.circuit.comps.len(), u.circuit.nets.len()), (30, 26), "30 components and 26 nets, as the document");
    assert_eq!(erc(&u.circuit), Vec::new(), "the circuit is ERC clean");
    assert_eq!(d.content_hash(), ledctl().content_hash(), "the canonical hash is deterministic");
    let shared = d.clone();
    assert_eq!(shared.content_hash(), d.content_hash(), "Arc sharing does not change the hash");
    let kind_of = |path: &str| u.circuit.comp(path).map(|c| c.part.kind.clone());
    assert_eq!((kind_of("reg.u"), kind_of("lamps[0].r")), (Some(PartKind::Regulator), Some(PartKind::Resistor)), "the fixture's parts carry their kinds");
    for k in [PartKind::Mcu, PartKind::Ic, PartKind::Connector, PartKind::Other("antenna".into())] {
        assert_eq!(PartKind::of_name(k.name()), k, "a part kind's name gives it back");
    }

    let b = board_of(&d, 0);
    let rules = resolve_rules(&b, None);
    let fills = fill_all(&b, &rules);
    // Tiles join exactly (a tile-wise reader relies on it): the pour crosses tile seams, and its
    // set has no edge on a lattice line with copper on both sides (a slit), nor copper or a gap
    // under 1 um beside one (a sliver).
    let side = fills[0].lattice.side();
    for layer in &fills[0].layers {
        let set = layer.set();
        let name = layer.layer.name();
        assert!(!set.is_empty(), "the GND pour fills on {name}");
        let cuts: usize = layer.tiles.iter().flat_map(|tile| tile.cuts.iter()).map(Vec::len).sum();
        assert!(cuts > 0, "the GND pour crosses tile seams on {name}");
        for ring in set.rings() {
            for (&a, &b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
                let vertical = a.x == b.x && a.x.rem_euclid(side) == 0;
                if a == b || !(vertical || (a.y == b.y && a.y.rem_euclid(side) == 0)) {
                    continue;
                }
                let mid = P::new(a.x + (b.x - a.x) / 2, a.y + (b.y - a.y) / 2);
                let across = |d: i32| {
                    let (l, r) = if vertical {
                        (P::new(mid.x - d, mid.y), P::new(mid.x + d, mid.y))
                    } else {
                        (P::new(mid.x, mid.y - d), P::new(mid.x, mid.y + d))
                    };
                    [set.contains(l), set.contains(r)]
                };
                let near = across(1);
                assert!(!(near[0] && near[1]), "no slit: copper on both sides of a seam edge at {mid:?} on {name}");
                if (a.x - b.x).abs() + (a.y - b.y).abs() >= 10_000 {
                    assert_eq!(near, across(1_000), "no sliver beside a seam edge at {mid:?} on {name}");
                }
            }
        }
    }
    let conn = connect(&b, &fills);
    assert!(conn.shorts.is_empty(), "no shorts: {:?}", conn.shorts);
    assert!(!conn.ratsnest.is_empty(), "the fixture is not fully routed: it has a ratsnest");
    let v = drc(&b, &conn, &rules, &fills, None).violations;
    // The generators place reference labels without the silk rule, so some cross a neighbour's pad
    // openings (the LED column's labels): those stay warnings until labels are placed clear of openings.
    let silk: Vec<_> = v.iter().filter(|v| v.kind == CheckKind::SilkOverPad).collect();
    assert!(
        silk.iter().all(|v| v.severity == fab_elec_core::Severity::Warning && v.message.starts_with("reference ")),
        "the only silk findings are warnings for reference labels over pad openings: {silk:#?}"
    );
    let bad: Vec<_> = v.iter().filter(|v| !matches!(v.kind, CheckKind::Unconnected | CheckKind::SilkOverPad)).collect();
    assert!(bad.is_empty(), "DRC reports nothing but unrouted connections and label overlaps: {bad:#?}");

    // the LED tracks are anchored and carry the A nets
    let r = b.placed_of(comp(&d, 0, "lamps[0].r").comp).unwrap();
    let t = b.tracks.iter().find(|t| t.ends[0].item() == Some(r.id)).expect("a track starts on lamps[0].r");
    assert!(matches!(t.ends[1], Anchor::Pad { .. }), "both ends of a fixture track are anchored");
    assert_eq!(b.nets[t.net.ix()].name, "lamps[0].d.A", "the track carries the A net");

    // trace queries on the single board
    let led_k = pin_named(&d, comp(&d, 0, "lamps[0].d"), "K").unwrap();
    let pa0 = pin_named(&d, comp(&d, 0, "mcu.u"), "PA0").unwrap();
    assert!(drivers(&d, led_k).iter().any(|t| t.end == pa0), "LED0's cathode is driven by PA0");
    let rails = power_tree(&d, comp(&d, 0, "mcu.u"));
    let vdd = rails.iter().find(|r| u.circuit.nets[r.rail.net.ix()].name == "3V3").expect("the MCU has a 3V3 rail");
    let vbus = pin_named(&d, comp(&d, 0, "usb.j"), "VBUS").unwrap();
    assert_eq!(vdd.source, Source::Pin(vbus), "3V3 climbs through the regulator to the connector's VBUS");
    let near = neighbourhood(&d, &[comp(&d, 0, "lamps[0].d")], 1);
    assert!(near.comps.iter().any(|(c, k)| *c == comp(&d, 0, "lamps[0].r") && *k == 1), "the LED's resistor is one hop away");
}

/// Full engine results for a board.
fn full(b: &Board) -> (fab_elec_board::RuleMatrix, Vec<ZoneFill>, fab_elec_board::Connectivity, Vec<fab_elec_circuit::Violation>) {
    let rules = resolve_rules(b, None);
    let fills = fill_all(b, &rules);
    let conn = connect(b, &fills);
    let v = drc(b, &conn, &rules, &fills, None).violations;
    (rules, fills, conn, v)
}

#[test]
fn ops_undo_and_update_forms_equal_full_forms() {
    let d = ledctl();
    let mut b = board_of(&d, 0);
    let start = b.clone();
    let r4 = b.placed_of(comp(&d, 0, "lamps[0].r").comp).unwrap().id;
    let led_track = b.tracks.iter().find(|t| t.ends[0].item() == Some(r4)).unwrap().id;
    let gnd = b.net("GND").unwrap();
    let via_id = b.alloc_id();
    let mut rules_wide = b.rules.clone();
    rules_wide.base.clearance = Some(mm(0.3));
    let ops = vec![
        BoardOp::Move { item: r4, by: P::new(mm(0.3), mm(-0.2)) },
        BoardOp::SetField { item: led_track, field: Field::Width(mm(0.3)) },
        BoardOp::Add(Item::Via(Via {
            id: via_id,
            net: gnd,
            at: P::new(mm(40.0), mm(16.0)),
            drill: mm(0.3),
            pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through,
            tent: Tent::Both,
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        })),
        BoardOp::Remove(led_track),
        BoardOp::SetRules(rules_wide),
    ];

    let (mut rules, mut fills, mut conn, mut viol) = full(&b);
    let mut undo = Vec::new();
    for op in &ops {
        let applied = b.apply(op).unwrap_or_else(|e| panic!("{op:?} applies: {e}"));
        let delta = &applied.delta;
        assert!(!delta.is_empty(), "{op:?} reports a change");
        if delta.global {
            rules = resolve_rules(&b, None);
        }
        let new_fills: Vec<ZoneFill> = fills
            .iter()
            .map(|f| fill_update(&b, &rules, f, delta))
            .collect();
        for f in &new_fills {
            assert_eq!(*f, fill(&b, &rules, f.zone), "fill_update equals fill after {op:?}");
        }
        fills = new_fills;
        let new_conn = connect_update(&conn, &b, &fills, delta);
        assert_eq!(new_conn, connect(&b, &fills), "connect_update equals connect after {op:?}");
        conn = new_conn;
        let merged = merge_violations(&viol, &drc(&b, &conn, &rules, &fills, Some(delta)));
        assert_eq!(merged, drc(&b, &conn, &rules, &fills, None).violations, "the merged DRC update equals full DRC after {op:?}");
        viol = merged;
        undo.push(applied.inverse);
    }
    assert!(viol.iter().any(|v| v.kind == CheckKind::Clearance), "the wider clearance rule shows up as clearance findings");

    let missing = b.clone();
    assert!(b.apply(&BoardOp::Remove(ItemId(u32::MAX))).is_err(), "an op on a missing id fails");
    assert_eq!(b, missing, "a failed op changes nothing");

    for inv in undo.iter().rev() {
        b.undo(inv).expect("the inverse applies");
    }
    assert_eq!(b.content_hash(), start.content_hash(), "undoing every op restores the canonical board");
    assert_eq!(b.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), start.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), "undo restores list order");
    assert!(b.next_item >= start.next_item, "next_item never goes back");
}

#[test]
fn two_boards_mate_into_system_nets() {
    let d = ledctl_pair();
    assert_eq!(d.check_consistent(), Vec::new(), "the pair is consistent");
    let link = &d.links[0];
    assert_eq!(d.link_pairs(link).len(), 5, "the mate pairs the five header pins by name");
    let nets = sysnets(&d);
    let probe_3v3 = NetRef { board: BoardId(1), net: d.units[1].circuit.net("3V3").unwrap().id };
    let main_3v3 = NetRef { board: BoardId(0), net: d.units[0].circuit.net("3V3").unwrap().id };
    let joined = nets.iter().find(|s| s.members.contains(&probe_3v3)).expect("the probe's 3V3 is in a system net");
    assert!(joined.members.contains(&main_3v3), "the mate joins the two 3V3 rails into one system net");
    let gnd_nets = nets.iter().filter(|s| s.members.iter().any(|m| d.unit(m.board).unwrap().circuit.nets[m.net.ix()].name == "GND")).count();
    assert_eq!(gnd_nets, 1, "the two grounds are one system net");
    assert_eq!(system_erc(&d), Vec::new(), "system ERC is clean: pins match and the probe's rail has a source through the mate");

    let j = comp(&d, 1, "j");
    match project(&d, &FeatureRef::Part(j), ProjectInto::Unit(BoardId(0))) {
        Ok(Feature::Point { at, z, .. }) => {
            assert_eq!(at, P::new(mm(20.0), mm(10.0)), "the probe's header projects through its mount into the main board's frame");
            assert_eq!(z, -mm(8.0), "and sits 8 mm below it");
        }
        other => panic!("a part projects to a point: {other:?}"),
    }
}

#[test]
fn engines_update_equals_full() {
    use fab_elec_design::engines;
    use fab_elec_design::ops::DesignApplied;
    use fab_elec_design::DesignOp;
    use fab_elec_board::BoardDelta;
    use std::sync::Arc;

    let mut d = ledctl();
    let mut res = engines::full(&d);
    let resistor = comp(&d, 0, "lamps[0].r").comp;
    let b = Arc::make_mut(d.units[0].board.as_mut().unwrap());
    let r4 = b.placed_of(resistor).unwrap().id;
    let led_track = b.tracks.iter().find(|t| t.ends[0].item() == Some(r4)).unwrap().id;
    let gnd = b.net("GND").unwrap();
    let via_id = b.alloc_id();
    let mut rules_wide = b.rules.clone();
    rules_wide.base.clearance = Some(mm(0.3));
    let ops = [
        ("move lamps[0].r", BoardOp::Move { item: r4, by: P::new(mm(0.3), mm(-0.2)) }),
        ("widen the LED track", BoardOp::SetField { item: led_track, field: Field::Width(mm(0.3)) }),
        ("add a GND via", BoardOp::Add(Item::Via(Via {
            id: via_id,
            net: gnd,
            at: P::new(mm(40.0), mm(16.0)),
            drill: mm(0.3),
            pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through,
            tent: Tent::Both,
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        }))),
        ("remove the LED track", BoardOp::Remove(led_track)),
        ("set wider rules", BoardOp::SetRules(rules_wide)),
    ];
    for (name, op) in &ops {
        let applied = d.apply(&DesignOp::Board { unit: BoardId(0), op: op.clone() }, false).unwrap_or_else(|e| panic!("{name} applies: {e:?}"));
        let DesignApplied::Board { applied, .. } = applied else { panic!("{name} returns a board delta") };
        let delta = applied.delta;
        res = engines::update(&res, &d, &[(BoardId(0), delta)]);
        assert_eq!(res, engines::full(&d), "engine update equals full after {name}");
    }

    // A replacement worker job covers both edits since the last published result.
    let mut d = ledctl();
    let mut res = engines::full(&d);
    let mut delta = BoardDelta::default();
    for (name, op) in &ops[..2] {
        let applied = d.apply(&DesignOp::Board { unit: BoardId(0), op: op.clone() }, false).unwrap_or_else(|e| panic!("{name} applies: {e:?}"));
        let DesignApplied::Board { applied, .. } = applied else { panic!("{name} returns a board delta") };
        engines::merge_delta(&mut delta, &applied.delta);
    }
    res = engines::update(&res, &d, &[(BoardId(0), delta)]);
    let expected = engines::full(&d);
    assert_eq!(res, expected, "merged move and width deltas equal a full run");

    let findings = engines::violations(&res);
    assert!(findings.windows(2).all(|w| w[0].id < w[1].id), "Checks findings are sorted by id without duplicates");
    assert!(res.boards.iter().flat_map(|b| b.drc.iter()).all(|v| findings.contains(v)), "Checks contains every board DRC finding");
    let expected_findings: Vec<_> = expected.boards.iter().flat_map(|b| b.drc.iter()).chain(expected.erc.iter()).chain(expected.system.iter()).collect();
    assert!(findings.iter().all(|v| expected_findings.contains(&v)), "Checks contains nothing outside the full engine findings");
}

#[test]
fn board_delta_between_drives_exact_updates() {
    use fab_elec_design::engines;
    use fab_elec_design::ops::DesignApplied;
    use fab_elec_design::DesignOp;
    use fab_elec_board::BoardDelta;
    use fab_elec_core::BBox;
    use std::sync::Arc;

    let mut d = ledctl();
    let resistor = comp(&d, 0, "lamps[0].r").comp;
    let b = Arc::make_mut(d.units[0].board.as_mut().unwrap());
    let unchanged = BoardDelta::between(b, b);
    assert!(unchanged.is_empty(), "an unchanged fixture board has an empty delta");
    assert_eq!(unchanged.bbox, BBox::EMPTY, "an unchanged fixture board has an empty delta box");

    let r4 = b.placed_of(resistor).unwrap().id;
    let led_track = b.tracks.iter().find(|t| t.ends[0].item() == Some(r4)).unwrap().id;
    let gnd = b.net("GND").unwrap();
    let via_id = b.alloc_id();
    let mut rules_wide = b.rules.clone();
    rules_wide.base.clearance = Some(mm(0.3));
    let ops = [
        ("move lamps[0].r", BoardOp::Move { item: r4, by: P::new(mm(0.3), mm(-0.2)) }),
        ("widen the LED track", BoardOp::SetField { item: led_track, field: Field::Width(mm(0.3)) }),
        ("add a GND via", BoardOp::Add(Item::Via(Via {
            id: via_id,
            net: gnd,
            at: P::new(mm(40.0), mm(16.0)),
            drill: mm(0.3),
            pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through,
            tent: Tent::Both,
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        }))),
        ("remove the LED track", BoardOp::Remove(led_track)),
        ("set wider rules", BoardOp::SetRules(rules_wide)),
    ];
    for (name, op) in &ops {
        let mut d = ledctl();
        let before = d.clone();
        let res = engines::full(&before);
        let applied = d.apply(&DesignOp::Board { unit: BoardId(0), op: op.clone() }, false).unwrap_or_else(|e| panic!("{name} applies: {e:?}"));
        let DesignApplied::Board { applied, .. } = applied else { panic!("{name} returns a board delta") };
        let op_delta = applied.delta;
        let between = BoardDelta::between(before.units[0].board.as_ref().unwrap(), d.units[0].board.as_ref().unwrap());
        for (kind, ids, touched) in [
            ("added", &between.added, &op_delta.added),
            ("removed", &between.removed, &op_delta.removed),
            ("changed", &between.changed, &op_delta.changed),
        ] {
            for id in ids {
                assert!(touched.contains(id), "{name}: between's {kind} id {id:?} is in the op delta's same list");
            }
        }
        assert_eq!(between.global, op_delta.global, "{name}: between and the op delta agree on global changes");
        assert_eq!(engines::update(&res, &d, &[(BoardId(0), between)]), engines::full(&d), "engine update with between equals full after {name}");
    }

    let mut d = ledctl();
    let before = d.clone();
    let res = engines::full(&before);
    for (name, op) in &ops[..2] {
        d.apply(&DesignOp::Board { unit: BoardId(0), op: op.clone() }, false).unwrap_or_else(|e| panic!("{name} applies: {e:?}"));
    }
    let between = BoardDelta::between(before.units[0].board.as_ref().unwrap(), d.units[0].board.as_ref().unwrap());
    assert_eq!(engines::update(&res, &d, &[(BoardId(0), between)]), engines::full(&d), "engine update with between equals full after move lamps[0].r and widen the LED track");
}

#[test]
fn pad_openings_follow_pad_and_rules() {
    use fab_elec_core::{PadIx, Shape, Side};
    use fab_elec_parts::PadKind;
    use std::sync::Arc;

    let d = ledctl();
    let mut b = board_of(&d, 0);
    let p = b.placed_of(comp(&d, 0, "lamps[0].r").comp).expect("the fixture resistor is placed");
    let placed = p.id;
    let pad = PadIx(0);
    assert_eq!(p.side, Side::Top, "the surface pad's part is on the top side");
    assert_eq!(p.footprint.pads[pad.ix()].kind, PadKind::Smd, "the resistor pad is a surface pad");
    let copper = p.pad_shape(pad, LayerId::F_CU, b.copper_count()).expect("the surface pad has front copper").bbox();
    assert_eq!(b.mask_opening(placed, pad, Side::Top).map(|s| s.bbox()), Some(copper.inflate(mm(0.05))), "the standard mask rule grows the surface pad by 0.05 mm on each side");
    assert!(b.mask_opening(placed, pad, Side::Bottom).is_none(), "a top surface pad has no bottom mask opening");
    assert_eq!(b.paste_opening(placed, pad, Side::Top).map(|s| s.bbox()), Some(copper), "standard paste margin and ratio preserve the copper pad's box");

    b.rules.base.mask_margin = None;
    b.rules.base.paste_margin = None;
    b.rules.base.paste_ratio_ppm = None;
    assert_eq!(b.mask_opening(placed, pad, Side::Top).map(|s| s.bbox()), Some(copper.inflate(mm(0.05))), "an unset board mask margin falls back to the standard rule");
    assert_eq!(b.paste_opening(placed, pad, Side::Top).map(|s| s.bbox()), Some(copper), "unset board paste margin and ratio fall back to the standard rule");

    b.rules.base.mask_margin = Some(mm(0.1));
    assert_eq!(b.mask_opening(placed, pad, Side::Top).map(|s| s.bbox()), Some(copper.inflate(mm(0.1))), "the board mask rule grows the surface pad by 0.1 mm on each side");

    let p = b.placed.iter_mut().find(|p| p.id == placed).expect("the surface pad's placement still exists");
    Arc::make_mut(&mut p.footprint).pads[pad.ix()].mask_margin = Some(0);
    assert_eq!(b.mask_opening(placed, pad, Side::Top).map(|s| s.bbox()), Some(copper), "the pad's explicit zero mask margin overrides the board rule");

    let header = b.placed_of(comp(&d, 0, "mcu.dbg").comp).expect("the fixture header is placed");
    assert_eq!(header.footprint.pads[pad.ix()].kind, PadKind::Thru, "the header pad is a plated through-hole pad");
    for side in [Side::Top, Side::Bottom] {
        assert!(b.mask_opening(header.id, pad, side).is_some(), "a through-hole pad has a mask opening on {side:?}");
        assert!(b.paste_opening(header.id, pad, side).is_none(), "a through-hole pad has no paste opening on {side:?}");
        assert!(b.mask_opening(placed, PadIx(u16::MAX), side).is_none(), "an unknown pad index has no mask opening on {side:?}");
        assert!(b.paste_opening(placed, PadIx(u16::MAX), side).is_none(), "an unknown pad index has no paste opening on {side:?}");
        assert!(b.mask_opening(ItemId(u32::MAX), pad, side).is_none(), "an unknown placement has no mask opening on {side:?}");
        assert!(b.paste_opening(ItemId(u32::MAX), pad, side).is_none(), "an unknown placement has no paste opening on {side:?}");
        assert!(b.via_mask_opening(ItemId(u32::MAX), side).is_none(), "an unknown via has no mask opening on {side:?}");
    }

    let via = b.vias.iter().find(|v| v.span == (LayerId::F_CU, LayerId::B_CU)).expect("the fixture has a through via").clone();
    let open = |b: &Board, tent: Tent, side: Side| {
        let mut b = b.clone();
        b.vias.iter_mut().find(|v| v.id == via.id).unwrap().tent = tent;
        b.via_mask_opening(via.id, side)
    };
    let grown = Shape::Circle { c: via.at, r: via.pad / 2 + mm(0.1) };
    // The fixture's document writes no `tent`, so its vias are tented by default.
    assert_eq!(via.tent, Tent::Both, "a via its document writes without `tent` is tented");
    for side in [Side::Top, Side::Bottom] {
        assert_eq!(b.via_mask_opening(via.id, side), None, "by default the mask covers a via on {side:?}");
        assert_eq!(open(&b, Tent::Both, side), None, "a via tented on both sides has no mask opening on {side:?}");
        assert_eq!(open(&b, Tent::None, side), Some(grown.clone()),
            "an open via (`tent: @none`, a test point) keeps its opening: its pad grown by the board mask rule on {side:?}");
    }
    assert_eq!((open(&b, Tent::Front, Side::Top), open(&b, Tent::Front, Side::Bottom)), (None, Some(grown.clone())), "a via tented on the front opens only on the back");
    assert_eq!((open(&b, Tent::Back, Side::Top), open(&b, Tent::Back, Side::Bottom)), (Some(grown), None), "a via tented on the back opens only on the front");
    b.rules.base.mask_margin = None;
    assert_eq!(open(&b, Tent::None, Side::Top), Some(Shape::Circle { c: via.at, r: via.pad / 2 + mm(0.05) }), "an unset board mask margin falls back to the standard rule for vias");
}

#[test]
fn connect_held_keeps_fill_joins_until_the_refill() {
    use fab_elec_board::{connect_held, ConnItem, Connectivity, Track};
    use fab_elec_core::Path;

    let d = ledctl();
    let b = board_of(&d, 0);
    let rules = resolve_rules(&b, None);
    let fills = fill_all(&b, &rules);
    let before = connect(&b, &fills);
    assert_eq!(connect_held(&b, &fills, &before, &[]), before, "held connectivity with no stale zone equals full connectivity");
    assert!(before.fill_joins().next().is_some(), "full connectivity records the joins made through fill islands");

    let gnd = b.net("GND").expect("the fixture has GND");
    let zone = b.zones.iter().find(|z| z.net == Some(gnd)).expect("the fixture has a GND zone").id;
    let held_same = connect_held(&b, &fills, &before, &[zone]);
    assert_eq!(held_same.held, vec![zone], "the held result names the zone whose joins it kept");
    assert_eq!(Connectivity { held: Vec::new(), ..held_same }, before, "a stale zone over an unchanged board keeps every join");
    let via_id = b.vias.iter().find(|v| v.net == gnd).expect("the fixture has a GND via").id;
    let via = ConnItem::Via(via_id);
    let before_cluster = before.clusters.iter().find(|c| c.items.contains(&via)).expect("the GND via has a cluster before the move");
    let islands: Vec<_> = before_cluster.items.iter().copied().filter(|item| matches!(item, ConnItem::Zone { item, .. } if *item == zone)).collect();
    assert!(!islands.is_empty(), "the GND via starts connected to its GND fill islands");

    let mut b1 = b.clone();
    b1.apply(&BoardOp::Move { item: via_id, by: P::mm(0.2, 0.0) }).expect("the GND via moves within the fill");
    let held_move = connect_held(&b1, &fills, &before, &[zone]);
    let moved_cluster = held_move.clusters.iter().find(|c| c.items.contains(&via)).expect("the moved GND via has a cluster");
    assert!(islands.iter().all(|island| moved_cluster.items.contains(island)), "moving a via keeps its previous GND fill joins until refill: no split through a stale fill");

    let fills1 = fill_all(&b1, &rules);
    let refilled1 = connect(&b1, &fills1);
    let mut b2 = b1.clone();
    let track_id = b2.alloc_id();
    // This front-copper area is clear of the fixture's parts, tracks, vias and mounting holes.
    let track = Track {
        id: track_id,
        net: b2.net("LED0").expect("the fixture has the LED0 signal net"),
        layer: LayerId::F_CU,
        w: mm(0.25),
        widths: None,
        path: Path::new(P::mm(40.0, 16.0)).line(P::mm(42.0, 16.0)),
        ends: [Anchor::Free, Anchor::Free],
        locked: false,
        origin: Origin::Authored,
    };
    b2.apply(&BoardOp::Add(Item::Track(track))).expect("a signal track is added over the old GND fill");
    // A short has one witness per cluster, which need not itself name every shorted item.
    let has_track_short = |conn: &Connectivity| {
        conn.shorts.iter().any(|short| conn.clusters[short.cluster as usize].items.iter().any(|item| item.item() == track_id))
    };
    assert!(has_track_short(&connect(&b2, &fills1)), "plain connectivity against the stale GND fill reports a short on the added signal track");
    let held_add = connect_held(&b2, &fills1, &refilled1, &[zone]);
    assert!(!has_track_short(&held_add), "an added track joins no stale fill island, so no fill-induced short shows while held");
    assert!(!held_add.fill_joins().any(|j| j.a.item() == track_id || j.b.item() == track_id), "the held joins never name the added track");
    let fills2 = fill_all(&b2, &rules);
    assert!(!has_track_short(&connect(&b2, &fills2)), "the refill carves clearance around the added signal track: no short");

    let mut b3 = b.clone();
    b3.apply(&BoardOp::Remove(zone)).expect("the GND zone is removed");
    let held_removed = connect_held(&b3, &fills, &before, &[zone]);
    let names_zone = |c: &ConnItem| matches!(c, ConnItem::Zone { item, .. } if *item == zone);
    assert!(!held_removed.joins.iter().any(|j| names_zone(&j.a) || names_zone(&j.b)), "a removed zone keeps no joins");
    assert!(!held_removed.clusters.iter().any(|c| c.items.iter().any(names_zone)), "a removed zone's islands do not come back");
}

/// Board's id index (`Board::index`) stays exact through the op path (apply, insert, replace,
/// undo, a rolled-back batch), `locate` stays correct after a direct push that bypasses it, and
/// the index is never content (equality and the canonical hash ignore it).
#[test]
fn item_index_agrees_with_a_scan() {
    use fab_elec_board::{Hole, ItemIndex, ItemKind, Track};
    fn scan(b: &Board) -> Vec<(ItemId, ItemKind, usize)> {
        let mut v = Vec::new();
        macro_rules! of {
            ($($list:ident => $k:ident),*) => {$(
                for (i, x) in b.$list.iter().enumerate() { v.push((x.id, ItemKind::$k, i)); }
            )*};
        }
        of!(placed => Placed, tracks => Track, vias => Via, zones => Zone, keepouts => Keepout, holes => Hole, cutouts => Cutout, graphics => Graphic, texts => Text, tunings => Tuning);
        v
    }
    let agree = |b: &Board, step: &str| {
        let all = scan(b);
        assert_eq!(b.index.len(), all.len(), "{step}: the index holds exactly the board's ids");
        for &(id, k, i) in &all {
            assert_eq!(b.index.get(id), Some((k, i)), "{step}: the index entry of {id:?} is its list and position");
            assert_eq!(b.locate(id), Some((k, i)), "{step}: locate finds {id:?} where the scan does");
        }
        assert_eq!(b.locate(ItemId(u32::MAX)), None, "{step}: an unknown id is not found");
    };
    let mut b = board_of(&ledctl(), 0);
    assert!(!b.vias.is_empty() && !b.tracks.is_empty() && !b.holes.is_empty() && b.placed.len() >= 2, "the fixture board has the lists this test edits");
    agree(&b, "the fixture board");

    let id = b.alloc_id();
    b.apply_host(&BoardOp::Add(Item::Via(Via { id, ..b.vias[0].clone() }))).expect("a via is appended");
    agree(&b, "after appending a via");
    let mid = b.vias[b.vias.len() / 2].id;
    b.apply_host(&BoardOp::Remove(mid)).expect("the middle via is removed");
    agree(&b, "after removing the middle via");
    let first = b.placed[0].id;
    b.apply_host(&BoardOp::Remove(first)).expect("the first part is removed");
    agree(&b, "after removing the first part (its whole list shifts)");

    let id = b.alloc_id();
    let track = Track { id, ..b.tracks.last().unwrap().clone() };
    let inserted = b.apply_host(&BoardOp::Insert { item: Item::Track(track), pos: 0 }).expect("a track is inserted at the front");
    agree(&b, "after inserting a track at position 0");
    let hole = b.holes[0].clone();
    let replaced = b.apply_host(&BoardOp::Replace(Item::Hole(Hole { at: hole.at.add(P::mm(0.1, 0.0)), ..hole }))).expect("a hole is replaced");
    agree(&b, "after replacing a hole");
    b.undo(&replaced.inverse).expect("the replace undoes");
    agree(&b, "after undoing the replace");
    b.undo(&inserted.inverse).expect("the insert undoes");
    agree(&b, "after undoing the insert");

    let id = b.alloc_id();
    let before = b.clone();
    let failed = b.apply_all(&[BoardOp::Add(Item::Via(Via { id, ..b.vias[0].clone() })), BoardOp::Remove(ItemId(u32::MAX))]);
    assert!(failed.is_err(), "a batch with a missing id fails");
    assert_eq!(b, before, "the failed batch changes nothing");
    agree(&b, "after a rolled-back batch");

    let id = b.alloc_id();
    b.vias.push(Via { id, ..b.vias[0].clone() });
    assert_eq!(b.index.get(id), None, "a direct push bypasses the index");
    assert_eq!(b.locate(id), Some((ItemKind::Via, b.vias.len() - 1)), "locate finds a directly pushed item by its scan");
    b.reindex();
    agree(&b, "after reindex");

    let mut bare = b.clone();
    bare.index = ItemIndex::default();
    assert_eq!(bare, b, "boards differing only in their index are equal");
    assert_eq!(bare.content_hash(), b.content_hash(), "the index is not part of the canonical hash");
}
