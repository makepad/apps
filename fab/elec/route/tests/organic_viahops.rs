use std::collections::BTreeMap;

use fab_elec_route::organic::{
    geom::Vec2,
    model::{ClearanceTable, DesignRules, Keepout, LayerInfo, LayerIntent, Net, OBoard, Pad, Trace, TracePoint, Via, Zone},
    opt::{
        hops::count_hops,
        viahops::{class_name, run, total_length, HopClass, ViaHopOptions, ViaHopStats},
        Finding, Guard,
    },
};

#[derive(Default)]
struct ScriptedGuard {
    calls: i32,
    refused: BTreeMap<i32, Vec<Finding>>,
    taps: Vec<(i32, i32, Vec2, f32)>,
    judged_nets: Vec<Vec<i32>>,
    judged_vias: Vec<usize>,
}
impl Guard for ScriptedGuard {
    fn judge(&mut self, b: &OBoard, nets: &[i32]) -> bool {
        self.calls += 1;
        self.judged_nets.push(nets.to_vec());
        self.judged_vias.push(b.vias.len());
        !self.refused.contains_key(&self.calls)
    }
    fn sync(&mut self, _b: &OBoard, _nets: &[i32]) {
        panic!("guard sync: ViaHops only restores unaccepted edits");
    }
    fn refusal(&self) -> &[Finding] {
        self.refused.get(&self.calls).map(Vec::as_slice).unwrap_or(&[])
    }
    fn tap_connected(&self, net: i32, layer: i32, pos: Vec2, radius: f32) -> bool {
        self.taps.iter().any(|&(n, l, p, r)| {
            n == net
                && l == layer
                && p.x.to_bits() == pos.x.to_bits()
                && p.y.to_bits() == pos.y.to_bits()
                && r.to_bits() == radius.to_bits()
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
            let TracePoint {
                pos,
                velocity,
                width,
                is_fixed,
                terminal_pad,
                attached_via,
                branch_trace,
                branch_point,
                straight_before,
            } = p;
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
        out.extend([
            length.to_bits() as u64,
            *is_routed as u64,
            *locked as u64,
            *imported as u64,
            *pair_frozen as u64,
            *rule as u64,
        ]);
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

// Include every non-copper board field too: rollback/no-op assertions compare
// the entire OBoard by bits, with exhaustive destructuring of nested structs.
fn board_bits(b: &OBoard) -> Vec<u64> {
    fn xy(o: &mut Vec<u64>, p: Vec2) {
        o.extend([p.x.to_bits() as u64, p.y.to_bits() as u64]);
    }
    fn poly(o: &mut Vec<u64>, p: &[Vec2]) {
        o.push(p.len() as u64);
        for &v in p {
            xy(o, v);
        }
    }
    let OBoard {
        layers,
        outline,
        cutouts,
        pads,
        nets,
        vias: _,
        traces: _,
        keepouts,
        zones,
        rules,
        clr,
        pour_rules,
        min_track_width_for,
        track_width_fixed,
    } = b;
    let mut o = copper_bits(b);
    o.push(layers.len() as u64);
    for l in layers {
        let LayerInfo { index, intent, plane_net } = l;
        o.extend([*index as u64, *intent as u64, *plane_net as u64]);
    }
    poly(&mut o, outline);
    o.push(cutouts.len() as u64);
    for c in cutouts {
        poly(&mut o, c);
    }
    o.push(pads.len() as u64);
    for p in pads {
        let Pad {
            id,
            center,
            size,
            drill_diameter,
            shape,
            layer_mask,
            net,
            is_smd,
            rotation,
            local_size,
            drill_size,
            round_rect_ratio,
            polygon,
            component,
            piece_of,
            rule,
            number,
            fanout,
            via_drill_gap,
        } = p;
        o.push(*id as u64);
        xy(&mut o, *center);
        xy(&mut o, *size);
        o.extend([
            drill_diameter.to_bits() as u64,
            *shape as u64,
            *layer_mask,
            *net as u64,
            *is_smd as u64,
            rotation.to_bits() as u64,
        ]);
        xy(&mut o, *local_size);
        xy(&mut o, *drill_size);
        o.push(round_rect_ratio.to_bits() as u64);
        poly(&mut o, polygon);
        o.extend([
            *component as u64,
            *piece_of as u64,
            *rule as u64,
            *number as u64,
            *fanout as u64,
            via_drill_gap.to_bits() as u64,
        ]);
    }
    o.push(nets.len() as u64);
    for n in nets {
        let Net {
            id,
            diff_pair_partner,
            pair_gap,
            via_diameter,
            via_drill,
            via_rule,
            via_in_pad,
            target_length,
            is_diff_pair,
            current,
        } = n;
        o.extend([
            *id as u64,
            *diff_pair_partner as u64,
            pair_gap.to_bits() as u64,
            via_diameter.to_bits() as u64,
            via_drill.to_bits() as u64,
            *via_rule as u64,
            *via_in_pad as u64,
            target_length.to_bits() as u64,
            *is_diff_pair as u64,
            *current as u64,
        ]);
    }
    o.push(keepouts.len() as u64);
    for k in keepouts {
        let Keepout {
            id,
            poly: p,
            layer_mask,
            clearance,
            no_tracks,
            no_vias,
            only_net,
        } = k;
        o.push(*id as u64);
        poly(&mut o, p);
        o.extend([*layer_mask, clearance.to_bits() as u64, *no_tracks as u64, *no_vias as u64, *only_net as u64]);
    }
    o.push(zones.len() as u64);
    for z in zones {
        let Zone {
            net,
            layer_mask,
            outline,
            is_rule_area,
        } = z;
        o.extend([*net as u64, *layer_mask, *is_rule_area as u64]);
        poly(&mut o, outline);
    }
    let DesignRules {
        default_trace_width,
        min_trace_width,
        min_clearance,
        via_diameter,
        via_drill,
        diff_pair_spacing,
        board_edge_clearance,
        hole_clearance,
        hole_to_hole,
    } = rules;
    for x in [
        default_trace_width,
        min_trace_width,
        min_clearance,
        via_diameter,
        via_drill,
        diff_pair_spacing,
        board_edge_clearance,
        hole_clearance,
        hole_to_hole,
    ] {
        o.push(x.to_bits() as u64);
    }
    let ClearanceTable { rows, layers, mm, max } = clr;
    o.extend([*rows as u64, *layers as u64, mm.len() as u64, max.to_bits() as u64]);
    o.extend(mm.iter().map(|x| x.to_bits() as u64));
    o.push(pour_rules.len() as u64);
    o.extend(pour_rules.iter().map(|&row| row as u64));
    o.push(min_track_width_for.len() as u64);
    for (&(n, l), w) in min_track_width_for {
        o.extend([n as u64, l as u64, w.to_bits() as u64]);
    }
    o.push(track_width_fixed.len() as u64);
    o.extend(track_width_fixed.iter().map(|&v| v as u64));
    o
}

fn stats_bits(s: &ViaHopStats) -> Vec<u64> {
    let ViaHopStats {
        hops_before,
        hops_after,
        candidates,
        by_class,
        removed_empty,
        go_around,
        pushed,
        flipped,
        relayered,
        slid,
        merged,
        absorbed,
        unresolved,
        unresolved_by_class,
        one_trace_reachable,
        resolvable,
        resolvable_resolved,
        unresolved_sites,
        attempts,
        search_fails,
        length_rejects,
        check_rejects,
        vias_before,
        vias_after,
        length_before,
        length_after,
        edits,
        reverts,
        guard_iterations,
        spike_candidates,
        spike_merged,
        spike_straightened,
        plane_tap_candidates,
        plane_taps_removed,
        pair_skipped,
        pair_edits,
        full_revert,
    } = s;
    let mut out = vec![length_before.to_bits(), length_after.to_bits(), *full_revert as u64];
    for v in [
        hops_before,
        hops_after,
        candidates,
        removed_empty,
        go_around,
        pushed,
        flipped,
        relayered,
        slid,
        merged,
        absorbed,
        unresolved,
        one_trace_reachable,
        resolvable,
        resolvable_resolved,
        attempts,
        search_fails,
        length_rejects,
        check_rejects,
        vias_before,
        vias_after,
        edits,
        reverts,
        guard_iterations,
        spike_candidates,
        spike_merged,
        spike_straightened,
        plane_tap_candidates,
        plane_taps_removed,
        pair_skipped,
        pair_edits,
    ] {
        out.push(*v as u64);
    }
    out.extend(by_class.iter().map(|&v| v as u64));
    out.extend(unresolved_by_class.iter().map(|&v| v as u64));
    out.push(unresolved_sites.len() as u64);
    for site in unresolved_sites {
        let fab_elec_route::organic::opt::viahops::Site {
            via_a,
            via_b,
            cls,
            reachable,
        } = site;
        out.extend([*via_a as u64, *via_b as u64, *cls as u64, *reachable as u64]);
    }
    out
}

fn add_hop(b: &mut OBoard, net: i32, y: f32) {
    let v = Vec2::new;
    let a = b.vias.len() as i32;
    let first = b.traces.len() as i32;
    for (id, pos) in [(a, v(-1.0, y)), (a + 1, v(1.0, y))] {
        b.vias.push(Via {
            id,
            net,
            pos,
            diameter: 0.6,
            drill: 0.3,
            layer_start: 0,
            layer_end: 1,
            velocity: v(0.125, -0.0),
            src: 100 + id,
            ..Via::default()
        });
    }
    for (id, layer, points) in [
        (
            first,
            0,
            vec![
                TracePoint {
                    pos: v(-4.0, y),
                    ..TracePoint::default()
                },
                TracePoint {
                    pos: v(-1.0, y),
                    attached_via: a,
                    ..TracePoint::default()
                },
            ],
        ),
        (
            first + 1,
            1,
            vec![
                TracePoint {
                    pos: v(-1.0, y),
                    attached_via: a,
                    ..TracePoint::default()
                },
                TracePoint {
                    pos: v(1.0, y),
                    attached_via: a + 1,
                    ..TracePoint::default()
                },
            ],
        ),
        (
            first + 2,
            0,
            vec![
                TracePoint {
                    pos: v(1.0, y),
                    attached_via: a + 1,
                    ..TracePoint::default()
                },
                TracePoint {
                    pos: v(4.0, y),
                    ..TracePoint::default()
                },
            ],
        ),
    ] {
        b.traces.push(Trace {
            id,
            net,
            layer,
            width: 0.25,
            is_routed: true,
            smoothed: points.iter().map(|p| p.pos).collect(),
            point_widths: vec![0.25; 2],
            length: if layer == 1 { 2.0 } else { 3.0 },
            points,
            ..Trace::default()
        });
    }
}
fn valid_vias(b: &OBoard, property: &str) {
    // ViaHops.cpp:1783-1792 compacts in old index order, assigns each kept via's
    // new index as its ID and remaps every nonnegative attachment.
    for (i, v) in b.vias.iter().enumerate() {
        assert_eq!(v.id, i as i32, "{property}: via ID equals index");
    }
    for t in &b.traces {
        for p in &t.points {
            if p.attached_via >= 0 {
                assert!((p.attached_via as usize) < b.vias.len(), "{property}: every attached_via is valid");
                assert_eq!(b.vias[p.attached_via as usize].net, t.net, "{property}: attachment keeps its net");
            }
        }
    }
}

#[test]
fn organic_viahops_strategies_guard_skips_and_repeatability() {
    let v = Vec2::new;
    let bits = |p: Vec2| (p.x.to_bits(), p.y.to_bits());
    let edit_counts = |s: &ViaHopStats| {
        [
            ("edits", s.edits),
            ("removed_empty", s.removed_empty),
            ("go_around", s.go_around),
            ("pushed", s.pushed),
            ("flipped", s.flipped),
            ("relayered", s.relayered),
            ("slid", s.slid),
            ("merged", s.merged),
            ("absorbed", s.absorbed),
            ("spike_merged", s.spike_merged),
            ("spike_straightened", s.spike_straightened),
            ("plane_taps_removed", s.plane_taps_removed),
            ("pair_edits", s.pair_edits),
        ]
    };
    let shell = OBoard {
        layers: vec![
            LayerInfo {
                index: 0,
                intent: LayerIntent::Signal,
                plane_net: -1,
            },
            LayerInfo {
                index: 1,
                intent: LayerIntent::Signal,
                plane_net: -1,
            },
        ],
        clr: ClearanceTable {
            rows: 1,
            layers: 2,
            mm: vec![0.2; 2],
            max: 0.2,
        },
        nets: vec![Net { id: 0, ..Net::default() }, Net { id: 1, ..Net::default() }],
        ..OBoard::default()
    };
    // ViaHops.cpp:398 uses the requested grid pitch. 0.2 mm gives a tiny
    // 51 x 41 grid in the default 4 mm window (cpp:401); the window has room for each detour.
    let opt = ViaHopOptions {
        grid_pitch: 0.2,
        ..ViaHopOptions::default()
    };
    let mut one = shell.clone();
    add_hop(&mut one, 0, 0.0);
    let mut empty = one.clone();
    let mut stats = ViaHopStats::default();
    let mut guard = ScriptedGuard::default();
    // ViaHops.cpp:287-327 accepts the 2 mm R with degree-two vias and P/Q on X.
    // 1415 classifies Empty; 2256-2259 chooses S_Empty. 725-759 keeps P's ID,
    // joins its free end to Q's free end, removes R/Q and marks both vias dead.
    // countHops:2035,2060-2061 counts the one short R before; no vias remain after.
    assert!(run(&mut empty, &opt, &mut stats, &mut guard), "empty crossing: pass accepts");
    assert_eq!(
        (empty.vias.len(), empty.traces.len(), stats.removed_empty),
        (0, 1, 1),
        "empty crossing: two vias disappear and P/Q join"
    );
    assert_eq!(
        (empty.traces[0].id, empty.traces[0].layer),
        (0, 0),
        "empty crossing: merged connection keeps P's ID and layer X"
    );
    assert_eq!(
        (
            bits(empty.traces[0].points[0].pos),
            bits(empty.traces[0].points.last().unwrap().pos)
        ),
        (bits(v(-4.0, 0.0)), bits(v(4.0, 0.0))),
        "empty crossing: both remote P/Q ends survive"
    );
    valid_vias(&empty, "empty crossing");
    assert_eq!(
        (stats.hops_before, stats.hops_after),
        (1, 0),
        "empty crossing: reference hop metric changes one to zero"
    );
    assert_eq!(
        stats.hops_before,
        count_hops(&one, opt.max_hop_length, false, None, None, false).all(),
        "empty crossing: before count uses count_hops"
    );
    assert_eq!(
        stats.hops_after,
        count_hops(&empty, opt.max_hop_length, false, None, None, false).all(),
        "empty crossing: after count uses count_hops"
    );
    // cpp:1833-1837 adds raw segment lengths; the unchanged input has 3+2+3 mm.
    assert_eq!(
        total_length(&one).to_bits(),
        8.0f64.to_bits(),
        "total_length: raw input length is eight millimetres"
    );
    // cpp:1826-1829 supplies these exact public labels.
    assert_eq!(
        [
            class_name(HopClass::Empty),
            class_name(HopClass::OneTrace),
            class_name(HopClass::PadOrVia),
            class_name(HopClass::Multiple)
        ],
        ["empty", "one-trace", "pad/via", "multiple"],
        "class_name: reference labels are preserved"
    );

    let mut crossing = one.clone();
    crossing.traces.push(Trace {
        id: 3,
        net: 1,
        layer: 0,
        width: 0.25,
        is_routed: true,
        points: vec![
            TracePoint {
                pos: v(0.0, -1.0),
                ..TracePoint::default()
            },
            TracePoint {
                pos: v(0.0, 1.0),
                ..TracePoint::default()
            },
        ],
        ..Trace::default()
    });
    let mut around = crossing.clone();
    let mut around_stats = ViaHopStats::default();
    let mut around_guard = ScriptedGuard::default();
    // cpp:1382-1389,1417 finds exactly one foreign net/trace. cpp:395-497 has
    // free grid rows beyond both ends of K inside the +/-4 mm window; 525-552
    // pulls only legal chords, and the detour is below maxExtraLength=6 (691-696).
    // cpp:2259 thus chooses Around, keeps K, and removes the two hop vias.
    assert!(
        run(&mut around, &opt, &mut around_stats, &mut around_guard),
        "one crossing: pass accepts"
    );
    assert_eq!(
        (around_stats.go_around, around_stats.removed_empty, around.vias.len()),
        (1, 0, 0),
        "one crossing: exactly one go-around removes its vias"
    );
    let detour = around.traces.iter().find(|t| t.net == 0).unwrap();
    assert_eq!(detour.layer, 0, "one crossing: detour stays on X");
    assert!(
        detour.points.iter().any(|p| p.pos.y.abs() > 1.0),
        "one crossing: detour goes beyond an end of the foreign trace"
    );
    assert_eq!(
        around.traces.iter().filter(|t| t.net == 1).count(),
        1,
        "one crossing: the foreign trace survives"
    );

    let mut two = one.clone();
    add_hop(&mut two, 1, 20.0);
    let mut partial = two.clone();
    let mut partial_stats = ViaHopStats::default();
    let mut partial_guard = ScriptedGuard {
        refused: BTreeMap::from([(
            1,
            vec![Finding {
                pos: Some(v(0.0, 0.0)),
                nets: vec![0, 1],
            }],
        )]),
        ..ScriptedGuard::default()
    };
    // cpp:738 pads the first edit's box by 1, 2600 adds 0.5: (0,0) matches
    // only the first hop (20 mm away from the second). cpp:2607 uses nets only
    // without a location hit, so naming both nets here still reverts just one.
    // cpp:2547-2563 finds no shared trace IDs; call 2 accepts the second edit.
    assert!(
        run(&mut partial, &opt, &mut partial_stats, &mut partial_guard),
        "localized refusal: pass accepts remainder"
    );
    assert_eq!(
        partial_guard.judged_vias,
        vec![0, 2],
        "localized refusal: both hops removed before first judge, one restored before second"
    );
    assert_eq!(
        partial_guard.judged_nets,
        vec![vec![0, 1], vec![1]],
        "localized refusal: judge receives only changed copper nets"
    );
    assert_eq!(
        (
            partial_guard.calls,
            partial_stats.guard_iterations,
            partial_stats.reverts,
            partial_stats.removed_empty
        ),
        (2, 2, 1, 1),
        "localized refusal: one revert then one acceptance"
    );
    // cpp:2538 deactivates the first of two independent S_Empty edits;
    // 2637,2643-2645 counts only the remaining non-pair Empty edit. Thus
    // edits=1, removed_empty=1, and every other edit counter=0. ADAPTED
    // cpp:2520: its pre-Guard total of two overcounts the reverted edit.
    for (name, count) in edit_counts(&partial_stats) {
        let expected = if matches!(name, "edits" | "removed_empty") { 1 } else { 0 };
        assert_eq!(count, expected, "localized refusal: {name} counts only the standing edit");
    }
    assert_eq!(
        partial.traces.iter().filter(|t| t.net == 0).count(),
        3,
        "localized refusal: first hop's three traces restored"
    );
    assert_eq!(
        partial.traces.iter().filter(|t| t.net == 1).count(),
        1,
        "localized refusal: second hop stays removed"
    );
    valid_vias(&partial, "localized refusal");

    // cpp:2607-2609 falls back to net membership when no box matches. The
    // Guard adaptation also takes that branch for a Finding without a position.
    for pos in [Some(v(100.0, 100.0)), None] {
        let mut by_net = two.clone();
        let mut s = ViaHopStats::default();
        let mut g = ScriptedGuard {
            refused: BTreeMap::from([(1, vec![Finding { pos, nets: vec![1] }])]),
            ..ScriptedGuard::default()
        };
        assert!(run(&mut by_net, &opt, &mut s, &mut g), "net refusal: pass accepts remainder");
        assert_eq!(
            (s.reverts, s.removed_empty, g.calls),
            (1, 1, 2),
            "net refusal: only named net's edit reverts"
        );
        assert_eq!(
            by_net.traces.iter().filter(|t| t.net == 0).count(),
            1,
            "net refusal: other net's hop stays removed"
        );
        assert_eq!(
            by_net.traces.iter().filter(|t| t.net == 1).count(),
            3,
            "net refusal: named net's hop restored"
        );
        assert_eq!(
            g.judged_nets,
            vec![vec![0, 1], vec![0]],
            "net refusal: surviving net alone is rejudged despite via index remapping"
        );
        valid_vias(&by_net, "net refusal compacts former via indices two and three");
        // ADAPTED ViaHops.cpp:1783-1792: dead vias 0,1 are skipped; kept vias
        // 2,3 receive indices 0,1 in that order. add_hop attaches trace 3's end
        // and trace 4's start to old 2, and trace 4's end and trace 5's start
        // to old 3, so all four preserved endpoints must remain attached below.
        for (id, end, expected) in [(3, 1, 0), (4, 0, 0), (4, 1, 1), (5, 0, 1)] {
            let t = by_net.traces.iter().find(|t| t.id == id).expect("net refusal: preserved trace survives");
            let p = if end == 0 { t.points.first().unwrap() } else { t.points.last().unwrap() };
            assert_ne!(p.attached_via, -1, "net refusal: trace {id} endpoint {end} remains attached");
            assert_eq!(
                p.attached_via, expected,
                "net refusal: trace {id} endpoint {end} remaps old vias 2,3 to 0,1 exactly"
            );
        }
    }
    let mut all = two.clone();
    let mut all_stats = ViaHopStats::default();
    let mut all_guard = ScriptedGuard {
        refused: BTreeMap::from([(1, Vec::new())]),
        ..ScriptedGuard::default()
    };
    // cpp:2611-2617 targets all active edits when no finding matches. Reverse
    // dependency restore plus input order (2563-2567) restores all copper.
    // Settled guard adaptation: changed is empty, so no second call; all edits
    // reverted after a refusal sets full_revert and restores the exact input.
    assert!(
        run(&mut all, &opt, &mut all_stats, &mut all_guard),
        "empty refusal: input restoration succeeds"
    );
    assert_eq!(
        (
            all_guard.calls,
            all_stats.guard_iterations,
            all_stats.reverts,
            all_stats.removed_empty
        ),
        (1, 1, 2, 0),
        "empty refusal: all edits revert without another judge call"
    );
    assert!(all_stats.full_revert, "empty refusal: every edit reverted sets full_revert");
    // cpp:2623 restores the input and 2635-2641 zeros the strategy/pair
    // counts: no edits stand, so all thirteen edit counters must be zero.
    // ADAPTED cpp:2520: edits must also be zero, not the pre-Guard total.
    for (name, count) in edit_counts(&all_stats) {
        assert_eq!(count, 0, "empty refusal: full input restore clears {name}");
    }
    assert_eq!(
        board_bits(&all),
        board_bits(&two),
        "empty refusal: entire board is bit-identical to input"
    );

    let mut straight = shell.clone();
    for (id, x, diameter) in [(0, 0.0, 0.6), (1, 1.0, 0.8)] {
        straight.vias.push(Via {
            id,
            net: 0,
            pos: v(x, 0.0),
            diameter,
            ..Via::default()
        });
    }
    // cpp:564-577,1604-1612 samples the 1 mm chord at 0, .15, .5,
    // 1-.15, 1 with spacing=.8 and cornerGuard=.15 (ViaHops.h:165-166). Start with
    // those exact points, widths and empty export geometry (1620-1624), so
    // straightening has identical copper. Nonzero velocity would be cleared
    // at 1615-1618; the no-op must preserve those metadata bits as well.
    straight.traces.push(Trace {
        id: 7,
        net: 0,
        layer: 0,
        width: 0.25,
        is_routed: true,
        length: 1.0,
        points: [(0.0, 0), (0.15, -1), (0.5, -1), (1.0 - 0.15, -1), (1.0, 1)]
            .into_iter()
            .map(|(x, attached_via)| TracePoint {
                pos: v(x, 0.0),
                velocity: v(0.125, -0.0),
                width: 0.25,
                attached_via,
                ..TracePoint::default()
            })
            .collect(),
        ..Trace::default()
    });
    let before = board_bits(&straight);
    // cpp:292 excludes the hop sweep (one trace end per via); 1464 admits
    // exactly this one spike because 1 > .5*1+.2. The .6/.8 diameters fail
    // merging at 1498, leaving straightening. ADAPTED cpp:1628,2514,2520:
    // its success would overcount identical copper; exclude it from the
    // ledger. Hence all edit counters, judge calls (2572) and reverts (2539)
    // are zero, full_revert is false (2624), and unchanged input is accepted
    // (2707). The no-op adaptation preserves the exact input. Check
    // with and without the Guard so a restore cannot mask a no-op ledger entry.
    for drc_guard in [true, false] {
        let mut b = straight.clone();
        let mut s = ViaHopStats::default();
        let mut g = ScriptedGuard::default();
        assert!(
            run(
                &mut b,
                &ViaHopOptions {
                    spike_merge: true,
                    spike_detour: 0.5,
                    drc_guard,
                    ..opt.clone()
                },
                &mut s,
                &mut g
            ),
            "straight spike (guard={drc_guard}): unchanged input is accepted"
        );
        assert_eq!(s.spike_candidates, 1, "straight spike (guard={drc_guard}): candidate is admitted");
        for (name, count) in edit_counts(&s) {
            assert_eq!(count, 0, "straight spike (guard={drc_guard}): no copper edit counts toward {name}");
        }
        assert_eq!(
            (g.calls, s.guard_iterations, s.reverts, s.full_revert),
            (0, 0, 0, false),
            "straight spike (guard={drc_guard}): no judge or rollback is needed"
        );
        assert_eq!(board_bits(&b), before, "straight spike (guard={drc_guard}): entire board is bit-identical");
    }

    let mut on_pad = one.clone();
    on_pad.pads.push(Pad {
        id: 0,
        net: 0,
        center: v(-1.0, 0.0),
        size: v(0.5, 0.5),
        is_smd: true,
        drill_diameter: 0.0,
        layer_mask: 1,
        ..Pad::default()
    });
    let mut locked = one.clone();
    locked.vias[0].locked = true;
    let mut plane = one.clone();
    plane.layers[1].intent = LayerIntent::GroundPlane;
    plane.layers[1].plane_net = 0;
    // cpp:290,325 excludes a locked via or a via on any pad. cpp:324 excludes
    // a net with a plane; findPlaneTap:1670 also fails since each via has two
    // attached ends, so that later sweep cannot alter this skip fixture.
    for (name, mut b) in [("via on pad", on_pad), ("locked via", locked), ("plane net", plane)] {
        let before = board_bits(&b);
        let mut s = ViaHopStats::default();
        let mut g = ScriptedGuard::default();
        assert!(run(&mut b, &opt, &mut s, &mut g), "{name}: skipped pass succeeds");
        assert_eq!(
            (s.candidates, s.edits, g.calls),
            (0, 0, 0),
            "{name}: excluded candidate never reaches judge"
        );
        assert_eq!(board_bits(&b), before, "{name}: entire skipped board stays bit-identical");
    }
    let mut paired = one.clone();
    paired.nets[0].diff_pair_partner = 1;
    paired.nets[1].diff_pair_partner = 0;
    paired.nets[0].pair_gap = 0.2;
    paired.nets[1].pair_gap = 0.2;
    paired.nets[0].is_diff_pair = true;
    paired.nets[1].is_diff_pair = true;
    paired.traces.push(Trace {
        id: 3,
        net: 1,
        layer: 1,
        width: 0.25,
        is_routed: true,
        locked: true,
        points: vec![
            TracePoint {
                pos: v(-1.0, 0.45),
                ..TracePoint::default()
            },
            TracePoint {
                pos: v(1.0, 0.45),
                ..TracePoint::default()
            },
        ],
        ..Trace::default()
    });
    let before = board_bits(&paired);
    let mut ps = ViaHopStats::default();
    let mut pg = ScriptedGuard::default();
    // cpp:2197-2211 compares both members' nearby old/new copper. The parallel
    // 2 mm R/K pair at centre spacing .25+.20 couples on Y before; after R
    // moves to X it has zero coupling to K. Loss exceeds tolMm=.05 (PairGuard.h:36),
    // so cpp:2381-2393 restores it immediately and never records an accepted edit.
    assert!(
        run(&mut paired, &opt, &mut ps, &mut pg),
        "pair guard: skipped edit returns accepted input"
    );
    assert_eq!(
        (ps.pair_skipped, ps.edits, pg.calls),
        (1, 0, 0),
        "pair guard: coupling loss is refused before the judge"
    );
    assert_eq!(board_bits(&paired), before, "pair guard: restored board is bit-identical");

    let mut no_hop = shell.clone();
    no_hop.traces.push(Trace {
        id: 7,
        net: 0,
        layer: 0,
        width: 0.25,
        is_routed: true,
        points: vec![
            TracePoint {
                pos: v(-2.0, 0.0),
                ..TracePoint::default()
            },
            TracePoint {
                pos: v(2.0, 0.0),
                ..TracePoint::default()
            },
        ],
        ..Trace::default()
    });
    let before = board_bits(&no_hop);
    let mut ns = ViaHopStats::default();
    let mut ng = ScriptedGuard::default();
    // cpp:289 rejects unattached ends; no edit exists for the guard at 2572.
    assert!(run(&mut no_hop, &opt, &mut ns, &mut ng), "nothing to do: pass succeeds");
    assert_eq!(
        (ns.hops_before, ns.hops_after, ns.edits, ng.calls),
        (0, 0, 0, 0),
        "nothing to do: zero hops and no judge call"
    );
    assert_eq!(board_bits(&no_hop), before, "nothing to do: entire board stays bit-identical");

    let mut repeat = crossing.clone();
    let mut rs = ViaHopStats::default();
    let mut rg = ScriptedGuard::default();
    // cpp:2225 sorts candidate IDs; 458 pops (score,cell) lexicographically,
    // including equal-cost paths around symmetric K. Every remaining loop is
    // serial, so two identical inputs/guard scripts yield identical geometry/stats.
    assert!(run(&mut repeat, &opt, &mut rs, &mut rg), "repeatability: repeated pass accepts");
    assert_eq!(board_bits(&repeat), board_bits(&around), "repeatability: boards are bit-identical");
    assert_eq!(
        stats_bits(&rs),
        stats_bits(&around_stats),
        "repeatability: all stats are bit-identical"
    );

    // ViaHops.h:167 and cpp:2572 disable only the production judge, preserving
    // the local edits; a scripted refusal is never consulted in this mode.
    let mut unguarded = one.clone();
    let mut us = ViaHopStats::default();
    let mut ug = ScriptedGuard {
        refused: BTreeMap::from([(1, Vec::new())]),
        ..ScriptedGuard::default()
    };
    assert!(
        run(&mut unguarded, &ViaHopOptions { drc_guard: false, ..opt }, &mut us, &mut ug),
        "guard off: local edit succeeds"
    );
    assert_eq!(
        (us.removed_empty, us.guard_iterations, ug.calls),
        (1, 0, 0),
        "guard off: no judge call"
    );
}
