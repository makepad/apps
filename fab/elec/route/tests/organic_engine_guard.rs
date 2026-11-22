use fab_elec_route::organic::{
    copper,
    engine::{guard::LocalModel, Engine, Prim},
    geom::Vec2,
    model::*,
    settings::OrganicSettings,
};

#[test]
fn organic_engine_guard_preserves_clearance() {
    let v = Vec2::new;
    let point = |pos| TracePoint { pos, ..TracePoint::default() };
    let mut b = OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        clr: ClearanceTable { rows: 1, layers: 2, mm: vec![0.2; 2], max: 0.2 },
        rules: DesignRules {
            min_clearance: 0.2,
            board_edge_clearance: 0.3,
            hole_clearance: 0.25,
            hole_to_hole: 0.25,
            ..DesignRules::default()
        },
        outline: vec![v(0.0, 0.0), v(50.0, 0.0), v(50.0, 30.0), v(0.0, 30.0)],
        nets: vec![Net { id: 0, ..Net::default() }, Net { id: 1, ..Net::default() }],
        traces: vec![
            Trace {
                id: 0, net: 0, layer: 0, width: 0.5, rule: 0, is_routed: true,
                points: vec![point(v(10.0, 10.0)), point(v(30.0, 10.0))],
                ..Trace::default()
            },
            Trace {
                id: 1, net: 1, layer: 0, width: 0.5, rule: 0, is_routed: true,
                points: vec![point(v(15.0, 12.0)), point(v(20.0, 11.0)), point(v(25.0, 12.0))],
                ..Trace::default()
            },
        ],
        ..OBoard::default()
    };
    let mut engine = Engine::new(&b, &OrganicSettings::base());
    engine.build_static(&b, true);
    engine.build_dynamic(&b, None, 0.0, None);
    engine.compute_essential(&b, None, 0.0);

    let moving_prims = |b: &OBoard, pos: Vec2| {
        let tr = &b.traces[1];
        let p = Prim {
            a: tr.points[0].pos, b: pos, h: tr.width as f64 * 0.5, layer: tr.layer, net: tr.net,
            trace: 1, ex_lo: 0, ex_hi: 1, owner: 0, obj: copper::trace_key(1), rule: tr.rule,
            ..Prim::default()
        };
        vec![p, Prim { a: tr.points[2].pos, ..p }]
    };
    let copper_gap = |prims: &[Prim]| {
        prims.iter().map(|p| {
            copper::seg_seg(p.a, p.b, v(10.0, 10.0), v(30.0, 10.0)).d - p.h - 0.25
        }).fold(f64::INFINITY, f64::min)
    };
    // A strong spring towards y = 9 pulls into the foreign trace at y = 10.
    let inward = |pos: Vec2| LocalModel {
        gy: 1000.0 * (pos.y as f64 - 9.0), hxx: 1000.0, hyy: 1000.0,
        ..LocalModel::default()
    };
    let start = b.traces[1].points[1].pos;
    let mut pos = start;
    let mut prims = moving_prims(&b, pos);
    let mut moved = false;
    for step in 0..20 {
        let result = engine.try_move_x0(&b, pos, &mut prims, 1, inward(pos), &[], 0.0, false, 1.0);
        pos = result.pos;
        moved |= result.moved;
        // Both neighbours stay at y = 12: the lowest endpoint gives the minimum distance.
        let gap = copper_gap(&moving_prims(&b, pos));
        assert!(gap >= engine.clr_g - 1e-9,
            "inward pull {step} keeps both segments' copper gap >= clr_g: {gap} vs {}", engine.clr_g);
    }
    assert!(moved && pos.y < start.y, "inward spring makes progress before the clearance guard stops it");

    let mut prims = moving_prims(&b, start);
    let away = LocalModel { gy: -1000.0, hxx: 1.0, hyy: 1.0, ..LocalModel::default() };
    let result = engine.try_move_x0(&b, start, &mut prims, 1, away, &[], 0.0, false, 1.0);
    assert!(result.moved && result.pos.y > start.y, "an outward spring moves away from the foreign trace");
    assert!(result.pos.distance_to(start) as f64 <= engine.max_step + 1e-6,
        "outward displacement never exceeds max_step");

    // Centre distance = both 0.25 mm half widths + clr_g. Round outwards by at most one
    // f32 ULP so the node begins on the legal side of the exact clearance boundary.
    let limit = 10.0 + 0.25 + 0.25 + engine.clr_g;
    let mut limit_y = limit as f32;
    if (limit_y as f64) < limit { limit_y = f32::from_bits(limit_y.to_bits() + 1); }
    let limit_pos = v(20.0, limit_y);
    let mut prims = moving_prims(&b, limit_pos);
    let old_gap = copper_gap(&prims);
    let result = engine.try_move_x0(&b, limit_pos, &mut prims, 1, inward(limit_pos), &[], 0.0, false, 1.0);
    let new_gap = copper_gap(&moving_prims(&b, result.pos));
    assert!(old_gap >= engine.clr_g && old_gap - engine.clr_g < 1e-6,
        "clearance-limit fixture starts within one f32 ULP on the legal side");
    assert!(!result.moved || new_gap >= old_gap - 1e-9,
        "a pull into the obstacle at the clearance limit stays put or preserves the gap");

    // The chord at y = 12 has a 2 - 0.25 - 0.25 = 1.5 mm copper gap.
    let old_prims = moving_prims(&b, start);
    let chord = Prim { b: old_prims[1].a, ..old_prims[0] };
    assert!((copper_gap(&[chord]) - 1.5).abs() < 1e-12, "safe chord has a geometric 1.5 mm copper gap");
    assert!(engine.guard_replace(&b, &old_prims, &[chord], 1, &[], 1, 0, 1),
        "guard_replace accepts a chord that preserves clearance");

    // Two legs pass around the endpoint (30,10): each centreline is 8/sqrt(29) mm
    // from it. Their chord, from (29,8) to (29,12), crosses the foreign centreline.
    b.traces[1].points = vec![point(v(29.0, 8.0)), point(v(34.0, 10.0)), point(v(29.0, 12.0))];
    engine.build_dynamic(&b, None, 0.0, None);
    let old_prims = moving_prims(&b, b.traces[1].points[1].pos);
    let chord = Prim { b: old_prims[1].a, ..old_prims[0] };
    let detour_gap = 8.0 / 29.0f64.sqrt() - 0.5;
    assert!((copper_gap(&old_prims) - detour_gap).abs() < 1e-12 && detour_gap > engine.clr_g,
        "detour legs clear the foreign endpoint by 8/sqrt(29) minus both half widths");
    assert!((copper_gap(&[chord]) - (-0.5)).abs() < 1e-12, "unsafe chord crosses copper with a -0.5 mm gap");
    assert!(!engine.guard_replace(&b, &old_prims, &[chord], 1, &[], 1, 0, 1),
        "guard_replace rejects a chord cutting through foreign clearance");

    b.traces[1].points = vec![point(v(15.0, 11.0)), point(v(20.0, 11.0)), point(v(25.0, 11.0))];
    engine.build_dynamic(&b, None, 0.0, None);
    let (slack, gap) = engine.segment_clearance_x0(&b, 1, 0, 0.3, 0.0);
    // Parallel centrelines are 1 mm apart, each half width is 0.25 mm.
    let expected_gap = 1.0 - 0.25 - 0.25;
    assert!((gap - expected_gap).abs() < 1e-12,
        "segment_clearance returns centre distance minus both half widths as copper_gap");
    assert!((slack - (expected_gap - engine.clr_g) * 0.5).abs() < 1e-12,
        "segment_clearance halves the available clearance slack because both traces may bulge");

    // A node that starts under the threshold gets out (a restoration step), even while its own
    // model pulls it further in; no step deepens the pair.
    b.traces[1].points = vec![point(v(15.0, 12.0)), point(v(20.0, 10.6)), point(v(25.0, 12.0))];
    engine.build_dynamic(&b, None, 0.0, None);
    let deep = v(20.0, 10.6);
    let start_gap = copper_gap(&moving_prims(&b, deep));
    assert!(start_gap < engine.clr_g - 0.05, "the restoration fixture starts under the threshold: {start_gap}");
    let (mut pos, mut restored) = (deep, 0);
    for step in 0..10 {
        let mut prims = moving_prims(&b, pos);
        let before = copper_gap(&prims);
        let result = engine.try_move_x0(&b, pos, &mut prims, 1, inward(pos), &[], 0.0, false, 1.0);
        let after = copper_gap(&moving_prims(&b, result.pos));
        assert!(after >= before.min(engine.clr_g) - 1e-9, "step {step} deepens no pair: {before} -> {after}");
        restored += usize::from(result.restored);
        pos = result.pos;
    }
    let end_gap = copper_gap(&moving_prims(&b, pos));
    assert!(restored >= 1 && end_gap >= engine.clr_g - 1e-9,
        "an inward pull cannot hold a node under the threshold: {start_gap} -> {end_gap}, {restored} restorations");

    // Boxed between two vias at the same depth, the pushes cancel and the node stays.
    b.nets.push(Net { id: 2, ..Net::default() });
    let via = |id, pos| Via { id, net: 2, pos, diameter: 0.5, drill: 0.3, layer_start: 0, layer_end: 1, connected_pad: -1, ..Via::default() };
    b.vias = vec![via(0, v(40.0, 20.0)), via(1, v(40.0, 21.2))];
    b.traces[1].points = vec![point(v(35.0, 20.6)), point(v(40.0, 20.6)), point(v(45.0, 20.6))];
    let mut engine = Engine::new(&b, &OrganicSettings::base());
    engine.build_static(&b, true);
    engine.build_dynamic(&b, None, 0.0, None);
    engine.compute_essential(&b, None, 0.0);
    let boxed = v(40.0, 20.6);
    let via_gap = |pos: Vec2, c: Vec2| moving_prims(&b, pos).iter()
        .map(|p| copper::seg_seg(p.a, p.b, c, c).d - p.h - 0.25).fold(f64::INFINITY, f64::min);
    let gaps = (via_gap(boxed, v(40.0, 20.0)), via_gap(boxed, v(40.0, 21.2)));
    assert!(gaps.0 < engine.clr_g && gaps.1 < engine.clr_g, "both vias start under the threshold: {gaps:?}");
    let mut prims = moving_prims(&b, boxed);
    let still = LocalModel { hxx: 1.0, hyy: 1.0, ..LocalModel::default() };
    let result = engine.try_move_x0(&b, boxed, &mut prims, 1, still, &[], 0.0, false, 1.0);
    assert!(!result.moved && !result.restored && result.pos.x == boxed.x && result.pos.y == boxed.y,
        "boxed between two pairs at the same depth, the node stays: {:?}", result.pos);

    // Two levels on a via and its own net's pad (via-in-pad off). The via starts just outside its
    // ring wall (ring a full clr_g off the pad) and 0.3 mm under its threshold to another net's
    // trace on the far side, more than it can cure. A restoration may take it inside the ring
    // wall, down to the drill rule (drill wall 0.15 mm off the pad copper) and no further; an
    // ordinary step never takes it deeper inside the wall.
    let (pad_edge, ring, drill_r, need) = (40.5, 0.25, 0.15, 0.15);
    b.pads = vec![Pad {
        id: 0, center: v(40.0, 10.0), size: v(1.0, 1.0), local_size: v(1.0, 1.0), drill_diameter: 0.0,
        shape: PadShape::Rect, net: 2, is_smd: true, via_drill_gap: need as f32, ..Pad::default()
    }];
    let x0 = pad_edge + ring + engine.clr_g + 0.01;
    b.vias = vec![via(0, v(x0 as f32, 10.0))];
    let xt = (x0 + ring + 0.25 + engine.clr_g - 0.3) as f32;
    b.traces[1].points = vec![point(v(xt, 5.0)), point(v(xt, 10.0)), point(v(xt, 15.0))];
    let mut engine = Engine::new(&b, &OrganicSettings::base());
    engine.build_static(&b, true);
    engine.build_dynamic(&b, None, 0.0, None);
    engine.compute_essential(&b, None, 0.0);
    let disc = |pos: Vec2| vec![Prim {
        a: pos, b: pos, h: ring, layer: -1, net: 2, via_id: 0, owner: 0, obj: copper::via_key(0),
        drill: 2.0 * drill_r, disc: true, ..Prim::default()
    }];
    let ring_gap = |pos: Vec2| pos.x as f64 - pad_edge - ring;
    let drill_gap = |pos: Vec2| pos.x as f64 - pad_edge - drill_r;
    let (mut pos, mut restored, mut inside) = (v(x0 as f32, 10.0), 0, false);
    for step in 0..12 {
        let mut prims = disc(pos);
        let result = engine.try_move_x0(&b, pos, &mut prims, 1, still, &[], 0.0, false, 1.0);
        assert!(drill_gap(result.pos) >= need - 1e-6,
            "step {step} keeps the drill wall at the pad's drill gap: {}", drill_gap(result.pos));
        if result.moved && !result.restored {
            assert!(ring_gap(result.pos) >= ring_gap(pos).min(engine.clr_g) - 1e-9,
                "ordinary step {step} goes no deeper inside the ring wall: {} -> {}", ring_gap(pos), ring_gap(result.pos));
        }
        restored += usize::from(result.restored);
        inside |= ring_gap(result.pos) < engine.clr_g - 0.05;
        pos = result.pos;
    }
    assert!(restored >= 1 && inside && drill_gap(pos) < need + engine.max_step,
        "restorations take the via inside its ring wall towards the drill rule: ring {}, drill {}, {restored} restorations",
        ring_gap(pos), drill_gap(pos));
}
