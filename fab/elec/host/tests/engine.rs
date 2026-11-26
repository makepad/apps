use fab_elec_board::{drc::content_id, BoardDelta, BoardOp, Factories, Item, Waiver};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{BoardId, Canon, Diag, FeatureRef, ItemId, Sel, Severity, P};
use fab_elec_design::engines::FillTask;
use fab_elec_design::{engines, BenchOp, By, Design, DesignOp, PartRef, Place, Resolved};
use fab_elec_fixtures::ledctl;
use fab_elec_host::{route_plan, CircuitChange, Cmd, Docs, Engine, FillJob, Mailbox, Outcome, Plan, Sample, TaskResult, Ticket};
use fab_elec_view::{CaseHeld, DesignSnapshot, StageFlags, StateOf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[test]
fn inbox_and_engine_step() {
    let mailbox = Mailbox::default();
    let sender = mailbox.clone();
    assert!(mailbox.is_empty(), "a new mailbox has no work");
    assert_eq!(mailbox.waited(Instant::now()), None, "an empty mailbox has no waiting command");
    sender.push(Cmd::Undo);
    assert!(mailbox.waited(Instant::now()).is_some(), "a pushed command has a waiting duration");
    sender.push(Cmd::Redo);
    sender.preview(Sample { tag: 0, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: None, seq: 1, ops: vec![], label: "older sample".into() });
    sender.preview(Sample { tag: 0, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: None, seq: 2, ops: vec![], label: "newer sample".into() });
    let inbox = mailbox.take();
    assert_eq!(inbox.cmds.len(), 2, "take retains every command");
    assert!(matches!(&inbox.cmds[0].1, Cmd::Undo), "commands retain their first-in order");
    assert!(matches!(&inbox.cmds[1].1, Cmd::Redo), "commands retain their second-in order");
    assert!(inbox.cmds[0].0 <= inbox.cmds[1].0, "command stamps follow queue order");
    assert_eq!(inbox.preview.as_ref().map(|s| s.seq), Some(2), "the second preview replaces the first slot");
    assert!(mailbox.is_empty(), "take empties the shared mailbox");
    assert_eq!(mailbox.waited(Instant::now()), None, "take clears the oldest-command wait");
    drop(inbox);

    let design = ledctl();
    let unit = BoardId(0);
    let via = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board").vias[0].clone();
    let move_by = |by| DesignOp::Board { unit, op: BoardOp::Move { item: via.id, by } };
    let hash = |d: &Design| d.unit(unit).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board").content_hash();
    let position = |d: &Design| {
        d.unit(unit).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board")
            .vias.iter().find(|v| v.id == via.id).expect("the moved via remains present").at
    };
    let before_move = hash(&design);
    let factories = Arc::new(Factories::new());
    let steps = Arc::new(AtomicU64::new(40));
    let logged = Arc::new(AtomicU64::new(40));
    let mut engine = Engine::new(design, factories.clone(), steps.clone(), logged.clone(), "fixture".into());
    let initial = engine.snapshot();
    assert!(initial.stale.is_empty(), "the load snapshot has full results");
    assert!(initial.preview.is_none(), "the load snapshot has no preview");
    assert_eq!(initial.head, "fixture", "snapshots retain the supplied document head");
    drop(initial);

    let first_move = P::mm(0.3, -0.2);
    mailbox.push(Cmd::Commit { by: By::Person, ticket: None, place: vec![], ops: vec![move_by(first_move)], label: "move via".into(), circuits: vec![], expect: Some((unit, before_move)) });
    let mut published = Vec::new();
    let first = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(first.rejected.is_empty(), "the matching board hash accepts the move");
    assert!(published.len() >= 2, "a commit publishes before and after pass A");
    let early = &published[0];
    assert_eq!(position(&early.design), via.at.add(first_move), "the first publish already contains the committed move");
    assert!(early.stale.has(StageFlags::CONNECT), "connectivity is stale in the first publish");
    assert!(early.stale.has(StageFlags::DRC), "DRC is stale in the first publish");
    let late = published.last().expect("pass A publishes a snapshot");
    assert!(!late.stale.has(StageFlags::CONNECT) && !late.stale.has(StageFlags::DRC), "pass A clears the immediate engine stages");
    let result = &engine.results().boards[0];
    let conn = late.of(&late.conn, unit).expect("the snapshot has connectivity for unit 0");
    let rules = late.of(&late.rules, unit).expect("the snapshot has rules for unit 0");
    let fills = late.of(&late.fills, unit).expect("the snapshot has fills for unit 0");
    assert!(Arc::ptr_eq(&conn.value, &result.conn), "the last publish shares pass A's connectivity");
    assert!(Arc::ptr_eq(&rules.value, &result.rules), "the last publish shares pass A's rules");
    assert!(Arc::ptr_eq(&fills.value, &result.fills), "the last publish shares pass A's held fills");
    assert_eq!(conn.of, StateOf::Committed, "pass A tags committed connectivity as committed");
    assert_eq!(conn.generation, result.generation, "pass A tags the board results' generation");
    let owed = engines::owed(engine.results());
    assert!(!owed.is_empty(), "moving the fixture via reaches a zone");
    assert!(late.stale.has(StageFlags::FILLS), "owed fills remain stale after pass A");
    assert_eq!(first.fills.iter().map(|job| &job.task).collect::<Vec<_>>(), owed.iter().collect::<Vec<_>>(), "the step starts every pending zone fill");
    assert_eq!(first.undo, Some(41), "the first commit takes the shared project counter's next step");
    assert_eq!(first.redo, None, "a new commit has no redo");
    published.clear();

    for job in &first.fills {
        mailbox.result(job.run());
    }
    assert!(!mailbox.is_empty(), "completed fills count as mailbox work");
    let filled = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(filled.rejected.is_empty(), "merging fills does not reject commands");
    assert!(filled.fills.is_empty(), "exact results start no further fills");
    assert_eq!(engine.results(), &engines::full_with(engine.design(), &factories), "pass A followed by every fill equals full results");
    assert!(engine.results().boards.iter().all(|b| b.pending.zones.is_empty()), "all completed zones lose their pending status");
    assert!(!published.last().expect("a merged fill publishes").stale.has(StageFlags::FILLS), "the merged publish clears FILLS");
    published.clear();
    drop(first);

    let after_move = engine.design().clone();
    assert_ne!(hash(&after_move), before_move, "moving the via changes its board's content hash");
    mailbox.push(Cmd::Commit { by: By::Person, ticket: None, place: vec![], ops: vec![move_by(first_move)], label: "wrong base".into(), circuits: vec![], expect: Some((unit, before_move)) });
    let rejected = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(rejected.rejected.len(), 1, "a wrong expected hash rejects exactly one command");
    assert_eq!(rejected.rejected[0].0, "wrong base", "the rejection identifies the command label");
    assert!(rejected.rejected[0].1.contains("hash mismatch"), "the rejection explains the hash mismatch");
    assert_eq!(engine.design(), &after_move, "a hash mismatch leaves the entire design unchanged");
    assert_eq!(engine.log().entries.len(), 1, "a rejected command makes no history entry");
    published.clear();

    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![move_by(first_move), DesignOp::Board { unit, op: BoardOp::Remove(ItemId(u32::MAX)) }],
        label: "failed batch".into(), circuits: vec![], expect: None,
    });
    let rejected = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(rejected.rejected.len(), 1, "an op failure rejects the whole command");
    assert_eq!(engine.design(), &after_move, "rollback restores content and generation counters after a partial command");
    assert!(rejected.fills.is_empty(), "rollback discards the failed command's deltas");
    assert_eq!(engine.log().entries.len(), 1, "a failed batch makes no history entry");
    published.clear();

    mailbox.preview(Sample { tag: 0, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: None, seq: 6, ops: vec![move_by(P::mm(-0.4, 0.0))], label: "older applied preview".into() });
    let previewed = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(previewed.rejected.is_empty(), "the older preview applies successfully");
    assert!(!previewed.fills.is_empty() && previewed.fills.iter().all(|job| job.preview == Some(6)), "preview fills start on the pool for the live sample");
    assert!(published.last().expect("preview pass publishes").preview.is_some(), "the older sample is live before the race");
    assert_eq!(engine.design(), &after_move, "a live preview leaves committed state unchanged");
    published.clear();

    let before_preview_commit = hash(engine.design());
    let before_preview_position = position(engine.design());
    let preview_move = P::mm(0.2, 0.1);
    let seq = steps.fetch_add(1, SeqCst) + 1;
    logged.fetch_max(seq, SeqCst); // A step from Fab's model host shares the same counter.
    mailbox.preview(Sample { tag: 0, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: None, seq: 7, ops: vec![move_by(preview_move)], label: "commit sample seven".into() });
    mailbox.push(Cmd::CommitPreview { seq: 7, tag: 0 });
    let committed = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(committed.rejected.is_empty(), "commit-preview accepts a sample still in the inbox slot");
    assert_eq!(position(engine.design()), before_preview_position.add(preview_move), "commit-preview uses seq 7 instead of the older applied sample");
    assert!(published.iter().all(|s| s.preview.is_none()), "committing the slot ends the applied preview at every publish");
    assert!(engine.snapshot().preview.is_none(), "no preview remains live after commit-preview");
    assert_eq!(engine.log().entries.len(), 2, "a preview commit makes one undo step");
    assert_eq!(engine.log().entries[1].label, "commit sample seven", "a preview commit retains the selected sample's label");
    assert_eq!(engine.log().entries[1].seq, engine.log().entries[0].seq + 2, "the shared counter leaves room for the model host's intervening step");
    assert_eq!(committed.undo, Some(43), "the undo candidate is the newest electronics step");
    assert_eq!(committed.redo, None, "the preview commit leaves no redo candidate");
    let after_preview_commit = hash(engine.design());
    published.clear();

    mailbox.push(Cmd::Undo);
    let undone = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(undone.rejected.is_empty(), "undo applies the stored inverse as host ops");
    assert_eq!(hash(engine.design()), before_preview_commit, "undo restores the board hash from before the preview commit");
    assert_eq!(undone.undo, Some(41), "undo exposes the preceding electronics step");
    assert_eq!(undone.redo, Some(43), "undo exposes the undone step for redo");
    published.clear();
    mailbox.push(Cmd::Redo);
    let redone = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(redone.rejected.is_empty(), "redo applies the stored ops as host ops");
    assert_eq!(hash(engine.design()), after_preview_commit, "redo restores the board hash after the preview commit");
    assert_eq!(redone.undo, Some(43), "redo restores the newest undo candidate");
    assert_eq!(redone.redo, None, "redo consumes the final redo candidate");
    assert_eq!(steps.load(SeqCst), 43, "undo and redo allocate no new project step numbers");
    published.clear();

    // A job from the preview commit was overtaken by undo and redo; it cannot clear their debt.
    for job in &committed.fills {
        mailbox.result(job.run());
    }
    let stale_fill = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(!engines::owed(engine.results()).is_empty(), "an overtaken fill leaves the newer generation pending");
    assert!(stale_fill.fills.is_empty(), "an old completion preserves the newer in-flight job");
    published.clear();
    for job in &redone.fills {
        mailbox.result(job.run());
    }
    let current_fill = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(engines::owed(engine.results()).is_empty(), "the current generation's fills clear the debt after redo");
    assert!(current_fill.fills.is_empty(), "accepting current fills does not reschedule them");
    published.clear();

    let retired = engine.snapshot();
    mailbox.retire(retired.clone());
    assert!(mailbox.is_empty(), "retired snapshots alone do not count as pending work");
    assert_eq!(Arc::strong_count(&retired), 2, "the mailbox owns the retired reference before step");
    engine.step(mailbox.take(), &mut |_| {
        assert_eq!(Arc::strong_count(&retired), 2, "retired snapshots remain alive until all step work finishes");
    });
    assert_eq!(Arc::strong_count(&retired), 1, "step drops the retired reference on the worker before returning");
}

#[test]
fn catalog_board_shapes_round_trip_through_ledctl_source() {
    use fab_elec_core::{BBox, LayerId, LayerSet, PolySet};
    use fab_elec_design::action::{find, Risk};
    use fab_elec_lang::board_doc;
    use fab_elec_parts::Forbid;
    fab_elec_host::edit_actions::register_actions();
    let sources = fab_elec_fixtures::docs("ledctl").unwrap();
    let (design, docs) = Docs::from_evaluated(sources, fab_elec_fixtures::ledctl_eval());
    let unit = design.units[0].id;
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::default(), Arc::default(), "shapes".into())
        .with_docs(docs).with_app_tools();
    let mail = Mailbox::default();
    let calls: &[(&str, &[(&str, &str)])] = &[
        ("elec.board.outline", &[("rect", "51 31 -1 -2")]),
        ("elec.keepout.add", &[("points", "10 1, 10 4, 12 2"), ("layers", "B.Cu"), ("forbid", "tracks, vias"), ("reason", "antenna")]),
        ("elec.cutout.add", &[("rect", "15 7 17 8"), ("name", "slot")]),
        ("elec.keepout.add", &[("rect", "2 3 4 5")]),
    ];
    for &(tool, fields) in calls {
        assert_eq!(find(tool).unwrap().risk, Risk::Edit);
        let mut args = vec![("unit".into(), "ledctl".into())];
        args.extend(fields.iter().map(|&(key, text)| (key.into(), text.into())));
        mail.push(Cmd::Invoke { tool: tool.into(), args, by: By::Person, preview: false, ticket: Ticket(1) });
        let step = engine.step(mail.take(), &mut |_| {});
        assert!(matches!(step.outcomes.as_slice(), [(Ticket(1), Outcome::Done { .. })]), "{tool}: {:?}", step.rejected);
    }
    let committed = engine.design().unit(unit).unwrap();
    let docs = engine.docs().unwrap();
    let written = fab_elec_lang::evaluate(docs.sources(), &fab_elec_lang::std_libs()).unwrap();
    let reloaded = written.design.unit(unit).unwrap();
    // A fresh load assigns ids in statement order. Lower with the committed source map as a
    // document reload does, so the comparison also covers every item id and anchor.
    let model = committed.board.as_ref().unwrap();
    let doc = docs.sources().find("ledctl.pcb.splash").unwrap();
    let ledger = board_doc::Ledger::of_map(docs.map(), unit, doc, model.next_item);
    let lowered = board_doc::lower(doc, &docs.sources().doc(doc).unwrap().text, &reloaded.circuit, Some(&ledger)).unwrap();
    assert_eq!(lowered.board.content_hash(), model.content_hash(), "the saved board round-trips with stable item ids");
    let board = reloaded.board.as_ref().unwrap();
    assert_eq!(board.outline[0].bbox(), BBox::new(P::mm(-1.0, -2.0), P::mm(51.0, 31.0)));
    let k = &board.keepouts[board.keepouts.len() - 2];
    assert_eq!(k.area, PolySet::from_ring_nonzero(vec![P::mm(10.0, 1.0), P::mm(10.0, 4.0), P::mm(12.0, 2.0)]));
    assert_eq!((k.layers, k.forbid, k.reason.as_str()), (LayerSet::one(LayerId::B_CU), Forbid::TRACKS.with(Forbid::VIAS), "antenna"));
    let default = board.keepouts.last().unwrap();
    assert_eq!((default.layers, default.forbid, default.reason.as_str()), (board.copper_set(), Forbid::ALL, ""));
    let cut = board.cutouts.last().unwrap();
    assert_eq!(cut.name, "slot");
    assert_eq!(cut.contour.bbox(), BBox::new(P::mm(15.0, 7.0), P::mm(17.0, 8.0)));
}

#[test]
fn project_factory_follows_text_history_and_exports_ledctl() {
    use fab_elec_cam::{profile_doc::lookup, tables::{fill_generic_tables, PlacementRules}, ResolvedProfile};
    use fab_elec_core::ClassId;
    use fab_elec_design::TextEdit;
    use fab_elec_lang::UnitState;

    let profile = r#"profile("Pilot", {
        format: "JLCPCB 2-layer" layers: [2] track: 0.4mm clearance: 0.3mm drill: 0.3mm annular: 0.13mm
        via_in_pad: false board_min: [5mm 5mm] board_max: [400mm 500mm] thickness: [1.6mm] thickness_tol_percent: 5
    })"#;
    let mut sources = fab_elec_fixtures::docs("ledctl").unwrap();
    let doc = sources.add("pilot.export.splash", profile).unwrap();
    let duplicate = sources.add("second.export.splash", format!("// another house\n{}", profile.replace("Pilot", "pilot"))).unwrap();
    let (design, docs) = Docs::load(sources, fab_elec_lang::std_libs());
    let unit = design.unit_by_name("ledctl").unwrap().id;
    assert_eq!(docs.unit_state(unit), Some(&UnitState::Built));
    assert!(!docs.view().notes.iter().any(|d| d.severity == Severity::Error));
    let mut engine = Engine::new(design, Arc::default(), Arc::default(), Arc::default(), "profile".into())
        .with_docs(docs).with_app_tools();
    let mail = Mailbox::default();
    let snap = engine.snapshot();
    let [collision] = snap.profile_diags.as_slice() else { panic!("expected the duplicate: {:?}", snap.profile_diags) };
    assert!(matches!(collision.sel.as_slice(), [Sel::Source { doc, .. }] if *doc == duplicate));
    assert_eq!(fab_elec_host::req_actions::located(snap.docs.as_ref().unwrap(), &collision.sel), ("second.export.splash", 2));

    let select = |name: &str| Cmd::Invoke { tool: "elec.factory.set".into(),
        args: vec![("board".into(), "ledctl".into()), ("profile".into(), name.into())],
        by: By::Person, preview: false, ticket: Ticket(1) };
    mail.push(select("unknown"));
    let rejected = engine.step(mail.take(), &mut |_| {}).rejected;
    assert_eq!(rejected.len(), 1);
    for name in ["standard 2-layer", "JLCPCB 2-layer", "Pilot"] { assert!(rejected[0].1.contains(name)); }
    mail.push(select("PILOT"));
    let selected = engine.step(mail.take(), &mut |_| {});
    assert!(matches!(selected.outcomes.as_slice(), [(Ticket(1), Outcome::Done { .. })]), "{:?}", selected.rejected);
    let min_track = |snap: &DesignSnapshot| snap.of(&snap.rules, unit).unwrap().value.track_min(ClassId(0));
    assert_eq!(min_track(&engine.snapshot()), 400_000);
    assert_eq!(engine.design().unit(unit).unwrap().factory.as_deref(), Some("Pilot"));
    let text = snap.docs.as_ref().unwrap().texts.iter().find(|t| t.doc == doc).unwrap();
    mail.push(Cmd::Text { doc, base_rev: text.rev, edits: vec![TextEdit::trimmed(doc, profile, &profile.replace("0.4mm", "0.6mm"))],
        group: None, label: "Pilot track".into(), tag: None, by: By::Person, ticket: None });
    let changed = engine.step(mail.take(), &mut |_| {});
    assert!(changed.evals.is_empty() && changed.rejected.is_empty());
    assert_eq!(min_track(&engine.snapshot()), 600_000);
    for (cmd, expected) in [(Cmd::Undo, 400_000), (Cmd::Redo, 600_000)] {
        mail.push(cmd);
        let step = engine.step(mail.take(), &mut |_| {});
        assert!(step.evals.is_empty() && step.rejected.is_empty());
        assert_eq!(min_track(&engine.snapshot()), expected);
    }

    let snap = engine.snapshot();
    let board = snap.design.unit(unit).unwrap();
    let def = lookup(&snap.profiles, board.factory.as_deref().unwrap()).unwrap();
    let mut resolved = ResolvedProfile::from_definition(def, &board.name, "A");
    assert_eq!(resolved.capabilities.min_track, min_track(&snap));
    let pcb = board.board.as_ref().unwrap();
    fill_generic_tables(&mut resolved, pcb, &board.circuit, &PlacementRules::jlc());
    let made = fab_elec_cam::export(pcb, &board.circuit, &snap.of(&snap.fills, unit).unwrap().value, &resolved).unwrap();
    let file = |name: &str| std::str::from_utf8(&made.files.iter().find(|f| f.name == name).unwrap().bytes).unwrap();
    assert!(file("README.txt").contains("Pilot"));
    assert!(file("ledctl-bom.csv").lines().next().unwrap().contains("LCSC Part #"));
}

#[test]
fn factory_choice_round_trips_and_only_authored_minima_warn() {
    use fab_elec_circuit::Measure;
    use fab_elec_core::ClassId;
    use fab_elec_design::TextEdit;
    let mut sources = fab_elec_fixtures::docs("ledctl").unwrap();
    let doc = sources.find("ledctl.pcb.splash").unwrap();
    let original = sources.doc(doc).unwrap().text.clone();
    let board = format!("{original}\nrules({{annular: 0.075mm}})\n");
    sources.apply(&[TextEdit::trimmed(doc, &original, &board)]).unwrap();
    let (design, docs) = Docs::load(sources, fab_elec_lang::std_libs());
    let unit = design.units[0].id;
    let mut engine = Engine::new(design, Arc::default(), Arc::default(), Arc::default(), "settings".into())
        .with_docs(docs).with_app_tools();
    let mail = Mailbox::default();
    let invoke = |tool: &str, key: &str, value: &str| Cmd::Invoke { tool: tool.into(), args: vec![
        ("board".into(), "ledctl".into()), (key.into(), value.into())], by: By::Person, ticket: Ticket(1), preview: false };
    let run = |engine: &mut Engine, cmd| {
        mail.push(cmd);
        loop {
            let step = engine.step(mail.take(), &mut |_| {});
            assert!(step.rejected.is_empty(), "{:?}", step.rejected);
            if step.evals.is_empty() && step.fills.is_empty() && step.checks.is_empty() { break; }
            for task in step.evals { mail.result(task.run()); }
            for task in step.fills { mail.result(task.run()); }
            for task in step.checks { mail.result(task.run()); }
        }
    };
    let text = |engine: &Engine| engine.docs().unwrap().text(doc).unwrap().clone();
    run(&mut engine, invoke("elec.rules.set", "track", "0.10"));
    let authored = text(&engine);
    assert!(authored.contains("annular: 0.075mm") && authored.contains("track: 0.1mm"));
    let coarse = "JLCPCB 2-layer";
    let fine = "HDI 6-layer";
    let choose = |name| invoke("elec.factory.set", "profile", name);
    run(&mut engine, choose(coarse));
    let selected = text(&engine);
    assert!(selected.contains("factory(\"JLCPCB 2-layer\")"));
    let docs = engine.docs().unwrap();
    let reloaded = fab_elec_lang::evaluate(docs.sources(), docs.libs()).unwrap();
    assert_eq!(reloaded.design.unit(unit).unwrap().factory.as_deref(), Some(coarse));
    let effective = |snap: &DesignSnapshot| snap.of(&snap.rules, unit).unwrap().value.track_min(ClassId(0));
    let snap = engine.snapshot();
    assert_eq!(effective(&snap), 150_000);
    let raised: Vec<_> = snap.violations.value.iter().filter(|v| v.rule.starts_with("factory:") && v.severity == Severity::Warning).collect();
    assert_eq!(raised.len(), 2, "only track and explicitly standard annular were authored");
    let track = raised.iter().find(|v| v.rule == "factory:JLCPCB 2-layer:track").unwrap();
    assert_eq!((track.actual, track.required), (Some(Measure::Len(100_000)), Some(Measure::Len(150_000))));
    assert_eq!(snap.design.unit(unit).unwrap().board.as_ref().unwrap().rules.base.track, Some(100_000));
    run(&mut engine, Cmd::Undo);
    assert_eq!(text(&engine), authored);
    run(&mut engine, Cmd::Redo);
    assert_eq!(text(&engine), selected);
    run(&mut engine, choose(fine));
    let snap = engine.snapshot();
    assert_eq!(effective(&snap), 100_000);
    assert!(text(&engine).contains("track: 0.1mm"));
    assert!(!snap.violations.value.iter().any(|v| v.rule.starts_with("factory:") && v.severity == Severity::Warning));
    assert!(snap.violations.value.iter().any(|v| v.rule == "factory:HDI 6-layer:layers"));
    run(&mut engine, Cmd::Undo);
    assert_eq!(text(&engine), selected);
    let again = engine.snapshot();
    assert!(again.violations.value.iter().any(|v| v.id == track.id));
    let typed_line = "factory(  \"Misspelled\"  )";
    let typed = selected.replace("factory(\"JLCPCB 2-layer\")", &format!("{typed_line} // typed by hand"));
    let rev = engine.docs().unwrap().view().texts.iter().find(|t| t.doc == doc).unwrap().rev;
    run(&mut engine, Cmd::Text { doc, base_rev: rev, edits: vec![TextEdit::trimmed(doc, &selected, &typed)],
        group: None, label: "type a factory".into(), tag: None, by: By::Person, ticket: None });
    let snap = engine.snapshot();
    assert_eq!(snap.design.unit(unit).unwrap().factory.as_deref(), Some("Misspelled"));
    assert_eq!(effective(&snap), 100_000);
    let unknown: Vec<_> = snap.violations.value.iter().filter(|v| v.rule.starts_with("factory:")).collect();
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].severity, Severity::Error);
    assert!(unknown[0].message.contains(coarse) && unknown[0].message.contains(fine));
    let [Sel::Source { doc: at, start, end }] = unknown[0].items.as_slice() else { panic!("expected factory source") };
    assert_eq!(*at, doc);
    assert_eq!(&typed[*start as usize..*end as usize], typed_line);
    run(&mut engine, choose(fine));
    run(&mut engine, Cmd::Undo);
    assert_eq!(&*text(&engine), typed, "undo restores the exact previous factory line");
}

#[test]
fn circuit_only_factory_is_in_memory_until_a_board_document_arrives() {
    use fab_elec_host::DocSet;
    let mut sources = fab_elec_lang::Sources::new();
    sources.add("solo.ckt.splash", "let e = mod.elec\nlet gnd = e.net({name: \"GND\"})").unwrap();
    let (design, docs) = Docs::load(sources, fab_elec_lang::std_libs());
    let unit = design.units[0].id;
    let mut engine = Engine::new(design, Arc::default(), Arc::default(), Arc::default(), "factory".into())
        .with_docs(docs).with_app_tools();
    let mail = Mailbox::default();
    let choose = || Cmd::Invoke { tool: "elec.factory.set".into(), args: vec![("board".into(), "solo".into()),
        ("profile".into(), "JLCPCB 2-layer".into())], by: By::Person, ticket: Ticket(1), preview: false };
    mail.push(choose());
    let step = engine.step(mail.take(), &mut |_| {});
    assert!(step.rejected.is_empty(), "{:?}", step.rejected);
    assert!(engine.log().entries[0].text.edits.is_empty());
    assert_eq!(engine.design().unit(unit).unwrap().factory.as_deref(), Some("JLCPCB 2-layer"));
    let pcb = "solo.pcb.splash";
    mail.push(Cmd::Docs { set: vec![DocSet::Add { name: pcb.into(), text: "// layout\n".into() }],
        label: "add a board document".into(), by: By::Person, ticket: None });
    let added = engine.step(mail.take(), &mut |_| {});
    for eval in added.evals { mail.result(eval.run()); }
    engine.step(mail.take(), &mut |_| {});
    assert_eq!(engine.design().unit(unit).unwrap().factory, None, "the new document supplies the factory");
    mail.push(choose());
    let step = engine.step(mail.take(), &mut |_| {});
    assert!(step.rejected.is_empty(), "{:?}", step.rejected);
    let sources = engine.docs().unwrap().sources();
    assert!(sources.doc(sources.find(pcb).unwrap()).unwrap().text.contains("factory(\"JLCPCB 2-layer\")"),
        "a document without a board(..) header still receives the choice");
}

#[test]
fn stitch_fill_wait_allows_edits_before_stitching_the_exact_pour() {
    use fab_elec_design::action::{find, Risk};
    fab_elec_host::edit_actions::register_actions();
    let action = find("elec.zone.stitch").expect("stitch is in the catalog");
    assert_eq!(action.risk, Risk::Edit);
    let sources = fab_elec_fixtures::docs("ledctl").unwrap();
    let (design, docs) = Docs::from_evaluated(sources, fab_elec_fixtures::ledctl_eval());
    let unit = &design.units[0];
    let board = unit.board.clone().unwrap();
    let ground = board.net("GND").unwrap();
    let zone = board.zones.iter().find(|z| z.net == Some(ground)).unwrap().id;
    let args = vec![("unit".to_owned(), unit.name.clone()), ("zone".into(), zone.0.to_string()),
        ("pitch".into(), "5 mm".into()), ("pad".into(), "0.8 mm".into()), ("drill".into(), "0.4 mm".into())];
    let planned_sites = |design: &Design| {
        let exact = engines::full(design);
        fab_elec_view::board::plan::stitch(design, board.unit, exact.boards.iter().find(|r| r.unit == board.unit).unwrap(),
            ground, Some(zone), 5_000_000, Some(800_000), Some(400_000)).unwrap().ops.into_iter().map(|op| match op {
                DesignOp::Board { op: BoardOp::Add(Item::Via(v)), .. } => v.at,
                _ => panic!("each planned op adds a via"),
            }).collect::<Vec<_>>()
    };
    let sites = planned_sites(&design);
    let existing = board.vias.iter().find(|via| via.net == ground).unwrap();
    // Moving this via here first would block the first stitch site by the half-pitch rule.
    let later_at = sites[0].add(P::mm(1.0, 0.0));
    let notices = Arc::new(Mutex::new(Vec::new()));
    let messages = notices.clone();
    let mut engine = Engine::new_held(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)), "stitch".into()).with_docs(docs).with_app_tools()
        .with_fill_wait_notice(move |wait| messages.lock().unwrap().push((wait.unit, wait.action, wait.waiting)));
    let mail = Mailbox::default();
    let stitch = |ticket| Cmd::Invoke { tool: action.name.into(), args: args.clone(), preview: false, by: By::Ai { turn: 1, call: 1 }, ticket };
    let move_via = |ticket| Cmd::Commit { by: By::Person, place: vec![], circuits: vec![], expect: None, ticket: Some(ticket),
        ops: vec![DesignOp::Board { unit: board.unit, op: BoardOp::Move { item: existing.id, by: later_at.sub(existing.at) } }],
        label: "move the existing via".into() };
    mail.push(stitch(Ticket(1)));
    let waiting = engine.step(mail.take(), &mut |_| {});
    assert!(waiting.waiting && waiting.outcomes.is_empty() && !waiting.fills.is_empty());
    mail.push(move_via(Ticket(2)));
    let moved = engine.step(mail.take(), &mut |_| {});
    assert!(matches!(moved.outcomes.as_slice(), [(Ticket(2), Outcome::Done { .. })]));
    assert!(moved.waiting && !moved.fills.is_empty(), "the move applied while the pour still owed a refill");
    let moved_board = engine.design().unit(board.unit).unwrap().board.clone().unwrap();
    assert_eq!(moved_board.vias.iter().find(|v| v.id == existing.id).unwrap().at, later_at);
    assert_eq!(moved_board.vias.len(), board.vias.len());
    let expected = planned_sites(engine.design());
    assert!(!expected.contains(&sites[0]), "moving the existing via blocks the old first site");
    for job in waiting.fills { mail.result(job.run()); }
    let stale = engine.step(mail.take(), &mut |_| {});
    assert!(stale.waiting && stale.outcomes.is_empty() && stale.fills.is_empty(), "the old fill cannot release the stitch");
    for job in moved.fills { mail.result(job.run()); }
    let step = engine.step(mail.take(), &mut |_| {});
    assert_eq!(*notices.lock().unwrap(), [(board.unit, action.name, true), (board.unit, action.name, false)]);
    let snap = engine.snapshot();
    let stitched = snap.design.unit(board.unit).unwrap().board.as_ref().unwrap();
    let added = &stitched.vias[board.vias.len()..];
    assert!(!added.is_empty(), "the two-sided fixture pour accepts sites: {:?}", step.rejected);
    for via in added {
        assert_eq!((via.net, via.pad, via.drill), (ground, 800_000, 400_000));
        assert_eq!((via.at.x % 5_000_000, via.at.y % 5_000_000), (0, 0));
        assert_eq!(via.origin, fab_elec_board::Origin::Authored);
    }
    assert_eq!(added.iter().map(|v| v.at).collect::<Vec<_>>(), expected, "the stitch used the moved board and its exact fills");
    assert_eq!(stitched.vias.iter().find(|v| v.id == existing.id).unwrap().at, later_at);
    assert!(matches!(step.outcomes.as_slice(), [(Ticket(1), Outcome::Done { label, .. })]
        if label == &format!("Stitch GND: {} vias", expected.len())));
    assert!(engine.step(mail.take(), &mut |_| {}).outcomes.is_empty(), "the catalog call answers exactly once");
    let written = fab_elec_lang::evaluate(engine.docs().unwrap().sources(), &fab_elec_lang::std_libs()).unwrap();
    let reloaded = written.design.unit(board.unit).unwrap().board.as_ref().unwrap();
    assert_eq!(reloaded.vias.iter().map(|v| (v.at, v.drill, v.pad, v.net)).collect::<Vec<_>>(),
        stitched.vias.iter().map(|v| (v.at, v.drill, v.pad, v.net)).collect::<Vec<_>>());
    mail.push(Cmd::Undo);
    engine.step(mail.take(), &mut |_| {});
    assert_eq!(engine.design().unit(board.unit).unwrap().board.as_ref().unwrap().content_hash(), moved_board.content_hash());
    mail.push(Cmd::Undo);
    let restored = engine.step(mail.take(), &mut |_| {});
    assert_eq!(engine.design().unit(board.unit).unwrap().board.as_ref().unwrap().content_hash(), board.content_hash());

    // Reverse the order in one inbox. Its arriving fills predate the move, so the stitch
    // must wait for the new pour before deciding whether the first lattice site is free.
    for fill in restored.fills { mail.result(fill.run()); }
    mail.push(move_via(Ticket(3)));
    mail.push(stitch(Ticket(4)));
    let moved_first = engine.step(mail.take(), &mut |_| {});
    assert!(matches!(moved_first.outcomes.as_slice(), [(Ticket(3), Outcome::Done { .. })]));
    assert!(moved_first.waiting && !moved_first.fills.is_empty());
    for fill in moved_first.fills { mail.result(fill.run()); }
    let stitched_after = engine.step(mail.take(), &mut |_| {});
    assert!(matches!(stitched_after.outcomes.as_slice(), [(Ticket(4), Outcome::Done { .. })]));
    assert!(engine.design().unit(board.unit).unwrap().board.as_ref().unwrap().vias.iter().all(|v| v.at != sites[0]));
}

#[test]
fn board_first_load_fills_then_checks_once() {
    let design = ledctl();
    let unit = BoardId(0);
    let via = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board").vias[0].clone();
    let factories = Arc::new(Factories::new());
    let mut engine = Engine::new_held(design, factories.clone(), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into())
        .with_resolver("elec.route", route_plan);
    let owed = engines::owed(engine.results());
    assert!(!owed.is_empty(), "a board-first load owes the fixture's zone fill");
    let loaded = engine.snapshot();
    for stage in [StageFlags::FILLS, StageFlags::CONNECT, StageFlags::DRC] {
        assert!(loaded.stale.has(stage), "the loaded snapshot marks fills, connectivity and DRC stale until the checks merge");
    }
    assert_eq!(loaded.checking, vec![unit], "views show the loaded unit as checking until its whole-board checks merge");
    let unchecked = engine.results().boards[0].drc.clone();
    assert!(loaded.violations.value.iter().all(|v| unchecked.iter().all(|u| u.id != v.id)),
        "no finding of the board without its pours is shown");
    let mailbox = Mailbox::default();
    let mut published = Vec::new();
    let first = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(first.fills.iter().map(|job| &job.task).collect::<Vec<_>>(), owed.iter().collect::<Vec<_>>(), "the first step starts every owed fill");
    assert!(first.checks.is_empty(), "no checks run while a fill is owed");
    for job in &first.fills {
        mailbox.result(job.run());
    }
    let filled = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(filled.checks.len(), 1, "after the last fill the step starts the unit's whole-board checks on the pool");
    let pass = filled.checks[0].clone();

    // A command queued while the checks run applies in the next step, before the pass merges:
    // the pass runs off the strand.
    let at = |engine: &Engine| engine.design().unit(unit).unwrap().board.as_ref().unwrap().vias.iter().find(|v| v.id == via.id).unwrap().at;
    let by = P::mm(0.3, -0.2);
    mailbox.push(Cmd::Commit { by: By::Person, ticket: None, place: vec![], ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item: via.id, by } }], label: "move via".into(), circuits: vec![], expect: None });
    // A route asked meanwhile waits for the checks: until they merge, the connectivity is the
    // board's without its pours.
    mailbox.push(Cmd::Invoke { tool: "elec.route".into(), args: vec![], preview: false, by: By::Person, ticket: Ticket(1) });
    let moved = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(moved.rejected.is_empty(), "the move applies while the checks run");
    assert_eq!(at(&engine), via.at.add(by), "the command applied before the pass's result arrived");
    assert!(engine.results().boards[0].pending.whole, "the edit joins the whole-board record");
    let mut routes = moved.routes;

    // The pass made before the edit is overtaken. It runs to its end (a running pool task cannot
    // be stopped) and no second pass starts beside it; it then merges under the edit since, so the
    // load ends exact without another pass.
    for job in &moved.fills {
        mailbox.result(job.run());
    }
    let refilled = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(!moved.fills.is_empty() && refilled.checks.is_empty(), "the edit's refill merges, and no pass starts while the overtaken one runs");
    assert!(engine.results().boards[0].pending.whole && routes.is_empty() && refilled.routes.is_empty(),
        "no route starts while the unit owes its checks");
    mailbox.result(pass.run());
    let mut next = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(!engine.results().boards[0].pending.whole && next.checks.is_empty(), "the overtaken pass merges under the edit: no pass runs again");
    routes.extend(next.routes.iter().cloned());
    for _ in 0..4 {
        if next.fills.is_empty() && next.checks.is_empty() {
            break;
        }
        for job in &next.fills {
            mailbox.result(job.run());
        }
        for job in &next.checks {
            mailbox.result(job.run());
        }
        next = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
        routes.extend(next.routes.iter().cloned());
    }
    let full = engines::full_with(engine.design(), &factories);
    assert_eq!(engine.results(), &full, "a board-first load with an edit during its checks ends equal to full results");
    let last = published.last().expect("the merged checks publish");
    assert!(last.stale.is_empty() && last.checking.is_empty(), "the merged checks leave no stage stale and no unit checking");
    assert_eq!(*last.violations.value, engines::violations(&full), "the merged checks show the board's findings");
    assert_eq!(routes.len(), 1, "the route asked during the load starts once, after the checks merge");
    assert!(routes[0].base.exact && Arc::ptr_eq(&routes[0].base.conn, &engine.results().boards[0].conn),
        "the route starts from the merged connectivity");
}

#[test]
fn router_inputs_drop_with_a_reason() {
    let mailbox = Mailbox::default();
    let design = ledctl();
    let unit = BoardId(0);
    let via = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board").vias[0].id;
    let move_by = |by| DesignOp::Board { unit, op: BoardOp::Move { item: via, by } };
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into());
    let before_move = engine.design().unit(unit).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board").content_hash();
    let mut wrong_hash = before_move;
    wrong_hash.0[0] ^= 1;

    mailbox.preview(Sample {
        seq: 1, tag: 7, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: Some((unit, wrong_hash)),
        ops: vec![move_by(P::mm(0.2, 0.0))], label: "stale sample".into(),
    });
    let dropped = engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(dropped.rejected.len(), 1, "a stale sample is refused once");
    let snapshot = engine.snapshot();
    let refusal = snapshot.last_refusal.clone().expect("a stale sample publishes its refusal (Fable H3)");
    assert_eq!(refusal.sample, Some(7), "the stale sample's refusal names its tag");
    assert!(refusal.text.contains("changed after this edit was planned"), "the refusal says the board changed: {}", refusal.text);
    assert!(snapshot.preview.is_none(), "a stale sample creates no live preview");
    mailbox.push(Cmd::CommitPreview { seq: 1, tag: 7 });
    let answered = engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(answered.rejected.len(), 1, "the stale sample's drop is answered once");
    assert_eq!(engine.snapshot().last_refusal, Some(refusal), "the drop's answer is the stale sample's refusal");

    mailbox.preview(Sample {
        seq: 2, tag: 8, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: Some((unit, before_move)),
        ops: vec![move_by(P::mm(0.2, 0.0))], label: "current sample".into(),
    });
    let accepted = engine.step(mailbox.take(), &mut |_| {});
    assert!(accepted.rejected.is_empty(), "a sample on the current board hash is accepted");
    assert_eq!(engine.snapshot().preview_tag, Some(8), "the accepted preview echoes its sample tag");

    mailbox.push(Cmd::Commit {
        ops: vec![move_by(P::mm(0.3, -0.2))], place: vec![], label: "move board base".into(),
        circuits: vec![], expect: None, by: By::Person, ticket: None,
    });
    let moved = engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(moved.rejected.len(), 1, "a live sample whose base a commit moved is refused once, not rebased onto the new board");
    let refusal = engine.snapshot().last_refusal.clone().expect("the moved sample publishes its refusal");
    assert_eq!(refusal.sample, Some(8), "the moved sample's refusal names its tag");
    assert!(engine.snapshot().preview.is_none(), "refusing the rebased sample ends the live preview");
    let logged = engine.log().entries.len();
    mailbox.push(Cmd::CommitPreview { seq: 2, tag: 8 });
    let answered = engine.step(mailbox.take(), &mut |_| {});
    assert_eq!((answered.rejected.len(), engine.snapshot().last_refusal.clone()), (1, Some(refusal)),
        "the moved sample's drop is answered once with its refusal");
    mailbox.preview(Sample {
        seq: 3, tag: 9, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: Some((unit, before_move)),
        ops: vec![move_by(P::mm(0.2, 0.0))], label: "stale commit sample".into(),
    });
    mailbox.push(Cmd::CommitPreview { seq: 3, tag: 9 });
    let answered = engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(engine.log().entries.len(), logged, "committing a sample on a moved base adds no history entry");
    assert_eq!((answered.rejected.len(), engine.snapshot().last_refusal.as_ref().and_then(|refusal| refusal.sample)), (1, Some(9)),
        "a stale sample dropped in the step it arrives is refused once, under its tag");

    let refusal = engine.snapshot().last_refusal.clone();
    mailbox.push(Cmd::RouteBase { unit });
    let asked = engine.step(mailbox.take(), &mut |_| {});
    assert!(asked.rejected.is_empty(), "a route base request is answered, not rejected");
    assert_eq!(engine.snapshot().last_refusal, refusal, "answering a route base preserves the previous refusal");
    assert!(asked.routes.is_empty(), "a route base request starts no route jobs");
    let board = engine.design().unit(unit).and_then(|found| found.board.clone()).expect("unit 0 keeps its board");
    assert_eq!(asked.bases.iter().map(|base| (base.unit, base.hash)).collect::<Vec<_>>(), vec![(unit, board.content_hash())],
        "a route base request is answered with exactly the unit's committed base");
    let quiet = engine.step(mailbox.take(), &mut |_| {});
    assert!(quiet.bases.is_empty(), "a step that moves nothing sends the router no base");
}

/// Fable H3 (11-16): a released sample ends in one commit or one refusal under its tag, which a
/// pane waits on: a sample refused while live, one another author's sample replaced, and one the
/// host refuses (the bed is changing) all answer their drop.
#[test]
fn a_released_sample_ends_in_one_commit_or_one_tagged_refusal() {
    fn answer(engine: &mut Engine, mailbox: &Mailbox, cmd: Cmd) -> (usize, Option<u64>) {
        mailbox.push(cmd);
        let step = engine.step(mailbox.take(), &mut |_| {});
        (step.rejected.len(), engine.snapshot().last_refusal.as_ref().and_then(|refusal| refusal.sample))
    }
    let mailbox = Mailbox::default();
    let design = ledctl();
    let unit = BoardId(0);
    let via = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board").vias[0].id;
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into());
    let sample = |seq, tag, by, op| Sample {
        seq, tag, by, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: None, ops: vec![DesignOp::Board { unit, op }],
        label: format!("sample {seq}"),
    };
    let move_via = || BoardOp::Move { item: via, by: P::mm(0.2, 0.0) };

    mailbox.preview(sample(1, 33, By::Person, BoardOp::Remove(ItemId(u32::MAX))));
    let refused = engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(refused.rejected.len(), 1, "a sample whose op cannot apply is refused while live");
    assert_eq!(answer(&mut engine, &mailbox, Cmd::CommitPreview { seq: 1, tag: 33 }), (1, Some(33)),
        "the refused sample's drop is answered once under its tag");

    mailbox.preview(sample(2, 34, By::Person, move_via()));
    engine.step(mailbox.take(), &mut |_| {});
    let by = By::Ai { turn: 1, call: 1 };
    mailbox.preview(sample(3, 35, by, move_via()));
    engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(engine.snapshot().preview_by, Some(by), "another author's sample replaces the pane's");
    assert_eq!(answer(&mut engine, &mailbox, Cmd::CommitPreview { seq: 2, tag: 34 }), (1, Some(34)),
        "the replaced sample's drop is refused once under its tag");
    let snapshot = engine.snapshot();
    assert!(snapshot.last_refusal.as_ref().is_some_and(|refusal| refusal.text.contains("ended")), "the refusal says the preview had ended");
    assert_eq!(snapshot.preview_tag, Some(35), "refusing the replaced sample leaves the other author's preview live");

    let logged = engine.log().entries.len();
    let reason = "another document is going on the bed: the edit was not applied";
    assert_eq!(answer(&mut engine, &mailbox, Cmd::RefusePreview { seq: 3, tag: 35, reason: reason.into() }), (1, Some(35)),
        "a sample the host refuses is refused once under its tag");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.last_refusal.as_ref().map(|refusal| refusal.text.as_str()), Some(reason), "the host's refusal carries its reason");
    assert!(snapshot.preview.is_none(), "the refused sample's preview ends");
    assert_eq!(engine.log().entries.len(), logged, "a refused sample adds no history entry");

    // One frame [Preview s1, CommitPreview s1, Preview s2 by another author]: s2 replaces s1 in the
    // slot after s1's commit was queued, and s1 still commits, once, under its tag.
    let logged = engine.log().entries.len();
    mailbox.preview(sample(4, 36, By::Person, move_via()));
    mailbox.push(Cmd::CommitPreview { seq: 4, tag: 36 });
    mailbox.preview(sample(5, 37, by, move_via()));
    let frame = engine.step(mailbox.take(), &mut |_| {});
    assert!(frame.rejected.is_empty(), "a sample replaced after its commit was queued is not refused: {:?}", frame.rejected);
    assert_eq!(engine.log().entries.len(), logged + 1, "the queued commit makes exactly one history entry");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.last_commit.as_ref().map(|commit| commit.tag), Some(Some(36)), "the commit is the replaced sample's, under its tag");
    assert_eq!((snapshot.preview_tag, snapshot.preview_by), (Some(37), Some(by)), "the newer sample previews over that commit under its own tag");

    // The reverse [Preview s1, Preview s2, CommitPreview s1]: s1 was replaced before its commit was
    // queued, so its drop is still refused as ended.
    let logged = engine.log().entries.len();
    mailbox.preview(sample(6, 38, By::Person, move_via()));
    mailbox.preview(sample(7, 39, by, move_via()));
    mailbox.push(Cmd::CommitPreview { seq: 6, tag: 38 });
    let frame = engine.step(mailbox.take(), &mut |_| {});
    // The newer preview, accepted in the same step, clears the snapshot's refusal: read the step's.
    assert!(matches!(frame.rejected.as_slice(), [(_, text)] if text.contains("ended")),
        "a sample replaced before its commit was queued is refused once, as ended: {:?}", frame.rejected);
    assert_eq!(engine.log().entries.len(), logged, "a sample replaced before its commit commits nothing");
    assert_eq!(engine.snapshot().preview_tag, Some(39), "the newer sample stays the preview");

    // A queued commit keeps only the sample it names by seq and tag: another counter's equal seq
    // with a different tag is replaced as before.
    mailbox.preview(sample(8, 40, By::Person, move_via()));
    mailbox.push(Cmd::CommitPreview { seq: 8, tag: 41 });
    mailbox.preview(sample(9, 42, by, move_via()));
    let frame = engine.step(mailbox.take(), &mut |_| {});
    assert!(matches!(frame.rejected.as_slice(), [(_, text)] if text.contains("ended")),
        "a commit naming the seq under another tag keeps no sample and is refused once, as ended: {:?}", frame.rejected);
    assert_eq!(engine.log().entries.len(), logged, "a commit naming the seq under another tag commits nothing");
}

#[test]
fn expected_board_hash_tracks_committed_moves() {
    let mailbox = Mailbox::default();
    let design = ledctl();
    let unit = BoardId(0);
    let via = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board").vias[0].id;
    let move_by = |by| DesignOp::Board { unit, op: BoardOp::Move { item: via, by } };
    let hash = |d: &Design| d.unit(unit).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board").content_hash();
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into());
    let h0 = hash(engine.design());
    let sample = |seq, expected, label: &str| Sample {
        seq, tag: seq, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: Some((unit, expected)),
        ops: vec![move_by(P::mm(0.2, 0.0))], label: label.into(),
    };

    mailbox.preview(sample(1, h0, "original board sample"));
    let accepted = engine.step(mailbox.take(), &mut |_| {});
    assert!(accepted.rejected.is_empty(), "a sample with the original committed hash is accepted");
    assert!(engine.snapshot().preview.is_some(), "the original board sample becomes the live preview");
    assert_eq!(engine.snapshot().preview_tag, Some(1), "the live preview identifies the original board sample");

    mailbox.push(Cmd::Commit {
        ops: vec![move_by(P::mm(0.3, -0.2))], place: vec![], label: "move committed board".into(),
        circuits: vec![], expect: Some((unit, h0)), by: By::Person, ticket: None,
    });
    let moved = engine.step(mailbox.take(), &mut |_| {});
    let refused = |engine: &Engine| engine.snapshot().last_refusal.as_ref().map(|refusal| (refusal.label.clone(), refusal.sample));
    assert_eq!((moved.rejected.len(), refused(&engine)), (1, Some(("original board sample".into(), Some(1)))),
        "a committed move refuses the live sample once, with its label and tag");
    let h1 = hash(engine.design());
    assert_ne!(h1, h0, "the committed via move changes the board content hash");

    mailbox.preview(sample(2, h0, "old board sample"));
    let dropped = engine.step(mailbox.take(), &mut |_| {});
    assert_eq!((dropped.rejected.len(), refused(&engine)), (1, Some(("old board sample".into(), Some(2)))),
        "a sample with the old committed hash is refused once, with its label and tag");
    assert!(engine.snapshot().preview.is_none(), "a stale expected hash creates no live preview");

    mailbox.preview(sample(3, h1, "new board sample"));
    let accepted = engine.step(mailbox.take(), &mut |_| {});
    assert!(accepted.rejected.is_empty(), "a sample with the new committed hash is accepted");
    assert!(engine.snapshot().preview.is_some(), "the new board sample becomes the live preview");
    assert_eq!(engine.snapshot().preview_tag, Some(3), "the live preview identifies the new board sample");
}

/// Why: one Ctrl+Z order across the plate and the electronics, 11-15.
#[test]
fn a_commit_elsewhere_kills_the_engines_redo() {
    let mailbox = Mailbox::default();
    let sources = fab_elec_fixtures::docs("ledctl").expect("the fixture supplies ledctl documents");
    let (design, docs) = Docs::from_evaluated(sources, fab_elec_fixtures::ledctl_eval());
    let unit = BoardId(0);
    let part = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board")
        .placed.first().expect("the fixture has a placed part").id;
    let steps = Arc::new(AtomicU64::new(40));
    let logged = Arc::new(AtomicU64::new(40));
    let mut engine = Engine::new(design, Arc::new(Factories::new()), steps.clone(), logged.clone(), "fixture".into()).with_docs(docs);
    let texts = |engine: &Engine| engine.docs().expect("the engine retains its documents").sources().docs().iter()
        .map(|doc| (doc.id, doc.text.clone())).collect::<Vec<_>>();
    let initial_texts = texts(&engine);
    let move_part = || Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item: part, by: P::mm(1.0, 0.0) } }],
        label: "move first part".into(), circuits: vec![], expect: None,
    };
    mailbox.push(move_part());
    let committed = engine.step(mailbox.take(), &mut |_| {});
    assert!(committed.rejected.is_empty(), "the board move is accepted");
    assert_eq!(committed.undo, Some(41), "the board commit takes the next shared step number");
    assert_ne!(texts(&engine), initial_texts, "the board move changes its document text");

    mailbox.push(Cmd::Undo);
    let undone = engine.step(mailbox.take(), &mut |_| {});
    assert!(undone.rejected.is_empty(), "the board move undoes");
    assert_eq!((undone.undo, undone.redo), (None, Some(41)), "undo exposes the same step for redo");
    let after_undo = engine.design().clone();
    let after_undo_texts = texts(&engine);
    assert_eq!(after_undo_texts, initial_texts, "undo restores every document's original text");
    let seq = steps.fetch_add(1, SeqCst) + 1;
    logged.fetch_max(seq, SeqCst);
    let idle = engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(idle.redo, None, "a commit elsewhere hides redo before a redo command arrives");

    mailbox.push(Cmd::Redo);
    let mut published = Vec::new();
    let refused = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(refused.rejected, vec![("move first part".to_string(), "nothing to redo: an edit came after the undo".to_string())], "redo reports the invalidated entry's label and reason");
    assert_eq!(refused.redo, None, "refused redo exposes no redo step");
    assert_eq!(engine.log().entries.len(), engine.log().head, "refused redo discards the stale log entries");
    assert_eq!(engine.design(), &after_undo, "refused redo leaves the model as it was after undo");
    assert_eq!(texts(&engine), after_undo_texts, "refused redo leaves every document as it was after undo");
    let snapshot = published.last().expect("refused redo publishes a snapshot");
    let refusal = snapshot.last_refusal.as_ref().expect("the snapshot retains the redo refusal");
    assert_eq!(refusal.op, "redo", "the published refusal identifies redo");
    assert_eq!(refusal.text, refused.rejected[0].1, "the published refusal carries the same explanation");
    assert_eq!(steps.load(SeqCst), 42, "undo and refused redo allocate no shared step numbers");

    mailbox.push(move_part());
    mailbox.push(move_part());
    let committed = engine.step(mailbox.take(), &mut |_| {});
    assert!(committed.rejected.is_empty(), "new board edits are accepted after redo is discarded");
    assert_eq!(committed.undo, Some(44), "new commits continue the shared sequence");
    mailbox.push(Cmd::Undo);
    let undone = engine.step(mailbox.take(), &mut |_| {});
    assert!(undone.rejected.is_empty(), "the latest new board edit undoes");
    assert_eq!((undone.undo, undone.redo), (Some(43), Some(44)), "undo exposes the two new step numbers");
    let seq = steps.fetch_add(1, SeqCst) + 1;
    logged.fetch_max(seq, SeqCst);
    mailbox.push(Cmd::Undo);
    mailbox.push(Cmd::Redo);
    let restored = engine.step(mailbox.take(), &mut |_| {});
    assert!(restored.rejected.is_empty(), "a fresh undo and redo work after a commit elsewhere");
    assert_eq!((restored.undo, restored.redo), (Some(43), None), "another undo cannot revive redo killed by an external commit");
    assert_eq!(steps.load(SeqCst), 45, "fresh undo and redo also retain the shared counter");
}

#[test]
fn refusal_is_published_and_cleared_by_an_accepted_edit() {
    let mailbox = Mailbox::default();
    let design = ledctl();
    let unit = BoardId(0);
    let via = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board").vias[0].id;
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into());
    let mut published = Vec::new();
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Bench {
            bench: "ledctl".into(), op: BenchOp::SetPressed { comp: "button".into(), pressed: true },
        }],
        label: "press bench button".into(), circuits: vec![], expect: None,
    });
    let refused = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(refused.rejected.len(), 1, "the bench edit is refused exactly once");
    assert!(refused.rejected[0].1.contains("BenchIsText"), "the bench edit is refused because it needs source text");
    let snapshot = published.last().expect("a refused edit publishes a snapshot without a design change");
    let refusal = snapshot.last_refusal.as_ref().expect("the snapshot retains the refused bench edit");
    assert_eq!(refusal.seq, 1, "the first refusal has seq one");
    assert_eq!(refusal.label, "press bench button", "the refusal retains the view's action label");
    assert_eq!(refusal.op, "bench", "the refusal identifies the first op as a bench edit");
    assert!(!refusal.text.is_empty(), "the refusal explains why the edit failed");
    assert_eq!(refusal.text, refused.rejected[0].1, "the snapshot and rejected list carry the same reason");
    assert!(refusal.site.is_none(), "the refusal has no source range yet");
    published.clear();

    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item: via, by: P::mm(0.3, -0.2) } }],
        label: "move via".into(), circuits: vec![], expect: None,
    });
    let accepted = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(accepted.rejected.is_empty(), "the board move is accepted");
    let snapshot = published.last().expect("the accepted board edit publishes a snapshot");
    assert!(snapshot.last_refusal.is_none(), "the next accepted edit clears the previous refusal");
}

#[test]
fn tags_tickets_and_authors_reach_the_snapshot_and_the_caller() {
    let mailbox = Mailbox::default();
    let sources = fab_elec_fixtures::docs("ledctl").expect("the fixture supplies ledctl documents");
    let (design, docs) = Docs::from_evaluated(sources, fab_elec_fixtures::ledctl_eval());
    let unit = BoardId(0);
    let part = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board")
        .placed.first().expect("the fixture has a placed part").clone();
    let move_part = || DesignOp::Board { unit, op: BoardOp::Move { item: part.id, by: P::mm(1.0, 0.0) } };
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into()).with_docs(docs);
    let by = By::Ai { turn: 1, call: 2 };
    let mut published = Vec::new();
    mailbox.preview(Sample { seq: 1, tag: 7, ops: vec![move_part()], place: vec![], label: "tagged move".into(), by, proposal: None, notes: vec![], circuits: vec![], expect: None });
    let previewed = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(previewed.rejected.is_empty(), "a tagged board preview is accepted");
    let snapshot = published.last().expect("a tagged preview publishes a snapshot");
    assert_eq!(snapshot.preview_tag, Some(7), "the published preview echoes its sample tag");
    assert_eq!(snapshot.preview_by, Some(by), "the published preview echoes its AI author");
    assert!(snapshot.created.is_empty(), "moving an existing part creates no preview items");
    published.clear();

    mailbox.push(Cmd::CommitPreview { seq: 1, tag: 7 });
    let committed = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(committed.rejected.is_empty(), "the tagged preview commits");
    let entry = engine.log().entries.last().expect("committing a preview logs an entry");
    assert_eq!(entry.by, by, "the preview's history entry retains its AI author");
    let snapshot = published.last().expect("committing the preview publishes a snapshot");
    let commit = snapshot.last_commit.as_ref().expect("the snapshot retains the accepted commit");
    assert_eq!(commit.tag, Some(7), "the accepted commit echoes the committed sample tag");
    assert_eq!(commit.by, by, "the accepted commit retains the preview's AI author");
    assert_eq!(commit.seq, entry.seq, "the accepted commit names the logged step");
    assert!(commit.created.contains(&Sel::Item(unit, part.id)), "a board commit reports the part statement it wrote");
    assert_eq!((snapshot.preview_tag, snapshot.preview_by), (None, None), "a finished preview clears its sample metadata");
    published.clear();

    mailbox.push(Cmd::Commit {
        ops: vec![move_part()], place: vec![], label: "ticketed move".into(), circuits: vec![], expect: None, by: By::Person, ticket: Some(Ticket(5)),
    });
    let committed = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(committed.rejected.is_empty(), "the ticketed board commit is accepted");
    let entry = engine.log().entries.last().expect("the ticketed commit logs an entry");
    let expected_created: Vec<_> = entry.text.adds.iter().map(|(sel, ..)| sel.clone()).collect();
    let expected_sites: Vec<_> = entry.text.adds.iter().map(|(_, doc, span)| Sel::Source {
        doc: *doc, start: span.start, end: span.end,
    }).collect();
    assert!(matches!(committed.outcomes.as_slice(), [(Ticket(5), Outcome::Done { seq, label, created, sites, by, targets, circuit: CircuitChange::None, notes })]
        if *seq == entry.seq && label == "ticketed move" && *created == expected_created && *sites == expected_sites && *by == By::Person
            && *targets == [Sel::Item(unit, part.id)] && notes.is_empty()),
        "the caller gets its ticket, logged sequence, label, author, every written board statement and the moved part, with no resolver notes");
    let snapshot = published.last().expect("the ticketed commit publishes a snapshot");
    assert_eq!(snapshot.last_commit.as_ref().expect("the ordinary commit is retained").tag, None,
        "an ordinary commit has no preview tag");
    published.clear();

    mailbox.push(Cmd::Invoke { tool: "board.place".into(), args: vec![], preview: false, by, ticket: Ticket(6) });
    let invoked = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(invoked.rejected.len(), 1, "an unresolved tool is refused exactly once");
    let snapshot = published.last().expect("an unresolved tool publishes a refusal");
    let refusal = snapshot.last_refusal.as_ref().expect("the tool refusal is retained");
    assert_eq!(refusal.text, "no plan function for board yet", "the tool refusal names its prefix's missing plan function");
    assert!(matches!(invoked.outcomes.as_slice(), [(Ticket(6), Outcome::Refused(answer))] if Arc::ptr_eq(answer, refusal)),
        "the tool caller gets its ticket and the exact published refusal Arc");
    assert_eq!(refusal.sample, None, "a tool refusal has no preview sample tag");

    let texts = |engine: &Engine| engine.docs().expect("the engine retains its documents").sources().docs().iter()
        .map(|doc| (doc.id, doc.text.clone())).collect::<Vec<_>>();
    let before = texts(&engine);
    let head = engine.log().head;
    mailbox.push(Cmd::Commit {
        ops: vec![move_part()],
        place: vec![Place { unit, part: PartRef::Path(part.path.clone()), at: P::mm(2.0, 3.0), rot: None, side: None }],
        label: "place with move".into(), circuits: vec![], expect: None, by: By::Person, ticket: None,
    });
    let refused = engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(refused.rejected, vec![("place with move".to_string(), format!("{} cannot be placed: already placed", part.path))],
        "placing an existing footprint again is refused with the part and reason");
    assert!(engine.snapshot().last_refusal.as_ref().expect("placement publishes its refusal").site.is_some(),
        "the already-placed refusal retains the component's source location");
    assert_eq!(texts(&engine), before, "refusing placement leaves every document's bytes unchanged");
    assert_eq!(engine.log().head, head, "refusing placement logs no partial commit");
}

#[test]
fn invokes_resolve_in_order_preserve_the_caller_and_answer_previews() {
    fn move_via(design: &Design, _docs: Option<&Docs>, _tool: &str, args: &[(String, String)]) -> Result<Plan, Diag> {
        let dx = args.iter().find(|(key, _)| key == "dx").and_then(|(_, value)| value.parse::<i32>().ok())
            .ok_or_else(|| Diag::error("dx must be an integer", vec![]))?;
        let unit = &design.units[0];
        let item = unit.board.as_ref().expect("the resolver fixture has a board").vias[0].id;
        Ok(Plan::Edit(Resolved {
            ops: vec![DesignOp::Board { unit: unit.id, op: BoardOp::Move { item, by: P::new(dx, 0) } }],
            place: vec![], label: "tool move".into(),
        }))
    }
    fn refuse(_design: &Design, _docs: Option<&Docs>, _tool: &str, _args: &[(String, String)]) -> Result<Plan, Diag> {
        Err(Diag::error("test resolver refused", vec![]))
    }
    let design = ledctl();
    let unit = design.units[0].id;
    let via = design.units[0].board.as_ref().expect("the fixture has a board").vias[0].clone();
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "invoke".into())
        .with_resolver("test.move", move_via).with_resolver("test", refuse);
    let mailbox = Mailbox::default();
    let by = By::Ai { turn: 1, call: 2 };
    let invoke = |ticket, preview| Cmd::Invoke {
        tool: "test.move.via".into(), args: vec![("dx".into(), "300000".into())], preview, by, ticket: Ticket(ticket),
    };
    mailbox.push(invoke(7, false));
    mailbox.push(Cmd::Commit {
        ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item: via.id, by: P::new(0, 200000) } }],
        place: vec![], label: "pane move".into(), circuits: vec![], expect: None, by: By::Person, ticket: None,
    });
    let committed = engine.step(mailbox.take(), &mut |_| {});
    assert!(committed.rejected.is_empty(), "the longest registered dotted prefix resolves the move");
    assert_eq!(engine.log().entries.len(), 2, "the tool and pane each log one entry");
    let entry = &engine.log().entries[0];
    assert_eq!(entry.by, by, "the first queued entry retains the tool caller's AI identity");
    assert_eq!(engine.log().entries[1].by, By::Person, "the following pane commit retains its person identity");
    assert!(matches!(committed.outcomes.as_slice(), [(Ticket(7), Outcome::Done { seq, .. })] if *seq == entry.seq),
        "the tool gets Done for its own entry before the following pane commit");
    assert_eq!(engine.design().units[0].board.as_ref().expect("the board remains").vias[0].at, via.at.add(P::new(300000, 200000)),
        "the argument-driven tool move and subsequent pane move both apply");

    for (tool, message) in [
        ("unknown.move.via", "no plan function for unknown.move yet"),
        ("test.moves.via", "test resolver refused"),
        ("test", "test resolver refused"),
    ] {
        mailbox.push(Cmd::Invoke { tool: tool.into(), args: vec![], preview: false, by, ticket: Ticket(8) });
        let refused = engine.step(mailbox.take(), &mut |_| {});
        assert!(matches!(refused.outcomes.as_slice(), [(Ticket(8), Outcome::Refused(refusal))] if refusal.text == message),
            "{tool}: unknown prefixes, segment boundaries and exact-prefix errors answer the ticket");
    }

    mailbox.push(invoke(9, true));
    mailbox.push(invoke(10, true));
    let previewed = engine.step(mailbox.take(), &mut |_| {});
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.preview_by, Some(by), "the tool preview carries its AI author");
    assert!(matches!(previewed.outcomes.as_slice(), [(Ticket(9), Outcome::Refused(_)), (Ticket(10), Outcome::Previewed { tag })]
        if Some(*tag) == snapshot.preview_tag), "a replaced waiting call is answered and the live call gets the snapshot's tag");
    assert_eq!(engine.log().entries.len(), 2, "preview calls add no history entries");

    let board = engine.design().units[0].board.as_ref().expect("the committed board remains");
    let mut added = via;
    added.id = ItemId(board.next_item);
    let created = Sel::Item(unit, added.id);
    mailbox.preview(Sample {
        seq: 100, tag: 100, ops: vec![DesignOp::Board { unit, op: BoardOp::Add(Item::Via(added)) }],
        place: vec![], label: "pane add".into(), by: By::Person, proposal: None, notes: vec![], circuits: vec![], expect: None,
    });
    let step = engine.step(mailbox.take(), &mut |_| {});
    assert!(step.rejected.is_empty(), "a pane sample replaces the live tool preview");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.preview_by, Some(By::Person), "the replacing pane sample has person authorship");
    assert_eq!(snapshot.created, vec![created], "the live board preview identifies the item its Add op applied");
    mailbox.push(invoke(11, true));
    engine.step(mailbox.take(), &mut |_| {});
    assert!(engine.snapshot().created.is_empty(), "a move preview clears the replaced Add preview's created items");
}

/// Why: route jobs need the exact committed fills, including edits earlier in their invoke's step,
/// and replacing a parked job must answer its caller without starting stale work.
#[test]
fn a_route_start_waits_for_exact_fills() {
    let design = ledctl();
    let unit = design.units[0].id;
    let via = design.units[0].board.as_ref().expect("the route fixture has a board").vias[0].id;
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "route".into())
        .with_resolver("elec.route", route_plan);
    let mailbox = Mailbox::default();
    let by = By::Ai { turn: 2, call: 3 };
    let invoke = |ticket, preview| Cmd::Invoke { tool: "elec.route".into(), args: vec![], preview, by, ticket: Ticket(ticket) };
    mailbox.push(Cmd::Commit {
        ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item: via, by: P::mm(0.3, -0.2) } }],
        place: vec![], label: "move via in pour".into(), circuits: vec![], expect: None, by: By::Person, ticket: None,
    });
    mailbox.push(invoke(1, false));
    let pending = engine.step(mailbox.take(), &mut |_| {});
    assert!(!pending.fills.is_empty(), "the committed via move leaves fills pending");
    assert!(pending.routes.is_empty(), "a route invoked in the edit's step waits for its fills");
    assert!(pending.outcomes.is_empty(), "a parked route keeps its caller pending");
    assert!(engine.snapshot().job.is_none(), "the engine leaves job state to the Driver overlay");

    mailbox.push(invoke(2, true));
    let replaced = engine.step(mailbox.take(), &mut |_| {});
    assert!(replaced.routes.is_empty(), "the newer route also waits for exact committed fills");
    assert!(matches!(replaced.outcomes.as_slice(), [(Ticket(1), Outcome::Refused(refusal))]
        if refusal.text == "replaced by a newer route" && refusal.sample == Some(fab_elec_route::JobId(1).tag())),
        "replacing a parked route refuses exactly the old ticket under its job tag");
    assert!(replaced.fills.is_empty(), "a replacement reuses the already running committed fills");

    let hash = engine.design().unit(unit).expect("the committed unit remains").board.as_ref()
        .expect("the committed unit retains its board").content_hash();
    for (i, fill) in pending.fills.iter().enumerate() {
        mailbox.result(fill.run());
        let step = engine.step(mailbox.take(), &mut |_| {});
        assert!(step.outcomes.is_empty(), "merging fills and starting a route keep its caller pending");
        if i + 1 < pending.fills.len() {
            assert!(step.routes.is_empty(), "a route remains parked until the final committed fill merges");
        } else {
            assert_eq!(step.routes.len(), 1, "the final fill merge starts exactly one route");
            let start = &step.routes[0];
            assert_eq!(start.ticket, Ticket(2), "only the newest parked route starts");
            assert_eq!(start.by, by, "the route start preserves the caller's author");
            assert_eq!(start.job.id(), fab_elec_route::JobId(2), "the strand numbers each resolved job");
            assert!(start.base.exact, "the route start carries exact committed fills");
            assert_eq!(start.base.hash, hash, "the route base hash is the committed board's content hash");
            assert_eq!(start.base.board.content_hash(), hash, "the route base board matches its committed hash");
        }
    }
    let idle = engine.step(mailbox.take(), &mut |_| {});
    assert!(idle.routes.is_empty() && idle.outcomes.is_empty(), "a started route is neither restarted nor refused on an idle step");
    mailbox.push(Cmd::RefusePreview { seq: 1, tag: fab_elec_route::JobId(2).tag(), reason: "the route worker could not start".into() });
    let refused = engine.step(mailbox.take(), &mut |_| {});
    assert!(matches!(refused.outcomes.as_slice(), [(Ticket(2), Outcome::Refused(refusal))]
        if refusal.text == "the route worker could not start"),
        "a route refusal answers its retained ticket even before any proposal");
}

#[test]
fn case_findings_join_the_checks_and_follow_their_unit() {
    let mailbox = Mailbox::default();
    let mut design = ledctl();
    let unit = BoardId(0);
    let board = Arc::make_mut(design.units[0].board.as_mut().expect("the fixture unit has a board"));
    // Start with a committed generation that has an older generation to test against.
    board.apply(&BoardOp::SetRules(board.rules.clone())).expect("the fixture rules can be committed");
    let generation = board.generation;
    let hole = board.holes.first().expect("the fixture has a hole");
    let finding = |rule| {
        let mut v = Violation::new(
            CheckKind::Mech, Severity::Error, Some(unit), vec![Sel::Feature(FeatureRef::Hole(unit, hole.id))],
            rule, format!("{rule} at the fixture hole"),
        );
        v.id = content_id(board, &v.kind, &v.items, &v.rule);
        v
    };
    let mut findings = vec![finding("case.fit"), finding("case.reach")];
    findings.sort_by_key(|v| std::cmp::Reverse(v.id));
    let steps = Arc::new(AtomicU64::new(40));
    let logged = Arc::new(AtomicU64::new(40));
    let mut engine = Engine::new(design, Arc::new(Factories::new()), steps.clone(), logged.clone(), "fixture".into());
    let initial = engine.snapshot();
    let case_findings = |snapshot: &DesignSnapshot| {
        snapshot.violations.value.iter().filter(|v| v.kind == CheckKind::Mech && v.unit == Some(unit)).cloned().collect::<Vec<_>>()
    };
    let mut published = Vec::new();

    mailbox.push(Cmd::CaseFindings { unit, generation, run: 1, cased: true, findings: findings.clone() });
    let joined = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(joined.rejected.is_empty(), "case findings are accepted for the committed board generation");
    let both = published.last().expect("accepted case findings publish a snapshot").clone();
    assert!(findings.iter().all(|v| both.violations.value.contains(v)), "both case findings join the Checks list");
    assert_eq!(case_findings(&both).len(), 2, "the Checks list has exactly the two case findings");
    assert!(both.violations.value.windows(2).all(|w| w[0].id < w[1].id), "the combined Checks list is sorted and unique by id");
    assert!(initial.violations.value.iter().all(|v| both.violations.value.contains(v)), "every previous DRC, ERC and system ERC finding remains in Checks");
    assert_eq!(both.violations.of, initial.violations.of, "case findings preserve the violations state tag");
    assert_eq!(both.violations.generation, initial.violations.generation, "case findings preserve the violations generation tag");
    assert!(engine.log().entries.is_empty(), "case findings do not enter the op log");
    assert_eq!((joined.undo, joined.redo), (None, None), "case findings do not create an undo or redo step");
    assert_eq!(steps.load(SeqCst), 40, "case findings do not allocate a project step number");
    assert!(initial.cases.is_empty(), "a board not case checked yet has no held case run");
    assert_eq!(*both.cases, vec![CaseHeld { unit, generation, run: 1, cased: true }], "the snapshot names the case run its case findings come from");
    published.clear();

    mailbox.push(Cmd::CaseFindings { unit, generation: generation - 1, run: 1, cased: true, findings: vec![] });
    engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(engine.snapshot().violations.value, both.violations.value, "an older case result changes no Checks findings");
    assert!(published.is_empty(), "an ignored older case result publishes no change");

    mailbox.preview(Sample { tag: 0, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: None, seq: 1, ops: vec![], label: "preview with held case checks".into() });
    engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    let preview = published.last().expect("the preview publishes a snapshot");
    assert!(preview.preview.is_some(), "the case findings are checked while a preview is live");
    assert_eq!(case_findings(preview), case_findings(&both), "a live preview keeps the committed case findings");
    assert_eq!(preview.violations.of, StateOf::Preview, "case findings preserve the preview violations tag");
    published.clear();

    mailbox.push(Cmd::CaseFindings { unit, generation: generation + 1, run: 1, cased: true, findings: vec![findings[0].clone()] });
    engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    let replaced = published.last().expect("newer case findings publish a snapshot");
    assert_eq!(case_findings(replaced), vec![findings[0].clone()], "a newer case result replaces both findings with exactly one");
    assert_eq!(replaced.cases.first().map(|held| held.generation), Some(generation + 1), "the held case run follows the newer result");
    assert!(replaced.preview.is_some(), "a case result leaves the live preview in place");
    published.clear();

    let mut rules = engine.design().unit(unit).expect("the unit remains present").board.as_ref().expect("the unit keeps its board").rules.clone();
    rules.waivers.push(Waiver { violation: findings[0].id, reason: "accepted case fit".into() });
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Board { unit, op: BoardOp::SetRules(rules) }], label: "waive case finding".into(), circuits: vec![], expect: None,
    });
    let waived = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(waived.rejected.is_empty(), "the board rules accept a content-id waiver");
    assert!(published.iter().all(|s| case_findings(s).is_empty()), "a committed waiver clears the held case finding at once, before the next case rebuild");
    published.clear();
    mailbox.push(Cmd::CaseFindings { unit, generation: generation + 1, run: 1, cased: true, findings: vec![findings[0].clone()] });
    engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(case_findings(published.last().expect("the case result publishes a snapshot")).is_empty(), "a case result with the waived content id leaves no case finding");
    assert_eq!(engine.log().entries.len(), 1, "only the SetRules commit enters history");
    published.clear();

    mailbox.push(Cmd::CaseFindings { unit, generation: generation + 2, run: 1, cased: true, findings: vec![findings[1].clone(), findings[1].clone()] });
    engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(case_findings(published.last().expect("the unwaived case result publishes")), vec![findings[1].clone()], "a different unwaived content id remains visible exactly once even when repeated");
    published.clear();
    mailbox.push(Cmd::CaseFindings { unit, generation: generation + 3, run: 1, cased: true, findings: vec![] });
    engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(case_findings(published.last().expect("the empty case result publishes")).is_empty(), "an empty result at a newer generation clears the remaining case finding");
}

/// A board drag never publishes a fill older than the one before it (the pour blinked back to the
/// committed one at every sample and at the drop): a sample continues from the fills the one
/// before it merged, and the drop takes the preview's results over, so it reverts no fill and
/// refills nothing the preview filled.
#[test]
fn a_drag_never_shows_an_older_fill_and_its_drop_refills_nothing() {
    let design = ledctl();
    let unit = BoardId(0);
    // The part the reported drag moved: its pads sit in the pour.
    let part = design.unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board")
        .placed.iter().find(|placed| placed.refdes == "SW1").expect("ledctl places SW1").id;
    let sample = |seq: u64, by| Sample {
        tag: 0, by: By::Person, place: vec![], proposal: None, notes: vec![], circuits: vec![], expect: None,
        seq, ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item: part, by } }], label: format!("drag sample {seq}"),
    };
    let factories = Arc::new(Factories::new());
    let mut engine = Engine::new(design, factories.clone(), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into());
    let mailbox = Mailbox::default();
    // What a board view draws: the live preview's fills, else the committed ones.
    let shown = |s: &DesignSnapshot| s.of(&s.fills, unit).expect("every snapshot has the board's fills").value.clone();
    let run = |jobs: &[FillJob]| {
        for job in jobs {
            mailbox.result(job.run());
        }
    };
    let mut published: Vec<Arc<DesignSnapshot>> = Vec::new();
    // Build a sample (or take a commit) and run its fills: the last publish, and the step's jobs.
    let settle = |engine: &mut Engine, published: &mut Vec<Arc<DesignSnapshot>>| {
        let step = engine.step(mailbox.take(), &mut |s| published.push(s));
        run(&step.fills);
        engine.step(mailbox.take(), &mut |s| published.push(s));
        published.last().expect("a merged fill publishes").clone()
    };
    let committed = shown(&engine.snapshot());

    mailbox.preview(sample(1, P::mm(1.0, 0.0)));
    let one = settle(&mut engine, &mut published);
    let one_full = engines::full_with(one.preview.as_ref().expect("sample 1 is live"), &factories);
    assert_eq!(shown(&one), one_full.boards[0].fills, "sample 1's refill gives its full fills");
    assert_ne!(shown(&one), committed, "sample 1 changes the pour");
    published.clear();

    // Sample 2 continues from sample 1's fills until its own refill lands.
    mailbox.preview(sample(2, P::mm(2.0, 0.0)));
    let second = engine.step(mailbox.take(), &mut |s| published.push(s));
    assert!(!second.fills.is_empty() && second.fills.iter().all(|job| job.preview == Some(2)), "sample 2 refills for the preview");
    assert!(!published.is_empty() && published.iter().all(|s| shown(s) == shown(&one)), "until its refill, sample 2 shows sample 1's fills, never the committed ones");
    run(&second.fills);
    engine.step(mailbox.take(), &mut |s| published.push(s));
    let two = published.last().expect("a merged fill publishes").clone();
    let two_full = engines::full_with(two.preview.as_ref().expect("sample 2 is live"), &factories);
    assert_eq!(shown(&two), two_full.boards[0].fills, "a continued sample's refill gives its full fills");
    assert_eq!(two.of(&two.conn, unit).expect("the preview has connectivity").value, two_full.boards[0].conn, "and its full connectivity");
    assert_eq!(*two.violations.value, engines::violations(&two_full), "and its full checks");
    published.clear();

    // A drop before its sample's refill landed keeps that sample's held fills.
    mailbox.preview(sample(3, P::mm(2.0, 1.0)));
    engine.step(mailbox.take(), &mut |_| {});
    mailbox.push(Cmd::CommitPreview { seq: 3, tag: 0 });
    let dropped = engine.step(mailbox.take(), &mut |s| published.push(s));
    assert!(!published.is_empty() && published.iter().all(|s| s.preview.is_none() && shown(s) == shown(&two)), "the drop shows sample 3's held fills, never the committed ones");
    assert!(!dropped.fills.is_empty() && dropped.fills.iter().all(|job| job.preview.is_none()), "what sample 3 still owed refills for the committed results");
    run(&dropped.fills);
    engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(engine.results(), &engines::full_with(engine.design(), &factories), "the taken-over results and their refill equal full results");
    published.clear();

    // A drop after its sample's refill landed refills nothing.
    mailbox.preview(sample(4, P::mm(-1.0, 0.5)));
    let four = settle(&mut engine, &mut published);
    published.clear();
    mailbox.push(Cmd::CommitPreview { seq: 4, tag: 0 });
    let dropped = engine.step(mailbox.take(), &mut |s| published.push(s));
    assert!(dropped.fills.is_empty(), "a drop after the preview's refill starts no fill");
    assert!(!published.is_empty() && published.iter().all(|s| s.preview.is_none() && shown(s) == shown(&four)), "the drop shows the preview's fills");
    assert_eq!(engine.results(), &engines::full_with(engine.design(), &factories), "the taken-over results equal full results");
    published.clear();

    // A drop whose last sample is still in the slot continues from the live preview as well.
    mailbox.preview(sample(5, P::mm(0.0, -1.0)));
    let five = settle(&mut engine, &mut published);
    published.clear();
    mailbox.preview(sample(6, P::mm(0.5, -2.0)));
    mailbox.push(Cmd::CommitPreview { seq: 6, tag: 0 });
    let dropped = engine.step(mailbox.take(), &mut |s| published.push(s));
    assert!(!published.is_empty() && published.iter().all(|s| s.preview.is_none() && shown(s) == shown(&five)), "a drop from the slot shows the live preview's fills");
    run(&dropped.fills);
    engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(engine.results(), &engines::full_with(engine.design(), &factories), "a drop from the slot and its refill equal full results");
    assert_eq!(engine.log().entries.len(), 3, "each drop is one undo step");
    published.clear();

    // While fills lag, a sample continues from where the one before it started, so the debt
    // its refill reads does not grow with the drag.
    mailbox.preview(sample(7, P::mm(1.0, 1.0)));
    settle(&mut engine, &mut published);
    mailbox.preview(sample(8, P::mm(2.0, 2.0)));
    let eighth = engine.step(mailbox.take(), &mut |_| {});
    mailbox.preview(sample(9, P::mm(3.0, 3.0)));
    let ninth = engine.step(mailbox.take(), &mut |_| {});
    assert!(!eighth.fills.is_empty() && !ninth.fills.is_empty(), "both lagging samples refill");
    assert_eq!(ninth.fills[0].delta.extents.len(), eighth.fills[0].delta.extents.len(), "a lagging sample's debt does not grow");
    published.clear();
    run(&ninth.fills);
    engine.step(mailbox.take(), &mut |s| published.push(s));
    let nine = published.last().expect("a merged fill publishes");
    let nine_full = engines::full_with(nine.preview.as_ref().expect("sample 9 is live"), &factories);
    assert_eq!(shown(nine), nine_full.boards[0].fills, "a sample continued past a lagging one refills to its full fills");

    // Undo of a taken-over commit and its refill equal full results.
    mailbox.push(Cmd::CancelPreview { seq: 9 });
    mailbox.push(Cmd::Undo);
    let undone = engine.step(mailbox.take(), &mut |_| {});
    run(&undone.fills);
    engine.step(mailbox.take(), &mut |_| {});
    assert_eq!(engine.results(), &engines::full_with(engine.design(), &factories), "undo of a taken-over commit and its refill equal full results");
}

/// Run `job` tiled with its spawned tasks in reverse order, cancelling first when asked; returns
/// every result `done` received and the number of spawned tasks.
fn run_reversed(job: FillJob, cancel: bool) -> (Vec<TaskResult>, usize) {
    let results = Arc::new(Mutex::new(Vec::new()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut tasks: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
    let (sink, flag) = (results.clone(), cancelled.clone());
    job.run_tiled(
        move || flag.load(SeqCst),
        |task| {
            tasks.push(task);
            true
        },
        move |result| sink.lock().unwrap().push(result),
    );
    cancelled.store(cancel, SeqCst);
    let spawned = tasks.len();
    for task in tasks.into_iter().rev() {
        task();
    }
    let results = std::mem::take(&mut *results.lock().unwrap());
    (results, spawned)
}

/// Tiled fill jobs finish exactly once, from whichever task stores the last tile, with the serial
/// result; a job cancelled while its tiles are queued never finishes.
#[test]
fn tiled_fill_jobs_match_serial() {
    let design = ledctl();
    let unit = BoardId(0);
    let results = engines::full(&design);
    let held = &results.boards[0];
    let board = design.unit(unit).expect("the fixture has unit 0").board.clone().expect("unit 0 has a board");
    let full: Vec<FillJob> = held.fills.iter().map(|prev| FillJob {
        task: FillTask { unit, zone: prev.zone, generation: board.generation },
        board: board.clone(),
        rules: held.rules.clone(),
        index: held.index.0.clone(),
        prev: prev.clone(),
        delta: BoardDelta { global: true, bbox: board.outline_bbox(), ..Default::default() },
        preview: None,
    }).collect();
    assert!(!full.is_empty(), "the fixture has zones to refill");

    let via = board.vias[0].clone();
    let factories = Arc::new(Factories::new());
    let mut engine = Engine::new(design.clone(), factories, Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "fixture".into());
    let mailbox = Mailbox::default();
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item: via.id, by: P::mm(0.3, -0.2) } }],
        label: "move via".into(),
        circuits: vec![], expect: None,
    });
    let regional = engine.step(mailbox.take(), &mut |_| {}).fills;
    assert!(!regional.is_empty(), "moving the fixture via owes a zone refill");

    for (kind, jobs) in [("full", full), ("regional", regional)] {
        for job in jobs {
            let TaskResult::Fill { fill: serial, .. } = job.run() else { panic!("{kind}: a fill job gives a fill result") };
            let tiles = job.plan().len();
            let (finished, spawned) = run_reversed(job.clone(), false);
            assert_eq!(spawned + 1, tiles.max(1), "{kind}: every tile but the inline last one is spawned");
            if kind == "full" {
                assert!(spawned > 0, "{kind}: a full refill runs as several tasks");
            }
            assert_eq!(finished.len(), 1, "{kind}: the job finishes exactly once");
            let TaskResult::Fill { unit: u, zone, generation, rules, fill, preview } = &finished[0] else { panic!("{kind}: a tiled fill job gives a fill result") };
            assert_eq!(fill, &serial, "{kind}: tiles in reverse order assemble the serial fill");
            assert_eq!(
                (*u, *zone, *generation, *preview),
                (job.task.unit, job.task.zone, job.task.generation, job.preview),
                "{kind}: the tiled result names the job's zone, generation and preview",
            );
            assert!(Arc::ptr_eq(rules, &job.rules), "{kind}: the tiled result keeps the job's rules");
            if spawned > 0 {
                let (finished, _) = run_reversed(job, true);
                assert!(finished.is_empty(), "{kind}: a job cancelled with queued tiles never finishes");
            }
        }
    }
}

#[test]
fn net_length_reads_pairs_groups_and_requested_nets_from_committed_copper() {
    use makepad_widgets_core::makepad_script::syntax::{self, Node};

    let mut docs = fab_elec_lang::Sources::new();
    docs.add("lengths.ckt.splash", r#"
use mod.elec.*
let e = mod.elec
let p = e.net({name: "P"})
let n = e.net({name: "N"})
let q = e.net({name: "Q"})
let x = e.net({name: "X"})
let a = e.r(1kohm, q, x)
let b = e.r(1kohm, q, x)
let c = e.r(1kohm, q, x)
e.pair(p, n, {skew: 0.1mm})
e.match_length([n, p, q], {tol: 0.25mm})
"#).unwrap();
    docs.add("lengths.pcb.splash", r#"board("lengths")
outline([[[0mm, 0mm], [24mm, 0mm], [24mm, 24mm], [0mm, 24mm], [0mm, 0mm]]])
track("P", "F.Cu", 0.2mm, [[1mm, 1mm], [6mm, 1mm]])
track("N", "F.Cu", 0.2mm, [[1mm, 3mm], [5mm, 3mm]])
track("Q", "F.Cu", 0.2mm, [[1mm, 5mm], [3mm, 5mm]])
track("Q", "F.Cu", 0.2mm, [[4mm, 5mm], [5mm, 5mm]])
place("a", [3mm, 16mm])
place("b", [8mm, 16mm])
place("c", [13mm, 16mm])
"#).unwrap();
    let ev = fab_elec_lang::evaluate(&docs, &fab_elec_lang::std_libs()).unwrap();
    let engine = Engine::new(ev.design, Arc::new(Factories::new()), Arc::default(), Arc::default(), "lengths".into());
    let mut snap = (*engine.snapshot()).clone();
    // A live preview with no tracks must not replace committed lengths in a read.
    let mut preview = (*snap.design).clone();
    Arc::make_mut(preview.units[0].board.as_mut().unwrap()).tracks.clear();
    snap.preview = Some(Arc::new(preview));
    fn field<'a>(node: &'a Node, key: &str) -> &'a Node { &node.field(key).unwrap().value }
    let answer = Engine::net_length(&snap, &[("unit".into(), "lengths".into())]).unwrap();
    let doc = syntax::parse(&answer).unwrap();
    let obj = &doc.statements()[0];
    assert_eq!(field(obj, "measurement").string(), Some("track length plus via barrels"));
    let nets = field(obj, "nets").items().unwrap();
    assert_eq!(nets.len(), 3);
    let row = |name| nets.iter().find(|row| field(row, "net").string() == Some(name)).unwrap();
    let n = row("N");
    assert_eq!(field(n, "length_nm").number(), Some(4_000_000.0));
    assert_eq!(field(n, "partial").ident(), Some("false"));
    assert_eq!(field(n, "branched").ident(), Some("false"));
    let matches = field(n, "matches").items().unwrap();
    assert_eq!(matches.len(), 2);
    for (i, tol, status) in [(0, 100_000.0, "broken"), (1, 250_000.0, "partial")] {
        assert_eq!(field(&matches[i], "reference").string(), Some("P"));
        assert_eq!(field(&matches[i], "difference_nm").number(), Some(1_000_000.0));
        assert_eq!(field(&matches[i], "tolerance_nm").number(), Some(tol));
        assert_eq!(field(&matches[i], "status").name(), Some(status));
    }
    let q = row("Q");
    assert_eq!(field(q, "partial").ident(), Some("true"));
    assert_eq!(field(q, "branched").ident(), Some("true"));
    assert_eq!(field(q, "length_nm").number(), Some(3_000_000.0));
    let answer = Engine::net_length(&snap, &[("nets".into(), "[X]".into())]).unwrap();
    let doc = syntax::parse(&answer).unwrap();
    let nets = field(&doc.statements()[0], "nets").items().unwrap();
    assert_eq!(nets.len(), 1);
    assert_eq!(field(&nets[0], "net").string(), Some("X"));
    assert!(field(&nets[0], "matches").items().unwrap().is_empty());
    assert!(Engine::net_length(&snap, &[("nets".into(), "missing".into())]).is_err());
    let board = Arc::make_mut(&mut snap.design).units[0].board.as_mut().unwrap();
    Arc::make_mut(board).stackup.layers.clear();
    let answer = Engine::net_length(&snap, &[]).unwrap();
    let doc = syntax::parse(&answer).unwrap();
    assert_eq!(field(&doc.statements()[0], "measurement").string(), Some("vias not counted: no stackup"));
}
