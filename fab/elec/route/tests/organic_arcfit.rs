use fab_elec_route::organic::{
    arcfit::{
        board_primitives, circle_from_3, deviation, distance_to, fit_arcs, fit_octilinear,
        fit_octilinear_legs, segments_of, tessellate, ArcFitOptions, ArcFitStats, TrackPrim, TrackStyle,
    },
    biarc::BiarcFitter,
    geom::Vec2,
    model::{ClearanceTable, LayerInfo, LayerIntent, Net, OBoard, Pad, PadShape, Trace, TracePoint},
};

#[test]
fn organic_arcfit_preserves_geometry_junctions_and_guard_levels() {
    let v = Vec2::new;
    let opt = ArcFitOptions::default();
    let width = 0.25f32;
    let pi = std::f64::consts::PI;
    let circle_samples = |radius: f64, sweep: f64, steps: usize| -> Vec<Vec2> {
        (0..=steps).map(|i| {
            let angle = sweep * i as f64 / steps as f64;
            v((radius * angle.cos()) as f32, (radius * angle.sin()) as f32)
        }).collect()
    };
    let splits_at = |prims: &[TrackPrim], junction: Vec2| {
        // C++ ArcFit.cpp:623-626 shares p[k] between consecutive runs.
        prims.windows(2).any(|pair| {
            pair[0].a.distance_to(pair[0].b) > 1e-6
                && pair[1].a.distance_to(pair[1].b) > 1e-6
                && pair[0].b.x == pair[1].a.x && pair[0].b.y == pair[1].a.y
                && pair[0].b.distance_to(junction) < 1e-6
                && pair[1].a.distance_to(junction) < 1e-6
        })
    };
    let check_stats = |name: &str, prims: &[Vec<TrackPrim>], stats: &ArcFitStats| {
        // C++ ArcFit.cpp:959-963 counts each returned primitive by its kind.
        let arcs = prims.iter().flatten().filter(|p| p.is_arc).count();
        let segments = prims.iter().flatten().filter(|p| !p.is_arc).count();
        assert_eq!(stats.arcs, arcs as i32, "{name} statistics: arc total equals returned arcs");
        assert_eq!(stats.segments, segments as i32, "{name} statistics: segment total equals returned segments");
    };

    // Exact integer coordinates on the circle centred at (2, 3), radius 5.
    let (mut cx, mut cy, mut r) = (0.0, 0.0, 0.0);
    assert!(circle_from_3(v(7.0, 3.0), v(2.0, 8.0), v(-3.0, 3.0), &mut cx, &mut cy, &mut r),
        "circle recovery: three non-collinear points define a circle");
    assert!((cx - 2.0).abs() < 1e-12 && (cy - 3.0).abs() < 1e-12 && (r - 5.0).abs() < 1e-12,
        "circle recovery: known centre and radius");
    assert!(!circle_from_3(v(0.0, 0.0), v(1.0, 1.0), v(2.0, 2.0), &mut cx, &mut cy, &mut r),
        "circle refusal: collinear points have no finite circle");

    // Radius 5 mm, quarter circle, spacing less than 0.05 mm.
    let quarter_sweep = pi * 0.5;
    let samples = circle_samples(5.0, quarter_sweep, 158);
    let arcs = fit_arcs(&samples, &[], width, opt.tol, &opt, None, &[]);
    // C++ ArcFit.cpp:223,239-245,253-294,311-317: these dense, constant-width
    // circles pass the radius/chord-error checks; only maxSweepDeg limits a run.
    // Greedy fitting consumes floor(limit / sample angle) intervals at a time.
    let quarter_intervals = samples.len() - 1;
    let quarter_capacity = (opt.max_sweep_deg as f64 * pi / 180.0
        / (quarter_sweep / quarter_intervals as f64)).floor() as usize;
    let expected_quarter_count = quarter_intervals.div_ceil(quarter_capacity);
    assert_eq!(arcs.len(), expected_quarter_count, "arc fitting: quarter-circle primitive count follows the greedy sweep rule");
    assert_eq!(arcs.iter().filter(|p| p.is_arc).count(), expected_quarter_count,
        "arc fitting: every quarter-circle primitive is an arc");
    assert!(arcs.iter().any(|p| p.is_arc), "arc fitting: a sampled circle produces arcs");
    assert!(deviation(&samples, &arcs) <= opt.tol as f64, "arc fitting: two-way deviation stays within tol");
    let straight: Vec<_> = (0..=12).map(|i| v(i as f32 * 0.25, 1.0)).collect();
    let lines = fit_arcs(&straight, &[], width, opt.tol, &opt, None, &[]);
    // C++ ArcFit.cpp:179-193 accepts the entire monotone collinear run;
    // longest reaches its last sample (253-294), and fitRun emits it (311-317).
    let expected_line_count = usize::from(straight.len() >= 2);
    assert_eq!(lines.len(), expected_line_count, "straight fitting: one primitive for the nonempty collinear run");
    assert!(!lines.is_empty() && lines.iter().all(|p| !p.is_arc), "straight fitting: segments only");
    assert!(deviation(&straight, &lines) < 1e-12, "straight fitting: preserves the line");

    // Sagitta is a geometric bound, independent of the fitter's choice of primitives.
    let half = (5.0 / 2.0f64.sqrt()) as f32;
    let quarter = TrackPrim { is_arc: true, a: v(5.0, 0.0), mid: v(half, half), b: v(0.0, 5.0), width };
    let sagitta = 0.001;
    let mut tess = Vec::new();
    let mut tess_widths = Vec::new();
    tessellate(&[quarter], sagitta, &mut tess, Some(&mut tess_widths));
    assert!(tess.len() > 2 && tess.len() == tess_widths.len(), "tessellation: arc vertices and widths are paired");
    assert!(tess_widths.iter().all(|&w| w == width), "tessellation: preserves the primitive width");
    assert!(tess[0].x == quarter.a.x && tess[0].y == quarter.a.y
        && tess.last().unwrap().x == quarter.b.x && tess.last().unwrap().y == quarter.b.y,
        "tessellation: preserves exact primitive endpoints");
    assert!(deviation(&tess, &[quarter]) <= sagitta + 1e-6, "tessellation round trip: bounded by sagitta plus f32 rounding");
    assert!(circle_from_3(quarter.a, quarter.mid, quarter.b, &mut cx, &mut cy, &mut r),
        "tessellation sagitta: recover the primitive's own circle");
    // C++ ArcFit.cpp:755-764 promises a chord-error bound. On this circle the
    // sagitta is radius minus centre-to-chord-midpoint distance, independently
    // of tessellate/deviation. The existing 1e-6 allowance covers f32 vertices.
    for (i, chord) in tess.windows(2).enumerate() {
        assert!(chord.iter().all(|p| ((p.x as f64 - cx).hypot(p.y as f64 - cy) - r).abs() < 1e-6),
            "tessellation sagitta: chord {i} endpoints lie on the primitive's circle");
        let mx = (chord[0].x as f64 + chord[1].x as f64) * 0.5;
        let my = (chord[0].y as f64 + chord[1].y as f64) * 0.5;
        let chord_sagitta = r - (mx - cx).hypot(my - cy);
        assert!(chord_sagitta >= -1e-6 && chord_sagitta <= sagitta + 1e-6,
            "tessellation sagitta: chord {i} independently obeys the error bound");
    }
    assert!((distance_to(v(6.0, 0.0), &quarter) - 1.0).abs() < 1e-6,
        "arc distance: radial offset is one millimetre");
    assert!((distance_to(v(-1.0, 5.0), &quarter) - 1.0).abs() < 1e-6,
        "arc distance: outside the sweep uses the endpoint");

    let oct = fit_octilinear(&samples, &[], width, opt.oct_tol);
    let oct_legs = fit_octilinear_legs(&samples, &[], width, opt.oct_tol, opt.oct_leg, &[]);
    for (name, prims, tolerance) in [
        ("shared tolerance", &oct, opt.oct_tol as f64),
        ("separate tolerances", &oct_legs, (opt.oct_tol + opt.oct_leg) as f64),
    ] {
        assert!(!prims.is_empty(), "octilinear {name}: nonempty path");
        for (i, p) in prims.iter().enumerate() {
            let dx = (p.b.x as f64 - p.a.x as f64).abs();
            let dy = (p.b.y as f64 - p.a.y as f64).abs();
            assert!(!p.is_arc && (dx < 2e-5 || dy < 2e-5 || (dx - dy).abs() < 2e-5),
                "octilinear {name}: leg {i} lies on a multiple of 45 degrees within f32 rounding");
        }
        assert!(deviation(&samples, prims) <= tolerance + 2e-5,
            "octilinear {name}: two-way deviation obeys the DP plus dogleg tolerance");
    }

    let limited = ArcFitOptions { max_sweep_deg: 110.0, ..opt.clone() };
    let three_quarter_sweep = pi * 1.5;
    let three_quarters = circle_samples(5.0, three_quarter_sweep, 472);
    let split = fit_arcs(&three_quarters, &[], width, limited.tol, &limited, None, &[]);
    // C++ ArcFit.cpp:223,253-294,311-330: at most floor(110 / (270/472))
    // sample intervals fit per arc; the greedy pieces cannot merge past 110 deg.
    let limited_intervals = three_quarters.len() - 1;
    let limited_capacity = (limited.max_sweep_deg as f64 * pi / 180.0
        / (three_quarter_sweep / limited_intervals as f64)).floor() as usize;
    let expected_split_count = limited_intervals.div_ceil(limited_capacity);
    assert_eq!(split.len(), expected_split_count, "sweep limit: primitive count follows the sampled greedy sweep rule");
    assert_eq!(split.iter().filter(|p| p.is_arc).count(), expected_split_count,
        "sweep limit: every limited-sweep primitive is an arc");
    assert!(split.iter().filter(|p| p.is_arc).count() >= 3, "sweep limit: a 270-degree arc splits at 110 degrees");
    for p in split.iter().filter(|p| p.is_arc) {
        assert!(circle_from_3(p.a, p.mid, p.b, &mut cx, &mut cy, &mut r), "radius limit: each arc has a finite circle");
        // C++ ArcFit.cpp:216-223 measures sweep about the fitted centre.
        // Sum signed a->mid and mid->b angles about this primitive's own centre.
        let angle = |a: Vec2, b: Vec2| {
            let (ax, ay) = (a.x as f64 - cx, a.y as f64 - cy);
            let (bx, by) = (b.x as f64 - cx, b.y as f64 - cy);
            (ax * by - ay * bx).atan2(ax * bx + ay * by)
        };
        let sweep = angle(p.a, p.mid) + angle(p.mid, p.b);
        assert!(sweep > 0.0, "sweep direction: each arc follows the positive-sweep fixture");
        assert!(sweep.abs() <= limited.max_sweep_deg as f64 * pi / 180.0 + 1e-6,
            "sweep limit: each arc respects max_sweep_deg");
        assert!((r - 5.0).abs() < 1e-4 && r <= limited.max_radius as f64,
            "radius limit: fitted arcs recover the known radius within max_radius");
    }
    assert!(deviation(&three_quarters, &split) <= limited.tol as f64, "sweep split: preserves the original curve within tol");
    // Radius 200 exceeds max_radius=100; its sagitta over 0.06 radians exceeds tol,
    // so several straight pieces are needed, rather than one straight chord.
    let near_flat = circle_samples(200.0, 0.06, 240);
    let flat = fit_arcs(&near_flat, &[], width, opt.tol, &opt, None, &[]);
    assert!(flat.len() > 1 && flat.iter().all(|p| !p.is_arc), "radius limit: near-flat large-radius curve stays segments");
    assert!(deviation(&near_flat, &flat) <= opt.tol as f64, "radius limit: segment fallback remains within tol");

    // A point strictly inside a sample chord exercises insertion, not just sample snapping.
    let junction = (samples[53] + samples[54]) * 0.5;
    let no_arc_end = |p: &Vec2| p.distance_to(samples[0]) <= 0.3;
    let broken = fit_arcs(&samples, &[], width, opt.tol, &opt, Some(&no_arc_end), &[junction]);
    assert!(broken.iter().any(|p| p.is_arc), "endpoint predicates: retain arcs away from forbidden ends");
    assert!(splits_at(&broken, junction), "T-junction: consecutive nondegenerate primitives share the inserted point");
    assert!(broken.iter().filter(|p| p.is_arc).all(|p| !no_arc_end(&p.a) && !no_arc_end(&p.b)),
        "no_arc_end: no fitted arc starts or ends where the predicate refuses");

    let trace = |id, pts: &[Vec2]| Trace {
        id, net: 0, layer: 0, width, is_routed: true,
        points: pts.iter().map(|&pos| TracePoint { pos, width, ..TracePoint::default() }).collect(),
        smoothed: pts.to_vec(),
        ..Trace::default()
    };
    let branch_points = [junction * 0.65, junction * 0.75, junction * 0.85, junction];
    let wide = width * 2.0;
    let mut branch = trace(20, &branch_points);
    branch.point_widths = vec![width, width, wide, wide];
    for (point, &w) in branch.points.iter_mut().zip(&branch.point_widths) {
        point.width = w;
    }
    let board = OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        clr: ClearanceTable { rows: 1, layers: 2, mm: vec![0.2; 2], max: 0.2 },
        outline: vec![v(-1.0, -1.0), v(6.0, -1.0), v(6.0, 6.0), v(-1.0, 6.0)],
        nets: vec![Net { id: 0, ..Net::default() }],
        pads: vec![Pad {
            id: 0, net: 0, center: samples[0], size: v(0.4, 0.4),
            shape: PadShape::Circle, layer_mask: 1, is_smd: true, drill_diameter: 0.0,
            ..Pad::default()
        }],
        // Non-index IDs exercise the callback's documented board-index convention.
        traces: vec![trace(10, &samples), branch],
        ..OBoard::default()
    };
    let mut unguarded_stats = ArcFitStats::default();
    let unguarded = board_primitives(&board, TrackStyle::Arcs, &opt, Some(&mut unguarded_stats), None);
    assert!(unguarded.len() == 2 && unguarded.iter().all(|p| !p.is_empty()), "board export: one nonempty list per routed trace");
    assert!(unguarded_stats.traces == 2 && unguarded_stats.arcs > 0, "board statistics: both traces and fitted arcs counted");
    check_stats("unguarded board", &unguarded, &unguarded_stats);
    assert!(splits_at(&unguarded[0], junction), "board T-junction: consecutive nondegenerate primitives share the branch end");
    let on_pad = |p: Vec2| (p.x - samples[0].x).abs() <= 0.201 && (p.y - samples[0].y).abs() <= 0.201;
    assert!(unguarded[0].iter().filter(|p| p.is_arc).all(|p| !on_pad(p.a) && !on_pad(p.b)),
        "board pad guard: no arc ends on the same-net pad's conservative copper box");
    assert!(unguarded_stats.guard_refit == 0 && unguarded_stats.guard_fallback == 0,
        "optional counter: None disables the guard even when drc_guard is true");

    // Segments baseline vertices are a subset of the handmade samples. The fitted
    // curve's tessellation (and inserted junction) introduces other vertices. This
    // deterministic synthetic counter therefore flags trace 0 until it falls back.
    // C++ ArcFit.cpp:684,691,937-944: callback order is Segments baseline,
    // tol, tol/2, then Segments for trace 0 after both candidates add a violation.
    let segment_baseline: Vec<_> = board.traces.iter().map(segments_of).collect();
    let tighter = ArcFitOptions { tol: opt.tol * 0.5, ..opt.clone() };
    let refitted = board_primitives(&board, TrackStyle::Arcs, &tighter, None, None);
    // Only trace 0 is flagged; the branch keeps its original fitted primitives
    // throughout the candidate levels (C++ ArcFit.cpp:948-953).
    let fallback = vec![segment_baseline[0].clone(), unguarded[1].clone()];
    let expected_calls = [&segment_baseline, &unguarded, &refitted, &fallback];
    let calls = std::cell::Cell::new(0usize);
    let count = |g: &OBoard| {
        assert!(g.traces.len() == 2 && g.traces.iter().all(|tr| tr.smoothed.len() == tr.point_widths.len()),
            "counter input: receives the geometry board with tessellated widths");
        let call = calls.get();
        assert!(call < expected_calls.len(), "counter input: no extra guard level is evaluated");
        for (t, prims) in expected_calls[call].iter().enumerate() {
            // C++ ArcFit.cpp:933 uses this exact sagitta when replacing geometry.
            // Check the complete ordered geometry, not just lengths or proximity.
            let mut expected_points = Vec::new();
            let mut expected_widths = Vec::new();
            tessellate(prims, 2e-4, &mut expected_points, Some(&mut expected_widths));
            let actual_points: Vec<_> = g.traces[t].smoothed.iter().map(|p| (p.x, p.y)).collect();
            let expected_points: Vec<_> = expected_points.iter().map(|p| (p.x, p.y)).collect();
            assert_eq!(actual_points, expected_points,
                "counter input: call {call} trace {t} receives its complete tessellated geometry");
            assert_eq!(g.traces[t].point_widths, expected_widths,
                "counter input: call {call} trace {t} receives its tessellated widths");
        }
        assert!(g.traces[0].point_widths.iter().all(|&w| w == width),
            "counter input: constant-width trace retains its fixture width at every vertex");
        // C++ ArcFit.cpp:366-377 retains the narrow transition segment, whereas
        // the Segments baseline decimates the collinear run through the width
        // break (135-163,560). At the join tessellate takes max, not min (771),
        // so min of each adjacent vertex pair still gives that segment's width.
        let (branch_geometry, branch_widths) = if call == 0 {
            (vec![branch_points[0], branch_points[2], branch_points[3]], vec![width, wide, wide])
        } else {
            (branch_points.to_vec(), vec![width, width, wide, wide])
        };
        assert_eq!(g.traces[1].smoothed.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
            branch_geometry.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(),
            "counter input: varying-width branch has the fixture-derived ordered join geometry");
        assert_eq!(g.traces[1].point_widths, branch_widths,
            "counter input: varying-width join uses the wider incident primitive's width");
        calls.set(call + 1);
        let new_vertex = g.traces[0].smoothed.iter().any(|q| samples.iter().all(|p| p.distance_to(*q) > 1e-5));
        vec![if new_vertex { 1 } else { 0 }, 0]
    };
    let mut guarded_stats = ArcFitStats::default();
    let guarded = board_primitives(&board, TrackStyle::Arcs, &opt, Some(&mut guarded_stats), Some(&count));
    assert_eq!(calls.get(), expected_calls.len(), "DRC guard: evaluates the baseline and every expected candidate level");
    check_stats("guarded board", &guarded, &guarded_stats);
    assert!(guarded[0].iter().all(|p| !p.is_arc) && !guarded[0].is_empty(), "DRC guard: flagged trace falls back to segments");
    assert!(guarded_stats.guard_refit == 1 && guarded_stats.guard_fallback == 1,
        "DRC statistics: trace 0 refits once then falls back once");
    assert!(guarded[0].iter().all(|pr| [pr.a, pr.b].iter().all(|q| samples.iter().any(|p| p.distance_to(*q) < 1e-6))),
        "DRC fallback: segment vertices return to the original sample polyline");
    // C++ ArcFit.cpp:684,937-953 returns segmentsOf at the final level.
    // TrackPrim/Vec2 have no PartialEq; compare every field in primitive order.
    let primitive_fields = |p: &TrackPrim| (p.is_arc, (p.a.x, p.a.y), (p.mid.x, p.mid.y), (p.b.x, p.b.y), p.width);
    assert_eq!(guarded[0].iter().map(primitive_fields).collect::<Vec<_>>(),
        segment_baseline[0].iter().map(primitive_fields).collect::<Vec<_>>(),
        "DRC fallback: complete ordered primitives and widths equal segments_of of the original trace");
    let baseline_violation = |_g: &OBoard| vec![1, 0];
    let mut baseline_stats = ArcFitStats::default();
    let baseline = board_primitives(&board, TrackStyle::Arcs, &opt, Some(&mut baseline_stats), Some(&baseline_violation));
    check_stats("baseline-violation board", &baseline, &baseline_stats);
    assert!(baseline_stats.guard_refit == 0 && baseline_stats.guard_fallback == 0 && baseline[0].iter().any(|p| p.is_arc),
        "DRC baseline: an existing violation does not cause refit or fallback");

    // Six-degree intervals exercise actual biarcs, beyond the 0.05 cross-product
    // threshold for the reference's nearly-collinear shortcut.
    let biarc_sweep = pi / 5.0;
    let biarc_points = circle_samples(5.0, biarc_sweep, 6);
    let points_per_arc = 6;
    let biarc = BiarcFitter::fit_biarc_spline(&biarc_points, points_per_arc);
    // C++ Biarc.h:75-85,130-137: every chord is nondegenerate; at least one
    // endpoint tangent differs from its chord by half the six-degree step, so
    // sin(3 deg) > 0.05 excludes the linear shortcut, including the end spans.
    let biarc_spans = biarc_points.len() - 1;
    let biarc_step = biarc_sweep / biarc_spans as f64;
    assert!((biarc_step * 0.5).sin() > 0.05
        && biarc_points.windows(2).all(|p| p[0].distance_to(p[1]) >= 1e-4),
        "biarc fixture: every span fits and escapes the reference's linear shortcut");
    // C++ Biarc.h:55-63 emits (pointsPerArc+1)+pointsPerArc per biarc;
    // 141-153 removes the first sample of each subsequent span: 1 + 6*12 = 73.
    let expected_biarc_count = 1 + biarc_spans * (2 * points_per_arc as usize);
    assert_eq!(biarc.len(), expected_biarc_count, "biarc spline: sample count includes both arcs and omits shared starts");
    assert!(biarc.len() > biarc_points.len(), "biarc spline: exercises sampled arcs, not only linear shortcuts");
    assert!(biarc[0].distance_to(biarc_points[0]) < 2e-5
        && biarc.last().unwrap().distance_to(*biarc_points.last().unwrap()) < 2e-5,
        "biarc spline: first and last points are preserved within f32 trigonometric rounding");
    assert!(biarc.iter().all(|p| ((p.x as f64).hypot(p.y as f64) - 5.0).abs() < 0.015),
        "biarc spline: samples stay within 0.015 mm of the known circle");
}
