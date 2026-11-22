use fab_elec_route::organic::{
    copper,
    geom::Vec2,
    model::*,
    simplify::{has_self_intersections, point_in_ring, prune_dangling_vias, reverse_points, simplify_all_nets, simplify_net},
};

#[test]
fn organic_simplify_preserves_copper_and_prunes_redundancy() {
    let v = Vec2::new;
    let point = |pos| TracePoint { pos, ..TracePoint::default() };
    let terminal = |pos, pad| TracePoint { pos, terminal_pad: pad, is_fixed: true, ..TracePoint::default() };
    let pad = |id, center, layer_mask| Pad {
        id, center, size: v(1.0, 1.0), local_size: v(1.0, 1.0), shape: PadShape::Rect,
        drill_diameter: 0.0, layer_mask, net: 0, is_smd: true, ..Pad::default()
    };
    let trace = |id, layer, points| Trace {
        id, net: 0, layer, width: 0.25, points, is_routed: true, ..Trace::default()
    };
    let base = OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        clr: ClearanceTable { rows: 1, layers: 2, mm: vec![0.2; 2], max: 0.2 },
        outline: vec![v(0.0, 0.0), v(50.0, 0.0), v(50.0, 30.0), v(0.0, 30.0)],
        nets: vec![Net { id: 0, ..Net::default() }],
        pads: vec![pad(0, v(5.0, 10.0), 1), pad(1, v(25.0, 10.0), 1)],
        ..OBoard::default()
    };
    let segments = |b: &OBoard| b.traces.iter().map(|tr| tr.points.len().saturating_sub(1)).sum::<usize>();
    // Compare every point field exactly, including signed-zero/float bits and attachments.
    let points_snapshot = |b: &OBoard| {
        b.traces.iter().map(|tr| {
            (tr.id, tr.net, tr.layer, tr.width.to_bits(), tr.rule,
                tr.points.iter().map(|p| {
                    (p.pos.x.to_bits(), p.pos.y.to_bits(), p.velocity.x.to_bits(), p.velocity.y.to_bits(),
                        p.width.to_bits(), p.is_fixed, p.terminal_pad, p.attached_via,
                        p.branch_trace, p.branch_point, p.straight_before)
                }).collect::<Vec<_>>())
        }).collect::<Vec<_>>()
    };

    // The direct path is 20 mm; the separate three-segment detour is 30 mm.
    // NetGraphSimplifier.cpp:475-499 keeps the shorter graph edge and removes the cycle.
    let mut loop_board = base.clone();
    loop_board.traces = vec![
        trace(0, 0, vec![terminal(v(5.0, 10.0), 0), terminal(v(25.0, 10.0), 1)]),
        trace(1, 0, vec![terminal(v(5.0, 10.0), 0), point(v(5.0, 15.0)), point(v(25.0, 15.0)), terminal(v(25.0, 10.0), 1)]),
    ];
    let original_loop = loop_board.clone();
    let before_segments = segments(&loop_board);
    assert!(simplify_net(0, &mut loop_board), "two routes between the same pads contain a removable cycle");
    assert_eq!(loop_board.traces.len(), 1, "the two pads retain exactly one trace path");
    assert!(segments(&loop_board) < before_segments, "loop removal reduces the number of trace segments");
    // NetGraphSimplifier.cpp:568, 801 compare netPadGroups as a connectivity deficit;
    // copper.rs::net_pad_groups returns groups minus one: connected is 0, not 1.
    assert_eq!(copper::net_pad_groups(&loop_board, 0, None, 1e-3), 0, "loop removal leaves one connected pad group");
    assert_eq!(loop_board.traces[0].id, 0, "the kept shortest path reuses its original trace id");

    // Exercise the acyclic-tree exit at NetGraphSimplifier.cpp:496-499 with two traces,
    // not just the single-trace shortcut at 325-328.
    let mut tree = base.clone();
    tree.pads.push(pad(2, v(15.0, 10.0), 1));
    tree.traces = vec![
        trace(0, 0, vec![terminal(v(5.0, 10.0), 0), point(v(10.0, 10.0)), terminal(v(15.0, 10.0), 2)]),
        trace(1, 0, vec![terminal(v(15.0, 10.0), 2), terminal(v(25.0, 10.0), 1)]),
    ];
    let before_tree = points_snapshot(&tree);
    assert!(!simplify_net(0, &mut tree), "an existing tree has no simplification");
    assert_eq!(points_snapshot(&tree), before_tree, "an existing tree retains every trace point and attachment unchanged");

    // Four pad terminals keep all four arms of the crossing. The rebuilt trunk has
    // an interior node at (15, 10), and the other two arms branch into that node.
    let mut crossing = base.clone();
    crossing.pads.extend([pad(2, v(15.0, 5.0), 1), pad(3, v(15.0, 15.0), 1)]);
    crossing.traces = vec![
        trace(0, 0, vec![terminal(v(5.0, 10.0), 0), terminal(v(25.0, 10.0), 1)]),
        trace(1, 0, vec![terminal(v(15.0, 5.0), 2), terminal(v(15.0, 15.0), 3)]),
    ];
    assert!(has_self_intersections(0, &crossing), "same-layer interior crossing is detected");
    assert!(simplify_net(0, &mut crossing), "a crossing is rebuilt as a trunk and branches");
    assert!(!has_self_intersections(0, &crossing), "rebuilt branch junctions are not interior self-intersections");
    assert_eq!(copper::net_pad_groups(&crossing, 0, None, 1e-3), 0, "all four crossing terminals remain connected");
    let mut branches = 0;
    for tr in &crossing.traces {
        for p in &tr.points {
            if p.branch_trace < 0 {
                continue;
            }
            branches += 1;
            let host = crossing.traces.iter().find(|h| h.id == p.branch_trace);
            assert!(host.is_some(), "every rebuilt branch names an existing host trace");
            let host = host.unwrap();
            assert!(p.branch_point > 0 && p.branch_point + 1 < host.points.len() as i32,
                "every rebuilt branch names an interior host point");
            let host_pos = host.points[p.branch_point as usize].pos;
            assert_eq!((p.pos.x, p.pos.y), (host_pos.x, host_pos.y), "branch endpoints coincide with their host junction");
        }
    }
    assert_eq!(branches, 2, "the four-arm crossing rebuilds as one trunk with two branches");

    let mut vias = base.clone();
    vias.pads[1].layer_mask = 2;
    vias.vias = vec![
        Via { id: 17, net: 0, pos: v(35.0, 20.0), ..Via::default() },
        Via { id: 42, net: 0, pos: v(15.0, 10.0), ..Via::default() },
    ];
    // attached_via is an index, even when the input via ids disagree with indices.
    let via_end = TracePoint { attached_via: 1, ..point(v(15.0, 10.0)) };
    vias.traces = vec![
        trace(0, 0, vec![terminal(v(5.0, 10.0), 0), via_end.clone()]),
        trace(1, 1, vec![via_end, terminal(v(25.0, 10.0), 1)]),
    ];
    assert_eq!(prune_dangling_vias(&mut vias), 1, "only the via touching no same-net copper is removed");
    assert_eq!(vias.vias.len(), 1, "the via joining two trace layers is kept");
    assert_eq!((vias.vias[0].pos.x, vias.vias[0].pos.y), (15.0, 10.0), "the surviving via is the two-layer junction");
    assert!(vias.vias.iter().enumerate().all(|(i, via)| via.id == i as i32), "via ids equal indices after pruning");
    assert_eq!((vias.traces[0].points[1].attached_via, vias.traces[1].points[0].attached_via), (0, 0),
        "both trace attachments follow the compacted via index");
    assert_eq!(copper::net_pad_groups(&vias, 0, None, 1e-3), 0, "via pruning preserves cross-layer pad connectivity");
    assert_eq!(prune_dangling_vias(&mut vias), 0, "pruning the retained two-layer via again is a no-op");

    let mut frozen = original_loop.clone();
    frozen.vias.push(Via { id: 0, net: 0, pos: v(35.0, 20.0), ..Via::default() });
    let before_frozen = points_snapshot(&frozen);
    assert_eq!(simplify_all_nets(&mut frozen, Some(&[0])), 0, "frozen nets are skipped");
    assert_eq!(points_snapshot(&frozen), before_frozen, "frozen net points remain unchanged");
    assert_eq!(frozen.vias.len(), 1, "simplify_all_nets does not prune vias when no net changed");
    assert_eq!(simplify_all_nets(&mut frozen, None), 1, "simplify_all_nets counts the changed net");
    assert!(frozen.vias.is_empty(), "simplify_all_nets prunes dangling vias after a net changes");

    let mut ruled = original_loop;
    ruled.clr = ClearanceTable { rows: 2, layers: 2, mm: vec![0.2; 8], max: 0.2 };
    for tr in &mut ruled.traces {
        tr.rule = 1;
    }
    assert!(simplify_net(0, &mut ruled), "loop simplification also accepts a nondefault clearance row");
    assert_eq!(ruled.traces[0].rule, 1, "rebuilding preserves the source trace's clearance row");

    let mut reversed = vec![point(v(0.0, 0.0)), point(v(1.0, 0.0)), point(v(2.0, 0.0)), point(v(3.0, 0.0))];
    reversed[1].straight_before = true;
    reverse_points(&mut reversed);
    assert_eq!(reversed.iter().map(|p| p.pos.x).collect::<Vec<_>>(), vec![3.0, 2.0, 1.0, 0.0],
        "reverse_points reverses point order");
    assert_eq!(reversed.iter().map(|p| p.straight_before).collect::<Vec<_>>(), vec![false, false, false, true],
        "reverse_points keeps the straight flag on the same physical segment");
    assert!(point_in_ring(&base.outline, v(25.0, 15.0)), "retained polygon helper contains an interior point");
    assert!(!point_in_ring(&base.outline, v(51.0, 15.0)), "retained polygon helper rejects an exterior point");
    assert!(!point_in_ring(&[], v(0.0, 0.0)), "retained polygon helper treats an empty ring as outside");
}
