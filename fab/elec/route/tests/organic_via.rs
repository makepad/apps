use fab_elec_route::organic::{
    copper::seg_pad_gap,
    geom::{std_max, Vec2},
    model::*,
    relax::{
        state::{frozen_mask_of, global_hash},
        Relaxation, Snapshot,
    },
    settings::OrganicSettings,
};

#[test]
fn organic_via_guarded_moves_keep_members_clearance_pins_and_soft_tether() {
    let v = Vec2::new;
    let pad = |id, net, center, layer| Pad {
        id,
        net,
        center,
        size: v(1.0, 1.0),
        shape: PadShape::Circle,
        layer_mask: 1u64 << layer,
        is_smd: true,
        drill_diameter: 0.0,
        ..Pad::default()
    };
    let base = OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        clr: ClearanceTable { rows: 1, layers: 2, mm: vec![0.2; 2], max: 0.2 },
        rules: DesignRules { min_clearance: 0.2, board_edge_clearance: 0.3, ..DesignRules::default() },
        outline: vec![v(0.0, 0.0), v(50.0, 0.0), v(50.0, 30.0), v(0.0, 30.0)],
        nets: vec![Net { id: 0, ..Net::default() }, Net { id: 1, ..Net::default() }],
        pads: vec![pad(0, 0, v(10.0, 10.0), 0), pad(1, 0, v(14.0, 10.0), 1)],
        vias: vec![Via { id: 0, net: 0, pos: v(12.0, 12.0), ..Via::default() }],
        traces: vec![
            Trace {
                id: 0,
                net: 0,
                layer: 0,
                is_routed: true,
                points: vec![
                    TracePoint { pos: v(10.0, 10.0), terminal_pad: 0, is_fixed: true, ..TracePoint::default() },
                    TracePoint { pos: v(12.0, 12.0), attached_via: 0, ..TracePoint::default() },
                ],
                ..Trace::default()
            },
            Trace {
                id: 1,
                net: 0,
                layer: 1,
                is_routed: true,
                points: vec![
                    TracePoint { pos: v(12.0, 12.0), attached_via: 0, ..TracePoint::default() },
                    TracePoint { pos: v(14.0, 10.0), terminal_pad: 1, is_fixed: true, ..TracePoint::default() },
                ],
                ..Trace::default()
            },
        ],
        ..OBoard::default()
    };
    let s = OrganicSettings { enable_dynamic_vias: true, ..OrganicSettings::base() };
    let prepare = |b: &OBoard, s: &OrganicSettings| {
        let mut r = Relaxation::new(b, s);
        r.apply_external(b, &Snapshot::default(), false, global_hash(b, s));
        r.c.frozen = frozen_mask_of(b, &[], s);
        r.spring_weights(b);
        r.compute_trace_boxes(b);
        r
    };
    let sweep = |r: &mut Relaxation, b: &mut OBoard| {
        r.c.sub += 1;
        r.sub_index = r.c.sub;
        r.budget = -1;
        r.sweep_vias(b);
    };
    let bits = |p: Vec2| (p.x.to_bits(), p.y.to_bits());
    let check_members = |b: &OBoard| {
        for (t, k) in [(0, 1), (1, 0)] {
            assert_eq!(bits(b.traces[t].points[k].pos), bits(b.vias[0].pos),
                "trace {t} member endpoint equals the via position bit for bit");
            assert_eq!(bits(b.traces[t].points[k].velocity), bits(b.vias[0].velocity),
                "trace {t} member endpoint shares the via velocity bit for bit");
        }
    };
    let line_distance = |b: &OBoard| {
        let a = b.pads[0].center;
        let ab = b.pads[1].center - a;
        (b.vias[0].pos - a).cross(ab).abs() / ab.length()
    };

    // Two unit-weight springs pull towards the midpoint (12, 10); the initial
    // perpendicular distance is 2 mm and four 0.1 mm steps cannot reach it yet.
    {
        let mut b = base.clone();
        let mut r = prepare(&b, &s);
        let before = line_distance(&b);
        assert_eq!(before, 2.0, "free via starts 2 mm off the pad-to-pad line");
        for _ in 0..4 {
            sweep(&mut r, &mut b);
            check_members(&b);
        }
        assert!(line_distance(&b) < before, "free via moves towards the pad-to-pad line");
        assert_eq!(r.eval_vias, 4, "four awake via sweeps count four evaluations");
    }

    // The foreign pad is beside the line: at the spring target (12, 10) the
    // copper gap is 0.8 - 0.5 - 0.4 = -0.1 mm, while the initial gap is 0.3 mm.
    // The initial layer-0 trace also clears it: sqrt(0.6^2 + 0.6^2) - 0.625 > 0.2.
    {
        let mut b = base.clone();
        b.pads.push(pad(2, 1, v(12.0, 10.8), 0));
        let rv = b.vias[0].diameter as f64 * 0.5;
        let target = (b.pads[0].center + b.pads[1].center) * 0.5f32;
        assert!(seg_pad_gap(target, target, rv, &b.pads[2]) < 0.2,
            "unconstrained straight-line target violates foreign-pad clearance");
        assert!(seg_pad_gap(b.vias[0].pos, b.vias[0].pos, rv, &b.pads[2]) >= 0.2,
            "foreign-pad fixture starts with legal via copper clearance");
        let tr = &b.traces[0];
        assert!(seg_pad_gap(tr.points[0].pos, tr.points[1].pos, tr.width as f64 * 0.5, &b.pads[2]) >= 0.2,
            "foreign-pad fixture starts with legal member-trace clearance");
        let mut r = prepare(&b, &s);
        for _ in 0..4 {
            sweep(&mut r, &mut b);
            let p = b.vias[0].pos;
            let gap = seg_pad_gap(p, p, rv, &b.pads[2]);
            assert!(gap >= 0.2 - 1e-6, "guard keeps at least 0.2 mm foreign-pad copper clearance; gap={gap}");
            check_members(&b);
        }
        assert_eq!(r.eval_vias, 4, "foreign-pad fixture evaluates the via on all four sweeps");
    }

    // OrganicRelaxer.cpp:2927 pins either an explicitly fixed via or a via
    // with dynamic vias disabled, before viaGate can spend an evaluation.
    for (fixed, dynamic) in [(true, true), (false, false)] {
        let mut b = base.clone();
        b.vias[0].is_fixed = fixed;
        let settings = OrganicSettings { enable_dynamic_vias: dynamic, ..s.clone() };
        let mut r = prepare(&b, &settings);
        let before = bits(b.vias[0].pos);
        assert_ne!(r.c.via_pinned[0], 0, "fixed={fixed}, dynamic={dynamic} pins the via in topology");
        for _ in 0..4 {
            sweep(&mut r, &mut b);
            assert_eq!(bits(b.vias[0].pos), before, "fixed={fixed}, dynamic={dynamic} preserves the via position bits");
            check_members(&b);
        }
        assert_eq!(r.eval_vias, 0, "fixed={fixed}, dynamic={dynamic} skips every via evaluation");
    }

    // OrganicRelaxer.cpp:5127-5130 sets tk = 4 * kv and td0, but 1295 and
    // 1367-1372 define a SOFT one-sided tether energy, not a hard radius guard.
    // Thus the requested universal distance <= td0 property does not hold.
    // Starting at radius 1.2, the two trace springs pull towards radius 2;
    // their balance beyond td0 is 2 * (d - 2) + 4 * (d - td0) = 0.
    // Check that equilibrium and the smaller excursion than without a tether.
    {
        let mut b = base.clone();
        b.vias[0].pos = v(11.2, 10.0);
        b.vias[0].connected_pad = 0;
        b.traces[0].points[1].pos = b.vias[0].pos;
        b.traces[1].points[0].pos = b.vias[0].pos;
        let p = &b.pads[0];
        let td0 = std_max(p.size.x, p.size.y) as f64 * 0.5
            + b.vias[0].diameter as f64 * 0.5 + b.rules.min_clearance as f64 + 0.1;
        let a = p.center;
        assert!(b.vias[0].pos.distance_to(a) as f64 <= td0, "escape via starts inside its tether radius");
        let target_radius = (b.pads[1].center.x as f64 - a.x as f64) * 0.5;
        let equilibrium_radius = (2.0 * target_radius + 4.0 * td0) / 6.0;
        let mut untethered = b.clone();
        untethered.vias[0].connected_pad = -1;
        let mut r = prepare(&b, &s);
        let mut free = prepare(&untethered, &s);
        assert_eq!(r.c.via_orphan[0], 0, "escape via has a copper link to its pad through the layer-0 trace");
        for _ in 0..6 {
            sweep(&mut r, &mut b);
            sweep(&mut free, &mut untethered);
            check_members(&b);
            check_members(&untethered);
        }
        let distance = b.vias[0].pos.distance_to(a) as f64;
        assert!(distance > td0, "reference's soft tether permits spring-balanced motion beyond td0");
        assert!((distance - equilibrium_radius).abs() <= 2e-5,
            "escape via settles within the minimum-apply distance of the derived soft-tether equilibrium");
        assert!(distance < untethered.vias[0].pos.distance_to(a) as f64,
            "escape tether reduces outward motion compared with the same untethered via");
    }
}
