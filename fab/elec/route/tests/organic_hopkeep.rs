use fab_elec_route::organic::{
    geom::{std_max, std_min, Vec2},
    model::{ClearanceTable, LayerInfo, LayerIntent, Net, OBoard, Trace, TracePoint, Via},
    relaxer::OrganicRelaxer,
    settings::OrganicSettings,
};

#[test]
fn organic_hop_keep_preserves_long_runs_and_leaves_off_and_short_runs_identical() {
    let v = Vec2::new;
    let bits = |p: Vec2| (p.x.to_bits(), p.y.to_bits());
    let fixture = |positions: &[Vec2]| OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        clr: ClearanceTable { rows: 1, layers: 2, mm: vec![0.2; 2], max: 0.2 },
        outline: vec![v(0.0, 0.0), v(50.0, 0.0), v(50.0, 30.0), v(0.0, 30.0)],
        nets: vec![Net { id: 0, ..Net::default() }],
        vias: [positions[0], positions[positions.len() - 1]]
            .into_iter()
            .enumerate()
            .map(|(id, pos)| Via {
                id: id as i32,
                net: 0,
                pos,
                diameter: 0.6,
                drill: 0.3,
                layer_start: 0,
                layer_end: 1,
                is_fixed: true,
                ..Via::default()
            })
            .collect(),
        traces: vec![Trace {
            id: 0,
            net: 0,
            layer: 0,
            width: 0.25,
            is_routed: true,
            points: positions
                .iter()
                .enumerate()
                .map(|(k, &pos)| TracePoint {
                    pos,
                    width: 0.25,
                    attached_via: if k == 0 { 0 } else if k + 1 == positions.len() { 1 } else { -1 },
                    ..TracePoint::default()
                })
                .collect(),
            ..Trace::default()
        }],
        ..OBoard::default()
    };
    // Each fixture has two different own-net vias, no terminal pads, no branches,
    // no interior vias and no immutable/pair copper (OrganicRelaxer.cpp:3195-3217).
    let long = fixture(&[v(10.0, 10.0), v(10.5, 10.75), v(11.0, 11.5), v(11.5, 10.75), v(12.0, 10.0)]);
    let short_height = (0.9f32 * 0.9f32 - 0.6f32 * 0.6f32).sqrt();
    let short = fixture(&[v(10.0, 10.0), v(10.6, 10.0 + short_height), v(11.4, 10.0 + short_height), v(12.0, 10.0)]);
    let base = OrganicSettings::base();
    let enabled = OrganicSettings { hop_keep_dist: 4.0, hop_keep_weight: 8.0, ..OrganicSettings::base() };
    let poly = |b: &OBoard| {
        // Match hopKeepRun: each distance is f32, accumulated into a double.
        b.traces[0].points.windows(2).fold(0.0f64, |length, pair| length + f64::from(pair[0].pos.distance_to(pair[1].pos)))
    };
    let l0 = poly(&long) as f32; // HopKeepRun::L0 is stored as float (cpp:3239).
    let ends = 0.25 * f64::from(long.vias[0].diameter + long.vias[1].diameter) + 0.5 * f64::from(long.traces[0].width);
    let lim = f64::from(std_max(0.0f32, enabled.hop_limit));
    let p = std_min(f64::from(std_max(enabled.hop_keep_dist, 0.0f32)), std_max(f64::from(l0), lim + ends));
    assert!((f64::from(l0) - 3.6).abs() < 0.01, "long fixture: initial centre polyline is about 3.6 mm");
    assert!((poly(&short) - 2.6).abs() < 1e-5, "short fixture: initial centre polyline is 2.6 mm");
    assert!(poly(&short) < lim && f64::from(l0) >= lim, "eligibility fixtures: only the long run starts at or above hop_limit");
    for b in [&long, &short] {
        assert_eq!(b.vias[0].pos.distance_to(b.vias[1].pos).to_bits(), 2.0f32.to_bits(), "fixtures: via centres are 2 mm apart");
        for pair in b.traces[0].points.windows(2) {
            let length = pair[0].pos.distance_to(pair[1].pos);
            // resample.rs:350-375,460: initially merge below 0.45 * node_spacing
            // and split above node_spacing, i.e. 0.54 and 1.2 mm respectively.
            assert!(length > 0.45f32 * base.node_spacing && length < base.node_spacing,
                "fixtures: initial segments stay inside the resampling band, length={length}");
        }
    }

    let assert_same = |a: &OBoard, b: &OBoard, property: &str| {
        assert_eq!(a.traces.len(), b.traces.len(), "{property}: trace counts agree");
        for (t, (ta, tb)) in a.traces.iter().zip(&b.traces).enumerate() {
            let points = |tr: &Trace| {
                tr.points.iter().map(|p| (
                    bits(p.pos), bits(p.velocity), p.width.to_bits(), p.is_fixed, p.terminal_pad,
                    p.attached_via, p.branch_trace, p.branch_point, p.straight_before,
                )).collect::<Vec<_>>()
            };
            assert_eq!(points(ta), points(tb), "{property}: trace {t} points agree bit for bit");
            let sa: Vec<_> = ta.smoothed.iter().copied().map(&bits).collect();
            let sb: Vec<_> = tb.smoothed.iter().copied().map(&bits).collect();
            assert!(!sa.is_empty(), "{property}: trace {t} has an exported curve to compare");
            assert_eq!(sa, sb, "{property}: trace {t} smoothed coordinates agree bit for bit");
            assert_eq!(ta.point_widths.iter().map(|w| w.to_bits()).collect::<Vec<_>>(),
                tb.point_widths.iter().map(|w| w.to_bits()).collect::<Vec<_>>(),
                "{property}: trace {t} exported widths agree bit for bit");
            assert_eq!(ta.length.to_bits(), tb.length.to_bits(), "{property}: trace {t} exported lengths agree bit for bit");
        }
    };
    let mut off = long.clone();
    let mut zero_weight = long.clone();
    let mut kept = long.clone();
    let mut short_off = short.clone();
    let mut short_on = short.clone();
    let mut r_off = OrganicRelaxer::new(base);
    let mut r_zero = OrganicRelaxer::new(OrganicSettings { hop_keep_dist: 4.0, hop_keep_weight: 0.0, ..OrganicSettings::base() });
    let mut r_kept = OrganicRelaxer::new(enabled.clone());
    let mut r_short_off = OrganicRelaxer::new(OrganicSettings::base());
    let mut r_short_on = OrganicRelaxer::new(enabled.clone());
    for step in 1..=20 {
        for (r, b, property) in [
            (&mut r_off, &mut off, "off"),
            (&mut r_zero, &mut zero_weight, "zero weight"),
            (&mut r_kept, &mut kept, "kept"),
            (&mut r_short_off, &mut short_off, "short off"),
            (&mut r_short_on, &mut short_on, "short on"),
        ] {
            assert_eq!(r.step(b, 1000), Ok(()), "{property}: step {step} succeeds");
            for (k, via) in [(0, 0), (b.traces[0].points.len() - 1, 1)] {
                let end = &b.traces[0].points[k];
                assert_eq!(end.attached_via, via as i32, "{property}: step {step} preserves via {via} attachment");
                assert_eq!(bits(end.pos), bits(b.vias[via].pos), "{property}: step {step} keeps the endpoint on via {via}");
                assert_eq!(bits(b.vias[via].pos), bits(long.vias[via].pos), "{property}: step {step} leaves fixed via {via} at its original centre");
            }
        }
    }
    r_off.update_splines(&mut off);
    r_zero.update_splines(&mut zero_weight);
    assert_same(&off, &zero_weight, "off is bit-identical");
    let off_length = poly(&off);
    assert!(off_length < p, "without hop keep: the run contracts below P, length={off_length}, P={p}");

    // OrganicRelaxer.cpp:1306-1324,1476-1489,3277-3279 uses a soft one-sided
    // energy, not a hard constraint. For a segment of length s and floor r,
    // E = ks/2 * s^2 + 8*ks/2 * max(0, r-s)^2 has its minimum at s = 8*r/9.
    // The floors sum to P (cpp:3245-3250), so P/9 (~0.40 mm here) is the
    // expected deficit for this isolated non-collinear run. Add 0.02 mm for
    // finite sweeps / per-node settling and f32 rounding, not an exact P bound.
    let tolerance = p / (1.0 + f64::from(enabled.hop_keep_weight)) + 0.02;
    let kept_length = poly(&kept);
    assert!(p - tolerance > lim, "soft floor: the energy-derived tolerance still excludes a sub-limit hop");
    assert!(kept_length >= p - tolerance,
        "hop keep: the run stays above its soft floor, length={kept_length}, P={p}, tolerance={tolerance}");
    assert_eq!(kept.traces[0].points.len(), long.traces[0].points.len(), "hop keep: the kept fixture neither merges nor splits nodes");
    assert!(kept_length > off_length + 0.5, "hop keep: the kept run remains materially longer than the unprotected run");

    // OrganicRelaxer.cpp:3242 skips floors when the first-seen L0 < hopLimit;
    // it does not lengthen a run that was already a hop.
    r_short_off.update_splines(&mut short_off);
    r_short_on.update_splines(&mut short_on);
    assert_same(&short_off, &short_on, "initially short runs are not kept");
    assert!(poly(&short_on) < poly(&short) - 0.2, "initially short runs: the 2.6 mm run contracts as without hop keep");
}
