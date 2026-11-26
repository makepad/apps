//! Guards R1's fixture routing promise: complete, applicable, DRC-clean, anchored copper, truthful
//! connectivity claims, deterministic solutions, an applicable first anytime offer, and a net
//! routed on committed copper as it would be among the job's own.

use fab_elec_board::drc::sel_of;
use fab_elec_board::zone::fill_all;
use fab_elec_board::{connect, drc, resolve_rules, Anchor, Board, Item};
use fab_elec_circuit::CheckKind;
use fab_elec_core::{LayerId, NetId, Sel};
use fab_elec_fixtures::ledctl;
use fab_elec_route::auto::GridRouter;
use fab_elec_route::grid::search::Costs;
use fab_elec_route::{problem, scope_nets, Autorouter, Base, Effort, JobPhase, Progress, RBase, RetryWhy, RouteScope, Style};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

#[test]
fn grid_router_routes_the_fixture() {
    let design = ledctl();
    let board: Arc<Board> = design.units[0].board.clone().expect("fixture setup: ledctl unit 0 has a board");
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let base = Base::new(board, rules.clone(), conn, fills);
    let nets = scope_nets(&base.board, &base.conn, &RouteScope::All);
    let world = Arc::new(RBase::build(&base.board));
    let p = problem(&base, world, &nets, Style::FortyFive, Effort::default());
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let output = statuses.clone();
    let progress = Progress::new().with_status(move |status| {
        output.lock().unwrap().push(status);
    });
    let sol = GridRouter::default().route(&p, &progress);
    let statuses = std::mem::take(&mut *statuses.lock().unwrap());
    let begins: Vec<_> = statuses.iter().enumerate().filter_map(|(index, status)| {
        (status.attempt == 0 && status.retry.is_none() && status.blocked_by.is_none()).then_some(index)
    }).collect();
    assert_eq!(begins.first().copied(), Some(0), "status: the stream starts with a begin publication");
    assert!(begins.len() >= p.nets.len(), "status: the first pass begins every planned net");
    assert_eq!(
        begins.iter().take(p.nets.len()).map(|&index| statuses[index].current).collect::<Vec<_>>(),
        p.nets.iter().map(|net| Some(net.net)).collect::<Vec<_>>(),
        "status: the first pass begins each planned net once in plan order"
    );
    if let Some(&index) = begins.get(p.nets.len()) {
        // offer updates the counts after every first-pass transaction, even without a copy.
        assert!(statuses[index].failed > 0,
            "status: a later segment requires an open net after the first pass: {:?}", statuses[index]);
    }

    // A transaction publishes PourNeck? Block?, in that order. An attempt can repeat
    // that sequence after each RipUp; a ripped net publishes Ripped before its first
    // transaction, and repeats it after each RipUp of its own (a ripped net rips in turn).
    // Without pour-neck retries, blocks or the rip-up phase, these checks reduce to
    // each planned net published exactly once in plan order.
    let mut pour_neck_retries = 0usize;
    let mut rip_up_retries = 0usize;
    for (segment_index, &start) in begins.iter().enumerate() {
        let end = begins.get(segment_index + 1).copied().unwrap_or(statuses.len());
        let segment = &statuses[start..end];
        let begin = &segment[0];
        assert!(p.nets.iter().any(|net| begin.current == Some(net.net)),
            "status: every segment begins a planned net: {begin:?}");
        assert!(segment.iter().all(|status| status.current == begin.current),
            "status: every publication names its segment's net: {segment:?}");
        let first_pass = segment_index < p.nets.len();
        let mut retried_transaction = false;
        let mut blocked_transaction = false;
        for (index, pair) in segment.windows(2).enumerate() {
            let (previous, status) = (&pair[0], &pair[1]);
            // block_net retains the previous retry reason, so count retries by their
            // attempt transition, not merely by the presence of a retry reason.
            if status.attempt == previous.attempt {
                assert!(status.blocked_by.is_some(),
                    "status: a publication without a new attempt must name a blocker: {pair:?}");
                assert_eq!(status.retry, previous.retry,
                    "status: a block preserves the previous retry reason: {pair:?}");
                assert!(!blocked_transaction,
                    "status: a transaction publishes at most one block: {segment:?}");
                blocked_transaction = true;
                continue;
            }
            assert_eq!(Some(status.attempt), previous.attempt.checked_add(1),
                "status: a retry increments the previous attempt by exactly one: {pair:?}");
            assert_eq!(status.blocked_by, previous.blocked_by,
                "status: a retry preserves the previous blocker: {pair:?}");
            assert!(status.retry.is_some(),
                "status: a retry publication names its reason: {pair:?}");
            match status.retry.unwrap() {
                RetryWhy::PourNeck => {
                    assert!(!retried_transaction,
                        "status: a transaction publishes at most one pour-neck retry: {segment:?}");
                    assert!(!blocked_transaction,
                        "status: a pour-neck retry precedes any block in its transaction: {segment:?}");
                    retried_transaction = true;
                    pour_neck_retries += 1;
                }
                RetryWhy::RipUp => {
                    assert!(!first_pass,
                        "status: rip-up retries occur only after the first pass: {segment:?}");
                    retried_transaction = false;
                    blocked_transaction = false;
                    rip_up_retries += 1;
                }
                RetryWhy::Ripped => {
                    assert!(!first_pass,
                        "status: ripped-net reroutes occur only after the first pass: {segment:?}");
                    assert_eq!(index, 0,
                        "status: a ripped-net retry directly follows its begin: {segment:?}");
                    assert_eq!(status.attempt, 1,
                        "status: a ripped-net retry is attempt one: {segment:?}");
                }
            }
        }
    }
    if !progress.cancelled() {
        let work = sol.metrics.work;
        assert_eq!(rip_up_retries > 0, work.rips > 0,
            "status: rip-up retries are published exactly when work counts a rip: work={work:?}");
        assert_eq!(pour_neck_retries, work.retries as usize,
            "status: every counted pour-neck retry is published exactly once: work={work:?}");
        // Both restore paths preserve work. Ripped count need not equal rips: a pour
        // block can replace the blocker list without clearing contested cells, and
        // rip counts every listed net while the reroute loop visits only planned nets.
    }
    assert!(statuses.iter().all(|status| status.phase == JobPhase::Nets), "status: the grid router stays in the nets phase");
    assert!(statuses.iter().all(|status| status.total == p.nets.len() as u32), "status: every publication carries the problem's net count");
    let status = progress.status();
    assert_eq!(
        (status.routed as usize + status.failed as usize, status.total as usize),
        (p.nets.len(), p.nets.len()),
        "status: the final counts cover every problem net once, even when no copy was requested, \
         and a net outside the problem that a repair reconnected is not routed of the total \
         (repaired={:?}, metrics={:?})",
        sol.repaired,
        sol.metrics
    );
    let added_ids: BTreeSet<_> = sol.add.iter().map(Item::id).collect();

    let mut b2 = (*base.board).clone();
    let applied = b2.apply_all(&sol.ops());
    assert!(
        applied.is_ok(),
        "applies: added items {added_ids:?}, removed items {:?}: {applied:?}",
        sol.remove
    );

    let fills2 = fill_all(&b2, &rules);
    let conn2 = connect(&b2, &fills2);
    let result = drc(&b2, &conn2, &rules, &fills2, None);
    let mut added_sels: BTreeSet<_> = added_ids.iter().map(|&id| Sel::Item(base.unit, id)).collect();
    // Copper can be reported by a feature selection instead of its owning board item.
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

    for item in &sol.add {
        if let Item::Track(track) = item {
            assert!(
                !track.ends.contains(&Anchor::Free),
                "anchored: added track {:?} on net {:?} has a free end: {:?}",
                track.id,
                track.net,
                track.ends
            );
        }
    }
    // The demo board routes every net, with rip-up when the first pass leaves one blocked.
    assert!(
        sol.open.is_empty(),
        "routes every net: open nets remain among {problem_nets:?}: {:?}",
        sol.open
    );
    assert!(
        !sol.add.is_empty(),
        "routes something: no added copper for nets {problem_nets:?}; metrics={:?}, open={:?}",
        sol.metrics,
        sol.open
    );

    let repeated = GridRouter::default().route(&p, &Progress::new());
    assert_eq!(sol, repeated, "deterministic: repeated routing differs for nets {problem_nets:?}");

    // The first offer is wanted by default; no later offers were requested during routing.
    let (step, partial) = progress.take_offer().unwrap_or_else(|| {
        panic!("anytime: no first partial was offered for nets {problem_nets:?}; added items={added_ids:?}")
    });
    assert_eq!(
        step, 1,
        "anytime: the first offer must remain available for nets {problem_nets:?}; partial={partial:?}"
    );
    let mut partial_board = (*base.board).clone();
    let partial_applied = partial_board.apply_all(&partial.ops());
    assert!(
        partial_applied.is_ok(),
        "anytime: a partial is legal and applies; step={step}, partial={partial:?}, result={partial_applied:?}"
    );
}

#[test]
fn grid_router_keeps_a_pour_neck_open() {
    let (board, gnd, signal, fence) = fab_elec_fixtures::pour_neck();
    let setup = |board: Board| {
        let board = Arc::new(board);
        let rules = Arc::new(resolve_rules(&board, None));
        let fills = Arc::new(fill_all(&board, &rules));
        let conn = Arc::new(connect(&board, &fills));
        let base = Base::new(board, rules, conn, fills);
        let nets = scope_nets(&base.board, &base.conn, &RouteScope::Nets(vec![signal]));
        let world = Arc::new(RBase::build(&base.board));
        problem(&base, world, &nets, Style::FortyFive, Effort::default())
    };
    let p = setup(board.clone());
    assert_eq!(p.stack.layers, vec![LayerId::F_CU], "fixture: only F.Cu is routable");
    assert_eq!(p.conn.unrouted, vec![signal], "fixture: GND and the fence start connected; only S is open");

    // Removing only the zone leaves the routing world, pitch and S's terminals unchanged.
    // GND is then already open, so the transaction exposes S's first route without repairs.
    // At y=5.25 its clearance leaves just 0.025 mm above X's pour clearance; the zone's
    // 0.20 mm minimum width removes that sliver and seals the mouth. A detour has ample room.
    let mut bare = board.clone();
    bare.zones.clear();
    bare.reindex();
    let first = GridRouter::default().route(&setup(bare), &Progress::new());
    assert!(first.open.is_empty(), "fixture: the unprotected signal route must complete: {:?}", first.open);
    let mut cut = board;
    assert!(cut.apply_all(&first.ops()).is_ok(), "fixture: the first signal route applies to the poured board");
    let cut_fills = fill_all(&cut, &p.rules);
    let cut_conn = connect(&cut, &cut_fills);
    assert!(cut_conn.unrouted.contains(&gnd), "fixture: S's first route must cut P off the GND pour");
    assert!(!cut_conn.unrouted.contains(&signal), "fixture: the neck-crossing first route must connect S");

    for guard in [0, Costs::default().guard] {
        let mut router = GridRouter { costs: Costs { guard, ..Costs::default() } };
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let output = statuses.clone();
        let progress = Progress::new().with_status(move |status| output.lock().unwrap().push(status));
        let sol = router.route(&p, &progress);
        let statuses = std::mem::take(&mut *statuses.lock().unwrap());
        let statuses: Vec<_> = statuses.iter()
            .map(|s| (s.current, s.attempt, s.retry, s.blocked_by)).collect();
        let work = sol.metrics.work;
        assert!(sol.open.is_empty(),
            "guard={guard}: preserves the neck with no open nets: {:?}; statuses={statuses:?}, work={work:?}", sol.open);
        if guard == 0 {
            assert_eq!(
                statuses,
                vec![(Some(signal), 0, None, None), (Some(signal), 1, Some(RetryWhy::PourNeck), None)],
                "guard off: S's first route, then its retry around the pour neck, and no block; work={work:?}"
            );
            assert_eq!(work.retries, 1,
                "guard off: the cut neck requires exactly one retry; statuses={statuses:?}, work={work:?}");
            assert!(work.discarded > 0,
                "guard off: the rolled-back attempt records discarded expansions; statuses={statuses:?}, work={work:?}");
        } else {
            assert_eq!(statuses, vec![(Some(signal), 0, None, None)],
                "guard on: the first signal route preserves the neck without a retry or block; work={work:?}");
            assert_eq!(work.retries, 0,
                "guard on: steering avoids every retry; statuses={statuses:?}, work={work:?}");
            assert_eq!(work.discarded, 0,
                "guard on: no attempted work is rolled back; statuses={statuses:?}, work={work:?}");
        }
        let mut after = (*p.board).clone();
        assert!(after.apply_all(&sol.ops()).is_ok(),
            "guard={guard}: the protected route's operations must apply");
        let fills = fill_all(&after, &p.rules);
        let conn = connect(&after, &fills);
        for (net, name) in [(gnd, "GND (including P)"), (signal, "S"), (fence, "fence")] {
            assert_eq!(
                conn.clusters.iter().filter(|cluster| cluster.nets.contains(&net)).count(), 1,
                "guard={guard}: {name} must have exactly one cluster after refilling"
            );
        }
        assert!(conn.unrouted.is_empty(),
            "guard={guard}: refilling must leave no unrouted nets: {:?}", conn.unrouted);
        let added_ids: BTreeSet<_> = sol.add.iter().map(Item::id).collect();
        let mut added_sels: BTreeSet<_> = added_ids.iter().map(|&id| Sel::Item(p.board.unit, id)).collect();
        added_sels.extend(conn.clusters.iter().flat_map(|cluster| cluster.items.iter())
            .filter(|item| added_ids.contains(&item.item())).map(|&item| sel_of(&after, item)));
        let result = drc(&after, &conn, &p.rules, &fills, None);
        let offending: Vec<_> = result.violations.iter().filter(|v| {
            matches!(v.kind, CheckKind::Clearance | CheckKind::Short)
                && v.items.iter().any(|sel| added_sels.contains(sel))
        }).collect();
        assert!(offending.is_empty(),
            "guard={guard}: added copper must have no clearance or short findings: {offending:#?}");
        let repeated = router.route(&p, &Progress::new());
        assert_eq!(
            sol, repeated,
            "guard={guard}: routing the pour neck twice must give the same solution"
        );
    }
}

/// Why: a stopped Route all commits what it routed, and Route selected then routes one net on
/// that copper. On ledctl, with the first seven nets of Route all committed (mcu.NRST's run past
/// U2 among them), a job routing 3V3 alone found no access to U2's pad 4: its grid took the pitch
/// of 3V3's wide class alone, too coarse beside NRST's copper. Route all had routed 3V3 last on
/// that same copper, without a rip, on the grid its signal nets set.
#[test]
fn a_wide_net_routes_on_committed_copper_as_among_the_job_s_own() {
    let design = ledctl();
    let board: Board = (*design.units[0].board.clone().expect("fixture setup: ledctl unit 0 has a board")).clone();
    let base_of = |board: Board| {
        let board = Arc::new(board);
        let rules = Arc::new(resolve_rules(&board, None));
        let fills = Arc::new(fill_all(&board, &rules));
        let conn = Arc::new(connect(&board, &fills));
        Base::new(board, rules, conn, fills)
    };
    let route = |base: &Base, scope: &RouteScope, progress: &Progress| {
        let nets = scope_nets(&base.board, &base.conn, scope);
        let world = Arc::new(RBase::build(&base.board));
        GridRouter::default().route(&problem(base, world, &nets, Style::default(), Effort::default()), progress)
    };
    let net = |name: &str| board.net(name).unwrap_or_else(|| panic!("fixture: ledctl has net {name}"));
    let (v3, nrst) = (net("3V3"), net("mcu.NRST"));

    // Stop Route all after its seventh net and commit what it routed.
    let stopped = Arc::new(Mutex::new(None));
    let keep = stopped.clone();
    route(&base_of(board.clone()), &RouteScope::All,
        &Progress::at_steps(vec![7], move |_, partial| *keep.lock().unwrap() = Some(partial)));
    let partial = stopped.lock().unwrap().take().expect("fixture: Route all offers its seventh net's partial");
    let mut committed = board.clone();
    committed.apply_all(&partial.ops()).expect("fixture: the stopped job's partial commits");
    let committed = base_of(committed);
    assert!(!committed.conn.unrouted.contains(&nrst) && committed.conn.unrouted.contains(&v3),
        "fixture: the committed nets hold mcu.NRST and not 3V3; unrouted={:?}", committed.conn.unrouted);

    // Route selected on 3V3.
    let sol = route(&committed, &RouteScope::Nets(vec![v3]), &Progress::new());
    let mut after = (*committed.board).clone();
    after.apply_all(&sol.ops()).expect("the 3V3 route applies");
    let after = base_of(after);
    assert!(sol.open.is_empty() && !after.conn.unrouted.contains(&v3),
        "3V3 joins every terminal on the committed copper: open={:?}, unrouted={:?}", sol.open, after.conn.unrouted);
}

/// The handheld's hand-routed stubs end where the router is meant to carry on. On the product's
/// path (its solver copper removed by ops, Route all, save, reload) every net connects, every
/// free end of fixed copper is continued, DRC is clean live and after the reload, and the fixed
/// copper is as it was.
#[test]
fn handheld_hand_routed_ends_are_continued() {
    use fab_elec_board::{dangling_ends, BoardOp, Origin};
    use fab_elec_lang::{evaluate, std_libs, write, Sources};

    let solved = |o: &Origin| matches!(o, Origin::Solved { .. });
    let base_of = |board: Arc<Board>| {
        let rules = Arc::new(resolve_rules(&board, None));
        let fills = Arc::new(fill_all(&board, &rules));
        let conn = Arc::new(connect(&board, &fills));
        Base::new(board, rules, conn, fills)
    };
    let findings = |board: &Base| drc(&board.board, &board.conn, &board.rules, &board.fills, None).violations;
    let fixed_ends = |board: &Board| board.tracks.iter().filter(|t| !solved(&t.origin))
        .map(|t| dangling_ends(board, t).into_iter().filter(|d| *d).count()).sum::<usize>();
    // What a route must leave as it was, by content (ids are reissued on a reload).
    let fixed_copper = |board: &Board| {
        let mut out: Vec<String> = board.tracks.iter().filter(|t| !solved(&t.origin))
            .map(|t| format!("{} {:?} {} {:?}", board.nets[t.net.ix()].name, t.layer, t.w, t.path))
            .chain(board.vias.iter().filter(|v| !solved(&v.origin))
                .map(|v| format!("{} {:?} {} {}", board.nets[v.net.ix()].name, v.at, v.drill, v.pad)))
            .collect();
        out.sort();
        out
    };

    let sources = fab_elec_fixtures::docs("handheld").expect("fixture setup: the handheld project");
    let evaluated = evaluate(&sources, &std_libs()).expect("fixture setup: the handheld evaluates");
    let unit = evaluated.design.units.iter().find(|u| u.board.is_some()).expect("fixture setup: a board unit");
    let board = unit.board.clone().expect("fixture setup: the handheld board");
    let pcb = sources.all_docs().iter().find(|d| d.present && d.name.ends_with(".pcb.splash"))
        .expect("fixture setup: a board document").name.clone();
    let unroute: Vec<BoardOp> = board.tracks.iter().filter(|t| solved(&t.origin)).map(|t| BoardOp::Remove(t.id))
        .chain(board.vias.iter().filter(|v| solved(&v.origin)).map(|v| BoardOp::Remove(v.id)))
        .collect();
    let mut bare = (*board).clone();
    bare.apply_all(&unroute).expect("fixture setup: the unroute applies");
    assert_eq!(fixed_ends(&bare), 5, "setup: the unroute leaves the hand-routed stubs' five free ends");
    let base = base_of(Arc::new(bare));
    let nets = scope_nets(&base.board, &base.conn, &RouteScope::All);
    let p = problem(&base, Arc::new(RBase::build(&base.board)), &nets, Style::default(), Effort::default());
    let sol = GridRouter::default().route(&p, &Progress::new());
    assert!(sol.open.is_empty(), "Route all connects every net: open {:?}", sol.open);
    let mut routed = (*base.board).clone();
    routed.apply_all(&sol.ops()).expect("the solution applies");
    assert_eq!(fixed_ends(&routed), 0, "every free end of fixed copper is continued");
    let live = findings(&base_of(Arc::new(routed.clone())));
    assert!(live.is_empty(), "live DRC is clean: {live:?}");

    let text = write(&routed, &unit.circuit);
    let mut saved = Sources::new();
    for d in sources.all_docs() {
        let body: Arc<str> = if d.name == pcb { Arc::from(text.as_str()) } else { d.text.clone() };
        let id = saved.add(d.name.clone(), body).expect("the project's documents add again");
        saved.set_present(id, d.present).expect("presence carries over");
    }
    for asset in sources.assets() {
        saved.add_asset(asset.clone());
    }
    saved.set_library(sources.library().to_vec());
    let reread = evaluate(&saved, &std_libs()).expect("the saved project evaluates");
    let reloaded = reread.design.units.iter().find(|u| u.name == unit.name).and_then(|u| u.board.clone())
        .expect("the saved project has the board");
    let after = findings(&base_of(reloaded.clone()));
    assert!(after.is_empty(), "DRC is clean after a save and reload: {after:?}");
    assert_eq!(fixed_copper(&*reloaded), fixed_copper(&*board), "the fixed copper is as it was");
}

/// P2's gate: the constructive seed's placement routes. With every part of ledctl placed afresh
/// by `place::seed` and the hand copper taken off (the pours stay), the grid router routes every
/// net and its copper is DRC-clean. (ledavr and the handheld are measured runs: their routes take
/// longer than a test may.)
#[test]
fn seeded_ledctl_routes_completely() {
    use fab_elec_route::place::{seed, PlaceProblem, SeedOpts};
    let design = ledctl();
    let unit = design.units.iter().find(|u| u.board.as_ref().is_some_and(|b| !b.placed.is_empty()))
        .expect("fixture setup: a placed board");
    let mut board = (**unit.board.as_ref().expect("a board")).clone();
    let placement = PlaceProblem::new(&board, &[]);
    let mut poses: Vec<_> = placement.movables.iter().map(|m| m.start).collect();
    let report = seed(&board, &unit.circuit, &placement, &mut poses, &SeedOpts::default());
    assert!(report.unplaced.is_empty(), "setup: the seed places every part: {:?}", report.unplaced);
    for (m, pose) in placement.movables.iter().zip(&poses) {
        let p = board.placed.iter_mut().find(|p| p.id == m.item).expect("a movable is on the board");
        (p.at, p.rot, p.side) = (pose.at, pose.rot, pose.side);
    }
    board.tracks.clear();
    board.vias.clear();
    board.reindex();

    let board = Arc::new(board);
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let base = Base::new(board, rules.clone(), conn, fills);
    let nets = scope_nets(&base.board, &base.conn, &RouteScope::All);
    let world = Arc::new(RBase::build(&base.board));
    let p = problem(&base, world, &nets, Style::FortyFive, Effort::default());
    let sol = GridRouter::default().route(&p, &Progress::new());
    let open: Vec<_> = sol.open.iter().map(|(net, why)| format!("{} {why:?}", base.board.nets[net.ix()].name)).collect();
    assert!(open.is_empty(), "every net of the seeded board routes: {} of {} do; open: {open:?}", p.nets.len() - open.len(), p.nets.len());

    let mut routed = (*base.board).clone();
    routed.apply_all(&sol.ops()).unwrap_or_else(|e| panic!("the routes apply: {e:?}"));
    let fills = fill_all(&routed, &rules);
    let conn = connect(&routed, &fills);
    let found: Vec<_> = drc(&routed, &conn, &rules, &fills, None).violations.iter()
        .filter(|v| matches!(v.kind, CheckKind::Clearance | CheckKind::EdgeClearance | CheckKind::HoleClearance
            | CheckKind::Short | CheckKind::Dangling | CheckKind::DanglingVia | CheckKind::ViaInPad | CheckKind::CourtyardOverlap))
        .map(|v| format!("{:?} {}", v.kind, v.message)).collect();
    assert!(found.is_empty(), "the seeded board's copper is DRC-clean: {found:?}");
}
