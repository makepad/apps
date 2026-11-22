use std::collections::BTreeMap;

use fab_elec_route::organic::{
    geom::Vec2,
    model::{ClearanceTable, LayerInfo, LayerIntent, Net, OBoard, Trace, TracePoint, Via},
    opt::{
        cleanup::{corner_segments, run, total_length, unported, CleanupOptions, CleanupStats},
        Finding, Guard,
    },
};

#[derive(Default)]
struct ScriptedGuard {
    calls: i32,
    syncs: i32,
    refuse: Vec<i32>,
    refusal_findings: BTreeMap<i32, Vec<Finding>>,
    taps: Vec<(i32, i32, Vec2, f32)>,
    judged_nets: Vec<Vec<i32>>,
    synced_nets: Vec<Vec<i32>>,
    judged_traces: Vec<Vec<Trace>>,
    synced_copper: Vec<Vec<u64>>,
}

impl Guard for ScriptedGuard {
    fn judge(&mut self, board: &OBoard, nets: &[i32]) -> bool {
        self.calls += 1;
        self.judged_nets.push(nets.to_vec());
        self.judged_traces.push(board.traces.clone());
        !self.refuse.contains(&self.calls)
    }

    fn sync(&mut self, board: &OBoard, nets: &[i32]) {
        self.syncs += 1;
        self.synced_nets.push(nets.to_vec());
        self.synced_copper.push(copper_bits(board));
    }

    fn refusal(&self) -> &[Finding] {
        if self.refuse.contains(&self.calls) {
            self.refusal_findings.get(&self.calls).map(Vec::as_slice).unwrap_or(&[])
        } else { &[] }
    }

    fn tap_connected(&self, net: i32, layer: i32, pos: Vec2, radius: f32) -> bool {
        self.taps.iter().any(|&(n, l, p, r)| {
            n == net && l == layer && p.x.to_bits() == pos.x.to_bits()
                && p.y.to_bits() == pos.y.to_bits() && r.to_bits() == radius.to_bits()
        })
    }
}

// Exhaustive destructuring makes this compare every Trace, TracePoint and Via
// field, including unused export geometry and metadata; floats compare by bits.
fn copper_bits(b: &OBoard) -> Vec<u64> {
    fn xy(out: &mut Vec<u64>, p: Vec2) {
        out.extend([p.x.to_bits() as u64, p.y.to_bits() as u64]);
    }
    fn poly(out: &mut Vec<u64>, points: &[Vec2]) {
        out.push(points.len() as u64);
        for &p in points {
            xy(out, p);
        }
    }
    let mut out = vec![b.traces.len() as u64];
    for tr in &b.traces {
        let Trace {
            id,
            net,
            layer,
            width,
            points,
            smoothed,
            point_widths,
            teardrop_front,
            teardrop_back,
            length,
            is_routed,
            locked,
            imported,
            pair_frozen,
            rule,
        } = tr;
        out.extend([*id as u64, *net as u64, *layer as u64, width.to_bits() as u64, points.len() as u64]);
        for p in points {
            let TracePoint { pos, velocity, width, is_fixed, terminal_pad, attached_via, branch_trace, branch_point, straight_before } = p;
            xy(&mut out, *pos);
            xy(&mut out, *velocity);
            out.extend([
                width.to_bits() as u64,
                *is_fixed as u64,
                *terminal_pad as u64,
                *attached_via as u64,
                *branch_trace as u64,
                *branch_point as u64,
                *straight_before as u64,
            ]);
        }
        poly(&mut out, smoothed);
        out.push(point_widths.len() as u64);
        out.extend(point_widths.iter().map(|w| w.to_bits() as u64));
        poly(&mut out, teardrop_front);
        poly(&mut out, teardrop_back);
        out.extend([length.to_bits() as u64, *is_routed as u64, *locked as u64, *imported as u64, *pair_frozen as u64, *rule as u64]);
    }
    out.push(b.vias.len() as u64);
    for v in &b.vias {
        let Via {
            id,
            net,
            pos,
            velocity,
            diameter,
            drill,
            layer_start,
            layer_end,
            connected_pad,
            is_fixed,
            locked,
            imported,
            pair_frozen,
            kind,
            rule,
            src,
        } = v;
        out.extend([*id as u64, *net as u64]);
        xy(&mut out, *pos);
        xy(&mut out, *velocity);
        out.extend([
            diameter.to_bits() as u64,
            drill.to_bits() as u64,
            *layer_start as u64,
            *layer_end as u64,
            *connected_pad as u64,
            *is_fixed as u64,
            *locked as u64,
            *imported as u64,
            *pair_frozen as u64,
            *kind as u64,
            *rule as u64,
            *src as u64,
        ]);
    }
    out
}

// PartialEq covers every counter/name; the additional float bits also
// distinguish signed zeros. There are no NaNs in these hand-built fixtures.
fn stats_bits(s: &CleanupStats) -> (CleanupStats, Vec<u64>) {
    let mut bits = vec![s.length_before.to_bits(), s.length_after.to_bits()];
    bits.extend(s.steps.iter().map(|s| s.length_gain.to_bits()));
    (s.clone(), bits)
}

#[test]
fn organic_cleanup_preserves_skeletons_guards_attachments_and_determinism() {
    let v = Vec2::new;
    let bits = |p: Vec2| (p.x.to_bits(), p.y.to_bits());
    let trace = |id, net, pos: &[Vec2]| Trace {
        id,
        net,
        layer: 0,
        width: 0.2,
        is_routed: true,
        // Sentinels exercise unchanged-skeleton restoration and final rollback
        // of metadata/export geometry, not merely endpoint positions.
        length: 9.25,
        smoothed: pos.to_vec(),
        point_widths: vec![0.2; pos.len()],
        teardrop_front: vec![pos[0]],
        teardrop_back: vec![pos[pos.len() - 1]],
        points: pos.iter().map(|&pos| TracePoint {
            pos, width: 0.2, velocity: v(-0.0, 0.125), ..TracePoint::default()
        }).collect(),
        ..Trace::default()
    };
    let base = OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        clr: ClearanceTable { rows: 1, layers: 2, mm: vec![0.2; 2], max: 0.2 },
        nets: (0..3).map(|id| Net { id, ..Net::default() }).collect(),
        ..OBoard::default()
    };
    let off = CleanupOptions {
        rules: false, collinear: false, vias: false, unloop: false,
        detours: false, acute: false, ..CleanupOptions::default()
    };
    // Cleanup.h:72 defaults rules to true; cpp:1410-1517/1682-1684 is the
    // explicitly omitted C5c step. Only its requested switch is reported.
    assert_eq!(unported(&CleanupOptions::default()), vec!["rules"], "unported: requested C5c rules are reported");
    assert!(unported(&off).is_empty(), "unported: no report when C5c rules are disabled");

    let straight = trace(0, 1, &[v(0.0, 10.0), v(0.8, 10.0), v(1.6, 10.0), v(2.4, 10.0), v(3.2, 10.0)]);
    let detour = trace(1, 0, &[v(0.0, 0.0), v(0.0, 2.0), v(4.0, 2.0), v(4.0, 0.0)]);
    let input = OBoard { traces: vec![straight.clone(), detour.clone()], ..base.clone() };
    let opt = CleanupOptions { collinear: true, detours: true, ..off.clone() };
    let mut b = input.clone();
    let mut guard = ScriptedGuard::default();
    let stats = run(&mut b, &opt, &mut guard);
    // cpp:1665-1669 decimates at 2e-5. All five y=10 nodes have distance 0
    // from the chord, so cpp:567-592 selects k=4, preserving only the ends.
    // The U's gain is 2+4+2-4 = 4 mm: cpp:1045-1047/1070 picks its full
    // straight span first; cpp:1145-1147 names exactly one straight shortcut.
    // Its step judge observes the straight trace's two-node skeleton before
    // cpp:1696-1714 restores that unchanged skeleton's original particles.
    assert_eq!(guard.judged_traces[0][0].points.len(), 2, "collinear: working skeleton has exactly two nodes");
    assert_eq!(bits(guard.judged_traces[0][0].points[0].pos), bits(straight.points[0].pos), "collinear: skeleton keeps the first endpoint");
    assert_eq!(bits(guard.judged_traces[0][0].points[1].pos), bits(straight.points[4].pos), "collinear: skeleton keeps the last endpoint");
    let single = |tr: Trace| OBoard { traces: vec![tr], ..OBoard::default() };
    assert_eq!(copper_bits(&single(b.traces[0].clone())), copper_bits(&single(straight.clone())), "collinear: output restores every original field bit for bit");
    assert_eq!(corner_segments(&single(b.traces[0].clone()), 1e-4), 1, "collinear: particles still describe one corner-to-corner segment");
    assert!(total_length(&single(detour.clone())) - total_length(&single(b.traces[1].clone())) > opt.detour_min_gain as f64, "detours: U shortcut saves more than detour_min_gain");
    assert_eq!((stats.straight_shortcuts, stats.elbows), (1, 0), "detours: statistics name one straight shortcut and no elbow");
    assert_eq!(stats.steps.iter().find(|s| s.name == "detours").unwrap().applied, 1, "detours: the detours step records the shortcut");
    // cpp:1630-1636 adaptation: only net 0 changes during the detours step.
    // cpp:1704-1716 then restores five particles on net 1 and resamples net 0
    // (4/0.8 -> five segments), so the final changed set is sorted [0,1].
    assert_eq!((stats.judge_calls, guard.calls, guard.syncs), (2, 2, 0), "changed nets: only detours and output need judging");
    assert_eq!(guard.judged_nets, vec![vec![0], vec![0, 1]], "changed nets: output restoration and resampling use sorted unique nets");

    // cpp:1301 accepts this 135-degree turn, cpp:1304 limits the leg to
    // 0.6 mm, and acutePrefer135=false gives two vertices (1307-1308).
    // cpp:1256-1264 places them at b-d0*0.6 and b+d1*0.6. No obstacles
    // exist; cpp:1331-1333 therefore applies one chamfer, replacing one
    // interior node by two (four nodes including the original endpoints).
    let mut acute = OBoard { traces: vec![trace(0, 0, &[v(0.0, 0.0), v(2.0, 0.0), v(1.0, 1.0)])], ..base.clone() };
    let acute_opt = CleanupOptions { acute: true, acute_prefer135: false, resample: false, ..off.clone() };
    let mut acute_guard = ScriptedGuard::default();
    let acute_stats = run(&mut acute, &acute_opt, &mut acute_guard);
    let expected_front = v(2.0, 0.0) - v(1.0, 0.0) * 0.6f32;
    let expected_back = v(2.0, 0.0) + v(-1.0, 1.0).normalized() * 0.6f32;
    assert_eq!((acute_stats.chamfers, acute.traces[0].points.len()), (1, 4), "acute: one corner becomes a two-node chamfer");
    assert_eq!(bits(acute.traces[0].points[1].pos), bits(expected_front), "acute: first chamfer node follows the reference tangent");
    assert_eq!(bits(acute.traces[0].points[2].pos), bits(expected_back), "acute: second chamfer node follows the reference tangent");

    let mut vias_input = OBoard {
        vias: vec![
            Via { id: 0, net: 2, pos: v(2.0, 0.0), diameter: 0.6, drill: 0.3, ..Via::default() },
            Via { id: 1, net: 1, pos: v(10.0, 10.0), diameter: 0.6, drill: 0.3, is_fixed: true, ..Via::default() },
        ],
        traces: vec![
            trace(0, 2, &[v(0.0, 0.0), v(2.0, 0.0)]),
            trace(1, 2, &[v(2.0, 0.0), v(4.0, 0.0)]),
            trace(2, 1, &[v(10.0, 10.0), v(12.0, 10.0)]),
        ],
        ..base.clone()
    };
    vias_input.traces[0].points[1].attached_via = 0;
    vias_input.traces[1].points[0].attached_via = 0;
    vias_input.traces[2].points[0].attached_via = 1;
    let mut vias = vias_input.clone();
    let mut via_guard = ScriptedGuard::default();
    let via_opt = CleanupOptions { collinear: true, vias: true, resample: false, ..off.clone() };
    let via_stats = run(&mut vias, &via_opt, &mut via_guard);
    // cpp:702-724 merges the two same-layer runs and deletes via 0; 1394-1406
    // remaps surviving via 1 to 0, including its unchanged net-1 attachment.
    // The merge step (1689) then drops the now-free collinear joint: two
    // changed steps, both affecting net 2 only. Resample=false adds no further
    // change in the fields changed_nets compares; the output phase still
    // recomputes length and clears export geometry (cpp:1717-1721).
    // The comparison must not confuse net 1's renumbering with copper.
    assert_eq!((via_stats.vias_removed, vias.vias.len(), vias.traces.len()), (1, 1, 2), "vias: degenerate via and one merged trace are removed");
    for (i, via) in vias.vias.iter().enumerate() { assert_eq!(via.id, i as i32, "vias: every surviving id equals its index"); }
    for tr in &vias.traces {
        for p in &tr.points {
            assert!(p.attached_via == -1 || (p.attached_via >= 0 && (p.attached_via as usize) < vias.vias.len()), "vias: every attachment is absent or a valid index");
        }
    }
    assert_eq!(vias.traces[1].points[0].attached_via, 0, "vias: unchanged net keeps its renumbered attachment");
    assert_eq!(via_guard.judged_nets, vec![vec![2], vec![2]], "vias: content comparison ignores unrelated via-id renumbering");
    assert_eq!(via_stats.judge_calls, 2, "vias: no empty-change final judge");

    let shallow = trace(0, 2, &[v(0.0, 10.0), v(1.0, 10.005), v(2.0, 10.0)]);
    let rollback_input = OBoard { traces: vec![shallow, detour], ..base.clone() };
    let step_opt = CleanupOptions { resample: false, ..opt.clone() };
    let mut step_board = rollback_input.clone();
    let mut step_guard = ScriptedGuard { refuse: vec![2], ..ScriptedGuard::default() };
    let step_stats = run(&mut step_board, &step_opt, &mut step_guard);
    // The shallow bend is > prep's 2e-5 (1668) but < collinearTol=0.01
    // (h:80), so call 1 accepts its decimation (1680). The U shortcut is
    // call 2 (1686), refused here; cpp:1639-1648 restores its four nodes and
    // straightShortcuts, while keeping the previously accepted decimation.
    // resample=false leaves no further change in the fields changed_nets
    // compares; the output phase still recomputes length and clears export
    // geometry (cpp:1717-1721).
    assert_eq!((step_stats.step_reverts, step_guard.calls, step_guard.syncs), (1, 2, 0), "step guard: one refused step restores locally without sync");
    assert!(step_stats.steps.iter().find(|s| s.name == "detours").unwrap().reverted, "step guard: the refused step is detours");
    assert_eq!(copper_bits(&single(step_board.traces[1].clone())), copper_bits(&single(rollback_input.traces[1].clone())), "step guard: the entire detour trace is restored");
    assert_eq!(step_board.traces[0].points.len(), 2, "step guard: the accepted collinear step stands");
    assert_eq!(step_stats.straight_shortcuts, 0, "step guard: shortcut total rolls back with the refused step");
    assert_eq!(step_guard.judged_nets, vec![vec![2], vec![0]], "step guard: each call names only its changed net");

    // Our net-fixpoint policy (not in cpp:1630-1648): both U detours have
    // gain 2+4+2-4 = 4 mm, so cpp:1045-1047/1070 shortens both. Net A=0
    // alone is named in the first refusal; restore it and judge only B=1.
    let partial_input = OBoard {
        traces: vec![
            trace(0, 0, &[v(0.0, 0.0), v(0.0, 2.0), v(4.0, 2.0), v(4.0, 0.0)]),
            trace(1, 1, &[v(0.0, 10.0), v(0.0, 12.0), v(4.0, 12.0), v(4.0, 10.0)]),
        ],
        ..base.clone()
    };
    let mut partial_board = partial_input.clone();
    let mut partial_guard = ScriptedGuard {
        refuse: vec![1],
        refusal_findings: [(1, vec![Finding { pos: None, nets: vec![0] }])].into_iter().collect(),
        ..ScriptedGuard::default()
    };
    let partial_opt = CleanupOptions { detours: true, resample: false, ..off.clone() };
    let partial_stats = run(&mut partial_board, &partial_opt, &mut partial_guard);
    let partial_step = partial_stats.steps.iter().find(|s| s.name == "detours").unwrap();
    let net_a = partial_board.traces.iter().find(|tr| tr.net == 0).unwrap();
    let net_b = partial_board.traces.iter().find(|tr| tr.net == 1).unwrap();
    assert_eq!(copper_bits(&single(net_a.clone())), copper_bits(&single(partial_input.traces[0].clone())), "net fixpoint: net A's entire trace is restored bit for bit");
    assert!(total_length(&single(partial_input.traces[1].clone())) - total_length(&single(net_b.clone())) > partial_opt.detour_min_gain as f64, "net fixpoint: net B keeps its shortened detour");
    assert_eq!((partial_step.reverted, partial_stats.step_reverts, partial_stats.reverted), (false, 0, false), "net fixpoint: partial accept does not revert the step or pass");
    assert_eq!((partial_step.nets_reverted, partial_stats.nets_reverted), (1, 1), "net fixpoint: step and pass count one restored net");
    assert_eq!((partial_stats.judge_calls, partial_guard.calls, partial_guard.syncs), (2, 2, 0), "net fixpoint: detours is judged twice without a final call or sync");
    assert_eq!(partial_guard.judged_nets, vec![vec![0, 1], vec![1]], "net fixpoint: retry judges net B only");
    assert_eq!((partial_step.applied, partial_stats.straight_shortcuts), (1, 1), "net fixpoint: edit counters count only net B's standing shortcut, not reverted net A's");

    let mut final_input = rollback_input.clone();
    final_input.vias.push(Via {
        id: 0, net: 1, pos: v(22.0, 0.0), diameter: 0.6, drill: 0.3,
        layer_start: 0, layer_end: 0, ..Via::default()
    });
    final_input.traces.push(trace(2, 1, &[v(20.0, 0.0), v(22.0, 0.0)]));
    final_input.traces.push(trace(3, 1, &[v(22.0, 0.0), v(24.0, 0.0)]));
    final_input.traces[2].points[1].attached_via = 0;
    final_input.traces[3].points[0].attached_via = 0;
    let mut final_board = final_input.clone();
    let mut final_guard = ScriptedGuard { refuse: vec![5], ..ScriptedGuard::default() };
    let final_opt = CleanupOptions { vias: true, ..opt.clone() };
    let final_stats = run(&mut final_board, &final_opt, &mut final_guard);
    // Calls 1/2/3/4 accept collinear/vias/detours/merge: cpp:702-724 removes
    // the degenerate via, and merge (1689) removes its freed collinear joint.
    // Output resampling (1716) expands all three two-node skeletons, so call
    // 5 is the final guard (1724-1737).
    // Its refusal restores BOTH input vectors (1734-1735). The Guard adapter
    // must sync exactly once, with the input/output difference: sorted [0,1,2].
    assert_eq!(final_stats.vias_removed, 1, "final guard: an accepted vias step removed the input via before rollback");
    assert!(final_stats.reverted, "final guard: refusal marks the complete pass reverted");
    assert_eq!(copper_bits(&final_board), copper_bits(&final_input), "final guard: every input trace, attached_via and via field restores bit for bit");
    assert_eq!((final_stats.judge_calls, final_guard.calls, final_guard.syncs), (5, 5, 1), "final guard: final refusal makes exactly one sync");
    assert_eq!(final_guard.judged_nets, vec![vec![2], vec![1], vec![0], vec![1], vec![0, 1, 2]], "final guard: last judged snapshot is compared with output");
    assert_eq!(final_guard.synced_nets, vec![vec![0, 1, 2]], "final guard: sync names exactly the input/output changed nets");
    assert_eq!(final_guard.synced_copper, vec![copper_bits(&final_input)], "final guard: sync sees the restored input");

    // With stepGuard=false (h:101/cpp:1631), only the final guard sees edits;
    // its comparison is against input, and its refusal follows 1734-1735.
    let mut final_only = rollback_input.clone();
    let mut final_only_guard = ScriptedGuard { refuse: vec![1], ..ScriptedGuard::default() };
    let final_only_stats = run(&mut final_only, &CleanupOptions { step_guard: false, ..opt.clone() }, &mut final_only_guard);
    assert_eq!((final_only_stats.judge_calls, final_only_guard.syncs), (1, 1), "final-only guard: no step calls and one restore sync");
    assert_eq!(final_only_guard.judged_nets, vec![vec![0, 2]], "final-only guard: comparison covers all edits from input");
    assert_eq!(copper_bits(&final_only), copper_bits(&rollback_input), "final-only guard: refusal restores input bit for bit");

    let untouched_input = OBoard { traces: vec![trace(0, 0, &[v(0.0, 0.0), v(2.0, 0.0)])], ..base };
    let mut untouched = untouched_input.clone();
    let mut untouched_guard = ScriptedGuard::default();
    let untouched_stats = run(&mut untouched, &CleanupOptions { rules: false, ..CleanupOptions::default() }, &mut untouched_guard);
    // Two points cannot decimate (562), unloop (927), shorten (1014) or
    // chamfer (1296); no vias exist. 1696-1714 restores the same points.
    // Every changed_nets set is empty, hence zero judges and syncs.
    assert_eq!((untouched_stats.judge_calls, untouched_guard.calls, untouched_guard.syncs), (0, 0, 0), "untouched: no guard calls for an unchanged board");
    assert_eq!(copper_bits(&untouched), copper_bits(&untouched_input), "untouched: all copper metadata and geometry stay bit-identical");

    // cpp:898,1070,1113 stable candidate orders and serial board traversals
    // (the clock/work budget removed by the brief) make repeated input and
    // options follow the same operations and collect the same statistics.
    let mut repeated = input.clone();
    let mut repeated_guard = ScriptedGuard::default();
    let repeated_stats = run(&mut repeated, &opt, &mut repeated_guard);
    assert_eq!(copper_bits(&repeated), copper_bits(&b), "determinism: same input produces bit-identical copper");
    assert_eq!(stats_bits(&repeated_stats), stats_bits(&stats), "determinism: all stats and float bits agree");
    assert_eq!(repeated_guard.judged_nets, guard.judged_nets, "determinism: judge calls have identical changed-net order");
}
