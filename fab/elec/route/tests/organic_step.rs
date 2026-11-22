use fab_elec_route::organic::{
    copper::seg_pad_gap,
    geom::Vec2,
    model::{ClearanceTable, DesignRules, LayerInfo, LayerIntent, Net, OBoard, Pad, PadShape, Trace, TracePoint},
    relaxer::{OrganicRelaxer, StepError},
    settings::OrganicSettings,
};

#[test]
fn organic_step_preserves_descent_determinism_clearance_and_exports() {
    let v = Vec2::new;
    let bits = |p: Vec2| (p.x.to_bits(), p.y.to_bits());
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
    let empty = OBoard {
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
    let add_trace = |b: &mut OBoard, positions: &[Vec2]| {
        let first_pad = b.pads.len() as i32;
        b.pads.push(pad(first_pad, 0, positions[0]));
        b.pads.push(pad(first_pad + 1, 0, positions[positions.len() - 1]));
        b.traces.push(Trace {
            id: b.traces.len() as i32,
            net: 0,
            layer: 0,
            width: 0.25,
            is_routed: true,
            points: positions.iter().enumerate().map(|(k, &pos)| TracePoint {
                pos,
                width: 0.25,
                is_fixed: k == 0 || k + 1 == positions.len(),
                terminal_pad: if k == 0 { first_pad } else if k + 1 == positions.len() { first_pad + 1 } else { -1 },
                ..TracePoint::default()
            }).collect(),
            ..Trace::default()
        });
    };

    // Build fixtures once, then give each property its own clones.
    let mut isolated = empty.clone();
    add_trace(&mut isolated, &[
        v(10.0, 10.0), v(10.625, 10.5), v(11.25, 9.5), v(11.875, 10.5),
        v(12.5, 9.5), v(13.125, 10.5), v(13.75, 10.0),
    ]);
    let mut guarded = empty.clone();
    add_trace(&mut guarded, &[
        v(10.0, 10.0), v(11.0, 9.5), v(12.0, 9.75), v(13.0, 9.5), v(14.0, 10.0),
    ]);
    let foreign_pad = guarded.pads.len();
    guarded.pads.push(pad(foreign_pad as i32, 1, v(12.0, 10.725)));
    let mut crowded = guarded.clone();
    // Both traces are net 0; the central pad is foreign to both, on net 1.
    add_trace(&mut crowded, &[
        v(10.0, 11.5), v(11.0, 12.0), v(12.0, 11.75), v(13.0, 12.0), v(14.0, 11.5),
    ]);

    let settings = OrganicSettings::base();
    // sweep.rs:24-25,264-300 and topology.rs:176-203: w = ks * seg_w,
    // seg_w = 1 without branches. Preserve spring_k's f32 sum before casting.
    let ks = f64::from(settings.tension + settings.straightening_force + 0.5f32 * settings.contraction_tension).max(0.05);
    let kb = (f64::from(settings.layer_crossing_perpendicularity) + f64::from(settings.straight_bend)) * ks;
    let energy = |b: &OBoard| {
        b.traces.iter().map(|tr| {
            let mut e = 0.0;
            for pair in tr.points.windows(2) {
                let dx = f64::from(pair[1].pos.x) - f64::from(pair[0].pos.x);
                let dy = f64::from(pair[1].pos.y) - f64::from(pair[0].pos.y);
                e += 0.5 * ks * (dx * dx + dy * dy);
            }
            // The three P4e arms (sweep.rs:280-296) cover centres 2..=n-3,
            // not the first/last interior node. base() sets kb to zero
            // (settings.rs:293-307); retain the actual stencil in this E.
            for j in 2..tr.points.len().saturating_sub(2) {
                let a = tr.points[j - 1].pos;
                let p = tr.points[j].pos;
                let c = tr.points[j + 1].pos;
                let dx = f64::from(a.x) - 2.0 * f64::from(p.x) + f64::from(c.x);
                let dy = f64::from(a.y) - 2.0 * f64::from(p.y) + f64::from(c.y);
                e += 0.5 * kb * (dx * dx + dy * dy);
            }
            e
        }).sum::<f64>()
    };

    // No other energy term is active in the isolated fixture: own-net pads
    // have no trace repulsion (engine/pair.rs:233-240), the outline is far
    // away, and there is no foreign copper, crossing, via/tether or branch.
    // base() disables pad-exit half-planes, line pulls, bundles and meanders;
    // straight_before is false everywhere. Self-contact checks add no energy.
    // The reference header:16-24 promises shared-energy descent/no cycles;
    // engine/guard.rs:472-512 checks the local energy change before each move.
    let mut settled = isolated.clone();
    let mut relaxer = OrganicRelaxer::new(OrganicSettings::base());
    let initial_energy = energy(&settled);
    let mut previous_energy = initial_energy;
    let node_count = settled.traces[0].points.len();
    let mut idle = false;
    // resample.rs:350-375,460: initial merge/split thresholds are 0.54/1.2 mm.
    // Exact, equally spaced x coordinates stay 0.625 mm apart under springs.
    // Initial lengths are about 0.800/1.179 mm and settle towards 0.625 mm;
    // the straight trace's E is about 34% of the starting E, without resampling.
    let merge_short = f64::from(0.45f32 * settings.node_spacing);
    let split_len = f64::from(settings.node_spacing);
    let check_spacing = |tr: &Trace| {
        for (k, pair) in tr.points.windows(2).enumerate() {
            let dx = f64::from(pair[1].pos.x) - f64::from(pair[0].pos.x);
            let dy = f64::from(pair[1].pos.y) - f64::from(pair[0].pos.y);
            let len = (dx * dx + dy * dy).sqrt();
            assert!(len > merge_short && len < split_len,
                "energy fixture: segment {k} stays inside the resampling band, length={len}");
        }
    };
    check_spacing(&settled.traces[0]);
    for step in 1..=200 {
        assert_eq!(relaxer.step(&mut settled, 1000), Ok(()), "energy descent: step {step} succeeds");
        assert_eq!(settled.traces[0].points.len(), node_count,
            "energy descent: step {step} neither splits nor merges nodes");
        check_spacing(&settled.traces[0]);
        let next_energy = energy(&settled);
        assert!(next_energy <= previous_energy + 1e-9,
            "energy monotone: step {step} changed E from {previous_energy} to {next_energy}");
        previous_energy = next_energy;
        if relaxer.last_step_info().idle {
            idle = true;
            break;
        }
    }
    // relaxer.rs:147-179 / OrganicRelaxer.cpp:6225-6244: no pending work
    // and an unchanged snapshot return idle, rather than another sweep.
    assert!(idle, "no limit cycles: isolated trace becomes idle within 200 steps");
    assert!(previous_energy < 0.5 * initial_energy,
        "straightening: E drops by more than half, initial={initial_energy}, final={previous_energy}");

    // Request exports after stepping: the spline includes each node, preserves both
    // endpoints exactly and sums the curve length.
    relaxer.update_splines(&mut settled);
    for (t, tr) in settled.traces.iter().enumerate() {
        assert!(tr.smoothed.len() >= tr.points.len(), "exported curves: trace {t} has at least one sample per node");
        assert_eq!(bits(tr.smoothed[0]), bits(tr.points[0].pos), "exported curves: trace {t} preserves its first endpoint");
        assert_eq!(bits(*tr.smoothed.last().unwrap()), bits(tr.points.last().unwrap().pos),
            "exported curves: trace {t} preserves its last endpoint");
        assert!(tr.length.is_finite() && tr.length > 0.0, "exported curves: trace {t} has finite positive length");
    }

    // mod.rs:24-25: same input, same bytes. relaxer.rs:260-286 and reference
    // OrganicRelaxer.cpp:6352-6396 split the budget deterministically; each
    // substep receives at least one unit. sweep.rs:28-38,610-621 spends one
    // unit per node. There are no vias (which would cost three units).
    let mut a = crowded.clone();
    let mut b = crowded.clone();
    let mut ra = OrganicRelaxer::new(OrganicSettings::base());
    let mut rb = OrganicRelaxer::new(OrganicSettings::base());
    let budget = 7i64;
    let substeps = i64::from(settings.substeps_per_frame.max(1));
    let mut budget_limited = false;
    for step in 1..=30 {
        assert_eq!(ra.step(&mut a, budget), Ok(()), "determinism: first run's step {step} succeeds");
        assert_eq!(rb.step(&mut b, budget), Ok(()), "determinism: second run's step {step} succeeds");
        assert_eq!(ra.last_step_info(), rb.last_step_info(), "determinism: step {step} reports identical work");
        for info in [ra.last_step_info(), rb.last_step_info()] {
            if info.budget_limited {
                budget_limited = true;
                assert!(info.evaluated_nodes >= 0 && i64::from(info.evaluated_nodes) <= budget + substeps,
                    "budget bound: step {step} evaluated {} nodes with budget {budget} and {substeps} substeps", info.evaluated_nodes);
            }
        }
    }
    assert!(budget_limited, "budget-limited fixture: at least one step exhausts its budget");
    assert_eq!(a.traces.len(), crowded.traces.len(), "determinism: the fixture's two traces survive");
    assert_eq!(a.traces.len(), b.traces.len(), "same bytes twice: trace counts agree");
    ra.update_splines(&mut a);
    rb.update_splines(&mut b);
    let mut moved = false;
    for (t, (ta, tb)) in a.traces.iter().zip(&b.traces).enumerate() {
        let pa: Vec<_> = ta.points.iter().map(|p| bits(p.pos)).collect();
        let pb: Vec<_> = tb.points.iter().map(|p| bits(p.pos)).collect();
        let initial: Vec<_> = crowded.traces[t].points.iter().map(|p| bits(p.pos)).collect();
        moved |= pa != initial;
        assert_eq!(pa, pb, "same bytes twice: trace {t} node coordinates agree bit for bit");
        let sa: Vec<_> = ta.smoothed.iter().copied().map(&bits).collect();
        let sb: Vec<_> = tb.smoothed.iter().copied().map(&bits).collect();
        assert!(!sa.is_empty(), "same bytes twice: trace {t} has an exported curve to compare");
        assert_eq!(sa, sb, "same bytes twice: trace {t} smoothed coordinates agree bit for bit");
    }
    assert!(moved, "determinism fixture: budgeted steps actually change the routing");

    // copper.rs:200-204 measures edge-to-edge copper gap, including half width.
    // The forbidden straight chord has 0.725 - 0.5 - 0.125 = 0.1 mm gap;
    // the zig-zag's nearest point has 0.975 - 0.5 - 0.125 = 0.35 mm gap.
    let minimum_gap = |b: &OBoard| {
        let tr = &b.traces[0];
        tr.points.windows(2).enumerate().map(|(k, pair)| {
            let gap = seg_pad_gap(pair[0].pos, pair[1].pos, f64::from(tr.width) * 0.5, &b.pads[foreign_pad]);
            assert!(gap.is_finite(), "clearance guard: segment {k} has a finite copper gap");
            gap
        }).fold(f64::INFINITY, f64::min)
    };
    let first = guarded.traces[0].points[0].pos;
    let last = guarded.traces[0].points.last().unwrap().pos;
    let chord_gap = seg_pad_gap(first, last, f64::from(guarded.traces[0].width) * 0.5, &guarded.pads[foreign_pad]);
    assert!((chord_gap - 0.1).abs() < 1e-6, "clearance fixture: the straight chord passes 0.1 mm from foreign copper");
    assert!(minimum_gap(&guarded) >= 0.25, "clearance fixture: every initial segment clears foreign copper by at least 0.25 mm");
    let mut b = guarded.clone();
    let mut r = OrganicRelaxer::new(OrganicSettings::base());
    // engine/guard.rs:545-565 rejects a move worsening a sub-threshold gap;
    // relax/nodes.rs:13-25 and sweep.rs:610 keep the terminal pad pins fixed.
    for step in 1..=30 {
        assert_eq!(r.step(&mut b, 1000), Ok(()), "clearance guard: step {step} succeeds");
        let gap = minimum_gap(&b);
        assert!(gap >= 0.2 - 1e-6, "clearance guard: step {step} keeps every segment legal, minimum gap={gap}");
        assert_eq!(bits(b.traces[0].points[0].pos), bits(first), "pad attachment: step {step} preserves the first endpoint");
        assert_eq!(bits(b.traces[0].points.last().unwrap().pos), bits(last),
            "pad attachment: step {step} preserves the last endpoint");
    }

    // relaxer.rs:108,120-126: both refusals precede all board mutation.
    // OBoard has no PartialEq; derived Debug covers every field, including
    // velocities, widths, pad metadata and exports (all fixture floats finite).
    let before = format!("{crowded:?}");
    let mut b = crowded.clone();
    let mut r = OrganicRelaxer::new(OrganicSettings::base());
    r.settings.enable_biarcs = true;
    assert_eq!(r.step(&mut b, 1000), Err(StepError::Unported("enable_biarcs")), "refusal: biarcs are explicitly unported");
    assert_eq!(format!("{b:?}"), before, "refusal: enabling biarcs leaves the entire board unchanged");
    let mut b = crowded.clone();
    let mut r = OrganicRelaxer::new(OrganicSettings::base());
    assert_eq!(r.step(&mut b, 0), Err(StepError::Unbudgeted), "refusal: zero budget is explicitly unbudgeted");
    assert_eq!(format!("{b:?}"), before, "refusal: zero budget leaves the entire board unchanged");
}
