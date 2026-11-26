use fab_elec_design::engines::{self, BoardResults, FillTask};
use fab_elec_fixtures::ledctl;
use fab_elec_design::ops::DesignApplied;
use fab_elec_design::{DesignOp, SysOp};
use fab_elec_board::ops::{BoardOp, Field, Item};
use fab_elec_board::{resolve_rules, zone, BoardDelta, Capabilities, Factories, Flash, Origin, Tent, Via, ViaKind};
use fab_elec_circuit::CheckKind;
use fab_elec_core::{mm, BoardId, LayerId, Sel, P};
use std::sync::Arc;

#[test]
fn held_fills_converge_to_full() {
    let mut d = ledctl();
    let mut res = engines::full(&d);
    let factories = Factories::new();
    assert_eq!(
        engines::update_held(&res, &d, &[(BoardId(0), BoardDelta::default())], &factories),
        res,
        "an empty delta at the same generation leaves exact results unchanged",
    );

    let resistor = d.units[0].circuit.comp("lamps[0].r").expect("the fixture has lamps[0].r").id;
    let b = Arc::make_mut(d.units[0].board.as_mut().expect("the fixture has a board"));
    let r4 = b.placed_of(resistor).expect("the fixture's resistor is placed").id;
    let led_track = b.tracks.iter().find(|t| t.ends[0].item() == Some(r4)).expect("the fixture has a track from the resistor").id;
    let gnd = b.net("GND").expect("the fixture has GND");
    let gnd_zone = b.zones.iter().find(|z| z.net == Some(gnd)).expect("the fixture has a GND zone").id;
    let zone_ids: Vec<_> = b.zones.iter().map(|z| z.id).collect();
    let via_id = b.alloc_id();
    let mut rules_wide = b.rules.clone();
    rules_wide.base.clearance = Some(mm(0.3));
    let ops = [
        ("move lamps[0].r", BoardOp::Move { item: r4, by: P::new(mm(0.3), mm(-0.2)) }),
        ("widen the LED track", BoardOp::SetField { item: led_track, field: Field::Width(mm(0.3)) }),
        ("add a GND via", BoardOp::Add(Item::Via(Via {
            id: via_id,
            net: gnd,
            at: P::new(mm(40.0), mm(16.0)),
            drill: mm(0.3),
            pad: mm(0.6),
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through,
            tent: Tent::Both,
            backdrill: None,
            locked: false,
            origin: Origin::Authored,
            flash: Flash::All,
        }))),
        ("remove the LED track", BoardOp::Remove(led_track)),
        ("remove the GND zone", BoardOp::Remove(gnd_zone)),
        ("set wider rules", BoardOp::SetRules(rules_wide)),
    ];
    // Connectivity findings (shorts, unconnected items, dangling vias) follow the held fills until the refill.
    let copper_findings = |r: &BoardResults| {
        r.drc.iter().filter(|v| {
            !matches!(v.kind, CheckKind::Short | CheckKind::Unconnected | CheckKind::DanglingVia)
                && !v.items.iter().any(|sel| matches!(sel, Sel::Item(unit, zone) if *unit == BoardId(0) && zone_ids.contains(zone)))
        }).cloned().collect::<Vec<_>>()
    };
    for (step, (name, op)) in ops.iter().enumerate() {
        let applied = d.apply(&DesignOp::Board { unit: BoardId(0), op: op.clone() }, false).unwrap_or_else(|e| panic!("{name} applies: {e:?}"));
        let DesignApplied::Board { applied, .. } = applied else { panic!("{name} returns a board delta") };
        let mut held = engines::update_held(&res, &d, &[(BoardId(0), applied.delta)], &factories);
        // One full result per edit serves both the pass-A subset and the final equality check.
        let expected = engines::full(&d);
        assert_eq!(copper_findings(&held.boards[0]), copper_findings(&expected.boards[0]), "pass A's copper-to-copper findings equal full's after {name}");
        let b = d.unit(BoardId(0)).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board");
        assert!(held.boards[0].drc.iter().all(|v| v.items.iter().all(|sel| {
            !matches!(sel, Sel::Item(unit, zone) if *unit == BoardId(0) && zone_ids.contains(zone) && !b.zones.iter().any(|z| z.id == *zone))
        })), "pass A names no zone removed from the board after {name}");
        let tasks = engines::owed(&held);
        let pending_tasks: Vec<_> = held.boards[0].pending.zones.iter().map(|(zone, _)| FillTask {
            unit: BoardId(0), zone: *zone, generation: held.boards[0].generation,
        }).collect();
        assert_eq!(tasks, pending_tasks, "owed lists exactly unit 0's pending zones in order at the results' generation after {name}");
        if step == 0 {
            assert!(!tasks.is_empty(), "the first move leaves a fill pending for the race check");
        }
        for (task_index, task) in tasks.into_iter().enumerate() {
            let i = held.boards.iter().position(|b| b.unit == task.unit).expect("an owed fill's unit has results");
            let board = d.unit(task.unit).expect("an owed fill's unit exists").board.as_ref().expect("an owed fill's unit has a board");
            let fill = zone::fill(board, &held.boards[i].rules, task.zone);
            if step == 0 && task_index == 0 {
                // Advance a shared snapshot before accepting this fill, while the main sequence
                // still proves convergence independently after each of the six edits.
                let mut raced = d.clone();
                let applied = raced.apply(&DesignOp::Board { unit: BoardId(0), op: ops[1].1.clone() }, false).expect("the next edit applies before the first fill is merged");
                let DesignApplied::Board { applied, .. } = applied else { panic!("the next edit returns a board delta") };
                let delta = applied.delta;
                let reached = zone::reaches(raced.units[0].board.as_ref().expect("the raced unit keeps its board"), &held.boards[i].rules, task.zone, &delta);
                let newer = engines::update_held(&held, &raced, &[(BoardId(0), delta)], &factories);
                assert!(reached, "the next edit reaches the first pending zone, exercising the overtaken-fill race");
                assert!(
                    engines::merge_fill(
                        &newer, &raced, task.unit, task.zone, task.generation, &held.boards[i].rules, fill.clone(),
                    ).is_none(),
                    "a fill overtaken by a delta that reaches its zone is dropped",
                );
            }
            let rules = held.boards[i].rules.clone();
            held = engines::merge_fill(&held, &d, task.unit, task.zone, task.generation, &rules, fill)
                .unwrap_or_else(|| panic!("an owed fill with current inputs is accepted after {name}"));
        }
        assert_eq!(held, expected, "pass A then pass B equals full after {name}");
        res = held;
    }

    // A factory change moves no board generation: a fill made with the rules before it is dropped.
    // (The edits above removed the fixture's one zone, so this starts again from the fixture.)
    let mut d = ledctl();
    let res = engines::full(&d);
    let zone = gnd_zone;
    let old_rules = res.boards[0].rules.clone();
    let at = res.boards[0].generation;
    let old_fill = zone::fill(d.units[0].board.as_ref().unwrap(), &old_rules, zone);
    let caps = Capabilities { min_track: 0, min_clearance: mm(0.4), min_drill: 0, min_annular: 0, via_in_pad: false, layers: vec![2], board_min: (0, 0), board_max: (0, 0), thickness: Vec::new(), thickness_tol_ppm: 0 };
    let factories: Factories = [("fab".to_string(), Arc::new(caps))].into_iter().collect();
    let applied = d.apply(&DesignOp::Sys(SysOp::SetFactory { unit: BoardId(0), profile: Some("fab".into()) }), false).expect("the factory is set");
    let deltas = engines::deltas_of(&d, &applied);
    let held = engines::update_held(&res, &d, &deltas, &factories);
    assert_eq!(held.boards[0].generation, at, "a factory change moves no board generation");
    assert!(held.boards[0].pending.zones.iter().any(|(z, _)| *z == zone), "a factory change makes every zone pending");
    assert!(engines::merge_fill(&held, &d, BoardId(0), zone, at, &old_rules, old_fill).is_none(), "a fill made with the rules before a factory change is dropped");
    let mut held = held;
    for task in engines::owed(&held) {
        let rules = held.boards[0].rules.clone();
        let fill = zone::fill(d.units[0].board.as_ref().unwrap(), &rules, task.zone);
        held = engines::merge_fill(&held, &d, task.unit, task.zone, task.generation, &rules, fill)
            .expect("a fill with the factory's rules is accepted");
    }
    assert_eq!(held, engines::full_with(&d, &factories), "pass A then pass B equals full after a factory change");

    // The load path, board first, on the fixture with a second zone: the copper shows at once
    // with every zone owed against a whole-board record; merges only swap fills in; an edit
    // mid-load joins the record; after the last fill the whole-board checks are owed, a pass
    // overtaken by an edit is dropped, and the merged pass equals full.
    let factories = Factories::new();
    let mut d = ledctl();
    let b = Arc::make_mut(d.units[0].board.as_mut().expect("the fixture has a board"));
    let mut second = b.zones.iter().find(|z| z.id == gnd_zone).expect("the fixture has its GND zone").clone();
    second.id = b.alloc_id();
    second.name = "GND again".into();
    second.priority += 1;
    let zones = [gnd_zone, second.id];
    d.apply(&DesignOp::Board { unit: BoardId(0), op: BoardOp::Add(Item::Zone(second)) }, false).expect("a second zone adds");
    let loaded = engines::load_held(&d, &factories);
    let first_board = &loaded.boards[0];
    assert!(first_board.pending.whole && first_board.pending.debt.global, "a load owes its whole board");
    assert!(engines::violations(&loaded).iter().all(|v| first_board.drc.iter().all(|d| d.id != v.id)),
        "a board that owes its whole-board checks lists none of its DRC findings");
    assert_eq!(first_board.pending.zones.iter().map(|(z, _)| *z).collect::<Vec<_>>(), zones, "a load owes every zone");
    let off_zones = |r: &BoardResults| {
        r.drc.iter().filter(|v| {
            !matches!(v.kind, CheckKind::Short | CheckKind::Unconnected | CheckKind::DanglingVia)
                && !v.items.iter().any(|sel| matches!(sel, Sel::Item(unit, zone) if *unit == BoardId(0) && zones.contains(zone)))
        }).cloned().collect::<Vec<_>>()
    };
    assert_eq!(off_zones(first_board), off_zones(&engines::full(&d).boards[0]), "the first board's copper-to-copper findings equal full's");
    let fill_owed = |held: &engines::DesignResults, d: &fab_elec_design::Design, task: &FillTask| {
        let rules = held.boards[0].rules.clone();
        let fill = zone::fill(d.unit(task.unit).unwrap().board.as_ref().unwrap(), &rules, task.zone);
        engines::merge_fill(held, d, task.unit, task.zone, task.generation, &rules, fill).expect("an owed fill with current inputs is accepted")
    };
    let tasks = engines::owed(&loaded);
    let mut held = fill_owed(&loaded, &d, &tasks[0]);
    let same_checks = |held: &engines::DesignResults| {
        Arc::ptr_eq(&held.boards[0].conn, &first_board.conn) && Arc::ptr_eq(&held.boards[0].drc, &first_board.drc)
    };
    assert!(same_checks(&held) && held.boards[0].pending.whole, "a merge runs no connectivity or DRC under a whole-board record");
    let applied = d.apply(&DesignOp::Board { unit: BoardId(0), op: ops[0].1.clone() }, false).expect("an edit applies mid-load");
    let DesignApplied::Board { applied, .. } = applied else { panic!("the edit returns a board delta") };
    held = engines::update_held(&held, &d, &[(BoardId(0), applied.delta)], &factories);
    assert!(same_checks(&held) && held.boards[0].pending.whole, "an edit mid-load joins the whole-board record");
    let fill_all_owed = |mut held: engines::DesignResults, d: &fab_elec_design::Design| {
        for task in engines::owed(&held) {
            held = fill_owed(&held, d, &task);
        }
        held
    };
    held = fill_all_owed(held, &d);
    assert!(same_checks(&held), "the last fill's merge runs no connectivity or DRC either");
    let run_checks = |held: &engines::DesignResults, d: &fab_elec_design::Design| {
        let tasks = engines::owed_checks(held);
        assert_eq!(tasks.len(), 1, "once no fill is owed, the unit owes its whole-board checks");
        let r = &held.boards[0];
        let (conn, drc) = engines::whole_board_checks(d.unit(tasks[0].unit).unwrap().board.as_ref().unwrap(), &r.rules, &r.fills);
        (tasks[0].clone(), r.rules.clone(), r.fills.clone(), conn, drc)
    };
    let (task, rules, fills, conn, drc) = run_checks(&held, &d);
    let (base, base_board) = (held.boards[0].clone(), d.units[0].board.clone().expect("the fixture has a board"));
    // Edits while the checks run (a track widened, then removed, and a zone removed): the pass
    // made before them is overtaken and does not merge as it is.
    let edit = |d: &mut fab_elec_design::Design, held: &engines::DesignResults, op: &BoardOp| {
        let applied = d.apply(&DesignOp::Board { unit: BoardId(0), op: op.clone() }, false).expect("an edit applies while the checks run");
        let DesignApplied::Board { applied, .. } = applied else { panic!("the edit returns a board delta") };
        engines::update_held(held, d, &[(BoardId(0), applied.delta)], &factories)
    };
    for (_, op) in [&ops[1], &ops[3], &ops[4]] {
        held = edit(&mut d, &held, op);
    }
    assert!(held.boards[0].pending.whole, "edits while the checks run join the whole-board record");
    assert!(engines::merge_checks(&held, task.unit, task.generation, &rules, &fills, conn.clone(), drc.clone()).is_none(),
        "checks overtaken by an edit do not merge as they are");
    // They merge under the edits since, before the edits' refills merge (those stay owed) or after
    // (the refills join at once); either way no second pass runs and the end equals full.
    let early = engines::rebase_checks(&held, &d, &base, &base_board, conn.clone(), drc.clone(), &factories).expect("overtaken checks rebase");
    assert!(!early.boards[0].pending.whole && engines::owed_checks(&early).is_empty(), "a rebased unit owes no whole-board checks");
    assert_eq!(fill_all_owed(early, &d), engines::full(&d), "checks rebased before the edits' refills, then the refills: full");
    held = fill_all_owed(held, &d);
    let late = engines::rebase_checks(&held, &d, &base, &base_board, conn.clone(), drc.clone(), &factories).expect("overtaken checks rebase after the refills");
    assert!(late.boards[0].pending.zones.is_empty(), "the refills the results hold join the rebased results at once");
    assert_eq!(late, engines::full(&d), "checks rebased after the edits' refills: full");
    // A rules change while they run re-resolves the rules: they run again.
    held = edit(&mut d, &held, &ops[5].1);
    assert!(engines::rebase_checks(&held, &d, &base, &base_board, conn, drc, &factories).is_none(), "checks overtaken by a rules change do not rebase");
    held = fill_all_owed(held, &d);
    let (task, rules, fills, conn, drc) = run_checks(&held, &d);
    held = engines::merge_checks(&held, task.unit, task.generation, &rules, &fills, conn, drc).expect("current checks merge");
    assert_eq!(held, engines::full(&d), "the load path with edits during the fills and the checks equals full");
}

#[test]
fn synchronous_updates_match_full_across_edits() {
    let mut d = ledctl();
    let mut results = engines::full(&d);
    let board = d.units[0].board.as_ref().expect("the fixture has a board");
    let edits = ["usb.cc[1]", "lamps[7].r"].map(|path| {
        let item = board.placed.iter().find(|p| p.path == path).expect("the fixture part is placed").id;
        (path, BoardOp::Move { item, by: P::new(mm(0.3), 0) })
    });
    for (name, op) in edits {
        let applied = d.apply(&DesignOp::Board { unit: BoardId(0), op }, false)
            .unwrap_or_else(|err| panic!("{name}: {err:?}"));
        let DesignApplied::Board { applied, .. } = applied else { panic!("the move returns a board delta") };
        assert!(!applied.delta.global, "{name}: the move exercises the synchronous incremental path");
        results = engines::update(&results, &d, &[(BoardId(0), applied.delta)]);
        assert_eq!(results, engines::full(&d), "{name}: the synchronous update equals a full recompute");
    }
}

#[test]
fn a_refill_covers_every_folded_edit() {
    let d = ledctl();
    let mut board = d.units[0].board.as_deref().expect("the fixture has a board").clone();
    let gnd = board.net("GND").expect("the fixture has GND");
    let gnd_zone = board.zones.iter().find(|z| z.net == Some(gnd))
        .expect("the fixture has a GND zone").id;
    let [left, right] = ["usb.cc[1]", "lamps[7].r"].map(|path| {
        board.placed.iter().find(|p| p.path == path).expect("the fixture part is placed").id
    });
    let rules = resolve_rules(&board, None);
    let before = zone::fill(&board, &rules, gnd_zone);
    let first = board.apply(&BoardOp::Move { item: left, by: P::new(mm(0.3), 0) })
        .expect("the left part moves").delta;
    let second = board.apply(&BoardOp::Move { item: right, by: P::new(mm(0.3), 0) })
        .expect("the right part moves").delta;
    let mut folded = BoardDelta::default();
    engines::merge_delta(&mut folded, &first);
    engines::merge_delta(&mut folded, &second);

    let expected = zone::fill(&board, &rules, gnd_zone);
    let updated = zone::fill_update(&board, &rules, &before, &folded);
    assert_eq!(updated, expected, "the folded delta refills every edit and equals a full fill");
    let last_only = zone::fill_update(&board, &rules, &before, &second);
    assert_ne!(last_only, before, "the second edit changes the GND fill");
    assert_ne!(last_only, expected, "the second delta alone misses the first edit's GND fill change");
}

/// A short through a stale island awaits its refill (EROUTE): pass A keeps the zone's old fill and
/// its joins, so a GND via joined to an island that is given another net shorts the two nets
/// through that island until pass B fills around the via; such a short reads as pending, every
/// other finding does not, and none is left after the refill.
#[test]
fn a_short_through_a_stale_island_awaits_its_refill() {
    let mut d = ledctl();
    let factories = Factories::new();
    let res = engines::full(&d);
    let unit = BoardId(0);
    let b = d.units[0].board.as_ref().expect("the fixture has a board");
    let gnd = b.net("GND").expect("the fixture has GND");
    let zone = b.zones.iter().find(|z| z.net == Some(gnd)).expect("the fixture has a GND zone").id;
    let id = b.vias.iter().find(|v| v.net == gnd).expect("the fixture has a GND via").id;
    let net = b.net("lamps[0].d.A").expect("the fixture has a lamp anode net");
    let applied = d.apply(&DesignOp::Board { unit, op: BoardOp::SetField { item: id, field: Field::Net(Some(net)) } }, false).expect("the via takes another net");
    let DesignApplied::Board { applied, .. } = applied else { panic!("the net change returns a board delta") };
    let held = engines::update_held(&res, &d, &[(unit, applied.delta)], &factories);
    let r = &held.boards[0];
    assert!(r.pending.zones.iter().any(|(z, _)| *z == zone), "a net change on a via in the zone leaves its fill pending");
    let through = |v: &fab_elec_circuit::Violation| v.items.contains(&Sel::Item(unit, id)) && v.items.contains(&Sel::Item(unit, zone));
    let short = r.drc.iter().find(|v| v.kind == CheckKind::Short && through(v)).expect("pass A finds the via shorting through the held island");
    assert!(engines::awaits_refill(r, short), "a short through the pending zone's island awaits the refill");
    assert!(r.drc.iter().filter(|v| !v.items.contains(&Sel::Item(unit, zone))).all(|v| !engines::awaits_refill(r, v)), "a finding not through a pending zone does not await it");

    let mut merged = held.clone();
    for task in engines::owed(&held) {
        let board = d.unit(task.unit).unwrap().board.as_ref().unwrap();
        let fill = zone::fill(board, &merged.boards[0].rules, task.zone);
        let rules = merged.boards[0].rules.clone();
        merged = engines::merge_fill(&merged, &d, task.unit, task.zone, task.generation, &rules, fill)
            .expect("the current fill merges");
    }
    let r = &merged.boards[0];
    assert!(r.pending.zones.is_empty(), "the refill leaves nothing pending");
    assert!(!r.drc.iter().any(|v| v.kind == CheckKind::Short && through(v)), "the refilled zone clears around the via: no short is left");
    assert!(r.drc.iter().all(|v| !engines::awaits_refill(r, v)), "with exact fills nothing awaits a refill");
}

/// A part dragged over a pour shows no short until the refill (EBOARD's drag): the moved pads sit on
/// the pour's held fill, where plain geometry over those fills shorts their nets with the pour's. Pass
/// A joins no copper through a stale fill (gate 3), so the held results report no short through the
/// pour; findings that name the pending pour await the refill; after it no short runs through a pour
/// and nothing awaits.
#[test]
fn a_part_dragged_over_a_pour_shows_no_short_until_its_refill() {
    let mut d = ledctl();
    let factories = Factories::new();
    let res = engines::full(&d);
    let unit = BoardId(0);
    let b = d.units[0].board.as_ref().expect("the fixture has a board");
    let gnd = b.net("GND").expect("the fixture has GND");
    let pour = b.zones.iter().find(|z| z.net == Some(gnd)).expect("the fixture has a GND pour").id;
    // The fixture's first two-pad part with neither pad on GND (lamps[0].d).
    let part = b.placed.iter().find(|p| p.footprint.pads.len() == 2 && p.pad_nets.len() == 2 && p.pad_nets.iter().all(|n| n.is_some() && *n != Some(gnd)))
        .expect("the fixture has a two-pad part off GND").id;
    let through_pour = |conn: &fab_elec_board::Connectivity| conn.shorts.iter()
        .filter(|s| conn.clusters[s.cluster as usize].items.iter().any(|i| matches!(i, fab_elec_board::ConnItem::Zone { .. })))
        .count();
    assert_eq!(through_pour(&res.boards[0].conn), 0, "the fixture starts with no short through a pour");
    let applied = d.apply(&DesignOp::Board { unit, op: BoardOp::Move { item: part, by: P::mm(3.0, 0.0) } }, false).expect("the part moves 3 mm");
    let DesignApplied::Board { applied, .. } = applied else { panic!("the move returns a board delta") };
    let held = engines::update_held(&res, &d, &[(unit, applied.delta)], &factories);
    let r = &held.boards[0];
    assert!(r.pending.zones.iter().any(|(z, _)| *z == pour), "the drag over the pour leaves its fill pending");
    let board = d.units[0].board.as_ref().unwrap();
    assert!(through_pour(&fab_elec_board::connect(board, &r.fills)) > 0, "the moved pads sit on the held fill: plain geometry over it shorts through the pour");
    assert_eq!(through_pour(&r.conn), 0, "the held results join nothing through the stale pour: no short until the refill");
    assert!(!r.drc.iter().any(|v| v.kind == CheckKind::Short && v.items.contains(&Sel::Item(unit, pour))), "no short names the pending pour");
    assert!(r.drc.iter().filter(|v| v.items.contains(&Sel::Item(unit, pour))).all(|v| engines::awaits_refill(r, v)), "every finding naming the pending pour awaits the refill");

    let mut merged = held.clone();
    for task in engines::owed(&held) {
        let board = d.unit(task.unit).unwrap().board.as_ref().unwrap();
        let rules = merged.boards[0].rules.clone();
        let fill = zone::fill(board, &rules, task.zone);
        merged = engines::merge_fill(&merged, &d, task.unit, task.zone, task.generation, &rules, fill).expect("the current fill merges");
    }
    let r = &merged.boards[0];
    assert_eq!(through_pour(&r.conn), 0, "the refill clears the pour around the moved pads: no short through it is left");
    assert!(r.drc.iter().all(|v| !engines::awaits_refill(r, v)), "with exact fills nothing awaits a refill");
}
