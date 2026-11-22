//! L2 sample-board contract: the reference's generic and applicable case checks,
//! evaluated on Fab's grid route followed by the organic job's exported geometry.

mod samples;

use fab_elec_board::drc::sel_of;
use fab_elec_board::zone::fill_all;
use fab_elec_board::{
    connect, drc, resolve_rules, Anchor, Board, BoardNet, Flash, Item, Origin, Tent, Track, Via,
    ViaKind,
};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{mm, ItemId, LayerId, NetId, PadIx, Path, Seg, Sel, P};
use fab_elec_parts::Forbid;
use fab_elec_route::auto::GridRouter;
use fab_elec_route::check::Basis;
use fab_elec_route::organic::job::{relax_with, Finish, OrganicReport};
use fab_elec_route::progress::JobPhase;
use fab_elec_route::{
    problem, scope_nets, Autorouter, Base, Effort, Progress, RBase, RouteProblem,
    RouteScope, RouteSolution, Style,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

struct Run {
    problem: RouteProblem,
    basis: Basis,
    grid: RouteSolution,
    /// The pre stage's accepted solution (its last offer), when it accepted a change.
    pre: Option<RouteSolution>,
    solution: RouteSolution,
    report: OrganicReport,
    step: u32,
    grid_ms: f64,
    relax_ms: f64,
}

fn setup(board: Board) -> RouteProblem {
    let board = Arc::new(board);
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let base = Base::new(board, rules, conn, fills);
    let nets = scope_nets(&base.board, &base.conn, &RouteScope::All);
    let world = Arc::new(RBase::build(&base.board));
    problem(&base, world, &nets, Style::FortyFive, Effort(1))
}

fn run(board: Board, finish: &Finish) -> Run {
    let p = setup(board);
    let routed = Instant::now();
    let grid = GridRouter::default().route(&p, &Progress::new());
    let grid_ms = routed.elapsed().as_secs_f64() * 1_000.0;
    let first_step = grid.metrics.work.transactions;
    let started = Instant::now();
    let basis = Basis::of(&p);
    let offers: Arc<Mutex<Vec<(u32, Arc<RouteSolution>)>>> = Arc::default();
    let sink = offers.clone();
    // Keep every pre-stage change so its final step is present, then stop requesting after
    // the first relax partial: this test only checks the pre-stage and final solutions.
    let progress = Arc::new_cyclic(|progress: &Weak<Progress>| {
        let progress = progress.clone();
        Progress::with_sink(Duration::MAX, move |step, sol| {
            sink.lock().unwrap().push((step, sol));
            let progress = progress.upgrade().expect("the job owns its progress during delivery");
            if progress.status().phase == JobPhase::Cleanup {
                progress.want_offer();
            }
        })
    });
    let (step, solution, report) = relax_with(&p, &basis, &grid, &progress, first_step, finish);
    let relax_ms = started.elapsed().as_secs_f64() * 1_000.0;
    // each accepted change (and sync) is one step, and the pre stage offers its state at its end
    let units = report.judge.accepted + report.judge.synced;
    let pre = (units > 0).then(|| {
        let offers = offers.lock().unwrap();
        let at = offers.iter().find(|(s, _)| *s == first_step + units);
        (*at.expect("the pre stage offers its accepted state at its end").1).clone()
    });
    Run { problem: p, basis, grid, pre, solution, report, step, grid_ms, relax_ms }
}

#[test]
fn organic_routes_the_reference_samples() {
    // The pair board runs twice: the job's default, and with the relax's pair term (`Finish::pairs`).
    let pairs = Finish { pairs: true, ..Finish::default() };
    let builders: [(fn() -> Board, &Finish); 7] = [
        (samples::eurorack_module, &Finish::default()),
        (samples::mcu_dev, &Finish::default()),
        (samples::bga_escape, &Finish::default()),
        (samples::diff_pair_simple, &Finish::default()),
        (samples::diff_pair_simple, &pairs),
        (samples::keepout, &Finish::default()),
        (samples::locked_via, &Finish::default()),
    ];
    let mut pre_checked = 0;
    let mut determinism_checked = 0;
    for (build, finish) in builders {
        let board_started = Instant::now();
        let first = run(build(), finish);
        let p = &first.problem;
        let name = &if finish.pairs { format!("{} (pairs)", p.board.name) } else { p.board.name.clone() };
        let sol = &first.solution;
        let report = &first.report;
        eprintln!(
            "organic {name}: routed={} open={} steps={} idle={} nets={} relaxed={} kept={} reverted={} awake={} vias={}/{} grid_ms={:.3} relax_ms={:.3}",
            sol.metrics.nets_routed, sol.metrics.nets_open, report.steps, report.idle, report.nets,
            report.relaxed, report.kept, report.reverted, report.awake, sol.metrics.vias, first.grid.metrics.vias, first.grid_ms, first.relax_ms,
        );

        let added_ids: BTreeSet<_> = sol.add.iter().map(Item::id).collect();
        let mut after = (*p.board).clone();
        let applied = after.apply_all(&sol.ops());
        assert!(applied.is_ok(), "{name}: applies: relaxed solution ops: {applied:?}");
        let fills = fill_all(&after, &p.rules);
        let conn = connect(&after, &fills);
        let result = drc(&after, &conn, &p.rules, &fills, None);
        let mut added_sels: BTreeSet<_> = added_ids.iter().map(|&id| Sel::Item(after.unit, id)).collect();
        // Exactly the organic_ledctl selection extension: a copper finding may name a
        // feature of the added item instead of selecting the owning item directly.
        added_sels.extend(
            conn.clusters.iter().flat_map(|cluster| cluster.items.iter())
                .filter(|item| added_ids.contains(&item.item()))
                .map(|&item| sel_of(&after, item)),
        );
        let offending: Vec<_> = result.violations.iter().filter(|v| {
            matches!(v.kind,
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
        }).collect();
        assert!(offending.is_empty(), "{name}: DRC clean: added items {added_ids:?}: {offending:#?}");
        let mut grid_board = (*p.board).clone();
        assert!(grid_board.apply_all(&first.grid.ops()).is_ok(), "{name}: applies: grid form ops");
        let grid_fills = fill_all(&grid_board, &p.rules);
        let grid_conn = connect(&grid_board, &grid_fills);
        let grid_result = drc(&grid_board, &grid_conn, &p.rules, &grid_fills, None);
        // A pair's halves (Fab's pair gap, `Board.pairs`): the relax and the finish passes bring
        // them no closer than the grid form did.
        let pair_gaps = |v: &[Violation]| v.iter().filter(|v| v.kind == CheckKind::PairGap).count();
        assert!(pair_gaps(&result.violations) <= pair_gaps(&grid_result.violations),
            "{name}: no new pair gap finding: {} after the relax, {} on the grid form", pair_gaps(&result.violations), pair_gaps(&grid_result.violations));
        // The pre stage moves no via (Cleanup and LayerReassign remove vias and add new ones
        // with fresh ids): each grid via of its accepted solution sits where the grid put it, to
        // the nm. Lifted through the port's f32 millimetres it would be a nm or two off, and a
        // via in a pad would then read to the judge as a new finding.
        if let Some(pre) = &first.pre {
            pre_checked += 1;
            let grid_at: BTreeMap<ItemId, P> =
                first.grid.add.iter().filter_map(|it| match it { Item::Via(v) => Some((v.id, v.at)), _ => None }).collect();
            for v in pre.add.iter().filter_map(|it| match it { Item::Via(v) => Some(v), _ => None }) {
                if let Some(at) = grid_at.get(&v.id) {
                    assert_eq!(*at, v.at, "{name}: the pre stage moves no via: {:?}", v.id);
                }
            }
        }
        assert_eq!(report.reverted, 0, "{name}: zero reverts: {report:?}");
        assert!(report.steps > 0, "{name}: real relaxation steps: {report:?}");
        assert!(report.idle && report.awake == 0, "{name}: settles: the job relaxes until the relaxer goes idle: {report:?}");
        assert!(report.idle && report.awake == 0, "{name}: settles: the job relaxes until the relaxer goes idle: {report:?}");
        let organic = Origin::Solved { by: fab_elec_route::solvers::ORGANIC };
        assert_eq!(report.relaxed as usize + report.kept as usize,
            sol.add.iter().filter(|item| matches!(item, Item::Track(_))).count(),
            "{name}: track accounting: relaxed + kept accounts for the solution's tracks");
        assert_eq!(report.relaxed as usize,
            sol.add.iter().filter(|item| matches!(item, Item::Track(t) if t.origin == organic)).count(),
            "{name}: track accounting: the relaxed tracks are the organic method's");
        let grid_open: BTreeSet<_> = first.grid.open.iter().map(|(net, _)| *net).collect();
        let organic_open: BTreeSet<_> = sol.open.iter().map(|(net, _)| *net).collect();
        assert_eq!(organic_open, grid_open, "{name}: connected as routed: open-net sets agree");

        // The complete organic_ledctl metrics/open-list/actual-connectivity checks.
        let problem_nets: BTreeSet<_> = p.nets.iter().map(|net| net.net).collect();
        assert_eq!(sol.metrics.nets_routed as usize + sol.metrics.nets_open as usize,
            p.nets.len() + sol.repaired.len(),
            "{name}: claims hold: metrics={:?}, problem={problem_nets:?}, repaired={:?}, open={:?}",
            sol.metrics, sol.repaired, sol.open);
        let mut open_nets = BTreeSet::new();
        for (net, reason) in &sol.open {
            assert!(problem_nets.contains(net),
                "{name}: claims hold: open net {net:?} ({reason:?}) is outside {problem_nets:?}");
            assert!(open_nets.insert(*net),
                "{name}: claims hold: duplicate open net {net:?}: {:?}", sol.open);
        }
        assert_eq!(sol.metrics.nets_open as usize, open_nets.len(),
            "{name}: claims hold: metrics open count matches the list: {:?}", sol.open);
        for net in problem_nets.difference(&open_nets) {
            assert!(!conn.unrouted.contains(net),
                "{name}: claims hold: routed net {net:?} remains unrouted: {:?}", conn.unrouted);
            assert_eq!(conn.clusters.iter().filter(|cluster| cluster.nets.contains(net)).count(), 1,
                "{name}: claims hold: routed net {net:?} has exactly one cluster");
        }
        for net in &conn.unrouted {
            assert!(p.conn.unrouted.contains(net) || open_nets.contains(net),
                "{name}: claims hold: previously connected net {net:?} was cut silently; open={:?}", sol.open);
        }
        for (index, net) in p.board.nets.iter().enumerate() {
            let id = NetId(index as u32);
            if p.conn.unrouted.contains(&id) || open_nets.contains(&id) {
                continue;
            }
            let unconnected: Vec<_> = result.violations.iter().filter(|v| {
                v.kind == CheckKind::Unconnected && v.message == format!("{} not connected here", net.name)
            }).collect();
            assert!(unconnected.is_empty(),
                "{name}: claims hold: previously connected net {id:?} has Unconnected findings: {unconnected:#?}");
        }

        if name == "keepout" || name == "locked_via" {
            // Both source cases require every connection, including GND in LockedVias.
            // Evaluate routeGate checks on the final applied organic solution as well.
            assert!(!problem_nets.is_empty(), "{name}: all connections routed: nonempty routing problem");
            assert!(open_nets.is_empty() && conn.unrouted.is_empty(),
                "{name}: all connections routed: open={:?}, unrouted={:?}", sol.open, conn.unrouted);
        }
        if name == "keepout" {
            check_keepouts(&after);
        }
        if name == "locked_via" {
            let original: Vec<_> = p.board.vias.iter().filter(|v| v.locked).collect();
            let retained: Vec<_> = after.vias.iter().filter(|v| v.locked).collect();
            assert_eq!(original.len(), 4, "{name}: fixture has four locked vias");
            assert_eq!(retained, original,
                "{name}: four locked vias unchanged: IDs, position, size, net, span, lock and origin");
            for signal in ["SIG", "SIG2"] {
                let id = NetId(after.nets.iter().position(|n| n.name == signal)
                    .unwrap_or_else(|| panic!("{name}: fixture contains {signal}")) as u32);
                assert!(problem_nets.contains(&id) && !open_nets.contains(&id) && !conn.unrouted.contains(&id),
                    "{name}: {signal} routed after relaxation");
                assert_eq!(conn.clusters.iter().filter(|c| c.nets.contains(&id)).count(), 1,
                    "{name}: {signal} belongs to one connected cluster");
            }
            // SIG, routed first, walls SIG2's pad in (no via may be drilled in a pad, and a via
            // keepout covers the pocket beside it), so only rip-up and reroute connects SIG2.
            assert!(first.grid.metrics.work.rips > 0, "{name}: rip-up ran: {:?}", first.grid.metrics.work);
            // R4:LockedVias' independent copper-to-copper clearance check.
            let mut min_gap = f64::INFINITY;
            for via in &retained {
                for track in after.tracks.iter().filter(|t| t.net != via.net) {
                    let pts = track_points(&after, track);
                    for (i, segment) in pts.windows(2).enumerate() {
                        let gap = point_segment_distance(xy(via.at), segment[0], segment[1])
                            - via.pad as f64 / 2e6 - track.seg_width(i) as f64 / 2e6;
                        min_gap = min_gap.min(gap);
                    }
                }
            }
            assert!(min_gap.is_finite() && min_gap >= 0.20 - 0.001,
                "{name}: foreign tracks keep locked-via clearance: min gap={min_gap} mm");
        }

        // A fresh relaxer on the same grid route (the grid router's own tests hold its
        // determinism): same step, solution and report. Determinism is the job's, not a board's:
        // small boards prove it, the pair board with and without the pair term and locked_via
        // (whose grid route rips up and reroutes SIG, and whose relax moves the routed vias).
        if name == "diff_pair_simple" || name == "diff_pair_simple (pairs)" || name == "locked_via" {
            determinism_checked += 1;
            let again = relax_with(p, &first.basis, &first.grid, &Progress::new(), first.grid.metrics.work.transactions, finish);
            assert!(again == (first.step, first.solution.clone(), first.report.clone()), "{name}: deterministic: a second relax gives the same step, solution and report");
        }
        eprintln!("organic {name}: board_ms={:.3}", board_started.elapsed().as_secs_f64() * 1_000.0);
    }
    assert!(pre_checked > 0, "the pre stage accepted a change on some sample board, so its via check ran");
    assert_eq!(determinism_checked, 3, "the determinism check ran on each of its three runs");
}

fn xy(p: P) -> (f64, f64) {
    (p.x as f64 / 1e6, p.y as f64 / 1e6)
}

fn track_points(board: &Board, track: &Track) -> Vec<(f64, f64)> {
    let mut points = vec![xy(track.path.start)];
    for segment in &track.path.segs {
        assert!(matches!(segment, Seg::Line(_)),
            "{}: independent geometry check: FortyFive/organic track {:?} is a polyline", board.name, track.id);
        points.push(xy(segment.end()));
    }
    points
}

// router_tests_main.cpp koPip / koEdgeDist: independent of Fab's geometry/DRC.
fn inside(poly: &[(f64, f64)], p: (f64, f64)) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > p.1) != (yj > p.1) && p.0 < (xj - xi) * (p.1 - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// A polyline passes through `poly`: a point sampled every 0.02 mm along one of its segments lies
/// inside (a long straight segment crosses an area with no vertex in it).
fn passes_through(poly: &[(f64, f64)], points: &[(f64, f64)]) -> bool {
    points.windows(2).any(|s| {
        let (a, b) = (s[0], s[1]);
        let steps = ((b.0 - a.0).hypot(b.1 - a.1) / 0.02).ceil().max(1.0) as usize;
        (0..=steps).any(|step| {
            let t = step as f64 / steps as f64;
            inside(poly, (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t))
        })
    })
}

fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a.0 + t * dx - p.0).hypot(a.1 + t * dy - p.1)
}

fn edge_distance(poly: &[(f64, f64)], p: (f64, f64)) -> f64 {
    poly.iter().zip(poly.iter().cycle().skip(1)).take(poly.len())
        .map(|(&a, &b)| point_segment_distance(p, a, b)).fold(f64::INFINITY, f64::min)
}

fn intrusions(board: &Board, areas: &[samples::RuleArea]) -> Vec<String> {
    let tracks: Vec<_> = board.tracks.iter().map(|t| (t, track_points(board, t))).collect();
    let copper = board.stackup.copper();
    let mut hits = Vec::new();
    for k in areas {
        let poly: Vec<_> = k.poly.iter().copied().map(xy).collect();
        let hit = |p| inside(&poly, p)
            || (k.clearance > 0 && edge_distance(&poly, p) < k.clearance as f64 / 1e6 - 0.001);
        if k.forbid.has(Forbid::TRACKS) {
            for (track, points) in &tracks {
                if !k.layers.has(track.layer) {
                    continue;
                }
                let bad = points.windows(2).enumerate().any(|(i, s)| {
                    let (a, b) = (s[0], s[1]);
                    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                    let len = dx.hypot(dy);
                    let (nx, ny) = if len > 1e-9 { (-dy / len, dx / len) } else { (0.0, 0.0) };
                    let half = track.seg_width(i) as f64 / 2e6 - 0.001;
                    let steps = (len / 0.02).ceil().max(1.0) as usize;
                    (0..=steps).any(|step| {
                        let t = step as f64 / steps as f64;
                        [0.0, half, -half].into_iter().any(|o| hit((a.0 + dx * t + nx * o, a.1 + dy * t + ny * o)))
                    })
                });
                if bad {
                    hits.push(format!("track {:?} on {:?} in {}", track.id, track.layer, k.name));
                }
            }
        }
        if k.forbid.has(Forbid::VIAS) {
            for via in &board.vias {
                let start = copper.iter().position(|l| *l == via.span.0)
                    .unwrap_or_else(|| panic!("{}: via {:?} span starts on a copper layer", board.name, via.id));
                let end = copper.iter().position(|l| *l == via.span.1)
                    .unwrap_or_else(|| panic!("{}: via {:?} span ends on a copper layer", board.name, via.id));
                assert!(start <= end, "{}: via {:?} span is in stack order", board.name, via.id);
                if !copper[start..=end].iter().any(|l| k.layers.has(*l)) {
                    continue;
                }
                let (x, y) = xy(via.at);
                let r = via.pad as f64 / 2e6 - 0.001;
                if hit((x, y)) || (0..16).any(|i| {
                    let angle = i as f64 * 0.3927;
                    hit((x + r * angle.cos(), y + r * angle.sin()))
                }) {
                    hits.push(format!("via {:?} in {}", via.id, k.name));
                }
            }
        }
    }
    hits
}

fn inject_track(board: &mut Board, net: NetId, layer: LayerId, a: P, b: P) {
    let id = board.alloc_id();
    board.tracks.push(Track {
        id, net, layer, w: mm(0.25), widths: None, path: Path::new(a).line(b),
        ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
    });
}

fn check_keepouts(board: &Board) {
    let name = &board.name;
    let areas = samples::keepout_areas();
    assert_eq!(board.keepouts.len(), areas.len(), "{name}: all five reference rule areas retained");
    let hits = intrusions(board, &areas);
    assert!(hits.is_empty(), "{name}: independent per-layer track/via/margin intrusion check: {hits:?}");

    let no_vias = areas.iter().find(|k| k.name == "KO_C_NO_VIAS")
        .expect("keepout: no-vias area exists");
    let poly: Vec<_> = no_vias.poly.iter().copied().map(xy).collect();
    let tracks = board.tracks.iter().filter(|t| passes_through(&poly, &track_points(board, t))).count();
    let vias = board.vias.iter().filter(|v| {
        inside(&poly, xy(v.at)) || edge_distance(&poly, xy(v.at)) < v.pad as f64 / 2e6
    }).count();
    assert!(tracks >= 4 && vias == 0,
        "{name}: no-vias area holds at least four tracks and no vias: tracks={tracks}, vias={vias}");
    let front_only = areas.iter().find(|k| k.name == "KO_A_FCU_NO_TRACKS")
        .expect("keepout: front-only no-tracks area exists");
    let poly: Vec<_> = front_only.poly.iter().copied().map(xy).collect();
    let back_tracks = board.tracks.iter().filter(|t| {
        t.layer == LayerId::B_CU && passes_through(&poly, &track_points(board, t))
    }).count();
    assert!(back_tracks > 0, "{name}: front-only no-tracks area is crossed on back copper");

    // S:Keepout's negative control for the sampler, on a clone of the final applied organic
    // result (Fab's keepout DRC has its own tests): crossings each area allows pass, and
    // injected intrusions are found.
    let mut reference = board.clone();
    let inj = NetId(reference.nets.len() as u32);
    reference.nets.push(BoardNet { name: "INJ".into(), class: reference.nets[0].class });
    inject_track(&mut reference, inj, LayerId::B_CU, P::mm(18.0, -33.0), P::mm(22.0, -33.0));
    let mut allowed = reference.clone();
    inject_track(&mut allowed, inj, LayerId::B_CU, P::mm(17.0, -4.0), P::mm(17.0, -30.0));
    inject_track(&mut allowed, inj, LayerId::F_CU, P::mm(54.0, -4.0), P::mm(54.0, -30.0));
    assert!(intrusions(&allowed, &areas).is_empty(),
        "{name}: negative control: sampler permits allowed layer/type crossings");
    let mut forbidden = reference;
    inject_track(&mut forbidden, inj, LayerId::F_CU, P::mm(30.0, -17.0), P::mm(50.0, -17.0));
    inject_track(&mut forbidden, inj, LayerId::F_CU, P::mm(17.5, -4.0), P::mm(17.5, -30.0));
    let id = forbidden.alloc_id();
    forbidden.vias.push(Via {
        id, net: inj, at: P::mm(54.0, -17.0), drill: mm(0.4), pad: mm(0.8),
        span: (LayerId::F_CU, LayerId::B_CU), kind: ViaKind::Through,
        tent: Tent::Both, backdrill: None, locked: false, origin: Origin::Authored,
        flash: Flash::All,
    });
    let hits = intrusions(&forbidden, &areas);
    assert!(hits.len() >= 3, "{name}: negative control: independent sampler flags injected intrusions: {hits:?}");
}

/// A sample's point: source mm, Y down.
fn at(x: f64, y: f64) -> P {
    P::mm(x, -y)
}

/// The anchor and centre of component `refdes`'s only pad.
fn pad_end(b: &Board, refdes: &str) -> (Anchor, P) {
    let placed = b.placed.iter().find(|pl| pl.refdes == refdes).expect("guard pad").id;
    let anchor = Anchor::Pad { placed, pad: PadIx(0) };
    (anchor, b.anchor_pos(anchor).expect("guard pad position"))
}

/// A grid track on F.Cu through `pts`, written by hand.
fn grid_track(id: u32, net: NetId, pts: &[P], ends: [Anchor; 2]) -> Item {
    let mut path = Path::new(pts[0]);
    for q in &pts[1..] {
        path.segs.push(Seg::Line(*q));
    }
    Item::Track(Track {
        id: ItemId(id), net, layer: LayerId::F_CU, w: mm(0.25), widths: None, path, ends, locked: false,
        // as the grid router makes it: the organic job moves only solved copper
        origin: Origin::Solved { by: fab_elec_route::solvers::GRID },
    })
}

/// Whether `net` is in one piece once `sol` is applied to `p`'s board and its pours refilled.
fn connected(p: &RouteProblem, sol: &RouteSolution, net: NetId) -> bool {
    let mut b = (*p.board).clone();
    b.apply_all(&sol.ops()).expect("solution applies");
    let fills = fill_all(&b, &p.rules);
    !connect(&b, &fills).unrouted.contains(&net)
}

#[test]
fn organic_guard_pour_cut() {
    let p = setup(samples::guard_pour_squeeze());
    let b = &*p.board;
    let net = |name: &str| NetId(b.nets.iter().position(|n| n.name == name).expect("guard net") as u32);
    let (gnd, sig) = (net("GND"), net("SIG"));
    let (s1, p1) = pad_end(b, "S1");
    let (s2, p2) = pad_end(b, "S2");
    // the grid form bows 2.2 mm off the straight line: the pour strip at the keepout's end is
    // about 2 mm wide
    let grid = RouteSolution {
        add: vec![grid_track(b.next_item, sig, &[p1, at(14.5, 6.0), at(14.5, 18.0), p2], [s1, s2])],
        ..RouteSolution::default()
    };
    assert!(connected(&p, &grid, gnd), "grid form: GND is one piece through the strip");

    let partials: Arc<Mutex<Vec<Arc<RouteSolution>>>> = Arc::default();
    let sink = partials.clone();
    let progress = Progress::at_steps((8..=4096).step_by(8).collect(), move |_, sol| sink.lock().unwrap().push(sol));
    // the relax alone (every finish pass off): the test pins the relax's fallback on this
    // hand-made grid form, which a pass that edits it first would change
    let (_, sol, report) = relax_with(&p, &Basis::of(&p), &grid, &progress, 0, &Finish::none());
    eprintln!("organic guard_pour_squeeze: {report:?} partials={}", partials.lock().unwrap().len());
    // the control of the cut's cause: G2's track fence leaves its join to the pour without a pour
    // path, so nothing keeps SIG off the strip
    assert_eq!(report.pour_unprotected, 1, "control: the forest cannot protect G2's join: {report:?}");

    // the control: a partial is offered without the connectivity check, so the relaxed form
    // shows that it cuts GND
    let last = partials.lock().unwrap().iter().rev().find(|s| s.add.iter().any(|it| *it.origin() == Origin::Solved { by: fab_elec_route::solvers::ORGANIC })).cloned();
    let last = last.expect("control: a partial with SIG relaxed was offered");
    assert!(!connected(&p, &last, gnd), "control: SIG relaxed squeezes the strip shut and cuts GND");

    assert_eq!((report.nets, report.reverted, report.idle), (0, 1, true), "pour cut: SIG keeps its grid form: {report:?}");
    assert_eq!(sol.add, grid.add, "pour cut: the final solution is the grid form");
    assert!(connected(&p, &sol, gnd), "pour cut: GND stays one piece");
}

#[test]
fn organic_pour_cut_reverts_only_the_nets_that_make_it() {
    let p = setup(samples::guard_pour_partial());
    let b = &*p.board;
    let net = |name: &str| NetId(b.nets.iter().position(|n| n.name == name).expect("guard net") as u32);
    let (gnd, sig, oth) = (net("GND"), net("SIG"), net("OTH"));
    let end = |refdes: &str| pad_end(b, refdes);
    let ((s1, p1), (s2, p2)) = (end("S1"), end("S2"));
    let ((g3, q3), (g4, q4)) = (end("G3"), end("G4"));
    let ((o1, r1), (o2, r2)) = (end("O1"), end("O2"));
    let n = b.next_item;
    // each grid form bows, so the relax would move each of them: SIG's 2.2 mm off its straight
    // line (which squeezes GND's strip shut), GND's (through a via of its own) and OTH's 2 mm
    // the via comes before the tracks that end on it: a track's anchor must exist when it is added
    let via = ItemId(n + 1);
    let grid = RouteSolution {
        add: vec![
            grid_track(n, sig, &[p1, at(14.5, 6.0), at(14.5, 18.0), p2], [s1, s2]),
            Item::Via(Via {
                id: via, net: gnd, at: at(6.0, 10.0), drill: mm(0.3), pad: mm(0.6),
                span: (LayerId::F_CU, LayerId::B_CU), kind: ViaKind::Through, tent: Tent::Both, backdrill: None,
                locked: false, origin: Origin::Solved { by: fab_elec_route::solvers::GRID }, flash: Flash::All,
            }),
            grid_track(n + 2, gnd, &[q3, at(4.0, 10.0), at(6.0, 10.0)], [g3, Anchor::Via(via)]),
            grid_track(n + 3, gnd, &[at(6.0, 10.0), at(8.0, 10.0), q4], [Anchor::Via(via), g4]),
            grid_track(n + 4, oth, &[r1, at(21.0, 8.0), at(21.0, 16.0), r2], [o1, o2]),
        ],
        ..RouteSolution::default()
    };
    assert!(connected(&p, &grid, gnd), "grid form: GND is one piece through the strip");

    let partials: Arc<Mutex<Vec<Arc<RouteSolution>>>> = Arc::default();
    let sink = partials.clone();
    let progress = Progress::at_steps((8..=4096).step_by(8).collect(), move |_, sol| sink.lock().unwrap().push(sol));
    // the relax alone (every finish pass off), as organic_guard_pour_cut
    let (_, sol, report) = relax_with(&p, &Basis::of(&p), &grid, &progress, 0, &Finish::none());
    eprintln!("organic guard_pour_partial: {report:?} partials={}", partials.lock().unwrap().len());
    // the control of the cut's cause: G2's track fence leaves its join to the pour without a pour
    // path, so nothing keeps SIG off the strip
    assert_eq!(report.pour_unprotected, 1, "control: the forest cannot protect G2's join: {report:?}");

    let relaxed = |s: &RouteSolution, net: NetId| s.add.iter().any(|it| matches!(it, Item::Track(t) if t.net == net)
        && *it.origin() == Origin::Solved { by: fab_elec_route::solvers::ORGANIC });
    let tracks = |s: &RouteSolution, net: NetId| s.add.iter().filter(|it| matches!(it, Item::Track(t) if t.net == net)).cloned().collect::<Vec<_>>();
    // the control: partials skip the connectivity check, so one with SIG relaxed shows its cut
    let last = partials.lock().unwrap().iter().rev().find(|s| relaxed(s, sig)).cloned();
    let last = last.expect("control: a partial with SIG relaxed was offered");
    assert!(!connected(&p, &last, gnd), "control: SIG relaxed squeezes the strip shut and cuts GND");
    assert!(relaxed(&last, oth), "control: OTH relaxes");
    let copper = |s: &RouteSolution, net: NetId| s.add.iter()
        .filter(|it| matches!(it, Item::Track(t) if t.net == net) || matches!(it, Item::Via(v) if v.net == net)).cloned().collect::<Vec<_>>();
    for (i, partial) in partials.lock().unwrap().iter().enumerate() {
        assert_eq!(copper(partial, gnd), copper(&grid, gnd), "partial {i}: a net with a pour keeps its grid tracks and vias whole (nothing blinks out of the preview)");
    }

    assert_eq!(copper(&sol, gnd), copper(&grid, gnd), "a net with a pour keeps its grid copper: the relaxer does not see its pour");
    assert_eq!(tracks(&sol, sig), tracks(&grid, sig), "the net whose relaxed form cuts the pour keeps its grid form");
    assert!(relaxed(&sol, oth), "a net far from the cut stays relaxed: a cut reverts only the nets nearest it: {report:?}");
    assert_eq!((report.nets, report.reverted), (1, 1), "OTH relaxed; SIG back in grid form (GND has a pour: it never lifts and does not count): {report:?}");
    assert!(connected(&p, &sol, gnd), "GND stays one piece");
}

#[test]
fn organic_hold_keeps_the_neighbours_of_a_reverted_net_relaxed() {
    let p = setup(samples::guard_pour_hold());
    let b = &*p.board;
    let net = |name: &str| NetId(b.nets.iter().position(|n| n.name == name).expect("guard net") as u32);
    let (gnd, sig, oth) = (net("GND"), net("SIG"), net("OTH"));
    let end = |refdes: &str| pad_end(b, refdes);
    let ((s1, p1), (s2, p2)) = (end("S1"), end("S2"));
    let ((g3, q3), (g4, q4)) = (end("G3"), end("G4"));
    let ((o1, r1), (o2, r2)) = (end("O1"), end("O2"));
    let n = b.next_item;
    // SIG's grid form bows 2.2 mm off its straight line, as in the pour-partial guard; OTH's bows
    // 2 mm around it, and OTH's straight line runs 0.2 mm from the line of SIG's grid form
    let grid = RouteSolution {
        add: vec![
            grid_track(n, sig, &[p1, at(14.5, 6.0), at(14.5, 18.0), p2], [s1, s2]),
            grid_track(n + 1, gnd, &[q3, at(6.0, 10.0), q4], [g3, g4]),
            grid_track(n + 2, oth, &[r1, at(16.7, 4.0), at(16.7, 20.0), r2], [o1, o2]),
        ],
        ..RouteSolution::default()
    };
    assert!(connected(&p, &grid, gnd), "grid form: GND is one piece through the strip");

    // the relax alone (every finish pass off), as the pour-partial guard
    let (_, sol, report) = relax_with(&p, &Basis::of(&p), &grid, &Progress::new(), 0, &Finish::none());
    eprintln!("organic guard_pour_hold: {report:?}");
    // the control of the cut's cause: G2's track fence leaves its join to the pour without a pour
    // path, so nothing keeps SIG off the strip
    assert_eq!(report.pour_unprotected, 1, "control: the forest cannot protect G2's join: {report:?}");

    let relaxed = |s: &RouteSolution, net: NetId| s.add.iter().any(|it| matches!(it, Item::Track(t) if t.net == net)
        && *it.origin() == Origin::Solved { by: fab_elec_route::solvers::ORGANIC });
    let tracks = |s: &RouteSolution, net: NetId| s.add.iter().filter(|it| matches!(it, Item::Track(t) if t.net == net)).cloned().collect::<Vec<_>>();
    assert_eq!(tracks(&sol, sig), tracks(&grid, sig), "SIG's relaxed form cuts the pour: SIG goes back to its grid form");
    assert!(relaxed(&sol, oth),
        "OTH relaxed into the room SIG left; held in its grid form, SIG pushes OTH clear, so OTH stays relaxed instead of reverting with it: {report:?}");
    assert!(connected(&p, &sol, gnd), "GND stays one piece");
    assert_eq!((report.nets, report.reverted, report.held), (1, 1, 1), "OTH relaxed, SIG held and back in grid form (GND has a pour): {report:?}");
}

#[test]
fn organic_keeps_a_pocket_pour_whole_with_both_nets_relaxed() {
    let p = setup(samples::guard_pour_pocket());
    let b = &*p.board;
    let net = |name: &str| NetId(b.nets.iter().position(|n| n.name == name).expect("guard net") as u32);
    let (gnd, s1, s2) = (net("GND"), net("SIG1"), net("SIG2"));
    let end = |refdes: &str| pad_end(b, refdes);
    let ((a1, p1), (b1, q1)) = (end("A1"), end("B1"));
    let ((a2, p2), (b2, q2)) = (end("A2"), end("B2"));
    let n = b.next_item;
    // the grid forms bunch both tracks against the mouth's right wall: centres 1.15 and 1.65 mm
    // across it, leaving the fill 1.025 mm on the left
    let grid = RouteSolution {
        add: vec![
            grid_track(n, s1, &[p1, at(10.65, 7.0), at(11.15, 7.5), at(11.15, 11.0), at(10.65, 11.5), q1], [a1, b1]),
            grid_track(n + 1, s2, &[p2, at(11.35, 6.0), at(11.65, 6.3), at(11.65, 11.5), at(11.35, 11.8), q2], [a2, b2]),
        ],
        ..RouteSolution::default()
    };
    assert!(connected(&p, &grid, gnd), "grid form: GND is one piece through the mouth");
    let straight = RouteSolution {
        add: vec![grid_track(n, s1, &[p1, q1], [a1, b1]), grid_track(n + 1, s2, &[p2, q2], [a2, b2])],
        ..RouteSolution::default()
    };
    assert!(!connected(&p, &straight, gnd), "fixture: the straight forms close the mouth");

    let (_, sol, report) = relax_with(&p, &Basis::of(&p), &grid, &Progress::new(), 0, &Finish::none());
    eprintln!("organic guard_pour_pocket: {report:?}");
    assert!(report.pour_paths >= 1 && report.pour_unprotected == 0,
        "the forest lays a pour path through the mouth and leaves no start-form join unprotected: {report:?}");
    assert_eq!((report.nets, report.held, report.reverted), (2, 0, 0),
        "both nets relax and none is held: the pour's path through the mouth keeps them apart: {report:?}");
    assert_eq!((report.pour_tight, report.pour_escaped), (0, 0),
        "the relax pushed every moving neighbour off the paths, and each path still lies in its refilled fill: {report:?}");
    assert!(connected(&p, &sol, gnd), "GND stays one piece");
}

#[test]
fn organic_relaxes_around_an_unplated_hole() {
    let p = setup(samples::guard_npth());
    let b = &*p.board;
    let sig = NetId(b.nets.iter().position(|n| n.name == "SIG").expect("guard net") as u32);
    let (s1, p1) = pad_end(b, "S1");
    let (s2, p2) = pad_end(b, "S2");
    // the grid form bows 2 mm around the hole on SIG's straight line
    let grid = RouteSolution {
        add: vec![grid_track(b.next_item, sig, &[p1, at(9.0, 12.0), at(11.0, 14.0), at(13.0, 14.0), at(15.0, 12.0), p2], [s1, s2])],
        ..RouteSolution::default()
    };
    let (_, sol, report) = relax_with(&p, &Basis::of(&p), &grid, &Progress::new(), 0, &Finish::none());
    eprintln!("organic guard_npth: {report:?}");
    assert_eq!((report.nets, report.reverted), (1, 0),
        "SIG relaxes clear of the hole's wall, so its relaxed form passes verification and stays: {report:?}");
    assert!(sol.add.iter().any(|it| matches!(it, Item::Track(t) if t.net == sig)
        && *it.origin() == Origin::Solved { by: fab_elec_route::solvers::ORGANIC }), "SIG is in relaxed form");
    // DRC on the result: no track or via within clearance of the hole (the board house drills it)
    let mut after = (*p.board).clone();
    after.apply_all(&sol.ops()).expect("the solution applies");
    let fills = fill_all(&after, &p.rules);
    let conn = connect(&after, &fills);
    let near: Vec<_> = drc(&after, &conn, &p.rules, &fills, None).violations.into_iter()
        .filter(|v| matches!(v.kind, CheckKind::Clearance | CheckKind::HoleClearance))
        .collect();
    assert!(near.is_empty(), "no copper within clearance of the unplated hole: {near:?}");
}

#[test]
fn organic_guard_id_reuse() {
    let p = setup(samples::guard_id_reuse());
    let b = &*p.board;
    let net = |name: &str| NetId(b.nets.iter().position(|n| n.name == name).expect("guard net") as u32);
    let (a, bn) = (net("A"), net("B"));
    let (a1, pa1) = pad_end(b, "A1");
    let (a2, pa2) = pad_end(b, "A2");
    let (b1, pb1) = pad_end(b, "B1");
    let (b2, pb2) = pad_end(b, "B2");
    let (b3, pb3) = pad_end(b, "B3");
    let n = b.next_item;
    // B's host turns at the junction and the branch goes on straight from its first leg. The
    // simplifier hands a split track's id to the chain holding most of its nodes: the first leg
    // gets more nodes than the second, so the second leg is the chain left without an id.
    let j = at(25.0, 5.0);
    let leg = [at(17.5, 5.0), at(20.0, 5.0), at(22.5, 5.0)];
    let host = grid_track(n, bn, &[pb1, leg[0], leg[1], leg[2], j, pb2], [b1, b2]);
    let Item::Track(h) = &host else { unreachable!() };
    let branch = grid_track(n + 1, bn, &[pb3, j], [b3, Anchor::Track { id: h.id, at: h.path.locate(j).0 }]);
    // Each net holds a loop: a short way and a longer track between the same pads. The
    // simplifier takes A first and drops its longer track, the highest port id. Then B: its loop
    // goes, and the chain walk joins the host's first leg to the branch (the straight way through
    // the junction), so the host's second leg is a chain of its own whose id is the highest
    // surviving id + 1: the dropped A track's port id. That track's Fab id must not come back on
    // a track of B.
    let grid = RouteSolution {
        add: vec![
            host.clone(),
            branch,
            grid_track(n + 2, bn, &[pb1, at(15.0, 14.0), at(25.0, 14.0), pb2], [b1, b2]),
            grid_track(n + 3, a, &[pa1, pa2], [a1, a2]),
            grid_track(n + 4, a, &[pa1, at(3.0, 10.0), at(13.0, 10.0), pa2], [a1, a2]),
        ],
        ..RouteSolution::default()
    };
    // the relax alone (every finish pass off): the test pins the relax's id rule on this
    // hand-made grid form, which a pass that edits it first would change
    let (_, sol, report) = relax_with(&p, &Basis::of(&p), &grid, &Progress::new(), 0, &Finish::none());
    eprintln!("organic guard_id_reuse: {report:?}");

    let tracks = |net: NetId| sol.add.iter().filter_map(|it| match it { Item::Track(t) if t.net == net => Some(t), _ => None }).collect::<Vec<_>>();
    let grid_net: std::collections::BTreeMap<ItemId, NetId> =
        grid.add.iter().filter_map(|it| match it { Item::Track(t) => Some((t.id, t.net)), _ => None }).collect();
    assert_eq!(report.reverted, 0, "id reuse: no net reverts: {report:?}");
    assert_eq!(tracks(a).len(), 1, "id reuse: the simplifier dropped A's longer loop track");
    let b_tracks = tracks(bn);
    assert_eq!(b_tracks.len(), 2, "id reuse: B lost its loop, and its host's second leg is a chain of its own");
    for t in tracks(a).into_iter().chain(b_tracks.iter().copied()) {
        if let Some(&g) = grid_net.get(&t.id) {
            assert_eq!(t.net, g, "id reuse: a track keeps a grid id only on that grid track's net ({:?})", t.id);
        }
    }
    assert!(
        b_tracks.iter().any(|t| !grid_net.contains_key(&t.id)),
        "id reuse: B's new chain gets a fresh id: {:?}",
        b_tracks.iter().map(|t| t.id).collect::<Vec<_>>()
    );
    let ids: BTreeSet<_> = sol.add.iter().map(Item::id).collect();
    assert_eq!(ids.len(), sol.add.len(), "id reuse: ids are unique");
    let mut after = (*p.board).clone();
    assert!(after.apply_all(&sol.ops()).is_ok(), "id reuse: the solution applies");
}

#[test]
fn organic_partials_keep_copper_ids() {
    let mut board = samples::guard_id_reuse();
    let net = |name: &str| NetId(board.nets.iter().position(|n| n.name == name).expect("guard net") as u32);
    let (a, b) = (net("A"), net("B"));
    // The five-pad fixture needs one inner signal layer for LayerReassign's split case:
    // the two foreign crossings have different layer constraints and leave a legal via site.
    let mut inner = board.stackup.layers.iter().find(|l| {
        l.kind == fab_elec_board::StackKind::Copper(LayerId::F_CU)
    }).expect("step 0: fixture has front copper").clone();
    inner.kind = fab_elec_board::StackKind::Copper(LayerId::inner(1));
    let back = board.stackup.layers.iter().position(|l| {
        l.kind == fab_elec_board::StackKind::Copper(LayerId::B_CU)
    }).expect("step 0: fixture has back copper");
    board.stackup.layers.insert(back, inner);
    inject_track(&mut board, a, LayerId::B_CU, at(19.0, 6.5), at(19.0, 9.5));
    inject_track(&mut board, a, LayerId::F_CU, at(26.0, 6.5), at(26.0, 9.5));
    board.reindex();
    let p = setup(board);
    let (b1, pb1) = pad_end(&p.board, "B1");
    let (b2, pb2) = pad_end(&p.board, "B2");
    let (b3, pb3) = pad_end(&p.board, "B3");
    let n = p.board.next_item;
    let positions = [at(17.0, 8.0), at(17.0, 13.0), at(30.0, 8.0)];
    let via_ends = [Anchor::Via(ItemId(n)), Anchor::Via(ItemId(n + 1)), Anchor::Via(ItemId(n + 2))];
    let mut grid = RouteSolution::default();
    for (k, &at) in positions.iter().enumerate() {
        grid.add.push(Item::Via(Via {
            id: ItemId(n + k as u32), net: b, at, drill: mm(0.4), pad: mm(0.8),
            span: (LayerId::F_CU, LayerId::B_CU), kind: ViaKind::Through,
            tent: Tent::Both, backdrill: None, locked: false,
            origin: Origin::Solved { by: fab_elec_route::solvers::GRID },
            flash: Flash::All,
        }));
    }
    let inner_track = |id, pts: &[P], ends| {
        let Item::Track(mut t) = grid_track(id, b, pts, ends) else { unreachable!() };
        t.layer = LayerId::inner(1);
        Item::Track(t)
    };
    grid.add.extend([
        grid_track(n + 3, b, &[pb1, positions[0]], [b1, via_ends[0]]),
        inner_track(n + 4, &[positions[0], positions[1]], [via_ends[0], via_ends[1]]),
        inner_track(n + 5, &[positions[0], at(22.0, 8.0), positions[2]], [via_ends[0], via_ends[2]]),
        grid_track(n + 6, b, &[positions[1], pb2], [via_ends[1], b2]),
        grid_track(n + 7, b, &[positions[2], pb3], [via_ends[2], b3]),
    ]);
    // Moving the two inner pieces at the first via to the front removes the first two
    // vias. The top crossing pins the right piece to the inner layer, so a new site via
    // at (22,8) replaces them. The back crossing supplies the left piece's distinct signature.
    // Only LayerReassign runs before the relax, preserving the hand-written split site.
    let finish = Finish { layer_reassign: true, ..Finish::none() };
    let mut grid_board = (*p.board).clone();
    grid_board.apply_all(&grid.ops()).expect("step 0: grid form applies");
    let base = grid_board.next_item;
    let basis = Basis::of(&p);
    const LAST_STEP: u32 = 4096;
    let collect = || {
        let partials: Arc<Mutex<Vec<(u32, Arc<RouteSolution>)>>> = Arc::default();
        let sink = partials.clone();
        // Keep the pre stage and first relax partials, including the old first relax offer
        // at step 9, then every eighth step so checking each partial stays inexpensive.
        let steps = (0..=9).chain((17..=LAST_STEP).step_by(8)).collect();
        let progress = Progress::at_steps(steps, move |step, sol| {
            sink.lock().unwrap().push((step, sol));
        });
        progress.offer(0, &grid);
        let (step, sol, report) = relax_with(&p, &basis, &grid, &progress, 0, &finish);
        let mut partials = partials.lock().unwrap().clone();
        // relax_with returns its final connectivity-checked solution separately from offers.
        partials.push((step, Arc::new(sol)));
        (step, report, partials)
    };
    let (step, report, partials) = collect();
    assert!(step <= LAST_STEP, "step {step}: every offered partial is in the replay range");
    assert!(report.idle && report.nets > 0, "step {step}: the split net lifts and settles: {report:?}");
    let pre_step = report.judge.accepted + report.judge.synced - report.judge.sync_failed;
    assert!(pre_step > 0, "step {pre_step}: LayerReassign accepted a change");
    let pre = &partials.iter().find(|(s, _)| *s == pre_step)
        .expect("pre-stage end: the accepted solution was offered").1;
    let fresh_vias = |sol: &RouteSolution| sol.add.iter().filter_map(|it| match it {
        Item::Via(v) if v.id.0 >= base => Some((v.net, v.id)),
        _ => None,
    }).collect::<Vec<_>>();
    let pre_vias = fresh_vias(pre);
    assert_eq!(pre_vias.len(), 1, "step {pre_step}: the pre stage adds one site via");
    let final_vias = fresh_vias(&partials.last().unwrap().1);
    // Match the sole pass-added via by net, not by id: intersecting ids would hide churn.
    assert_eq!(final_vias.iter().map(|v| v.0).collect::<Vec<_>>(),
        pre_vias.iter().map(|v| v.0).collect::<Vec<_>>(),
        "step {step}: the pass-added via survives to the final solution");
    let mut previous = BTreeMap::new();
    let mut relax_partials = 0;
    for (s, sol) in &partials {
        let ids: BTreeSet<_> = sol.add.iter().map(Item::id).collect();
        assert_eq!(ids.len(), sol.add.len(), "step {s}: item ids are unique within the partial");
        let kinds: BTreeMap<_, _> = sol.add.iter().filter_map(|it| match it {
            Item::Track(t) => Some((t.id, "track")),
            Item::Via(v) => Some((v.id, "via")),
            _ => None,
        }).collect();
        for (id, kind) in &kinds {
            if let Some(before) = previous.get(id) {
                assert_eq!(before, kind, "step {s}: persistent id {id:?} keeps its copper kind");
            }
        }
        previous = kinds;
        if *s > pre_step {
            relax_partials += 1;
            assert_eq!(fresh_vias(sol), pre_vias,
                "step {s}: pass-added vias keep their pre-stage ids throughout relaxation");
        }
    }
    assert!(relax_partials > 2, "step {step}: at least two relax offers and the final solution were checked");
    let (again_step, again_report, again) = collect();
    assert_eq!((again_step, again_report), (step, report), "step {step}: the run is deterministic");
    assert_eq!(again.len(), partials.len(), "step {step}: the replay has the same number of partials");
    for ((s, sol), (again_s, again_sol)) in partials.iter().zip(&again) {
        assert_eq!((again_s, again_sol), (s, sol), "step {s}: the replay partial is identical");
    }
}
