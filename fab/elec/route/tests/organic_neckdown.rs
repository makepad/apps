use fab_elec_route::organic::{
    geom::Vec2,
    model::{ClearanceTable, DesignRules, LayerInfo, NeckdownIssue, Net, OBoard, Pad, PadShape, Trace, TracePoint},
    neckdown::{neck_trace, output_stamp, point_in_poly, poly_seg_dist, proper_cross, simple_polygon, Ctx, Scratch},
    settings::OrganicSettings,
};

#[test]
fn organic_neckdown_geometry_stamp_clearance_and_determinism() {
    let v = Vec2::new;
    // The L occupies x < 1 above y=1, and y < 1 to the right of x=1.
    let concave = [v(0.0, 0.0), v(3.0, 0.0), v(3.0, 1.0), v(1.0, 1.0), v(1.0, 3.0), v(0.0, 3.0)];
    assert!(point_in_poly(&concave, v(0.5, 2.0)), "concave polygon: interior of vertical arm");
    assert!(!point_in_poly(&concave, v(2.0, 2.0)), "concave polygon: missing upper-right quadrant is outside");
    // NeckdownTeardrop.cpp:87-93: strict ray tests, not an inclusive boundary predicate.
    assert!(point_in_poly(&concave, v(0.0, 2.0)), "concave polygon: left boundary is included by the ray rule");
    assert!(!point_in_poly(&concave, v(1.0, 2.0)), "concave polygon: right boundary is excluded by the ray rule");
    let square = [v(0.0, 0.0), v(2.0, 0.0), v(2.0, 2.0), v(0.0, 2.0)];
    assert_eq!(poly_seg_dist(&square, v(-1.0, 1.0), v(3.0, 1.0)), 0.0, "polygon distance: segment crosses square");
    assert_eq!(poly_seg_dist(&square, v(2.0, 0.5), v(2.0, 1.5)), 0.0, "polygon distance: segment touches square edge");
    let away_x = 5.0f32;
    let expected_gap = (away_x - square[1].x) as f64;
    assert_eq!(poly_seg_dist(&square, v(away_x, 0.5), v(away_x, 1.5)), expected_gap, "polygon distance: perpendicular gap to parallel segment");
    assert!(proper_cross(square[0], square[2], square[1], square[3]), "proper crossing: square diagonals");
    assert!(!proper_cross(square[0], square[1], square[1], square[2]), "proper crossing excludes shared endpoints");
    assert!(!proper_cross(v(0.0, 0.0), v(2.0, 0.0), v(1.0, 0.0), v(3.0, 0.0)), "proper crossing excludes collinear overlap");
    assert!(simple_polygon(&square), "simple polygon: square");
    assert!(!simple_polygon(&[square[0], square[2], square[1], square[3]]), "simple polygon rejects bow-tie");

    let path: Vec<Vec2> = (0..=8).map(|i| v(i as f32, 0.0)).collect();
    let trace = Trace {
        id: 17, net: 0, layer: 0, width: 1.0, is_routed: true,
        points: path.iter().map(|&pos| TracePoint { pos, width: 1.0, ..TracePoint::default() }).collect(),
        smoothed: path.clone(), point_widths: vec![1.0; path.len()], ..Trace::default()
    };
    let stamp = output_stamp(&trace);
    assert_ne!(stamp, 0, "output stamp is never zero");
    assert_eq!(stamp, output_stamp(&trace.clone()), "output stamp is stable for identical exports");
    let mut changed = trace.clone();
    changed.smoothed[4].y = 0.125;
    assert_ne!(stamp, output_stamp(&changed), "output stamp detects exported point change");
    changed = trace.clone(); changed.point_widths[4] = 0.5;
    assert_ne!(stamp, output_stamp(&changed), "output stamp detects exported width change");
    changed = trace.clone(); changed.teardrop_front = square.to_vec();
    assert_ne!(stamp, output_stamp(&changed), "output stamp detects front teardrop change");
    changed = trace.clone(); changed.teardrop_back = square.to_vec();
    assert_ne!(stamp, output_stamp(&changed), "output stamp detects back teardrop change");
    changed = trace.clone(); changed.length = 123.0; changed.points[4].pos.y = 2.0; changed.points[4].width = 0.125; changed.points[4].velocity.x = 3.0;
    assert_eq!(stamp, output_stamp(&changed), "output stamp ignores non-exported state when a smoothed path exists");
    changed = trace.clone(); changed.smoothed.clear(); changed.point_widths.clear();
    let node_stamp = output_stamp(&changed); changed.points[4].pos.y = 2.0;
    assert_ne!(node_stamp, output_stamp(&changed), "output stamp detects exported node fallback changes");
    // NeckdownTeardrop.cpp:223-230 samples by n/16, so the requested change-detection property
    // cannot hold for every point of a long path. At n=64 it visits 0,4,...,60 and 63, not 1.
    changed = trace.clone(); changed.smoothed = (0..64).map(|i| v(i as f32, 0.0)).collect(); changed.point_widths = vec![1.0; 64];
    let sampled_stamp = output_stamp(&changed); changed.smoothed[1].y = 0.125; changed.point_widths[1] = 0.5;
    assert_eq!(sampled_stamp, output_stamp(&changed), "sampled stamp deliberately misses unsampled long-path changes");

    // Two foreign rectangular pads leave a 0.8 mm gap. With 0.2 mm clearance on each side,
    // the allowed width is 0.4 mm, less than the nominal 1.0 mm and greater than the floor.
    let pad = |id, y| Pad {
        id, net: 1, center: v(4.0, y), size: v(0.8, 1.0), local_size: v(0.8, 1.0),
        shape: PadShape::Rect, layer_mask: 1, drill_diameter: 0.0, is_smd: true, ..Pad::default()
    };
    let board = OBoard {
        layers: vec![LayerInfo { index: 0, ..LayerInfo::default() }],
        nets: vec![Net { id: 0, ..Net::default() }, Net { id: 1, ..Net::default() }],
        clr: ClearanceTable { rows: 1, layers: 1, mm: vec![0.2], max: 0.2 },
        rules: DesignRules { min_trace_width: 0.15, min_clearance: 0.2, ..DesignRules::default() },
        pads: vec![pad(0, 0.9), pad(1, -0.9)], traces: vec![trace], ..OBoard::default()
    };
    let settings = OrganicSettings {
        enable_dynamic_neckdown: true, min_neckdown_width: 0.2, neckdown_taper_length: 0.5,
        guard_clearance_slack: 0.0, enable_teardrops: false, ..OrganicSettings::default()
    };
    let run = |mut b: OBoard| {
        let mut scratch = Scratch::default(); let mut issues: Vec<NeckdownIssue> = Vec::new(); let mut nominal = Vec::new();
        {
            let mut c = Ctx::new(&mut b, &settings, &mut scratch, None);
            c.build(false);
            neck_trace(&mut c, 0, &mut issues, false, Some(&mut nominal));
        }
        (b, issues, nominal)
    };
    let (first, issues, nominal) = run(board.clone());
    let (second, _, _) = run(board.clone());
    let result = &first.traces[0];
    assert!(issues.is_empty(), "feasible gap produces no impossible-neckdown issue");
    assert_eq!(nominal.len(), path.len(), "neckdown returns the original nominal path");
    assert!(nominal.iter().zip(&path).all(|(a, b)| a.x.to_bits() == b.x.to_bits() && a.y.to_bits() == b.y.to_bits()), "nominal path is bit-identical before resampling");
    assert!(result.smoothed.len() > path.len(), "constrained trace is resampled");
    assert_eq!(result.smoothed.len(), result.point_widths.len(), "every exported point has exactly one width");
    let mut gap_count = 0; let mut far_count = 0;
    for (&p, &width) in result.smoothed.iter().zip(&result.point_widths) {
        assert!(width >= settings.min_neckdown_width, "all exported widths respect minimum neckdown width");
        assert!(width <= result.width, "neckdown never widens past nominal width");
        if (p.x - 4.0).abs() <= 0.3 {
            gap_count += 1;
            assert!(width < result.width, "exported widths narrow inside the foreign-pad gap");
            let gap = (board.pads[0].center.y - 0.5 * board.pads[0].size.y) as f64;
            let expected = 2.0 * (gap - board.rules.min_clearance as f64);
            assert!((width as f64 - expected).abs() < 1e-6, "gap width matches twice centerline-to-pad gap minus clearance");
        }
        if p.x <= 2.0 || p.x >= 6.0 {
            far_count += 1;
            assert_eq!(width.to_bits(), result.width.to_bits(), "width stays nominal far from the constrained region and taper");
        }
    }
    assert!(gap_count > 0 && far_count > 0, "fixture exercises both the bottleneck and distant trace");
    assert_eq!(result.point_widths.iter().map(|w| w.to_bits()).collect::<Vec<_>>(), second.traces[0].point_widths.iter().map(|w| w.to_bits()).collect::<Vec<_>>(), "same input produces bit-identical exported widths");
    assert_eq!(result.points.iter().map(|p| p.width.to_bits()).collect::<Vec<_>>(), second.traces[0].points.iter().map(|p| p.width.to_bits()).collect::<Vec<_>>(), "same input produces bit-identical node widths");
    // An impossible squeeze keeps the floor and reports its worst sample (C++:1396-1421).
    let mut impossible = board.clone(); impossible.pads[0].center.y = 0.75; impossible.pads[1].center.y = -0.75;
    // NeckdownTeardrop.cpp:1397-1413 reports max(0, allowed); :1335-1336 allows
    // min(nominal width, 2 * (centerline-to-pad gap - clearance)).
    let tight_gap = impossible.pads[0].center.y as f64 - 0.5 * impossible.pads[0].size.y as f64 - path[0].y as f64;
    let expected_required_width = (2.0 * (tight_gap - impossible.rules.min_clearance as f64))
        .min(impossible.traces[0].width as f64).max(0.0);
    let expected_min_width = settings.min_neckdown_width.max(impossible.rules.min_trace_width);
    // NeckdownTeardrop.cpp:1377-1382 samples every 0.05 mm; :1404 keeps the first
    // strict minimum and :1411 records its midpoint. The f32 sample at 3.60 lies
    // just left of the f64 pad edge, so [3.60, 3.65] is the first minimum segment.
    let expected_pos = v(impossible.pads[0].center.x - 0.5 * impossible.pads[0].size.x + 0.5 * 0.05, path[0].y);
    let (tight, tight_issues, _) = run(impossible);
    assert_eq!(tight_issues.len(), 1, "one contiguous impossible bottleneck produces one issue");
    assert!(tight_issues[0].required_width < settings.min_neckdown_width, "issue records a required width below the floor");
    assert!((tight_issues[0].required_width as f64 - expected_required_width).abs() < 1e-6,
        "worst sample requirement matches the fixture's available width before applying the floor");
    assert_eq!(tight_issues[0].trace_id, 17, "issue retains the trace id rather than its board index");
    assert_eq!(tight_issues[0].net_id, board.traces[0].net, "worst sample retains the trace net");
    assert_eq!(tight_issues[0].layer, board.traces[0].layer, "worst sample retains the trace layer");
    assert!((tight_issues[0].pos.x - expected_pos.x).abs() < 1e-6, "worst sample x is the first minimum segment's midpoint");
    assert_eq!(tight_issues[0].pos.y.to_bits(), expected_pos.y.to_bits(), "worst sample y stays on the trace centerline");
    assert_eq!(tight_issues[0].min_width.to_bits(), expected_min_width.to_bits(), "worst sample records the effective minimum neckdown width");
    assert!(tight.traces[0].point_widths.iter().all(|&w| w >= settings.min_neckdown_width), "impossible gap never drives widths below the floor");
    assert!(tight.traces[0].point_widths.iter().any(|&w| w.to_bits() == settings.min_neckdown_width.to_bits()), "impossible bottleneck reaches the minimum width");
}
