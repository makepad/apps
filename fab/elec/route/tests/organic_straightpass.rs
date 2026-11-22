use fab_elec_route::organic::{
    copper::seg_pad_gap,
    geom::Vec2,
    model::{ClearanceTable, DesignRules, LayerInfo, LayerIntent, Net, OBoard, Pad, PadShape, Trace, TracePoint},
    relax::Relaxation,
    relaxer::OrganicRelaxer,
    settings::OrganicSettings,
};

#[test]
fn organic_straightpass_preserves_off_short_runs_edits_and_determinism() {
    let v = Vec2::new;
    let bits = |p: Vec2| (p.x.to_bits(), p.y.to_bits());
    let trace_bits = |b: &OBoard| {
        b.traces.iter().map(|tr| {
            (
                (tr.id, tr.net, tr.layer, tr.width.to_bits(), tr.is_routed, tr.locked, tr.imported, tr.pair_frozen, tr.rule),
                tr.points.iter().map(|p| {
                    (bits(p.pos), bits(p.velocity), p.width.to_bits(), p.is_fixed, p.terminal_pad,
                        p.attached_via, p.branch_trace, p.branch_point, p.straight_before)
                }).collect::<Vec<_>>(),
                tr.smoothed.iter().copied().map(&bits).collect::<Vec<_>>(),
                tr.point_widths.iter().map(|w| w.to_bits()).collect::<Vec<_>>(),
                tr.length.to_bits(),
            )
        }).collect::<Vec<_>>()
    };
    let pad = |id, net, center| Pad {
        id,
        net,
        center,
        size: v(1.0, 1.0),
        shape: PadShape::Circle,
        layer_mask: 1,
        is_smd: true,
        drill_diameter: 0.0,
        ..Pad::default()
    };
    let fixture = |segments: usize| {
        let end_x = 10.0 + 0.75 * segments as f32;
        OBoard {
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
            pads: vec![pad(0, 0, v(10.0, 10.0)), pad(1, 0, v(end_x, 10.0)), pad(2, 1, v((10.0 + end_x) * 0.5, 10.95))],
            traces: vec![Trace {
                id: 0,
                net: 0,
                layer: 0,
                width: 0.25,
                is_routed: true,
                points: (0..=segments).map(|k| TracePoint {
                    pos: v(10.0 + 0.75 * k as f32, 10.0),
                    width: 0.25,
                    is_fixed: k == 0 || k == segments,
                    terminal_pad: if k == 0 { 0 } else if k == segments { 1 } else { -1 },
                    ..TracePoint::default()
                }).collect(),
                ..Trace::default()
            }],
            ..OBoard::default()
        }
    };
    let base = OrganicSettings::base();
    let off = OrganicSettings { straight_pass_relief: 0.0, straight_taper: 1.0, straight_min_len: 2.0, ..base.clone() };
    let on = OrganicSettings { straight_pass_relief: 0.8, ..base.clone() };
    let long = fixture(8); // 6 mm, eight 0.75 mm segments, seven movable interior nodes.
    let short = fixture(3); // 2.25 mm: below straight_min_len, still two movable interior nodes.
    let tr = &long.traces[0];
    let gap = seg_pad_gap(tr.points[0].pos, tr.points.last().unwrap().pos, tr.width as f64 * 0.5, &long.pads[2]);
    let hard = long.rules.min_clearance as f64 + base.guard_clearance_slack as f64;
    assert!(gap > hard && gap < hard + base.clearance_margin as f64,
        "relief fixture: the foreign pad is legal but inside the soft margin, gap={gap}");
    assert!(tr.points[0].pos.distance_to(tr.points.last().unwrap().pos) > on.straight_min_len,
        "relief fixture: the long run exceeds straight_min_len");
    assert!(short.traces[0].points[0].pos.distance_to(short.traces[0].points.last().unwrap().pos) < on.straight_min_len,
        "short-run fixture: the chord is below straight_min_len");

    let run = |input: &OBoard, settings: &OrganicSettings, property: &str| {
        let mut b = input.clone();
        let mut r = OrganicRelaxer::new(settings.clone());
        for step in 1..=20 {
            assert_eq!(r.step(&mut b, 1000), Ok(()), "{property}: step {step} succeeds");
        }
        r.update_splines(&mut b);
        assert!(!b.traces[0].smoothed.is_empty(), "{property}: exported points are present for bit comparison");
        b
    };
    let without = run(&long, &base, "off baseline");
    let explicit_off = run(&long, &off, "explicit off");
    assert_eq!(trace_bits(&without), trace_bits(&explicit_off),
        "off is bit-identical: changing inactive taper/minimum preserves every trace's points and smoothed output");

    let relieved = run(&long, &on, "relief");
    let deviation = |b: &OBoard| {
        let pts = &b.traces[0].points;
        pts[1..pts.len() - 1].iter().map(|p| (p.pos.y - 10.0).abs()).fold(0.0f32, f32::max)
    };
    let base_bow = deviation(&without);
    let relief_bow = deviation(&relieved);
    assert!(base_bow > 0.0, "relief fixture: soft clearance bows the unrelieved straight run");
    assert!(relief_bow < base_bow,
        "relief keeps the run straighter: largest original-line distance is {relief_bow}, versus {base_bow} without relief");
    for (name, b) in [("baseline", &without), ("relief", &relieved)] {
        assert_eq!(bits(b.traces[0].points[0].pos), bits(tr.points[0].pos), "{name}: first pad end stays fixed");
        assert_eq!(bits(b.traces[0].points.last().unwrap().pos), bits(tr.points.last().unwrap().pos),
            "{name}: last pad end stays fixed");
    }
    let repeated = run(&long, &on, "repeated relief");
    assert_eq!(trace_bits(&relieved), trace_bits(&repeated),
        "same settings twice: every trace's points and smoothed output are bit-identical");

    let short_off = run(&short, &base, "short run without relief");
    let short_on = run(&short, &on, "short run with relief");
    assert_eq!(trace_bits(&short_off), trace_bits(&short_on),
        "short run is not remembered: relief leaves every trace's points and smoothed output bit-identical");
    // OrganicRelaxer.cpp:3135 still creates a map entry for a short eligible trace;
    // 3111 stores no lines. 'Not remembered' means no remembered stretches, not no entry.
    let mut short_cache = Relaxation::new(&short, &on);
    short_cache.build_topology(&short);
    short_cache.straight_prepare(&short);
    assert!(short_cache.c.straight_mem[&0].lines.is_empty() && short_cache.c.seg_pass[0].is_empty(),
        "short run: the cache has no remembered stretch or segment relief");

    // Warm both drivers with the foreign pad far away so their boards and node sleep
    // histories agree, while the relief driver remembers the 6 mm line. Its private
    // cache is checked behaviorally below; the public Relaxation cache also exposes
    // the preparation/seen hooks for a direct check of the remembered geometry.
    let mut edited_off = long.clone();
    edited_off.pads[2].center.y = 20.0;
    let mut edited_on = edited_off.clone();
    let mut roff = OrganicRelaxer::new(base.clone());
    let mut ron = OrganicRelaxer::new(on.clone());
    let mut cache = Relaxation::new(&edited_on, &on);
    cache.build_topology(&edited_on);
    cache.straight_prepare(&edited_on);
    assert_eq!(cache.c.straight_mem[&0].lines.len(), 1, "external edit: initial memory contains one straight run");
    let line = cache.c.straight_mem[&0].lines[0];
    assert_eq!((bits(line.a), bits(line.u), line.l.to_bits()), (bits(v(10.0, 10.0)), bits(v(1.0, 0.0)), 6.0f32.to_bits()),
        "external edit: initial memory describes the original 6 mm line");
    assert_eq!(roff.step(&mut edited_off, 1000), Ok(()), "external edit: no-relief warmup succeeds");
    assert_eq!(ron.step(&mut edited_on, 1000), Ok(()), "external edit: relief warmup succeeds");
    roff.update_splines(&mut edited_off);
    ron.update_splines(&mut edited_on);
    assert_eq!(trace_bits(&edited_off), trace_bits(&edited_on), "external edit: warmup histories leave identical boards");
    cache.straight_seen(&edited_on);
    let seen = cache.c.straight_mem[&0].seen;

    // OrganicRelaxer.cpp:3137 detects edits by polyline length/endpoints, not a
    // pointwise comparison. Moving the centre 0.25 mm increases length by >0.001 mm.
    // The new maximal straight stretches are 2.25 mm at either end and two shorter
    // sloped segments: none reaches the 2.5 mm minimum (3083-3112).
    for b in [&mut edited_off, &mut edited_on] {
        b.traces[0].points[4].pos.y -= 0.25;
        b.pads[2].center.y = 10.95;
    }
    let edited_len = edited_on.traces[0].points.windows(2)
        .map(|p| p[0].pos.distance_to(p[1].pos) as f64).sum::<f64>() as f32;
    assert!((edited_len - seen).abs() > 1e-3f32, "external edit: the manual node move crosses the reference's length threshold");
    cache.straight_prepare(&edited_on);
    assert!(cache.c.straight_mem[&0].lines.is_empty() && cache.c.seg_pass[0].is_empty(),
        "external edit: memory is rebuilt from the bent polyline, with no qualifying stretches");
    for step in 1..=20 {
        assert_eq!(roff.step(&mut edited_off, 1000), Ok(()), "external edit: baseline step {step} succeeds");
        assert_eq!(ron.step(&mut edited_on, 1000), Ok(()), "external edit: relief step {step} succeeds");
        if step == 1 {
            assert_eq!(ron.last_step_info().changed_nodes, 1, "external edit: the driver detects the one manually moved node");
            assert!(ron.last_step_info().external, "external edit: the driver reports external input");
        }
        // A stale original line would still relieve the pad; a rebuilt empty memory
        // must follow the no-relief driver exactly, even as its own sweeps straighten
        // the edit again. straightSeen (3173-3188) must not call that motion an edit.
        roff.update_splines(&mut edited_off);
        ron.update_splines(&mut edited_on);
        assert_eq!(trace_bits(&edited_off), trace_bits(&edited_on),
            "external edit: step {step} matches the no-relief board bit for bit after rebuilding memory");
    }
}
