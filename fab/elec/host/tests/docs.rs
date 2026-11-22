use fab_elec_board::zone::fill_all;
use fab_elec_board::{connect, resolve_rules, Board, BoardOp, Factories, Field, Item};
use fab_elec_core::{BoardId, Canon, CompRef, Diag, ItemId, Sel, P};
use fab_elec_design::{By, Design, DesignOp};
use fab_elec_host::{Cmd, Docs, Engine, Mailbox, Outcome, Plan, Ticket};
use fab_elec_lang::{board_doc::{self, Ledger}, write};
use fab_elec_route::auto::GridRouter;
use fab_elec_route::{problem, scope_nets, Autorouter, Base, Effort, Progress, RBase, RouteScope, Style};
use fab_elec_view::DesignSnapshot;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

fn check(engine: &Engine, published: &[Arc<DesignSnapshot>], what: &str) {
    let docs = engine.docs().expect("document-backed edits retain their documents");
    let doc = docs.sources().find("ledctl.pcb.splash").expect("the board document remains present");
    let text = docs.text(doc).expect("the board document retains its text");
    let unit = engine.design().unit(BoardId(0)).expect("the committed design retains unit 0");
    let board = unit.board.as_ref().expect("the committed unit retains its board");
    assert_eq!(text.as_ref(), write(board, &unit.circuit), "{what}: committed board text equals the canonical writer");

    let ledger = Ledger::of_map(docs.map(), unit.id, doc, board.next_item);
    let lowered = board_doc::lower(doc, text, &unit.circuit, Some(&ledger))
        .unwrap_or_else(|diags| panic!("{what}: committed text lowers with its source-map ledger: {diags:?}"));
    assert_eq!(lowered.board.content_hash(), board.content_hash(), "{what}: ledger lowering preserves the committed board content hash");
    let ids = |board: &Board| [
        ("placed", board.placed.iter().map(|item| item.id).collect::<Vec<_>>()),
        ("tracks", board.tracks.iter().map(|item| item.id).collect::<Vec<_>>()),
        ("vias", board.vias.iter().map(|item| item.id).collect::<Vec<_>>()),
        ("zones", board.zones.iter().map(|item| item.id).collect::<Vec<_>>()),
        ("keepouts", board.keepouts.iter().map(|item| item.id).collect::<Vec<_>>()),
        ("holes", board.holes.iter().map(|item| item.id).collect::<Vec<_>>()),
        ("cutouts", board.cutouts.iter().map(|item| item.id).collect::<Vec<_>>()),
        ("graphics", board.graphics.iter().map(|item| item.id).collect::<Vec<_>>()),
        ("texts", board.texts.iter().map(|item| item.id).collect::<Vec<_>>()),
        ("tunings", board.tunings.iter().map(|item| item.id).collect::<Vec<_>>()),
    ];
    for ((name, actual), (_, expected)) in ids(&lowered.board).into_iter().zip(ids(board)) {
        assert_eq!(actual, expected, "{what}: ledger lowering preserves {name} item ids and list order");
    }

    assert!(!published.is_empty(), "{what}: the step publishes a snapshot");
    for snapshot in published {
        let map = snapshot.source_map.as_ref().expect("published document-backed snapshots carry a source map");
        assert!(Arc::ptr_eq(map, docs.map()), "{what}: the published snapshot shares the current source map");
        let view = snapshot.docs.as_ref().expect("published document-backed snapshots carry documents");
        let shown = view.texts.iter().find(|text| text.doc == doc).expect("published documents include the board text");
        assert_eq!(&shown.text, text, "{what}: the published board text equals the current document text");
    }
}

#[test]
fn an_added_board_item_answers_with_its_written_statement() {
    let sources = fab_elec_fixtures::docs("ledctl").expect("the fixture supplies controller documents");
    let (design, docs) = Docs::from_evaluated(sources, fab_elec_fixtures::ledctl_eval());
    let unit = &design.units[0];
    let board = unit.board.as_ref().expect("the fixture has a board");
    let mut via = board.vias[0].clone();
    via.id = ItemId(board.next_item);
    via.at = P::mm(2.0, 3.0);
    let sel = Sel::Item(unit.id, via.id);
    let op = DesignOp::Board { unit: unit.id, op: BoardOp::Add(Item::Via(via)) };
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "created".into())
        .with_docs(docs);
    let mailbox = Mailbox::default();
    mailbox.push(Cmd::Commit {
        ops: vec![op], place: vec![], label: "add via".into(), circuits: vec![], expect: None, by: By::Person, ticket: Some(Ticket(1)),
    });
    let step = engine.step(mailbox.take(), &mut |_| {});
    assert!(step.rejected.is_empty(), "the board item addition is accepted");
    let [(Ticket(1), Outcome::Done { seq, created, sites, .. })] = step.outcomes.as_slice() else {
        panic!("the board item addition answers its caller exactly once with Done")
    };
    assert_eq!(created, &[sel.clone()], "Done identifies the item whose statement was added");
    let docs = engine.docs().expect("the commit keeps its documents");
    let range = docs.map().locate(&sel).expect("the new item is located in the source map");
    assert_eq!(sites, &[Sel::Source { doc: range.doc, start: range.span.start, end: range.span.end }],
        "Done supplies the new statement's complete source range");
    let text = docs.text(range.doc).expect("the new item's document is retained");
    let statement = &text[range.span.start as usize..range.span.end as usize];
    let unit = &engine.design().units[0];
    let canonical = write(unit.board.as_ref().expect("the board remains"), &unit.circuit);
    assert!(statement.starts_with("via(") && canonical.lines().any(|line| line == statement),
        "the reported source range contains exactly the new via statement");
    let snapshot = engine.snapshot();
    let committed = snapshot.last_commit.as_ref().expect("the snapshot retains the board commit");
    assert_eq!(committed.seq, *seq, "the snapshot and Done refer to the same log entry");
    assert_eq!(&committed.created, created, "the snapshot and Done carry the same created selections");

    fn located_error(design: &Design, _docs: Option<&Docs>, _tool: &str, _args: &[(String, String)]) -> Result<Plan, Diag> {
        let unit = &design.units[0];
        let item = unit.board.as_ref().expect("the resolver fixture has a board").vias[0].id;
        Err(Diag::error("this via cannot be moved", vec![Sel::Item(unit.id, item)]))
    }
    let first = engine.design().units[0].board.as_ref().expect("the board remains").vias[0].id;
    let site = docs.site(&Sel::Item(unit.id, first));
    let mut engine = engine.with_resolver("test.fail", located_error);
    mailbox.push(Cmd::Invoke { tool: "test.fail.via".into(), args: vec![], preview: false, by: By::Ai { turn: 1, call: 2 }, ticket: Ticket(2) });
    let refused = engine.step(mailbox.take(), &mut |_| {});
    assert!(matches!(refused.outcomes.as_slice(), [(Ticket(2), Outcome::Refused(refusal))]
        if refusal.text == "this via cannot be moved" && refusal.site == site),
        "a resolver error answers with its message and its document-located site");

    // A Library part dropped on a pin is written after that pin's part, so its evaluation gives it
    // an earlier part's id: Done still names it as made, by its path (elec.part.add plans the drop).
    let boot = engine.design().units[0].circuit.comp("mcu.boot_r").expect("the fixture has the boot resistor").refdes.clone();
    let args = [("part".to_string(), "r".to_string()), ("ends".to_string(), format!("{boot}.1"))];
    let Ok(Plan::Edit(drop)) = fab_elec_host::sheet_actions::part_add(engine.design(), engine.docs(), "elec.part.add", &args) else {
        panic!("a resistor dropped on the boot resistor's pin plans one part addition");
    };
    let before: Vec<String> = engine.design().units[0].circuit.comps.iter().map(|comp| comp.path.clone()).collect();
    mailbox.push(Cmd::Commit { ops: drop.ops, place: drop.place, label: drop.label, circuits: vec![], expect: None, by: By::Person, ticket: Some(Ticket(3)) });
    let mut step = engine.step(mailbox.take(), &mut |_| {});
    while step.outcomes.is_empty() && !step.evals.is_empty() {
        for job in step.evals {
            mailbox.result(job.run());
        }
        step = engine.step(mailbox.take(), &mut |_| {});
    }
    let unit = &engine.design().units[0];
    let added = unit.circuit.comps.iter().find(|comp| !before.contains(&comp.path)).expect("the drop adds one part");
    assert!(added.id.ix() + 1 < unit.circuit.comps.len(), "the dropped part lands mid-circuit, under an id another part had");
    let [(Ticket(3), Outcome::Done { created, .. })] = step.outcomes.as_slice() else {
        panic!("the drop answers its caller once with Done")
    };
    assert!(created.contains(&Sel::Comp(CompRef { board: unit.id, comp: added.id })), "Done names the dropped part as made");
}

#[test]
fn board_commits_splice_the_text_and_undo_restores_the_bytes() {
    let sources = fab_elec_fixtures::docs("ledctl").expect("the fixture supplies ledctl documents");
    let (design, docs) = Docs::from_evaluated(sources, fab_elec_fixtures::ledctl_eval());
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "docs".into()).with_docs(docs);
    let mailbox = Mailbox::default();
    let unit = BoardId(0);
    let doc = engine.docs().expect("the engine has documents").sources().find("ledctl.pcb.splash")
        .expect("the fixture supplies the board document");
    let texts = |engine: &Engine| engine.docs().expect("the engine retains its documents").sources().docs().iter()
        .map(|doc| (doc.id, doc.text.clone())).collect::<Vec<_>>();
    let loaded_texts = texts(&engine);
    let loaded_map = engine.docs().expect("the engine has documents").map().as_ref().clone();
    let board = engine.design().unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board");
    let loaded_hash = board.content_hash();
    let part = board.placed.first().expect("the fixture has a placed part").clone();
    let before_move = engine.docs().expect("the engine has documents").text(doc).expect("the board has text").clone();
    let mut published = Vec::new();

    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item: part.id, by: P::mm(1.0, 0.0) } }],
        label: "move first part".into(), circuits: vec![], expect: None,
    });
    let moved = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(moved.rejected.is_empty(), "moving the first part is accepted: {:?}", moved.rejected);
    check(&engine, &published, "move first part");
    let after_move = engine.docs().expect("the engine retains documents").text(doc).expect("the board retains text");
    let before_lines: Vec<_> = before_move.lines().collect();
    let after_lines: Vec<_> = after_move.lines().collect();
    assert_eq!(after_lines.len(), before_lines.len(), "moving a part preserves the board document's line count");
    let changed: Vec<_> = before_lines.iter().zip(&after_lines).filter(|(before, after)| before != after).collect();
    assert_eq!(changed.len(), 1, "moving a part changes exactly one board-document line");
    let place = format!("place({:?},", part.path);
    assert!(changed[0].0.starts_with(&place) && changed[0].1.starts_with(&place), "the sole changed line is the moved part's place statement and path");
    let sent = &moved.docs.as_ref().expect("a commit that changed the text gives the plate its documents").docs;
    assert_eq!(sent.len(), loaded_texts.len() - 1, "the plate gets every project document, not only the changed one");
    assert!(!sent.contains_key("ledctl.sys.splash"), "an empty system document is no file of the plate's");
    assert!(sent.get("ledctl.pcb.splash").is_some_and(|text| Arc::ptr_eq(text, after_move)), "the plate's board document is the engine's text itself");
    published.clear();

    let before_vias = after_move.lines().filter(|line| line.starts_with("via(")).count();
    let board = engine.design().unit(unit).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board");
    let mut via = board.vias.first().expect("the fixture has a via").clone();
    via.id = ItemId(board.next_item);
    via.at = board.outline_bbox().min.add(P::mm(2.0, 3.0));
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Board { unit, op: BoardOp::Add(Item::Via(via)) }],
        label: "add via".into(), circuits: vec![], expect: None,
    });
    let added = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(added.rejected.is_empty(), "adding a via is accepted: {:?}", added.rejected);
    check(&engine, &published, "add via");
    let after_add = engine.docs().expect("the engine retains documents").text(doc).expect("the board retains text");
    assert_eq!(after_add.lines().filter(|line| line.starts_with("via(")).count(), before_vias + 1, "adding a via adds exactly one via statement");
    published.clear();

    let before_tracks = after_add.lines().filter(|line| line.starts_with("track(")).count();
    let board = engine.design().unit(unit).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board");
    let track = board.tracks.first().expect("the fixture has a track").id;
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Board { unit, op: BoardOp::Remove(track) }],
        label: "remove first track".into(), circuits: vec![], expect: None,
    });
    let removed = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(removed.rejected.is_empty(), "removing the first track is accepted: {:?}", removed.rejected);
    check(&engine, &published, "remove first track");
    let after_remove = engine.docs().expect("the engine retains documents").text(doc).expect("the board retains text");
    assert_eq!(after_remove.lines().filter(|line| line.starts_with("track(")).count(), before_tracks - 1, "removing a track removes exactly one track statement");
    published.clear();

    let board = engine.design().unit(unit).expect("unit 0 remains present").board.clone().expect("unit 0 keeps its board");
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let base = Base::new(board, rules, conn, fills);
    // Two of the open nets: a router commit of many ops in one command, at a test's cost.
    let mut nets = scope_nets(&base.board, &base.conn, &RouteScope::All);
    nets.truncate(2);
    let world = Arc::new(RBase::build(&base.board));
    let problem = problem(&base, world, &nets, Style::FortyFive, Effort::default());
    let progress = Progress::new();
    let sol = GridRouter::default().route(&problem, &progress);
    assert!(!sol.add.is_empty(), "routing the committed board adds at least one item");
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: sol.ops().into_iter().map(|op| DesignOp::Board { unit, op }).collect(),
        label: "route two nets".into(), circuits: vec![], expect: None,
    });
    let routed = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(routed.rejected.is_empty(), "routing two nets commits without any rejection: {:?}", routed.rejected);
    check(&engine, &published, "route two nets");
    let routed_text = engine.docs().expect("the engine retains documents").text(doc).expect("the board retains text").clone();
    assert_eq!(engine.log().entries.len(), 4, "the four board commands create exactly four history entries");
    published.clear();

    for step in 1..=4 {
        mailbox.push(Cmd::Undo);
        let undone = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
        assert!(undone.rejected.is_empty(), "undo {step} is accepted: {:?}", undone.rejected);
        assert!(undone.docs.is_some(), "undo {step} changed the text, so the plate gets its documents");
        check(&engine, &published, &format!("undo {step}"));
        published.clear();
    }
    assert_eq!(texts(&engine), loaded_texts, "four undos restore every loaded document byte for byte");
    assert_eq!(engine.docs().expect("the engine retains documents").map().as_ref(), &loaded_map, "four undos restore the loaded source map");
    let board = engine.design().unit(unit).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board");
    assert_eq!(board.content_hash(), loaded_hash, "four undos restore the loaded board content hash");

    for step in 1..=4 {
        mailbox.push(Cmd::Redo);
        let redone = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
        assert!(redone.rejected.is_empty(), "redo {step} is accepted: {:?}", redone.rejected);
        check(&engine, &published, &format!("redo {step}"));
        published.clear();
    }
    assert_eq!(engine.docs().expect("the engine retains documents").text(doc), Some(&routed_text), "four redos restore the routed board text byte for byte");
}

#[test]
fn a_refused_board_edit_leaves_text_and_model_untouched_and_names_its_statement() {
    let sources = fab_elec_fixtures::docs("ledctl").expect("the fixture supplies ledctl documents");
    let (design, docs) = Docs::from_evaluated(sources, fab_elec_fixtures::ledctl_eval());
    let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "docs".into()).with_docs(docs);
    let mailbox = Mailbox::default();
    let unit = BoardId(0);
    let doc = engine.docs().expect("the engine has documents").sources().find("ledctl.pcb.splash")
        .expect("the fixture supplies the board document");
    let board = engine.design().unit(unit).expect("the fixture has unit 0").board.as_ref().expect("unit 0 has a board");
    let item = board.placed.first().expect("the fixture has a placed part").id;
    let mut published = Vec::new();
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Board { unit, op: BoardOp::SetField { item, field: Field::Locked(true) } }],
        label: "lock first part".into(), circuits: vec![], expect: None,
    });
    let locked = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert!(locked.rejected.is_empty(), "locking the first part is accepted: {:?}", locked.rejected);
    check(&engine, &published, "lock first part");
    published.clear();

    let texts = |engine: &Engine| engine.docs().expect("the engine retains its documents").sources().docs().iter()
        .map(|doc| (doc.id, doc.text.clone())).collect::<Vec<_>>();
    let before_texts = texts(&engine);
    let board = engine.design().unit(unit).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board");
    let before_hash = board.content_hash();
    let before_log = engine.log().entries.len();
    let site = engine.docs().expect("the engine retains documents").map().locate(&Sel::Item(unit, item))
        .expect("the placed part has a source-map statement range");
    assert_eq!(site.doc, doc, "the part's source-map statement belongs to the board document");

    // Locked items may move under the current edit rules. An authored placement cannot detach.
    mailbox.push(Cmd::Commit {
        by: By::Person, ticket: None, place: vec![],
        ops: vec![DesignOp::Board { unit, op: BoardOp::Detach(item) }],
        label: "detach authored part".into(), circuits: vec![], expect: None,
    });
    let refused = engine.step(mailbox.take(), &mut |snapshot| published.push(snapshot));
    assert_eq!(refused.rejected.len(), 1, "detaching an authored placement rejects exactly one command");
    assert!(refused.rejected[0].1.contains("NotDetachable"), "the authored-placement detach rule causes the refusal");
    assert!(refused.docs.is_none(), "a refused edit gives the plate nothing to store");
    assert_eq!(texts(&engine), before_texts, "a refused edit leaves every document byte for byte unchanged");
    let board = engine.design().unit(unit).expect("unit 0 remains present").board.as_ref().expect("unit 0 keeps its board");
    assert_eq!(board.content_hash(), before_hash, "a refused edit leaves the board content hash unchanged");
    assert_eq!(engine.log().entries.len(), before_log, "a refused edit leaves the history length unchanged");
    check(&engine, &published, "refused detach");
    let snapshot = published.last().expect("the refused edit publishes a snapshot");
    let refusal = snapshot.last_refusal.as_ref().expect("the published snapshot records the refusal");
    assert_eq!(refusal.site, Some(Sel::Source { doc, start: site.span.start, end: site.span.end }), "the refusal names exactly the placed part's board-document statement range");
}
