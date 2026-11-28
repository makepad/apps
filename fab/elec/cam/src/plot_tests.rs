use super::build_model;
use crate::image::{DrillHole, Geom, ObjAttrs};
use crate::profile::{BomRow, PlaceRow, ResolvedProfile};
use fab_elec_board::{Anchor, Area, Board, BoardNet, Cutout, Flash, Graphic, Hole, Origin, Placed, Stackup, Tent, Track, Via, ViaKind, Zone, ZoneSettings};
use fab_elec_circuit::{Circuit, Component, Net, NetKind};
use fab_elec_core::{mm, Angle, BBox, BlockId, BoardId, ClassId, CompId, Contour, ItemId, LayerId, LayerSet, NetId, P, Path, PolySet, Shape, Side};
use fab_elec_parts::{Drill, Footprint, Gfx, Pad, PadKind, PadProp, PadShape, PadStack, PartDef, PasteRule};
use std::sync::Arc;

#[test]
fn board_geometry_attributes_and_factory_roundtrip() {
    let pad = |no: &str, x| Pad {
        no: no.into(), kind: PadKind::Smd, stack: PadStack::smd(PadShape::Rect { size: P::mm(0.8, 0.9) }),
        at: P::mm(x, 0.0), rot: Angle::ZERO, drill: None, mask_margin: None,
        paste: PasteRule { margin: Some(-mm(0.02)), ratio_ppm: Some(-100_000) },
        thermal: None, zone_connect: None, die_len: 0, prop: PadProp::None,
    };
    let fp = Arc::new(Footprint {
        name: "0603".into(), pads: vec![pad("1", -0.8), pad("2", 0.8)],
        graphics: vec![(LayerId::F_SILK, Gfx::Stroke { path: Path::new(P::mm(-0.5, 0.6)).line(P::mm(0.5, 0.6)).arc(P::mm(0.7, 0.8), P::mm(0.5, 1.0)), w: mm(0.1) })],
        ..Footprint::default()
    });
    let part = Arc::new(PartDef { name: "resistor".into(), kind: fab_elec_parts::PartKind::Resistor, symbol: Default::default(), footprint: Some(fp.clone()), body: None, pins: vec![], fields: Default::default(), emitter: None, gpio: None, lifecycle: None, approved: false });
    let mut circuit = Circuit::default();
    circuit.comps = ["R1", "R2", "R3"].iter().enumerate().map(|(i, r)| Component {
        id: CompId(i as u32), path: (*r).into(), refdes: (*r).into(), ref_kind: Default::default(), part: part.clone(), value: None,
        conns: vec![], nc: vec![], block: BlockId(0), dnp: i == 2, fields: Default::default(),
    }).collect();
    circuit.nets.push(Net { id: NetId(0), name: "GND".into(), class: ClassId(0), volts: None, kind: NetKind::Ground, scope: None });
    let mut board = Board::new(BoardId(0), "plot");
    board.stackup = Stackup::standard_2_layer();
    board.nets.push(BoardNet { name: "GND".into(), class: ClassId(0) });
    board.outline.push(Contour::rect(P::ZERO, P::mm(10.0, 10.0), mm(1.0)));
    board.placed.push(Placed { id: ItemId(7), comp: CompId(0), path: "R1".into(), refdes: "R1".into(), footprint: fp,
        pad_nets: vec![Some(NetId(0)); 2], at: P::mm(3.0, 3.0), rot: Angle::ZERO, side: Side::Top, locked: false, origin: Origin::Authored, body_z: None, ref_label: None, value_label: None, legend: None, value: None });
    board.tracks.push(Track { id: ItemId(3), net: NetId(0), layer: LayerId::F_CU, w: mm(0.2),
        widths: Some(vec![mm(0.2), mm(0.3), mm(0.4)]), path: Path::new(P::mm(3.8, 3.0)).line(P::mm(5.0, 3.0)).arc(P::mm(5.5, 3.5), P::mm(6.0, 3.0)),
        ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored });
    board.vias.push(Via { id: ItemId(2), net: NetId(0), at: P::mm(6.0, 3.0), drill: mm(0.3), pad: mm(0.6),
        span: (LayerId::F_CU, LayerId::B_CU), kind: ViaKind::Through, tent: Tent::Back, backdrill: None, locked: false, origin: Origin::Authored, flash: Flash::All });
    board.zones.push(Zone { id: ItemId(9), net: Some(NetId(0)), name: "ground".into(), layers: LayerSet::one(LayerId::F_CU),
        area: Area::Board, priority: 0, settings: ZoneSettings::default(), locked: false, origin: Origin::Authored });
    for (id, layer) in [(6, LayerId::B_SILK), (4, LayerId::F_SILK)] {
        board.graphics.push(Graphic { id: ItemId(id), layer, gfx: Gfx::Fill(Shape::Circle { c: P::mm(8.0, 2.0), r: mm(0.1) }), locked: false, origin: Origin::Authored });
    }
    board.holes.push(Hole { id: ItemId(5), name: "mount".into(), at: P::mm(8.0, 8.0), drill: mm(1.0), pad: None, net: None, plated: false, origin: Origin::Authored });
    board.next_item = 10;
    let pour = PolySet::rect(BBox::new(P::mm(1.0, 5.0), P::mm(4.0, 8.0)));
    let fills = vec![crate::testfill::one_poly(ItemId(9), LayerId::F_CU, pour.clone())];
    let model = build_model(&board, &circuit, &fills);
    assert_eq!(model.copper_layers, 2, "copper layer count follows the stackup");
    for (layer, count) in [(LayerId::F_CU, 6), (LayerId::B_CU, 1), (LayerId::F_MASK, 3), (LayerId::B_MASK, 0),
        (LayerId::F_PASTE, 2), (LayerId::B_PASTE, 0), (LayerId::F_SILK, 3), (LayerId::B_SILK, 1), (LayerId::EDGE, 8)] {
        assert_eq!(model.layers[&layer].len(), count, "item count for {}", layer.name());
    }
    let cu = &model.layers[&LayerId::F_CU];
    assert_eq!(cu[0].geom, Geom::Shape(Shape::Poly(pour.clone())), "zone fill is first");
    assert_eq!(cu[0].attrs, ObjAttrs { net: Some("GND".into()), ..Default::default() }, "fill has its net and no pin");
    for (i, no) in [(1, "1"), (2, "2")] {
        assert_eq!(cu[i].attrs, ObjAttrs { aper_function: vec!["SMDPad".into(), "CuDef".into()], net: Some("GND".into()),
            pin: Some(("R1".into(), no.into())), component: Some("R1".into()) }, "pad {no} attributes");
    }
    assert!(matches!(cu[3].geom, Geom::Shape(Shape::Segment { w, .. }) if w == mm(0.3)), "line uses segment width");
    assert!(matches!(cu[4].geom, Geom::Shape(Shape::Arc { w, .. }) if w == mm(0.4)), "arc stays an arc with segment width");
    assert!(matches!(cu[5].geom, Geom::Shape(Shape::Circle { r, .. }) if r == mm(0.3)), "via pad is a circle");
    for (i, function) in [(3, "Conductor"), (4, "Conductor"), (5, "ViaPad")] {
        assert_eq!(cu[i].attrs, ObjAttrs { aper_function: vec![function.into()], net: Some("GND".into()), ..Default::default() }, "copper {function} attributes");
    }
    assert!(matches!(model.layers[&LayerId::F_MASK][0].geom, Geom::Shape(Shape::Rect { half, .. }) if half == P::mm(0.45, 0.50)), "mask grows pad by fallback margin");
    assert!(matches!(model.layers[&LayerId::F_PASTE][0].geom, Geom::Shape(Shape::Rect { half, .. }) if half == P::mm(0.34, 0.385)), "paste applies signed ratio and margin");
    assert_eq!(model.layers[&LayerId::EDGE].iter().filter(|g| matches!(g.geom, Geom::Shape(Shape::Arc { w: 100_000, .. }))).count(), 4, "rounded outline keeps four arcs");
    assert_eq!(model.holes, vec![(true, DrillHole { at: P::mm(6.0, 3.0), dia: mm(0.3), to: None }),
        (false, DrillHole { at: P::mm(8.0, 8.0), dia: mm(1.0), to: None })], "via plated and standalone hole emitted once");
    assert_eq!(model.poses.len(), 1, "only placed parts have poses");
    assert_eq!((&model.poses[0].refdes, model.poses[0].at, model.poses[0].rot, model.poses[0].side), (&"R1".to_string(), P::mm(3.0, 3.0), Angle::ZERO, Side::Top), "placement pose");
    assert_eq!(model.bom_refs, ["R1", "R2"], "BOM includes unplaced parts and omits DNP");

    let p = &mut board.placed[0];
    p.side = Side::Bottom;
    p.rot = Angle::QUARTER;
    let pad = &mut Arc::make_mut(&mut p.footprint).pads[1];
    pad.kind = PadKind::Thru;
    pad.stack = PadStack::thru(PadShape::Oval { size: P::mm(0.9, 1.5) });
    pad.rot = Angle::QUARTER;
    pad.drill = Some(Drill { size: P::mm(0.4, 1.0), offset: P::mm(0.1, 0.0) });
    pad.mask_margin = Some(mm(0.1));
    let model = build_model(&board, &circuit, &fills);
    assert_eq!(model.holes[2], (true, DrillHole { at: P::mm(2.9, 1.9), dia: mm(0.4), to: Some(P::mm(2.9, 2.5)) }), "slot end-cap centres include offset, pad rotation, mirror and placement rotation");
    assert_eq!(model.layers[&LayerId::F_CU][1].attrs.aper_function, ["ComponentPad"], "through pad aperture function");
    assert_eq!((model.layers[&LayerId::F_PASTE].len(), model.layers[&LayerId::B_PASTE].len()), (0, 1), "bottom SMD paste only, no through paste");
    assert!(matches!(model.layers[&LayerId::F_MASK][0].geom, Geom::Shape(Shape::Oval { half, .. }) if half == P::mm(0.55, 0.85)), "pad mask override");
    assert_eq!(model.layers[&LayerId::B_SILK][1].geom, Geom::Shape(Shape::Segment { a: P::mm(2.4, 3.5), b: P::mm(2.4, 2.5), w: mm(0.1) }), "footprint silk maps to bottom with mirror and rotation");
    // a part's legend plots with its silk, on its own side: this part is on the bottom now
    board.placed[0].legend = Some(fab_elec_board::text::legend_text("GAIN", &board.placed[0].footprint));
    let marked = build_model(&board, &circuit, &fills);
    let (layer, strokes) = fab_elec_board::text::footprint_text(&board.placed[0], board.placed[0].legend.as_ref().expect("the legend"), 2);
    assert_eq!(layer, LayerId::B_SILK, "a bottom part's legend is on the bottom silk");
    let silk = model.layers[&LayerId::B_SILK].len();
    assert!(!strokes.is_empty() && marked.layers[&LayerId::B_SILK][silk..].iter().map(|g| &g.geom).eq(strokes.iter().map(|s| Geom::Shape(s.clone())).collect::<Vec<_>>().iter()),
        "the legend's strokes follow the part's silk");
    assert_eq!(marked.layers[&LayerId::F_SILK].len(), model.layers[&LayerId::F_SILK].len(), "nothing of it reaches the front");
    board.placed[0].legend = None;
    let mut profile = ResolvedProfile::standard_2_layer("plot", "A");
    for r in &model.bom_refs {
        profile.bom.rows.push(BomRow { cells: vec!["10k".into(), r.clone(), "0603".into(), "resistor".into()], refs: vec![r.clone()] });
    }
    profile.placement.rows.push(PlaceRow { cells: vec!["R1".into(), "3".into(), "3".into(), "B".into(), "90".into()],
        refdes: "R1".into(), at: P::mm(3.0, 3.0), rot: Angle::QUARTER, rot_offset: Angle::ZERO, side: Side::Bottom });
    let files = crate::model::render(&model, &profile).expect("factory render succeeds");
    let validation = crate::validate::validate(&files, &profile, &model);
    assert!(validation.ok(), "factory roundtrip validates: {:?}", validation.failures().collect::<Vec<_>>());

    let pad = &mut Arc::make_mut(&mut board.placed[0].footprint).pads[1];
    pad.kind = PadKind::Npth;
    pad.stack.front = PadShape::Circle { d: 0 };
    let npth = build_model(&board, &circuit, &fills);
    assert_eq!((npth.layers[&LayerId::F_CU].len(), npth.layers[&LayerId::B_CU].len()), (4, 2), "NPTH omits copper on both sides");
    assert!(!npth.holes[2].0 && npth.holes[2].1.to.is_some(), "NPTH slot is non-plated");
    assert!(matches!(npth.layers[&LayerId::F_MASK][0].geom, Geom::Shape(Shape::Oval { half, .. }) if half == P::mm(0.3, 0.6)), "NPTH opens drill plus margin when no land exists");
    let mut dnp = board.placed[0].clone();
    dnp.id = board.alloc_id(); dnp.comp = CompId(2); dnp.refdes = "R3".into(); dnp.path = "R3".into();
    board.placed.insert(0, dnp);
    let cutout_id = board.alloc_id();
    board.cutouts.push(Cutout { id: cutout_id, name: "cutout".into(), contour: Contour::rect(P::mm(7.0, 5.0), P::mm(8.0, 6.0), 0), origin: Origin::Authored });
    let ordered = build_model(&board, &circuit, &fills);
    assert_eq!(ordered.poses, npth.poses, "DNP placement omitted");
    assert_eq!(ordered.layers[&LayerId::EDGE].len(), 12, "cutout contour adds four edge lines");
    board.placed.reverse(); board.graphics.reverse();
    assert_eq!(build_model(&board, &circuit, &fills), ordered, "item order is independent of input vector order");
}
