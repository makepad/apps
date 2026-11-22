use fab_elec_route::organic::{copper, geom, model, neckdown, settings};

use geom::Vec2;
use model::{ClearanceTable, DesignRules, LayerInfo, LayerIntent, NeckdownIssue, Net, OBoard, Pad, PadShape, Trace, TracePoint};
use neckdown::neckdowns_and_teardrops;
use settings::OrganicSettings;

#[test]
fn organic_neckdown_td_geometry_switch_rejection_and_determinism() {
    let v = Vec2::new;
    let points = [v(2.0, 2.0), v(3.0, 2.0), v(5.0, 2.0)];
    let pad = Pad {
        id: 0,
        net: 0,
        center: points[0],
        size: v(1.0, 1.0),
        shape: PadShape::Circle,
        layer_mask: 1,
        is_smd: true,
        drill_diameter: 0.0,
        ..Pad::default()
    };
    let input = OBoard {
        layers: vec![LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 }],
        nets: vec![Net { id: 0, ..Net::default() }, Net { id: 1, ..Net::default() }],
        outline: vec![v(0.0, 0.0), v(8.0, 0.0), v(8.0, 5.0), v(0.0, 5.0)],
        clr: ClearanceTable { rows: 1, layers: 1, mm: vec![0.2], max: 0.2 },
        rules: DesignRules { min_clearance: 0.2, board_edge_clearance: 0.3, ..DesignRules::default() },
        pads: vec![pad.clone()],
        traces: vec![Trace {
            id: 7,
            net: 0,
            layer: 0,
            width: 0.25,
            is_routed: true,
            points: points.iter().enumerate().map(|(i, &pos)| TracePoint {
                pos,
                width: 0.25,
                is_fixed: i == 0,
                terminal_pad: if i == 0 { 0 } else { -1 },
                ..TracePoint::default()
            }).collect(),
            smoothed: points.to_vec(),
            point_widths: vec![0.25; points.len()],
            length: 3.0,
            ..Trace::default()
        }],
        ..OBoard::default()
    };
    // base() disables teardrops but retains style 2. Turn on only the feature
    // being tested: style 0 is classic, 1 fillet, 2 SDF (OrganicRelaxer.h:277-280).
    let on = OrganicSettings { enable_teardrops: true, teardrop_max_width: 0.7, ..OrganicSettings::base() };
    assert_eq!(on.teardrop_style, 2, "style fixture: base selects the SDF style");
    let off = OrganicSettings { enable_teardrops: false, ..on.clone() };
    let run = |board: &OBoard, st: &OrganicSettings| {
        let mut out = board.clone();
        let mut issues = Vec::new();
        neckdowns_and_teardrops(&mut out, st, None, &mut issues, None, None, None, true);
        (out, issues)
    };
    let (a, a_issues) = run(&input, &on);
    let (b, b_issues) = run(&input, &on);
    // Start with existing polygons, so this also proves that the off switch
    // clears stale teardrops rather than merely leaving initially empty fields.
    let (disabled, disabled_issues) = run(&a, &off);
    let poly = &a.traces[0].teardrop_front;
    assert!(poly.len() >= 3, "roomy round pad: the attached trace end gets a teardrop");
    assert!(a.traces[0].teardrop_back.is_empty(), "unattached end: no teardrop without a junction");

    // NeckdownTeardrop.cpp:112-123 checks edges (i, (i+1)%n), and :2010
    // removes a repeated closing vertex in the classic builder. The contract
    // is an implicitly closed ring, not first == last. Check that closing edge
    // along with every other edge, including nonadjacent touching/overlap.
    let edges: Vec<_> = (0..poly.len()).map(|i| (poly[i], poly[(i + 1) % poly.len()])).collect();
    let mut twice_area = 0.0f64;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut at_pad = 0;
    for (i, &(p, q)) in edges.iter().enumerate() {
        assert!(p.x.is_finite() && p.y.is_finite(), "closed simple ring: vertex {i} is finite");
        assert!(p.distance_squared_to(q) > 0.0, "closed simple ring: edge {i}, including the closing edge, is nonzero");
        twice_area += p.x as f64 * q.y as f64 - p.y as f64 * q.x as f64;
        if p.distance_to(pad.center) <= 0.5 + 1e-5 {
            min_y = geom::std_min(min_y, p.y);
            max_y = geom::std_max(max_y, p.y);
            at_pad += 1;
        }
        for j in i + 2..edges.len() {
            if i == 0 && j + 1 == edges.len() { continue; }
            let (r, s) = edges[j];
            assert!(copper::seg_seg(p, q, r, s).d > 1e-8,
                "closed simple ring: nonadjacent edges {i} and {j} neither cross nor touch");
        }
    }
    assert!(twice_area.abs() > 1e-8, "closed simple ring: the last-to-first edge encloses positive area");
    assert!(at_pad >= 2, "pad flare: at least two landing vertices lie on or inside the pad (f32 tolerance)");
    assert!(max_y > points[0].y + 0.5 * input.traces[0].width,
        "pad flare: upper landing extends above its nominal track edge y + width/2");
    assert!(min_y < points[0].y - 0.5 * input.traces[0].width,
        "pad flare: lower landing extends below its nominal track edge y - width/2");
    // NeckdownTeardrop.cpp:1662 caps the flare's width at the pad outline (teardropWidthRatio *
    // cross, at most teardropMaxWidth; TeardropSdf's Input::width2). The polygon closes inside
    // the pad, where the pad's copper covers it and its sides may spread wider, and the distance
    // field meets the outline itself within its sampling (0.72 here): the cap holds for the
    // flare beyond the outline.
    let (out_count, out_half) = poly
        .iter()
        .filter(|p| p.distance_to(pad.center) > 0.5 + 0.01)
        .fold((0, 0.0f32), |(count, w), p| (count + 1, geom::std_max(w, (p.y - pad.center.y).abs())));
    assert!(out_count > 0, "width cap: fixture has vertices more than 0.01 mm outside the pad");
    assert!(2.0 * out_half <= on.teardrop_max_width + 1e-5,
        "width cap: vertices more than 0.01 mm outside the pad fit within teardrop_max_width centered on the trace (f32 tolerance): {}", 2.0 * out_half);

    // Record every Trace/TracePoint field, including lengths and velocities;
    // float equality is always to_bits, and vector lengths delimit the data.
    let trace_bits = |tr: &Trace, include_teardrops: bool| {
        let mut bits = vec![tr.id as u32, tr.net as u32, tr.layer as u32, tr.width.to_bits(), tr.length.to_bits(),
            tr.is_routed as u32, tr.locked as u32, tr.imported as u32, tr.pair_frozen as u32, tr.rule as u32,
            tr.points.len() as u32];
        for p in &tr.points {
            bits.extend([p.pos.x.to_bits(), p.pos.y.to_bits(), p.velocity.x.to_bits(), p.velocity.y.to_bits(), p.width.to_bits(),
                p.is_fixed as u32, p.terminal_pad as u32, p.attached_via as u32, p.branch_trace as u32, p.branch_point as u32,
                p.straight_before as u32]);
        }
        bits.push(tr.smoothed.len() as u32);
        for p in &tr.smoothed { bits.extend([p.x.to_bits(), p.y.to_bits()]); }
        bits.push(tr.point_widths.len() as u32);
        bits.extend(tr.point_widths.iter().map(|w| w.to_bits()));
        if include_teardrops {
            for poly in [&tr.teardrop_front, &tr.teardrop_back] {
                bits.push(poly.len() as u32);
                for p in poly { bits.extend([p.x.to_bits(), p.y.to_bits()]); }
            }
        }
        bits
    };
    let issue_bits = |issues: &[Vec<NeckdownIssue>]| {
        issues.iter().map(|list| list.iter().map(|i| (
            i.trace_id, i.net_id, i.layer, i.pos.x.to_bits(), i.pos.y.to_bits(), i.required_width.to_bits(), i.min_width.to_bits()
        )).collect::<Vec<_>>()).collect::<Vec<_>>()
    };
    assert_eq!(a.traces.len(), b.traces.len(), "determinism: trace counts match");
    assert_eq!(a.traces.len(), disabled.traces.len(), "off switch: trace count is unchanged");
    for t in 0..a.traces.len() {
        assert!(disabled.traces[t].teardrop_front.is_empty() && disabled.traces[t].teardrop_back.is_empty(),
            "off switch: both teardrops of trace {t} are empty");
        assert_eq!(trace_bits(&a.traces[t], false), trace_bits(&disabled.traces[t], false),
            "off switch: trace {t} points, widths and other non-teardrop fields are bit-identical");
        assert_eq!(trace_bits(&a.traces[t], true), trace_bits(&b.traces[t], true),
            "determinism: trace {t}, including both teardrop polygons, is bit-identical");
    }
    assert_eq!(issue_bits(&a_issues), issue_bits(&b_issues), "determinism: issues are bit-identical");
    assert_eq!(issue_bits(&a_issues), issue_bits(&disabled_issues), "off switch: issues are unchanged");

    // A foreign pad covers the attachment itself: every possible root overlaps
    // foreign copper, so shrinking cannot yield a clear polygon. This purposely
    // impossible input distinguishes rejection from a merely smaller teardrop.
    let mut blocked = input.clone();
    blocked.pads.push(Pad { id: 1, net: 1, ..pad });
    let (blocked_a, blocked_issues_a) = run(&blocked, &on);
    let (blocked_b, blocked_issues_b) = run(&blocked, &on);
    assert!(blocked_a.traces[0].teardrop_front.is_empty(),
        "foreign pad at the root: all shrinking attempts are rejected and no teardrop remains");
    // NeckdownTeardrop.cpp:1842-1884 shrinks/retries and then continues without
    // recording an issue. :2289 clears issues; only neckTrace can append them.
    // Dynamic neckdowns are off in base(), so teardrop rejection records none.
    assert_eq!(blocked_issues_a.len(), blocked_a.traces.len(), "rejection issues: one issue list exists per trace");
    assert!(blocked_issues_a.iter().all(Vec::is_empty),
        "rejection issues: the reference emits no NeckdownIssue for a rejected teardrop");
    assert_eq!(trace_bits(&blocked_a.traces[0], true), trace_bits(&blocked_b.traces[0], true),
        "determinism: blocked geometry and empty polygons are bit-identical");
    assert_eq!(issue_bits(&blocked_issues_a), issue_bits(&blocked_issues_b),
        "determinism: blocked issue lists are bit-identical");
}
