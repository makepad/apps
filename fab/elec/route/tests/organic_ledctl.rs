//! Guards the organic job's fixture promise: applicable partials, every net relaxed without
//! reverts, the same terminals, removals and open nets, DRC-clean copper, truthful claims, a
//! written board that reads back as routed, and determinism.

use fab_elec_board::drc::sel_of;
use fab_elec_board::zone::fill_all;
use fab_elec_board::{connect, drc, resolve_rules, Anchor, Board, Item, Origin, Via};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{ItemId, NetId, Seg, Sel, P};
use fab_elec_fixtures::ledctl;
use fab_elec_lang::{board_doc::write_unit, evaluate, std_libs, Sources};
use fab_elec_route::auto::GridRouter;
use fab_elec_route::check::Basis;
use fab_elec_route::organic::job::{relax, OUTPUT_GRID_NM, STRAIGHT_NM};
use fab_elec_route::{problem, scope_nets, solvers, Autorouter, Base, Effort, Progress, RBase, RouteScope, Style};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[test]
fn organic_relaxes_the_ledctl_fixture() {
    let design = ledctl();
    let board: Arc<Board> = design.units[0].board.clone().expect("fixture setup: ledctl unit 0 has a board");
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let base = Base::new(board, rules.clone(), conn, fills);
    let nets = scope_nets(&base.board, &base.conn, &RouteScope::All);
    let world = Arc::new(RBase::build(&base.board));
    // Effort 1: the cheapest grid route of the fixture; the relaxer settles well inside its 100-step bound.
    let p = problem(&base, world, &nets, Style::FortyFive, Effort(1));
    let grid = GridRouter::default().route(&p, &Progress::new());
    let first_step = grid.metrics.work.transactions;

    // Sample every eighth job step after the grid; the sink runs synchronously during relax.
    let partials = Arc::new(Mutex::new(Vec::new()));
    let captured = partials.clone();
    let progress = Progress::at_steps((8..=4096).step_by(8).map(|step| first_step + step).collect(), move |step, solution| {
        captured.lock().expect("partials: capture lock is not poisoned").push((step, solution));
    });
    let basis = Basis::of(&p);
    let started = Instant::now();
    let first = relax(&p, &basis, &grid, &progress, first_step);
    let elapsed = started.elapsed();
    let (last_step, sol, report) = &first;

    let partials = std::mem::take(&mut *partials.lock().expect("partials: capture lock is not poisoned"));
    assert!(!partials.is_empty(), "partials: relaxation offers at least one solution after grid step {first_step}");
    let mut previous_step = first_step;
    for (step, partial) in &partials {
        assert!(
            *step > previous_step,
            "partials: steps strictly increase after grid step {first_step}; previous={previous_step}, current={step}"
        );
        previous_step = *step;
        let mut partial_board = (*base.board).clone();
        let applied = partial_board.apply_all(&partial.ops());
        assert!(
            applied.is_ok(),
            "partials apply: step={step}, partial={partial:?}, result={applied:?}"
        );
    }
    assert!(
        *last_step >= previous_step,
        "partials: returned step {last_step} is at least the last offered step {previous_step}"
    );

    assert_eq!(report.reverted, 0, "zero reverts: every net lifts and passes verification; report={report:?}");
    assert!(report.steps > 0, "real work: at least one relax step ran; report={report:?}");
    assert!(report.nets > 0 && report.relaxed > 0, "real work: nets and tracks are relaxed; report={report:?}");
    assert!(report.idle && report.awake == 0, "settles: the job relaxes until the relaxer goes idle; report={report:?}");

    assert_eq!(sol.remove, grid.remove, "same structure: removal list is unchanged");
    let grid_open: BTreeSet<_> = grid.open.iter().map(|(net, _)| *net).collect();
    let organic_open: BTreeSet<_> = sol.open.iter().map(|(net, _)| *net).collect();
    assert_eq!(organic_open, grid_open, "same structure: open nets are unchanged as a set");
    assert_eq!(sol.repaired, grid.repaired, "same structure: repaired nets are unchanged");
    let grid_items: BTreeMap<_, _> = grid.add.iter().map(|item| (item.id(), item)).collect();
    let added_ids: BTreeSet<_> = sol.add.iter().map(Item::id).collect();
    let organic_nets: BTreeSet<NetId> = sol
        .add
        .iter()
        .filter_map(|item| match item {
            Item::Track(t) if t.origin == (Origin::Solved { by: solvers::ORGANIC }) => Some(t.net),
            _ => None,
        })
        .collect();
    assert_eq!(organic_nets.len(), report.nets as usize, "net accounting: nets with relaxed tracks match the report");
    // The pads each net's tracks end on: the relaxed form ends on the same pads as the grid form.
    let pad_ends = |items: &mut dyn Iterator<Item = &Item>| -> BTreeSet<(NetId, ItemId, u16)> {
        items
            .filter_map(|item| if let Item::Track(t) = item { Some(t) } else { None })
            .flat_map(|t| {
                t.ends.iter().filter_map(move |a| match *a {
                    Anchor::Pad { placed, pad } => Some((t.net, placed, pad.0)),
                    Anchor::Hole(h) => Some((t.net, h, u16::MAX)),
                    _ => None,
                })
            })
            .collect()
    };
    assert_eq!(pad_ends(&mut sol.add.iter()), pad_ends(&mut grid.add.iter()), "same terminals: every net's tracks end on the pads its grid tracks end on");
    let mut relaxed = 0usize;
    for item in &sol.add {
        match item {
            Item::Track(track) if organic_nets.contains(&track.net) => {
                relaxed += 1;
                assert_eq!(track.origin, Origin::Solved { by: solvers::ORGANIC }, "relaxed net: track {:?} is the organic method's", track.id);
                assert!(!track.locked && track.widths.is_none(), "relaxed net: track {:?} is unlocked with its nominal width", track.id);
                if let Some(Item::Track(g)) = grid_items.get(&track.id) {
                    assert_eq!((track.net, track.w), (g.net, g.w), "relaxed net: track {:?} keeps its grid track's net and width", track.id);
                }
                for (index, (from, segment)) in track.path.steps().enumerate() {
                    assert!(matches!(segment, Seg::Line(_)), "relaxed geometry: track {:?} segment {index} is a line; segment={segment:?}", track.id);
                    assert_ne!(from, segment.end(), "relaxed geometry: track {:?} segment {index} has nonzero length", track.id);
                }
            }
            Item::Via(via) if organic_nets.contains(&via.net) => {
                let Some(Item::Via(g)) = grid_items.get(&via.id) else {
                    panic!("relaxed vias: via {:?} is one of the grid's", via.id);
                };
                assert_eq!(Via { at: g.at, ..via.clone() }, *g, "relaxed vias: via {:?} only moves", via.id);
            }
            _ => assert_eq!(Some(&item), grid_items.get(&item.id()), "grid form: item {:?} equals its grid item", item.id()),
        }
    }
    let kept_tracks = sol.add.iter().filter(|item| matches!(item, Item::Track(t) if !organic_nets.contains(&t.net))).count();
    assert_eq!(relaxed, report.relaxed as usize, "track accounting: tracks of relaxed nets match the relaxed count");
    assert_eq!(kept_tracks, report.kept as usize, "track accounting: grid-form tracks match the kept count");

    let mut b2 = (*base.board).clone();
    let applied = b2.apply_all(&sol.ops());
    assert!(
        applied.is_ok(),
        "final applies: added items {added_ids:?}, removed items {:?}: {applied:?}",
        sol.remove
    );
    let fills2 = fill_all(&b2, &rules);
    let conn2 = connect(&b2, &fills2);
    let result = drc(&b2, &conn2, &rules, &fills2, None);

    // Round trip: the routed board written to its document and read back connects and checks as
    // it does in memory. A reader finds a junction from coordinates alone: a branch end that sat on
    // its host's segment a few nm beside the host's vertex (interior vertices rounded to the output
    // grid, the end not) read back attached to nothing, a dangling track end.
    let mut unit = design.units[0].clone();
    unit.board = Some(Arc::new(b2.clone()));
    let mut sources = Sources::new();
    for (name, text) in [("ledctl.ckt.splash", include_str!("../../fixtures/ledctl.ckt.splash").to_owned()), ("ledctl.pcb.splash", write_unit(&unit))] {
        sources.add(name, text).expect("round trip: ledctl's circuit and its written board are distinct documents");
    }
    sources.ensure_system().expect("round trip: ledctl has a system document");
    let read = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("round trip: the written ledctl board reads back: {diags:?}"));
    let b3 = read.design.units.iter().find(|u| u.name == unit.name).and_then(|u| u.board.clone())
        .expect("round trip: the read-back unit has a board");
    let rules3 = resolve_rules(&b3, None);
    let fills3 = fill_all(&b3, &rules3);
    let conn3 = connect(&b3, &fills3);
    let checks = |violations: &[Violation]| {
        let mut out: Vec<_> = violations.iter().map(|v| (v.kind.clone(), v.message.clone())).collect();
        out.sort();
        out
    };
    assert_eq!(
        (&conn3.unrouted, checks(&drc(&b3, &conn3, &rules3, &fills3, None).violations)),
        (&conn2.unrouted, checks(&result.violations)),
        "round trip: the board read back from its written document connects and checks as in memory"
    );
    let mut added_sels: BTreeSet<_> = added_ids.iter().map(|&id| Sel::Item(base.unit, id)).collect();
    // Match route_fixture: copper findings can select a feature of the owning board item.
    added_sels.extend(
        conn2
            .clusters
            .iter()
            .flat_map(|cluster| cluster.items.iter())
            .filter(|item| added_ids.contains(&item.item()))
            .map(|&item| sel_of(&b2, item)),
    );
    let offending: Vec<_> = result
        .violations
        .iter()
        .filter(|v| {
            matches!(
                v.kind,
                CheckKind::Clearance
                    | CheckKind::EdgeClearance
                    | CheckKind::HoleClearance
                    | CheckKind::HoleToHole
                    | CheckKind::Keepout
                    | CheckKind::Short
                    | CheckKind::TrackWidth
                    | CheckKind::ViaSize
                    | CheckKind::AnnularRing
                    | CheckKind::Drill
                    | CheckKind::Dangling
                    | CheckKind::DanglingVia
                    | CheckKind::ViaInPad
            ) && v.items.iter().any(|sel| added_sels.contains(sel))
        })
        .collect();
    assert!(
        offending.is_empty(),
        "DRC clean: added items {added_ids:?} have offending violations: {offending:#?}"
    );

    let problem_nets: BTreeSet<_> = p.nets.iter().map(|net| net.net).collect();
    assert_eq!(
        sol.metrics.nets_routed as usize + sol.metrics.nets_open as usize,
        p.nets.len() + sol.repaired.len(),
        "claims hold: metrics {:?} must account for problem nets {problem_nets:?}; repaired={:?}, open={:?}",
        sol.metrics,
        sol.repaired,
        sol.open
    );
    let mut open_nets = BTreeSet::new();
    for (net, reason) in &sol.open {
        assert!(
            problem_nets.contains(net),
            "claims hold: open net {net:?} ({reason:?}) is outside problem nets {problem_nets:?}"
        );
        assert!(
            open_nets.insert(*net),
            "claims hold: open net {net:?} occurs more than once: {:?}",
            sol.open
        );
    }
    assert_eq!(
        sol.metrics.nets_open as usize,
        open_nets.len(),
        "claims hold: the open count must match the listed open nets: {:?}",
        sol.open
    );
    for net in problem_nets.difference(&open_nets) {
        assert!(
            !conn2.unrouted.contains(net),
            "claims hold: a routed net is one cluster; net {net:?} remains unrouted: {:?}",
            conn2.unrouted
        );
        assert_eq!(
            conn2.clusters.iter().filter(|cluster| cluster.nets.contains(net)).count(),
            1,
            "claims hold: routed problem net {net:?} belongs to exactly one cluster"
        );
    }
    for net in &conn2.unrouted {
        assert!(
            base.conn.unrouted.contains(net) || open_nets.contains(net),
            "claims hold: no net is cut silently; previously connected net {net:?} is unrouted; open={:?}",
            sol.open
        );
    }
    for (index, net) in base.board.nets.iter().enumerate() {
        let id = NetId(index as u32);
        if base.conn.unrouted.contains(&id) || open_nets.contains(&id) {
            continue;
        }
        let unconnected: Vec<_> = result.violations.iter().filter(|violation| {
            violation.kind == CheckKind::Unconnected
                && violation.message == format!("{} not connected here", net.name)
        }).collect();
        assert!(
            unconnected.is_empty(),
            "claims hold: previously connected net {id:?} has new Unconnected findings: {unconnected:#?}"
        );
    }

    // Straight runs are written once: no organic track keeps a vertex that lies on the way between
    // its neighbours (within STRAIGHT_NM), so a doc never carries the relaxer's resampled steps;
    // only a junction, the vertex another track's end shares, stays wherever it lies.
    // Connections are exact: each end is attached and sits exactly where its anchor puts it on the
    // applied board (nothing left for the board to snap); interior vertices are on the output grid.
    let organic = Origin::Solved { by: solvers::ORGANIC };
    let junctions: BTreeSet<(ItemId, P)> = sol.add.iter()
        .filter_map(|item| match item { Item::Track(t) => Some(t), _ => None })
        .flat_map(|t| [(t.ends[0], t.path.start), (t.ends[1], t.path.end())])
        .filter_map(|(anchor, end)| match anchor { Anchor::Track { id, .. } => Some((id, end)), _ => None })
        .collect();
    let mut organic_segments = 0;
    for track in sol.add.iter().filter_map(|item| match item {
        Item::Track(t) if t.origin == organic => Some(t),
        _ => None,
    }) {
        let mut points = vec![track.path.start];
        for segment in &track.path.segs {
            assert!(matches!(segment, Seg::Line(_)), "straight runs: organic track {:?} is a polyline", track.id);
            points.push(segment.end());
        }
        organic_segments += track.path.segs.len();
        for (e, (anchor, end)) in [(track.ends[0], track.path.start), (track.ends[1], track.path.end())].into_iter().enumerate() {
            assert_ne!(anchor, Anchor::Free, "exact connections: organic track {:?} end {e} at {end:?} is attached", track.id);
            assert_eq!(b2.anchor_pos(anchor), Some(end),
                "exact connections: organic track {:?} end {e} sits exactly on its anchor {anchor:?}", track.id);
        }
        for p in &points[1..points.len() - 1] {
            assert!(p.x % OUTPUT_GRID_NM == 0 && p.y % OUTPUT_GRID_NM == 0,
                "output grid: organic track {:?} interior vertex {p:?} is on the {OUTPUT_GRID_NM} nm grid", track.id);
        }
        for w in points.windows(3).filter(|w| !junctions.contains(&(track.id, w[1]))) {
            let gap = segment_gap(w[1], w[0], w[2]);
            assert!(gap > STRAIGHT_NM as f64,
                "straight runs: organic track {:?} keeps vertex {:?} {gap:.1} nm off the way from {:?} to {:?}",
                track.id, w[1], w[0], w[2]);
        }
    }
    assert!(organic_segments > 0, "straight runs: the relaxed form has organic tracks to check");

    let repeated = relax(&p, &basis, &grid, &Progress::new(), first_step);
    assert_eq!(first, repeated, "same bytes twice: step, solution and report are independent of progress delivery");
    eprintln!(
        "organic ledctl: steps={} awake={} nets={} relaxed={} kept={} vias={}/{} ms_per_step={:.3} grid_len_mm={:.3} organic_len_mm={:.3} organic_segments={}",
        report.steps,
        report.awake,
        report.nets,
        report.relaxed,
        report.kept,
        sol.metrics.vias,
        grid.metrics.vias,
        elapsed.as_secs_f64() * 1_000.0 / f64::from(report.steps),
        grid.metrics.length as f64 / 1e6,
        sol.metrics.length as f64 / 1e6,
        organic_segments
    );
}

/// Distance in nm from `p` to the segment `a`-`b`, computed here independently of the lift.
fn segment_gap(p: P, a: P, b: P) -> f64 {
    let (ax, ay, bx, by, px, py) = (a.x as f64, a.y as f64, b.x as f64, b.y as f64, p.x as f64, p.y as f64);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) };
    (px - ax - t * dx).hypot(py - ay - t * dy)
}
