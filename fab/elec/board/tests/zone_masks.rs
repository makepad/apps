use fab_elec_board::{
    resolve_rules, zone::{fill, static_masks}, Area, Board, BoardKeepout, BoardNet, Islands,
    Origin, Placed, Zone, ZoneSettings, ARC_ERR,
};
use fab_elec_core::{Angle, BBox, BoardId, ClassId, CompId, Contour, LayerId, LayerSet, NetId, P, PolySet, Side};
use fab_elec_parts::{Footprint, Forbid, Pad, PadKind, PadProp, PadShape, PadStack, PasteRule, ThermalRule, ZoneConnect};
use std::sync::Arc;

fn pad(board: &mut Board, at: P, connect: ZoneConnect) {
    let id = board.alloc_id();
    board.placed.push(Placed {
        id, comp: CompId(board.placed.len() as u32), path: String::new(), refdes: String::new(),
        footprint: Arc::new(Footprint {
            pads: vec![Pad {
                no: "1".into(), kind: PadKind::Smd,
                stack: PadStack::smd(PadShape::Rect { size: P::new(800_000, 800_000) }),
                at: P::ZERO, rot: Angle::ZERO, drill: None, mask_margin: None,
                paste: PasteRule::default(), thermal: Some(ThermalRule {
                    gap: 300_000, spoke: 200_000, angle: Angle::ZERO,
                }),
                zone_connect: Some(connect), die_len: 0, prop: PadProp::None,
            }],
            ..Footprint::default()
        }),
        pad_nets: vec![Some(NetId(0))], at, rot: Angle::ZERO, side: Side::Top,
        locked: false, origin: Origin::Authored, body_z: None, ref_label: None, value_label: None, legend: None, value: None,
    });
}

#[test]
fn static_masks_match_fill_exclusions_and_preserve_fill() {
    let mut board = Board::new(BoardId(0), "pour masks");
    board.outline.push(Contour::rect(P::ZERO, P::new(12_000_000, 8_000_000), 0));
    board.nets = vec![
        BoardNet { name: "pour".into(), class: ClassId(0) },
        BoardNet { name: "other".into(), class: ClassId(0) },
    ];
    let zone = board.alloc_id();
    board.zones.push(Zone {
        id: zone, net: Some(NetId(0)), name: "pour".into(), layers: board.copper_set(),
        area: Area::Poly(PolySet::rect(BBox::new(P::new(1_000_000, 1_000_000), P::new(11_000_000, 7_000_000)))),
        priority: 0, settings: ZoneSettings {
            clearance: Some(250_000), islands: Islands::Keep, ..ZoneSettings::default()
        },
        locked: false, origin: Origin::Authored,
    });
    pad(&mut board, P::new(3_000_000, 4_000_000), ZoneConnect::Thermal);
    pad(&mut board, P::new(6_000_000, 4_000_000), ZoneConnect::Off);
    pad(&mut board, P::new(3_000_000, 2_000_000), ZoneConnect::Solid);
    pad(&mut board, P::new(5_000_000, 2_000_000), ZoneConnect::ThruThermal);

    let hole = PolySet::rect(BBox::point(P::new(9_000_000, 4_000_000)).inflate(200_000));
    let ring = PolySet::rect(BBox::point(P::new(9_000_000, 4_000_000)).inflate(600_000)).difference(&hole);
    for (area, forbid) in [
        (ring, Forbid::ZONES),
        (PolySet::rect(BBox::point(P::new(2_000_000, 2_000_000)).inflate(200_000)), Forbid::TRACKS),
    ] {
        let id = board.alloc_id();
        board.keepouts.push(BoardKeepout {
            id, area, layers: LayerSet::one(LayerId::F_CU), forbid,
            reason: String::new(), origin: Origin::Authored, z: None,
        });
    }
    let id = board.alloc_id();
    board.zones.push(Zone {
        id, net: Some(NetId(1)), name: "higher".into(), layers: LayerSet::one(LayerId::F_CU),
        area: Area::Poly(PolySet::rect(BBox::point(P::new(9_000_000, 6_000_000)).inflate(300_000))),
        priority: 1, settings: ZoneSettings::default(), locked: false, origin: Origin::Authored,
    });
    board.reindex();
    let rules = resolve_rules(&board, None);
    let before = fill(&board, &rules, zone);
    let masks = static_masks(&board, &rules, zone);
    assert_eq!(masks.iter().map(|(l, _)| *l).collect::<Vec<_>>(), board.copper_set().iter().collect::<Vec<_>>(),
        "static masks retain zone copper-layer order");
    let front = &masks.iter().find(|(l, _)| *l == LayerId::F_CU).expect("front masks").1;
    assert!(!front.is_empty(), "thermal, disconnected-pad and polygon exclusions yield nonempty masks");
    assert!(front.contains(P::new(3_550_000, 4_350_000)), "thermal ring away from a spoke is masked");
    assert!(!front.contains(P::new(3_550_000, 4_000_000)), "thermal spoke remains an opening in the mask");
    assert!(!front.contains(P::new(3_000_000, 4_000_000)), "thermal pad copper remains outside its ring mask");
    assert!(front.contains(P::new(6_000_000, 4_000_000)), "ZoneConnect::Off masks its own pad");
    assert!(front.contains(P::new(6_600_000, 4_000_000)), "ZoneConnect::Off mask includes the zone gap");
    assert!(!front.contains(P::new(3_000_000, 2_000_000)), "solid own-net pads need no static mask");
    assert!(!front.contains(P::new(5_550_000, 2_350_000)), "ThruThermal does not mask a surface pad");
    assert!(front.contains(P::new(500_000, 4_000_000)), "polygon-zone complement is masked");
    assert!(front.contains(P::new(-200_000, 4_000_000)), "polygon complement extends through the zone gap");
    assert!(!front.contains(P::new(-1_000_000, 4_000_000)), "polygon complement has a finite outline-box clip");
    assert!(front.contains(P::new(9_400_000, 4_000_000)), "zone-forbidding keepout copper is masked");
    assert!(!front.contains(P::new(9_000_000, 4_000_000)), "a keepout hole remains outside the mask");
    assert!(!front.contains(P::new(2_000_000, 2_000_000)), "track-only keepouts are not zone masks");
    assert!(front.contains(P::new(9_000_000, 6_000_000)), "higher-priority foreign zone bases are masked");
    let back = &masks.iter().find(|(l, _)| *l == LayerId::B_CU).expect("back masks").1;
    assert!(!back.contains(P::new(6_000_000, 4_000_000)), "surface-pad masks do not reach back copper");
    assert!(!back.contains(P::new(9_000_000, 6_000_000)), "higher-priority masks respect their zone layers");

    assert!(!before.is_empty(), "mask/fill intersection checks exercise kept copper");
    for (layer, mask) in &masks {
        let interior = mask.offset(-ARC_ERR, ARC_ERR);
        assert!(!interior.is_empty(), "layer {layer:?} has mask interiors after arc-error shrink");
        let kept = before.layer(*layer).expect("filled mask layer").set();
        assert_eq!(kept.intersection(&interior).area(), 0.0,
            "kept fill never overlaps static masks after arc-error shrink on {layer:?}");
    }
    let after = fill(&board, &rules, zone);
    assert!(before.same(&after), "repeated fill retains identical tile hashes and labels after mask extraction");
    assert_eq!(before, after, "repeated fill retains the entire ZoneFill bit for bit");
    let absent = board.alloc_id();
    assert!(static_masks(&board, &rules, absent).is_empty(), "absent zones have no mask layers");
}
