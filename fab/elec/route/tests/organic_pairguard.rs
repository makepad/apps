use fab_elec_route::organic::{
    diffpairs::{coupled_length, member_gap, partner_of},
    geom::{Box2, Vec2},
    model::{LayerInfo, Net, OBoard, Trace, TracePoint},
    opt::pairguard::PairGuard,
};

#[test]
fn organic_pairguard_preserves_sampled_pair_coupling() {
    let v = Vec2::new;
    let fixture = |edge_gap: f32| OBoard {
        layers: vec![LayerInfo { index: 0, ..LayerInfo::default() }, LayerInfo { index: 1, ..LayerInfo::default() }],
        nets: vec![
            Net { id: 0, is_diff_pair: true, diff_pair_partner: 1, pair_gap: 0.2, ..Net::default() },
            Net { id: 1, is_diff_pair: true, diff_pair_partner: 0, pair_gap: 0.2, ..Net::default() },
        ],
        // DiffPairs.cpp:326 subtracts half the summed widths: for 0.2 mm
        // tracks, centre separation is edge_gap + (0.2 + 0.2) / 2.
        traces: [0.0, 0.2 + edge_gap].into_iter().enumerate().map(|(i, y)| Trace {
            id: i as i32,
            net: i as i32,
            layer: 0,
            width: 0.2,
            is_routed: true,
            points: [v(0.0, y), v(10.0, y)].into_iter().map(|pos| TracePoint {
                pos, width: 0.2, ..TracePoint::default()
            }).collect(),
            ..Trace::default()
        }).collect(),
        ..OBoard::default()
    };
    let board = fixture(0.2);
    let a = [PairGuard::poly_trace(&board.traces[0])];
    let b = [PairGuard::poly_trace(&board.traces[1])];
    let gap = member_gap(&board, 0);

    // DiffPairs.cpp:283,304-311,330-333: L = 10, step = 0.05,
    // n = ceil(10 / 0.05) = 200, dl = 10 / 200 = 0.05. All 200
    // midpoints pass cpp:322-327, hence n * dl = 10 mm. cpp:415-418
    // adds the identical reverse contribution for both = true: 20 mm.
    let step = 0.05f64;
    let samples = (10.0f64 / step).ceil() as i32;
    let dl = 10.0 / samples as f64;
    let expected = samples as f64 * dl;
    // Only accumulated f64 roundoff is allowed, much less than one sample.
    let roundoff = 1e-10;
    let one = coupled_length(&a, &b, gap, false);
    let both = coupled_length(&a, &b, gap, true);
    assert!((one - expected).abs() < roundoff, "one direction: all 200 samples give the full 10 mm within one 0.05 mm sample");
    assert!((both - 2.0 * expected).abs() < roundoff, "both directions: two full sampled lengths give 20 mm");

    // DiffPairs.cpp:327: abs(eg - 0.2) <= 0.2 * 0.2 = 0.04.
    // Thus 0.23 is inside [0.16, 0.24], while 0.25 and 0.30 are outside.
    // Inside: all n samples contribute dl; outside: none contribute (cpp:333).
    for (edge_gap, want) in [(0.23, expected), (0.25, 0.0), (0.3, 0.0)] {
        let changed = fixture(edge_gap);
        let partner = [PairGuard::poly_trace(&changed.traces[1])];
        assert!((coupled_length(&a, &partner, gap, false) - want).abs() < roundoff,
            "gap window: edge gap {edge_gap} must contribute {want} mm");
    }

    // DiffPairs.cpp:283,324-325: abs(dot) = cos(20 deg) < cos(15 deg),
    // so no sample is coupled even where the segment passes near the member.
    let mut angled = board.clone();
    let angle = 20.0f64 * 3.14159265358979 / 180.0;
    angled.traces[1].points[1].pos = v((10.0 * angle.cos()) as f32, 0.4 + (10.0 * angle.sin()) as f32);
    let angled_partner = [PairGuard::poly_trace(&angled.traces[1])];
    assert_eq!(coupled_length(&a, &angled_partner, gap, false), 0.0, "angle window: 20 degrees contributes zero coupled length");

    // DiffPairs.cpp:322 skips every segment whose layer differs: zero hits.
    let mut other_layer = board.clone();
    other_layer.traces[1].layer = 1;
    let other_partner = [PairGuard::poly_trace(&other_layer.traces[1])];
    assert_eq!(coupled_length(&a, &other_partner, gap, false), 0.0, "layer separation: a partner on another layer contributes zero");

    // DiffPairs.cpp:425-429 returns the configured partner; cpp:434-436
    // returns its positive resolved gap, and Board.h:565 defaults to 0.20.
    assert_eq!(partner_of(&board, 0), 1, "membership: first member resolves its partner");
    assert_eq!(partner_of(&board, 1), 0, "membership: second member resolves its partner");
    assert_eq!(gap, 0.2, "member gap: a positive resolved pair_gap is returned");
    assert_eq!(board.rules.diff_pair_spacing, 0.20, "rule default: diff_pair_spacing is 0.20 mm");
    let mut fallback = board.clone();
    fallback.nets[0].pair_gap = 0.0;
    fallback.rules.diff_pair_spacing = 0.3;
    assert_eq!(member_gap(&fallback, 0), 0.3, "member gap fallback: zero pair_gap returns rules.diff_pair_spacing");

    // PairGuard.h:57-79 constructs copper from the chosen inclusive range.
    // The replaced 2 mm stretch is collinear but has one redundant midpoint.
    let old_points: Vec<TracePoint> = [v(4.0, 0.0), v(5.0, 0.0), v(6.0, 0.0)].into_iter().map(|pos| TracePoint {
        pos, width: 0.2, ..TracePoint::default()
    }).collect();
    let old = [PairGuard::poly_trace_points(&old_points, 0, 0.2, 0, -1)];
    let detour = [PairGuard::poly_points(&[v(4.0, 0.0), v(4.0, -1.0), v(6.0, -1.0), v(6.0, 0.0)], 0, 0.2, 0, -1)];
    let decimated = [PairGuard::poly_points(&[v(4.0, 0.0), v(6.0, 0.0)], 0, 0.2, 0, -1)];

    let mut unpaired = board.clone();
    for net in &mut unpaired.nets {
        net.is_diff_pair = false;
        net.diff_pair_partner = -1;
    }
    // DiffPairs.cpp:428,434 gives no partner / zero gap. PairGuard.h:40-51
    // never sets on; keeps exits true at h:114, after clearing lost at h:113.
    assert_eq!(member_gap(&unpaired, 0), 0.0, "nonmember gap: a net without a partner has zero gap");
    let mut guard = PairGuard::default();
    guard.init(&unpaired, true);
    assert!(!guard.active(), "unpaired board: guard is inactive");
    let mut lost = -1.0;
    assert!(guard.keeps(&unpaired, 0, &old, &detour, Some(&mut lost)), "unpaired board: any edit is kept without measuring coupling");
    assert_eq!(lost, 0.0, "unpaired board: keeps clears the optional loss");

    // PairGuard.h:46-50 recognizes both members and sets on. h:54-55 then
    // exposes the partner because its gap is positive; h:36 defaults to 0.05.
    guard.init(&board, true);
    assert!(guard.active() && guard.member(0) && guard.member(1), "paired board: guard and both members are active");
    assert_eq!(guard.partner(0), 1, "guard partner: the cached partner is net 1");
    assert_eq!(guard.tol_mm, step as f32, "guard tolerance: the default is one metrics sample");
    // PairGuard.h:85-96 includes the one routed partner trace in board order;
    // an invalid Box2 skips the spatial filter (h:90).
    let partner = guard.partner_copper(&board, 0, Box2::default());
    assert_eq!(partner.len(), 1, "partner copper: an invalid box includes the routed partner trace");

    // Derive loss before calling keeps; no observed result sets the oracle.
    // DiffPairs.cpp:305-306: each old 1 mm segment has ceil(1 / 0.05) = 20
    // samples, so the two segments contribute 40 * 0.05 = 2 mm.
    // In reverse, ptSeg clamps to the endpoints (cpp:256-259). cpp:326-327
    // accepts centre distance <= 0.2 + 1.2 * 0.2 = 0.44, at separation 0.4.
    // The endpoint reach along x is sqrt(0.44^2 - 0.4^2) = 0.183303... mm.
    // Midpoints outside [4,6] are 0.025, 0.075, 0.125, 0.175, 0.225, ...
    // away (cpp:310-311): four pass at EACH end. Thus the partner contributes
    // (40 + 2*4) * 0.05 = 2.4 mm. The detour's vertical legs fail cpp:325;
    // its horizontal leg has edge gap 1.2, failing cpp:327 in both directions.
    // PairGuard.h:127-129 therefore reports (2 + 2.4) - 0 = 4.4 mm lost.
    let stretch_samples = 2 * (1.0f64 / step).ceil() as i32;
    let expected_lost = stretch_samples as f64 * step + (stretch_samples + 2 * 4) as f64 * dl;
    assert!((guard.coupled(&board, 0, &old, &partner) - expected_lost).abs() < roundoff,
        "partial stretch: bidirectional coupling includes four endpoint samples per side");
    assert!(!guard.keeps(&board, 0, &old, &detour, Some(&mut lost)), "pair guard: a 1 mm detour of a coupled 2 mm stretch is refused");
    assert!(lost > 0.0 && (lost - expected_lost).abs() < roundoff, "pair guard loss: the detour loses the formula-derived 4.4 mm");
    // DiffPairs.cpp:305-311: one 2 mm segment has ceil(2 / 0.05) = 40
    // samples at the same midpoints as two 1 mm segments; endpoint reach is
    // unchanged. before == after, so h:128 keeps the edit and h:113 leaves 0.
    assert!(guard.keeps(&board, 0, &old, &decimated, Some(&mut lost)), "pair guard: collinear decimation preserves coupling");
    assert_eq!(lost, 0.0, "collinear decimation: accepted edits leave zero loss");
    assert!(guard.keeps(&board, 0, &old, &decimated, None), "optional loss: an accepted edit works without a loss output");

    // DiffPairs.cpp:288-341 uses fixed input, cell and sample order, and its
    // map only for lookup: repeating the input repeats every addition's bits.
    assert_eq!(one.to_bits(), coupled_length(&a, &b, gap, false).to_bits(), "determinism: one-direction length repeats bit for bit");
    assert_eq!(both.to_bits(), coupled_length(&a, &b, gap, true).to_bits(), "determinism: both-direction length repeats bit for bit");
}
