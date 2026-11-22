use fab_elec_route::organic::{
    geom::Vec2,
    model::{ClearanceTable, DesignRules, Keepout, LayerInfo, LayerIntent, Net, OBoard, Pad, Trace, TracePoint, Via, Zone},
    opt::{
        hops::count_hops,
        layer_reassign::{run, LayerReassignOptions, LayerReassignStats},
        Guard,
    },
};

#[derive(Default)]
struct ScriptedGuard {
    calls: i32,
    refuse: Vec<i32>,
    nets: Vec<Vec<i32>>,
    layers: Vec<Vec<(i32, i32)>>,
    taps: Vec<(i32, i32, Vec2, f32)>,
}

impl Guard for ScriptedGuard {
    fn judge(&mut self, b: &OBoard, nets: &[i32]) -> bool {
        self.calls += 1;
        self.nets.push(nets.to_vec());
        self.layers.push(b.traces.iter().map(|t| (t.net, t.layer)).collect());
        !self.refuse.contains(&self.calls)
    }
    fn sync(&mut self, _b: &OBoard, _nets: &[i32]) {
        panic!("guard state: LayerReassign restores only refused edits, which the guard never accepted");
    }
    fn tap_connected(&self, net: i32, layer: i32, pos: Vec2, radius: f32) -> bool {
        self.taps.iter().any(|&(n, l, p, r)| {
            n == net && l == layer && bits(p) == bits(pos) && r.to_bits() == radius.to_bits()
        })
    }
}

fn bits(p: Vec2) -> (u32, u32) { (p.x.to_bits(), p.y.to_bits()) }

// Exhaustive destructuring checks every OBoard field, including unchanged data.
// Floating values compare by bits, including export caches, velocities and widths.
fn board_bits(b: &OBoard) -> Vec<u64> {
    fn xy(out: &mut Vec<u64>, p: Vec2) { out.extend([p.x.to_bits() as u64, p.y.to_bits() as u64]); }
    fn poly(out: &mut Vec<u64>, p: &[Vec2]) {
        out.push(p.len() as u64);
        for &v in p { xy(out, v); }
    }
    let OBoard { layers, outline, cutouts, pads, nets, vias, traces, keepouts, zones, rules, clr,
        pour_rules, min_track_width_for, track_width_fixed } = b;
    let mut out = vec![layers.len() as u64];
    for l in layers {
        let LayerInfo { index, intent, plane_net } = l;
        out.extend([*index as u64, *intent as u64, *plane_net as u64]);
    }
    poly(&mut out, outline);
    out.push(cutouts.len() as u64);
    for p in cutouts { poly(&mut out, p); }
    out.push(pads.len() as u64);
    for p in pads {
        let Pad { id, center, size, drill_diameter, shape, layer_mask, net, is_smd, rotation, local_size, drill_size, round_rect_ratio, polygon, component, piece_of, rule, number, fanout, via_drill_gap } = p;
        out.push(*id as u64);
        xy(&mut out, *center);
        xy(&mut out, *size);
        out.extend([drill_diameter.to_bits() as u64, *shape as u64, *layer_mask, *net as u64, *is_smd as u64, rotation.to_bits() as u64]);
        xy(&mut out, *local_size);
        xy(&mut out, *drill_size);
        out.push(round_rect_ratio.to_bits() as u64);
        poly(&mut out, polygon);
        out.extend([*component as u64, *piece_of as u64, *rule as u64, *number as u64, *fanout as u64, via_drill_gap.to_bits() as u64]);
    }
    out.push(nets.len() as u64);
    for n in nets {
        let Net { id, diff_pair_partner, pair_gap, via_diameter, via_drill, via_rule, via_in_pad, target_length, is_diff_pair, current } = n;
        out.extend([*id as u64, *diff_pair_partner as u64, pair_gap.to_bits() as u64, via_diameter.to_bits() as u64, via_drill.to_bits() as u64,
            *via_rule as u64, *via_in_pad as u64, target_length.to_bits() as u64, *is_diff_pair as u64, *current as u64]);
    }
    out.push(vias.len() as u64);
    for v in vias {
        let Via { id, net, pos, velocity, diameter, drill, layer_start, layer_end, connected_pad, is_fixed, locked, imported, pair_frozen, kind, rule, src } = v;
        out.extend([*id as u64, *net as u64]);
        xy(&mut out, *pos);
        xy(&mut out, *velocity);
        out.extend([diameter.to_bits() as u64, drill.to_bits() as u64, *layer_start as u64, *layer_end as u64, *connected_pad as u64, *is_fixed as u64,
            *locked as u64, *imported as u64, *pair_frozen as u64, *kind as u64, *rule as u64, *src as u64]);
    }
    out.push(traces.len() as u64);
    for t in traces {
        let Trace { id, net, layer, width, points, smoothed, point_widths, teardrop_front, teardrop_back, length, is_routed, locked, imported, pair_frozen, rule } = t;
        out.extend([*id as u64, *net as u64, *layer as u64, width.to_bits() as u64, points.len() as u64]);
        for p in points {
            let TracePoint { pos, velocity, width, is_fixed, terminal_pad, attached_via, branch_trace, branch_point, straight_before } = p;
            xy(&mut out, *pos);
            xy(&mut out, *velocity);
            out.extend([width.to_bits() as u64, *is_fixed as u64, *terminal_pad as u64, *attached_via as u64, *branch_trace as u64, *branch_point as u64, *straight_before as u64]);
        }
        poly(&mut out, smoothed);
        out.push(point_widths.len() as u64);
        out.extend(point_widths.iter().map(|w| w.to_bits() as u64));
        poly(&mut out, teardrop_front);
        poly(&mut out, teardrop_back);
        out.extend([length.to_bits() as u64, *is_routed as u64, *locked as u64, *imported as u64, *pair_frozen as u64, *rule as u64]);
    }
    out.push(keepouts.len() as u64);
    for k in keepouts {
        let Keepout { id, poly: p, layer_mask, clearance, no_tracks, no_vias, only_net } = k;
        out.push(*id as u64);
        poly(&mut out, p);
        out.extend([*layer_mask, clearance.to_bits() as u64, *no_tracks as u64, *no_vias as u64, *only_net as u64]);
    }
    out.push(zones.len() as u64);
    for z in zones {
        let Zone { net, layer_mask, outline, is_rule_area } = z;
        out.extend([*net as u64, *layer_mask]);
        poly(&mut out, outline);
        out.push(*is_rule_area as u64);
    }
    let DesignRules { default_trace_width, min_trace_width, min_clearance, via_diameter, via_drill, diff_pair_spacing, board_edge_clearance, hole_clearance, hole_to_hole } = rules;
    out.extend([default_trace_width, min_trace_width, min_clearance, via_diameter, via_drill, diff_pair_spacing, board_edge_clearance, hole_clearance, hole_to_hole].map(|x| x.to_bits() as u64));
    let ClearanceTable { rows, layers, mm, max } = clr;
    out.extend([*rows as u64, *layers as u64, mm.len() as u64]);
    out.extend(mm.iter().map(|x| x.to_bits() as u64));
    out.push(max.to_bits() as u64);
    out.push(pour_rules.len() as u64);
    out.extend(pour_rules.iter().map(|&row| row as u64));
    out.push(min_track_width_for.len() as u64);
    for (&(net, layer), width) in min_track_width_for { out.extend([net as u64, layer as u64, width.to_bits() as u64]); }
    out.push(track_width_fixed.len() as u64);
    out.extend(track_width_fixed.iter().map(|&x| x as u64));
    out
}

fn pad(id: i32, net: i32, pos: Vec2, layer: i32) -> Pad {
    Pad { id, net, center: pos, size: Vec2::new(1.0, 1.0), drill_diameter: 0.0, is_smd: true, layer_mask: 1u64 << layer, ..Pad::default() }
}
fn via(id: i32, net: i32, pos: Vec2) -> Via {
    Via { id, net, pos, diameter: 0.6, drill: 0.3, layer_start: 0, layer_end: 1, ..Via::default() }
}
fn end(pos: Vec2, pad: i32, via: i32) -> TracePoint {
    TracePoint { pos, terminal_pad: pad, attached_via: via, is_fixed: pad >= 0, width: 0.25, ..TracePoint::default() }
}
fn trace(id: i32, net: i32, layer: i32, a: TracePoint, b: TracePoint) -> Trace {
    Trace {
        id, net, layer, width: 0.25, points: vec![a.clone(), b.clone()], is_routed: true,
        // Nonempty caches make the exact rollback/off comparisons cover invalidation too.
        smoothed: vec![a.pos, b.pos], point_widths: vec![0.25, 0.25],
        teardrop_front: vec![a.pos], teardrop_back: vec![b.pos], length: a.pos.distance_to(b.pos),
        ..Trace::default()
    }
}

fn case_a() -> OBoard {
    let v = Vec2::new;
    OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        clr: ClearanceTable { rows: 1, layers: 2, mm: vec![0.2; 2], max: 0.2 },
        outline: vec![v(0.0, 0.0), v(50.0, 0.0), v(50.0, 30.0), v(0.0, 30.0)],
        nets: vec![Net { id: 0, ..Net::default() }],
        pads: vec![pad(0, 0, v(5.0, 10.0), 0), pad(1, 0, v(25.0, 10.0), 0)],
        vias: vec![via(0, 0, v(10.0, 10.0)), via(1, 0, v(20.0, 10.0))],
        traces: vec![
            trace(0, 0, 0, end(v(5.0, 10.0), 0, -1), end(v(10.0, 10.0), -1, 0)),
            trace(1, 0, 1, end(v(10.0, 10.0), -1, 0), end(v(20.0, 10.0), -1, 1)),
            trace(2, 0, 0, end(v(20.0, 10.0), -1, 1), end(v(25.0, 10.0), 1, -1)),
        ],
        ..OBoard::default()
    }
}

fn case_b() -> OBoard {
    let v = Vec2::new;
    let mut b = case_a();
    b.nets.push(Net { id: 1, ..Net::default() });
    b.pads.extend([pad(2, 1, v(15.0, 1.0), 1), pad(3, 1, v(15.0, 20.0), 1)]);
    b.vias.extend([via(2, 1, v(15.0, 6.0)), via(3, 1, v(15.0, 14.0)), via(4, 1, v(15.0, 3.0)), via(5, 1, v(15.0, 17.0))]);
    b.traces.extend([
        trace(3, 1, 0, end(v(15.0, 6.0), -1, 2), end(v(15.0, 14.0), -1, 3)),
        trace(4, 1, 0, end(v(15.0, 3.0), -1, 4), end(v(15.0, 6.0), -1, 2)),
        trace(5, 1, 0, end(v(15.0, 14.0), -1, 3), end(v(15.0, 17.0), -1, 5)),
        trace(6, 1, 1, end(v(15.0, 1.0), 2, -1), end(v(15.0, 3.0), -1, 4)),
        trace(7, 1, 1, end(v(15.0, 17.0), -1, 5), end(v(15.0, 20.0), 3, -1)),
    ]);
    // cpp:272-291 joins unexplained ends lying on another same-net/layer trace.
    // This detour joins the two outer top legs into one unit, away from net 0.
    let detour = [v(15.0, 4.0), v(35.0, 4.0), v(35.0, 16.0), v(15.0, 16.0)];
    b.traces.push(Trace {
        id: 8, net: 1, layer: 0, width: 0.25, is_routed: true,
        points: detour.iter().map(|&p| end(p, -1, -1)).collect(),
        smoothed: detour.to_vec(), point_widths: vec![0.25; 4],
        length: 20.0 + 12.0 + 20.0, ..Trace::default()
    });
    b
}

#[test]
fn layer_reassign_guards_closures_rolls_back_and_is_bit_exact() {
    let v = Vec2::new;
    let opt = LayerReassignOptions::default();
    let a0 = case_a();
    let mut a = a0.clone();
    let mut stats = LayerReassignStats::default();
    let mut guard = ScriptedGuard::default();
    // cpp:470-488 pins the two top SMD tails. cpp:695-707 tries top first,
    // moves only the bottom run, and reduces needed from two to zero. cpp:1056-1058
    // removes both vias. cpp:803-823 joins [pad0,v0,v1,pad1] without reversing,
    // preserving all coordinates and both terminal attachments. Thus one change/judge.
    assert!(run(&mut a, &opt, &mut stats, &mut guard), "case A: every kept change is not worse");
    assert!(a.vias.is_empty(), "case A: both unnecessary vias are removed");
    assert_eq!(a.traces.len(), 1, "case A: both via joins merge the three traces");
    assert_eq!(a.traces[0].layer, 0, "case A: the run moves to the top");
    assert_eq!(a.traces[0].points.iter().map(|p| bits(p.pos)).collect::<Vec<_>>(),
        [v(5.0, 10.0), v(10.0, 10.0), v(20.0, 10.0), v(25.0, 10.0)].map(bits), "case A: every original endpoint position is bit-exact");
    assert_eq!((a.traces[0].points[0].terminal_pad, a.traces[0].points[3].terminal_pad), (0, 1), "case A: both ends remain attached to their original pads");
    assert!(a.traces[0].points.iter().all(|p| p.attached_via == -1), "case A: no stale via attachment remains");
    assert_eq!(guard.nets, vec![vec![0]], "case A: one judge call contains exactly the changed net");
    assert_eq!((stats.removed, stats.kept, stats.units_moved, stats.judge_calls), (2, 1, 1, 1), "case A: counters describe one move and two removals");

    let mut split0 = case_a();
    split0.layers.push(LayerInfo { index: 2, intent: LayerIntent::Signal, plane_net: -1 });
    // Rows 0 (tracks) and 1 (net 0's vias): every pair/layer has 0.2 mm clearance.
    split0.clr = ClearanceTable { rows: 2, layers: 3, mm: vec![0.2; 12], max: 0.2 };
    split0.nets[0].via_diameter = 1.6;
    split0.nets[0].via_drill = 0.8;
    split0.nets[0].via_rule = 1;
    split0.nets.extend([Net { id: 1, ..Net::default() }, Net { id: 2, ..Net::default() }]);
    split0.pads[1] = pad(1, 0, v(14.0, 5.0), 0);
    split0.vias = vec![via(0, 0, v(10.0, 10.0)), via(1, 0, v(14.0, 10.0))];
    for via in &mut split0.vias { via.layer_end = 2; }
    split0.traces[1].points = vec![end(v(10.0, 10.0), -1, 0), end(v(14.0, 10.0), -1, 1), end(v(20.0, 10.0), -1, -1), end(v(26.0, 10.0), -1, -1)];
    split0.traces[2] = trace(2, 0, 0, end(v(14.0, 10.0), -1, 1), end(v(14.0, 5.0), 1, -1));
    split0.traces.extend([
        Trace { locked: true, ..trace(3, 1, 2, end(v(12.0, 8.0), -1, -1), end(v(12.0, 12.0), -1, -1)) },
        Trace { locked: true, ..trace(4, 2, 0, end(v(23.0, 8.0), -1, -1), end(v(23.0, 12.0), -1, -1)) },
    ]);
    // cpp:193-217,228-234: the only free interior node is (20,10), between
    // conflicts with traces 3 and 4. Its nearest foreign copper is 3 mm away,
    // above the class-via requirement 1.6/2 + .003 + .2 + .25/2 = 1.128 (cpp:159).
    // cpp:399-439 attaches both real vias to the left piece; moving it top removes
    // both, but the locked top crossing keeps the right piece bottom (cpp:610-630).
    // cpp:635-663 leaves that piece behind; cpp:902-910 creates one via at the site,
    // using Board.cpp:314-370's net-class sizes. Thus two real vias become one site
    // via, with its own row 1, not the trace's row 0. The old vias are 4 mm apart,
    // so neither the input nor the one-via result has a <3 mm hop (cpp:1052,1065).
    let mut split = split0.clone();
    let mut sp = LayerReassignStats::default();
    let mut gp = ScriptedGuard::default();
    assert!(run(&mut split, &opt, &mut sp, &mut gp), "class via split: the change remains guarded");
    assert_eq!(sp.sites, 1, "class via split: the free node between crossing signatures is one site");
    assert_eq!(split.vias.len(), 1, "class via split: removing two real vias leaves one new site via");
    assert_eq!(bits(split.vias[0].pos), bits(v(20.0, 10.0)), "class via split: the new via occupies the selected node");
    assert_eq!(split.vias[0].diameter.to_bits(), 1.6f32.to_bits(), "class via split: the new via uses the class diameter");
    assert_eq!(split.vias[0].drill.to_bits(), 0.8f32.to_bits(), "class via split: the new via uses the class drill");
    assert_eq!(split.vias[0].rule, 1, "class via split: the new via uses the net's Via row");

    // cpp:159-160: this foreign segment is 1 mm from the site. The base via
    // needs max(.8/2+.003+.2, .4/2+.25+.003)+.25/2 = .728 mm; the class via
    // needs max(1.6/2+.003+.2, .8/2+.25+.003)+.25/2 = 1.128 mm. It therefore
    // fits only the base via. The segment gap exceeds the trace conflict threshold
    // .2+.25/2+.25/2+.003 = .453 (cpp:332), so the crossing signatures stay as above.
    let mut near = split0.clone();
    near.traces.push(Trace { locked: true, ..trace(5, 1, 2, end(v(19.75, 11.0), -1, -1), end(v(20.25, 11.0), -1, -1)) });
    let mut base = near.clone();
    base.nets[0].via_diameter = 0.0;
    base.nets[0].via_drill = 0.0;
    let mut sbv = LayerReassignStats::default();
    let mut gbv = ScriptedGuard::default();
    assert!(run(&mut base, &opt, &mut sbv, &mut gbv), "base via site: unresolved sizes fall back to design rules");
    assert_eq!(sbv.sites, 1, "base via site: the 0.8 mm base via fits beside the foreign segment");
    let mut scv = LayerReassignStats::default();
    let mut gcv = ScriptedGuard::default();
    assert!(run(&mut near, &opt, &mut scv, &mut gcv), "class via site: rejecting the oversized site keeps the pass guarded");
    assert_eq!(scv.sites, 0, "class via site: the 1.6 mm class via cannot use a site that fits only the base via");

    // The same base-sized via with Via/Track clearance .6 needs
    // .8/2+.003+.6+.25/2 = 1.128 mm (cpp:159); the Track/Track row stays .2,
    // which would wrongly admit it at .728 mm. Set both symmetric row pairs.
    let mut row = split0.clone();
    row.nets[0].via_diameter = 0.8;
    row.nets[0].via_drill = 0.4;
    row.traces.push(Trace { locked: true, ..trace(5, 1, 2, end(v(19.75, 11.0), -1, -1), end(v(20.25, 11.0), -1, -1)) });
    for l in 0..3 { row.clr.mm[3 + l] = 0.6; row.clr.mm[6 + l] = 0.6; }
    row.clr.max = 0.6;
    let mut svr = LayerReassignStats::default();
    let mut gvr = ScriptedGuard::default();
    assert!(run(&mut row, &opt, &mut svr, &mut gvr), "via row site: the pass remains guarded with distinct item clearances");
    assert_eq!(svr.sites, 0, "via row site: legality uses the net's Via row instead of the trace's Track row");

    let b0 = case_b();
    let mut b = b0.clone();
    let mut sb = LayerReassignStats::default();
    let mut gb = ScriptedGuard::default();
    // cpp:568-574 initially counts four needed vias: 0,1,4,5 (2,3 are neutral).
    // Moving unit 1 top displaces crossing unit 3 bottom (cpp:616-629): 0,1
    // disappear but 2,3 become needed, so the cost is still four. cpp:709-720
    // at depth two reaches via 2; putting its other unit (traces 4,5,8, joined
    // by cpp:272-291) bottom removes costs 2,3,4,5. The apply removes all six
    // real vias, moves three units, and judges both nets together, once.
    // All via-to-via legs are >=3 mm, so the
    // strict <3 hop test (cpp:1052,1065) permits both changes.
    assert!(run(&mut b, &opt, &mut sb, &mut gb), "case B: the depth-two closure is accepted");
    assert_eq!(gb.nets, vec![vec![0, 1]], "case B: the closure judges both nets sorted in one call");
    // cpp:803-823 erases merged-away traces 1,2,4,5,6,7, leaving trace ids 0,3,8:
    // moved net-0 run is now in trace 0 top; displaced net-1 crossing is in trace 3 bottom.
    assert_eq!(gb.layers[0], vec![(0, 0), (1, 1), (1, 1)], "case B: the judged change moves copper of both nets");
    assert!(b.vias.is_empty(), "case B: the accepted change removes all six vias");
    assert!(b.traces.iter().all(|t| t.layer == t.net), "case B: net zero finishes top and net one bottom");
    assert_eq!((sb.kept, sb.units_moved, sb.removed, sb.judge_calls), (1, 3, 6, 1), "case B: depth-two closure counters match the reference search");
    // cpp:704,709: depth one cannot take the neutral first seed. The sweep instead
    // reaches already-unneeded via 2 (cpp:1022-1025), removing 2,3 with only net 1.
    let mut shallow = b0.clone();
    let mut gs = ScriptedGuard::default();
    let mut ss = LayerReassignStats::default();
    assert!(run(&mut shallow, &LayerReassignOptions { depth: 1, ..opt.clone() }, &mut ss, &mut gs), "case B depth one: all accepted changes remain guarded");
    assert_eq!(gs.nets.first(), Some(&vec![1]), "case B depth: the first seed's joint gain requires the second move");

    // cpp:1077-1081 restores traces0/vias0 and rejects seed 0. On rebuilding,
    // cpp:438 fixes that seed, so seed 1 alone can remove its own via; refusing
    // call 2 fixes it too. No third judge is possible; both snapshots are restored.
    let mut refused = a0.clone();
    let mut sr = LayerReassignStats::default();
    let mut gr = ScriptedGuard { refuse: vec![1, 2], ..ScriptedGuard::default() };
    assert!(run(&mut refused, &opt, &mut sr, &mut gr), "refusal: restored input still returns true");
    assert_eq!(board_bits(&refused), board_bits(&a0), "refusal: the complete board is restored bit for bit");
    assert_eq!((gr.calls, sr.found, sr.rolled_back, sr.kept), (2, 2, 2, 0), "refusal: each seed is judged once and never retried");
    assert_eq!(gr.nets, vec![vec![0], vec![0]], "refusal: each restored candidate names only its touched net");

    // cpp:1028-1034 ADAPTED: zero seed allowance fails the first place call
    // (cpp:600) for each of the two seeds, counts two limits, and ends the sweep.
    let mut limited = a0.clone();
    let mut sl = LayerReassignStats::default();
    let mut gl = ScriptedGuard::default();
    assert!(run(&mut limited, &LayerReassignOptions { max_seed_work: 0, ..opt.clone() }, &mut sl, &mut gl), "seed limit: a completed sweep returns true");
    assert_eq!((sl.seed_limit, sl.work, gl.calls), (2, 2, 0), "seed limit: exhaustion skips each seed without judging");
    assert_eq!(board_bits(&limited), board_bits(&a0), "seed limit: failed searches do not change board bits");

    // cpp:1022-1025: a same-layer seed needs no search. Give via 0 a wide disc
    // masking both endpoints of the separate 2 mm witness. countHops attaches
    // geometrically to the first disc (ViaHops.cpp:1878-2076), so its endpoints
    // initially name the SAME via (no hop). Removing 0 merges its two vertical
    // stubs at its centre (cpp:803-823) and exposes fixed vias 1,2 at the witness
    // endpoints: one real hop, since the endpoints are their centres and 2<3
    // (ViaHops.cpp:1878-2076). The hop-first guard (cpp:1061-1066 ADAPTED) must
    // restore the board without calling judge. Fixed vias 1,2 cannot seed another edit.
    let mut hop = case_a();
    hop.pads.clear();
    hop.vias = vec![Via { diameter: 6.0, ..via(0, 0, v(31.0, 20.0)) },
        Via { is_fixed: true, ..via(1, 0, v(30.0, 20.0)) }, Via { is_fixed: true, ..via(2, 0, v(32.0, 20.0)) }];
    hop.traces = vec![
        trace(0, 0, 0, end(v(31.0, 15.0), -1, -1), end(v(31.0, 20.0), -1, 0)),
        trace(1, 0, 0, end(v(31.0, 20.0), -1, 0), end(v(31.0, 25.0), -1, -1)),
        trace(2, 0, 0, end(v(30.0, 20.0), -1, 1), end(v(32.0, 20.0), -1, 2)),
    ];
    let hop0 = hop.clone();
    assert_eq!(count_hops(&hop, 3.0, false, None, None, false).real, 0, "hop fixture: the wide first disc masks the two witness attachments");
    let mut exposed = hop.clone();
    exposed.vias.remove(0);
    // countHops uses geometry, not TracePoint::attached_via or Via::id.
    assert_eq!(count_hops(&exposed, 3.0, false, None, None, false).real, 1, "hop fixture: removing the mask exposes a real 2 mm hop");
    let mut sh = LayerReassignStats::default();
    let mut gh = ScriptedGuard::default();
    assert!(run(&mut hop, &opt, &mut sh, &mut gh), "hop refusal: restoring the input returns true");
    assert_eq!((sh.found, sh.rolled_back, sh.judge_calls, gh.calls), (1, 1, 0, 0), "hop refusal: the hop test refuses before the judge");
    assert_eq!(board_bits(&hop), board_bits(&hop0), "hop refusal: the whole board is restored bit for bit");

    // cpp:986-988 initializes only statistics and returns true before any model,
    // mutation or guard call when depth is zero.
    let mut off = b0.clone();
    let mut so = LayerReassignStats::default();
    let mut go = ScriptedGuard::default();
    assert!(run(&mut off, &LayerReassignOptions { depth: 0, ..opt.clone() }, &mut so, &mut go), "depth zero: off returns true");
    assert_eq!((go.calls, so.judge_calls, so.work), (0, 0, 0), "depth zero: no judge or search runs");
    assert_eq!(board_bits(&off), board_bits(&b0), "depth zero: the complete board is bit-identical");

    // cpp:278,495,697,874 and 1017-1029 impose deterministic group/edge/layer/
    // trace/seed order; the port's BTree containers keep it. No clock is consulted.
    let mut repeat = b0.clone();
    let mut sd = LayerReassignStats::default();
    let mut gd = ScriptedGuard::default();
    assert!(run(&mut repeat, &opt, &mut sd, &mut gd), "determinism: repeating the accepted run returns true");
    assert_eq!(board_bits(&repeat), board_bits(&b), "determinism: identical inputs produce bit-identical boards");
    assert_eq!(sd, sb, "determinism: every statistic is identical");
    assert_eq!(gd.nets, gb.nets, "determinism: guard calls keep the same sorted scopes and order");
}
