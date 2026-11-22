use fab_elec_route::organic::{
    copper,
    engine::{Engine, KoEdge, Ob, ObKind, PairClass, PairEval, Prim},
    geom::Vec2,
    model::{ClearanceTable, DesignRules, LayerInfo, Net, OBoard, Pad, PadShape, Trace, TracePoint, Via},
    settings::OrganicSettings,
};

#[test]
fn pair_geometry_clearances_and_energies() {
    let v = Vec2::new;
    let check = |property: &str, actual: f64, expected: f64| {
        assert!((actual - expected).abs() < 1e-10, "{property}: {actual} vs {expected}");
    };
    let b = OBoard {
        layers: vec![
            LayerInfo { index: 0, plane_net: -1, ..LayerInfo::default() },
            LayerInfo { index: 1, plane_net: -1, ..LayerInfo::default() },
        ],
        nets: vec![Net { id: 0, ..Net::default() }, Net { id: 1, ..Net::default() }],
        rules: DesignRules {
            min_clearance: 0.2,
            board_edge_clearance: 0.3,
            hole_clearance: 0.25,
            hole_to_hole: 0.25,
            ..DesignRules::default()
        },
        clr: ClearanceTable { rows: 2, layers: 2, mm: vec![0.2, 0.2, 0.3, 0.3, 0.3, 0.3, 0.3, 0.3], max: 0.3 },
        traces: [0.0, 0.5].into_iter().enumerate().map(|(i, y)| Trace {
            id: i as i32,
            net: i as i32,
            layer: 0,
            width: 0.25,
            rule: i as u16,
            points: vec![
                TracePoint { pos: v(0.0, y), ..TracePoint::default() },
                TracePoint { pos: v(2.0, y), ..TracePoint::default() },
            ],
            ..Trace::default()
        }).collect(),
        pads: vec![
            Pad {
                id: 0, center: v(0.0, 0.0), size: v(2.0, 1.0), shape: PadShape::Rect,
                net: 1, rule: 1, is_smd: true, drill_diameter: 0.0, layer_mask: 1,
                ..Pad::default()
            },
            Pad {
                id: 1, center: v(0.0, 0.0), size: v(2.0, 1.0), shape: PadShape::Rect,
                net: 1, rule: 1, is_smd: false, drill_diameter: 1.0, layer_mask: 3,
                ..Pad::default()
            },
        ],
        vias: vec![
            Via { id: 0, net: 0, pos: v(0.0, 0.0), diameter: 0.5, drill: 0.5, layer_start: 1, layer_end: 0, ..Via::default() },
            Via { id: 1, net: 1, pos: v(1.0, 0.0), diameter: 0.5, drill: 0.5, rule: 1, ..Via::default() },
        ],
        ..OBoard::default()
    };
    let mut engine = Engine::new(&b, &OrganicSettings::default());
    engine.c.pad_m = b.pads.iter().map(copper::pad_model).collect();
    engine.c.seg_trace = vec![1];
    engine.c.seg_idx = vec![0];
    engine.c.jgroup = vec![-1, -1];
    engine.c.board_center = v(2.0, 1.0);
    engine.c.ko_edges = vec![KoEdge {
        a: v(0.0, 0.5), b: v(2.0, 0.5), track_layers: 1, via_layers: 2,
        thr: 0.3, ..KoEdge::default()
    }];
    engine.c.cy_nets = vec![vec![1]];

    // Decimal rules/settings are stored as floats; the geometric lengths below are exact.
    let clr_rule = 0.2_f32 as f64;
    let shift = 0.3_f32 as f64 - clr_rule;
    let slack = 0.002_f32 as f64;
    let clr_g = clr_rule + slack;
    let margin = 0.35_f32 as f64;
    let margin_wide = margin * 1.5;
    let edge_g = 0.3_f32 as f64 + slack;
    let p = Prim {
        a: v(0.0, 0.0), b: v(2.0, 0.0), h: 0.125, layer: 0, net: 0, trace: 0,
        ex_lo: 0, ex_hi: 0, rule: 0, ..Prim::default()
    };
    let seg = Ob {
        kind: ObKind::Seg, id: 0, a: v(0.0, 0.5), b: v(2.0, 0.5),
        x0: -0.125, y0: 0.375, x1: 2.125, y1: 0.625,
        h: 0.125, layer: 0, net: 1, trace: 1, seg: 0, rule: 1, ..Ob::default()
    };
    let mut e = PairEval::default();
    assert_eq!(engine.eval_pair(&b, &p, &seg, &mut e), PairClass::Eval, "foreign same-layer segment is evaluated");
    check("segment gap subtracts both half widths and rule shift", e.gap, 0.5 - 0.125 - 0.125 - shift);
    check("segment threshold", e.thr, clr_g);
    check("segment soft target", e.target, clr_g + margin);
    check("segment wide target", e.target_wide, clr_g + margin_wide);
    check("segment separating x", e.vx, 0.0);
    check("segment separating y", e.vy, -0.5);
    assert!(e.repel && !e.partner, "foreign nonpartner segment repels");
    assert_eq!(engine.eval_pair(&b, &p, &Ob { layer: 1, ..seg }, &mut e), PairClass::Na, "other-layer segment is inapplicable");
    assert!(!e.repel && !e.partner && e.gap == copper::K_BIG, "inapplicable pair clears the previous evaluation");
    let adjacent = Prim { ex_lo: 2, ex_hi: 3, ..p };
    for s in [2, 3] {
        assert_eq!(engine.eval_pair(&b, &adjacent, &Ob { trace: 0, net: 0, seg: s, ..seg }, &mut e), PairClass::Na,
            "own-trace exclusion includes segment {s} at either endpoint");
    }
    let far = Ob { a: v(0.0, 20.0), b: v(2.0, 20.0), y0: 19.875, y1: 20.125, ..seg };
    assert_eq!(engine.eval_pair(&b, &p, &far, &mut e), PairClass::Far, "distant foreign segment is pruned");
    let same = Ob { net: 0, a: v(0.0, 0.125), b: v(2.0, 0.125), y0: 0.0, y1: 0.25, ..seg };
    assert_eq!(engine.eval_pair(&b, &p, &same, &mut e), PairClass::Eval, "unjoined same-net segment keeps a centre-line guard");
    check("same-net segment uses centre distance", e.gap, 0.125);
    check("same-net segment threshold", e.thr, 0.005);
    assert!(!e.repel, "same-net segment has no soft repulsion");
    engine.c.jgroup[0] = 0;
    assert_eq!(engine.eval_pair(&b, &p, &same, &mut e), PairClass::Na, "junction-connected trace may slide over same-net copper");
    engine.c.jgroup[0] = -1;

    // Both pads have half extents (1, 0.5). The THT drill has radius 0.5.
    let pad = Ob { kind: ObKind::Pad, id: 0, net: 1, rule: 0, x0: -1.0, y0: -0.5, x1: 1.0, y1: 0.5, ..Ob::default() };
    let above = Prim { a: v(-0.5, 1.0), b: v(0.5, 1.0), ..p };
    assert_eq!(engine.eval_pair(&b, &above, &pad, &mut e), PairClass::Eval, "foreign SMD rectangle is evaluated");
    check("SMD gap uses the pad rule rather than the obstacle row", e.gap, 1.0 - 0.5 - 0.125 - shift);
    check("pad threshold", e.thr, clr_g);
    check("pad soft target", e.target, clr_g + margin);
    assert!(e.repel, "foreign pad repels");
    assert_eq!(engine.eval_pair(&b, &Prim { layer: 1, ..above }, &pad, &mut e), PairClass::Na, "SMD pad excludes the other layer");
    let tht = Ob { id: 1, ..pad };
    let side = Prim { a: v(1.5, -0.25), b: v(1.5, 0.25), ..p };
    assert_eq!(engine.eval_pair(&b, &side, &tht, &mut e), PairClass::Eval, "THT copper side is evaluated");
    check("THT copper wins away from drill", e.gap, 1.5 - 1.0 - 0.125 - shift);
    assert_eq!(engine.eval_pair(&b, &Prim { layer: 1, ..above }, &tht, &mut e), PairClass::Eval, "THT drill applies on the second layer");
    check("THT hole wall wins over copper", e.gap, 1.0 - (0.125 + 0.5 + 0.25) + clr_rule - shift);
    check("THT hole uses its closest point on the segment", e.t, 0.5);
    check("THT hole separating vector", e.vy, 1.0);
    check("point-to-pad copper distance", engine.point_pad_dist(&b, v(0.0, 1.0), 0), 0.5);

    let disc = Prim { a: v(0.0, 0.0), b: v(0.0, 0.0), h: 0.25, drill: 0.5, disc: true,
        layer: -1, trace: -1, via_id: 0, net: 0, rule: 0, ..Prim::default() };
    let via = Ob { kind: ObKind::Via, id: 1, a: v(1.0, 0.0), b: v(1.0, 0.0),
        x0: 0.75, y0: -0.25, x1: 1.25, y1: 0.25, h: 0.25, drill: 0.5, net: 1, rule: 1, ..Ob::default() };
    // At centre distance 1: requirements are respectively 1 + clr_rule, 1, 1, 0.75.
    // The last synthetic case has copper inside its drills to isolate hole-to-hole dominance.
    for (name, ra, dra, rb, drb, req) in [
        ("copper", 0.5, 0.5, 0.5, 0.5, 1.0 + clr_rule),
        ("first hole to copper", 0.25, 0.5, 0.5, 0.5, 1.0),
        ("second hole to copper", 0.5, 0.5, 0.25, 0.5, 1.0),
        ("hole to hole", 0.125, 0.5, 0.125, 0.5, 0.75),
    ] {
        check(&format!("via gap folds {name}"), engine.via_via_gap(1.0, ra, dra, rb, drb), 1.0 - req + clr_rule);
        let moving = Prim { h: ra, drill: dra, ..disc };
        let other = Ob { h: rb as f32, drill: drb as f32, x0: 1.0 - rb as f32, y0: -rb as f32,
            x1: 1.0 + rb as f32, y1: rb as f32, ..via };
        assert_eq!(engine.eval_pair(&b, &moving, &other, &mut e), PairClass::Eval, "foreign via evaluates {name}");
        check(&format!("foreign via shifts {name}"), e.gap, 1.0 - req + clr_rule - shift);
        check("foreign via threshold", e.thr, clr_g);
        check("foreign via soft target includes extra margin", e.target, clr_g + (margin + 0.1));
        assert!(e.repel, "foreign via repels for {name}");
    }
    let same_via = Ob { net: 0, a: v(0.625, 0.0), b: v(0.625, 0.0), x0: 0.375, x1: 0.875, ..via };
    assert_eq!(engine.eval_pair(&b, &disc, &same_via, &mut e), PairClass::Eval, "same-net via drill is evaluated");
    check("same-net via gap is between drill walls", e.gap, 0.625 - 0.5);
    check("same-net via threshold is hole clearance plus slack", e.thr, 0.25 + slack);
    check("same-net via target is hard threshold", e.target, 0.25 + slack);
    check("same-net via point parameter", e.t, 1.0);
    assert!(e.repel, "same-net drill violation repels with via-extra enabled");
    let safe_via = Ob { a: v(0.875, 0.0), b: v(0.875, 0.0), x0: 0.625, x1: 1.125, ..same_via };
    assert_eq!(engine.eval_pair(&b, &disc, &safe_via, &mut e), PairClass::Eval, "nearby safe same-net via is evaluated");
    assert!(!e.repel, "safe same-net drill has no repulsion");
    assert_eq!(engine.eval_pair(&b, &disc, &Ob { id: 0, ..via }, &mut e), PairClass::Na, "via ignores itself");
    assert_eq!(engine.eval_pair(&b, &p, &same_via, &mut e), PairClass::Na, "trace may touch same-net via");

    let edge = Ob { kind: ObKind::Edge, a: v(0.0, 0.5), b: v(2.0, 0.5),
        x0: 0.0, y0: 0.5, x1: 2.0, y1: 0.5, ..Ob::default() };
    assert_eq!(engine.eval_pair(&b, &p, &edge, &mut e), PairClass::Eval, "board edge is evaluated");
    check("edge gap subtracts only moving half width", e.gap, 0.5 - 0.125);
    check("edge threshold", e.thr, edge_g);
    check("edge target uses half the margin", e.target, edge_g + margin / 2.0);
    check("edge wide target equals target", e.target_wide, edge_g + margin / 2.0);
    assert!(e.repel, "board edge repels");

    let ko = Ob { kind: ObKind::Ko, id: 0, ..edge };
    assert_eq!(engine.eval_pair(&b, &p, &ko, &mut e), PairClass::Eval, "keepout edge is evaluated");
    check("keepout geometric gap", e.gap, 0.5 - 0.125);
    check("keepout has a hard target", e.target, 0.3);
    check("keepout has no wide soft margin", e.target_wide, 0.3);
    assert_eq!(engine.eval_pair(&b, &Prim { layer: 1, ..p }, &ko, &mut e), PairClass::Na, "keepout track mask excludes other layers");
    engine.c.ko_edges[0].courtyard = 0;
    assert_eq!(engine.eval_pair(&b, &p, &ko, &mut e), PairClass::Na, "courtyard excludes trace primitives");
    assert_eq!(engine.eval_pair(&b, &disc, &ko, &mut e), PairClass::Eval, "reversed via span meets courtyard mask");
    assert_eq!(engine.eval_pair(&b, &Prim { net: 1, ..disc }, &ko, &mut e), PairClass::Na, "courtyard exempts a footprint pad net");

    let normal = engine.fallback_dir(&b, &p, &seg);
    assert!(normal.x == 0.0 && normal.y == 1.0, "segment fallback uses cached trace geometry and trace order");
    let inward = engine.fallback_dir(&b, &p, &edge);
    assert!(inward.x == 0.0 && inward.y == 1.0, "edge fallback points to board centre");

    for (name, g, expected) in [("outside", 3.0, 0.0), ("wide only", 1.5, 0.0625), ("both bands", 0.0, 2.0)] {
        check(&format!("penetration energy {name}"), Engine::pen_energy(g, 1.0, 2.0, 2.0), expected);
    }
    for (name, d, repel_only, expected) in [
        ("at target", 1.0, false, 0.0), ("too close", 0.5, false, 0.25),
        ("too far", 2.0, false, 1.0), ("clamped extension", 5.0, false, 4.0),
        ("repel-only far", 2.0, true, 0.0), ("repel-only close", 0.5, true, 0.25),
    ] {
        check(&format!("coupling energy {name}"), Engine::cpl_energy(d, 1.0, 2.0, repel_only), expected);
    }
    let box_prim = Prim { a: v(2.0, 2.0), b: v(0.0, 0.0), ..p };
    let overlap = Ob { x0: 1.0, y0: 1.0, x1: 3.0, y1: 3.0, ..Ob::default() };
    let separated = Ob { x0: 5.0, y0: 6.0, x1: 7.0, y1: 8.0, ..Ob::default() };
    check("overlapping bounding boxes have zero gap", Engine::bbox_gap2(&box_prim, &overlap), 0.0);
    check("separated bounding boxes have squared 3-4 gap", Engine::bbox_gap2(&box_prim, &separated), 25.0);
    check("bounding box gap is the square root", Engine::bbox_gap(&box_prim, &separated), 5.0);
    let crossing = Ob { a: v(1.0, -1.0), b: v(1.0, 1.0), ..seg };
    check("parallel segments have no perpendicular weight", Engine::perp_weight(p.a, p.b, &seg), 0.0);
    check("interior crossing has full perpendicular weight", Engine::perp_weight(p.a, p.b, &crossing), 1.0);
    check("crossing at an endpoint has half weight", Engine::perp_weight(v(1.0, 0.0), p.b, &crossing), 0.5);
    let (c, gx, gy) = Engine::perp_cos(p.a, p.b, &crossing).expect("nondegenerate crossing has a cosine gradient");
    check("right-angle cosine", c, 0.0);
    check("right-angle cosine x gradient", gx, 0.0);
    check("right-angle cosine y gradient", gy, 0.5);
    assert!(Engine::perp_cos(p.a, p.a, &crossing).is_none(), "zero-length segment has no cosine gradient");
}
