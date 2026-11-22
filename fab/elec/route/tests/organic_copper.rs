use fab_elec_route::organic::{copper, padgeom, geom::Vec2, model::*};

#[test]
fn copper_geometry_and_pad_connectivity() {
    let v = Vec2::new;
    for (name, a, b, c, d, expected) in [
        ("crossing", v(0.0, 0.0), v(10.0, 0.0), v(5.0, -1.0), v(5.0, 1.0), 0.0),
        ("parallel", v(0.0, 0.0), v(10.0, 0.0), v(0.0, 2.0), v(10.0, 2.0), 2.0),
        ("collinear gap", v(0.0, 0.0), v(10.0, 0.0), v(12.0, 0.0), v(15.0, 0.0), 2.0),
        ("collinear overlap", v(0.0, 0.0), v(10.0, 0.0), v(5.0, 0.0), v(15.0, 0.0), 0.0),
        ("two points", v(0.0, 0.0), v(0.0, 0.0), v(3.0, 4.0), v(3.0, 4.0), 5.0),
    ] {
        let actual = copper::seg_seg(a, b, c, d).d;
        assert!((actual - expected).abs() < 1e-9, "segment distance {name}: {actual} vs {expected}");
    }

    let mut pad = Pad { center: v(0.0, 0.0), round_rect_ratio: 0.25, ..Pad::default() };
    let half_width = 0.125;
    for (name, shape, size, q, expected, tol) in [
        ("oval end", PadShape::Oval, v(3.0, 1.0), v(2.0, 0.0), 0.5, 1e-9),
        ("oval side", PadShape::Oval, v(3.0, 1.0), v(0.0, 1.0), 0.5, 1e-9),
        ("oval shoulder", PadShape::Oval, v(3.0, 1.0), v(1.0, 1.0), 0.5, 1e-9),
        ("rect corner", PadShape::Rect, v(2.0, 1.0), v(2.0, 2.0), (1.0_f64 + 2.25).sqrt(), 1e-6),
        ("rect interior", PadShape::Rect, v(2.0, 1.0), v(0.5, 0.2), 0.0, 1e-6),
        ("roundrect corner", PadShape::RoundRect, v(2.0, 1.0), v(2.0, 2.0), (1.25_f64 * 1.25 + 1.75 * 1.75).sqrt() - 0.25, 1e-6),
        ("circle corner", PadShape::Circle, v(2.0, 1.0), v(2.0, 2.0), 8.0_f64.sqrt() - 1.0, 1e-6),
    ] {
        pad.shape = shape;
        for rotated in [false, true] {
            pad.rotation = if rotated { 90.0 } else { 0.0 };
            pad.local_size = if rotated { size } else { v(0.0, 0.0) };
            pad.size = if rotated { v(size.y, size.x) } else { size };
            let q = if rotated { v(q.y, -q.x) } else { q };
            let actual = padgeom::point_distance_pad(&pad, q.x as f64, q.y as f64);
            assert!((actual - expected).abs() < tol, "point pad {name}, rotated={rotated}: {actual} vs {expected}");
            let gap = copper::seg_pad_gap(q, q, half_width, &pad);
            assert!((gap - (expected - half_width)).abs() < tol, "point copper gap {name}, rotated={rotated}: {gap}");
        }
    }
    for (name, shape, size, a, b, expected) in [
        ("circle above", PadShape::Circle, v(2.0, 1.0), v(-5.0, 3.0), v(5.0, 3.0), 2.0),
        ("rect above", PadShape::Rect, v(2.0, 1.0), v(-5.0, 3.0), v(5.0, 3.0), 2.5),
        ("rect crossing", PadShape::Rect, v(2.0, 1.0), v(-5.0, 0.0), v(5.0, 0.0), 0.0),
        ("oval above", PadShape::Oval, v(3.0, 1.0), v(-5.0, 3.0), v(5.0, 3.0), 2.5),
        ("roundrect above", PadShape::RoundRect, v(2.0, 1.0), v(-5.0, 3.0), v(5.0, 3.0), 2.5),
    ] {
        pad.shape = shape;
        for rotated in [false, true] {
            pad.rotation = if rotated { 90.0 } else { 0.0 };
            pad.local_size = if rotated { size } else { v(0.0, 0.0) };
            pad.size = if rotated { v(size.y, size.x) } else { size };
            let a = if rotated { v(a.y, -a.x) } else { a };
            let b = if rotated { v(b.y, -b.x) } else { b };
            let actual = padgeom::segment_distance_pad(&pad, a.x as f64, a.y as f64, b.x as f64, b.y as f64);
            assert!((actual - expected).abs() < 1e-9, "segment pad {name}, rotated={rotated}: {actual} vs {expected}");
            let gap = copper::seg_pad_gap(a, b, half_width, &pad);
            assert!((gap - (expected - half_width)).abs() < 1e-9, "segment copper gap {name}, rotated={rotated}: {gap}");
        }
    }

    pad.center = v(4.0, -2.0);
    pad.rotation = 0.0;
    pad.drill_diameter = 1.0;
    let round = padgeom::hole(&pad).expect("round THT drill exists");
    assert!(round.a.approx_eq(pad.center) && round.b.approx_eq(pad.center), "round hole endpoints equal centre");
    assert_eq!(round.r, pad.drill_diameter / 2.0, "round hole radius is half drill diameter");
    assert!(round.centre().approx_eq(pad.center) && !round.slot(), "round hole centre and slot classification");
    pad.drill_size = v(2.0, 1.0);
    let slot = padgeom::hole(&pad).expect("slotted THT drill exists");
    assert!(slot.a.approx_eq(v(3.5, -2.0)) && slot.b.approx_eq(v(4.5, -2.0)), "slot axis runs along local x");
    assert_eq!(slot.half_length(), 0.5, "slot half length");
    assert_eq!(slot.r, 0.5, "slot radius is half minor drill extent");
    assert!(slot.slot() && slot.centre().approx_eq(pad.center), "slot classification and centre");
    pad.is_smd = true;
    assert!(padgeom::hole(&pad).is_none(), "SMD pads have no drill capsule");

    let mut board = OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        nets: vec![Net { id: 0, ..Net::default() }],
        pads: [v(0.0, 0.0), v(10.0, 0.0), v(5.0, 2.0)].into_iter().enumerate().map(|(i, center)| Pad {
            id: i as i32, center, size: v(1.0, 1.0), drill_diameter: 0.5,
            shape: PadShape::Circle, layer_mask: 3, net: 0, is_smd: false, ..Pad::default()
        }).collect(),
        ..OBoard::default()
    };
    let mut essential = Vec::new();
    assert_eq!(copper::net_pad_groups(&board, 0, Some(&mut essential), 1e-3), 2, "three pads without copper need two joins");
    assert!(essential.is_empty(), "no geometric contacts without copper");
    board.traces.push(Trace {
        id: 17, net: 0, layer: 0, width: 0.5, is_routed: true,
        points: vec![
            TracePoint { pos: board.pads[0].center, terminal_pad: 0, ..TracePoint::default() },
            TracePoint { pos: board.pads[1].center, terminal_pad: 1, ..TracePoint::default() },
        ],
        ..Trace::default()
    });
    assert_eq!(copper::net_pad_groups(&board, 0, Some(&mut essential), 1e-3), 1, "terminal attachments join pads zero and one");
    assert!(essential.is_empty(), "terminal attachments are not geometric contacts");
    board.vias.push(Via { id: 23, net: 0, pos: v(5.0, 1.0), diameter: 2.0, layer_start: 0, layer_end: 1, ..Via::default() });
    assert_eq!(copper::net_pad_groups(&board, 0, Some(&mut essential), 1e-3), 0, "geometric via bridge joins the third pad");
    let expected = vec![
        copper::Contact { a: copper::trace_key(0), b: copper::via_key(0) },
        copper::Contact { a: copper::via_key(0), b: copper::pad_key(2) },
    ];
    assert_eq!(essential, expected, "essential contacts use slot keys, in trace-via then via-pad order");
    board.pads.push(Pad { id: 31, center: v(20.0, 20.0), piece_of: 0, ..board.pads[0].clone() });
    essential.clear();
    assert_eq!(copper::net_pad_groups(&board, 0, Some(&mut essential), 1e-3), 0, "piece_of does not add a pad group");
    assert_eq!(essential, expected, "piece attachment does not add an essential geometric contact");

    // The model adaptations: masks are stack bits, planes carry their owning net.
    board.pads[3].layer_mask = 1_u64 << 63;
    assert!(copper::pad_on_layer(&board.pads[3], 63, 2), "pad mask accepts stack bit 63 independently of n_layers");
    assert!(!copper::pad_on_layer(&board.pads[3], 0, 2), "THT mask excludes absent stack bits");
    assert!(!copper::pad_on_layer(&board.pads[3], -1, 2) && !copper::pad_on_layer(&board.pads[3], 64, 2), "pad mask rejects out-of-range shifts");
    board.layers[1].intent = LayerIntent::PowerPlane;
    board.layers[1].plane_net = 0;
    let mut planes = vec![99];
    copper::plane_layers_of(&board, 0, &mut planes);
    assert_eq!(planes, vec![1], "plane membership uses intent and stored net, replacing prior output");
    essential.clear();
    assert_eq!(copper::net_pad_groups(&board, 0, Some(&mut essential), 1e-3), 0, "plane and piece attachments join pads before geometry");
    assert!(essential.is_empty(), "plane attachments need no essential geometric contacts");
    board.vias[0].layer_start = 1;
    board.vias[0].layer_end = 0;
    assert!(copper::via_on_layer(&board.vias[0], 0) && copper::via_on_layer(&board.vias[0], 1), "via span includes both ends in reverse order");
    assert!(!copper::via_on_layer(&board.vias[0], 2), "via span excludes layers beyond its ends");
}
