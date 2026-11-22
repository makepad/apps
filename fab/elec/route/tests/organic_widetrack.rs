use fab_elec_route::organic::{
    copper::{proj_seg, seg_pad_gap},
    geom::{std_max, Vec2},
    model::{ClearanceTable, DesignRules, LayerInfo, LayerIntent, Net, OBoard, Pad, PadShape, Trace, TracePoint},
    relaxer::OrganicRelaxer,
    settings::OrganicSettings,
};

#[test]
fn organic_widetrack_off_stiffness_snap_pull_and_determinism() {
    let v = Vec2::new;
    let bits = |p: Vec2| (p.x.to_bits(), p.y.to_bits());
    let trace_bits = |tr: &Trace| {
        (
            tr.points.iter().map(|p| bits(p.pos)).collect::<Vec<_>>(),
            tr.smoothed.iter().copied().map(&bits).collect::<Vec<_>>(),
        )
    };
    let same_boards = |a: &OBoard, b: &OBoard, property: &str| {
        assert_eq!(a.traces.len(), b.traces.len(), "{property}: trace counts agree");
        for (t, (ta, tb)) in a.traces.iter().zip(&b.traces).enumerate() {
            assert!(!ta.smoothed.is_empty(), "{property}: trace {t} has an exported curve");
            assert_eq!(trace_bits(ta), trace_bits(tb), "{property}: trace {t} points and smoothed agree bit for bit");
        }
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
    let mut fixture = OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        clr: ClearanceTable { rows: 1, layers: 2, mm: vec![0.2; 2], max: 0.2 },
        rules: DesignRules { default_trace_width: 0.25, ..DesignRules::default() },
        outline: vec![v(0.0, 0.0), v(50.0, 0.0), v(50.0, 30.0), v(0.0, 30.0)],
        nets: vec![Net { id: 0, ..Net::default() }, Net { id: 1, ..Net::default() }],
        ..OBoard::default()
    };
    // A gentle dogleg: the first three segments are about 38 degrees, the last
    // three about 31.5 degrees. The +/-1-segment chords of OrganicRelaxer.cpp:3378
    // select only the first stretch for the 45-degree snap. Snapping every segment
    // to one axis would rescale the isolated chain's normal energy uniformly and
    // would not change its straight-chord equilibrium with both ends fixed.
    let wide = [
        v(10.0, 5.0), v(10.7, 5.55), v(11.4, 6.1), v(12.1, 6.65),
        v(12.875, 7.125), v(13.65, 7.6), v(14.425, 8.075),
    ];
    let normal = [
        v(30.0, 24.0), v(30.75, 24.05), v(31.5, 24.1), v(32.25, 24.15),
        v(33.0, 24.1), v(33.75, 24.05), v(34.5, 24.0),
    ];
    for (t, positions) in [&wide, &normal].into_iter().enumerate() {
        let first_pad = fixture.pads.len() as i32;
        let width = fixture.rules.default_trace_width * if t == 0 { 3.0 } else { 1.0 };
        fixture.pads.push(pad(first_pad, t as i32, positions[0]));
        fixture.pads.push(pad(first_pad + 1, t as i32, positions[positions.len() - 1]));
        fixture.traces.push(Trace {
            id: t as i32,
            net: t as i32,
            layer: 0,
            width,
            is_routed: true,
            points: positions.iter().enumerate().map(|(k, &pos)| TracePoint {
                pos,
                width,
                is_fixed: k == 0 || k + 1 == positions.len(),
                terminal_pad: if k == 0 { first_pad } else if k + 1 == positions.len() { first_pad + 1 } else { -1 },
                ..TracePoint::default()
            }).collect(),
            ..Trace::default()
        });
    }
    // The requested strict straightening comparison needs a competing force:
    // OrganicRelaxer.cpp:3320-3321 scales all springs by the same factor (9 here),
    // 4800-4803 scales both gradient and Hessian, and 1724-1729 divides them again
    // (apart from 1e-6 regularisation). An isolated spring-only chain therefore has
    // no guaranteed reduction in turning. Add a small foreign pad in the wide
    // trace's soft margin; the chord itself stays legal. It also gives line pull
    // a nonzero bow to reduce instead of comparing two sleep-threshold residuals.
    let chord = wide[wide.len() - 1] - wide[0];
    let length = chord.length();
    let normal_to_chord = v(-chord.y / length, chord.x / length);
    let obstacle = (wide[0] + wide[wide.len() - 1]) * 0.5 - normal_to_chord * 0.9;
    let mut obstacle_pad = pad(fixture.pads.len() as i32, 1, obstacle);
    obstacle_pad.size = v(0.3, 0.3);
    let chord_gap = seg_pad_gap(wide[0], wide[wide.len() - 1], fixture.traces[0].width as f64 * 0.5, &obstacle_pad);
    assert!(chord_gap > 0.2 && chord_gap < (0.2f32 + OrganicSettings::base().clearance_margin) as f64,
        "wide-track fixture: the straight chord is legal but inside the foreign pad's soft margin");
    fixture.pads.push(obstacle_pad);
    let run = |input: &OBoard, settings: OrganicSettings, property: &str| {
        let mut board = input.clone();
        let mut relaxer = OrganicRelaxer::new(settings);
        // This test runs after the lead removes the three P3 entries in unported().
        for step in 1..=20 {
            assert_eq!(relaxer.step(&mut board, 1000), Ok(()), "{property}: step {step} succeeds");
        }
        assert_eq!(board.traces.len(), input.traces.len(), "{property}: both fixture traces survive");
        for (t, (tr, initial)) in board.traces.iter().zip(&input.traces).enumerate() {
            assert_eq!(tr.points.len(), initial.points.len(), "{property}: trace {t} stays inside the resampling band");
            assert_eq!(bits(tr.points[0].pos), bits(initial.points[0].pos), "{property}: trace {t} keeps its first pad end");
            assert_eq!(bits(tr.points.last().unwrap().pos), bits(initial.points.last().unwrap().pos),
                "{property}: trace {t} keeps its last pad end");
        }
        relaxer.update_splines(&mut board);
        board
    };
    let base = run(&fixture, OrganicSettings::base(), "base");
    let off = run(&fixture, OrganicSettings {
        wide_track_ratio: 1.2,
        wide_track_deviation: 0.2,
        ..OrganicSettings::base()
    }, "off");
    same_boards(&base, &off, "off parameters have no effect");

    let turning = |tr: &Trace| {
        tr.points.windows(3).map(|p| {
            let a = p[1].pos - p[0].pos;
            let b = p[2].pos - p[1].pos;
            let cross = a.x as f64 * b.y as f64 - a.y as f64 * b.x as f64;
            let dot = a.x as f64 * b.x as f64 + a.y as f64 * b.y as f64;
            cross.atan2(dot).abs()
        }).sum::<f64>()
    };
    let stiff = run(&fixture, OrganicSettings {
        wide_track_straightening: 4.0,
        ..OrganicSettings::base()
    }, "straightening");
    let base_turn = turning(&base.traces[0]);
    let stiff_turn = turning(&stiff.traces[0]);
    assert!(stiff_turn < base_turn,
        "straightening: the wide trace turns less against soft clearance, stiff={stiff_turn}, base={base_turn}");
    assert_eq!(trace_bits(&stiff.traces[1]), trace_bits(&base.traces[1]),
        "straightening: the far normal-width trace stays bit-identical");

    let angle_error = |tr: &Trace| {
        let d = tr.points[1].pos - tr.points[0].pos;
        ((d.y as f64).atan2(d.x as f64) - std::f64::consts::FRAC_PI_4).abs()
    };
    let initial_error = angle_error(&fixture.traces[0]);
    assert!(initial_error > 0.0 && initial_error < 10.0f64.to_radians(),
        "snap fixture: the first wide segment starts off-axis and within ten degrees of 45 degrees");
    let snap = run(&fixture, OrganicSettings {
        wide_track_snap_deg: 10.0,
        ..OrganicSettings::base()
    }, "snap");
    let snapped_error = angle_error(&snap.traces[0]);
    let base_error = angle_error(&base.traces[0]);
    assert!(snapped_error < base_error,
        "snap: the first wide segment ends closer to 45 degrees, snap={snapped_error}, base={base_error}");

    // OrganicRelaxer.cpp:3326-3372: split at anchors, then at the first worst
    // deviation strictly above tolerance; measure each interior node against
    // its own resulting chord. This fixture has no junction hosts or via pins.
    let stretch_distance = |tr: &Trace, tolerance: f32| {
        let seg_dist = |p: Vec2, a: Vec2, b: Vec2| {
            let (_, qx, qy) = proj_seg(p.x as f64, p.y as f64, a.x as f64, a.y as f64, b.x as f64, b.y as f64);
            ((p.x as f64 - qx) * (p.x as f64 - qx) + (p.y as f64 - qy) * (p.y as f64 - qy)).sqrt() as f32
        };
        let n = tr.points.len();
        let mut vtx = vec![false; n];
        vtx[0] = true;
        vtx[n - 1] = true;
        for k in 1..n - 1 {
            let p = &tr.points[k];
            vtx[k] = p.is_fixed || p.terminal_pad >= 0 || p.attached_via >= 0 || p.branch_trace >= 0
                || !p.pos.x.is_finite() || !p.pos.y.is_finite();
        }
        let mut stack = Vec::new();
        let mut a = 0;
        for b in 1..n {
            if vtx[b] {
                if b - a > 1 { stack.push((a, b)); }
                a = b;
            }
        }
        let tol = std_max(0.01f32, tolerance);
        while let Some((a, b)) = stack.pop() {
            let mut worst = -1.0f32;
            let mut wk = None;
            for k in a + 1..b {
                let d = seg_dist(tr.points[k].pos, tr.points[a].pos, tr.points[b].pos);
                if d > worst { worst = d; wk = Some(k); }
            }
            if let Some(k) = wk {
                if worst > tol {
                    vtx[k] = true;
                    if k - a > 1 { stack.push((a, k)); }
                    if b - k > 1 { stack.push((k, b)); }
                }
            }
        }
        let mut largest = 0.0f32;
        let mut stretches = Vec::new();
        let mut a = 0;
        for b in 1..n {
            if vtx[b] {
                stretches.push((a, b));
                for k in a + 1..b {
                    largest = std_max(largest, seg_dist(tr.points[k].pos, tr.points[a].pos, tr.points[b].pos));
                }
                a = b;
            }
        }
        (stretches, largest)
    };
    let pull_settings = OrganicSettings { wide_track_line_pull: 2.0, ..OrganicSettings::base() };
    let tol = pull_settings.wide_track_deviation;
    let (initial_stretches, initial_distance) = stretch_distance(&fixture.traces[0], tol);
    assert_eq!(initial_stretches, vec![(0, wide.len() - 1)], "line pull fixture: Douglas-Peucker keeps the two pad ends");
    assert!(initial_distance > 0.0 && initial_distance < tol, "line pull fixture: free nodes deviate within the stretch tolerance");
    let pull = run(&fixture, pull_settings, "line pull");
    let (base_stretches, base_distance) = stretch_distance(&base.traces[0], tol);
    let (pull_stretches, pull_distance) = stretch_distance(&pull.traces[0], tol);
    assert_eq!(base_stretches, initial_stretches, "line pull: baseline still uses the original chord stretch");
    assert_eq!(pull_stretches, initial_stretches, "line pull: pull still uses the original chord stretch");
    assert!(pull_distance < base_distance,
        "line pull: interior nodes end closer to their chord stretch, pull={pull_distance}, base={base_distance}");

    let all_terms = OrganicSettings {
        wide_track_straightening: 4.0,
        wide_track_snap_deg: 10.0,
        wide_track_line_pull: 2.0,
        ..OrganicSettings::base()
    };
    let first = run(&fixture, all_terms.clone(), "determinism first run");
    let second = run(&fixture, all_terms, "determinism second run");
    same_boards(&first, &second, "same wide-track settings twice");
}
