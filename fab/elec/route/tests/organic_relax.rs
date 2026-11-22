use fab_elec_route::organic::{
    copper::seg_pad_gap,
    geom::Vec2,
    model::*,
    relax::{
        state::{frozen_mask_of, global_hash, take_snapshot},
        Relaxation, Snapshot, K_QUIET,
    },
    settings::OrganicSettings,
};

#[test]
fn organic_relax_detects_changes_and_resamples() {
    let v = Vec2::new;
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
    let mut base = OBoard {
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
        ..OBoard::default()
    };
    // Two independent traces, each with three free interior nodes and two pad pins.
    for (net, y) in [(0, 10.0), (1, 20.0)] {
        let first_pad = base.pads.len() as i32;
        base.pads.push(pad(first_pad, net, v(10.0, y)));
        base.pads.push(pad(first_pad + 1, net, v(14.0, y)));
        base.traces.push(Trace {
            id: net,
            net,
            layer: 0,
            width: 0.25,
            is_routed: true,
            points: (0..5).map(|k| TracePoint {
                pos: v(10.0 + k as f32, y),
                width: 0.25,
                is_fixed: k == 0 || k == 4,
                terminal_pad: match k {
                    0 => first_pad,
                    4 => first_pad + 1,
                    _ => -1,
                },
                ..TracePoint::default()
            }).collect(),
            ..Trace::default()
        });
    }
    let set_positions = |b: &mut OBoard, positions: [Vec2; 5]| {
        for (p, pos) in b.traces[0].points.iter_mut().zip(positions) {
            p.pos = pos;
        }
        b.pads[0].center = positions[0];
        b.pads[1].center = positions[4];
    };
    let s = OrganicSettings::base();
    // The step driver's initialization, including the snapshot for a later call.
    let prepare = |b: &OBoard| {
        let mut r = Relaxation::new(b, &s);
        let gh = global_hash(b, &s);
        let x = r.apply_external(b, &Snapshot::default(), false, gh);
        r.c.frozen = frozen_mask_of(b, &[], &s);
        r.spring_weights(b);
        r.compute_trace_boxes(b);
        let mut sn = Snapshot::default();
        take_snapshot(&mut sn, b, gh);
        (r, sn, x)
    };

    // OrganicRelaxer.cpp:3925-3935: an invalid snapshot rebuilds and resetAll wakes
    // the movable nodes. There are exactly six here; pad pins must not count.
    {
        let b = base.clone();
        let (r, _, x) = prepare(&b);
        assert!(x.full && x.topo && x.any, "first external update reports a full topology rebuild");
        assert_eq!(r.awake_count(&b), 6, "full rebuild wakes all six free interior nodes");
        for (t, tr) in b.traces.iter().enumerate() {
            for k in 1..tr.points.len() - 1 {
                assert!(r.c.quiet[t][k] < K_QUIET, "full rebuild wakes trace {t} interior node {k}");
            }
        }
    }

    // OrganicRelaxer.cpp:3948-3949, 4018-4041, 4112: equal inputs add no changes.
    {
        let b = base.clone();
        let (mut r, sn, _) = prepare(&b);
        let x = r.apply_external(&b, &sn, true, global_hash(&b, &s));
        assert!(!x.any && !x.full && !x.topo, "unchanged snapshot reports no external change or rebuild");
        assert_eq!(x.changed_nodes, 0, "unchanged snapshot reports zero changed nodes");
    }

    // OrganicRelaxer.cpp:4022-4051, 4112: position-only edits wake and count the
    // edited node without invalidating topology. Start it asleep to exercise wake.
    {
        let mut b = base.clone();
        let (mut r, sn, _) = prepare(&b);
        r.set_quiet(0, 2, K_QUIET);
        assert_eq!(r.c.quiet[0][2], K_QUIET, "motion fixture starts with the edited node asleep");
        b.traces[0].points[2].pos.y += 0.1;
        let x = r.apply_external(&b, &sn, true, global_hash(&b, &s));
        assert!(x.any && !x.full && !x.topo, "a 0.1 mm node edit changes geometry without rebuilding topology");
        assert_eq!(x.changed_nodes, 1, "a single interior-node edit reports exactly one changed node");
        assert!(r.c.quiet[0][2] < K_QUIET, "external motion wakes the previously sleeping node");
    }

    // Each resample uses a fresh cache: OrganicRelaxer.cpp:5788, 5793-5798 and
    // 5900-5902 apply backoff and widen hysteresis on later passes. No branches,
    // frozen traces, diff pairs or exit stubs are present in these fixtures.
    // OrganicRelaxer.cpp:5853-5879: 6 / 1.2 gives five pieces, hence four new nodes;
    // the other segments are 1 mm, and none is short enough to trigger a merge.
    {
        let mut b = base.clone();
        set_positions(&mut b, [v(10.0, 10.0), v(11.0, 10.0), v(17.0, 10.0), v(18.0, 10.0), v(19.0, 10.0)]);
        let old = b.traces[0].points.clone();
        let (mut r, _, _) = prepare(&b);
        r.c.sub += 1;
        r.sub_index = r.c.sub;
        let changed = r.resample(&mut b, 1);
        let pts = &b.traces[0].points;
        assert!(changed, "splitting a 6 mm segment reports resampling changed the board");
        assert_eq!(pts.len(), old.len() + 4, "6 mm at 1.2 mm spacing adds ceil(6 / 1.2) - 1 nodes");
        let chord = old[2].pos - old[1].pos;
        for (k, p) in pts.iter().enumerate().take(6).skip(2) {
            let offset = p.pos - old[1].pos;
            let along = offset.dot(chord) / chord.length_squared();
            let distance = offset.cross(chord).abs() / chord.length();
            assert!(distance <= 1e-5 && along > 0.0 && along < 1.0,
                "new split node {k} lies on the open old chord within 1e-5 mm");
            let expected = v(11.0 + 1.2 * (k - 1) as f32, 10.0);
            assert!(p.pos.distance_to(expected) <= 1e-5, "new split node {k} divides the chord into equal 1.2 mm pieces");
        }
        assert!(pts.windows(2).all(|w| w[0].pos.x < w[1].pos.x), "splitting preserves node order along the trace");
        for (before, after) in [0, 1, 6, 7, 8].into_iter().enumerate() {
            assert_eq!((pts[after].pos.x, pts[after].pos.y), (old[before].pos.x, old[before].pos.y),
                "splitting preserves original node {before} at its ordered position");
            assert_eq!((pts[after].terminal_pad, pts[after].is_fixed), (old[before].terminal_pad, old[before].is_fixed),
                "splitting preserves original node {before}'s pad attachment and pin state");
        }
    }

    // OrganicRelaxer.cpp:5803-5811: only k = 2 is eligible. Its short side is
    // 0.05 < 0.45 * 1.2 = 0.54 mm, and its replacement chord is sqrt(1 + 0.05^2)
    // < 0.9 * 1.2 = 1.08 mm. The other two skip-over chords exceed 1.08 mm.
    // All segments, including the replacement, stay below the split threshold;
    // there is no second merge candidate within four nodes (or anywhere else).
    let short_pair = [v(19.95, 8.9), v(19.95, 10.0), v(20.0, 10.0), v(20.0, 11.0), v(20.0, 12.1)];
    // OrganicRelaxer.cpp:5831-5853, 5892-5916: a safe replacement drops that node.
    {
        let mut b = base.clone();
        set_positions(&mut b, short_pair);
        let old = b.traces[0].points.clone();
        let (mut r, _, _) = prepare(&b);
        r.c.sub += 1;
        r.sub_index = r.c.sub;
        let changed = r.resample(&mut b, 1);
        let pts = &b.traces[0].points;
        assert!(changed, "a safe short-pair merge reports resampling changed the board");
        assert_eq!(pts.len(), old.len() - 1, "a safe 0.05 mm short pair removes exactly one interior node");
        for (after, before) in [0, 1, 3, 4].into_iter().enumerate() {
            assert_eq!((pts[after].pos.x, pts[after].pos.y), (old[before].pos.x, old[before].pos.y),
                "safe merge removes node 2 and keeps survivor {before} in order");
        }
    }

    // OrganicRelaxer.cpp:5835-5839, 5881-5890: guardReplace rejects the merge;
    // a rejected merge alone changes no points (it may be retried later).
    {
        let mut b = base.clone();
        set_positions(&mut b, short_pair);
        b.pads.push(pad(4, 1, v(19.16, 10.5)));
        let old = b.traces[0].points.clone();
        let obstacle = &b.pads[4];
        // The circle radius is 0.5 and trace half width is 0.125 mm. The old
        // vertical leg has gap 0.84 - 0.5 - 0.125 = 0.215 mm. The new chord's
        // gap is (0.79 + 0.05 * 0.5) / sqrt(1 + 0.05^2) - 0.625 ~= 0.189 mm.
        // Thus the old route clears both 0.2 mm and the guard's added 0.002 mm
        // slack; the replacement violates even the uninflated clearance.
        let old_gap = old.windows(2).map(|w| seg_pad_gap(w[0].pos, w[1].pos, 0.125, obstacle))
            .fold(f64::INFINITY, f64::min);
        let chord_gap = seg_pad_gap(old[1].pos, old[3].pos, 0.125, obstacle);
        let expected_chord_gap = 0.815 / (1.0f64 + 0.05 * 0.05).sqrt() - 0.625;
        assert!((old_gap - 0.215).abs() < 1e-5, "merge guard fixture's original route has the geometric 0.215 mm gap");
        assert!((chord_gap - expected_chord_gap).abs() < 1e-5 && chord_gap < 0.2,
            "merge guard fixture's proposed chord enters the foreign pad's clearance");
        let (mut r, _, _) = prepare(&b);
        assert!(old_gap > r.e.clr_g, "merge guard fixture starts outside the guard's clearance including slack");
        r.c.sub += 1;
        r.sub_index = r.c.sub;
        let changed = r.resample(&mut b, 1);
        let pts = &b.traces[0].points;
        assert!(!changed, "a foreign pad blocks the only eligible merge without other resampling changes");
        assert_eq!(pts.len(), old.len(), "foreign-pad clearance guard preserves the trace's point count");
        for (k, p) in pts.iter().enumerate() {
            assert_eq!((p.pos.x, p.pos.y), (old[k].pos.x, old[k].pos.y),
                "rejected short-pair merge leaves node {k} at its original position");
        }
    }
}
